//! running one step: its program and arguments, never read by a shell, in the project folder,
//! with nothing on stdin and both outputs read into a log only its owner may read. the log keeps
//! the latest 16 MiB, and the last lines stay in memory for the error a failed step gives.
//!
//! on unix the step runs in a process group of its own, so the terminal's ctrl-c reaches only
//! fetchloom, which ends the whole group: when asked to stop, when the step ends, so nothing it
//! started still writes while its result is taken in, and when a second ctrl-c quits. windows
//! ends the step's own process only, until a job object ends what it started too.

use std::collections::{BTreeMap, VecDeque};
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::process::{Child, ExitStatus, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::recipe::{Slot, Template};
use crate::stop::Stop;

/// the most of a step's output its log keeps: the latest, once there is more.
pub(crate) const LOG_MOST: u64 = 16 << 20;

/// how much of the latest output stays in memory for an error.
const TAIL_BYTES: usize = 64 << 10;

/// the lines of output a failed step's error shows.
const TAIL_LINES: usize = 20;

/// the longest line passed on to the terminal; a longer one is cut into lines this long.
const LINE_BYTES: usize = 4096;

/// the most lines waiting for the terminal. a step that prints faster than a terminal shows
/// skips the rest, which its log keeps.
const WAITING_LINES: usize = 10_000;

/// the longest fetchloom waits for a step's outputs to close once it ended. a process the step
/// started that left its group can hold them open for ever.
const DRAIN: Duration = Duration::from_secs(2);

/// what to run for a step.
pub(crate) struct Command<'a> {
    /// the program and its arguments, each folder in place.
    pub(crate) words: Vec<OsString>,
    /// the folder it runs in: the project's.
    pub(crate) folder: &'a Path,
    /// the variables data.toml sets for it, on top of fetchloom's own environment.
    pub(crate) env: &'a BTreeMap<String, String>,
}

/// how a step ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Ended {
    /// it exited with this code.
    Exited(i32),
    /// a signal ended it, as when the system runs out of memory.
    #[cfg(unix)]
    Signaled(i32),
    /// a stop was asked, so fetchloom ended it.
    Stopped,
}

/// a step that ran.
#[derive(Debug)]
pub(crate) struct Ran {
    pub(crate) ended: Ended,
    /// its last lines of output, oldest first.
    pub(crate) tail: Vec<String>,
    pub(crate) took: Duration,
}

/// the words of `run` as the step gets them, with each folder `folder` gives in place: one
/// argument each, spaces and all, since no shell splits them.
pub(crate) fn words(run: &[Template], folder: &dyn Fn(Slot<'_>) -> OsString) -> Vec<OsString> {
    run.iter().map(|word| word.expand(folder)).collect()
}

/// runs `command` until it ends or `stop` asks, its outputs read into a new log at `log` and,
/// with `forward`, each line handed to it. what the step started that is left in its group ends
/// with it. `building` is the project the build works in, which the step gets as
/// `FETCHLOOM_BUILDING`.
///
/// # Errors
///
/// the program cannot start, as when no program of its name is on PATH, which is `NotFound`, or
/// the log cannot be written.
pub(crate) fn run(
    command: &Command<'_>,
    building: &Path,
    log: &Path,
    forward: Option<&dyn Fn(&str)>,
    stop: &Stop,
) -> io::Result<Ran> {
    let _span = tracing::debug_span!("step").entered();
    let log = Log::create(log)?;
    let mut process = process(command, building)?;
    let started = Instant::now();
    let mut child = process.spawn()?;
    // known to a second ctrl-c until the step's run ends
    let _running = group::Running::new(&child);
    let caught = Arc::new(Mutex::new(Caught {
        log,
        tail: VecDeque::new(),
        failed: None,
    }));
    let lines = Arc::new(Mutex::new(Lines {
        waiting: Vec::new(),
        skipped: 0,
        kept: forward.is_some(),
    }));
    let (done, finished) = mpsc::channel();
    let streams = [
        child
            .stdout
            .take()
            .map(|out| Box::new(out) as Box<dyn Read + Send>),
        child
            .stderr
            .take()
            .map(|out| Box::new(out) as Box<dyn Read + Send>),
    ];
    let mut readers = 0;
    for stream in streams.into_iter().flatten() {
        readers += 1;
        let (caught, lines, done) = (Arc::clone(&caught), Arc::clone(&lines), done.clone());
        std::thread::spawn(move || {
            read(stream, &caught, &lines);
            let _ = done.send(());
        });
    }
    let ended = watch(&mut child, &lines, forward, stop);
    group::end(&mut child);
    let ended = ended?;
    let deadline = Instant::now() + DRAIN;
    for _ in 0..readers {
        if finished
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .is_err()
        {
            tracing::debug!(
                "a process the step started holds its output open, so the rest is unread"
            );
            break;
        }
    }
    hand_on(&lines, forward);
    let mut caught = caught.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(err) = caught.failed.take() {
        return Err(err);
    }
    Ok(Ran {
        ended,
        tail: tail(&caught.tail),
        took: started.elapsed(),
    })
}

/// the process for `command`: its program and arguments, in its folder, with stdin closed, both
/// outputs piped and its variables set.
fn process(command: &Command<'_>, building: &Path) -> io::Result<std::process::Command> {
    let (program, arguments) = command
        .words
        .split_first()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "a step with no program"))?;
    let mut process = std::process::Command::new(program);
    process
        .args(arguments)
        .current_dir(command.folder)
        .envs(command.env)
        .env("FETCHLOOM_BUILDING", building)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    group::configure(&mut process);
    Ok(process)
}

/// waits for `child` to end, handing on its lines meanwhile, and ends it when `stop` asks. it
/// looks often at first, so a short step costs little waiting, then every 200 ms.
fn watch(
    child: &mut Child,
    lines: &Mutex<Lines>,
    forward: Option<&dyn Fn(&str)>,
    stop: &Stop,
) -> io::Result<Ended> {
    let mut pause = Duration::from_millis(1);
    loop {
        hand_on(lines, forward);
        if let Some(status) = child.try_wait()? {
            return Ok(ended(status));
        }
        if stop.is_asked() {
            group::end(child);
            child.wait()?;
            return Ok(Ended::Stopped);
        }
        std::thread::sleep(pause);
        pause = (pause * 2).min(Duration::from_millis(200));
    }
}

fn ended(status: ExitStatus) -> Ended {
    #[cfg(unix)]
    if let Some(signal) = std::os::unix::process::ExitStatusExt::signal(&status) {
        return Ended::Signaled(signal);
    }
    Ended::Exited(status.code().unwrap_or(-1))
}

/// the lines read from a step that wait to reach the terminal from the thread that watches it.
struct Lines {
    waiting: Vec<String>,
    /// lines dropped since the last were handed on, since too many waited.
    skipped: usize,
    /// whether lines are kept at all: not when nothing shows them.
    kept: bool,
}

/// hands every waiting line to `forward`, and says how many were skipped.
fn hand_on(lines: &Mutex<Lines>, forward: Option<&dyn Fn(&str)>) {
    let Some(forward) = forward else {
        return;
    };
    let (waiting, skipped) = {
        let mut lines = lines.lock().unwrap_or_else(PoisonError::into_inner);
        (
            std::mem::take(&mut lines.waiting),
            std::mem::take(&mut lines.skipped),
        )
    };
    if skipped > 0 {
        forward(&format!(
            "… {} more, which the log keeps",
            crate::units::count(skipped as u64, "line", "lines")
        ));
    }
    for line in waiting {
        forward(&line);
    }
}

/// what both outputs of a step gave so far.
struct Caught {
    log: Log,
    tail: VecDeque<u8>,
    /// the first failure to write the log, which fails the build.
    failed: Option<io::Error>,
}

/// reads `stream` until it closes, into the log and the tail, and its lines into `lines`.
fn read(mut stream: Box<dyn Read + Send>, caught: &Mutex<Caught>, lines: &Mutex<Lines>) {
    let mut block = vec![0; 64 << 10];
    let mut partial: Vec<u8> = Vec::new();
    let keep = |line: &[u8]| {
        let mut lines = lines.lock().unwrap_or_else(PoisonError::into_inner);
        if !lines.kept {
            return;
        }
        if lines.waiting.len() < WAITING_LINES {
            lines.waiting.push(shown(line));
        } else {
            lines.skipped += 1;
        }
    };
    loop {
        let read = match stream.read(&mut block) {
            Ok(0) => break,
            Ok(read) => read,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        let bytes = &block[..read];
        {
            let mut caught = caught.lock().unwrap_or_else(PoisonError::into_inner);
            if caught.failed.is_none()
                && let Err(err) = caught.log.write(bytes)
            {
                caught.failed = Some(err);
            }
            caught.tail.extend(bytes);
            let over = caught.tail.len().saturating_sub(TAIL_BYTES);
            caught.tail.drain(..over);
        }
        partial.extend_from_slice(bytes);
        while let Some(end) = partial.iter().position(|byte| *byte == b'\n') {
            keep(&partial[..end]);
            partial.drain(..=end);
        }
        while partial.len() > LINE_BYTES {
            keep(&partial[..LINE_BYTES]);
            partial.drain(..LINE_BYTES);
        }
    }
    if !partial.is_empty() {
        keep(&partial);
    }
}

/// a line of output as text, without the carriage return windows ends lines with.
fn shown(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes.strip_suffix(b"\r").unwrap_or(bytes)).into_owned()
}

/// the last [`TAIL_LINES`] lines of `tail`.
fn tail(tail: &VecDeque<u8>) -> Vec<String> {
    let bytes: Vec<u8> = tail.iter().copied().collect();
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<&str> = text.lines().collect();
    let first = lines.len().saturating_sub(TAIL_LINES);
    lines[first..]
        .iter()
        .map(|line| line.trim_end_matches('\r').to_owned())
        .collect()
}

/// a step's log: a file only its owner may read, made anew for each run, that keeps the latest
/// [`LOG_MOST`] bytes of output.
struct Log {
    file: File,
    written: u64,
}

impl Log {
    /// a new, empty log at `path`. whatever was at `path` goes first, a link as itself, so the
    /// log never writes through a link someone left there.
    fn create(path: &Path) -> io::Result<Log> {
        match fs::remove_file(path) {
            Err(err) if err.kind() != io::ErrorKind::NotFound => return Err(err),
            _ => {}
        }
        let mut options = fs::OpenOptions::new();
        options.read(true).write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        Ok(Log {
            file: options.open(path)?,
            written: 0,
        })
    }

    /// appends `bytes`, first dropping the older half of the log when they would take it past
    /// [`LOG_MOST`].
    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        if self.written + bytes.len() as u64 > LOG_MOST {
            self.keep_latest(LOG_MOST / 2)?;
        }
        self.file.write_all(bytes)?;
        self.written += bytes.len() as u64;
        Ok(())
    }

    /// moves the last `keep` bytes of the log to its start and cuts it there.
    fn keep_latest(&mut self, keep: u64) -> io::Result<()> {
        let start = self.written.saturating_sub(keep);
        self.file.seek(SeekFrom::Start(start))?;
        let mut latest = Vec::new();
        (&self.file).take(keep).read_to_end(&mut latest)?;
        self.file.seek(SeekFrom::Start(0))?;
        self.file.write_all(&latest)?;
        self.written = latest.len() as u64;
        self.file.set_len(self.written)?;
        Ok(())
    }
}

#[cfg(unix)]
mod group {
    use std::process::Child;
    use std::sync::{Mutex, PoisonError};

    use rustix::process::{Pid, Signal, kill_process_group};

    /// the process groups of the steps running now, which a second ctrl-c ends before fetchloom
    /// quits.
    static RUNNING: Mutex<Vec<u32>> = Mutex::new(Vec::new());

    /// a step's group, known to [`end_running`] while this lives.
    pub(super) struct Running(u32);

    impl Running {
        pub(super) fn new(child: &Child) -> Running {
            RUNNING
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(child.id());
            Running(child.id())
        }
    }

    impl Drop for Running {
        fn drop(&mut self) {
            RUNNING
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .retain(|group| *group != self.0);
        }
    }

    /// starts the step in a process group of its own, named by its process id.
    pub(super) fn configure(process: &mut std::process::Command) {
        std::os::unix::process::CommandExt::process_group(process, 0);
    }

    /// ends `child`'s whole group, whatever is left of it.
    pub(super) fn end(child: &mut Child) {
        kill(child.id());
    }

    fn kill(group: u32) {
        // group 1 would signal every process this user may signal
        if let Some(pid) = i32::try_from(group).ok().and_then(Pid::from_raw)
            && pid != Pid::INIT
            && let Err(err) = kill_process_group(pid, Signal::KILL)
        {
            tracing::debug!(%err, group, "the step's group had ended");
        }
    }

    /// ends the group of every step running now.
    pub(crate) fn end_running() {
        for group in RUNNING
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
        {
            kill(*group);
        }
    }
}

#[cfg(windows)]
mod group {
    use std::process::Child;

    /// a step running now. windows ends only its own process for now, and the console's ctrl-c
    /// reaches it as it reaches fetchloom.
    pub(super) struct Running;

    impl Running {
        pub(super) fn new(_child: &Child) -> Running {
            Running
        }
    }

    pub(super) fn configure(_process: &mut std::process::Command) {}

    /// ends `child` itself, which is all std can end without a job object.
    pub(super) fn end(child: &mut Child) {
        if let Err(err) = child.kill() {
            tracing::debug!(%err, "the step had ended");
        }
    }

    pub(crate) fn end_running() {}
}

pub(crate) use group::end_running;

#[cfg(test)]
mod tests {
    use super::*;

    /// a folder with spaces stays inside its one argument, and the step gets its variables and
    /// the project it is built in.
    #[test]
    fn templates_expand_to_single_arguments() {
        let run: Vec<Template> = ["tool", "--in={raw}", "{out}", "{{literal}}"]
            .iter()
            .map(|word| Template::parse(word).unwrap())
            .collect();
        let folder = |slot: Slot<'_>| match slot {
            Slot::Input(name) => OsString::from(format!("/a build/in/{name}")),
            Slot::Out => OsString::from("/a build/out"),
        };
        let env = BTreeMap::from([("SEED".to_owned(), "1".to_owned())]);
        let command = Command {
            words: words(&run, &folder),
            folder: Path::new("/project"),
            env: &env,
        };
        let process = process(&command, Path::new("/project")).unwrap();
        assert_eq!(process.get_program(), "tool");
        assert_eq!(
            process.get_args().collect::<Vec<_>>(),
            ["--in=/a build/in/raw", "/a build/out", "{literal}"]
        );
        assert_eq!(process.get_current_dir(), Some(Path::new("/project")));
        let envs: Vec<_> = process.get_envs().collect();
        for (name, value) in [("FETCHLOOM_BUILDING", "/project"), ("SEED", "1")] {
            assert!(
                envs.contains(&(name.as_ref(), Some(value.as_ref()))),
                "{name}"
            );
        }
    }

    /// the log keeps the latest output once a step writes more than it holds, and the tail its
    /// last lines.
    #[test]
    fn the_log_keeps_the_latest_output() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("step.log");
        let mut log = Log::create(&path).unwrap();
        let block = vec![b'a'; 1 << 20];
        for _ in 0..(LOG_MOST >> 20) {
            log.write(&block).unwrap();
        }
        log.write(b"the end\n").unwrap();
        let kept = fs::read(&path).unwrap();
        assert!(kept.len() as u64 <= LOG_MOST);
        assert!(kept.ends_with(b"the end\n"));
        let lines: VecDeque<u8> = (1..=30)
            .flat_map(|line| format!("line {line}\r\n").into_bytes())
            .collect();
        let shown = tail(&lines);
        assert_eq!(shown.len(), TAIL_LINES);
        assert_eq!(shown.first().map(String::as_str), Some("line 11"));
        assert_eq!(shown.last().map(String::as_str), Some("line 30"));
    }
}
