//! Reconciling `get` of an archive against an existing destination: the four
//! outcomes, and the flags that answer the two that stop a run.

#![expect(
    clippy::unwrap_used,
    reason = "test setup, where a failure to run the binary is the assertion"
)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use clap as _;
use clap_complete as _;
use fetchloom_archive as _;
use fetchloom_cache as _;
use fetchloom_cli as _;
use fetchloom_engine as _;
use fetchloom_platform as _;
use fetchloom_sources as _;
use serde as _;
use toml as _;

use fetchloom_faults::{TYPEFLAG_DIRECTORY, TYPEFLAG_REGULAR, TarHeader, TarWriter};
use flate2::Compression;
use flate2::write::GzEncoder;
use tempfile::TempDir;

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_fetchloom")
}

/// A gzip-wrapped tar holding a directory and three files.
fn archive_bytes() -> Vec<u8> {
    let mut writer = TarWriter::new();
    writer.push(&TarHeader::ustar(b"docs/", TYPEFLAG_DIRECTORY), b"");
    for (name, body) in [
        (&b"docs/one.txt"[..], &b"one\n"[..]),
        (&b"docs/two.txt"[..], &b"two\n"[..]),
        (&b"three.txt"[..], &b"three\n"[..]),
    ] {
        let mut header = TarHeader::ustar(name, TYPEFLAG_REGULAR);
        header.set_size(body.len() as u64).set_mode(0o644);
        writer.push(&header, body);
    }
    let tar = writer.finish();
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&tar).unwrap();
    encoder.finish().unwrap()
}

struct Subject {
    _temporary: TempDir,
    archive: PathBuf,
    destination: PathBuf,
    cache: PathBuf,
}

fn fetched() -> Subject {
    let temporary = TempDir::new().unwrap();
    let archive = temporary.path().join("corpus.tar.gz");
    std::fs::write(&archive, archive_bytes()).unwrap();
    let subject = Subject {
        archive,
        destination: temporary.path().join("out"),
        cache: temporary.path().join("cache"),
        _temporary: temporary,
    };
    let first = get(&subject, &[]);
    assert!(
        first.status.success(),
        "the first run failed: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    subject
}

fn get(subject: &Subject, extra: &[&str]) -> Output {
    Command::new(binary())
        .current_dir(scratch())
        .arg("get")
        .arg(&subject.archive)
        .arg("--output")
        .arg(&subject.destination)
        .arg("--json")
        .args(extra)
        .env("FETCHLOOM_CACHE_DIR", &subject.cache)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn body(output: &Output) -> serde_json::Value {
    serde_json::from_slice(&output.stdout).unwrap_or_default()
}

fn status(output: &Output) -> String {
    body(output)["status"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

fn complaint(output: &Output) -> String {
    body(output)["kind"].as_str().unwrap_or_default().to_owned()
}

/// Every path under a root with its length and write time.
fn state_of(root: &Path) -> Vec<(String, u64, Option<std::time::SystemTime>)> {
    let mut found = Vec::new();
    walk(root, root, &mut found);
    found.sort();
    found
}

fn walk(root: &Path, at: &Path, found: &mut Vec<(String, u64, Option<std::time::SystemTime>)>) {
    let Ok(entries) = std::fs::read_dir(at) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        found.push((
            path.strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/"),
            metadata.len(),
            metadata.modified().ok(),
        ));
        if metadata.is_dir() {
            walk(root, &path, found);
        }
    }
}

#[test]
fn a_missing_entry_is_restored_and_nothing_else_is_touched() {
    let subject = fetched();
    let removed = subject.destination.join("docs").join("two.txt");
    std::fs::remove_file(&removed).unwrap();
    let kept = state_of(&subject.destination);

    let second = get(&subject, &[]);
    assert!(
        second.status.success(),
        "the restoring run failed: {}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert_eq!(status(&second), "restored");
    assert_eq!(std::fs::read_to_string(&removed).unwrap(), "two\n");

    let after: Vec<_> = state_of(&subject.destination)
        .into_iter()
        .filter(|(path, _, _)| path != "docs/two.txt" && path != "docs")
        .collect();
    let kept: Vec<_> = kept
        .into_iter()
        .filter(|(path, _, _)| path != "docs")
        .collect();
    assert_eq!(
        after, kept,
        "restoring one entry moved a file that was already there"
    );
    assert!(
        subject.destination.join("docs").is_dir(),
        "restoring an entry removed the directory holding it"
    );
}

#[test]
fn a_modified_entry_stops_the_run_and_names_it() {
    let subject = fetched();
    std::fs::write(subject.destination.join("three.txt"), b"changed\n").unwrap();
    let untouched = state_of(&subject.destination);

    let second = get(&subject, &[]);
    assert_eq!(second.status.code(), Some(60));
    assert_eq!(complaint(&second), "destination.modified");
    let text = String::from_utf8_lossy(&second.stdout);
    assert!(
        text.contains("three.txt"),
        "the refusal did not name the modified entry: {text}"
    );
    assert_eq!(
        state_of(&subject.destination),
        untouched,
        "a refused run wrote into the destination anyway"
    );
}

#[test]
fn a_foreign_entry_stops_the_run_and_names_it() {
    let subject = fetched();
    std::fs::write(subject.destination.join("stranger.txt"), b"mine\n").unwrap();
    let untouched = state_of(&subject.destination);

    let second = get(&subject, &[]);
    assert_eq!(second.status.code(), Some(60));
    assert_eq!(complaint(&second), "destination.foreign");
    let text = String::from_utf8_lossy(&second.stdout);
    assert!(
        text.contains("stranger.txt"),
        "the refusal did not name the foreign entry: {text}"
    );
    assert_eq!(state_of(&subject.destination), untouched);
}

#[test]
fn force_overwrites_a_modified_entry_and_removes_a_foreign_one() {
    let subject = fetched();
    let first_tree = {
        let again = get(&subject, &["--adopt"]);
        body(&again)["tree"].as_str().unwrap_or_default().to_owned()
    };
    std::fs::write(subject.destination.join("three.txt"), b"changed\n").unwrap();
    std::fs::write(subject.destination.join("stranger.txt"), b"mine\n").unwrap();

    let forced = get(&subject, &["--force"]);
    assert!(
        forced.status.success(),
        "the forced run failed: {}",
        String::from_utf8_lossy(&forced.stderr)
    );
    assert_eq!(status(&forced), "materialized");
    assert_eq!(
        std::fs::read_to_string(subject.destination.join("three.txt")).unwrap(),
        "three\n"
    );
    assert!(
        !subject.destination.join("stranger.txt").exists(),
        "--force left a foreign entry in place"
    );
    assert_eq!(
        body(&forced)["tree"].as_str().unwrap_or_default(),
        first_tree,
        "the forced run did not reproduce the archive's own tree"
    );
}

#[test]
fn adopt_reports_the_destination_and_writes_nothing() {
    let subject = fetched();
    std::fs::write(subject.destination.join("three.txt"), b"changed\n").unwrap();
    let untouched = state_of(&subject.destination);

    let adopted = get(&subject, &["--adopt"]);
    assert!(
        adopted.status.success(),
        "the adopting run failed: {}",
        String::from_utf8_lossy(&adopted.stderr)
    );
    assert_eq!(status(&adopted), "adopted");
    assert_eq!(
        state_of(&subject.destination),
        untouched,
        "--adopt wrote into the destination it was told to accept"
    );

    let verified = Command::new(binary())
        .current_dir(scratch())
        .arg("verify")
        .arg(&subject.destination)
        .arg("--json")
        .env("FETCHLOOM_CACHE_DIR", &subject.cache)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(
        body(&adopted)["tree"].as_str().unwrap_or_default(),
        body(&verified)["tree"].as_str().unwrap_or_default(),
        "--adopt reported a tree the destination does not hold"
    );
}

/// The directory every command in this file runs in.
///
/// A run writes its lock beside the working directory, so each test binary is
/// given one of its own rather than writing into the workspace.
fn scratch() -> &'static std::path::Path {
    static SCRATCH: std::sync::OnceLock<TempDir> = std::sync::OnceLock::new();
    SCRATCH.get_or_init(|| TempDir::new().unwrap()).path()
}
