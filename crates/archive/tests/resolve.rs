//! Reading an archive's tree without writing it answers exactly what writing it
//! would have answered.

#![expect(
    clippy::unwrap_used,
    reason = "test assertions, where the archive that failed is the message"
)]

use blake3 as _;
use bzip2 as _;
use flate2 as _;
use lzma_rust2 as _;
use ruzstd as _;
use tar as _;
use zip as _;

use std::io::Cursor;

use fetchloom_archive::{ArchiveReader, extract, resolve};
use fetchloom_engine::limits::Limits;
use fetchloom_engine::manifest::ArchiveFormat;
use fetchloom_engine::selection::{Glob, Selection};
use fetchloom_engine::tree::TreeEntry;
use fetchloom_faults::{
    TYPEFLAG_DIRECTORY, TYPEFLAG_REGULAR, TYPEFLAG_SYMLINK, TarHeader, TarWriter,
};
use fetchloom_platform::NativePlatform;

fn subject() -> Vec<u8> {
    let mut writer = TarWriter::new();
    writer.push(&TarHeader::ustar(b"tools/", TYPEFLAG_DIRECTORY), b"");
    let mut readable = TarHeader::ustar(b"tools/notes.txt", TYPEFLAG_REGULAR);
    readable.set_size(6).set_mode(0o644);
    writer.push(&readable, b"hello\n");
    let mut runnable = TarHeader::ustar(b"tools/run.sh", TYPEFLAG_REGULAR);
    runnable.set_size(18).set_mode(0o755);
    writer.push(&runnable, b"#!/bin/sh\necho hi\n");
    let mut link = TarHeader::ustar(b"tools/latest", TYPEFLAG_SYMLINK);
    link.set_linkname(b"notes.txt");
    writer.push(&link, b"");
    writer.finish()
}

fn reader(bytes: Vec<u8>) -> ArchiveReader<Cursor<Vec<u8>>> {
    ArchiveReader::new(
        Cursor::new(bytes),
        ArchiveFormat::Tar,
        "subject.tar",
        Limits::default(),
    )
    .unwrap()
}

fn guard(limits: Limits) -> fetchloom_archive::bomb::BombGuard {
    fetchloom_archive::bomb::BombGuard::new("subject.tar", subject().len() as u64, limits)
}

fn sorted(mut entries: Vec<TreeEntry>) -> Vec<TreeEntry> {
    entries.sort_by(|left, right| path_of(left).cmp(path_of(right)));
    entries
}

fn path_of(entry: &TreeEntry) -> &str {
    match entry {
        TreeEntry::File { path, .. }
        | TreeEntry::Directory { path }
        | TreeEntry::Symlink { path, .. } => path.as_str(),
    }
}

#[test]
fn resolving_answers_what_extracting_answers() {
    let staging = tempfile::tempdir().unwrap();
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    let selection = Selection::default();

    let written = extract(
        &mut reader(subject()),
        &selection,
        staging.path(),
        guard(Limits::default()),
        &platform,
        &fetchloom_engine::work::WorkCounter::new(),
    )
    .unwrap();
    let read = resolve(&mut reader(subject()), &selection, guard(Limits::default())).unwrap();

    assert_eq!(
        sorted(read),
        sorted(written),
        "reading an archive's tree disagreed with writing it"
    );
}

#[test]
fn resolving_writes_nothing_into_a_directory_it_is_not_given() {
    let staging = tempfile::tempdir().unwrap();
    let before: Vec<std::path::PathBuf> = std::fs::read_dir(staging.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert!(before.is_empty());

    resolve(
        &mut reader(subject()),
        &Selection::default(),
        guard(Limits::default()),
    )
    .unwrap();

    let after: Vec<std::path::PathBuf> = std::fs::read_dir(staging.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert!(after.is_empty(), "resolving created {after:?}");
}

#[test]
fn resolving_honors_the_selection_extraction_honors() {
    let selection = Selection {
        include: vec![Glob::new("tools/notes.txt")],
        ..Selection::default()
    };
    let staging = tempfile::tempdir().unwrap();
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));

    let written = extract(
        &mut reader(subject()),
        &selection,
        staging.path(),
        guard(Limits::default()),
        &platform,
        &fetchloom_engine::work::WorkCounter::new(),
    )
    .unwrap();
    let read = resolve(&mut reader(subject()), &selection, guard(Limits::default())).unwrap();

    assert_eq!(sorted(read), sorted(written));
}

#[test]
fn resolving_reports_an_archive_that_expands_past_the_limit() {
    let limits = Limits {
        expanded_bytes: 4,
        ..Limits::default()
    };
    let error = resolve(&mut reader(subject()), &Selection::default(), guard(limits)).unwrap_err();
    assert_eq!(error.kind().label(), "archive.bomb");
}

#[test]
fn resolving_reports_an_archive_that_expands_past_the_ratio_and_names_it() {
    let limits = Limits {
        expansion_ratio: 0,
        ..Limits::default()
    };
    let tight = fetchloom_archive::bomb::BombGuard::new("subject.tar", 1, limits);
    let error = resolve(&mut reader(subject()), &Selection::default(), tight).unwrap_err();
    assert_eq!(error.kind().label(), "archive.bomb");
    assert!(
        error.next_action().contains("subject.tar"),
        "the archive is not named: {}",
        error.next_action()
    );
}
