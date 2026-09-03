//! What a run probes, which source it takes, and what it records about that.

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
use fetchloom_engine::hashing::hash_bytes;
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

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Output;

use fetchloom_faults::{Script, TestServer};
mod support;

use tempfile::TempDir;

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

    fn events(&self) -> PathBuf {
        self.path().join("events.ndjson")
    }

    fn write(&self, name: &str, bytes: &[u8]) {
        std::fs::write(self.path().join(name), bytes).unwrap();
    }

    fn run(&self, arguments: &[&str]) -> Output {
        support::fetchloom()
            .current_dir(self.path())
            .args(arguments)
            .arg("--events")
            .arg(self.events())
            .env("FETCHLOOM_CACHE_DIR", self.path().join("cache"))
            .output()
            .unwrap()
    }

    fn stream(&self) -> Vec<serde_json::Value> {
        std::fs::read_to_string(self.events())
            .unwrap_or_default()
            .lines()
            .filter(|line| !line.trim().is_empty())
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }

    fn of_kind(&self, kind: &str) -> Vec<serde_json::Value> {
        self.stream()
            .into_iter()
            .filter(|event| event["event"] == kind)
            .collect()
    }
}

fn object(index: usize) -> Vec<u8> {
    (0..4096_usize)
        .map(|offset| u8::try_from((offset + index * 29) % 251).unwrap_or(0))
        .collect()
}

/// Writes a manifest naming one artifact that every server given serves.
fn mirrored(workspace: &Workspace, servers: &[&TestServer], bytes: &[u8]) {
    let mut sources = String::new();
    for server in servers {
        if !sources.is_empty() {
            sources.push_str(", ");
        }
        write!(sources, "\"{}/object\"", server.origin()).unwrap();
    }
    workspace.write(
        "dataset.yaml",
        format!(
            "name: mirrored\nartifacts:\n  - id: object\n    sources: [{sources}]\n    digest:\n      blake3: \"{}\"\n",
            hash_bytes(bytes)
        )
        .as_bytes(),
    );
}

/// Returns the authority of an origin, which is what an event carries after
/// redaction.
fn authority(origin: &str) -> String {
    origin.trim_start_matches("http://").to_owned()
}

fn succeeded(run: &Output) {
    assert_eq!(
        run.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn a_run_with_one_source_spends_no_probe_on_it_and_still_says_what_it_took() {
    let workspace = Workspace::new();
    let bytes = object(0);
    let only = TestServer::start(Script::serving(bytes.clone())).unwrap();
    mirrored(&workspace, &[&only], &bytes);

    succeeded(&workspace.run(&["get", "dataset.yaml", "--output", "out"]));

    assert!(
        workspace.of_kind("source.probe").is_empty(),
        "a probe was spent choosing between one candidate"
    );
    let selected = workspace.of_kind("source.selected");
    assert_eq!(
        selected.len(),
        1,
        "the run did not say which source it took"
    );
    assert_eq!(selected[0]["reason"], "the manifest named one source");
}

#[test]
fn a_source_that_serves_ranges_is_taken_over_one_that_does_not() {
    let workspace = Workspace::new();
    let bytes = object(1);
    let plain = TestServer::start(Script::serving(bytes.clone()).ranges(false)).unwrap();
    let ranged = TestServer::start(Script::serving(bytes.clone())).unwrap();
    mirrored(&workspace, &[&plain, &ranged], &bytes);

    succeeded(&workspace.run(&["get", "dataset.yaml", "--output", "out"]));

    assert_eq!(
        workspace.of_kind("source.probe").len(),
        2,
        "both candidates were not probed"
    );
    let selected = workspace.of_kind("source.selected");
    assert_eq!(selected.len(), 1);
    assert_eq!(
        selected[0]["reason"],
        "it serves part of an object and the alternatives do not"
    );
    assert!(
        selected[0]["source"]
            .as_str()
            .unwrap()
            .contains(&authority(&ranged.origin())),
        "the source that serves no range was taken: {}",
        selected[0]["source"]
    );
    assert!(
        plain
            .received()
            .iter()
            .all(|request| request.method == "HEAD"),
        "the source that lost the scoring was asked for bytes anyway"
    );
}

#[test]
fn no_more_candidates_are_probed_than_the_probe_limit_allows() {
    let workspace = Workspace::new();
    let bytes = object(2);
    let servers: Vec<TestServer> = (0..6)
        .map(|_| TestServer::start(Script::serving(bytes.clone())).unwrap())
        .collect();
    let held: Vec<&TestServer> = servers.iter().collect();
    mirrored(&workspace, &held, &bytes);

    succeeded(&workspace.run(&["get", "dataset.yaml", "--output", "out"]));

    assert_eq!(
        workspace.of_kind("source.probe").len(),
        4,
        "the probe limit of four did not bound how many candidates were probed"
    );
}

#[test]
fn candidates_nothing_separates_come_out_in_manifest_order() {
    let workspace = Workspace::new();
    let bytes = object(3);
    let first = TestServer::start(Script::serving(bytes.clone())).unwrap();
    let second = TestServer::start(Script::serving(bytes.clone())).unwrap();
    mirrored(&workspace, &[&first, &second], &bytes);

    succeeded(&workspace.run(&["get", "dataset.yaml", "--output", "out"]));

    let selected = workspace.of_kind("source.selected");
    assert_eq!(
        selected[0]["reason"],
        "nothing measured separated the candidates, so the manifest did"
    );
    assert!(
        selected[0]["source"]
            .as_str()
            .unwrap()
            .contains(&authority(&first.origin())),
        "a tie was not broken by manifest order: {}",
        selected[0]["source"]
    );
}

#[test]
fn the_receipt_records_the_source_that_was_taken_and_why() {
    let workspace = Workspace::new();
    let bytes = object(4);
    let plain = TestServer::start(Script::serving(bytes.clone()).ranges(false)).unwrap();
    let ranged = TestServer::start(Script::serving(bytes.clone())).unwrap();
    mirrored(&workspace, &[&plain, &ranged], &bytes);

    succeeded(&workspace.run(&["get", "dataset.yaml", "--output", "out"]));

    let receipts = workspace.path().join("cache").join("receipts");
    let text: String = std::fs::read_dir(&receipts)
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .map(|path| std::fs::read_to_string(path).unwrap_or_default())
        .collect();
    assert!(
        text.contains("source_reason"),
        "the receipt did not record why the source was taken: {text}"
    );
    assert!(
        text.contains("it serves part of an object and the alternatives do not"),
        "the receipt recorded a reason other than the one the run gave: {text}"
    );
    assert!(
        text.contains(&authority(&ranged.origin())),
        "the receipt named a source other than the one the run took: {text}"
    );
}
