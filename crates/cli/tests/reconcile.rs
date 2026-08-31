//! Contract tests over reconciling `get` against an existing destination:
//! unchanged, restored, modified, foreign, `--force`, `--adopt`, and the
//! selection flags that decide which members land and under what path.

#![expect(
    clippy::unwrap_used,
    reason = "test setup, where a failure to run the binary is the assertion"
)]

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
use serde as _;
use toml as _;
#[cfg(windows)]
use windows_sys as _;

use fetchloom_faults as _;
use tempfile::TempDir;

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_fetchloom")
}

fn run(arguments: &[&str]) -> Output {
    let cache = TempDir::new().unwrap();
    Command::new(binary())
        .current_dir(scratch())
        .args(arguments)
        .env("FETCHLOOM_CACHE_DIR", cache.path())
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn corpus(root: &Path) -> std::path::PathBuf {
    let source = root.join("source");
    std::fs::create_dir_all(source.join("nested")).unwrap();
    std::fs::write(source.join("a.txt"), b"hello").unwrap();
    std::fs::write(source.join("nested").join("b.txt"), b"world").unwrap();
    source
}

fn get_json(arguments: &[&str]) -> (Output, serde_json::Value) {
    let output = run(arguments);
    let body: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap_or_default();
    (output, body)
}

#[test]
fn a_second_identical_run_reports_unchanged_and_writes_exactly_zero_bytes() {
    let temporary = TempDir::new().unwrap();
    let source = corpus(temporary.path());
    let destination = temporary.path().join("destination");

    let first = run(&[
        "get",
        source.to_str().unwrap(),
        "--output",
        destination.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(
        first.status.code(),
        Some(0),
        "stderr was {}",
        String::from_utf8_lossy(&first.stderr)
    );

    let (second, body) = get_json(&[
        "get",
        source.to_str().unwrap(),
        "--output",
        destination.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(
        second.status.code(),
        Some(0),
        "stderr was {}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert_eq!(body["status"].as_str(), Some("unchanged"));
    assert_eq!(
        body["work"]["bytes_written"].as_u64(),
        Some(0),
        "the second run wrote bytes into an already correct destination: {body}"
    );
}

#[test]
fn a_modified_entry_stops_the_run_names_the_path_and_leaves_the_destination_untouched() {
    let temporary = TempDir::new().unwrap();
    let source = corpus(temporary.path());
    let destination = temporary.path().join("destination");

    let first = run(&[
        "get",
        source.to_str().unwrap(),
        "--output",
        destination.to_str().unwrap(),
    ]);
    assert_eq!(first.status.code(), Some(0));

    std::fs::write(destination.join("a.txt"), b"tampered").unwrap();

    let second = run(&[
        "get",
        source.to_str().unwrap(),
        "--output",
        destination.to_str().unwrap(),
    ]);
    assert_eq!(second.status.code(), Some(60));
    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(
        stderr.contains("destination.modified"),
        "stderr was {stderr}"
    );
    assert!(stderr.contains("a.txt"), "the path was not named: {stderr}");
    assert_eq!(
        std::fs::read(destination.join("a.txt")).unwrap(),
        b"tampered",
        "a run that stopped before staging touched the modified entry"
    );
}

#[test]
fn a_foreign_entry_stops_the_run_names_the_path_and_is_never_deleted() {
    let temporary = TempDir::new().unwrap();
    let source = corpus(temporary.path());
    let destination = temporary.path().join("destination");

    let first = run(&[
        "get",
        source.to_str().unwrap(),
        "--output",
        destination.to_str().unwrap(),
    ]);
    assert_eq!(first.status.code(), Some(0));

    std::fs::write(destination.join("stray.txt"), b"nobody asked for this").unwrap();

    let second = run(&[
        "get",
        source.to_str().unwrap(),
        "--output",
        destination.to_str().unwrap(),
    ]);
    assert_eq!(second.status.code(), Some(60));
    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(
        stderr.contains("destination.foreign"),
        "stderr was {stderr}"
    );
    assert!(
        stderr.contains("stray.txt"),
        "the foreign path was not named: {stderr}"
    );
    assert!(
        destination.join("stray.txt").is_file(),
        "a foreign entry was deleted without --force"
    );
}

#[test]
fn a_missing_entry_is_restored_without_touching_entries_already_correct() {
    let temporary = TempDir::new().unwrap();
    let source = corpus(temporary.path());
    let destination = temporary.path().join("destination");

    let first = run(&[
        "get",
        source.to_str().unwrap(),
        "--output",
        destination.to_str().unwrap(),
    ]);
    assert_eq!(first.status.code(), Some(0));

    let untouched_before = std::fs::metadata(destination.join("a.txt"))
        .unwrap()
        .modified()
        .unwrap();
    std::fs::remove_file(destination.join("nested").join("b.txt")).unwrap();

    let (second, body) = get_json(&[
        "get",
        source.to_str().unwrap(),
        "--output",
        destination.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(
        second.status.code(),
        Some(0),
        "stderr was {}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert_eq!(body["status"].as_str(), Some("restored"));
    assert_eq!(
        std::fs::read(destination.join("nested").join("b.txt")).unwrap(),
        b"world"
    );
    assert_eq!(std::fs::read(destination.join("a.txt")).unwrap(), b"hello");
    let untouched_after = std::fs::metadata(destination.join("a.txt"))
        .unwrap()
        .modified()
        .unwrap();
    assert_eq!(
        untouched_before, untouched_after,
        "an entry that was already correct was rewritten"
    );
}

#[test]
fn force_overwrites_a_modified_entry() {
    let temporary = TempDir::new().unwrap();
    let source = corpus(temporary.path());
    let destination = temporary.path().join("destination");

    let first = run(&[
        "get",
        source.to_str().unwrap(),
        "--output",
        destination.to_str().unwrap(),
    ]);
    assert_eq!(first.status.code(), Some(0));

    std::fs::write(destination.join("a.txt"), b"tampered").unwrap();

    let second = run(&[
        "get",
        source.to_str().unwrap(),
        "--output",
        destination.to_str().unwrap(),
        "--force",
    ]);
    assert_eq!(
        second.status.code(),
        Some(0),
        "stderr was {}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert_eq!(std::fs::read(destination.join("a.txt")).unwrap(), b"hello");
}

#[test]
fn adopt_writes_nothing_and_reports_the_destinations_own_tree() {
    let temporary = TempDir::new().unwrap();
    let source = corpus(temporary.path());
    let destination = temporary.path().join("destination");

    let first = run(&[
        "get",
        source.to_str().unwrap(),
        "--output",
        destination.to_str().unwrap(),
    ]);
    assert_eq!(first.status.code(), Some(0));

    std::fs::write(destination.join("a.txt"), b"tampered").unwrap();
    std::fs::write(destination.join("stray.txt"), b"kept as is").unwrap();

    let (second, body) = get_json(&[
        "get",
        source.to_str().unwrap(),
        "--output",
        destination.to_str().unwrap(),
        "--adopt",
        "--json",
    ]);
    assert_eq!(
        second.status.code(),
        Some(0),
        "stderr was {}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert_eq!(body["status"].as_str(), Some("adopted"));
    assert_eq!(
        body["work"]["bytes_written"].as_u64(),
        Some(0),
        "--adopt wrote bytes: {body}"
    );
    assert_eq!(
        std::fs::read(destination.join("a.txt")).unwrap(),
        b"tampered",
        "--adopt changed a destination entry"
    );
    assert!(
        destination.join("stray.txt").is_file(),
        "--adopt removed a foreign entry"
    );
}

#[test]
fn select_and_exclude_choose_the_right_members_end_to_end() {
    let temporary = TempDir::new().unwrap();
    let source = corpus(temporary.path());
    let destination = temporary.path().join("destination");

    let output = run(&[
        "get",
        source.to_str().unwrap(),
        "--output",
        destination.to_str().unwrap(),
        "--select",
        "**",
        "--exclude",
        "nested/**",
    ]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr was {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(destination.join("a.txt").is_file());
    assert!(
        !destination.join("nested").exists(),
        "an excluded directory was materialized"
    );
}

#[test]
fn layout_flatten_one_drops_the_first_path_component() {
    let temporary = TempDir::new().unwrap();
    let source = corpus(temporary.path());
    let destination = temporary.path().join("destination");

    let output = run(&[
        "get",
        source.to_str().unwrap(),
        "--output",
        destination.to_str().unwrap(),
        "--select",
        "nested/b.txt",
        "--layout",
        "flatten:1",
    ]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr was {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read(destination.join("b.txt")).unwrap(),
        b"world",
        "flatten:1 did not drop the leading nested/ component"
    );
    assert!(!destination.join("nested").exists());
}

#[test]
fn layout_flatten_past_the_depth_of_a_shallow_tree_fails_unrepresentable() {
    let temporary = TempDir::new().unwrap();
    let source = corpus(temporary.path());
    let destination = temporary.path().join("destination");

    let output = run(&[
        "get",
        source.to_str().unwrap(),
        "--output",
        destination.to_str().unwrap(),
        "--layout",
        "flatten:9",
    ]);
    assert_eq!(output.status.code(), Some(60));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("destination.unrepresentable"),
        "stderr was {stderr}"
    );
}

#[test]
fn a_selection_matching_nothing_fails_unresolved() {
    let temporary = TempDir::new().unwrap();
    let source = corpus(temporary.path());
    let destination = temporary.path().join("destination");

    let output = run(&[
        "get",
        source.to_str().unwrap(),
        "--output",
        destination.to_str().unwrap(),
        "--select",
        "does-not-exist",
    ]);
    assert_eq!(output.status.code(), Some(10));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("reference.unresolved"),
        "stderr was {stderr}"
    );
}

#[test]
fn a_malformed_layout_value_is_a_usage_error() {
    let temporary = TempDir::new().unwrap();
    let source = corpus(temporary.path());
    let destination = temporary.path().join("destination");

    let output = run(&[
        "get",
        source.to_str().unwrap(),
        "--output",
        destination.to_str().unwrap(),
        "--layout",
        "sideways",
    ]);
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn every_status_a_run_reports_is_one_the_contract_names() {
    let allowed = ["materialized", "unchanged", "restored", "adopted"];
    let source = std::fs::read_to_string("src/run.rs").unwrap();
    let mut seen = Vec::new();
    for piece in source.split("status: \"").skip(1) {
        let value = piece.split('"').next().unwrap_or_default().to_owned();
        seen.push(value);
    }
    assert!(!seen.is_empty(), "no status is reported anywhere");
    for status in &seen {
        assert!(
            allowed.contains(&status.as_str()),
            "the run reports {status}, which contracts.md does not name"
        );
    }
}

/// The directory every command in this file runs in.
///
/// A run writes its lock beside the working directory, so each test binary is
/// given one of its own rather than writing into the workspace.
fn scratch() -> &'static std::path::Path {
    static SCRATCH: std::sync::OnceLock<TempDir> = std::sync::OnceLock::new();
    SCRATCH.get_or_init(|| TempDir::new().unwrap()).path()
}
