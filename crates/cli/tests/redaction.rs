//! What a transport failure is allowed to say about the location it failed on.

#![expect(
    clippy::unwrap_used,
    reason = "test setup, where a failure to run the binary is the assertion"
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
use serde_json as _;
use toml as _;
#[cfg(windows)]
use windows_sys as _;

use fetchloom_faults::{Reply, Script, TestServer};
mod support;

use tempfile::TempDir;

/// A value that must never appear in any stream a run writes.
const SENTINEL: &str = "sentinel-token-4f19bd7c";

#[test]
fn a_transport_failure_never_prints_the_location_it_failed_on() {
    let temporary = TempDir::new().unwrap();
    let events = temporary.path().join("events.ndjson");

    for reference in [
        format!("https://example.invalid/object?token={SENTINEL}"),
        format!("https://user:{SENTINEL}@example.invalid/object"),
        format!("https://example.invalid/ object?token={SENTINEL}"),
    ] {
        let output = support::fetchloom()
            .arg("get")
            .arg(&reference)
            .arg("--output")
            .arg(temporary.path().join("out"))
            .arg("--no-config")
            .arg("--retries")
            .arg("0")
            .arg("--events")
            .arg(&events)
            .arg("--json")
            .env("FETCHLOOM_CACHE_DIR", temporary.path().join("cache"))
            .output()
            .unwrap();

        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        let stream = std::fs::read_to_string(&events).unwrap_or_default();

        for (name, body) in [
            ("the result", &stdout),
            ("the log", &stderr),
            ("the event stream", &stream),
        ] {
            assert!(
                !body.contains(SENTINEL),
                "{name} carried the secret from {reference}: {body}"
            );
        }
    }
}

#[test]
fn a_location_a_transport_could_not_parse_is_redacted_like_any_other() {
    let server =
        TestServer::start(
            Script::serving(vec![0_u8; 32]).replying(vec![Reply::Redirect {
                code: 302,
                location: format!("this-is-not-a-url?token={SENTINEL}"),
            }]),
        )
        .unwrap();

    let temporary = TempDir::new().unwrap();
    let events = temporary.path().join("events.ndjson");
    let output = support::fetchloom()
        .arg("get")
        .arg(format!("{}/object", server.origin()))
        .arg("--output")
        .arg(temporary.path().join("out"))
        .arg("--no-config")
        .arg("--events")
        .arg(&events)
        .arg("--json")
        .env("FETCHLOOM_CACHE_DIR", temporary.path().join("cache"))
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let stream = std::fs::read_to_string(&events).unwrap_or_default();

    assert!(
        !output.status.success(),
        "a redirect to something that is not a location succeeded: {stdout}"
    );
    for (name, body) in [
        ("the result", &stdout),
        ("the log", &stderr),
        ("the event stream", &stream),
    ] {
        assert!(
            !body.contains(SENTINEL),
            "{name} carried the secret out of a location it could not parse: {body}"
        );
    }
}
