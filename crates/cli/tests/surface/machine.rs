//! The field names of every `--json` result, frozen. Other tools parse this
//! output, so a rename is a break and this is what makes it fail here rather
//! than in someone's pipeline.

#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test setup, where a failure to run the binary is the assertion"
)]

use std::path::{Path, PathBuf};

use tempfile::TempDir;

use crate::support;

struct Scene {
    temporary: TempDir,
    cache: TempDir,
    library: TempDir,
}

impl Scene {
    fn new() -> Self {
        let temporary = TempDir::new().unwrap();
        let source = temporary.path().join("source");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("a.txt"), b"hello").unwrap();
        std::fs::write(
            temporary.path().join("corpus.yaml"),
            "name: \"corpus\"\nartifacts:\n  - id: \"data\"\n    sources:\n      - \"source/a.txt\"\n",
        )
        .unwrap();
        Self {
            temporary,
            cache: TempDir::new().unwrap(),
            library: TempDir::new().unwrap(),
        }
    }

    fn at(&self, name: &str) -> PathBuf {
        self.temporary.path().join(name)
    }

    fn json(&self, arguments: &[&str]) -> serde_json::Value {
        let output = support::fetchloom()
            .current_dir(self.temporary.path())
            .args(arguments)
            .arg("--json")
            .env("FETCHLOOM_CACHE_DIR", self.cache.path())
            .env("FETCHLOOM_LIBRARY_DIR", self.library.path())
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(0),
            "{arguments:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let last = String::from_utf8_lossy(&output.stdout)
            .lines()
            .last()
            .unwrap_or_default()
            .to_owned();
        serde_json::from_str(&last).unwrap_or_else(|reason| {
            panic!("{arguments:?} printed something that is not one JSON object: {reason}: {last}")
        })
    }
}

fn names_of(value: &serde_json::Value) -> Vec<String> {
    let mut names: Vec<String> = value
        .as_object()
        .unwrap_or_else(|| panic!("a result is a JSON object: {value}"))
        .keys()
        .cloned()
        .collect();
    names.sort();
    names
}

fn assert_named(value: &serde_json::Value, expected: &[&str], what: &str) {
    let found = names_of(value);
    let mut wanted: Vec<String> = expected.iter().map(|name| (*name).to_owned()).collect();
    wanted.sort();
    assert_eq!(
        found, wanted,
        "the fields {what} prints changed, which breaks every tool that reads them"
    );
}

fn first_of<'a>(value: &'a serde_json::Value, field: &str) -> &'a serde_json::Value {
    value[field]
        .as_array()
        .unwrap_or_else(|| panic!("{field} is an array: {value}"))
        .first()
        .unwrap_or_else(|| panic!("{field} holds at least one entry: {value}"))
}

#[test]
fn every_json_result_prints_the_fields_the_contract_names() {
    let scene = Scene::new();
    let manifest = scene.at("corpus.yaml");
    let manifest = manifest.to_str().unwrap();
    let destination = scene.at("dest");
    let destination = destination.to_str().unwrap();
    let lock = scene.at("fetchloom.lock");
    let lock = lock.to_str().unwrap();

    let fetched = scene.json(&["get", manifest, "--output", destination, "--lock", lock]);
    assert_named(
        &fetched,
        &[
            "status",
            "dataset",
            "tree",
            "destination",
            "entries",
            "bytes",
            "work",
            "trust",
        ],
        "get",
    );
    assert_named(
        &fetched["work"],
        &["bytes_read", "bytes_written", "requests", "file_operations"],
        "the work a run reports",
    );

    let probed = scene.json(&["probe", manifest]);
    assert_named(&probed, &["dataset", "artifacts"], "probe");
    assert_named(
        first_of(&probed, "artifacts"),
        &[
            "artifact", "location", "size", "digests", "trust", "ranges", "cached",
        ],
        "an artifact probe reports",
    );

    let listed = scene.json(&["list", scene.at("source").to_str().unwrap()]);
    assert_named(&listed, &["entries", "skipped"], "list");
    assert_named(
        first_of(&listed, "entries"),
        &["path", "type", "size"],
        "an entry list reports",
    );

    let named = scene.json(&["where", manifest]);
    assert_named(&named, &["dataset", "path", "held"], "where");

    std::fs::write(Path::new(destination).join("data"), b"mine now").unwrap();

    let status = scene.json(&["status", destination]);
    assert_named(&status, &["entries"], "status");
    assert_named(
        first_of(&status, "entries"),
        &["path", "state"],
        "an entry status reports",
    );

    let differed = scene.json(&["diff", destination]);
    assert_named(&differed, &["entries"], "diff");
    assert_named(
        first_of(&differed, "entries"),
        &["path", "state", "record", "found"],
        "an entry diff reports",
    );

    let promoted = scene.json(&[
        "promote",
        destination,
        "--lock",
        scene.at("promoted.lock").to_str().unwrap(),
    ]);
    assert_named(
        &promoted,
        &["dataset", "artifacts", "lock", "derived_from"],
        "promote",
    );

    let reverted = scene.json(&["revert", destination]);
    assert_named(&reverted, &["restored", "path"], "revert");

    let verified = scene.json(&["verify", destination]);
    assert_named(&verified, &["status", "tree", "entries", "path"], "verify");

    assert_eq!(
        scene
            .json(&["get", manifest, "--library"])
            .get("destination")
            .and_then(serde_json::Value::as_str)
            .map(PathBuf::from),
        named
            .get("path")
            .and_then(serde_json::Value::as_str)
            .map(PathBuf::from),
        "get --library and where disagree about the path they name"
    );

    let holding = scene.json(&["library", "ls"]);
    assert_named(&holding, &["entries", "bytes"], "library ls");
    assert_named(
        first_of(&holding, "entries"),
        &["dataset", "path", "held"],
        "an entry the library holds",
    );

    let removed = scene.json(&[
        "library",
        "rm",
        first_of(&holding, "entries")["path"].as_str().unwrap(),
    ]);
    assert_named(&removed, &["path", "bytes"], "library rm");
}

#[test]
fn a_failure_prints_the_error_object_the_contract_names() {
    let scene = Scene::new();
    let output = support::fetchloom()
        .current_dir(scene.temporary.path())
        .args(["get", "./definitely-missing-path", "--json"])
        .env("FETCHLOOM_CACHE_DIR", scene.cache.path())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(10));
    let printed: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).unwrap();
    assert_named(
        &printed,
        &[
            "kind",
            "layer",
            "dataset",
            "artifact",
            "source",
            "attempts",
            "retryable",
            "next_action",
        ],
        "a failure",
    );
}
