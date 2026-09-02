//! Contract tests that drive the real binary and assert what it does with a
//! credential and with a manifest's recorded terms.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test setup, where a failure to build the input is the assertion"
)]

use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::Arc;

use clap as _;
use clap_complete as _;
use ctrlc as _;
use fetchloom_archive as _;
use fetchloom_cache::Cache;
use fetchloom_cli::cache as cache_cli;
use fetchloom_engine as _;
use fetchloom_engine::pool::Processor;
use fetchloom_engine::threads::ThreadBudget;
use fetchloom_engine::work::WorkCounter;
use fetchloom_platform as _;
use fetchloom_platform::NativePlatform;
use fetchloom_sources as _;
use flate2 as _;
#[cfg(unix)]
use rustix as _;
use serde as _;
use toml as _;
#[cfg(windows)]
use windows_sys as _;

use fetchloom_faults::{Reply, Script, TestServer};
use tempfile::TempDir;

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_fetchloom")
}

/// The directory every command in this file runs in, so a lock file a command
/// writes beside itself never lands inside the repository.
fn scratch() -> &'static Path {
    static SCRATCH: std::sync::OnceLock<TempDir> = std::sync::OnceLock::new();
    SCRATCH.get_or_init(|| TempDir::new().unwrap()).path()
}

fn run_with(cache_dir: &Path, extra: &[(&str, &str)], arguments: &[&str]) -> Output {
    let mut command = Command::new(binary());
    command
        .current_dir(scratch())
        .args(arguments)
        .env("FETCHLOOM_CACHE_DIR", cache_dir)
        .stdin(Stdio::null());
    for (name, value) in extra {
        command.env(name, value);
    }
    command.output().unwrap()
}

fn token_variable_for(host: &str) -> String {
    fetchloom_engine::credential::token_variable(&fetchloom_engine::reference::Host::new(host))
}

fn object(length: usize) -> Vec<u8> {
    (0..length)
        .map(|index| u8::try_from(index % 251).unwrap_or(0))
        .collect()
}

#[test]
fn a_run_sends_the_authorization_header_when_a_credential_is_resolved() {
    let bytes = object(256);
    let server = TestServer::start(Script::serving(bytes)).unwrap();
    let cache = TempDir::new().unwrap();
    let destination = TempDir::new().unwrap();
    let variable = token_variable_for("127.0.0.1");

    let output = run_with(
        cache.path(),
        &[(&variable, "the-token-value")],
        &[
            "get",
            &format!("{}/object", server.origin()),
            "--output",
            destination.path().join("out").to_str().unwrap(),
            "--json",
        ],
    );

    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr was {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let received = server.received();
    assert!(
        received
            .iter()
            .any(|request| request.header("authorization") == Some("the-token-value")),
        "no request carried the credential: {received:?}"
    );
}

#[test]
fn a_run_with_no_credential_sends_no_authorization_header() {
    let bytes = object(256);
    let server = TestServer::start(Script::serving(bytes)).unwrap();
    let cache = TempDir::new().unwrap();
    let destination = TempDir::new().unwrap();

    let output = run_with(
        cache.path(),
        &[],
        &[
            "get",
            &format!("{}/object", server.origin()),
            "--output",
            destination.path().join("out").to_str().unwrap(),
            "--json",
        ],
    );

    assert_eq!(output.status.code(), Some(0));
    let received = server.received();
    assert!(
        received
            .iter()
            .all(|request| request.header("authorization").is_none()),
        "a request carried a credential nobody supplied: {received:?}"
    );
}

#[test]
fn a_credential_the_source_rejects_fails_as_credential_invalid_and_names_the_provider() {
    let server = TestServer::start(Script::serving(Vec::new()).replying(vec![Reply::Status {
        code: 401,
        retry_after: None,
    }]))
    .unwrap();
    let cache = TempDir::new().unwrap();
    let destination = TempDir::new().unwrap();
    let variable = token_variable_for("127.0.0.1");

    let output = run_with(
        cache.path(),
        &[(&variable, "an-invalid-token")],
        &[
            "get",
            &format!("{}/object", server.origin()),
            "--output",
            destination.path().join("out").to_str().unwrap(),
            "--json",
        ],
    );

    assert_eq!(output.status.code(), Some(40));
    let body: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(body["kind"].as_str(), Some("policy.credential_invalid"));
    let action = body["next_action"].as_str().unwrap_or_default();
    assert!(
        action.contains("127.0.0.1"),
        "the failure did not name the provider: {action}"
    );
    assert!(
        action.to_lowercase().contains("renew") || action.to_lowercase().contains("widen"),
        "the failure did not say how to renew or widen the credential: {action}"
    );
}

#[test]
fn a_credential_that_stops_working_between_transfers_fails_as_credential_invalid_rather_than_retrying_forever()
 {
    let server = TestServer::start(Script::serving(object(256)).replying(vec![
        Reply::Whole,
        Reply::Status {
            code: 401,
            retry_after: None,
        },
    ]))
    .unwrap();
    let cache = TempDir::new().unwrap();
    let variable = token_variable_for("127.0.0.1");

    let first_destination = TempDir::new().unwrap();
    let first = run_with(
        cache.path(),
        &[(&variable, "a-working-token")],
        &[
            "get",
            &format!("{}/object", server.origin()),
            "--output",
            first_destination.path().join("out").to_str().unwrap(),
            "--json",
        ],
    );
    assert_eq!(
        first.status.code(),
        Some(0),
        "the first transfer, made while the credential still worked, did not succeed: {}",
        String::from_utf8_lossy(&first.stderr)
    );

    let second_destination = TempDir::new().unwrap();
    let second = run_with(
        cache.path(),
        &[(&variable, "a-working-token")],
        &[
            "get",
            &format!("{}/object", server.origin()),
            "--output",
            second_destination.path().join("out").to_str().unwrap(),
            "--json",
        ],
    );

    assert_eq!(second.status.code(), Some(40));
    let body: serde_json::Value = serde_json::from_slice(&second.stdout).unwrap();
    assert_eq!(body["kind"].as_str(), Some("policy.credential_invalid"));
    assert_eq!(
        body["attempts"].as_u64(),
        Some(1),
        "a credential failure was retried instead of failing once"
    );
}

fn accepting_manifest(origin: &str, requires_acceptance: bool) -> String {
    format!(
        "name: terms-dataset\nartifacts:\n  - id: object\n    sources:\n      - {origin}/object\nlicense:\n  requires_acceptance: {requires_acceptance}\n"
    )
}

#[test]
fn a_manifest_requiring_acceptance_without_yes_refuses_before_a_byte_moves() {
    let server = TestServer::start(Script::serving(object(256))).unwrap();
    let cache = TempDir::new().unwrap();
    let workspace = TempDir::new().unwrap();
    let manifest_path = workspace.path().join("dataset.yaml");
    std::fs::write(&manifest_path, accepting_manifest(&server.origin(), true)).unwrap();
    let destination = workspace.path().join("out");

    let output = run_with(
        cache.path(),
        &[],
        &[
            "get",
            manifest_path.to_str().unwrap(),
            "--output",
            destination.to_str().unwrap(),
            "--json",
        ],
    );

    assert_eq!(output.status.code(), Some(40));
    let body: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(body["kind"].as_str(), Some("policy.terms_required"));
    assert!(
        server.received().is_empty(),
        "a byte moved before the recorded terms were accepted: {:?}",
        server.received()
    );
    assert!(!destination.exists(), "a destination was written");
}

#[test]
fn the_same_manifest_with_yes_completes_and_the_receipt_records_the_acceptance() {
    let server = TestServer::start(Script::serving(object(256))).unwrap();
    let cache = TempDir::new().unwrap();
    let workspace = TempDir::new().unwrap();
    let manifest_path = workspace.path().join("dataset.yaml");
    std::fs::write(&manifest_path, accepting_manifest(&server.origin(), true)).unwrap();
    let destination = workspace.path().join("out");

    let output = run_with(
        cache.path(),
        &[],
        &[
            "get",
            manifest_path.to_str().unwrap(),
            "--output",
            destination.to_str().unwrap(),
            "--json",
            "--yes",
        ],
    );

    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr was {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(destination.join("object").exists());

    let work = Arc::new(WorkCounter::new());
    let processor = Arc::new(
        Processor::new(ThreadBudget::resolve(
            std::thread::available_parallelism().unwrap_or(std::num::NonZeroUsize::MIN),
            None,
        ))
        .unwrap(),
    );
    let held: Cache<NativePlatform> =
        cache_cli::require(cache.path(), work, processor).expect("the cache could not be opened");
    let receipt = held
        .read_receipt(&destination)
        .expect("the receipt could not be read")
        .expect("no receipt was written");
    assert_eq!(
        receipt.accepted_terms,
        Some(fetchloom_engine::license::Acceptance::Asserted),
        "the receipt did not record that the terms were accepted"
    );
}

#[test]
fn a_source_refusing_without_a_credential_stops_the_run_with_steps_a_first_time_user_can_follow() {
    let server = TestServer::start(Script::serving(Vec::new()).replying(vec![Reply::Status {
        code: 401,
        retry_after: None,
    }]))
    .unwrap();
    let cache = TempDir::new().unwrap();
    let destination = TempDir::new().unwrap();
    let events = destination.path().join("events.ndjson");

    let refused = run_with(
        cache.path(),
        &[],
        &[
            "get",
            &format!("{}/object.bin", server.origin()),
            "--output",
            destination.path().join("out").to_str().unwrap(),
            "--events",
            events.to_str().unwrap(),
            "--json",
        ],
    );

    assert_eq!(
        refused.status.code(),
        Some(40),
        "a run needing a credential it does not have did not exit on policy: {}",
        String::from_utf8_lossy(&refused.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&refused.stdout).unwrap();
    assert_eq!(result["kind"].as_str(), Some("policy.credential_missing"));

    let stream = std::fs::read_to_string(&events).unwrap();
    assert!(
        stream.contains("credential.required"),
        "credential.required was not emitted by a real run: {stream}"
    );

    let printed = String::from_utf8_lossy(&refused.stderr);
    assert!(
        printed.contains("FETCHLOOM_TOKEN_"),
        "the run did not print where to put a token: {printed}"
    );
    assert!(
        printed.contains('1') && printed.contains('2'),
        "the run did not print numbered steps: {printed}"
    );
}
