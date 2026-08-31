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
use fetchloom_engine::tuning::FIRST_PER_HOST;
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

use fetchloom_faults::{Reply, Script, TYPEFLAG_REGULAR, TarHeader, TarWriter, TestServer};
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

/// A corpus of several files, so a run has more than one object to move.
fn corpus(workspace: &Workspace, files: usize) -> PathBuf {
    for index in 0..files {
        let body: Vec<u8> = (0..1024_usize)
            .map(|offset| u8::try_from((offset + index * 7) % 251).unwrap_or(0))
            .collect();
        workspace.write(&format!("corpus/file-{index}.bin"), &body);
    }
    workspace.path().join("corpus")
}

/// Runs the same fetch under a setting and returns the tree digest and the
/// four deterministic counters.
fn shape(run: &Run) -> (String, serde_json::Value) {
    let body = run.json();
    (
        body["tree"].as_str().unwrap_or_default().to_owned(),
        body["work"].clone(),
    )
}

#[test]
fn a_warm_measurement_cache_and_an_empty_one_produce_the_same_bytes() {
    let workspace = Workspace::new();
    let source = corpus(&workspace, 8);
    let source = source.to_str().unwrap();

    let mut trees = Vec::new();
    for (index, ceilings) in [
        vec![],
        vec!["--concurrency", "1", "--per-host", "1"],
        vec!["--concurrency", "4", "--per-host", "2"],
        vec!["--per-host", "1"],
    ]
    .into_iter()
    .enumerate()
    {
        let out = format!("out-{index}");
        let mut arguments = vec!["get", source, "--output", out.as_str(), "--json"];
        arguments.extend_from_slice(&ceilings);
        let run = workspace.run(&arguments);
        assert_eq!(run.code(), 0, "{ceilings:?} said {}", run.err());
        trees.push(run.json()["tree"].as_str().unwrap().to_owned());
    }
    assert!(
        trees.windows(2).all(|pair| pair[0] == pair[1]),
        "a ceiling changed the tree digest: {trees:?}"
    );

    let entries: Vec<Vec<String>> = (0..4)
        .map(|index| entries_under(&workspace.path().join(format!("out-{index}"))))
        .collect();
    assert!(
        entries.windows(2).all(|pair| pair[0] == pair[1]),
        "a ceiling changed what was materialized: {entries:?}"
    );
}

#[test]
fn a_measurement_a_run_recorded_never_changes_what_the_next_run_produces() {
    let object: Vec<u8> = (0..64 * 1024_usize)
        .map(|index| u8::try_from(index % 251).unwrap_or(0))
        .collect();
    let server =
        TestServer::start(Script::serving(object).tagged(vec!["\"one\"".to_owned()])).unwrap();
    let location = format!("{}/object", server.origin());

    let observing = Workspace::new();
    let observed = observing.run(&["get", &location, "--output", "first", "--json"]);
    assert_eq!(observed.code(), 0, "{}", observed.err());
    let learned = recorded_concurrency(&observing);
    assert!(
        learned > u64::from(FIRST_PER_HOST),
        "the run recorded {learned}, which is where a host nothing is known about starts, so \
         there is no warm state for this test to vary"
    );

    let empty = Workspace::new();
    let warm = Workspace::new();
    for workspace in [&empty, &warm] {
        assert_eq!(
            workspace.run(&["cache", "status"]).code(),
            0,
            "the cache would not open"
        );
    }
    seed_measurements(&observing, &warm);
    assert!(
        measurement_files(&empty.cache()).is_empty(),
        "the empty side already holds a measurement"
    );
    assert_eq!(
        measurement_files(&warm.cache()).len(),
        1,
        "the warm side holds no measurement, so this test varies nothing"
    );

    let mut shapes = Vec::new();
    for (workspace, name) in [(&empty, "empty"), (&warm, "warm")] {
        let run = workspace.run(&[
            "get",
            &location,
            "--output",
            "out",
            "--json",
            "--deterministic-io",
        ]);
        assert_eq!(run.code(), 0, "the {name} side said {}", run.err());
        shapes.push(shape(&run));
    }
    assert_eq!(
        shapes[0].0, shapes[1].0,
        "the measurement state changed the tree digest"
    );
    assert_eq!(
        shapes[0].1, shapes[1].1,
        "deterministic mode did not produce identical counters across measurement states"
    );
    assert_eq!(
        recorded_concurrency(&warm),
        learned,
        "the deterministic run rewrote the measurement it was told not to adapt from"
    );
}

/// Returns the measurement records a cache holds.
fn measurement_files(cache: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(cache.join("meta").join("host"))
        .map(|entries| entries.flatten().map(|entry| entry.path()).collect())
        .unwrap_or_default()
}

/// Copies the measurements one workspace recorded into another, so the two
/// differ in the measurement state and in nothing else.
fn seed_measurements(from: &Workspace, into: &Workspace) {
    let target = into.cache().join("meta").join("host");
    std::fs::create_dir_all(&target).unwrap();
    for path in measurement_files(&from.cache()) {
        std::fs::copy(&path, target.join(path.file_name().unwrap())).unwrap();
    }
}

#[test]
fn a_rate_limit_reduces_the_recorded_concurrency_in_the_run_it_happened_in() {
    let object: Vec<u8> = (0..32 * 1024_usize)
        .map(|index| u8::try_from(index % 251).unwrap_or(0))
        .collect();
    let workspace = Workspace::new();

    let clean = TestServer::start(Script::serving(object.clone())).unwrap();
    let first = workspace.run(&[
        "get",
        &format!("{}/object", clean.origin()),
        "--output",
        "clean",
        "--json",
    ]);
    assert_eq!(first.code(), 0, "{}", first.err());
    let unlimited = recorded_concurrency(&workspace);
    drop(clean);

    let limited = TestServer::start(Script::serving(object).replying(vec![
        Reply::Status {
            code: 429,
            retry_after: Some("0".to_owned()),
        };
        2
    ]))
    .unwrap();
    let second = workspace.run(&[
        "get",
        &format!("{}/object", limited.origin()),
        "--output",
        "limited",
        "--json",
    ]);
    assert_eq!(second.code(), 0, "{}", second.err());
    let after = recorded_concurrency(&workspace);

    assert!(
        after < unlimited,
        "a rate limit left the concurrency at {after} where a clean run recorded {unlimited}"
    );
}

/// Returns the concurrency the cache recorded for the one host a test fetched
/// from.
fn recorded_concurrency(workspace: &Workspace) -> u64 {
    let directory = workspace.cache().join("meta").join("host");
    let mut found = Vec::new();
    for entry in std::fs::read_dir(&directory)
        .unwrap_or_else(|error| panic!("{}: {error}", directory.display()))
        .flatten()
    {
        let text = std::fs::read_to_string(entry.path()).unwrap();
        let record: serde_json::Value = serde_json::from_str(&text).unwrap();
        found.push(record["measurement"]["concurrency"].as_u64().unwrap());
    }
    assert_eq!(found.len(), 1, "expected one host, found {found:?}");
    found[0]
}

#[test]
fn a_plan_lists_every_field_no_source_stated_and_reports_none_of_them_as_zero() {
    let workspace = Workspace::new();
    let archive = workspace.write("sample.tar.gz", &gzip(&greeting_tar()));
    let fetched = workspace.run(&[
        "get",
        archive.to_str().unwrap(),
        "--output",
        "out",
        "--json",
    ]);
    assert_eq!(fetched.code(), 0, "{}", fetched.err());

    let planned = workspace.run(&[
        "plan",
        archive.to_str().unwrap(),
        "--output",
        "out",
        "--json",
    ]);
    assert_eq!(planned.code(), 0, "{}", planned.err());
    let plan = planned.json();

    let unknown: Vec<&str> = plan["unknown"]
        .as_array()
        .unwrap()
        .iter()
        .map(|name| name.as_str().unwrap())
        .collect();
    assert!(
        unknown.contains(&"expanded"),
        "expanded is not listed as unknown: {unknown:?}"
    );
    for beside in ["staging", "destination"] {
        assert!(
            unknown.contains(&beside),
            "{beside} depends on the expanded size, which is unknown, so it must be listed \
             beside it rather than reported as a number: {unknown:?}"
        );
        assert!(
            plan["disk"][beside]["bytes"].is_null(),
            "{beside} was estimated into {}",
            plan["disk"][beside]["bytes"]
        );
    }
    for known in ["partial", "cache"] {
        assert!(
            !unknown.contains(&known),
            "{known} comes from the size the lock pins and is not unknown: {unknown:?}"
        );
        assert!(
            plan["disk"][known]["bytes"].is_number(),
            "{known} was not stated"
        );
    }
}
