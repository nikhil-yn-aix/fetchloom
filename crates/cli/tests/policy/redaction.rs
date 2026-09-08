//! What a transport failure is allowed to say about the location it failed on.

#![expect(
    clippy::unwrap_used,
    reason = "test setup, where a failure to run the binary is the assertion"
)]

use crate::support;
use fetchloom_faults::{Reply, Script, TestServer};

use std::path::{Path, PathBuf};

use tempfile::TempDir;

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

fn every_file(root: &Path, seen: &mut Vec<PathBuf>) {
    let Ok(listing) = std::fs::read_dir(root) else {
        return;
    };
    for entry in listing.flatten() {
        let path = entry.path();
        if path.is_dir() {
            every_file(&path, seen);
        } else {
            seen.push(path);
        }
    }
}

#[test]
fn a_credential_reaches_the_source_and_no_artifact_the_run_writes() {
    let bytes: Vec<u8> = (0..4096u32).map(|index| (index % 251) as u8).collect();
    let server = TestServer::start(Script::serving(bytes)).unwrap();
    let temporary = TempDir::new().unwrap();
    let destination = temporary.path().join("out");
    let cache = temporary.path().join("cache");
    let lock = temporary.path().join("fetchloom.lock");
    let events = temporary.path().join("events.ndjson");
    let location = format!("{}/object.bin?signature={SENTINEL}", server.origin());
    let host = server
        .origin()
        .trim_start_matches("http://")
        .split(':')
        .next()
        .unwrap()
        .replace(['.', '-'], "_")
        .to_uppercase();

    let fetched = support::fetchloom()
        .arg("get")
        .arg(&location)
        .arg("--output")
        .arg(&destination)
        .arg("--lock")
        .arg(&lock)
        .arg("--events")
        .arg(&events)
        .arg("--no-config")
        .arg("--json")
        .env("FETCHLOOM_CACHE_DIR", &cache)
        .env(
            format!("FETCHLOOM_TOKEN_{host}"),
            format!("Bearer {SENTINEL}"),
        )
        .output()
        .unwrap();
    assert!(
        fetched.status.success(),
        "the fetch failed: {}",
        String::from_utf8_lossy(&fetched.stderr)
    );

    let asked = server.received();
    assert_eq!(
        asked.last().unwrap().header("authorization"),
        Some(format!("Bearer {SENTINEL}").as_str()),
        "the credential never reached the source, so nothing about redaction was tested"
    );

    let planned = support::fetchloom()
        .arg("plan")
        .arg(&location)
        .arg("--output")
        .arg(&destination)
        .arg("--lock")
        .arg(&lock)
        .arg("--no-config")
        .arg("--json")
        .env("FETCHLOOM_CACHE_DIR", &cache)
        .env(
            format!("FETCHLOOM_TOKEN_{host}"),
            format!("Bearer {SENTINEL}"),
        )
        .output()
        .unwrap();

    let mut written = vec![lock.clone(), events.clone()];
    every_file(&cache, &mut written);
    every_file(&destination, &mut written);
    assert!(
        written.len() > 4,
        "the run left {} files, so this proves nothing",
        written.len()
    );

    for path in written {
        let held = std::fs::read(&path).unwrap_or_default();
        assert!(
            !held
                .windows(SENTINEL.len())
                .any(|window| window == SENTINEL.as_bytes()),
            "{} carries the secret",
            path.display()
        );
    }
    for (what, stream) in [
        ("the result", &fetched.stdout),
        ("the log", &fetched.stderr),
        ("the plan", &planned.stdout),
        ("the plan's log", &planned.stderr),
    ] {
        assert!(
            !stream
                .windows(SENTINEL.len())
                .any(|window| window == SENTINEL.as_bytes()),
            "{what} carries the secret"
        );
    }
}
