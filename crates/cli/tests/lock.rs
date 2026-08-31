//! Contract tests over the lock: what it pins, what it refuses to hold, and
//! what a locked run does when resolution differs from it.

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
use flate2 as _;
use serde as _;
use toml as _;
#[cfg(windows)]
use windows_sys as _;

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use fetchloom_faults::{Reply, Script, TYPEFLAG_REGULAR, TarHeader, TarWriter, TestServer};
use tempfile::TempDir;

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_fetchloom")
}

fn object(length: usize) -> Vec<u8> {
    (0..length)
        .map(|value| u8::try_from(value % 251).unwrap_or(0))
        .collect()
}

struct Run {
    output: Output,
}

impl Run {
    fn body(&self) -> serde_json::Value {
        let text = String::from_utf8_lossy(&self.output.stdout);
        serde_json::from_str(text.trim()).unwrap_or_else(|error| {
            panic!(
                "the result was not JSON ({error}): {text}\nstderr: {}",
                String::from_utf8_lossy(&self.output.stderr)
            )
        })
    }

    fn code(&self) -> i32 {
        self.output.status.code().unwrap_or(-1)
    }

    fn stdout(&self) -> String {
        String::from_utf8_lossy(&self.output.stdout).into_owned()
    }

    fn stderr(&self) -> String {
        String::from_utf8_lossy(&self.output.stderr).into_owned()
    }
}

fn get(reference: &str, destination: &Path, cache: &Path, lock: &Path, extra: &[&str]) -> Run {
    let output = Command::new(binary())
        .current_dir(scratch())
        .arg("get")
        .arg(reference)
        .arg("--output")
        .arg(destination)
        .arg("--lock")
        .arg(lock)
        .arg("--json")
        .args(extra)
        .env("FETCHLOOM_CACHE_DIR", cache)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    Run { output }
}

struct Scene {
    _temporary: TempDir,
    destination: PathBuf,
    cache: PathBuf,
    lock: PathBuf,
}

fn scene() -> Scene {
    let temporary = TempDir::new().unwrap();
    Scene {
        destination: temporary.path().join("out"),
        cache: temporary.path().join("cache"),
        lock: temporary.path().join("fetchloom.lock"),
        _temporary: temporary,
    }
}

#[test]
fn a_run_writes_a_lock_that_pins_the_bytes_and_the_tree() {
    let bytes = object(4096);
    let server = TestServer::start(Script::serving(bytes.clone())).unwrap();
    let scene = scene();
    let location = format!("{}/object.bin", server.origin());

    let run = get(
        &location,
        &scene.destination,
        &scene.cache,
        &scene.lock,
        &[],
    );
    assert_eq!(
        run.code(),
        0,
        "the run failed: {} {}",
        run.stderr(),
        String::from_utf8_lossy(&run.output.stdout)
    );

    let text = std::fs::read_to_string(&scene.lock).unwrap();
    assert!(
        text.contains("object.bin"),
        "the lock named no dataset: {text}"
    );
    assert!(
        text.contains("blake3:"),
        "the lock pinned no digest: {text}"
    );
    assert!(
        text.contains("sha256:"),
        "the lock recorded no interop digest: {text}"
    );
    assert!(
        text.contains(run.body()["tree"].as_str().unwrap_or_default()),
        "the lock did not record the tree the run reported: {text}"
    );
}

#[test]
fn a_lock_holds_nothing_that_belongs_to_one_machine() {
    let bytes = object(4096);
    let server = TestServer::start(Script::serving(bytes)).unwrap();
    let scene = scene();
    let location = format!("{}/object.bin", server.origin());
    assert_eq!(
        get(
            &location,
            &scene.destination,
            &scene.cache,
            &scene.lock,
            &[]
        )
        .code(),
        0
    );

    let text = std::fs::read_to_string(&scene.lock).unwrap();
    let forbidden: Vec<String> = vec![
        scene.destination.display().to_string(),
        scene.cache.display().to_string(),
        std::env::var("USERNAME").unwrap_or_else(|_| "\u{0}no-user".to_owned()),
        std::env::var("USER").unwrap_or_else(|_| "\u{0}no-user".to_owned()),
        std::env::var("COMPUTERNAME").unwrap_or_else(|_| "\u{0}no-host".to_owned()),
        std::env::var("HOSTNAME").unwrap_or_else(|_| "\u{0}no-host".to_owned()),
    ];
    for value in forbidden {
        assert!(
            !text.contains(&value),
            "the lock holds {value}, which belongs to this machine: {text}"
        );
    }
    assert!(
        !text.contains("completed_at") && !text.contains("destination"),
        "the lock holds a field that belongs in a receipt: {text}"
    );
    assert!(
        !text.contains('\\'),
        "the lock holds a backslash, which is how a path leaks in: {text}"
    );
}

#[test]
fn two_runs_of_one_source_write_the_same_lock_bytes() {
    let bytes = object(4096);
    let server = TestServer::start(Script::serving(bytes)).unwrap();
    let first = scene();
    let second = scene();
    let location = format!("{}/object.bin", server.origin());
    assert_eq!(
        get(
            &location,
            &first.destination,
            &first.cache,
            &first.lock,
            &[]
        )
        .code(),
        0
    );
    assert_eq!(
        get(
            &location,
            &second.destination,
            &second.cache,
            &second.lock,
            &[]
        )
        .code(),
        0
    );
    assert_eq!(
        std::fs::read(&first.lock).unwrap(),
        std::fs::read(&second.lock).unwrap(),
        "two runs of one source wrote two different locks"
    );
}

#[test]
fn a_locked_run_with_no_entry_is_refused_on_policy() {
    let bytes = object(4096);
    let server = TestServer::start(Script::serving(bytes)).unwrap();
    let scene = scene();
    let location = format!("{}/object.bin", server.origin());
    let run = get(
        &location,
        &scene.destination,
        &scene.cache,
        &scene.lock,
        &["--locked"],
    );
    assert_eq!(
        run.code(),
        40,
        "a locked run with nothing to compare against did not fail on policy: {}",
        run.stderr()
    );
}

#[test]
fn a_locked_run_of_an_unchanged_source_asks_the_source_for_nothing() {
    let bytes = object(4096);
    let server = TestServer::start(Script::serving(bytes)).unwrap();
    let scene = scene();
    let location = format!("{}/object.bin", server.origin());
    assert_eq!(
        get(
            &location,
            &scene.destination,
            &scene.cache,
            &scene.lock,
            &[]
        )
        .code(),
        0
    );

    std::fs::remove_dir_all(&scene.destination).unwrap();
    let again = get(
        &location,
        &scene.destination,
        &scene.cache,
        &scene.lock,
        &["--locked"],
    );
    assert_eq!(again.code(), 0, "the locked run failed: {}", again.stderr());
    assert_eq!(
        again.body()["work"]["requests"],
        0,
        "a locked run asked a source for bytes the cache already held"
    );
}

#[test]
fn a_locked_run_against_changed_content_fails_on_integrity() {
    let bytes = object(4096);
    let mut script = Script::serving(bytes).replying(vec![Reply::Whole]);
    script.then = Reply::Flipped { offset: 17 };
    let server = TestServer::start(script).unwrap();
    let scene = scene();
    let location = format!("{}/object.bin", server.origin());
    assert_eq!(
        get(
            &location,
            &scene.destination,
            &scene.cache,
            &scene.lock,
            &[]
        )
        .code(),
        0
    );

    std::fs::remove_dir_all(&scene.destination).unwrap();
    let cold = scene.cache.with_file_name("cold-cache");
    let run = get(
        &location,
        &scene.destination,
        &cold,
        &scene.lock,
        &["--locked"],
    );
    assert_eq!(
        run.code(),
        30,
        "a source serving changed bytes under a lock did not fail on integrity: {}",
        run.stderr()
    );
    assert!(
        !scene.destination.exists(),
        "a run that failed on integrity published a destination"
    );
}

#[test]
fn a_directory_source_writes_no_lock_and_says_so() {
    let temporary = TempDir::new().unwrap();
    let source = temporary.path().join("tree");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(source.join("one.txt"), b"one\n").unwrap();
    let scene = scene();

    let output = Command::new(binary())
        .current_dir(scratch())
        .arg("get")
        .arg(&source)
        .arg("--output")
        .arg(&scene.destination)
        .arg("--lock")
        .arg(&scene.lock)
        .arg("--events")
        .arg("-")
        .env("FETCHLOOM_CACHE_DIR", &scene.cache)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(
        !scene.lock.exists(),
        "a directory source wrote a lock naming bytes it cannot pin"
    );
    let events = String::from_utf8_lossy(&output.stdout);
    assert!(
        events.contains("\"event\":\"degrade\"") && events.contains("lock"),
        "a run that wrote no lock did not say so: {events}"
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

#[test]
fn one_source_writes_one_lock_on_every_platform() {
    let temporary = TempDir::new().unwrap();
    let source = temporary.path().join("silesia.tar.gz");
    std::fs::write(&source, FIXED_ARCHIVE).unwrap();
    let scene = scene();
    let run = get(
        source.to_str().unwrap(),
        &scene.destination,
        &scene.cache,
        &scene.lock,
        &[],
    );
    assert_eq!(
        run.code(),
        0,
        "the run failed: {} {}",
        run.stderr(),
        String::from_utf8_lossy(&run.output.stdout)
    );
    assert_eq!(
        std::fs::read_to_string(&scene.lock).unwrap(),
        RECORDED_LOCK,
        "the lock this platform writes is not the one every platform writes"
    );
}

/// One archive, written out byte for byte rather than built here.
const FIXED_ARCHIVE: &[u8] = &[
    0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0xff, 0xed, 0xcd, 0x31, 0x0a, 0x83, 0x40,
    0x10, 0x05, 0xd0, 0xa9, 0x73, 0x0a, 0x4f, 0x20, 0x2b, 0xa8, 0xf7, 0x09, 0x21, 0x60, 0x21, 0x28,
    0xba, 0x42, 0x8e, 0x9f, 0x4d, 0xaa, 0x90, 0x5e, 0x41, 0x7c, 0xaf, 0xf9, 0xc3, 0xff, 0xc5, 0x3c,
    0xa6, 0x65, 0xde, 0xd6, 0x3a, 0xbf, 0x72, 0xec, 0x26, 0x15, 0x7d, 0xdb, 0x7e, 0xb3, 0xf8, 0xcf,
    0xcf, 0xfa, 0x73, 0x97, 0xbe, 0x49, 0xa9, 0xe9, 0xa2, 0x4a, 0x71, 0x80, 0x6d, 0xcd, 0xf7, 0xa5,
    0xbc, 0x8f, 0x6b, 0x1a, 0x9e, 0xe3, 0x38, 0xdd, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x80, 0x93, 0x79, 0x03, 0x2d, 0xa8, 0xc8, 0x0a, 0x00, 0x28, 0x00, 0x00,
];

/// The lock a run against `FIXED_ARCHIVE` writes, on every platform.
const RECORDED_LOCK: &str = concat!(
    "datasets:
",
    "  silesia.tar.gz:
",
    "    artifacts:
",
    "      silesia.tar.gz:
",
    "        digest: \"blake3:cf52b6abae6d66eb56df9175634fcdd006c25cb564f60cb9e2f886b1f7af0c11\"
",
    "        interop: \"sha256:967748294c12cf02139f121c7eb17dfef4e619d225f6bd80ad25bc555249a9ec\"
",
    "        layout: \"keep\"
",
    "        size: 109
",
    "    manifest: \"blake3:f78758b5f44fe17e2af6ba728667fa2ad90b060851bb8a697f6f5403b795267a\"
",
    "    tree: \"blake3:29ab1d09d9a21492d3cad40ca3adb1204739e6fa9fe2f51b1ac0776d836a8cc8\"
",
);

#[test]
fn a_locked_run_asking_for_a_different_layout_says_the_alias_moved() {
    let server = TestServer::start(Script::serving(one_member_tar())).unwrap();
    let scene = scene();
    let location = format!("{}/object.tar", server.origin());
    assert_eq!(
        get(
            &location,
            &scene.destination,
            &scene.cache,
            &scene.lock,
            &[]
        )
        .code(),
        0
    );
    let run = get(
        &location,
        &scene.destination,
        &scene.cache,
        &scene.lock,
        &["--locked", "--layout", "flatten:1"],
    );

    assert_eq!(
        run.code(),
        10,
        "a locked run under a different layout did not fail on resolution: {}",
        run.stderr()
    );
    assert!(
        run.stderr().contains("alias.unstable") || run.stdout().contains("alias.unstable"),
        "the refusal did not name the kind: {} {}",
        run.stdout(),
        run.stderr()
    );
}

fn one_member_tar() -> Vec<u8> {
    let body = object(512);
    let mut header = TarHeader::ustar(b"pkg-1.0/data.bin", TYPEFLAG_REGULAR);
    header.set_size(body.len() as u64);
    let mut writer = TarWriter::new();
    writer.push(&header, &body);
    writer.finish()
}
