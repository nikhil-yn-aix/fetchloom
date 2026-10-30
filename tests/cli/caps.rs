//! resource caps read from the environment.

use std::time::{Duration, Instant};

use crate::support::{Result, Sandbox, Server, file, json, text};

#[test]
fn a_rate_cap_slows_a_download() -> Result {
    let sandbox = Sandbox::new()?;
    let body = vec![b'x'; 2 * 1024 * 1024];
    let server = Server::start("/capped.bin", vec![file(&body, "\"v1\"")])?;
    let started = Instant::now();
    let output = sandbox
        .command(&["--json", "get", &server.url])
        .env("FETCHLOOM_RATE", "1M")
        .output()?;
    let elapsed = started.elapsed();
    assert!(output.status.success(), "{}", text(&output.stdout)?);
    // the first MiB may burst; the second needs one second's tokens.
    assert!(elapsed >= Duration::from_secs(1), "{elapsed:?}");
    assert_eq!(json(&output)?["work"]["bytes_down"], body.len());
    assert_eq!(
        std::fs::read(sandbox.work().join("capped").join("capped.bin"))?,
        body
    );
    Ok(())
}

#[test]
fn a_bad_rate_is_a_settings_error() -> Result {
    let sandbox = Sandbox::new()?;
    let output = sandbox
        .command(&["--json", "cache"])
        .env("FETCHLOOM_RATE", "fast")
        .output()?;
    assert_eq!(output.status.code(), Some(2));
    let document = json(&output)?;
    assert_eq!(document["errors"][0]["kind"], "usage");
    assert_eq!(
        document["errors"][0]["message"],
        "FETCHLOOM_RATE is \"fast\", which is not a positive byte rate"
    );
    assert_eq!(
        document["errors"][0]["help"],
        "unset FETCHLOOM_RATE for no cap, or use bytes per second such as 500K or 2.5M"
    );
    Ok(())
}

#[test]
fn a_bad_thread_cap_is_a_settings_error() -> Result {
    let sandbox = Sandbox::new()?;
    let output = sandbox
        .command(&["--json", "cache"])
        .env("FETCHLOOM_THREADS", "257")
        .output()?;
    assert_eq!(output.status.code(), Some(2));
    let document = json(&output)?;
    assert_eq!(document["errors"][0]["kind"], "usage");
    assert_eq!(
        document["errors"][0]["message"],
        "FETCHLOOM_THREADS is \"257\", which is not a number from 1 to 256"
    );
    assert_eq!(
        document["errors"][0]["help"],
        "unset FETCHLOOM_THREADS to use the default worker counts, or set it to a number from 1 to 256"
    );
    Ok(())
}

#[test]
fn one_thread_unpacks_places_and_verifies_a_project() -> Result {
    let sandbox = Sandbox::new()?;
    let zip = std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("archives")
            .join("python.zip"),
    )?;
    let server = Server::start("/dataset.zip", vec![file(&zip, "\"v1\"")])?;
    for args in [
        vec!["--json", "add", &server.url],
        vec!["--json", "sync"],
        vec!["--json", "status"],
        vec!["--json", "cache", "verify"],
    ] {
        let output = sandbox
            .command(&args)
            .env("FETCHLOOM_THREADS", "1")
            .output()?;
        assert!(output.status.success(), "{}", text(&output.stdout)?);
    }
    assert_eq!(
        std::fs::read(sandbox.work().join("data").join("dataset").join("README.md"))?,
        b"# sample dataset\n"
    );
    assert_eq!(server.requests().len(), 1);
    Ok(())
}
