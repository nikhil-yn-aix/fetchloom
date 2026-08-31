//! What a consumer of the event stream alone can reconstruct about a run.

#![expect(
    clippy::unwrap_used,
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
#[cfg(unix)]
use rustix as _;
use serde as _;
use toml as _;
#[cfg(windows)]
use windows_sys as _;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use fetchloom_faults::{TYPEFLAG_REGULAR, TarHeader, TarWriter};
use tempfile::TempDir;

/// What a tar holding `hello.txt` decompresses to.
const GREETING: &[u8] = b"hello\n";

fn greeting_tar() -> Vec<u8> {
    let mut header = TarHeader::ustar(b"hello.txt", TYPEFLAG_REGULAR);
    header.set_size(GREETING.len() as u64);
    let mut writer = TarWriter::new();
    writer.push(&header, GREETING);
    writer.finish()
}

/// A tar whose one member climbs out of the destination.
fn escaping_tar() -> Vec<u8> {
    let mut header = TarHeader::ustar(b"../escape.txt", TYPEFLAG_REGULAR);
    header.set_size(GREETING.len() as u64);
    let mut writer = TarWriter::new();
    writer.push(&header, GREETING);
    writer.finish()
}

fn gzip(content: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(content).unwrap();
    encoder.finish().unwrap()
}

struct Run {
    output: Output,
    stream: String,
}

impl Run {
    fn events(&self) -> Vec<serde_json::Value> {
        self.stream
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn names(&self) -> Vec<String> {
        self.events()
            .iter()
            .map(|event| event["event"].as_str().unwrap_or_default().to_owned())
            .collect()
    }

    fn only(&self, name: &str) -> serde_json::Value {
        let found: Vec<serde_json::Value> = self
            .events()
            .into_iter()
            .filter(|event| event["event"] == name)
            .collect();
        assert_eq!(
            found.len(),
            1,
            "the stream carried {} of {name}: {:?}",
            found.len(),
            self.names()
        );
        found.into_iter().next().unwrap_or(serde_json::Value::Null)
    }
}

struct Workspace {
    temporary: TempDir,
}

impl Workspace {
    fn new() -> Self {
        Self {
            temporary: TempDir::new().unwrap(),
        }
    }

    fn path(&self) -> &Path {
        self.temporary.path()
    }

    fn write(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let target = self.path().join(name);
        std::fs::write(&target, bytes).unwrap();
        target
    }

    fn run(&self, arguments: &[&str]) -> Run {
        let stream = self
            .path()
            .join(format!("stream-{}.ndjson", arguments.len()));
        let output = Command::new(env!("CARGO_BIN_EXE_fetchloom"))
            .current_dir(self.path())
            .args(arguments)
            .arg("--events")
            .arg(&stream)
            .env("FETCHLOOM_CACHE_DIR", self.path().join("cache"))
            .stdin(Stdio::null())
            .output()
            .unwrap();
        Run {
            output,
            stream: std::fs::read_to_string(&stream).unwrap_or_default(),
        }
    }
}

#[test]
fn a_cold_archive_fetch_names_the_extraction_and_the_cache_decision() {
    let workspace = Workspace::new();
    let archive = workspace.write("sample.tar.gz", &gzip(&greeting_tar()));
    let run = workspace.run(&["get", archive.to_str().unwrap(), "--output", "out"]);
    assert_eq!(run.output.status.code(), Some(0));

    let names = run.names();
    for wanted in [
        "run.start",
        "resolve.start",
        "resolve.end",
        "cache.miss",
        "plan.ready",
        "extract.start",
        "extract.end",
        "publish.commit",
        "run.end",
    ] {
        assert!(
            names.iter().any(|name| name == wanted),
            "the stream named no {wanted}: {names:?}"
        );
    }

    let extracted = run.only("extract.end");
    assert_eq!(extracted["entries"], 1);
    assert_eq!(extracted["bytes"], GREETING.len());
}

#[test]
fn a_warm_fetch_says_the_cache_answered() {
    let workspace = Workspace::new();
    let archive = workspace.write("sample.tar.gz", &gzip(&greeting_tar()));
    workspace.run(&["get", archive.to_str().unwrap(), "--output", "first"]);
    let run = workspace.run(&["get", archive.to_str().unwrap(), "--output", "second"]);
    assert_eq!(run.output.status.code(), Some(0));
    assert!(
        run.names().iter().any(|name| name == "cache.hit"),
        "{:?}",
        run.names()
    );
}

#[test]
fn every_end_event_carries_the_time_its_operation_took() {
    let workspace = Workspace::new();
    let source = workspace.path().join("many");
    std::fs::create_dir_all(&source).unwrap();
    for index in 0..512 {
        std::fs::write(
            source.join(format!("file-{index}.bin")),
            format!("distinct content for file {index}").as_bytes(),
        )
        .unwrap();
    }
    let run = workspace.run(&["get", source.to_str().unwrap(), "--output", "out"]);
    assert_eq!(run.output.status.code(), Some(0));

    for event in run.events() {
        let name = event["event"].as_str().unwrap_or_default();
        if name.rsplit('.').next() == Some("end") {
            assert!(
                event["duration_ms"].is_u64(),
                "{name} carried no duration: {event}"
            );
        }
    }
    let ended = run.only("run.end");
    assert!(
        ended["duration_ms"].as_u64().unwrap_or_default() > 0,
        "a run that materialized 512 files reported taking no time: {ended}"
    );
}

#[test]
fn a_failure_reaches_the_stream_once() {
    let workspace = Workspace::new();
    let run = workspace.run(&["get", "./not-here", "--output", "out"]);
    assert_eq!(run.output.status.code(), Some(10));
    let failure = run.only("error");
    assert_eq!(failure["error"]["kind"], "reference.unresolved");
}

#[test]
fn an_archive_rejection_names_the_member_it_rejected() {
    let workspace = Workspace::new();
    let archive = workspace.write("escaping.tar", &escaping_tar());
    let run = workspace.run(&["get", archive.to_str().unwrap(), "--output", "out"]);
    assert_eq!(run.output.status.code(), Some(70));
    let rejected = run.only("extract.reject");
    assert_eq!(rejected["path"], "../escape.txt");
    assert_eq!(rejected["error"]["kind"], "archive.unsafe_path");
}
