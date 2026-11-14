//! running one step, with no shell, in the project folder, its outputs in a log only its owner
//! may read. fetchloom ends the step's whole group when a stop is asked and once it ends, so
//! nothing it started still writes. a terminal's ctrl-c reaches that group on windows alone.

#[cfg(any(windows, test))]
mod program;

use std::collections::{BTreeMap, VecDeque};
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::process::{ExitStatus, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::project::manifest::recipes::{Slot, Template};
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

/// how often a running step's watch looks at the free space on the disk holding the cache.
const FLOOR_EVERY: Duration = Duration::from_secs(2);

pub(crate) struct Command<'a> {
    /// the program and its arguments, each folder in place.
    pub(crate) words: Vec<OsString>,
    /// the folder it runs in: the project's.
    pub(crate) folder: &'a Path,
    /// the variables data.toml sets for it, on top of fetchloom's own environment.
    pub(crate) env: &'a BTreeMap<String, String>,
}

/// the least free space a running step may leave on the cache's disk, and how to read it.
pub(crate) struct Floor<'a> {
    pub(crate) bytes: u64,
    /// the bytes free now, none when the system does not say.
    pub(crate) free: &'a dyn Fn() -> Option<u64>,
}

impl Floor<'_> {
    /// what the disk has free, when that is less than the floor.
    fn breached(&self) -> Option<u64> {
        (self.free)().filter(|free| *free < self.bytes)
    }
}

/// whether the watch looks at the free space at `now`: at once, then every [`FLOOR_EVERY`].
fn due(looked: Option<Instant>, now: Instant) -> bool {
    looked.is_none_or(|looked| now.saturating_duration_since(looked) >= FLOOR_EVERY)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Ended {
    /// it exited with this code.
    Exited(i32),
    /// a signal ended it, as when the system runs out of memory.
    #[cfg(unix)]
    Signaled(i32),
    /// a stop was asked, so fetchloom ended it.
    Stopped,
    /// the cache's disk had less free than the floor, `free` bytes, so fetchloom ended it.
    Low { free: u64 },
}

impl Ended {
    /// how a step that ended counts once `stopping`: a failure then likely came from the stop, as a
    /// windows ctrl-c or a scheduler's SIGTERM reaches the step too, while exit 0 keeps its result.
    fn or_stopped(self, stopping: bool) -> Ended {
        match self {
            Ended::Exited(code) if stopping && code != 0 => Ended::Stopped,
            #[cfg(unix)]
            Ended::Signaled(_) if stopping => Ended::Stopped,
            ended => ended,
        }
    }
}

#[derive(Debug)]
pub(crate) struct Ran {
    pub(crate) ended: Ended,
    /// its last lines of output, oldest first.
    pub(crate) tail: Vec<String>,
    /// whether every line of its output was handed on as it came, none skipped.
    pub(crate) told: bool,
    pub(crate) took: Duration,
}

/// the words of `run` as the step gets them, with each folder `folder` gives in place: one
/// argument each, spaces and all, since no shell splits them.
pub(crate) fn words(run: &[Template], folder: &dyn Fn(Slot<'_>) -> OsString) -> Vec<OsString> {
    run.iter().map(|word| word.expand(folder)).collect()
}

/// runs `command` until it ends, `stop` asks or the cache's disk falls below `floor`, its outputs
/// in a new log at `log`, each line handed to `forward`, and its group ended with it. the step gets
/// `building` as `FETCHLOOM_BUILDING`. it fails when the program cannot start, `NotFound` when none
/// of its name is on PATH, or the log cannot be written.
pub(crate) fn run(
    command: &Command<'_>,
    building: &Path,
    log: &Path,
    forward: Option<&dyn Fn(&str)>,
    stop: &Stop,
    floor: &Floor<'_>,
) -> io::Result<Ran> {
    let _span = tracing::debug_span!("step").entered();
    let log = Log::create(log)?;
    let process = process(command, building)?;
    let started = Instant::now();
    let mut group = Group::start(process)?;
    let caught = Arc::new(Mutex::new(Caught {
        log,
        tail: VecDeque::new(),
        failed: None,
    }));
    let lines = Arc::new(Mutex::new(Lines {
        waiting: Vec::new(),
        skipped: 0,
        dropped: false,
        kept: forward.is_some(),
    }));
    let (done, finished) = mpsc::channel();
    let mut readers = 0;
    for stream in group.outputs() {
        readers += 1;
        let (caught, lines, done) = (Arc::clone(&caught), Arc::clone(&lines), done.clone());
        std::thread::spawn(move || {
            read(stream, &caught, &lines);
            let _ = done.send(());
        });
    }
    let ended = watch(&mut group, &lines, forward, stop, floor);
    group.end();
    let ended = ended?;
    let deadline = Instant::now() + DRAIN;
    let drained = (0..readers).all(|_| {
        finished
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .is_ok()
    });
    if !drained {
        tracing::debug!("a process the step started holds its output open, so the rest is unread");
    }
    hand_on(&lines, forward);
    let dropped = lines.lock().unwrap_or_else(PoisonError::into_inner).dropped;
    let mut caught = caught.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(err) = caught.failed.take() {
        return Err(err);
    }
    Ok(Ran {
        ended: ended.or_stopped(stop.is_asked()),
        tail: tail(&caught.tail),
        told: forward.is_some() && drained && !dropped,
        took: started.elapsed(),
    })
}

/// the process for `command`, with stdin closed and both outputs piped.
fn process(command: &Command<'_>, building: &Path) -> io::Result<std::process::Command> {
    let (program, arguments) = command
        .words
        .split_first()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "a step with no program"))?;
    #[cfg(windows)]
    let program = program::find(program, command.folder, command.env);
    let mut process = std::process::Command::new(program);
    process
        .args(arguments)
        .current_dir(command.folder)
        .envs(command.env)
        .env("FETCHLOOM_BUILDING", building)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    Ok(process)
}

/// waits for the step of `group` to end, handing on its lines, and ends the group when `stop`
/// asks or the disk falls below `floor`, looking often at first so a short step waits little.
fn watch(
    group: &mut Group,
    lines: &Mutex<Lines>,
    forward: Option<&dyn Fn(&str)>,
    stop: &Stop,
    floor: &Floor<'_>,
) -> io::Result<Ended> {
    let mut pause = Duration::from_millis(1);
    let mut looked = None;
    loop {
        hand_on(lines, forward);
        if let Some(status) = group.try_wait()? {
            return Ok(ended(status));
        }
        if stop.is_asked() {
            group.end();
            group.wait()?;
            return Ok(Ended::Stopped);
        }
        let now = Instant::now();
        if due(looked, now) {
            looked = Some(now);
            if let Some(free) = floor.breached() {
                group.end();
                group.wait()?;
                return Ok(Ended::Low { free });
            }
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
    /// whether any line was ever dropped.
    dropped: bool,
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
            lines.dropped = true;
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

    /// appends `bytes`, first dropping the log's older half when they would pass [`LOG_MOST`].
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
    use std::io::{self, Read};
    use std::process::{Child, Command, ExitStatus};
    use std::sync::{Mutex, PoisonError};

    use rustix::process::{Pid, Signal, kill_process_group};

    /// the process groups of running steps, which a second ctrl-c ends before fetchloom quits.
    static RUNNING: Mutex<Vec<u32>> = Mutex::new(Vec::new());

    /// a step's process in a group of its own, its id known to [`end_running`] while this lives.
    pub(super) struct Group(Child);

    impl Group {
        pub(super) fn start(mut process: Command) -> io::Result<Group> {
            std::os::unix::process::CommandExt::process_group(&mut process, 0);
            let child = process.spawn()?;
            RUNNING
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(child.id());
            Ok(Group(child))
        }

        /// the step's stdout and stderr, once.
        pub(super) fn outputs(&mut self) -> Vec<Box<dyn Read + Send>> {
            let stdout = self
                .0
                .stdout
                .take()
                .map(|out| Box::new(out) as Box<dyn Read + Send>);
            let stderr = self
                .0
                .stderr
                .take()
                .map(|out| Box::new(out) as Box<dyn Read + Send>);
            stdout.into_iter().chain(stderr).collect()
        }

        pub(super) fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
            self.0.try_wait()
        }

        pub(super) fn wait(&mut self) -> io::Result<ExitStatus> {
            self.0.wait()
        }

        /// ends the whole group, whatever is left of it.
        pub(super) fn end(&mut self) {
            kill(self.0.id());
        }
    }

    impl Drop for Group {
        fn drop(&mut self) {
            let id = self.0.id();
            RUNNING
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .retain(|group| *group != id);
        }
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
    use std::fs::File;
    use std::io::{self, Read};
    use std::process::{Command, ExitStatus};
    use std::time::Duration;

    use process_wrap::tokio::{ChildWrapper, CommandWrap, JobObject, KillOnDrop};

    /// a step's process in a job object that takes in every process the step starts, ending them
    /// all when told and when fetchloom ends, however it ends. process-wrap sets that up only
    /// through tokio, so the step is tokio's child, touched only by calls that need no runtime.
    pub(super) struct Group {
        child: Box<dyn ChildWrapper>,
        outputs: Vec<File>,
    }

    impl Group {
        pub(super) fn start(process: Command) -> io::Result<Group> {
            let mut command = CommandWrap::from(tokio::process::Command::from(process));
            command.wrap(KillOnDrop).wrap(JobObject);
            let mut child = command.spawn()?;
            let mut outputs = Vec::new();
            if let Some(stdout) = child.stdout().take() {
                outputs.push(File::from(stdout.into_owned_handle()?));
            }
            if let Some(stderr) = child.stderr().take() {
                outputs.push(File::from(stderr.into_owned_handle()?));
            }
            Ok(Group { child, outputs })
        }

        /// the step's stdout and stderr, once.
        pub(super) fn outputs(&mut self) -> Vec<Box<dyn Read + Send>> {
            self.outputs
                .drain(..)
                .map(|output| Box::new(output) as Box<dyn Read + Send>)
                .collect()
        }

        pub(super) fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
            self.child.try_wait()
        }

        /// waits for the step's own process to end, once [`Group::end`] ended its job.
        pub(super) fn wait(&mut self) -> io::Result<ExitStatus> {
            let mut pause = Duration::from_millis(1);
            loop {
                if let Some(status) = self.child.try_wait()? {
                    return Ok(status);
                }
                std::thread::sleep(pause);
                pause = (pause * 2).min(Duration::from_millis(50));
            }
        }

        pub(super) fn end(&mut self) {
            if let Err(err) = self.child.start_kill() {
                tracing::debug!(%err, "the step's job had ended");
            }
        }
    }

    /// leaves the jobs as they are: each ends with fetchloom's own process, which holds the last
    /// handle to it.
    pub(crate) fn end_running() {}
}

use group::Group;
pub(crate) use group::end_running;

#[cfg(test)]
mod tests {
    use super::*;

    /// the floor is checked at once, then every two seconds, and a disk that does not say what is
    /// free never stops a step.
    #[test]
    fn the_floor_check_stops_below_its_bytes() {
        let breached = |free: Option<u64>| {
            let free = move || free;
            Floor {
                bytes: 100,
                free: &free,
            }
            .breached()
        };
        assert_eq!(breached(Some(99)), Some(99));
        assert_eq!(breached(Some(100)), None);
        assert_eq!(breached(None), None);
        let start = Instant::now();
        assert!(due(None, start));
        assert!(!due(Some(start), start + Duration::from_millis(1999)));
        assert!(due(Some(start), start + FLOOR_EVERY));
    }

    /// a step that exits 0 after a stop keeps its result; a failure with no stop stays a failure.
    #[test]
    fn a_step_that_fails_after_a_stop_is_stopped() {
        assert_eq!(Ended::Exited(1).or_stopped(true), Ended::Stopped);
        assert_eq!(Ended::Exited(0).or_stopped(true), Ended::Exited(0));
        assert_eq!(Ended::Exited(1).or_stopped(false), Ended::Exited(1));
        let low = Ended::Low { free: 1 };
        assert_eq!(low.or_stopped(true), low);
        #[cfg(unix)]
        {
            assert_eq!(Ended::Signaled(15).or_stopped(true), Ended::Stopped);
            assert_eq!(Ended::Signaled(9).or_stopped(false), Ended::Signaled(9));
        }
    }

    /// a folder with spaces stays one argument, and the step gets its variables and project.
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

    /// once a step writes more than the log holds, the tail keeps the last lines.
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
