//! A recognized archive is extracted by `get`, unless `--no-extract` says not.

#![expect(
    clippy::unwrap_used,
    reason = "test assertions, where the archive that failed is the message"
)]

use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use clap as _;
use clap_complete as _;
use ctrlc as _;
use fetchloom_archive as _;
use fetchloom_cache as _;
use fetchloom_cli as _;
use fetchloom_engine as _;
use fetchloom_platform as _;
use fetchloom_sources as _;
use flate2 as _;
#[cfg(unix)]
use rustix as _;
use serde as _;
use serde_json as _;
use toml as _;
#[cfg(windows)]
use windows_sys as _;

use fetchloom_faults::{TYPEFLAG_DIRECTORY, TYPEFLAG_REGULAR, TarHeader, TarWriter};
use flate2::Compression;
use flate2::write::GzEncoder;
use tempfile::TempDir;

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_fetchloom")
}

/// Builds a gzip-wrapped tar holding a directory and two files.
fn corpus_archive() -> Vec<u8> {
    let mut writer = TarWriter::new();
    writer.push(&TarHeader::ustar(b"docs/", TYPEFLAG_DIRECTORY), b"");
    let mut first = TarHeader::ustar(b"docs/readme.txt", TYPEFLAG_REGULAR);
    first.set_size(6);
    writer.push(&first, b"hello\n");
    let mut second = TarHeader::ustar(b"notes.txt", TYPEFLAG_REGULAR);
    second.set_size(6);
    writer.push(&second, b"world\n");
    let tar = writer.finish();

    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&tar).unwrap();
    encoder.finish().unwrap()
}

fn get(source: &Path, destination: &Path, cache: &Path, extra: &[&str]) -> Output {
    Command::new(binary())
        .current_dir(scratch())
        .arg("get")
        .arg(source)
        .arg("--output")
        .arg(destination)
        .arg("--json")
        .args(extra)
        .env("FETCHLOOM_CACHE_DIR", cache)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn archive_at(directory: &Path, name: &str) -> std::path::PathBuf {
    let path = directory.join(name);
    std::fs::write(&path, corpus_archive()).unwrap();
    path
}

#[test]
fn a_recognized_archive_is_extracted_into_the_destination() {
    let temporary = TempDir::new().unwrap();
    let archive = archive_at(temporary.path(), "corpus.tar.gz");
    let destination = temporary.path().join("out");

    let output = get(&archive, &destination, &temporary.path().join("cache"), &[]);
    assert!(
        output.status.success(),
        "the run failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert_eq!(
        std::fs::read_to_string(destination.join("docs").join("readme.txt")).unwrap(),
        "hello\n"
    );
    assert_eq!(
        std::fs::read_to_string(destination.join("notes.txt")).unwrap(),
        "world\n"
    );
    assert!(
        !destination.join("corpus.tar.gz").exists(),
        "the archive itself was published beside what it holds"
    );
}

#[test]
fn no_extract_keeps_the_archive_as_one_file() {
    let temporary = TempDir::new().unwrap();
    let archive = archive_at(temporary.path(), "corpus.tar.gz");
    let destination = temporary.path().join("out");

    let output = get(
        &archive,
        &destination,
        &temporary.path().join("cache"),
        &["--no-extract"],
    );
    assert!(output.status.success());
    assert!(
        destination.join("corpus.tar.gz").is_file(),
        "--no-extract must leave the archive as one file"
    );
    assert!(!destination.join("notes.txt").exists());
}

#[test]
fn a_selection_takes_only_the_members_it_names() {
    let temporary = TempDir::new().unwrap();
    let archive = archive_at(temporary.path(), "corpus.tar.gz");
    let destination = temporary.path().join("out");

    let output = get(
        &archive,
        &destination,
        &temporary.path().join("cache"),
        &["--select", "docs/**"],
    );
    assert!(
        output.status.success(),
        "the run failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(destination.join("docs").join("readme.txt").is_file());
    assert!(
        !destination.join("notes.txt").exists(),
        "a member outside the selection was extracted anyway"
    );
}

#[test]
fn a_name_that_disagrees_with_the_bytes_is_refused() {
    let temporary = TempDir::new().unwrap();
    let lying = temporary.path().join("corpus.tar.gz");
    std::fs::write(&lying, b"PK\x03\x04 not really a gzip stream").unwrap();

    let output = get(
        &lying,
        &temporary.path().join("out"),
        &temporary.path().join("cache"),
        &[],
    );
    assert_eq!(
        output.status.code(),
        Some(70),
        "an archive this build cannot read exits seventy"
    );
    let complaint = String::from_utf8_lossy(&output.stdout);
    assert!(
        complaint.contains("archive.unsupported"),
        "the refusal did not name archive.unsupported: {complaint}"
    );
    assert!(
        complaint.contains("tar+gzip") && complaint.contains("zip"),
        "the refusal named only one of the two answers: {complaint}"
    );
}

/// The directory every command in this file runs in.
fn scratch() -> &'static std::path::Path {
    static SCRATCH: std::sync::OnceLock<TempDir> = std::sync::OnceLock::new();
    SCRATCH.get_or_init(|| TempDir::new().unwrap()).path()
}
