//! a child whose stderr can signal a race condition before the test releases its server.

use std::io::{self, BufRead};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::Result;

pub(crate) struct Running {
    child: Option<Child>,
    stderr: Option<JoinHandle<io::Result<Vec<u8>>>>,
    lines: Receiver<String>,
}

impl Running {
    pub(crate) fn start(command: &mut Command) -> Result<Self> {
        let mut child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
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

    pub(crate) fn finish(mut self) -> Result<Output> {
        let child = self.child.take().ok_or("child already finished")?;
        let mut output = child.wait_with_output()?;
        output.stderr = self
            .stderr
            .take()
            .ok_or("stderr already collected")?
            .join()
            .map_err(|_| "stderr reader panicked")??;
        Ok(output)
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(reader) = self.stderr.take() {
            let _ = reader.join();
        }
    }
}
