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

use std::sync::Arc;
use std::time::Duration;

use fetchloom_faults::{Flight, Latency, Script, TestServer};
mod support;

use tempfile::TempDir;

const CHARGED: Duration = Duration::from_millis(150);

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

    fn run(&self, arguments: &[&str]) -> (i32, String) {
        let output = support::fetchloom()
            .current_dir(self.path())
            .args(arguments)
            .env("FETCHLOOM_CACHE_DIR", self.path().join("cache"))
            .output()
            .unwrap();
        (
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

fn delayed_host(workspace: &Workspace, objects: usize, flight: &Arc<Flight>) -> Vec<TestServer> {
    let mut servers = Vec::with_capacity(objects);
    let mut artifacts = String::new();
    for index in 0..objects {
        let server = TestServer::start(
            Script::serving(object(index))
                .delayed(Latency::default().every_request(CHARGED))
                .reporting(flight),
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

fn in_flight_under(settings: &[&str]) -> usize {
    let workspace = Workspace::new();
    let flight = Flight::new();
    let _servers = delayed_host(&workspace, OBJECTS, &flight);
    let mut arguments = vec!["get", "dataset.yaml", "--output", "out"];
    arguments.extend_from_slice(settings);
    let (code, said) = workspace.run(&arguments);
    assert_eq!(code, 0, "the run under {settings:?} failed: {said}");
    assert_eq!(
        std::fs::read_dir(workspace.path().join("out"))
            .unwrap()
            .count(),
        OBJECTS,
        "the run under {settings:?} did not materialize every object, so it measures nothing"
    );
    flight.peak()
}

#[test]
fn the_global_ceiling_is_the_most_transfers_that_are_ever_in_flight() {
    let alone = in_flight_under(&["--concurrency", "1", "--per-host", "8"]);
    assert_eq!(
        alone, 1,
        "a global ceiling of one had {alone} transfers in flight at once, so the ceiling bounds \
         nothing"
    );

    let together = in_flight_under(&["--concurrency", "8", "--per-host", "8"]);
    assert!(
        together > 1,
        "a global ceiling of eight never had more than one transfer in flight, so nothing ran \
         concurrently"
    );
    assert!(
        together <= OBJECTS,
        "a global ceiling of eight had {together} transfers in flight at once"
    );
}

#[test]
fn the_per_host_ceiling_is_the_most_transfers_in_flight_to_the_one_host() {
    let alone = in_flight_under(&["--concurrency", "8", "--per-host", "1"]);
    assert_eq!(
        alone, 1,
        "a per-host ceiling of one had {alone} transfers in flight to the one host, so the \
         ceiling bounds nothing"
    );

    let four = in_flight_under(&["--concurrency", "8", "--per-host", "4"]);
    assert!(
        four > 1,
        "a per-host ceiling of four never had more than one transfer in flight to the one host"
    );
    assert!(
        four <= 4,
        "a per-host ceiling of four had {four} transfers in flight to the one host"
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
    let (code, said) = workspace.run(&[
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
