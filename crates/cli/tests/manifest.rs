//! A reference that names a manifest resolves to every artifact the manifest
//! names.

#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test assertions, where the run that failed is the message"
)]

use clap as _;
use clap_complete as _;
use ctrlc as _;
use fetchloom_archive as _;
use fetchloom_cache as _;
use fetchloom_cli as _;
use fetchloom_engine as _;
use fetchloom_platform as _;
use fetchloom_sources as _;
use serde as _;
use toml as _;
#[cfg(windows)]
use windows_sys as _;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use fetchloom_faults::{TYPEFLAG_DIRECTORY, TYPEFLAG_REGULAR, TarHeader, TarWriter};
use flate2::Compression;
use flate2::write::GzEncoder;
use tempfile::TempDir;

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_fetchloom")
}

fn scratch() -> &'static Path {
    static SCRATCH: std::sync::OnceLock<TempDir> = std::sync::OnceLock::new();
    SCRATCH.get_or_init(|| TempDir::new().unwrap()).path()
}

/// A gzip-wrapped tar holding one directory and one file under it.
fn archive(directory: &str, name: &str, body: &[u8]) -> Vec<u8> {
    let mut writer = TarWriter::new();
    writer.push(
        &TarHeader::ustar(format!("{directory}/").as_bytes(), TYPEFLAG_DIRECTORY),
        b"",
    );
    let mut entry = TarHeader::ustar(format!("{directory}/{name}").as_bytes(), TYPEFLAG_REGULAR);
    entry.set_size(body.len() as u64).set_mode(0o644);
    writer.push(&entry, body);
    let tar = writer.finish();
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&tar).unwrap();
    encoder.finish().unwrap()
}

struct Scene {
    _temporary: TempDir,
    root: PathBuf,
    manifest: PathBuf,
    destination: PathBuf,
    cache: PathBuf,
    lock: PathBuf,
}

/// A dataset of two artifacts, each a small archive beside the manifest.
fn two_artifacts(body: &str) -> Scene {
    let temporary = TempDir::new().unwrap();
    let root = temporary.path().to_path_buf();
    std::fs::write(
        root.join("tools.tar.gz"),
        archive("tools", "run.sh", b"one\n"),
    )
    .unwrap();
    std::fs::write(
        root.join("data.tar.gz"),
        archive("data", "rows.csv", body.as_bytes()),
    )
    .unwrap();
    let manifest = root.join("dataset.yaml");
    std::fs::write(
        &manifest,
        "name: pair\nartifacts:\n  - id: tools\n    sources: [tools.tar.gz]\n  - id: data\n    sources: [data.tar.gz]\n",
    )
    .unwrap();
    Scene {
        destination: root.join("out"),
        cache: root.join("cache"),
        lock: root.join("fetchloom.lock"),
        manifest,
        root,
        _temporary: temporary,
    }
}

fn get(scene: &Scene, extra: &[&str]) -> Output {
    Command::new(binary())
        .current_dir(scratch())
        .arg("get")
        .arg(&scene.manifest)
        .arg("--output")
        .arg(&scene.destination)
        .arg("--lock")
        .arg(&scene.lock)
        .arg("--json")
        .args(extra)
        .env("FETCHLOOM_CACHE_DIR", &scene.cache)
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

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn a_manifest_materializes_every_artifact_it_names() {
    let scene = two_artifacts("a,b\n");
    let run = get(&scene, &[]);
    assert!(run.status.success(), "the run failed: {}", stderr(&run));
    assert_eq!(
        std::fs::read_to_string(scene.destination.join("tools").join("run.sh")).unwrap(),
        "one\n"
    );
    assert_eq!(
        std::fs::read_to_string(scene.destination.join("data").join("rows.csv")).unwrap(),
        "a,b\n"
    );
    assert_eq!(body(&run)["dataset"], "pair");
}

#[test]
fn a_manifest_pins_every_artifact_it_names() {
    let scene = two_artifacts("a,b\n");
    assert!(get(&scene, &[]).status.success());
    let text = std::fs::read_to_string(&scene.lock).unwrap();
    assert!(text.contains("pair:"), "the lock named no dataset: {text}");
    assert!(text.contains("tools:"), "the lock pinned no tools: {text}");
    assert!(text.contains("data:"), "the lock pinned no data: {text}");
    assert_eq!(
        text.matches("interop:").count(),
        2,
        "the lock did not record an interop digest for each artifact: {text}"
    );
}

#[test]
fn a_second_run_of_a_manifest_writes_nothing() {
    let scene = two_artifacts("a,b\n");
    assert!(get(&scene, &[]).status.success());
    let again = get(&scene, &[]);
    assert!(again.status.success(), "{}", stderr(&again));
    let again = body(&again);
    assert_eq!(again["status"], "unchanged");
    assert_eq!(again["work"]["bytes_written"], 0);
}

#[test]
fn an_artifact_that_cannot_be_resolved_publishes_nothing_and_pins_what_verified() {
    let scene = two_artifacts("a,b\n");
    std::fs::write(
        &scene.manifest,
        "name: pair\nartifacts:\n  - id: tools\n    sources: [tools.tar.gz]\n  - id: missing\n    sources: [absent.tar.gz]\n",
    )
    .unwrap();

    let run = get(&scene, &[]);
    assert!(
        !run.status.success(),
        "a run whose artifact could not be resolved succeeded"
    );
    assert!(
        !scene.destination.exists(),
        "a run that failed published a destination"
    );
    let text = std::fs::read_to_string(&scene.lock).unwrap();
    assert!(
        text.contains("tools:"),
        "the lock did not pin the artifact that did verify: {text}"
    );
    assert!(
        !text.contains("tree:"),
        "a lock recorded a tree for a run that materialized nothing: {text}"
    );
    let receipts = scene.cache.join("receipts");
    let held = std::fs::read_dir(&receipts).map_or(0, |entries| entries.flatten().count());
    assert_eq!(held, 0, "a run that materialized nothing wrote a receipt");
}

#[test]
fn a_declared_digest_the_bytes_do_not_have_fails_on_integrity() {
    let scene = two_artifacts("a,b\n");
    std::fs::write(
        &scene.manifest,
        "name: pair\nartifacts:\n  - id: tools\n    sources: [tools.tar.gz]\n    digest:\n      blake3: \"blake3:0000000000000000000000000000000000000000000000000000000000000000\"\n",
    )
    .unwrap();
    let run = get(&scene, &[]);
    assert_eq!(
        run.status.code(),
        Some(30),
        "a declared digest the bytes do not have was accepted: {}",
        stderr(&run)
    );
    assert!(!scene.destination.exists());
}

#[test]
fn a_manifest_that_does_not_parse_says_so() {
    let scene = two_artifacts("a,b\n");
    std::fs::write(
        &scene.manifest,
        "name: pair\nartifacts:\n  - id: tools\n    whatever: 1\n",
    )
    .unwrap();
    let run = get(&scene, &[]);
    assert_eq!(run.status.code(), Some(10), "{}", stderr(&run));
    let said = String::from_utf8_lossy(&run.stdout).into_owned() + &stderr(&run);
    assert!(
        said.contains("manifest.invalid"),
        "a manifest that does not parse did not say so: {said}"
    );
}

#[test]
fn two_artifacts_landing_on_one_path_is_a_collision() {
    let scene = two_artifacts("a,b\n");
    std::fs::write(
        scene.root.join("same.tar.gz"),
        archive("tools", "run.sh", b"two\n"),
    )
    .unwrap();
    std::fs::write(
        &scene.manifest,
        "name: pair\nartifacts:\n  - id: tools\n    sources: [tools.tar.gz]\n  - id: other\n    sources: [same.tar.gz]\n",
    )
    .unwrap();
    let run = get(&scene, &[]);
    assert_eq!(
        run.status.code(),
        Some(70),
        "two artifacts writing one path were not refused: {}",
        stderr(&run)
    );
    assert!(!scene.destination.exists());
}
