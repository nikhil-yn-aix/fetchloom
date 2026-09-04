//! What `--offline` refuses, in every shape a reference is written in.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test setup, where a failure to run the binary is the assertion"
)]

use std::path::Path;
use std::process::Stdio;

use clap as _;
use clap_complete as _;
use ctrlc as _;
use fetchloom_archive as _;
use fetchloom_cache as _;
use fetchloom_cli as _;
use fetchloom_engine as _;
use fetchloom_faults as _;
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

use fetchloom_engine::work::Work;
mod support;

use tempfile::TempDir;

#[derive(serde::Deserialize)]
struct Reported {
    kind: String,
}

#[derive(serde::Deserialize)]
struct Completed {
    work: Work,
}

const POLICY: i32 = 40;

fn offline_run(reference: &str, destination: &Path, cache: &Path) -> (i32, String) {
    let output = support::fetchloom()
        .arg("get")
        .arg(reference)
        .arg("--output")
        .arg(destination)
        .arg("--offline")
        .arg("--no-config")
        .arg("--json")
        .env("FETCHLOOM_CACHE_DIR", cache)
        .env("HTTPS_PROXY", "http://127.0.0.1:1")
        .env("HTTP_PROXY", "http://127.0.0.1:1")
        .stderr(Stdio::null())
        .output()
        .unwrap();
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
    )
}

const REMOTE_SHAPES: &[&str] = &[
    "https://lab.edu/eeg.yaml",
    "https://host/x.tar.zst",
    "https://s3.amazonaws.com/bucket/prefix/",
    "hf:datasets/org/name",
    "hf:datasets/org/name@rev",
    "zenodo:10.5281/zenodo.1234567",
    "croissant:https://host/metadata.json",
    "silesia",
    "acme/imagenet@2012",
];

#[test]
fn every_remote_reference_shape_is_refused_offline() {
    for reference in REMOTE_SHAPES {
        let temporary = TempDir::new().unwrap();
        let (code, body) = offline_run(
            reference,
            &temporary.path().join("out"),
            &temporary.path().join("cache"),
        );

        assert_eq!(
            code, POLICY,
            "an offline run of {reference} exited {code} rather than {POLICY}, and its result was \
             {body}"
        );
        let reported: Reported = serde_json::from_str(body.lines().last().unwrap_or_default())
            .unwrap_or(Reported { kind: body.clone() });
        assert_eq!(
            reported.kind, "policy.offline",
            "an offline run of {reference} failed {} rather than refusing to go out at all",
            reported.kind
        );
    }
}

#[test]
fn an_offline_run_of_a_remote_reference_issues_no_request() {
    for reference in REMOTE_SHAPES {
        let temporary = TempDir::new().unwrap();
        let events = temporary.path().join("events.ndjson");
        let output = support::fetchloom()
            .arg("get")
            .arg(reference)
            .arg("--output")
            .arg(temporary.path().join("out"))
            .arg("--offline")
            .arg("--no-config")
            .arg("--events")
            .arg(&events)
            .arg("--json")
            .env("FETCHLOOM_CACHE_DIR", temporary.path().join("cache"))
            .stderr(Stdio::null())
            .output()
            .unwrap();
        assert!(!output.status.success());

        let stream = std::fs::read_to_string(&events).unwrap_or_default();
        assert!(
            !stream.contains("\"transfer.start\"") && !stream.contains("\"source.probe\""),
            "an offline run of {reference} reached a source: {stream}"
        );
    }
}

#[test]
fn a_local_reference_is_materialized_offline() {
    let temporary = TempDir::new().unwrap();
    let source = temporary.path().join("source");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(source.join("a.txt"), b"hello").unwrap();

    for reference in [
        source.join("a.txt").display().to_string(),
        source.display().to_string(),
    ] {
        let destination = temporary.path().join(format!("out-{}", reference.len()));
        let output = support::fetchloom()
            .arg("get")
            .arg(&reference)
            .arg("--output")
            .arg(&destination)
            .arg("--offline")
            .arg("--no-config")
            .arg("--json")
            .env("FETCHLOOM_CACHE_DIR", temporary.path().join("cache"))
            .stderr(Stdio::null())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "an offline run of the local reference {reference} failed: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        let completed: Completed = serde_json::from_slice(&output.stdout).expect("a json result");
        assert_eq!(
            completed.work.requests, 0,
            "a local offline run issued a request"
        );
    }
}
