//! What a manifest determines, and what `init` infers.

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
use fetchloom_view as _;
use flate2 as _;
#[cfg(unix)]
use rustix as _;
use serde as _;
use toml as _;
#[cfg(windows)]
use windows_sys as _;

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use fetchloom_faults::{Script, TestServer};
use tempfile::TempDir;

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_fetchloom")
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
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&target, bytes).unwrap();
        target
    }

    fn run(&self, arguments: &[&str]) -> Output {
        Command::new(binary())
            .current_dir(self.path())
            .args(arguments)
            .env("FETCHLOOM_CACHE_DIR", self.path().join("cache"))
            .env_remove("FETCHLOOM_CONFIG")
            .env_remove("FETCHLOOM_OFFLINE")
            .env_remove("FETCHLOOM_LOG")
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }
}

fn tree_of(output: &Output) -> String {
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    let value: serde_json::Value = serde_json::from_str(&text).unwrap_or_else(|reason| {
        panic!("the result was not JSON: {reason}: {text}");
    });
    value
        .get("tree")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_else(|| panic!("the result carried no tree: {text}"))
        .to_owned()
}

#[test]
fn two_artifacts_whose_sources_share_a_basename_do_not_collide() {
    let workspace = Workspace::new();
    let server = TestServer::start(Script::serving(b"shared basename".to_vec())).unwrap();
    let origin = server.origin();
    workspace.write(
        "dataset.yaml",
        format!(
            "name: nested\nartifacts:\n  - id: one/data.bin\n    sources: \
             [\"{origin}/one/data.bin\"]\n  - id: two/data.bin\n    sources: \
             [\"{origin}/two/data.bin\"]\n"
        )
        .as_bytes(),
    );
    let output = workspace.run(&["get", "dataset.yaml", "--output", "out", "--json"]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "two artifacts whose sources share a basename failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let tree = tree_of(&output);
    assert!(!tree.is_empty());
    for path in ["one/data.bin", "two/data.bin"] {
        assert!(
            workspace.path().join("out").join(path).is_file(),
            "{path} is missing, so the two artifacts collided under one basename"
        );
    }
}

#[test]
fn a_manifest_places_a_plain_artifact_under_its_own_identifier() {
    let workspace = Workspace::new();
    let server = TestServer::start(Script::serving(b"bytes".to_vec())).unwrap();
    let origin = server.origin();
    workspace.write(
        "dataset.yaml",
        format!(
            "name: named\nartifacts:\n  - id: corpus\n    sources: [\"{origin}/whatever.bin\"]\n"
        )
        .as_bytes(),
    );
    let output = workspace.run(&["get", "dataset.yaml", "--output", "out", "--json"]);
    assert_eq!(output.status.code(), Some(0));
    assert!(
        workspace.path().join("out").join("corpus").is_file(),
        "the artifact was not placed under its identifier: {:?}",
        std::fs::read_dir(workspace.path().join("out"))
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name())
            .collect::<Vec<_>>()
    );
}

#[test]
fn an_identifier_that_escapes_the_destination_is_refused() {
    let workspace = Workspace::new();
    let server = TestServer::start(Script::serving(b"bytes".to_vec())).unwrap();
    let origin = server.origin();
    workspace.write(
        "dataset.yaml",
        format!(
            "name: escaping\nartifacts:\n  - id: \"../outside\"\n    sources: [\"{origin}/x\"]\n"
        )
        .as_bytes(),
    );
    let output = workspace.run(&["get", "dataset.yaml", "--output", "out", "--json"]);
    assert_eq!(
        output.status.code(),
        Some(70),
        "an identifier holding a traversal was accepted: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn init_over_a_directory_reproduces_that_directory_exactly() {
    let workspace = Workspace::new();
    workspace.write("source/one.txt", b"first entry");
    workspace.write("source/nested/two.txt", b"second entry");
    workspace.write("source/nested/deeper/three.txt", b"third entry");

    let written = workspace.run(&["init", "source", "--output", "source/dataset.yaml"]);
    assert_eq!(
        written.status.code(),
        Some(0),
        "init failed: {}",
        String::from_utf8_lossy(&written.stderr)
    );

    let got = workspace.run(&[
        "get",
        "source/dataset.yaml",
        "--output",
        "rebuilt",
        "--json",
    ]);
    assert_eq!(
        got.status.code(),
        Some(0),
        "the inferred manifest did not fetch: {}",
        String::from_utf8_lossy(&got.stderr)
    );

    for (path, expected) in [
        ("one.txt", "first entry"),
        ("nested/two.txt", "second entry"),
        ("nested/deeper/three.txt", "third entry"),
    ] {
        let found = workspace.path().join("rebuilt").join(path);
        assert!(found.is_file(), "{path} is missing from the rebuilt tree");
        assert_eq!(
            std::fs::read_to_string(&found).unwrap(),
            expected,
            "{path} came back with different bytes"
        );
    }
}

#[test]
fn init_writes_the_manifest_to_standard_output_by_default() {
    let workspace = Workspace::new();
    workspace.write("source/one.txt", b"first entry");
    let output = workspace.run(&["init", "source"]);
    assert_eq!(output.status.code(), Some(0));
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        text.contains("name") && text.contains("artifacts") && text.contains("one.txt"),
        "init wrote no manifest to standard output: {text}"
    );
    assert!(
        !workspace
            .path()
            .join("source")
            .join("dataset.yaml")
            .exists(),
        "init wrote a file when it was asked for nothing but standard output"
    );
}

#[test]
fn init_refuses_to_overwrite_a_manifest_without_force() {
    let workspace = Workspace::new();
    workspace.write("source/one.txt", b"first entry");
    workspace.write("held.yaml", b"name: edited by hand\n");

    let refused = workspace.run(&["init", "source", "--output", "held.yaml"]);
    assert_eq!(
        refused.status.code(),
        Some(60),
        "init overwrote a manifest that was already there"
    );
    assert_eq!(
        std::fs::read_to_string(workspace.path().join("held.yaml")).unwrap(),
        "name: edited by hand\n"
    );

    let forced = workspace.run(&["init", "source", "--output", "held.yaml", "--force"]);
    assert_eq!(forced.status.code(), Some(0));
    assert!(
        std::fs::read_to_string(workspace.path().join("held.yaml"))
            .unwrap()
            .contains("one.txt")
    );
}

#[test]
fn init_over_a_directory_records_the_digests_it_read() {
    let workspace = Workspace::new();
    workspace.write("source/one.txt", b"first entry");
    let output = workspace.run(&["init", "source"]);
    assert_eq!(output.status.code(), Some(0));
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        text.contains("blake3") && text.contains("sha256"),
        "init recorded no digests it had read: {text}"
    );
}

#[test]
fn init_emits_the_same_manifest_twice_over_an_unchanged_directory() {
    let workspace = Workspace::new();
    workspace.write("source/b.txt", b"second");
    workspace.write("source/a.txt", b"first");
    let one = workspace.run(&["init", "source"]);
    let two = workspace.run(&["init", "source"]);
    assert_eq!(one.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&one.stdout),
        String::from_utf8_lossy(&two.stdout),
        "two runs over one unchanged directory emitted different manifests"
    );
}

#[test]
fn init_over_a_listing_records_only_digests_it_read() {
    let workspace = Workspace::new();
    let server = TestServer::start(Script::serving(b"served bytes".to_vec()).replying(vec![
        fetchloom_faults::Reply::Listing {
            format: fetchloom_faults::IndexFormat::GeneratedHtml,
        },
    ]))
    .unwrap();
    let origin = server.origin();
    let output = workspace.run(&["init", &format!("{origin}/set/")]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "init over a listing failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        text.contains("blake3") && text.contains("sha256"),
        "init over a listing recorded no digest for bytes it fetched: {text}"
    );

    let bodies = server
        .received()
        .into_iter()
        .filter(|request| request.method == "GET" && !request.target.ends_with('/'))
        .count();
    assert!(
        bodies >= 1,
        "init recorded digests without reading any object's bytes"
    );
}

#[test]
fn init_over_one_object_says_to_name_a_container() {
    let workspace = Workspace::new();
    let server = TestServer::start(Script::serving(b"one object".to_vec())).unwrap();
    let output = workspace.run(&["init", &format!("{}/object.bin", server.origin())]);
    assert_eq!(
        output.status.code(),
        Some(10),
        "init over one object did not say to name a container"
    );
}

#[test]
fn a_metadata_document_resolves_to_the_files_it_describes() {
    let workspace = Workspace::new();
    let bytes = b"the described bytes".to_vec();
    let server = TestServer::start(Script::serving(bytes.clone())).unwrap();
    let origin = server.origin();

    let document = format!(
        r#"{{"@context":{{"cr":"croissant"}},"@type":"Dataset","name":"described",
        "distribution":[{{"@type":"sc:FileObject","@id":"one.bin",
        "contentUrl":"{origin}/one.bin"}}]}}"#
    );
    let path = workspace.write("metadata.json", document.as_bytes());
    let reference = format!("croissant:{}", located(&path));

    let output = workspace.run(&["get", &reference, "--output", "out", "--json"]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "a metadata document did not resolve: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        workspace.path().join("out").join("one.bin").is_file(),
        "the file the document described was not materialized"
    );
    assert_eq!(
        std::fs::read(workspace.path().join("out").join("one.bin")).unwrap(),
        bytes
    );
}

#[test]
fn a_bare_name_resolves_through_the_configured_sources() {
    let workspace = Workspace::new();
    let bytes = b"the named dataset".to_vec();
    let server = TestServer::start(Script::serving(bytes.clone())).unwrap();
    workspace.write(
        "fetchloom.toml",
        format!("sources = [\"{}/data/\"]\n", server.origin()).as_bytes(),
    );

    let output = workspace.run(&["get", "silesia", "--output", "out", "--json"]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "a bare name did not resolve through the configured sources: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(workspace.path().join("out").join("silesia").is_file());
}

#[test]
fn a_namespaced_release_resolves_the_same_way() {
    let workspace = Workspace::new();
    let server = TestServer::start(Script::serving(b"a release".to_vec())).unwrap();
    workspace.write(
        "fetchloom.toml",
        format!("sources = [\"{}/data/\"]\n", server.origin()).as_bytes(),
    );

    let output = workspace.run(&["get", "acme/imagenet@2012", "--output", "out", "--json"]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "a namespaced release did not resolve: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn a_name_with_no_sources_configured_says_to_configure_one() {
    let workspace = Workspace::new();
    let output = workspace.run(&["get", "silesia", "--json"]);
    assert_eq!(output.status.code(), Some(10));
    let said = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        said.contains("sources") && said.contains("fetchloom.toml"),
        "the failure did not say how to configure a source: {said}"
    );
}

#[test]
fn a_name_that_matches_no_configured_source_is_never_guessed_at() {
    let workspace = Workspace::new();
    workspace.write(
        "fetchloom.toml",
        b"sources = [\"http://127.0.0.1:9/data/\"]\n",
    );

    let output = workspace.run(&["get", "nothing-is-published-here", "--json"]);
    assert_eq!(
        output.status.code(),
        Some(10),
        "a name that matched no source failed as something other than an unresolved reference: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let said = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        said.contains("never guessed") && said.contains('1'),
        "the failure did not say the name was not guessed at, or how many were tried: {said}"
    );
}

/// Returns a path as a `file:` location, which is how a test names one to a
/// scheme that reads a document.
fn located(path: &Path) -> String {
    let text: String = path
        .display()
        .to_string()
        .chars()
        .map(|letter| if letter == '\\' { '/' } else { letter })
        .collect();
    format!("file:///{text}")
}
