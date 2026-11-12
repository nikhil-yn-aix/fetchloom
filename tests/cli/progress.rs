//! what a run says while it waits when stderr is no terminal, as in a log or a pipe.

use std::sync::mpsc::{self, Sender};
use std::time::Duration;

use crate::support::{Part, Result, Running, Sandbox, Server, reply, text};

const IRIS: &[u8] = b"sepal_length,species\n5.1,setosa\n";

/// a reply that sends its head at once and its body once the sender it returns is dropped, so
/// a download lasts until the test lets it end.
fn held_iris() -> (Vec<Part>, Sender<()>) {
    let mut held = reply(
        "200 OK",
        &[
            ("Content-Length", &IRIS.len().to_string()),
            ("ETag", "\"v1\""),
        ],
        b"",
    );
    let (release, waiting) = mpsc::channel();
    held.push(Part::Wait(waiting));
    held.push(Part::Bytes(IRIS.to_vec()));
    (held, release)
}

/// a reply that holds back its body for 3 s, past the moment a batch says what it is doing.
fn slow_iris() -> Vec<Part> {
    let mut slow = reply(
        "200 OK",
        &[
            ("Content-Length", &IRIS.len().to_string()),
            ("ETag", "\"v1\""),
        ],
        b"",
    );
    slow.push(Part::Pause(Duration::from_millis(3000)));
    slow.push(Part::Bytes(IRIS.to_vec()));
    slow
}

/// the sandbox turns off the lines a batch tells as time passes, so a run prints the same
/// lines on a busy machine as on an idle one, and a snapshot of it holds.
#[test]
fn tests_see_no_line_that_time_decides() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/files/iris.csv", vec![slow_iris()])?;
    let output = sandbox.run(&["add", &server.url])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(!stderr.contains("Downloading iris.csv"), "{stderr}");
    Ok(())
}

#[test]
fn a_long_download_says_what_it_is_doing_once() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/files/iris.csv", vec![slow_iris()])?;
    let output = sandbox
        .command(&["add", &server.url])
        .env("FETCHLOOM_TEST_TICKER", "on")
        .output()?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    // a loaded machine can take past 10 s, which adds lines of how far it got; they come after
    let started: Vec<&str> = stderr
        .lines()
        .filter(|line| line.starts_with("Downloading iris.csv"))
        .collect();
    assert_eq!(started, ["Downloading iris.csv · 32 B"], "{stderr}");
    assert!(
        stderr.find("Downloading") < stderr.find("Downloaded"),
        "{stderr}"
    );
    Ok(())
}

/// `--json` alone keeps stderr empty, and `--progress` beside it keeps the plain lines a long
/// download tells there, while stdout holds the one JSON document either way. the run without
/// the flag starts its download first, so it has run longer when the other tells its first line.
#[test]
fn progress_tells_its_lines_beside_json() -> Result {
    let (quiet_reply, release_quiet) = held_iris();
    let (told_reply, release_told) = held_iris();
    let quiet_server = Server::start("/files/iris.csv", vec![quiet_reply])?;
    let told_server = Server::start("/files/iris.csv", vec![told_reply])?;
    let (quiet_sandbox, told_sandbox) = (Sandbox::new()?, Sandbox::new()?);
    let quiet = Running::start(
        quiet_sandbox
            .command(&["--json", "get", &quiet_server.url])
            .env("FETCHLOOM_TEST_TICKER", "on"),
    )?;
    quiet_server.wait_for_requests(1)?;
    let told = Running::start(
        told_sandbox
            .command(&["--json", "--progress", "get", &told_server.url])
            .env("FETCHLOOM_TEST_TICKER", "on"),
    )?;
    let telling = told.wait_for("Downloading iris.csv");
    drop((release_quiet, release_told));
    telling?;
    let (quiet, told) = (quiet.finish()?, told.finish()?);
    for output in [&quiet, &told] {
        assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
        let document: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(document["status"], "ok", "{document}");
    }
    assert_eq!(text(&quiet.stderr)?, "");
    let stderr = text(&told.stderr)?;
    assert_eq!(
        stderr.lines().next(),
        Some("Downloading iris.csv · 32 B"),
        "{stderr}"
    );
    // a loaded machine can take past 10 s, which adds lines of how far it got
    assert!(
        stderr.lines().all(|line| {
            let line = line.trim_start();
            line.starts_with("Downloading ") || line.starts_with("Waiting for the disk")
        }),
        "{stderr}"
    );
    Ok(())
}
