//! What the two in-flight ceilings bound, and what they may never change.

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
use serde_json as _;
use toml as _;
#[cfg(windows)]
use windows_sys as _;

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use fetchloom_faults::{Latency, Script, TestServer};
use tempfile::TempDir;

/// The wait each request is charged, which is what makes a loopback socket
/// behave like a remote one.
const CHARGED: Duration = Duration::from_millis(150);

/// How many objects one manifest names.
const OBJECTS: usize = 8;

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

    fn timed(&self, arguments: &[&str]) -> (Duration, i32, String) {
        let started = Instant::now();
        let output = Command::new(env!("CARGO_BIN_EXE_fetchloom"))
            .current_dir(self.path())
            .args(arguments)
            .env("FETCHLOOM_CACHE_DIR", self.path().join("cache"))
            .env_remove("FETCHLOOM_CONFIG")
            .env_remove("FETCHLOOM_OFFLINE")
            .env_remove("FETCHLOOM_CONCURRENCY")
            .env_remove("FETCHLOOM_PER_HOST")
            .env_remove("FETCHLOOM_BANDWIDTH")
            .env_remove("FETCHLOOM_THREADS")
            .stdin(Stdio::null())
            .output()
            .unwrap();
        (
            started.elapsed(),
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    }
}

fn object(index: usize) -> Vec<u8> {
    (0..4096_usize)
        .map(|offset| u8::try_from((offset + index * 37) % 251).unwrap_or(0))
        .collect()
}

/// Writes a manifest naming one distinct object per server, every server on the
/// one loopback host and every request charged a wait, and returns the servers
/// so they outlive the run.
fn delayed_host(workspace: &Workspace, objects: usize) -> Vec<TestServer> {
    let mut servers = Vec::with_capacity(objects);
    let mut artifacts = String::new();
    for index in 0..objects {
        let server = TestServer::start(
            Script::serving(object(index)).delayed(Latency::default().every_request(CHARGED)),
        )
        .unwrap();
        writeln!(
            artifacts,
            "  - id: object-{index}\n    sources: [\"{}/object-{index}\"]",
            server.origin()
        )
        .unwrap();
        servers.push(server);
    }
    workspace.write(
        "dataset.yaml",
        format!("name: delayed\nartifacts:\n{artifacts}").as_bytes(),
    );
    servers
}

/// Runs the same eight-object manifest under two settings and returns how long
/// each took.
fn under(first: &[&str], second: &[&str]) -> (Duration, Duration) {
    let mut taken = Vec::new();
    for settings in [first, second] {
        let workspace = Workspace::new();
        let _servers = delayed_host(&workspace, OBJECTS);
        let mut arguments = vec!["get", "dataset.yaml", "--output", "out"];
        arguments.extend_from_slice(settings);
        let (elapsed, code, said) = workspace.timed(&arguments);
        assert_eq!(code, 0, "the run under {settings:?} failed: {said}");
        assert_eq!(
            std::fs::read_dir(workspace.path().join("out"))
                .unwrap()
                .count(),
            OBJECTS,
            "the run under {settings:?} did not materialize every object, so this compares nothing"
        );
        taken.push(elapsed);
    }
    (taken[0], taken[1])
}

#[test]
fn eight_artifacts_behind_a_charged_wait_finish_in_far_less_than_eight_one_at_a_time() {
    let (sequential, together) = under(
        &["--concurrency", "1", "--per-host", "1"],
        &["--concurrency", "8", "--per-host", "8"],
    );
    assert!(
        together < sequential.mul_f64(0.5),
        "eight artifacts took {together:?} together against {sequential:?} one at a time, which is \
         not materially less, so nothing ran concurrently"
    );
}

#[test]
fn a_per_host_ceiling_of_one_takes_measurably_longer_than_a_ceiling_of_four() {
    let (one, four) = under(
        &["--concurrency", "8", "--per-host", "1"],
        &["--concurrency", "8", "--per-host", "4"],
    );
    assert!(
        four < one.mul_f64(0.7),
        "four transfers per host took {four:?} against {one:?} for one, which is no measurable \
         difference, so the per-host ceiling bounds nothing"
    );
}

#[test]
fn two_artifacts_naming_the_same_digest_transfer_it_once() {
    let workspace = Workspace::new();
    let bytes = object(0);
    let digest = hash_bytes(&bytes).to_string();
    let server = TestServer::start(Script::serving(bytes)).unwrap();
    let origin = server.origin();
    workspace.write(
        "dataset.yaml",
        format!(
            "name: shared\nartifacts:\n  - id: first\n    sources: [\"{origin}/first\"]\n    \
             digest:\n      blake3: \"{digest}\"\n  - id: second\n    sources: \
             [\"{origin}/second\"]\n    digest:\n      blake3: \"{digest}\"\n"
        )
        .as_bytes(),
    );
    let (_, code, said) = workspace.timed(&[
        "get",
        "dataset.yaml",
        "--output",
        "out",
        "--concurrency",
        "8",
        "--per-host",
        "8",
    ]);
    assert_eq!(code, 0, "the shared-digest run failed: {said}");

    let bodies = server
        .received()
        .into_iter()
        .filter(|request| request.method == "GET")
        .count();
    assert_eq!(
        bodies, 1,
        "the same digest was transferred {bodies} times, so a second wanter did not wait and \
         reuse the first transfer"
    );
}
