//! a program that prints a credential, such as a `credential_process`, the AWS CLI or the az
//! cli. it is found by a full path and runs with no shell, from its own folder, with nothing on
//! its input. what it prints on stdout is read up to 64 KiB and kept in memory alone; stderr is
//! never read, so nothing it prints reaches a message, a log or a file. a stop, or a run past its
//! time, ends it.

use std::io::Read as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::stop::Stop;

/// the most a program may print: keys and a token take a few KiB.
pub(crate) const LARGEST: u64 = 64 << 10;

/// how long a program may run, which leaves time to answer a question it asks on the terminal,
/// such as the code of a second sign-in step.
pub(crate) const PATIENCE: Duration = Duration::from_secs(60);

/// how long a program that ended may hold its output open, as a process it started can.
const DRAIN: Duration = Duration::from_secs(2);

/// how a run went wrong.
pub(crate) enum Ran {
    Unstarted(std::io::Error),
    /// it ended with this exit code, none for a signal.
    Failed(Option<i32>),
    Late,
    TooMuch,
    /// it ended while a process it started held its output open.
    Open,
    Lost(std::io::Error),
    Stopped,
}

impl Ran {
    /// what went wrong with a run of `named`, the program at `path` that prints `what`, in words
    /// that never repeat what it printed. none for a stop, which is no failure of the program's.
    pub(crate) fn said(
        self,
        named: &str,
        what: &str,
        path: &Path,
        patience: Duration,
    ) -> Option<String> {
        Some(match self {
            Ran::Stopped => return None,
            Ran::Unstarted(err) => format!(
                "{named} could not start {}: {}",
                path.display(),
                crate::error::said(&err)
            ),
            Ran::Failed(Some(code)) => format!("{named} exited with code {code}"),
            Ran::Failed(None) => format!("{named} was ended by a signal"),
            Ran::Late => format!(
                "{named} gave no {what} within {}, so fetchloom ended it",
                crate::units::duration(patience)
            ),
            Ran::TooMuch => format!("{named} printed more than {} KiB", LARGEST >> 10),
            Ran::Open => format!("{named} ended, but a program it started kept its output open"),
            Ran::Lost(err) => format!("{named} could not be read: {}", crate::error::said(&err)),
        })
    }
}

/// what the program at `path`, run with `args`, printed on stdout once it ended well, within
/// `patience`.
pub(crate) async fn printed(
    path: &Path,
    args: &[String],
    patience: Duration,
    stop: &Stop,
) -> Result<Vec<u8>, Ran> {
    if stop.is_asked() {
        return Err(Ran::Stopped);
    }
    let mut child = Command::new(path)
        .args(args)
        // started outside the project, so windows never loads a library the project holds
        .current_dir(path.parent().unwrap_or(path))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(Ran::Unstarted)?;
    let Some(mut stdout) = child.stdout.take() else {
        end(&mut child);
        return Err(Ran::Lost(std::io::ErrorKind::BrokenPipe.into()));
    };
    let (sender, mut answer) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let read = (&mut stdout).take(LARGEST + 1).read_to_end(&mut bytes);
        let _ = sender.send(read.map(|_| bytes));
    });
    let deadline = tokio::time::Instant::now() + patience;
    let mut pause = Duration::from_millis(1);
    let mut output = None;
    loop {
        if output.is_none() {
            output = answer.try_recv().ok();
        }
        if let Some(Ok(bytes)) = &output
            && bytes.len() as u64 > LARGEST
        {
            end(&mut child);
            return Err(Ran::TooMuch);
        }
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(status)) => return Err(Ran::Failed(status.code())),
            Ok(None) => {}
            Err(err) => {
                end(&mut child);
                return Err(Ran::Lost(err));
            }
        }
        if tokio::time::Instant::now() >= deadline {
            end(&mut child);
            return Err(Ran::Late);
        }
        tokio::select! {
            () = tokio::time::sleep(pause) => {}
            () = stop.asked() => {
                end(&mut child);
                return Err(Ran::Stopped);
            }
        }
        pause = (pause * 2).min(Duration::from_millis(50));
    }
    let output = if let Some(output) = output {
        output
    } else {
        tokio::select! {
            answered = tokio::time::timeout(DRAIN, answer) => match answered {
                Ok(Ok(output)) => output,
                _ => return Err(Ran::Open),
            },
            () = stop.asked() => return Err(Ran::Stopped),
        }
    };
    let bytes = output.map_err(Ran::Lost)?;
    if bytes.len() as u64 > LARGEST {
        return Err(Ran::TooMuch);
    }
    Ok(bytes)
}

/// ends a program that is still running, and waits for it.
fn end(child: &mut std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}
