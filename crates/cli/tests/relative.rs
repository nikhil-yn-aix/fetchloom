//! A relative `--output`, a relative `--cache-dir`, or a relative `verify`
//! target resolve against the process working directory exactly as an
//! absolute path resolves against itself.

#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test setup, where a failure to run the binary is the assertion"
)]

use std::path::Path;
use std::process::{Command, Output, Stdio};

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
use serde_json as _;
use toml as _;

use tempfile::TempDir;

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_fetchloom")
}

fn source_tree(under: &Path) -> std::path::PathBuf {
    let source = under.join("source");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(source.join("a.txt"), b"hello").unwrap();
    source
}

fn get(cwd: &Path, source: &Path, output: &str, cache_dir: &str) -> (Output, serde_json::Value) {
    let raw = Command::new(binary())
        .arg("get")
        .arg(source)
        .arg("--output")
        .arg(output)
        .arg("--cache-dir")
        .arg(cache_dir)
        .arg("--json")
        .current_dir(cwd)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&raw.stdout).unwrap_or_else(|error| {
        panic!(
            "stdout was not JSON: {error}\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&raw.stdout),
            String::from_utf8_lossy(&raw.stderr)
        )
    });
    (raw, body)
}

fn assert_get_succeeded(output: &Output, body: &serde_json::Value) {
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr was {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(body["status"].as_str(), Some("materialized"));
}

#[test]
fn a_relative_output_with_a_single_component_lands_beside_the_working_directory() {
    let temporary = TempDir::new().unwrap();
    let cwd = temporary.path().join("cwd");
    std::fs::create_dir_all(&cwd).unwrap();
    let source = source_tree(temporary.path());
    let cache_dir = temporary.path().join("cache");

    let (output, body) = get(&cwd, &source, "relout", cache_dir.to_str().unwrap());
    assert_get_succeeded(&output, &body);

    let expected = cwd.join("relout");
    assert!(
        expected.join("a.txt").exists(),
        "expected {} to hold a.txt",
        expected.display()
    );
}

#[test]
fn a_relative_output_with_a_separator_lands_beside_the_working_directory() {
    let temporary = TempDir::new().unwrap();
    let cwd = temporary.path().join("cwd");
    std::fs::create_dir_all(&cwd).unwrap();
    let source = source_tree(temporary.path());
    let cache_dir = temporary.path().join("cache");

    let (output, body) = get(&cwd, &source, "a/b", cache_dir.to_str().unwrap());
    assert_get_succeeded(&output, &body);

    let expected = cwd.join("a").join("b");
    assert!(
        expected.join("a.txt").exists(),
        "expected {} to hold a.txt",
        expected.display()
    );
}

#[test]
fn an_absolute_output_with_a_relative_cache_dir_uses_the_working_directory_for_the_cache() {
    let temporary = TempDir::new().unwrap();
    let cwd = temporary.path().join("cwd");
    std::fs::create_dir_all(&cwd).unwrap();
    let source = source_tree(temporary.path());
    let destination = temporary.path().join("destination");

    let (output, body) = get(&cwd, &source, destination.to_str().unwrap(), "relcache");
    assert_get_succeeded(&output, &body);

    assert!(destination.join("a.txt").exists());
    assert!(
        cwd.join("relcache").exists(),
        "expected the cache to have been created at {}",
        cwd.join("relcache").display()
    );
}

#[test]
fn a_relative_output_and_a_relative_cache_dir_both_resolve_against_the_working_directory() {
    let temporary = TempDir::new().unwrap();
    let cwd = temporary.path().join("cwd");
    std::fs::create_dir_all(&cwd).unwrap();
    let source = source_tree(temporary.path());

    let (output, body) = get(&cwd, &source, "relout", "relcache");
    assert_get_succeeded(&output, &body);

    assert!(cwd.join("relout").join("a.txt").exists());
    assert!(cwd.join("relcache").exists());
}

#[test]
fn a_relative_output_and_a_relative_cache_dir_with_separators_both_resolve_against_the_working_directory()
 {
    let temporary = TempDir::new().unwrap();
    let cwd = temporary.path().join("cwd");
    std::fs::create_dir_all(&cwd).unwrap();
    let source = source_tree(temporary.path());

    let (output, body) = get(&cwd, &source, "out/nested", "cache/nested");
    assert_get_succeeded(&output, &body);

    assert!(cwd.join("out").join("nested").join("a.txt").exists());
    assert!(cwd.join("cache").join("nested").exists());
}

fn verify(cwd: &Path, target: &str) -> Output {
    Command::new(binary())
        .arg("verify")
        .arg(target)
        .arg("--json")
        .current_dir(cwd)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

#[test]
fn verify_accepts_a_relative_target_with_a_single_component() {
    let temporary = TempDir::new().unwrap();
    let cwd = temporary.path().join("cwd");
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::create_dir_all(cwd.join("dataset")).unwrap();
    std::fs::write(cwd.join("dataset").join("a.txt"), b"hello").unwrap();

    let output = verify(&cwd, "dataset");
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr was {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let body: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(body["status"].as_str(), Some("verified"));
}

#[test]
fn verify_accepts_a_relative_target_with_a_separator() {
    let temporary = TempDir::new().unwrap();
    let cwd = temporary.path().join("cwd");
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::create_dir_all(cwd.join("nested").join("dataset")).unwrap();
    std::fs::write(cwd.join("nested").join("dataset").join("a.txt"), b"hello").unwrap();

    let output = verify(&cwd, "nested/dataset");
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr was {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let body: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(body["status"].as_str(), Some("verified"));
}
