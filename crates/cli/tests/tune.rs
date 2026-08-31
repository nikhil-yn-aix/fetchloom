//! What measurement is allowed to change, and what it may never change.

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
use flate2::Compression;
use flate2::write::GzEncoder;
use tempfile::TempDir;

struct Run {
    output: Output,
}

impl Run {
    fn code(&self) -> i32 {
        self.output.status.code().unwrap_or(-1)
    }

    fn out(&self) -> String {
        String::from_utf8_lossy(&self.output.stdout).into_owned()
    }

    fn err(&self) -> String {
        String::from_utf8_lossy(&self.output.stderr).into_owned()
    }

    fn json(&self) -> serde_json::Value {
        let text = self.out();
        serde_json::from_str(text.trim()).unwrap_or_else(|error| {
            panic!(
                "the result was not JSON ({error}): {text}\nstderr: {}",
                self.err()
            )
        })
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

    fn cache(&self) -> PathBuf {
        self.path().join("cache")
    }

    fn write(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let target = self.path().join(name);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&target, bytes).unwrap();
        target
    }

    fn run(&self, arguments: &[&str]) -> Run {
        Run {
            output: Command::new(env!("CARGO_BIN_EXE_fetchloom"))
                .current_dir(self.path())
                .args(arguments)
                .env("FETCHLOOM_CACHE_DIR", self.cache())
                .env_remove("FETCHLOOM_CONFIG")
                .env_remove("FETCHLOOM_OFFLINE")
                .env_remove("FETCHLOOM_CONCURRENCY")
                .env_remove("FETCHLOOM_PER_HOST")
                .env_remove("FETCHLOOM_BANDWIDTH")
                .env_remove("FETCHLOOM_THREADS")
                .stdin(Stdio::null())
                .output()
                .unwrap(),
        }
    }
}

fn greeting_tar() -> Vec<u8> {
    let mut header = TarHeader::ustar(b"hello.txt", TYPEFLAG_REGULAR);
    header.set_size(6);
    let mut writer = TarWriter::new();
    writer.push(&header, b"hello\n");
    writer.finish()
}

fn gzip(content: &[u8]) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(content).unwrap();
    encoder.finish().unwrap()
}

fn entries_under(root: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path.clone());
            }
            found.push(
                path.strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
        }
    }
    found.sort();
    found
}

fn degrades(stream: &str) -> Vec<String> {
    stream
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|event| event["event"] == "degrade")
        .map(|event| {
            format!(
                "{} -> {} because {}",
                event["requested"], event["used"], event["reason"]
            )
        })
        .collect()
}

#[test]
fn no_cache_extracts_the_same_archive_a_cached_run_extracts() {
    let workspace = Workspace::new();
    let archive = workspace.write("sample.tar.gz", &gzip(&greeting_tar()));
    let cached = workspace.run(&[
        "get",
        archive.to_str().unwrap(),
        "--output",
        "cached",
        "--json",
    ]);
    let uncached = workspace.run(&[
        "get",
        archive.to_str().unwrap(),
        "--output",
        "uncached",
        "--no-cache",
        "--json",
    ]);
    assert_eq!(cached.code(), 0, "{}", cached.err());
    assert_eq!(uncached.code(), 0, "{}", uncached.err());
    assert_eq!(
        cached.json()["tree"],
        uncached.json()["tree"],
        "--no-cache changed the tree digest"
    );
    assert_eq!(
        entries_under(&workspace.path().join("cached")),
        entries_under(&workspace.path().join("uncached"))
    );
    assert_eq!(
        entries_under(&workspace.path().join("uncached")),
        vec!["hello.txt".to_owned()]
    );
}

#[test]
fn no_cache_leaves_nothing_beside_the_destination() {
    let workspace = Workspace::new();
    let archive = workspace.write("sample.tar.gz", &gzip(&greeting_tar()));
    let run = workspace.run(&[
        "get",
        archive.to_str().unwrap(),
        "--output",
        "out",
        "--no-cache",
        "--json",
    ]);
    assert_eq!(run.code(), 0, "{}", run.err());
    assert_eq!(workspace.run(&["cache", "ls"]).out().trim(), "");
    let beside = entries_under(workspace.path());
    let left: Vec<&String> = beside
        .iter()
        .filter(|name| name.contains("fetchloom-scratch"))
        .collect();
    assert!(left.is_empty(), "{beside:?}");
}

#[test]
fn a_thread_ceiling_of_zero_is_a_usage_error() {
    let workspace = Workspace::new();
    let run = workspace.run(&["explain", "threads", "--threads", "0"]);
    assert_eq!(run.code(), 2, "stdout {} stderr {}", run.out(), run.err());
    assert!(
        run.err().contains("--threads"),
        "the message never named the flag: {}",
        run.err()
    );
}

#[test]
fn a_thread_ceiling_above_the_detected_budget_is_clamped_and_says_so() {
    let workspace = Workspace::new();
    let source = workspace.write("tree/one.txt", b"one\n");
    let run = workspace.run(&[
        "get",
        source.parent().unwrap().to_str().unwrap(),
        "--output",
        "out",
        "--threads",
        "99999",
        "--json",
        "--events",
        "stream.ndjson",
    ]);
    assert_eq!(run.code(), 0, "{}", run.err());
    let stream = std::fs::read_to_string(workspace.path().join("stream.ndjson")).unwrap();
    let found = degrades(&stream);
    assert!(
        found.iter().any(|line| line.contains("99999")),
        "no degrade named the clamp: {found:?}"
    );
    let explained = workspace.run(&["explain", "threads", "--threads", "99999"]);
    assert!(
        explained.out().contains("clamped"),
        "explain never said the ceiling was clamped: {}",
        explained.out()
    );
}
