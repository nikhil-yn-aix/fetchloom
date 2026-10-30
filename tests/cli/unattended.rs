//! fetchloom as a script or an agent runs it: no terminal anywhere.

use std::process::{Output, Stdio};
use std::time::{Duration, Instant};

use crate::support::{Result, Sandbox, Server, file, reply, text};

const IRIS: &[u8] = b"sepal_length,species\n5.1,setosa\n";

/// runs a command whose stdin is a pipe that stays open and never sends a byte, as many tool
/// runners leave it. a command that read stdin or waited for an answer would never finish.
fn finishes(sandbox: &Sandbox, args: &[&str]) -> Result<Output> {
    let mut child = sandbox
        .command(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let open = child.stdin.take();
    let started = Instant::now();
    while child.try_wait()?.is_none() {
        if started.elapsed() > Duration::from_secs(60) {
            child.kill()?;
            return Err(format!("{args:?} is still waiting after a minute").into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    drop(open);
    Ok(child.wait_with_output()?)
}

#[test]
fn no_command_waits_for_input_when_stdin_is_not_a_terminal() -> Result {
    let sandbox = Sandbox::new()?;
    let iris = Server::start("/files/iris.csv", vec![file(IRIS, "\"v1\"")])?;
    let huge = || reply("200 OK", &[("Content-Length", "2147483648")], b"");
    let large = Server::start("/huge.bin", vec![huge(), huge()])?;
    for (args, code) in [
        (&[][..], 0),
        (&["add", iris.url.as_str()], 0),
        (&["sync"], 0),
        (&["lock"], 0),
        (&["lock", "--check"], 0),
        (&["status"], 0),
        (&["path", "iris"], 0),
        (&[], 0),
        (&["add", large.url.as_str()], 1),
        (&["get", large.url.as_str()], 1),
        (&["remove", "iris"], 0),
        (&["cache"], 0),
        (&["cache", "gc"], 0),
        (&["cache", "verify"], 0),
        (&["--help"], 0),
    ] {
        let output = finishes(&sandbox, args)?;
        assert_eq!(
            output.status.code(),
            Some(code),
            "{args:?}: {}",
            text(&output.stderr)?
        );
    }
    Ok(())
}
