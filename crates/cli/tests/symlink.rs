//! A tree holding a symbolic link materializes that link, or says it cannot.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test assertions, where the entry that failed is the message"
)]

use std::path::Path;
use std::process::{Command, Stdio};

use clap as _;
use clap_complete as _;
use fetchloom_archive as _;
use fetchloom_cache as _;
use fetchloom_cli as _;
use fetchloom_faults as _;
use fetchloom_sources as _;
use flate2 as _;
use serde as _;
use serde_json as _;
use toml as _;

use fetchloom_engine::seam::platform::Platform;
use fetchloom_platform::NativePlatform;
use tempfile::TempDir;

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_fetchloom")
}

/// Reports whether this machine lets this process create a symbolic link.
///
/// Windows grants the privilege only under developer mode or elevation, so a
/// run without it proves nothing about materialization and says so.
fn links_are_permitted(directory: &Path) -> bool {
    let platform = NativePlatform::new();
    let probe = directory.join("fetchloom-link-probe");
    let created = platform.create_symlink(b"target", &probe).is_ok();
    let _ = std::fs::remove_file(&probe);
    created
}

fn get(source: &Path, destination: &Path, cache: &Path) -> std::process::Output {
    Command::new(binary())
        .arg("get")
        .arg(source)
        .arg("--output")
        .arg(destination)
        .arg("--json")
        .env("FETCHLOOM_CACHE_DIR", cache)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

#[test]
fn a_symlink_in_the_source_is_created_in_the_destination() {
    let temporary = TempDir::new().unwrap();
    if !links_are_permitted(temporary.path()) {
        return;
    }
    let source = temporary.path().join("source");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(source.join("real.txt"), b"contents").unwrap();
    let platform = NativePlatform::new();
    platform
        .create_symlink(b"real.txt", &source.join("link.txt"))
        .unwrap();

    let destination = temporary.path().join("out");
    let output = get(&source, &destination, &temporary.path().join("cache"));
    assert!(
        output.status.success(),
        "the run failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let placed = destination.join("link.txt");
    let found = std::fs::symlink_metadata(&placed)
        .unwrap_or_else(|reason| panic!("nothing at {}: {reason}", placed.display()));
    assert!(
        found.is_symlink(),
        "the destination holds {} as an ordinary entry rather than a symbolic link",
        placed.display()
    );
    assert_eq!(
        std::fs::read_link(&placed)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/"),
        "real.txt"
    );
}

#[test]
fn a_materialized_tree_holding_a_symlink_verifies_to_the_tree_it_reported() {
    let temporary = TempDir::new().unwrap();
    if !links_are_permitted(temporary.path()) {
        return;
    }
    let source = temporary.path().join("source");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(source.join("real.txt"), b"contents").unwrap();
    let platform = NativePlatform::new();
    platform
        .create_symlink(b"real.txt", &source.join("link.txt"))
        .unwrap();

    let destination = temporary.path().join("out");
    let output = get(&source, &destination, &temporary.path().join("cache"));
    assert!(output.status.success());
    let reported: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let materialized = reported["tree"].as_str().expect("a tree digest").to_owned();

    let verified = Command::new(binary())
        .arg("verify")
        .arg(&destination)
        .arg("--json")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        verified.status.success(),
        "verify failed: {}",
        String::from_utf8_lossy(&verified.stderr)
    );
    let seen: serde_json::Value = serde_json::from_slice(&verified.stdout).unwrap();
    assert_eq!(
        seen["tree"].as_str().expect("a tree digest"),
        materialized,
        "the destination does not reproduce the tree the run reported"
    );
}
