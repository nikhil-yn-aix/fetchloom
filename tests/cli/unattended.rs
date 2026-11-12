//! fetchloom as a script or an agent runs it: no terminal anywhere.

use crate::support::{Result, Running, Sandbox, Server, file, reply, text};

const IRIS: &[u8] = b"sepal_length,species\n5.1,setosa\n";

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
        (&["list"], 0),
        (&["export"], 0),
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
        let output = Running::start_with_open_stdin(&mut sandbox.command(args))?
            .finish()
            .map_err(|error| format!("{args:?}: {error}"))?;
        assert_eq!(
            output.status.code(),
            Some(code),
            "{args:?}: {}",
            text(&output.stderr)?
        );
    }
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn cli_output_ctrl_c_cancels_an_open_link_list() -> Result {
    use rustix::process::{Pid, Signal, kill_process};
    use std::io::Write as _;
    use std::process::Stdio;

    let sandbox = Sandbox::new()?;
    let mut child = sandbox
        .command(&["get", "-i", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut input = child.stdin.take().ok_or("stdin was not piped")?;
    input.write_all(b"# the producer keeps this pipe open\n")?;
    let ended = (|| -> Result {
        crate::support::wait_until("the link-list reader to consume input", || {
            Ok(rustix::io::ioctl_fionread(&input)? == 0)
        })?;
        kill_process(Pid::from_child(&child), Signal::INT)?;
        crate::support::wait_until("the first ctrl-c to end the link-list read", || {
            Ok(child.try_wait()?.is_some())
        })
    })();
    if ended.is_err() {
        let _ = child.kill();
    }
    let output = child.wait_with_output()?;
    drop(input);
    let stderr = text(&output.stderr)?;
    assert!(ended.is_ok(), "{ended:?}\n{stderr}");
    assert_eq!(output.status.code(), Some(130), "{stderr}");
    assert!(
        stderr.contains("    Stopped at your request\n") && !stderr.contains("error:"),
        "{stderr}"
    );
    assert!(
        !sandbox.cache().exists(),
        "cancelled input must not open the cache"
    );
    Ok(())
}
