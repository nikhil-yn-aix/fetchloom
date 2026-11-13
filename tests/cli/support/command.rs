//! a command a test runs, which the harness ends once it runs past [`DEADLINE`], failing the test
//! with what the command wrote to stderr and the end of its debug log, so a stall shows where it
//! waited rather than leaving nextest to end the test with nothing kept.

use std::ffi::OsStr;
use std::fmt::Write as _;
use std::io::{self, Read as _, Seek, SeekFrom, Write as _};
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// how long a command may run: well below the 180 s after which nextest ends a test.
pub(crate) const DEADLINE: Duration = Duration::from_secs(120);

/// the lines of the debug log a stall shows.
const TAIL: usize = 40;

/// a command the harness runs: fetchloom, or a program that runs it, such as `sh`. its methods
/// are [`Command`]'s, so a test sets it up as one, and every way it runs keeps the deadline.
pub(crate) struct Fetchloom {
    pub(super) command: Command,
    /// the file the command writes its debug log to, through `FETCHLOOM_TEST_LOG`.
    pub(super) log: Option<PathBuf>,
    deadline: Duration,
    set: Streams,
}

/// which of the standard streams the test chose.
#[derive(Default)]
struct Streams {
    stdin: bool,
    stdout: bool,
    stderr: bool,
}

impl Fetchloom {
    /// `command`, which writes its debug log to `log`.
    pub(crate) fn new(command: Command, log: Option<PathBuf>) -> Fetchloom {
        Fetchloom {
            command,
            log,
            deadline: DEADLINE,
            set: Streams::default(),
        }
    }

    /// another command, `make` builds from this one, such as a shell that runs it, keeping its
    /// debug log and its deadline. its streams are its own.
    #[cfg(unix)]
    pub(crate) fn through(&self, make: impl FnOnce(&Command) -> Command) -> Fetchloom {
        Fetchloom {
            command: make(&self.command),
            log: self.log.clone(),
            deadline: self.deadline,
            set: Streams::default(),
        }
    }

    /// the command itself, for a setting this type does not offer, such as how windows creates
    /// the process. it still runs through this type.
    #[cfg(windows)]
    pub(crate) fn command_mut(&mut self) -> &mut Command {
        &mut self.command
    }

    /// ends the command after `deadline` in place of [`DEADLINE`].
    pub(crate) fn deadline(&mut self, deadline: Duration) -> &mut Self {
        self.deadline = deadline;
        self
    }

    pub(crate) fn arg(&mut self, arg: impl AsRef<OsStr>) -> &mut Self {
        self.command.arg(arg);
        self
    }

    pub(crate) fn args<I, S>(&mut self, args: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.command.args(args);
        self
    }

    pub(crate) fn env(&mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> &mut Self {
        self.command.env(key, value);
        self
    }

    pub(crate) fn envs<I, K, V>(&mut self, vars: I) -> &mut Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        self.command.envs(vars);
        self
    }

    pub(crate) fn env_remove(&mut self, key: impl AsRef<OsStr>) -> &mut Self {
        self.command.env_remove(key);
        self
    }

    pub(crate) fn current_dir(&mut self, folder: impl AsRef<Path>) -> &mut Self {
        self.command.current_dir(folder);
        self
    }

    pub(crate) fn stdin(&mut self, stdin: impl Into<Stdio>) -> &mut Self {
        self.command.stdin(stdin);
        self.set.stdin = true;
        self
    }

    pub(crate) fn stdout(&mut self, stdout: impl Into<Stdio>) -> &mut Self {
        self.command.stdout(stdout);
        self.set.stdout = true;
        self
    }

    pub(crate) fn stderr(&mut self, stderr: impl Into<Stdio>) -> &mut Self {
        self.command.stderr(stderr);
        self.set.stderr = true;
        self
    }

    pub(crate) fn get_envs(&self) -> std::process::CommandEnvs<'_> {
        self.command.get_envs()
    }

    /// runs the command as [`Command::output`] does, stdin empty unless the test chose one.
    ///
    /// # Panics
    ///
    /// once the command runs past its deadline, after ending it, with what it wrote to stderr
    /// and the end of its debug log.
    pub(crate) fn output(&mut self) -> io::Result<Output> {
        self.output_with(None)
    }

    /// [`Fetchloom::output`] with `input` on stdin, as a person types it.
    #[expect(clippy::panic, reason = "a stall fails the test that ran the command")]
    pub(crate) fn output_with(&mut self, input: Option<&[u8]>) -> io::Result<Output> {
        if !self.set.stdin {
            self.command.stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            });
        }
        if !self.set.stdout {
            self.command.stdout(Stdio::piped());
        }
        if !self.set.stderr {
            self.command.stderr(Stdio::piped());
        }
        let mut child = self.command.spawn()?;
        let started = Instant::now();
        if let (Some(input), Some(mut stdin)) = (input, child.stdin.take()) {
            let input = input.to_vec();
            // written beside the wait, so a command that never reads it cannot hold the test
            std::thread::spawn(move || stdin.write_all(&input));
        }
        let ended = Ended {
            line: self.line(),
            log: self.log.clone(),
            deadline: self.deadline,
            started,
        };
        match ended.collect(child) {
            Ok(output) => Ok(output),
            Err(stalled) => panic!("{stalled}"),
        }
    }

    /// starts the command as [`Command::spawn`] does. waiting for it keeps the deadline.
    pub(crate) fn spawn(&mut self) -> io::Result<Spawned> {
        let child = self.command.spawn()?;
        Ok(Spawned {
            child: Some(child),
            ended: Ended {
                line: self.line(),
                log: self.log.clone(),
                deadline: self.deadline,
                started: Instant::now(),
            },
        })
    }

    /// the command line, as a stall names it.
    fn line(&self) -> String {
        std::iter::once(self.command.get_program())
            .chain(self.command.get_args())
            .map(OsStr::to_string_lossy)
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// a command a test started, whose wait keeps the deadline. it is the child otherwise, so a test
/// takes its streams and signals it as before.
pub(crate) struct Spawned {
    child: Option<Child>,
    ended: Ended,
}

impl Deref for Spawned {
    type Target = Child;

    fn deref(&self) -> &Child {
        self.child
            .as_ref()
            .unwrap_or_else(|| unreachable!("only a wait takes the child"))
    }
}

impl DerefMut for Spawned {
    fn deref_mut(&mut self) -> &mut Child {
        self.child
            .as_mut()
            .unwrap_or_else(|| unreachable!("only a wait takes the child"))
    }
}

impl Spawned {
    /// waits for the command to end, as [`Child::wait`] does.
    ///
    /// # Panics
    ///
    /// once the command runs past its deadline, after ending it, with the end of its debug log.
    #[expect(clippy::panic, reason = "a stall fails the test that ran the command")]
    pub(crate) fn wait(&mut self) -> io::Result<ExitStatus> {
        let mut pause = Duration::from_millis(1);
        loop {
            if let Some(status) = self.try_wait()? {
                return Ok(status);
            }
            if self.ended.started.elapsed() >= self.ended.deadline {
                let child = self.child.take().unwrap_or_else(|| unreachable!());
                panic!("{}", self.ended.stalled(child, None));
            }
            std::thread::sleep(pause);
            pause = (pause * 2).min(Duration::from_millis(20));
        }
    }

    /// waits for the command to end and close its output, as [`Child::wait_with_output`] does.
    ///
    /// # Panics
    ///
    /// as [`Spawned::wait`].
    #[expect(clippy::panic, reason = "a stall fails the test that ran the command")]
    #[expect(
        clippy::unnecessary_wraps,
        reason = "it stands in for `Child::wait_with_output`, which tests end with `?`"
    )]
    pub(crate) fn wait_with_output(mut self) -> io::Result<Output> {
        let child = self.child.take().unwrap_or_else(|| unreachable!());
        match self.ended.collect(child) {
            Ok(output) => Ok(output),
            Err(stalled) => panic!("{stalled}"),
        }
    }
}

/// what ending a command needs: what to call it, its debug log, and when it must end by.
struct Ended {
    line: String,
    log: Option<PathBuf>,
    deadline: Duration,
    started: Instant,
}

impl Ended {
    /// the output of `child` once it ends and closes its output, or the account of a command
    /// that ran past the deadline, which this ends.
    fn collect(&self, mut child: Child) -> Result<Output, String> {
        let (closed, closing) = mpsc::channel();
        let stdout = Stream::read(child.stdout.take(), closed.clone());
        let stderr = Stream::read(child.stderr.take(), closed);
        let left = || self.deadline.saturating_sub(self.started.elapsed());
        for _ in 0..stdout.open + stderr.open {
            match closing.recv_timeout(left()) {
                Ok(()) | Err(RecvTimeoutError::Disconnected) => {}
                Err(RecvTimeoutError::Timeout) => {
                    return Err(self.stalled(child, Some(&stderr.bytes())));
                }
            }
        }
        // a command that closed its output has ended, or is about to
        loop {
            let waited = child.try_wait();
            let waited =
                waited.map_err(|error| format!("waiting for `{}` failed: {error}", self.line))?;
            if let Some(status) = waited {
                return Ok(Output {
                    status,
                    stdout: stdout.bytes(),
                    stderr: stderr.bytes(),
                });
            }
            if left().is_zero() {
                return Err(self.stalled(child, Some(&stderr.bytes())));
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// ends `child`, which ran past the deadline, and says so with what it wrote to stderr, when
    /// the harness read it, and the end of its debug log.
    fn stalled(&self, mut child: Child, stderr: Option<&[u8]>) -> String {
        let _ = child.kill();
        // a process stuck in the kernel may outlive its kill, so the test does not wait on it
        let gone = Instant::now() + Duration::from_secs(5);
        while matches!(child.try_wait(), Ok(None)) && Instant::now() < gone {
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut said = format!(
            "`{}` ran past its deadline of {} s, so the harness ended it\n",
            self.line,
            self.deadline.as_secs_f64()
        );
        match stderr {
            Some(stderr) => {
                let _ = writeln!(
                    said,
                    "its stderr so far:\n{}",
                    String::from_utf8_lossy(stderr)
                );
            }
            None => said.push_str("its stderr went to the test\n"),
        }
        match &self.log {
            Some(log) => {
                let _ = write!(
                    said,
                    "the end of its debug log, {}:\n{}",
                    log.display(),
                    tail(log)
                );
            }
            None => said.push_str("it kept no debug log"),
        }
        said
    }
}

/// an output stream of a command, read on a thread as it comes, so what it said before a stall is
/// there to show.
struct Stream {
    bytes: Arc<Mutex<Vec<u8>>>,
    /// 1 for a stream the harness reads, 0 for one the test took or never piped.
    open: usize,
}

impl Stream {
    /// reads `stream`, saying on `closed` once it ends.
    fn read(stream: Option<impl io::Read + Send + 'static>, closed: mpsc::Sender<()>) -> Stream {
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let Some(mut stream) = stream else {
            return Stream { bytes, open: 0 };
        };
        let kept = Arc::clone(&bytes);
        std::thread::spawn(move || {
            let mut chunk = [0; 8192];
            while let Ok(read @ 1..) = stream.read(&mut chunk) {
                kept.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .extend_from_slice(&chunk[..read]);
            }
            let _ = closed.send(());
        });
        Stream { bytes, open: 1 }
    }

    fn bytes(&self) -> Vec<u8> {
        self.bytes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// the last [`TAIL`] lines of the debug log at `path`.
pub(super) fn tail(path: &Path) -> String {
    let mut text = Vec::new();
    let read = std::fs::File::open(path).and_then(|mut file| {
        let length = file.metadata()?.len();
        file.seek(SeekFrom::Start(length.saturating_sub(64 * 1024)))?;
        file.read_to_end(&mut text)
    });
    if let Err(error) = read {
        return format!("(unread: {error})");
    }
    let text = String::from_utf8_lossy(&text);
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        return "(empty)".to_owned();
    }
    lines[lines.len().saturating_sub(TAIL)..].join("\n")
}
