//! a command running beside its test: its stderr read as it comes, so the test can wait for a
//! line before it releases a server or stops the command, and a finish that gives up on a command
//! still running after a minute.

use std::io::{self, BufRead, Read};
use std::process::{Child, ChildStdin, Command, ExitStatus, Output, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::Result;

/// how long [`Running::finish`] waits for a command to close its output.
const FINISH: Duration = Duration::from_secs(60);

pub(crate) struct Running {
    child: Option<Child>,
    // open and silent until the command ends, for [`Running::start_with_open_stdin`]
    stdin: Option<ChildStdin>,
    stdout: Option<JoinHandle<io::Result<Vec<u8>>>>,
    stderr: Option<JoinHandle<io::Result<Vec<u8>>>>,
    lines: Receiver<String>,
    closed: Receiver<()>,
}

impl Running {
    /// starts `command`, its stdin as the command sets it.
    pub(crate) fn start(command: &mut Command) -> Result<Self> {
        Self::spawn(command)
    }

    /// starts `command` with stdin a pipe that stays open and never sends a byte, as many tool
    /// runners leave it, so a command that read stdin or waited for an answer never finishes.
    pub(crate) fn start_with_open_stdin(command: &mut Command) -> Result<Self> {
        Self::spawn(command.stdin(Stdio::piped()))
    }

    fn spawn(command: &mut Command) -> Result<Self> {
        let mut child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let stdin = child.stdin.take();
        let (done, closed) = mpsc::channel();
        let mut stdout = child.stdout.take().ok_or("child stdout was not piped")?;
        let stdout_done = done.clone();
        let stdout = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let read = stdout.read_to_end(&mut bytes);
            let _ = stdout_done.send(());
            read.map(|_| bytes)
        });
        let stderr = child.stderr.take().ok_or("child stderr was not piped")?;
        let (sender, lines) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut reader = io::BufReader::new(stderr);
            let mut bytes = Vec::new();
            let read = loop {
                let start = bytes.len();
                match reader.read_until(b'\n', &mut bytes) {
                    Ok(0) => break Ok(()),
                    Ok(_) => {
                        let line = String::from_utf8_lossy(&bytes[start..]).into_owned();
                        let _ = sender.send(line);
                    }
                    Err(error) => break Err(error),
                }
            };
            let _ = done.send(());
            read.map(|()| bytes)
        });
        Ok(Self {
            child: Some(child),
            stdin,
            stdout: Some(stdout),
            stderr: Some(reader),
            lines,
            closed,
        })
    }

    /// the command's process id.
    pub(crate) fn id(&self) -> Result<u32> {
        Ok(self.child.as_ref().ok_or("child already finished")?.id())
    }

    pub(crate) fn wait_for(&self, text: &str) -> Result {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let line = self
                .lines
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .map_err(|error| format!("waiting for stderr to contain {text:?}: {error}"))?;
            if line.contains(text) {
                return Ok(());
            }
        }
    }

    /// sends SIGINT, as ctrl-c at a terminal does.
    #[cfg(unix)]
    pub(crate) fn interrupt(&self) -> Result {
        self.signal("-INT")
    }

    /// sends `signal`, such as `-TERM`, which a scheduler sends a job it ends, with `kill`.
    #[cfg(unix)]
    pub(crate) fn signal(&self, signal: &str) -> Result {
        let sent = Command::new("kill")
            .args([signal, &self.id()?.to_string()])
            .status()?;
        if sent.success() {
            Ok(())
        } else {
            Err(format!("kill {signal} failed").into())
        }
    }

    /// ends the command at once, as a killed job ends.
    pub(crate) fn kill(&mut self) -> Result {
        Ok(self
            .child
            .as_mut()
            .ok_or("child already finished")?
            .kill()?)
    }

    /// whether the command has ended.
    #[cfg(unix)]
    pub(crate) fn has_ended(&mut self) -> Result<bool> {
        let child = self.child.as_mut().ok_or("child already finished")?;
        Ok(child.try_wait()?.is_some())
    }

    /// waits for the command to end, while what it started may still hold its output open.
    pub(crate) fn wait(&mut self) -> Result<ExitStatus> {
        Ok(self
            .child
            .as_mut()
            .ok_or("child already finished")?
            .wait()?)
    }

    /// the command's status and output, once it has closed its output. a command still running
    /// after [`FINISH`] is killed, and the error holds what it said.
    pub(crate) fn finish(mut self) -> Result<Output> {
        let deadline = Instant::now() + FINISH;
        for closed in 0..2 {
            if self
                .closed
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .is_err()
            {
                let said = self.give_up(closed);
                return Err(format!("the command was still running after a minute\n{said}").into());
            }
        }
        let status = self.wait()?;
        self.child = None;
        self.stdin = None;
        let stdout = self
            .stdout
            .take()
            .ok_or("stdout already collected")?
            .join()
            .map_err(|_| "stdout reader panicked")??;
        let stderr = self
            .stderr
            .take()
            .ok_or("stderr already collected")?
            .join()
            .map_err(|_| "stderr reader panicked")??;
        Ok(Output {
            status,
            stdout,
            stderr,
        })
    }

    /// kills the command, which had closed `closed` of its two outputs, and gives what it said, the
    /// only clue to a stuck run on a runner nobody can reach.
    fn give_up(&mut self, closed: usize) -> String {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        for _ in closed..2 {
            if self
                .closed
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .is_err()
            {
                // left to run on, so dropping the run does not wait on them as well
                self.stdout.take();
                self.stderr.take();
                return "and what it started still holds its output open".to_owned();
            }
        }
        [self.stdout.take(), self.stderr.take()]
            .into_iter()
            .flatten()
            .map(|reader| match reader.join() {
                Ok(Ok(bytes)) => String::from_utf8_lossy(&bytes).into_owned(),
                _ => String::new(),
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(reader) = self.stdout.take() {
            let _ = reader.join();
        }
        if let Some(reader) = self.stderr.take() {
            let _ = reader.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;
    use std::process::Command;

    use super::{Result, Running};

    const CHILD: &str = "FETCHLOOM_TEST_PIPE_CHILD";
    const BYTES: usize = 4 * 1024 * 1024;

    /// the child `stderr_readiness_does_not_wait_for_stdout_collection` starts: it fills more
    /// stdout than a pipe holds, then says on stderr that it is ready. alone it tests nothing.
    #[test]
    #[ignore = "a child process its parent test starts"]
    fn stdout_pipe_child() -> Result {
        if std::env::var_os(CHILD).is_none() {
            return Ok(());
        }
        let block = vec![b'~'; 64 * 1024];
        let mut stdout = std::io::stdout().lock();
        for _ in 0..BYTES / block.len() {
            stdout.write_all(&block)?;
        }
        stdout.flush()?;
        writeln!(std::io::stderr(), "pipe child ready")?;
        Ok(())
    }

    #[test]
    fn stderr_readiness_does_not_wait_for_stdout_collection() -> Result {
        let mut command = Command::new(std::env::current_exe()?);
        command
            .args([
                "--exact",
                "support::running::tests::stdout_pipe_child",
                "--ignored",
                "--nocapture",
            ])
            .env(CHILD, "1");
        let child = Running::start(&mut command)?;
        child.wait_for("pipe child ready")?;
        let output = child.finish()?;
        assert!(output.status.success());
        let block = output
            .stdout
            .split(|byte| *byte != b'~')
            .max_by_key(|part| part.len())
            .unwrap_or_default();
        assert_eq!(block.len(), BYTES);
        assert!(String::from_utf8_lossy(&output.stderr).contains("pipe child ready"));
        Ok(())
    }
}
