//! Bounded extraction driven over the hostile archive corpus and the properties
//! the reader defers to it.

#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test assertions, where the corpus entry that failed is the message"
)]

use blake3 as _;
use bzip2 as _;
use flate2 as _;
use lzma_rust2 as _;
use tar as _;
use zip as _;
use zstd as _;

use std::io::Cursor;
use std::path::Path;

use fetchloom_archive::{ArchiveReader, extract};
use fetchloom_engine::capability::{CaseFolding, Normalization};
use fetchloom_engine::error::Error;
use fetchloom_engine::limits::Limits;
use fetchloom_engine::manifest::ArchiveFormat;
use fetchloom_engine::seam::archive::Archive;
use fetchloom_engine::seam::platform::Platform;
use fetchloom_engine::selection::Selection;
use fetchloom_engine::tree::TreeEntry;
use fetchloom_faults::{
    Container, Corpus, CorpusEntry, Expectation, TYPEFLAG_DIRECTORY, TYPEFLAG_HARDLINK,
    TYPEFLAG_PAX, TYPEFLAG_REGULAR, TYPEFLAG_SYMLINK, TarHeader, TarWriter, pax_block, pax_record,
};
use fetchloom_platform::NativePlatform;

const CASE_COLLISION_ENTRIES: &[&str] =
    &["tar_case_collision", "tar_unicode_normalization_collision"];

const VOLUME_DEPENDENT_NAMES: &[&str] = &[
    "tar_windows_reserved_con",
    "tar_windows_reserved_aux_txt",
    "tar_windows_reserved_trailing_dot",
    "tar_windows_reserved_trailing_space",
    "tar_windows_reserved_colon",
    "tar_path_too_long",
];

fn volume_stores_this_name(name: &str) -> bool {
    let Ok(directory) = tempfile::tempdir() else {
        return false;
    };
    if std::fs::File::create_new(directory.path().join(name)).is_err() {
        return false;
    }
    std::fs::read_dir(directory.path()).is_ok_and(|entries| {
        entries
            .flatten()
            .any(|entry| entry.file_name().to_string_lossy() == name)
    })
}

const BOMB_ENTRIES: &[&str] = &[
    "tar_bomb_entry_count",
    "tar_bomb_expanded_bytes",
    "zip_bomb_expansion_ratio",
];

fn format_of(entry: &CorpusEntry) -> ArchiveFormat {
    match entry.container() {
        Container::Tar => ArchiveFormat::Tar,
        Container::Zip => ArchiveFormat::Zip,
    }
}

fn reader_for(entry: &CorpusEntry, limits: Limits) -> ArchiveReader<Cursor<Vec<u8>>> {
    ArchiveReader::new(
        Cursor::new(entry.bytes().to_vec()),
        format_of(entry),
        entry.name(),
        limits,
    )
    .unwrap_or_else(|error| {
        panic!(
            "entry {} could not even be opened: {} {}",
            entry.name(),
            error.kind().label(),
            error.next_action()
        )
    })
}

fn extract_entry(
    entry: &CorpusEntry,
    staging: &Path,
    reader_limits: Limits,
    extract_limits: Limits,
    platform: &NativePlatform,
) -> Result<Vec<TreeEntry>, Error> {
    let mut reader = reader_for(entry, reader_limits);
    let guard = reader.bomb_guard(extract_limits);
    extract(
        &mut reader,
        &Selection::default(),
        staging,
        guard,
        platform,
        &fetchloom_engine::work::WorkCounter::new(),
    )
}

fn is_empty(dir: &Path) -> bool {
    std::fs::read_dir(dir).unwrap().next().is_none()
}

#[test]
fn every_benign_entry_extracts_cleanly_into_a_real_staging_directory() {
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    let corpus = Corpus::build();
    for entry in corpus.benign() {
        let staging = tempfile::tempdir().unwrap();
        let entries = extract_entry(
            entry,
            staging.path(),
            Limits::default(),
            Limits::default(),
            &platform,
        )
        .unwrap_or_else(|error| {
            panic!(
                "benign entry {} was refused: {} {}",
                entry.name(),
                error.kind().label(),
                error.next_action()
            )
        });
        assert!(
            !entries.is_empty(),
            "benign entry {} produced no tree entries",
            entry.name()
        );
        assert!(
            !is_empty(staging.path()),
            "benign entry {} wrote no files to staging",
            entry.name()
        );
    }
}

fn check_volume_dependent(
    entry: &CorpusEntry,
    kind: &str,
    member: &str,
    platform: &NativePlatform,
) {
    let staging = tempfile::tempdir().unwrap();
    let result = extract_entry(
        entry,
        staging.path(),
        Limits::default(),
        Limits::default(),
        platform,
    );
    if volume_stores_this_name(member) {
        result.unwrap_or_else(|error| {
            panic!(
                "entry {} was refused on a volume that stores {member} exactly: {} {}",
                entry.name(),
                error.kind().label(),
                error.next_action()
            )
        });
        return;
    }
    let error = result.unwrap_err();
    assert_eq!(error.kind().label(), kind, "entry {}", entry.name());
    assert!(
        is_empty(staging.path()),
        "entry {} left files in staging after failing",
        entry.name()
    );
}

#[test]
fn every_hostile_entry_the_reader_defers_is_finally_decided() {
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    let corpus = Corpus::build();
    let mut checked = 0;
    for entry in corpus.hostile() {
        if BOMB_ENTRIES.contains(&entry.name()) {
            continue;
        }
        let Expectation::Rejected { kind, member } = entry.expectation() else {
            continue;
        };
        let staging = tempfile::tempdir().unwrap();

        if VOLUME_DEPENDENT_NAMES.contains(&entry.name()) {
            check_volume_dependent(entry, kind, member, &platform);
            checked += 1;
            continue;
        }

        if CASE_COLLISION_ENTRIES.contains(&entry.name()) {
            let capabilities = platform.volume_capabilities(staging.path()).unwrap();
            let this_volume_folds = if entry.name() == "tar_case_collision" {
                capabilities.case_folding == CaseFolding::Folding
            } else {
                capabilities.normalization == Normalization::Normalizing
            };
            let result = extract_entry(
                entry,
                staging.path(),
                Limits::default(),
                Limits::default(),
                &platform,
            );
            if this_volume_folds {
                let error = result.unwrap_err();
                assert_eq!(error.kind().label(), *kind, "entry {}", entry.name());
                assert!(
                    error.next_action().contains(member.as_str()),
                    "entry {} did not name {member}: {}",
                    entry.name(),
                    error.next_action()
                );
                assert!(
                    is_empty(staging.path()),
                    "entry {} left files in staging after failing",
                    entry.name()
                );
            } else {
                result.unwrap_or_else(|error| {
                    panic!(
                        "entry {} was refused on a volume measured as not folding: {} {}",
                        entry.name(),
                        error.kind().label(),
                        error.next_action()
                    )
                });
            }
            checked += 1;
            continue;
        }

        let result = extract_entry(
            entry,
            staging.path(),
            Limits::default(),
            Limits::default(),
            &platform,
        );
        let Err(error) = result else {
            panic!(
                "hostile entry {} was extracted without being refused, and must fail with {kind}",
                entry.name()
            )
        };
        assert_eq!(
            error.kind().label(),
            *kind,
            "entry {} was refused with the wrong kind: {}",
            entry.name(),
            error.next_action()
        );
        assert!(
            error.next_action().contains(member.as_str()),
            "entry {} was refused without naming {member}: {}",
            entry.name(),
            error.next_action()
        );
        assert!(
            is_empty(staging.path()),
            "entry {} left files in staging after failing",
            entry.name()
        );
        checked += 1;
    }
    assert!(
        checked > 0,
        "no hostile entry reached extraction, so this test asserts nothing"
    );
}

#[test]
fn every_deferred_entry_is_covered_by_this_suite() {
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
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    let corpus = Corpus::build();
    for name in DECIDED_BY_EXTRACTION {
        let entry = corpus
            .entries()
            .iter()
            .find(|candidate| candidate.name() == *name)
            .unwrap_or_else(|| panic!("the corpus no longer holds {name}"));
        let staging = tempfile::tempdir().unwrap();
        let result = extract_entry(
            entry,
            staging.path(),
            Limits::default(),
            Limits::default(),
            &platform,
        );
        if CASE_COLLISION_ENTRIES.contains(name) {
            let _ = result;
            continue;
        }
        let Expectation::Rejected { kind, member } = entry.expectation() else {
            panic!("{name} is deferred to extraction but is not a rejection");
        };
        if VOLUME_DEPENDENT_NAMES.contains(name) && volume_stores_this_name(member) {
            result.unwrap_or_else(|error| {
                panic!(
                    "deferred entry {name} was refused on a volume that stores {member} exactly: {}",
                    error.next_action()
                )
            });
            continue;
        }
        let Err(error) = result else {
            panic!("deferred entry {name} was extracted without being refused")
        };
        assert_eq!(error.kind().label(), *kind, "entry {name}");
        assert!(
            error.next_action().contains(member.as_str()),
            "entry {name} did not name {member}: {}",
            error.next_action()
        );
    }
}

#[test]
fn a_symlink_member_is_created_only_after_every_regular_member_exists() {
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    let mut writer = TarWriter::new();
    let mut symlink = TarHeader::ustar(b"conflict", TYPEFLAG_SYMLINK);
    symlink.set_linkname(b"elsewhere.txt");
    writer.push(&symlink, b"");
    let mut inside = TarHeader::ustar(b"conflict/inside.txt", TYPEFLAG_REGULAR);
    inside.set_size(1);
    writer.push(&inside, b"x");
    let bytes = writer.finish();

    let staging = tempfile::tempdir().unwrap();
    let mut reader = ArchiveReader::new(
        Cursor::new(bytes),
        ArchiveFormat::Tar,
        "ordering.tar",
        Limits::default(),
    )
    .unwrap();
    let guard = reader.bomb_guard(Limits::default());
    let error = extract(
        &mut reader,
        &Selection::default(),
        staging.path(),
        guard,
        &platform,
        &fetchloom_engine::work::WorkCounter::new(),
    )
    .unwrap_err();
    assert_eq!(error.kind().label(), "archive.collision");
    assert!(error.next_action().contains("conflict"));
    assert!(
        is_empty(staging.path()),
        "a failed extraction left files behind"
    );
}

#[test]
fn a_hard_link_to_a_member_the_archive_does_not_hold_is_a_link_escape() {
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    let mut writer = TarWriter::new();
    let mut hardlink = TarHeader::ustar(b"copy.txt", TYPEFLAG_HARDLINK);
    hardlink.set_linkname(b"missing.txt");
    writer.push(&hardlink, b"");
    let bytes = writer.finish();

    let staging = tempfile::tempdir().unwrap();
    let mut reader = ArchiveReader::new(
        Cursor::new(bytes),
        ArchiveFormat::Tar,
        "dangling_hardlink.tar",
        Limits::default(),
    )
    .unwrap();
    let guard = reader.bomb_guard(Limits::default());
    let error = extract(
        &mut reader,
        &Selection::default(),
        staging.path(),
        guard,
        &platform,
        &fetchloom_engine::work::WorkCounter::new(),
    )
    .unwrap_err();
    assert_eq!(error.kind().label(), "archive.link_escape");
    assert!(error.next_action().contains("copy.txt"));
    assert!(is_empty(staging.path()));
}

#[test]
fn a_hard_link_member_materializes_the_bytes_of_its_target() {
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    let mut writer = TarWriter::new();
    let mut original = TarHeader::ustar(b"original.txt", TYPEFLAG_REGULAR);
    original.set_size(5);
    writer.push(&original, b"hello");
    let mut hardlink = TarHeader::ustar(b"copy.txt", TYPEFLAG_HARDLINK);
    hardlink.set_linkname(b"original.txt");
    writer.push(&hardlink, b"");
    let bytes = writer.finish();

    let staging = tempfile::tempdir().unwrap();
    let mut reader = ArchiveReader::new(
        Cursor::new(bytes),
        ArchiveFormat::Tar,
        "hardlink.tar",
        Limits::default(),
    )
    .unwrap();
    let guard = reader.bomb_guard(Limits::default());
    let entries = extract(
        &mut reader,
        &Selection::default(),
        staging.path(),
        guard,
        &platform,
        &fetchloom_engine::work::WorkCounter::new(),
    )
    .unwrap();
    assert_eq!(entries.len(), 2);
    let copy = std::fs::read(staging.path().join("copy.txt")).unwrap();
    assert_eq!(copy, b"hello");
}

#[test]
fn many_members_extract_correctly_through_one_reused_buffer() {
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    let mut writer = TarWriter::new();
    let count = 500;
    for index in 0..count {
        let name = format!("file-{index:04}.bin");
        let mut header = TarHeader::ustar(name.as_bytes(), TYPEFLAG_REGULAR);
        let data = vec![u8::try_from(index % 251).unwrap_or(0); 200];
        header.set_size(data.len() as u64);
        writer.push(&header, &data);
    }
    let bytes = writer.finish();

    let staging = tempfile::tempdir().unwrap();
    let mut reader = ArchiveReader::new(
        Cursor::new(bytes),
        ArchiveFormat::Tar,
        "many_members.tar",
        Limits::default(),
    )
    .unwrap();
    let guard = reader.bomb_guard(Limits::default());
    let entries = extract(
        &mut reader,
        &Selection::default(),
        staging.path(),
        guard,
        &platform,
        &fetchloom_engine::work::WorkCounter::new(),
    )
    .unwrap();
    assert_eq!(entries.len(), count);
    for index in 0..count {
        let name = format!("file-{index:04}.bin");
        let content = std::fs::read(staging.path().join(&name)).unwrap();
        assert_eq!(content, vec![u8::try_from(index % 251).unwrap_or(0); 200]);
    }
}

#[test]
fn entry_count_is_enforced_against_what_extraction_actually_writes() {
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    let corpus = Corpus::build();
    let entry = corpus
        .entries()
        .iter()
        .find(|candidate| candidate.name() == "tar_bomb_entry_count")
        .unwrap();
    let staging = tempfile::tempdir().unwrap();
    let tiny = Limits {
        archive_entries: 2,
        ..Limits::default()
    };
    let error =
        extract_entry(entry, staging.path(), Limits::default(), tiny, &platform).unwrap_err();
    assert_eq!(error.kind().label(), "archive.bomb");
    assert!(error.next_action().contains('2'));
    assert!(is_empty(staging.path()));
}

#[test]
fn expanded_bytes_are_enforced_against_what_extraction_actually_writes() {
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    let corpus = Corpus::build();
    let entry = corpus
        .entries()
        .iter()
        .find(|candidate| candidate.name() == "tar_bomb_expanded_bytes")
        .unwrap();
    let staging = tempfile::tempdir().unwrap();
    let tiny = Limits {
        expanded_bytes: 100,
        ..Limits::default()
    };
    let error =
        extract_entry(entry, staging.path(), Limits::default(), tiny, &platform).unwrap_err();
    assert_eq!(error.kind().label(), "archive.bomb");
    assert!(error.next_action().contains("100"));
    assert!(is_empty(staging.path()));
}

#[test]
fn expansion_ratio_is_decided_by_the_reader_before_extraction_runs() {
    let corpus = Corpus::build();
    let entry = corpus
        .entries()
        .iter()
        .find(|candidate| candidate.name() == "zip_bomb_expansion_ratio")
        .unwrap();
    let tiny = Limits {
        expansion_ratio: 50,
        ..Limits::default()
    };
    let mut reader = ArchiveReader::new(
        Cursor::new(entry.bytes().to_vec()),
        ArchiveFormat::Zip,
        entry.name(),
        tiny,
    )
    .unwrap();
    let error = Archive::members(&mut reader).unwrap_err();
    assert_eq!(error.kind().label(), "archive.bomb");
    assert!(error.next_action().contains("50"));
}

#[cfg(windows)]
#[test]
fn a_colon_in_a_member_name_is_refused_rather_than_hidden_in_an_alternate_data_stream() {
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    let mut writer = TarWriter::new();
    let mut header = TarHeader::ustar(b"weird:name.txt", TYPEFLAG_REGULAR);
    header.set_size(1);
    writer.push(&header, b"x");
    let bytes = writer.finish();

    let staging = tempfile::tempdir().unwrap();
    let mut reader = ArchiveReader::new(
        Cursor::new(bytes),
        ArchiveFormat::Tar,
        "ads.tar",
        Limits::default(),
    )
    .unwrap();
    let guard = reader.bomb_guard(Limits::default());
    let error = extract(
        &mut reader,
        &Selection::default(),
        staging.path(),
        guard,
        &platform,
        &fetchloom_engine::work::WorkCounter::new(),
    )
    .unwrap_err();
    assert_eq!(error.kind().label(), "destination.unrepresentable");
    assert!(is_empty(staging.path()));
}

fn three_entry_tar(prefix: &str, pax: bool) -> Vec<u8> {
    let mut writer = TarWriter::new();
    let directory = format!("{prefix}docs/");
    writer.push(
        &TarHeader::ustar(directory.as_bytes(), TYPEFLAG_DIRECTORY),
        b"",
    );
    for (name, body) in [
        ("docs/readme.txt", &b"hello\n"[..]),
        ("notes.txt", b"world\n"),
    ] {
        let full = format!("{prefix}{name}");
        if pax {
            let records = [pax_record("mtime", "1700000000.123456789")];
            let block = pax_block(&records);
            let mut header = TarHeader::ustar(b"PaxHeader", TYPEFLAG_PAX);
            header.set_size(block.len() as u64);
            writer.push(&header, &block);
        }
        let mut header = TarHeader::ustar(full.as_bytes(), TYPEFLAG_REGULAR);
        header.set_size(body.len() as u64);
        writer.push(&header, body);
    }
    writer.finish()
}

fn tree_of(bytes: Vec<u8>) -> fetchloom_engine::digest::TreeDigest {
    let staging = tempfile::tempdir().unwrap();
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    let mut reader = ArchiveReader::new(
        Cursor::new(bytes),
        ArchiveFormat::Tar,
        "dialect.tar",
        Limits::default(),
    )
    .unwrap();
    let guard = reader.bomb_guard(Limits::default());
    let entries = extract(
        &mut reader,
        &Selection::default(),
        staging.path(),
        guard,
        &platform,
        &fetchloom_engine::work::WorkCounter::new(),
    )
    .unwrap();
    fetchloom_engine::canonical::tree_digest(&entries)
}

#[test]
fn every_tar_dialect_of_one_tree_produces_one_tree_digest() {
    let plain = tree_of(three_entry_tar("", false));
    let dotted = tree_of(three_entry_tar("./", false));
    let paxed = tree_of(three_entry_tar("", true));
    assert_eq!(
        plain, dotted,
        "a writer that prefixes every member with ./ named a different tree"
    );
    assert_eq!(
        plain, paxed,
        "a writer that records a timestamp in a pax header named a different tree"
    );
}

#[test]
fn the_expansion_ratio_is_enforced_where_the_bytes_are_written_and_the_archive_is_named() {
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    let corpus = Corpus::build();
    let entry = corpus
        .entries()
        .iter()
        .find(|candidate| candidate.name() == "zip_bomb_expansion_ratio")
        .unwrap();
    let staging = tempfile::tempdir().unwrap();
    let tiny = Limits {
        expansion_ratio: 50,
        ..Limits::default()
    };
    let error =
        extract_entry(entry, staging.path(), Limits::default(), tiny, &platform).unwrap_err();
    assert_eq!(error.kind().label(), "archive.bomb");
    assert!(
        error.next_action().contains("50"),
        "{}",
        error.next_action()
    );
    assert!(
        error.next_action().contains(entry.name()),
        "the archive is not named: {}",
        error.next_action()
    );
    assert!(is_empty(staging.path()));
}
