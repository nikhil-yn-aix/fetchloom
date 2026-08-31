//! Plan on a connected machine, carry the bundle, apply with no network, and
//! get the same tree.

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

use fetchloom_faults::{
    Script, TYPEFLAG_DIRECTORY, TYPEFLAG_REGULAR, TarHeader, TarWriter, TestServer,
};
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

/// A gzip-wrapped tar holding one `0644` file and one `0755` file.
fn archive() -> Vec<u8> {
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

fn run(arguments: &[&str], cache: &Path) -> Output {
    Command::new(binary())
        .current_dir(scratch())
        .args(arguments)
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

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn a_plan_and_a_bundle_made_online_reproduce_the_tree_with_no_network() {
    let temporary = TempDir::new().unwrap();
    let connected = temporary.path().join("connected");
    let carried = temporary.path().join("bundle.tar");
    let lock = temporary.path().join("fetchloom.lock");
    let plan_file = temporary.path().join("dataset.plan");
    let here = temporary.path().join("here");
    let there = temporary.path().join("there");

    let server = TestServer::start(Script::serving(archive())).unwrap();
    let location = format!("{}/tools.tar.gz", server.origin());

    let fetched = run(
        &[
            "get",
            &location,
            "--output",
            here.to_str().unwrap(),
            "--lock",
            lock.to_str().unwrap(),
            "--json",
        ],
        &connected,
    );
    assert!(
        fetched.status.success(),
        "the fetch failed: {}",
        stderr(&fetched)
    );
    let expected = body(&fetched)["tree"].as_str().unwrap().to_owned();

    let planned = run(
        &[
            "plan",
            &location,
            "--output",
            there.to_str().unwrap(),
            "--lock",
            lock.to_str().unwrap(),
        ],
        &connected,
    );
    assert!(
        planned.status.success(),
        "the plan failed: {}",
        stderr(&planned)
    );
    std::fs::write(&plan_file, &planned.stdout).unwrap();

    let exported = run(
        &["cache", "export", carried.to_str().unwrap(), "--json"],
        &connected,
    );
    assert!(
        exported.status.success(),
        "the export failed: {}",
        stderr(&exported)
    );
    assert!(
        body(&exported)["objects"].as_u64().unwrap_or(0) > 0,
        "the bundle holds nothing"
    );

    drop(server);
    let offline = temporary.path().join("offline");
    let imported = run(
        &["cache", "import", carried.to_str().unwrap(), "--json"],
        &offline,
    );
    assert!(
        imported.status.success(),
        "the import failed: {}",
        stderr(&imported)
    );

    let applied = run(
        &[
            "apply",
            plan_file.to_str().unwrap(),
            "--output",
            there.to_str().unwrap(),
            "--offline",
            "--json",
        ],
        &offline,
    );
    assert!(
        applied.status.success(),
        "the offline apply failed: {}",
        stderr(&applied)
    );
    assert_eq!(
        body(&applied)["tree"].as_str().unwrap_or_default(),
        expected,
        "the tree an offline apply produced is not the tree the connected run reported"
    );
    assert_eq!(
        body(&applied)["work"]["requests"],
        0,
        "an offline apply issued a request"
    );
}

#[test]
fn a_plan_states_the_digest_it_will_execute_and_what_no_source_supplied() {
    let temporary = TempDir::new().unwrap();
    let cache = temporary.path().join("cache");
    let lock = temporary.path().join("fetchloom.lock");
    let server = TestServer::start(Script::serving(archive())).unwrap();
    let location = format!("{}/tools.tar.gz", server.origin());
    assert!(
        run(
            &[
                "get",
                &location,
                "--output",
                temporary.path().join("out").to_str().unwrap(),
                "--lock",
                lock.to_str().unwrap(),
            ],
            &cache,
        )
        .status
        .success()
    );

    let planned = run(
        &[
            "plan",
            &location,
            "--output",
            temporary.path().join("elsewhere").to_str().unwrap(),
            "--lock",
            lock.to_str().unwrap(),
        ],
        &cache,
    );
    assert!(planned.status.success(), "{}", stderr(&planned));
    let text = String::from_utf8_lossy(&planned.stdout);
    assert!(text.contains("blake3:"), "a plan states no digest: {text}");
    assert!(
        text.contains("expanded"),
        "a plan did not say what no source supplied: {text}"
    );
}

#[test]
fn planning_a_reference_the_lock_does_not_pin_is_refused() {
    let temporary = TempDir::new().unwrap();
    let server = TestServer::start(Script::serving(archive())).unwrap();
    let planned = run(
        &[
            "plan",
            &format!("{}/tools.tar.gz", server.origin()),
            "--lock",
            temporary.path().join("absent.lock").to_str().unwrap(),
        ],
        &temporary.path().join("cache"),
    );
    assert_eq!(
        planned.status.code(),
        Some(40),
        "planning something with no recorded digest was not refused: {}",
        stderr(&planned)
    );
}

struct Hostile {
    _temporary: TempDir,
    cache: PathBuf,
    bundle: PathBuf,
}

/// A cache holding one object, and the bundle exported from it.
fn exported() -> Hostile {
    let temporary = TempDir::new().unwrap();
    let cache = temporary.path().join("cache");
    let bundle = temporary.path().join("bundle.tar");
    let source = temporary.path().join("tools.tar.gz");
    std::fs::write(&source, archive()).unwrap();
    assert!(
        run(
            &[
                "get",
                source.to_str().unwrap(),
                "--output",
                temporary.path().join("out").to_str().unwrap(),
            ],
            &cache,
        )
        .status
        .success()
    );
    assert!(
        run(&["cache", "export", bundle.to_str().unwrap()], &cache)
            .status
            .success()
    );
    Hostile {
        _temporary: temporary,
        cache,
        bundle,
    }
}

fn objects_in(cache: &Path) -> usize {
    std::fs::read_dir(cache.join("objects")).map_or(0, |entries| entries.flatten().count())
}

fn import_into(bundle: &Path, cache: &Path) -> Output {
    run(&["cache", "import", bundle.to_str().unwrap()], cache)
}

#[test]
fn a_truncated_bundle_publishes_nothing() {
    let subject = exported();
    let bytes = std::fs::read(&subject.bundle).unwrap();
    let cut = subject.bundle.with_file_name("cut.tar");
    std::fs::write(&cut, &bytes[..bytes.len() / 2]).unwrap();

    let cold = subject.cache.with_file_name("cold");
    let outcome = import_into(&cut, &cold);
    assert!(!outcome.status.success(), "a truncated bundle imported");
    assert!(
        stderr(&outcome).contains("integrity.truncated"),
        "a truncated bundle did not fail by name: {}",
        stderr(&outcome)
    );
    assert_eq!(
        objects_in(&cold),
        0,
        "a truncated bundle published an object"
    );
}

#[test]
fn a_bundle_with_a_flipped_byte_publishes_nothing() {
    let subject = exported();
    let mut bytes = std::fs::read(&subject.bundle).unwrap();
    let at = 512 + 4;
    bytes[at] ^= 0xff;
    let flipped = subject.bundle.with_file_name("flipped.tar");
    std::fs::write(&flipped, &bytes).unwrap();

    let cold = subject.cache.with_file_name("cold-flipped");
    let outcome = import_into(&flipped, &cold);
    assert!(
        !outcome.status.success(),
        "a bundle with a flipped byte imported"
    );
    assert!(
        stderr(&outcome).contains("integrity.mismatch"),
        "a flipped byte did not fail by name: {}",
        stderr(&outcome)
    );
    assert_eq!(objects_in(&cold), 0, "a flipped bundle published an object");
}

#[test]
fn a_bundle_naming_a_digest_it_does_not_hold_publishes_nothing() {
    let temporary = TempDir::new().unwrap();
    let bundle = temporary.path().join("lying.tar");
    let mut writer = TarWriter::new();
    let mut header = TarHeader::ustar(
        b"0000000000000000000000000000000000000000000000000000000000000000",
        TYPEFLAG_REGULAR,
    );
    header.set_size(5).set_mode(0o444);
    writer.push(&header, b"hello");
    std::fs::write(&bundle, writer.finish()).unwrap();

    let cache = temporary.path().join("cache");
    let outcome = import_into(&bundle, &cache);
    assert!(
        !outcome.status.success(),
        "a bundle that lied about a digest imported"
    );
    assert!(
        stderr(&outcome).contains("integrity.mismatch"),
        "a lying digest did not fail by name: {}",
        stderr(&outcome)
    );
    assert_eq!(objects_in(&cache), 0, "a lying bundle published an object");
}

#[test]
fn a_bundle_member_naming_a_path_publishes_nothing() {
    let temporary = TempDir::new().unwrap();
    let bundle = temporary.path().join("traversal.tar");
    let mut writer = TarWriter::new();
    let mut header = TarHeader::ustar(b"../../etc/passwd", TYPEFLAG_REGULAR);
    header.set_size(5).set_mode(0o444);
    writer.push(&header, b"hello");
    std::fs::write(&bundle, writer.finish()).unwrap();

    let cache = temporary.path().join("cache");
    let outcome = import_into(&bundle, &cache);
    assert!(!outcome.status.success(), "a bundle naming a path imported");
    assert!(
        stderr(&outcome).contains("archive.unsafe_path"),
        "a traversal did not fail by name: {}",
        stderr(&outcome)
    );
    assert_eq!(
        objects_in(&cache),
        0,
        "a traversal bundle published an object"
    );
    assert!(
        !temporary.path().join("etc").exists(),
        "a traversal escaped the cache"
    );
}

#[test]
fn a_bundle_imports_the_objects_it_carries() {
    let subject = exported();
    let cold = subject.cache.with_file_name("cold-good");
    let outcome = import_into(&subject.bundle, &cold);
    assert!(
        outcome.status.success(),
        "a good bundle failed: {}",
        stderr(&outcome)
    );
    assert_eq!(
        objects_in(&cold),
        objects_in(&subject.cache),
        "an imported cache holds a different number of objects than the one it came from"
    );
}
