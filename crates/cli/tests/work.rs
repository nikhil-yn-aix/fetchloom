//! The deterministic work counters, asserted through the real binary.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test setup, where a failure to run the binary is the assertion"
)]

use std::path::Path;
use std::process::{Command, Stdio};

use clap as _;
use clap_complete as _;
use fetchloom_archive as _;
use fetchloom_cache as _;
use fetchloom_cli as _;
use fetchloom_engine as _;
use fetchloom_faults as _;
use fetchloom_platform as _;
use fetchloom_sources as _;
use flate2 as _;
use serde as _;
use toml as _;

use fetchloom_engine::work::Work;
use tempfile::TempDir;

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_fetchloom")
}

fn corpus(root: &Path) {
    std::fs::create_dir_all(root.join("nested")).unwrap();
    std::fs::write(root.join("a.txt"), b"hello").unwrap();
    std::fs::write(root.join("zero.txt"), b"").unwrap();
    std::fs::write(root.join("nested").join("b.txt"), b"world").unwrap();
}

/// What the `--json` result carries that this suite reads.
#[derive(serde::Deserialize)]
struct Reported {
    work: Work,
}

fn get_reporting_work(source: &Path, destination: &Path, cache: &Path) -> Work {
    let output = Command::new(binary())
        .current_dir(scratch())
        .arg("get")
        .arg(source)
        .arg("--output")
        .arg(destination)
        .arg("--json")
        .env("FETCHLOOM_CACHE_DIR", cache)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "the run failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let reported: Reported = serde_json::from_slice(&output.stdout).expect("a json result");
    reported.work
}

#[test]
fn a_run_reports_the_work_it_did() {
    let temporary = TempDir::new().unwrap();
    let source = temporary.path().join("source");
    corpus(&source);
    let work = get_reporting_work(
        &source,
        &temporary.path().join("out"),
        &temporary.path().join("cache"),
    );
    assert!(work.bytes_read > 0, "a run that copied files read bytes");
    assert!(
        work.bytes_written > 0,
        "a run that copied files wrote bytes"
    );
    assert_eq!(
        work.requests, 0,
        "a local run issues no request to any source"
    );
}

#[test]
fn identical_inputs_count_identically() {
    let first = TempDir::new().unwrap();
    let second = TempDir::new().unwrap();
    corpus(&first.path().join("source"));
    corpus(&second.path().join("source"));

    let one = get_reporting_work(
        &first.path().join("source"),
        &first.path().join("out"),
        &first.path().join("cache"),
    );
    let two = get_reporting_work(
        &second.path().join("source"),
        &second.path().join("out"),
        &second.path().join("cache"),
    );
    assert_eq!(
        one, two,
        "the same inputs must produce the same counts, or the metric cannot gate"
    );
}

#[test]
fn a_local_ingest_writes_nothing_into_a_cache_that_already_holds_the_bytes() {
    let temporary = TempDir::new().unwrap();
    let source = temporary.path().join("source");
    corpus(&source);
    let cache = temporary.path().join("cache");

    let cold = get_reporting_work(&source, &temporary.path().join("cold"), &cache);
    let warm = get_reporting_work(&source, &temporary.path().join("warm"), &cache);

    assert!(
        warm.bytes_written < cold.bytes_written,
        "a run against a cache that already holds every object wrote as much as the run that \
         filled it: cold {} warm {}",
        cold.bytes_written,
        warm.bytes_written
    );
    assert_eq!(
        cold.bytes_read, warm.bytes_read,
        "a local source states no digest, so both runs read it exactly once to learn one: \
         cold {} warm {}",
        cold.bytes_read, warm.bytes_read
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
