//! Mode comes from the source, never from a stat of a destination, so one tree
//! digests the same on every platform and a second run writes nothing.

#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test assertions, where the run that failed is the message"
)]

use std::io::Write;
use std::path::{Path, PathBuf};
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
use fetchloom_view as _;
#[cfg(unix)]
use rustix as _;
use serde as _;
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

/// A gzip-wrapped tar holding one `0644` file and one `0755` file.
fn mixed_mode_archive() -> Vec<u8> {
    let mut writer = TarWriter::new();
    writer.push(&TarHeader::ustar(b"tools/", TYPEFLAG_DIRECTORY), b"");
    let mut readable = TarHeader::ustar(b"tools/notes.txt", TYPEFLAG_REGULAR);
    readable.set_size(6).set_mode(0o644);
    writer.push(&readable, b"hello\n");
    let mut runnable = TarHeader::ustar(b"tools/run.sh", TYPEFLAG_REGULAR);
    runnable.set_size(18).set_mode(0o755);
    writer.push(&runnable, b"#!/bin/sh\necho hi\n");
    let tar = writer.finish();

    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&tar).unwrap();
    encoder.finish().unwrap()
}

/// A directory holding one ordinary file and one the platform may mark
/// executable.
fn mixed_mode_directory(at: &Path) -> PathBuf {
    let root = at.join("tree");
    std::fs::create_dir_all(root.join("tools")).unwrap();
    std::fs::write(root.join("tools").join("notes.txt"), b"hello\n").unwrap();
    let script = root.join("tools").join("run.sh");
    std::fs::write(&script, b"#!/bin/sh\necho hi\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    root
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

fn body(output: &Output) -> serde_json::Value {
    let text = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(text.trim()).unwrap_or_else(|error| {
        panic!(
            "the result was not JSON ({error}): {text}\nstderr: {}",
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn field<'a>(value: &'a serde_json::Value, key: &str) -> &'a str {
    value[key]
        .as_str()
        .unwrap_or_else(|| panic!("the result carried no {key}: {value}"))
}

#[test]
fn a_directory_source_digests_the_same_on_every_platform() {
    let temporary = TempDir::new().unwrap();
    let root = mixed_mode_directory(temporary.path());
    let output = get(
        &root,
        &temporary.path().join("out"),
        &temporary.path().join("cache"),
        &[],
    );
    assert!(
        output.status.success(),
        "the run failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        field(&body(&output), "tree"),
        "blake3:4ca72e4f6a16344154859eb10b2e31474f0a9a5b850205d0e063bbd54a5bf91c",
        "a walked tree digested differently than it does on the other platforms, \
         which means a mode was read from a stat"
    );
}

#[test]
fn get_and_verify_agree_for_a_directory_source() {
    let temporary = TempDir::new().unwrap();
    let root = mixed_mode_directory(temporary.path());
    let destination = temporary.path().join("out");
    let cache = temporary.path().join("cache");
    let fetched = get(&root, &destination, &cache, &[]);
    assert!(fetched.status.success());
    let verified = verify_with_cache(&destination, &cache);
    assert!(
        verified.status.success(),
        "verify failed: {}",
        String::from_utf8_lossy(&verified.stderr)
    );
    assert_eq!(
        field(&body(&fetched), "tree"),
        field(&body(&verified), "tree"),
        "get and verify reported different trees for one destination"
    );
}

#[test]
fn an_archive_states_its_own_mode() {
    let temporary = TempDir::new().unwrap();
    let archive = temporary.path().join("tools.tar.gz");
    std::fs::write(&archive, mixed_mode_archive()).unwrap();
    let output = get(
        &archive,
        &temporary.path().join("out"),
        &temporary.path().join("cache"),
        &[],
    );
    assert!(
        output.status.success(),
        "the run failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        field(&body(&output), "tree"),
        "blake3:bbc14e61d84d0e6b4580730f6b0512a26e4a6ae39b3a4330856feb77fbc68ca0",
        "an archive's own mode was not what the tree recorded"
    );
}

#[test]
fn a_second_get_of_an_archive_writes_nothing() {
    let temporary = TempDir::new().unwrap();
    let archive = temporary.path().join("tools.tar.gz");
    std::fs::write(&archive, mixed_mode_archive()).unwrap();
    let destination = temporary.path().join("out");
    let cache = temporary.path().join("cache");

    let first = get(&archive, &destination, &cache, &[]);
    assert!(
        first.status.success(),
        "the first run failed: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert_eq!(field(&body(&first), "status"), "materialized");

    let second = get(&archive, &destination, &cache, &[]);
    assert!(
        second.status.success(),
        "the second run failed: {}",
        String::from_utf8_lossy(&second.stderr)
    );
    let second = body(&second);
    assert_eq!(
        field(&second, "status"),
        "unchanged",
        "running an archive twice did not report the destination unchanged"
    );
    assert_eq!(
        second["work"]["bytes_written"], 0,
        "the second run wrote bytes into a destination that already held the tree"
    );
    assert_eq!(
        field(&body(&first), "tree"),
        field(&second, "tree"),
        "the two runs disagreed about the tree they resolved"
    );
}

#[test]
fn a_second_get_of_a_directory_writes_nothing() {
    let temporary = TempDir::new().unwrap();
    let root = mixed_mode_directory(temporary.path());
    let destination = temporary.path().join("out");
    let cache = temporary.path().join("cache");

    assert!(get(&root, &destination, &cache, &[]).status.success());
    let second = get(&root, &destination, &cache, &[]);
    assert!(
        second.status.success(),
        "the second run failed: {}",
        String::from_utf8_lossy(&second.stderr)
    );
    let second = body(&second);
    assert_eq!(field(&second, "status"), "unchanged");
    assert_eq!(second["work"]["bytes_written"], 0);
}

fn verify_with_cache(path: &Path, cache: &Path) -> Output {
    Command::new(binary())
        .current_dir(scratch())
        .arg("verify")
        .arg(path)
        .arg("--json")
        .env("FETCHLOOM_CACHE_DIR", cache)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

#[test]
fn verify_against_a_receipt_reproduces_the_digest_get_reported() {
    let temporary = TempDir::new().unwrap();
    let archive = temporary.path().join("tools.tar.gz");
    std::fs::write(&archive, mixed_mode_archive()).unwrap();
    let destination = temporary.path().join("out");
    let cache = temporary.path().join("cache");

    let fetched = get(&archive, &destination, &cache, &[]);
    assert!(
        fetched.status.success(),
        "the run failed: {}",
        String::from_utf8_lossy(&fetched.stderr)
    );

    let verified = verify_with_cache(&destination, &cache);
    assert!(
        verified.status.success(),
        "verify failed: {}",
        String::from_utf8_lossy(&verified.stderr)
    );
    assert_eq!(
        field(&body(&fetched), "tree"),
        field(&body(&verified), "tree"),
        "verify could not reproduce the digest get reported for an archive stating a mode"
    );
}

#[test]
fn verify_reads_no_mode_when_no_receipt_names_the_destination() {
    let temporary = TempDir::new().unwrap();
    let archive = temporary.path().join("tools.tar.gz");
    std::fs::write(&archive, mixed_mode_archive()).unwrap();
    let destination = temporary.path().join("out");
    let cache = temporary.path().join("cache");
    assert!(get(&archive, &destination, &cache, &[]).status.success());

    let elsewhere = temporary.path().join("other-cache");
    let output = Command::new(binary())
        .current_dir(scratch())
        .arg("verify")
        .arg(&destination)
        .arg("--events")
        .arg("-")
        .env("FETCHLOOM_CACHE_DIR", &elsewhere)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success());
    let events = String::from_utf8_lossy(&output.stdout);
    assert!(
        events.contains("\"event\":\"degrade\"") && events.contains("mode"),
        "verify without a receipt did not say it read no mode: {events}"
    );
}

#[test]
fn a_receipt_naming_another_destination_is_not_read_for_this_one() {
    let temporary = TempDir::new().unwrap();
    let archive = temporary.path().join("tools.tar.gz");
    std::fs::write(&archive, mixed_mode_archive()).unwrap();
    let destination = temporary.path().join("out");
    let cache = temporary.path().join("cache");
    assert!(get(&archive, &destination, &cache, &[]).status.success());

    let moved = temporary.path().join("moved");
    std::fs::rename(&destination, &moved).unwrap();
    let output = Command::new(binary())
        .current_dir(scratch())
        .arg("verify")
        .arg(&moved)
        .arg("--events")
        .arg("-")
        .env("FETCHLOOM_CACHE_DIR", &cache)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success());
    let events = String::from_utf8_lossy(&output.stdout);
    assert!(
        events.contains("\"event\":\"degrade\"") && events.contains("mode"),
        "a receipt written for another path was read for this one: {events}"
    );
}

#[test]
fn verify_against_a_receipt_reports_a_destination_that_changed() {
    let temporary = TempDir::new().unwrap();
    let archive = temporary.path().join("tools.tar.gz");
    std::fs::write(&archive, mixed_mode_archive()).unwrap();
    let destination = temporary.path().join("out");
    let cache = temporary.path().join("cache");
    assert!(get(&archive, &destination, &cache, &[]).status.success());

    std::fs::write(destination.join("tools").join("notes.txt"), b"changed\n").unwrap();
    let verified = verify_with_cache(&destination, &cache);
    assert_eq!(
        verified.status.code(),
        Some(30),
        "a changed destination verified against its receipt: {}",
        String::from_utf8_lossy(&verified.stderr)
    );
}

/// The directory every command in this file runs in.
fn scratch() -> &'static std::path::Path {
    static SCRATCH: std::sync::OnceLock<TempDir> = std::sync::OnceLock::new();
    SCRATCH.get_or_init(|| TempDir::new().unwrap()).path()
}
