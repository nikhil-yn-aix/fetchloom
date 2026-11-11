//! what a run says while it waits when stderr is no terminal, as in a log or a pipe.

use std::time::Duration;

use crate::support::{Part, Result, Sandbox, Server, reply, text};

const IRIS: &[u8] = b"sepal_length,species\n5.1,setosa\n";

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
