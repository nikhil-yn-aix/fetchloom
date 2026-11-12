//! a child whose stderr can signal a race condition before the test releases its server.

use std::io::{self, BufRead, Read};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::Result;

pub(crate) struct Running {
    child: Option<Child>,
    stdout: Option<JoinHandle<io::Result<Vec<u8>>>>,
    stderr: Option<JoinHandle<io::Result<Vec<u8>>>>,
    lines: Receiver<String>,
}

impl Running {
    pub(crate) fn start(command: &mut Command) -> Result<Self> {
        let mut child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let mut stdout = child.stdout.take().ok_or("child stdout was not piped")?;
        let stdout = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            stdout.read_to_end(&mut bytes)?;
            Ok(bytes)
        });
        let stderr = child.stderr.take().ok_or("child stderr was not piped")?;
        let (sender, lines) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut reader = io::BufReader::new(stderr);
            let mut bytes = Vec::new();
            loop {
                let start = bytes.len();
                if reader.read_until(b'\n', &mut bytes)? == 0 {
                    break;
                }
                let _ = sender.send(String::from_utf8_lossy(&bytes[start..]).into_owned());
            }
            Ok(bytes)
        });
        Ok(Self {
            child: Some(child),
            stdout: Some(stdout),
            stderr: Some(reader),
            lines,
        })
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
        let child = self.child.as_ref().ok_or("child already finished")?;
        let sent = Command::new("kill")
            .args([signal, &child.id().to_string()])
            .status()?;
        if sent.success() {
            Ok(())
        } else {
            Err(format!("kill {signal} failed").into())
        }
    }

    pub(crate) fn finish(mut self) -> Result<Output> {
        let status = self
            .child
            .as_mut()
            .ok_or("child already finished")?
            .wait()?;
        self.child = None;
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

    #[test]
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
