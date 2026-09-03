//! Contract tests for `doctor` and `why`.

#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test setup, where a failure to run the binary is the assertion"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Output;

use clap as _;
use clap_complete as _;
use ctrlc as _;
use fetchloom_archive as _;
use fetchloom_cache as _;
use fetchloom_cli as _;
use fetchloom_engine as _;
use fetchloom_faults::{Script, TestServer};
use fetchloom_platform as _;
use fetchloom_sources as _;
use fetchloom_view as _;
use flate2 as _;
#[cfg(unix)]
use rustix as _;
use serde as _;
use toml as _;
#[cfg(windows)]
use windows_sys as _;

mod support;

use tempfile::TempDir;

fn run_in(directory: &Path, cache: &Path, extra: &[(&str, &str)], arguments: &[&str]) -> Output {
    let mut command = support::fetchloom();
    command
        .current_dir(directory)
        .args(arguments)
        .env("FETCHLOOM_CACHE_DIR", cache);
    for (key, value) in extra {
        command.env(key, value);
    }
    command.output().unwrap()
}

fn source(under: &Path, name: &str) -> std::path::PathBuf {
    let source = under.join(name);
    std::fs::create_dir_all(source.join("inner")).unwrap();
    std::fs::write(source.join("one.txt"), b"the first file").unwrap();
    std::fs::write(source.join("inner").join("two.txt"), b"the second file").unwrap();
    source
}

struct Snapshot {
    dirs: BTreeSet<String>,
    files: BTreeMap<String, Vec<u8>>,
}

fn snapshot(root: &Path) -> Option<Snapshot> {
    if !root.exists() {
        return None;
    }
    let mut dirs = BTreeSet::new();
    let mut files = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(current) = stack.pop() {
        for entry in std::fs::read_dir(&current).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let relative = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if path.is_dir() {
                dirs.insert(relative);
                stack.push(path);
            } else {
                let bytes = std::fs::read(&path).unwrap();
                files.insert(relative, bytes);
            }
        }
    }
    Some(Snapshot { dirs, files })
}

fn assert_unchanged(before: Option<&Snapshot>, after: Option<&Snapshot>, label: &str) {
    match (before, after) {
        (None, None) => {}
        (None, Some(_)) => panic!("{label}: doctor created the cache directory"),
        (Some(_), None) => panic!("{label}: doctor removed the cache directory"),
        (Some(before), Some(after)) => {
            assert_eq!(before.dirs, after.dirs, "{label}: the directories changed");
            assert_eq!(before.files, after.files, "{label}: a file changed");
        }
    }
}

fn healthy_cache(temporary: &Path) -> std::path::PathBuf {
    let cache = temporary.join("cache");
    let directory = temporary.join("healthy-source");
    let output = run_in(
        temporary,
        &cache,
        &[],
        &["get", source(&directory, "s").to_str().unwrap()],
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "setup get failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    cache
}

#[test]
fn doctor_changes_nothing_in_a_healthy_cache() {
    let temporary = TempDir::new().unwrap();
    let cache = healthy_cache(temporary.path());
    let before = snapshot(&cache);

    let output = run_in(temporary.path(), &cache, &[], &["doctor"]);

    let after = snapshot(&cache);
    assert_unchanged(before.as_ref(), after.as_ref(), "healthy cache");
    let _ = output;
}

#[test]
fn doctor_changes_nothing_when_the_format_does_not_match() {
    let temporary = TempDir::new().unwrap();
    let cache = healthy_cache(temporary.path());
    std::fs::write(
        cache.join("format"),
        "blake3:0000000000000000000000000000000000000000000000000000000000000000\n",
    )
    .unwrap();
    let before = snapshot(&cache);

    run_in(temporary.path(), &cache, &[], &["doctor"]);

    let after = snapshot(&cache);
    assert_unchanged(before.as_ref(), after.as_ref(), "mismatched format");
}

#[test]
fn doctor_changes_nothing_when_the_cache_directory_does_not_exist() {
    let temporary = TempDir::new().unwrap();
    let cache = temporary.path().join("never-created");
    let before = snapshot(&cache);
    assert!(before.is_none());

    run_in(temporary.path(), &cache, &[], &["doctor"]);

    let after = snapshot(&cache);
    assert!(after.is_none(), "doctor created the cache directory");
}

#[cfg(unix)]
#[test]
fn doctor_changes_nothing_when_the_cache_directory_is_read_only() {
    use std::os::unix::fs::PermissionsExt;

    let temporary = TempDir::new().unwrap();
    let cache = healthy_cache(temporary.path());
    let before = snapshot(&cache);

    let mut permissions = std::fs::metadata(&cache).unwrap().permissions();
    permissions.set_mode(0o555);
    std::fs::set_permissions(&cache, permissions).unwrap();

    let output = run_in(temporary.path(), &cache, &[], &["doctor"]);

    let after = snapshot(&cache);
    assert_unchanged(before.as_ref(), after.as_ref(), "read-only cache");

    let mut restored = std::fs::metadata(&cache).unwrap().permissions();
    restored.set_mode(0o755);
    std::fs::set_permissions(&cache, restored).unwrap();

    assert_eq!(
        output.status.code(),
        Some(50),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn doctor_leaves_no_config_file_behind() {
    let temporary = TempDir::new().unwrap();
    let cache = temporary.path().join("cache");

    run_in(temporary.path(), &cache, &[], &["doctor"]);

    assert!(!temporary.path().join("fetchloom.toml").exists());
}

#[test]
fn doctor_exits_zero_on_a_healthy_environment() {
    let temporary = TempDir::new().unwrap();
    let cache = healthy_cache(temporary.path());

    let output = run_in(temporary.path(), &cache, &[], &["doctor"]);

    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(unix)]
#[test]
fn doctor_exits_fifty_when_something_is_actionable() {
    let temporary = TempDir::new().unwrap();
    let cache = healthy_cache(temporary.path());
    std::fs::write(
        cache.join("format"),
        "blake3:0000000000000000000000000000000000000000000000000000000000000000\n",
    )
    .unwrap();

    let output = run_in(temporary.path(), &cache, &[], &["doctor"]);

    assert_eq!(
        output.status.code(),
        Some(50),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn doctor_never_prints_a_secret() {
    let temporary = TempDir::new().unwrap();
    let cache = healthy_cache(temporary.path());
    let sentinel = "sentinel-value-doctor-must-never-print-9f2c";

    let output = run_in(
        temporary.path(),
        &cache,
        &[("FETCHLOOM_TOKEN_STORAGE_GOOGLEAPIS_COM", sentinel)],
        &["doctor", "--verbose", "--verbose"],
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stdout.contains(sentinel),
        "stdout leaked the secret: {stdout}"
    );
    assert!(
        !stderr.contains(sentinel),
        "stderr leaked the secret: {stderr}"
    );
}

#[test]
fn doctor_json_produces_parseable_json() {
    let temporary = TempDir::new().unwrap();
    let cache = healthy_cache(temporary.path());

    let output = run_in(temporary.path(), &cache, &[], &["doctor", "--json"]);

    let parsed: serde_json::Value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|reason| panic!("{reason}: {}", String::from_utf8_lossy(&output.stdout)));
    assert!(parsed.get("checks").is_some());
}

#[test]
fn doctor_offline_issues_no_request() {
    let server = TestServer::start(Script::serving(b"unused".to_vec())).unwrap();
    let temporary = TempDir::new().unwrap();
    let cache = temporary.path().join("cache");

    run_in(temporary.path(), &cache, &[], &["doctor", "--offline"]);

    assert!(server.received().is_empty(), "doctor reached the network");
}

#[test]
fn why_on_a_reference_with_no_recorded_run_exits_zero() {
    let temporary = TempDir::new().unwrap();
    let cache = temporary.path().join("cache");
    let directory = source(temporary.path(), "never-fetched");

    let output = run_in(
        temporary.path(),
        &cache,
        &[],
        &["why", directory.to_str().unwrap()],
    );

    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.contains("no run has been recorded") || stderr.contains("no run has been recorded"),
        "stdout: {stdout}\nstderr: {stderr}"
    );
}

#[test]
fn why_after_a_real_get_reports_the_recorded_source_and_trust() {
    let server = TestServer::start(Script::serving(b"the object's bytes".to_vec())).unwrap();
    let temporary = TempDir::new().unwrap();
    let cache = temporary.path().join("cache");
    let reference = format!("{}/object", server.origin());

    let got = run_in(temporary.path(), &cache, &[], &["get", &reference]);
    assert_eq!(
        got.status.code(),
        Some(0),
        "setup get failed: {}",
        String::from_utf8_lossy(&got.stderr)
    );

    let output = run_in(
        temporary.path(),
        &cache,
        &[],
        &["why", &reference, "--json"],
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let body: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(body["source"]["recorded"], serde_json::json!(true));
    assert_eq!(body["trust"]["recorded"], serde_json::json!(true));
    assert!(
        body["source"]["source"]
            .as_str()
            .is_some_and(|found| found.contains("object"))
    );
}

#[test]
fn why_does_not_invent_a_source_for_a_reference_never_fetched() {
    let temporary = TempDir::new().unwrap();
    let cache = temporary.path().join("cache");
    let fetched = source(temporary.path(), "was-fetched");
    let never = source(temporary.path(), "was-never-fetched");

    let got = run_in(
        temporary.path(),
        &cache,
        &[],
        &["get", fetched.to_str().unwrap()],
    );
    assert_eq!(got.status.code(), Some(0));

    let output = run_in(
        temporary.path(),
        &cache,
        &[],
        &["why", never.to_str().unwrap(), "--json"],
    );
    assert_eq!(output.status.code(), Some(0));
    let body: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(body["source"]["recorded"], serde_json::json!(false));
    assert!(body["source"]["source"].is_null());
}

#[test]
fn why_on_unparseable_input_exits_ten() {
    let temporary = TempDir::new().unwrap();
    let cache = temporary.path().join("cache");

    let output = run_in(
        temporary.path(),
        &cache,
        &[],
        &[
            "why",
            "s3://bucket/without/a/signing/scheme/this/build/reads",
        ],
    );

    assert_eq!(
        output.status.code(),
        Some(10),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("reference.unresolved"),
        "stderr was {stderr}"
    );
}
