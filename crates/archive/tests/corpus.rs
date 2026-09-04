//! Every entry of the hostile archive corpus, driven through the reader.

#![expect(
    clippy::panic,
    reason = "test assertions, where the corpus entry that failed is the message"
)]

use blake3 as _;
use bzip2 as _;
use fetchloom_platform as _;
use flate2 as _;
use lzma_rust2 as _;
use ruzstd as _;
use tar as _;
use tempfile as _;
use zip as _;

use std::io::{Cursor, Read};

use fetchloom_archive::ArchiveReader;
use fetchloom_engine::limits::Limits;
use fetchloom_engine::manifest::ArchiveFormat;
use fetchloom_engine::seam::archive::Archive;
use fetchloom_faults::{Container, Corpus, CorpusEntry, Expectation};

const DECIDED_BY_EXTRACTION: &[&str] = &[
    "tar_case_collision",
    "tar_unicode_normalization_collision",
    "tar_windows_reserved_con",
    "tar_windows_reserved_aux_txt",
    "tar_windows_reserved_trailing_dot",
    "tar_windows_reserved_trailing_space",
    "tar_windows_reserved_colon",
    "tar_path_too_long",
];

const REQUIRES_OVERRIDDEN_LIMITS: &[&str] = &[
    "tar_bomb_entry_count",
    "tar_bomb_expanded_bytes",
    "zip_bomb_expansion_ratio",
];

fn read_through(
    entry: &CorpusEntry,
) -> Result<Vec<(String, Vec<u8>)>, fetchloom_engine::error::Error> {
    let format = match entry.container() {
        Container::Tar => ArchiveFormat::Tar,
        Container::Zip => ArchiveFormat::Zip,
    };
    let mut reader = ArchiveReader::new(
        Cursor::new(entry.bytes().to_vec()),
        format,
        entry.name(),
        Limits::default(),
    )?;
    let members = reader.members()?;
    let mut read = Vec::with_capacity(members.len());
    for member in &members {
        let mut bytes = Vec::new();
        reader
            .open(member)?
            .read_to_end(&mut bytes)
            .map_err(|error| {
                fetchloom_engine::error::Error::new(
                    fetchloom_engine::error::ErrorKind::ArchiveUnsupported,
                    format!("member \"{}\" could not be read: {error}", member.path),
                )
            })?;
        read.push((member.path.clone(), bytes));
    }
    Ok(read)
}

#[test]
fn every_benign_entry_lists_its_members_and_reads_their_bytes() {
    let corpus = Corpus::build();
    for entry in corpus.benign() {
        let read = read_through(entry).unwrap_or_else(|error| {
            panic!(
                "benign entry {} was refused with {}: {}",
                entry.name(),
                error.kind().label(),
                error.next_action()
            )
        });
        assert!(
            !read.is_empty(),
            "benign entry {} listed no members",
            entry.name()
        );
    }
}

#[test]
fn every_hostile_entry_the_reader_decides_fails_with_its_declared_kind() {
    let corpus = Corpus::build();
    let mut checked = 0;
    for entry in corpus.hostile() {
        if DECIDED_BY_EXTRACTION.contains(&entry.name())
            || REQUIRES_OVERRIDDEN_LIMITS.contains(&entry.name())
        {
            continue;
        }
        let Expectation::Rejected { kind, member } = entry.expectation() else {
            continue;
        };
        let Err(error) = read_through(entry) else {
            panic!(
                "hostile entry {} was read without being refused, and must fail with {kind}",
                entry.name()
            )
        };
        assert_eq!(
            error.kind().label(),
            *kind,
            "hostile entry {} was refused with the wrong kind: {}",
            entry.name(),
            error.next_action()
        );
        assert!(
            error.next_action().contains(member.as_str()),
            "hostile entry {} was refused without naming {member}: {}",
            entry.name(),
            error.next_action()
        );
        checked += 1;
    }
    assert!(
        checked > 0,
        "no hostile entry was decided by the reader, so this test asserts nothing"
    );
}

#[test]
fn an_entry_left_to_extraction_is_listed_rather_than_refused() {
    let corpus = Corpus::build();
    for name in DECIDED_BY_EXTRACTION {
        let entry = corpus
            .entries()
            .iter()
            .find(|candidate| candidate.name() == *name)
            .unwrap_or_else(|| panic!("the corpus no longer holds {name}"));
        assert!(
            matches!(entry.expectation(), Expectation::Rejected { .. }),
            "{name} is listed as decided by extraction but is not a rejection"
        );
        let read = read_through(entry);
        assert!(
            read.is_ok(),
            "{name} is left to extraction, so the reader must list it rather than refuse it"
        );
    }
}

#[test]
fn the_corpus_is_covered_entry_by_entry() {
    let corpus = Corpus::build();
    let names: Vec<&str> = corpus.entries().iter().map(CorpusEntry::name).collect();
    for deferred in DECIDED_BY_EXTRACTION {
        assert!(
            names.contains(deferred),
            "{deferred} is deferred to extraction but is not in the corpus"
        );
    }
    assert_eq!(
        corpus.entries().len(),
        corpus.hostile().len() + corpus.benign().len(),
        "every corpus entry is either hostile or benign"
    );
}
