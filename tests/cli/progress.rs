//! what a run says while it waits when stderr is no terminal, as in a log or a pipe.

use std::time::Duration;

use crate::support::{Part, Result, Sandbox, Server, reply, text};

const IRIS: &[u8] = b"sepal_length,species\n5.1,setosa\n";

#[test]
fn a_long_download_says_what_it_is_doing_once() -> Result {
    let sandbox = Sandbox::new()?;
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
    let server = Server::start("/files/iris.csv", vec![slow])?;
    let output = sandbox.run(&["add", &server.url])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    let waiting: Vec<&str> = stderr
        .lines()
        .filter(|line| line.contains("Downloading"))
        .collect();
    assert_eq!(waiting, ["Downloading iris.csv · 32 B"], "{stderr}");
    assert!(
        stderr.find("Downloading") < stderr.find("Downloaded"),
        "{stderr}"
    );
    Ok(())
}
