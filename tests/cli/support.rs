//! what every end to end test needs: a clean folder, a private cache and home, and a server that
//! answers with exactly the bytes a test scripts.

use std::fmt::Write as _;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};

mod command;
#[path = "../support/links.rs"]
mod links;
#[path = "../support/paths.rs"]
mod paths;
#[path = "../support/ports.rs"]
mod ports;
mod programs;
mod running;
mod server;
// marked as test code, which clippy lets print
#[cfg(test)]
#[path = "../support/skips.rs"]
mod skips;

pub(crate) use command::Fetchloom;
pub(crate) use links::link_folder;
pub(crate) use paths::workspace;
pub(crate) use ports::Refusing;
pub(crate) use programs::{executable, recipe_step};
pub(crate) use running::Running;

use server::{Connection, ServerTask, Shutdown};

pub(crate) type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

/// ends a test this machine cannot run, saying why, as [`skips::skip`] does.
#[expect(
    clippy::unnecessary_wraps,
    reason = "a test returns its skip as its result"
)]
pub(crate) fn skipped(capability: &str, reason: &str) -> Result {
    skips::skip(capability, reason);
    Ok(())
}

/// a working folder, a cache and a home of its own, so no test sees another's state or the
/// machine's.
pub(crate) struct Sandbox {
    /// the folder as fetchloom sees it. unix resolves links in a working folder, as getcwd does,
    /// so a temporary folder reached through a link shows by its real path.
    root: PathBuf,
    /// the origins of servers on this machine that data.toml, data.lock or a list of links may
    /// name, as a person lists them in `allowed_origins`.
    allowed: Mutex<Vec<String>>,
    /// how many commands the sandbox has made, which numbers their debug logs.
    commands: AtomicUsize,
    /// removes the folder when the sandbox goes.
    _folder: tempfile::TempDir,
}

/// which links the sandbox's filesystem makes, so a test expects what fetchloom can do here
/// rather than what one filesystem does.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Links {
    pub(crate) reflink: bool,
    pub(crate) hardlink: bool,
}

impl Links {
    /// whether fetchloom places a read-only file as a reflink here: where the drive clones,
    /// except on windows, which tries a hardlink first, since it reuses a file already scanned.
    pub(crate) fn reflinks_read_only(self) -> bool {
        self.reflink && !(cfg!(windows) && self.hardlink)
    }

    /// whether fetchloom places a read-only file as a hardlink here.
    pub(crate) fn hardlinks_read_only(self) -> bool {
        self.hardlink && !self.reflinks_read_only()
    }
}

impl Sandbox {
    pub(crate) fn new() -> Result<Sandbox> {
        let folder = tempfile::tempdir()?;
        let root = if cfg!(unix) {
            std::fs::canonicalize(folder.path())?
        } else {
            folder.path().to_path_buf()
        };
        for name in ["work", "home", "logs"] {
            std::fs::create_dir(root.join(name))?;
        }
        Ok(Sandbox {
            root,
            allowed: Mutex::default(),
            commands: AtomicUsize::new(0),
            _folder: folder,
        })
    }

    /// lets every later command reach the server `url` is on when data.toml, data.lock or a list
    /// of links names it, through the `allowed_origins` setting.
    pub(crate) fn allow(&self, url: &str) {
        let origin = url.splitn(4, '/').take(3).collect::<Vec<_>>().join("/");
        let mut allowed = self
            .allowed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !allowed.contains(&origin) {
            allowed.push(origin);
        }
    }

    pub(crate) fn work(&self) -> PathBuf {
        self.root.join("work")
    }

    pub(crate) fn cache(&self) -> PathBuf {
        self.root.join("cache")
    }

    /// another folder in the sandbox, such as a second clone of a project.
    pub(crate) fn folder(&self, name: &str) -> Result<PathBuf> {
        let path = self.root.join(name);
        std::fs::create_dir_all(&path)?;
        Ok(path)
    }

    /// tries each link fetchloom places with, between two files in the sandbox.
    pub(crate) fn links(&self) -> Result<Links> {
        let probe = self.folder(".links")?;
        let original = probe.join("original");
        std::fs::write(&original, b"links")?;
        let links = Links {
            reflink: reflink_copy::reflink(&original, probe.join("reflink")).is_ok(),
            hardlink: std::fs::hard_link(&original, probe.join("hardlink")).is_ok(),
        };
        std::fs::remove_dir_all(&probe)?;
        Ok(links)
    }

    /// [`Sandbox::filters`] for `story`, which tells how files were placed: where the drive links
    /// nothing, its copy wording, which must be there, reads as linking, so a snapshot holds both.
    pub(crate) fn placed_filters(&self, story: &str) -> Result<insta::Settings> {
        let mut settings = self.filters();
        let links = self.links()?;
        if !links.reflink && !links.hardlink {
            if !story.contains("cannot link files") {
                return Err(
                    format!("this drive cannot link files, yet nothing says so:\n{story}").into(),
                );
            }
            settings.add_filter(
                r" · copied, \d+ B\n  hint: the drive holding \S+ cannot link files, so the file was copied",
                " · linked to the cache",
            );
            settings.add_filter(
                r"warning: the drive holding \S+ cannot link files, so files were copied\n",
                "",
            );
            settings.add_filter(
                r"(Placed \S+ · \d+ files?) copied, \d+ B",
                "${1} linked to the cache",
            );
        }
        Ok(settings)
    }

    /// waits until a file written now would carry a later time than `time`: filesystems stamp
    /// times in steps, two seconds on FAT32, so the next file written can carry the same time.
    pub(crate) fn wait_for_a_later_time(&self, time: SystemTime) -> Result {
        let probe = self.root.join(".clock");
        let started = Instant::now();
        // the probe is written over in place with bytes. exFAT need only stamp a write that
        // changes a file's bytes or length, and on windows a file made again under a name just
        // deleted gets the old file's creation time back.
        let passed = loop {
            std::fs::write(&probe, b"tick")?;
            if std::fs::metadata(&probe)?.modified()? > time {
                break true;
            }
            if started.elapsed() > Duration::from_secs(10) {
                break false;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        std::fs::remove_file(&probe)?;
        if passed {
            Ok(())
        } else {
            Err("the filesystem's clock never passed the time".into())
        }
    }

    /// the command, run in `folder` instead of the work folder.
    pub(crate) fn command_in(&self, folder: &std::path::Path, args: &[&str]) -> Fetchloom {
        let mut command = self.command(args);
        command.current_dir(folder);
        command
    }

    /// fetchloom with `args`, in the work folder, with nothing of this machine's and its debug
    /// log in a file of its own, which a stall shows the end of.
    pub(crate) fn command(&self, args: &[&str]) -> Fetchloom {
        let home = self.root.join("home");
        let log = self.root.join("logs").join(format!(
            "{}.log",
            self.commands.fetch_add(1, Ordering::Relaxed)
        ));
        let mut command = Command::new(executable());
        // nothing from the machine running the tests may reach fetchloom: its own settings,
        // proxies, colors, synced folders or any source's credentials
        for (name, _) in std::env::vars_os() {
            if leaks(&name.to_string_lossy()) {
                command.env_remove(name);
            }
        }
        command
            .args(args)
            .current_dir(self.work())
            .env("FETCHLOOM_CACHE", self.cache())
            .env("CLICOLOR", "0")
            .env("LC_ALL", "C.UTF-8")
            .env("TERM", "xterm")
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .env("APPDATA", &home)
            .env("LOCALAPPDATA", &home)
            // a test never reads or writes the credential store of the machine running it
            .env("FETCHLOOM_KEYRING", "0")
            .env("FETCHLOOM_TEST_TICKER", "off")
            // a build's floor would follow the free space of the disk the tests run on; a test
            // of the floor sets its own
            .env("FETCHLOOM_BUILD_FLOOR", "1")
            .env("AWS_CONFIG_FILE", self.root.join("no-aws-config"))
            .env(
                "AWS_SHARED_CREDENTIALS_FILE",
                self.root.join("no-aws-credentials"),
            )
            .env("FETCHLOOM_TEST_LOG", &log);
        let allowed = self
            .allowed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !allowed.is_empty() {
            command.env("FETCHLOOM_ALLOWED_ORIGINS", allowed.join(","));
        }
        Fetchloom::new(command, Some(log))
    }

    pub(crate) fn run(&self, args: &[&str]) -> Result<Output> {
        Ok(self.command(args).output()?)
    }

    /// what a person at a terminal sees from the command: stdout and stderr both on a new pseudo
    /// terminal that `script` opens, read back as text. none when `script` is not installed.
    #[cfg(target_os = "linux")]
    pub(crate) fn on_a_terminal(&self, args: &[&str]) -> Result<Option<String>> {
        self.on_a_terminal_running(&self.command(args))
    }

    /// [`Sandbox::on_a_terminal`] for a command built elsewhere, with its arguments and settings.
    #[cfg(target_os = "linux")]
    pub(crate) fn on_a_terminal_running(&self, inner: &Fetchloom) -> Result<Option<String>> {
        let mut script = inner.through(|inner| {
            let line: Vec<String> = std::iter::once(inner.get_program())
                .chain(inner.get_args())
                .map(|word| format!("'{}'", word.to_string_lossy().replace('\'', r"'\''")))
                .collect();
            let mut script = Command::new("script");
            script
                .args(["-qec", &line.join(" "), "/dev/null"])
                .current_dir(self.work());
            for (name, value) in inner.get_envs() {
                match value {
                    Some(value) => script.env(name, value),
                    None => script.env_remove(name),
                };
            }
            script
                .env("SHELL", "/bin/sh")
                .env_remove("ENV")
                .env_remove("BASH_ENV");
            script
        });
        match script.output() {
            Ok(output) if !output.status.success() => Err(format!(
                "terminal command exited with {}: {}{}",
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
            )
            .into()),
            Ok(output) => Ok(Some(
                // a return to the start of a fresh line, as each line written there begins
                // with, moves nothing
                String::from_utf8_lossy(&output.stdout)
                    .replace("\r\n", "\n")
                    .replace("\n\r", "\n")
                    .trim_start_matches('\r')
                    .to_owned(),
            )),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                skipped("script", "no script, so no terminal to run on")?;
                Ok(None)
            }
            Err(err) => Err(err.into()),
        }
    }

    /// snapshot settings that hide what changes between runs: this sandbox's path, ports, timings,
    /// rates, which a slow run alone shows, and backslashes, which become slashes for windows.
    pub(crate) fn filters(&self) -> insta::Settings {
        let mut settings = insta::Settings::clone_current();
        settings.add_filter(&literal(&self.root.display().to_string()), "[sandbox]");
        settings.add_filter(r"\\", "/");
        settings.add_filter(r"127\.0\.0\.1:\d+", "127.0.0.1:[port]");
        settings.add_filter(r" · \d+(\.\d)? [KMGT]?i?B/s", "");
        settings.add_filter(r"\d+ min \d+ s|\d+(\.\d)? m?s\b", "[time]");
        settings
    }
}

/// insta settings for one command's help: the options every command takes show as `[global
/// options]`, so changing one of them changes `basics::help_lists_every_command` alone.
pub(crate) fn command_help() -> insta::Settings {
    let mut settings = insta::Settings::clone_current();
    settings.add_filter(
        r"(?ms)^  -q, --quiet .*?^  -h, --help +print help\b[^\n]*$",
        "  [global options]",
    );
    settings
}

/// what a person at a terminal sees from `inner` in `folder` under `script`, with its exit code,
/// typing `keys` once `after` shows; none without `script`. `stty` runs first, as `cols 40` or
/// `-isig` for a ctrl-c a question reads, and keys typed before it are read the old way.
#[cfg(target_os = "linux")]
pub(crate) fn typed(
    folder: &Path,
    inner: &Fetchloom,
    stty: &str,
    after: &str,
    keys: &str,
) -> Result<Option<(Option<i32>, String)>> {
    let mut script = inner.through(|inner| {
        let mut line: Vec<String> = std::iter::once(inner.get_program())
            .chain(inner.get_args())
            .map(|word| format!("'{}'", word.to_string_lossy().replace('\'', r"'\''")))
            .collect();
        if !stty.is_empty() {
            line.insert(0, format!("stty {stty} &&"));
        }
        let mut script = Command::new("script");
        script
            .args(["-qec", &line.join(" "), "/dev/null"])
            .current_dir(folder);
        for (name, value) in inner.get_envs() {
            match value {
                Some(value) => script.env(name, value),
                None => script.env_remove(name),
            };
        }
        script
            .env("SHELL", "/bin/sh")
            .env_remove("ENV")
            .env_remove("BASH_ENV");
        script
    });
    script
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    let mut child = match script.spawn() {
        Ok(child) => child,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            skipped("script", "no script, so no terminal to run on")?;
            return Ok(None);
        }
        Err(err) => return Err(err.into()),
    };
    // what the screen showed, and whether the terminal closed, as it does once the command ends
    let shown = Arc::new(Watched::<(Vec<u8>, bool)>::default());
    let mut stdout = child.stdout.take().ok_or("script has no stdout")?;
    let reader = {
        let shown = Arc::clone(&shown);
        std::thread::spawn(move || {
            let mut bytes = [0; 4096];
            while let Ok(read @ 1..) = stdout.read(&mut bytes) {
                shown.change(|(shown, _)| shown.extend_from_slice(&bytes[..read]));
            }
            shown.change(|(_, closed)| *closed = true);
        })
    };
    let screen =
        |(bytes, _): &(Vec<u8>, bool)| String::from_utf8_lossy(bytes).replace("\r\n", "\n");
    // a command that ended can show nothing more, so its waiting ends there too
    if !after.is_empty()
        && let Err(err) = shown.wait(&format!("{after:?} on the screen"), |state| {
            screen(state).contains(after) || state.1
        })
    {
        let _ = child.kill();
        let _ = child.wait();
        return Err(format!("{err}\n{}", shown.read(screen)).into());
    }
    if !after.is_empty() && !shown.read(|state| screen(state).contains(after)) {
        let _ = child.wait();
        return Err(format!(
            "the command ended before {after:?} showed\n{}",
            shown.read(screen)
        )
        .into());
    }
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(keys.as_bytes())?;
    }
    // a key the command misread would leave it waiting for another, and the test with it
    if let Err(err) = shown.wait("the command to end after the keys", |(_, closed)| *closed) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(format!("{err}\n{}", shown.read(screen)).into());
    }
    let status = child.wait()?;
    reader.join().map_err(|_| "reading the screen failed")?;
    Ok(Some((status.code(), shown.read(screen))))
}

pub(crate) fn text(bytes: &[u8]) -> Result<String> {
    Ok(String::from_utf8(bytes.to_vec())?)
}

/// fails the test unless `output` exited 0, showing its stderr.
pub(crate) fn ok(output: &Output) -> Result {
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    Ok(())
}

/// the SHA-256 of `bytes` in hex, as data.lock and export write it.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest as _, Sha256};
    use std::fmt::Write as _;
    let mut text = String::with_capacity(64);
    for byte in Sha256::digest(bytes) {
        let _ = write!(text, "{byte:02x}");
    }
    text
}

/// a zip of `entries`, each a path and its bytes, with the options most tools write.
pub(crate) fn zip_of(entries: &[(&str, &[u8])]) -> Result<Vec<u8>> {
    let mut archive = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (path, bytes) in entries {
        archive.start_file(*path, zip::write::SimpleFileOptions::default())?;
        archive.write_all(bytes)?;
    }
    Ok(archive.finish()?.into_inner())
}

pub(crate) fn json(output: &Output) -> Result<serde_json::Value> {
    Ok(serde_json::from_slice(&output.stdout)?)
}

/// each warning of a `--json` document as one sentence, its help after a semicolon.
pub(crate) fn warned(document: &serde_json::Value) -> Vec<String> {
    document["warnings"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|warning| {
            let message = warning["message"].as_str().unwrap_or_default();
            match warning["help"].as_str() {
                Some(help) => format!("{message}; {help}"),
                None => message.to_owned(),
            }
        })
        .collect()
}

/// writes over a file as a user would, clearing its read-only flag first.
pub(crate) fn edit(path: &Path, bytes: &[u8]) -> Result {
    let mut permissions = std::fs::metadata(path)?.permissions();
    #[expect(
        clippy::permissions_set_readonly_false,
        reason = "the test edits a read-only file as a user would"
    )]
    permissions.set_readonly(false);
    std::fs::set_permissions(path, permissions)?;
    std::fs::write(path, bytes)?;
    Ok(())
}

/// the files of the unfinished downloads in `cache`, in the folders inside `partial/` that spread
/// the downloads.
pub(crate) fn partial_files(cache: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(cache.join("partial"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|shard| shard.file_type().is_ok_and(|kind| kind.is_dir()))
        .flat_map(|shard| {
            std::fs::read_dir(shard.path())
                .into_iter()
                .flatten()
                .flatten()
        })
        .map(|entry| entry.path())
        .collect();
    files.sort();
    files
}

/// `stderr` without the warning a drive that cannot link adds, as ci's fat32 and exfat passes
/// do, so a test can compare the rest whole.
pub(crate) fn without_copy_warning(stderr: &str) -> String {
    stderr
        .split_inclusive('\n')
        .filter(|line| {
            !(line.starts_with("warning: the drive holding ")
                && line.ends_with(" cannot link files, so files were copied\n"))
        })
        .collect()
}

/// deletes the file at `path` as a person can, clearing its read-only flag only where removal
/// needs that, as on windows' FAT32 and exFAT: a placed hardlink shares the flag with the cache.
pub(crate) fn delete(path: &Path) -> Result {
    match std::fs::remove_file(path) {
        Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
            let mut permissions = std::fs::metadata(path)?.permissions();
            #[expect(
                clippy::permissions_set_readonly_false,
                reason = "the file is deleted next"
            )]
            permissions.set_readonly(false);
            std::fs::set_permissions(path, permissions)?;
            Ok(std::fs::remove_file(path)?)
        }
        removed => Ok(removed?),
    }
}

/// deletes the folder at `path` with all it holds as a person can, each file through [`delete`].
pub(crate) fn delete_folder(path: &Path) -> Result {
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            delete_folder(&entry.path())?;
        } else {
            delete(&entry.path())?;
        }
    }
    Ok(std::fs::remove_dir(path)?)
}

/// whether a variable of the machine running the tests could change what fetchloom does.
fn leaks(name: &str) -> bool {
    let name = name.to_ascii_uppercase();
    name.starts_with("FETCHLOOM_")
        || name.starts_with("ONEDRIVE")
        || name.starts_with("KAGGLE_")
        || name.starts_with("HF_")
        || name.starts_with("DATAVERSE_")
        || name.starts_with("AWS_")
        || name.starts_with("GOOGLE_")
        || name.starts_with("CLOUDSDK_")
        || name.starts_with("AZURE_")
        || name.ends_with("_PROXY")
        || name.ends_with("_TOKEN")
        || matches!(
            name.as_str(),
            "NO_COLOR"
                | "CLICOLOR_FORCE"
                | "XDG_CACHE_HOME"
                | "XDG_CONFIG_HOME"
                | "NETRC"
                // cargo sets both for what it runs, from where openssl keeps its certificates
                | "SSL_CERT_FILE"
                | "SSL_CERT_DIR"
                | "COLORTERM"
                | "COLORFGBG"
                // what the terminal running the tests says about itself, so colors and symbols
                // match what CI sees
                | "WT_SESSION"
                | "TERM_PROGRAM"
                | "CONEMUANSI"
                | "VTE_VERSION"
                | "KONSOLE_VERSION"
                // set by CI services and by coding agents running the tests, which turn
                // questions off even at a terminal
                | "CI"
                | "AI_AGENT"
                | "VSCODE_AGENT"
                | "CLAUDECODE"
                | "CODEX_SANDBOX"
                | "CODEX_THREAD_ID"
                | "GEMINI_CLI"
                | "CURSOR_AGENT"
                | "OPENCODE"
                | "AUGMENT_AGENT"
                | "ANTIGRAVITY_AGENT"
        )
}

/// a regex that matches `text` exactly.
fn literal(text: &str) -> String {
    let mut pattern = String::new();
    for c in text.chars() {
        if r"\.+*?()|[]{}^$#&-~".contains(c) {
            pattern.push('\\');
        }
        pattern.push(c);
    }
    pattern
}

/// a piece of a scripted response.
pub(crate) enum Part {
    Bytes(Vec<u8>),
    Pause(Duration),
    Action(Box<dyn FnOnce() + Send>),
    Wait(std::sync::mpsc::Receiver<()>),
}

/// a server on this machine that answers each new connection with the next scripted response,
/// byte for byte, then hangs up, so it can stop halfway through a body as real servers do.
pub(crate) struct Server {
    pub(crate) url: String,
    heads: Arc<Watched<Vec<String>>>,
    _running: ServerTask,
}

impl Server {
    pub(crate) fn start(path: &str, responses: Vec<Vec<Part>>) -> Result<Server> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let url = format!("http://{}{path}", listener.local_addr()?);
        let heads = Arc::new(Watched::<Vec<String>>::default());
        let seen = heads.clone();
        let running = ServerTask::start(listener, move |listener, stop| {
            for response in responses {
                let Some((mut stream, _connection, head)) = asking(&listener, &stop) else {
                    return;
                };
                seen.change(|heads| heads.push(head));
                for part in response {
                    if stop.stopped() {
                        return;
                    }
                    match part {
                        Part::Bytes(bytes) => {
                            let _ = stream.write_all(&bytes);
                            let _ = stream.flush();
                        }
                        Part::Pause(pause) => {
                            if stop.pause(pause) {
                                return;
                            }
                        }
                        Part::Action(action) => action(),
                        Part::Wait(release) => {
                            while matches!(
                                release.recv_timeout(Duration::from_millis(10)),
                                Err(std::sync::mpsc::RecvTimeoutError::Timeout)
                            ) {
                                if stop.stopped() {
                                    return;
                                }
                            }
                        }
                    }
                }
            }
        })?;
        Ok(Server {
            url,
            heads,
            _running: running,
        })
    }

    /// serves `body` with a strong `ETag` and `Accept-Ranges: bytes`: slowly on the first
    /// connection, then at full speed and from the asked range on the second.
    pub(crate) fn slow_file(path: &str, body: Vec<u8>) -> Result<Server> {
        Server::file_twice(path, body, false)
    }

    /// [`Server::slow_file`], except that the first connection holds back its last 64 KiB until
    /// its client hangs up, so a stop lands inside the body however busy the machine is. a download
    /// puts its first bytes on disk at its first checkpoint, a second in, so a test waiting for
    /// them waits that long.
    #[cfg(unix)]
    pub(crate) fn held_file(path: &str, body: Vec<u8>) -> Result<Server> {
        Server::file_twice(path, body, true)
    }

    /// [`Server::slow_file`], or with `hold` [`Server::held_file`].
    fn file_twice(path: &str, body: Vec<u8>, hold: bool) -> Result<Server> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let url = format!("http://{}{path}", listener.local_addr()?);
        let heads = Arc::new(Watched::<Vec<String>>::default());
        let seen = heads.clone();
        let running = ServerTask::start(listener, move |listener, stop| {
            for slow in [true, false] {
                let Some((mut stream, _connection)) = stop.accept(&listener) else {
                    return;
                };
                let asked = read_head(&mut stream).to_ascii_lowercase();
                let start = asked.lines().find_map(|line| {
                    let start = line.strip_prefix("range: bytes=")?.strip_suffix('-')?;
                    start.parse::<usize>().ok()
                });
                seen.change(|heads| heads.push(asked));
                let rest = &body[start.unwrap_or_default()..];
                let length = rest.len().to_string();
                let mut headers = vec![
                    ("Content-Length", length.as_str()),
                    ("ETag", "\"v1\""),
                    ("Accept-Ranges", "bytes"),
                ];
                let range =
                    start.map(|start| format!("bytes {start}-{}/{}", body.len() - 1, body.len()));
                let status = match &range {
                    Some(range) => {
                        headers.push(("Content-Range", range));
                        "206 Partial Content"
                    }
                    None => "200 OK",
                };
                let _ = stream.write_all(&head(status, &headers));
                let mut chunks = rest.chunks(64 * 1024).peekable();
                while let Some(chunk) = chunks.next() {
                    if slow && hold && chunks.peek().is_none() {
                        // a read ends when the client hangs up or the server stops
                        let mut probe = [0; 1];
                        while stream.read(&mut probe).is_ok_and(|read| read > 0) {}
                        break;
                    }
                    if stream.write_all(chunk).is_err() {
                        break;
                    }
                    if slow && stop.pause(Duration::from_millis(100)) {
                        return;
                    }
                }
            }
        })?;
        Ok(Server {
            url,
            heads,
            _running: running,
        })
    }

    /// the head of every request so far, lowercased.
    pub(crate) fn requests(&self) -> Vec<String> {
        self.heads.read(Vec::clone)
    }

    /// waits until the server has taken `count` requests in all.
    pub(crate) fn wait_for_requests(&self, count: usize) -> Result {
        self.heads
            .wait(&format!("{count} requests"), |heads| heads.len() >= count)
    }
}

/// a server on this machine for one large file, to any number of connections at once, with a
/// strong `ETag`, ranges and `If-Range`, pacing each answer as loss on a far link would.
pub(crate) struct Ranged {
    pub(crate) url: String,
    site: Arc<Site>,
    _running: ServerTask,
}

/// what every connection to a [`Ranged`] server shares.
struct Site {
    body: Vec<u8>,
    /// bytes a second for each answer, or none for full speed.
    rate: AtomicU64,
    requests: Mutex<Vec<String>>,
    active: AtomicUsize,
    most: AtomicUsize,
}

impl Ranged {
    /// serves `body` at `path`, each answer at `rate` bytes a second, or at full speed for 0.
    pub(crate) fn start(path: &str, body: Vec<u8>, rate: u64) -> Result<Ranged> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let url = format!("http://{}{path}", listener.local_addr()?);
        let site = Arc::new(Site {
            body,
            rate: AtomicU64::new(rate),
            requests: Mutex::default(),
            active: AtomicUsize::new(0),
            most: AtomicUsize::new(0),
        });
        let shared = site.clone();
        let running = ServerTask::start(listener, move |listener, stop| {
            std::thread::scope(|scope| {
                while let Some((stream, connection)) = stop.accept(&listener) {
                    let site = shared.clone();
                    let stop = &stop;
                    scope.spawn(move || {
                        let _connection = connection;
                        site.answer(stream, stop);
                    });
                }
            });
        })?;
        Ok(Ranged {
            url,
            site,
            _running: running,
        })
    }

    /// paces the answers from now on to `rate` bytes a second, or none for 0.
    pub(crate) fn pace(&self, rate: u64) {
        self.site.rate.store(rate, Ordering::SeqCst);
    }

    /// the head of every request so far, lowercased.
    pub(crate) fn requests(&self) -> Vec<String> {
        self.site
            .requests
            .lock()
            .map(|requests| requests.clone())
            .unwrap_or_default()
    }

    /// the requests that asked for a range with an end, as the added streams of a split
    /// download do.
    pub(crate) fn parts(&self) -> Vec<String> {
        self.requests()
            .into_iter()
            .filter(|head| {
                head.lines()
                    .any(|line| line.starts_with("range: bytes=") && !line.ends_with('-'))
            })
            .collect()
    }

    /// the most answers the server sent at once.
    pub(crate) fn most(&self) -> usize {
        self.site.most.load(Ordering::SeqCst)
    }
}

impl Site {
    fn answer(&self, mut stream: TcpStream, stop: &Shutdown) {
        let request = read_head(&mut stream).to_ascii_lowercase();
        if let Ok(mut requests) = self.requests.lock() {
            requests.push(request.clone());
        }
        let now = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.most.fetch_max(now, Ordering::SeqCst);
        self.respond(&mut stream, &request, stop);
        self.active.fetch_sub(1, Ordering::SeqCst);
    }

    fn respond(&self, stream: &mut TcpStream, request: &str, stop: &Shutdown) {
        const ETAG: &str = "\"v1\"";
        let size = self.body.len();
        let current = request
            .lines()
            .find_map(|line| line.strip_prefix("if-range: "))
            .is_none_or(|tag| tag.trim() == ETAG);
        let asked = request
            .lines()
            .find_map(|line| line.strip_prefix("range: bytes="))
            .and_then(|range| {
                let (start, end) = range.trim().split_once('-')?;
                let start: usize = start.parse().ok()?;
                let end = match end {
                    "" => size,
                    end => end.parse::<usize>().ok()?.saturating_add(1).min(size),
                };
                (start < end).then_some((start, end))
            })
            .filter(|_| current);
        let (start, end) = asked.unwrap_or((0, size));
        let length = (end - start).to_string();
        let mut headers = vec![
            ("Content-Length", length.as_str()),
            ("ETag", ETAG),
            ("Accept-Ranges", "bytes"),
        ];
        let range = format!("bytes {start}-{}/{size}", end.saturating_sub(1));
        let status = if asked.is_some() {
            headers.push(("Content-Range", &range));
            "206 Partial Content"
        } else {
            "200 OK"
        };
        if stream.write_all(&head(status, &headers)).is_err() {
            return;
        }
        let begun = Instant::now();
        let mut sent: u64 = 0;
        for chunk in self.body[start..end].chunks(64 * 1024) {
            let rate = self.rate.load(Ordering::SeqCst);
            if let Some(nanos) = (sent * 1_000_000_000).checked_div(rate) {
                let due = begun + Duration::from_nanos(nanos);
                if stop.pause(due.saturating_duration_since(Instant::now())) {
                    return;
                }
            }
            if stream.write_all(chunk).is_err() {
                return;
            }
            sent += chunk.len() as u64;
        }
    }
}

/// a forward proxy on this machine. it tunnels `CONNECT` and passes a request in absolute form on
/// to its server, and keeps the first line of each request, lowercased.
pub(crate) struct Proxy {
    pub(crate) url: String,
    asked: Arc<Mutex<Vec<String>>>,
    _running: ServerTask,
}

impl Proxy {
    pub(crate) fn start() -> Result<Proxy> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let url = format!("http://{}", listener.local_addr()?);
        let asked = Arc::new(Mutex::new(Vec::new()));
        let seen = asked.clone();
        let running = ServerTask::start(listener, move |listener, stop| {
            std::thread::scope(|scope| {
                while let Some((client, connection)) = stop.accept(&listener) {
                    let seen = seen.clone();
                    let stop = &stop;
                    scope.spawn(move || {
                        let _connection = connection;
                        relay(client, &seen, stop);
                    });
                }
            });
        })?;
        Ok(Proxy {
            url,
            asked,
            _running: running,
        })
    }

    /// the first line of every request so far.
    pub(crate) fn asked(&self) -> Vec<String> {
        self.asked
            .lock()
            .map(|asked| asked.clone())
            .unwrap_or_default()
    }
}

/// passes one client's request on, then copies bytes both ways until a side hangs up. a
/// connection that asks nothing, as fetchloom's check whether a proxy is up, is no request.
fn relay(mut client: TcpStream, seen: &Mutex<Vec<String>>, stop: &Arc<Shutdown>) {
    let head = read_head(&mut client);
    if head.is_empty() {
        return;
    }
    let line = head.lines().next().unwrap_or_default().to_owned();
    if let Ok(mut seen) = seen.lock() {
        seen.push(line.to_ascii_lowercase());
    }
    let mut words = line.split(' ');
    let (method, target) = (
        words.next().unwrap_or_default(),
        words.next().unwrap_or_default(),
    );
    let (authority, forwarded) = if method == "CONNECT" {
        (target.to_owned(), None)
    } else {
        let rest = target.strip_prefix("http://").unwrap_or(target);
        let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
        (
            authority.to_owned(),
            Some(head.replacen(target, &format!("/{path}"), 1)),
        )
    };
    let Ok(mut server) = TcpStream::connect(&authority) else {
        let _ = client.write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\n\r\n");
        return;
    };
    let Some(_connection) = stop.track(&server) else {
        return;
    };
    let _ = match forwarded {
        Some(head) => server.write_all(head.as_bytes()),
        None => client.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n"),
    };
    if let (Ok(mut from_client), Ok(mut to_server)) = (client.try_clone(), server.try_clone()) {
        std::thread::scope(|scope| {
            scope.spawn(move || {
                let _ = std::io::copy(&mut from_client, &mut to_server);
                let _ = to_server.shutdown(std::net::Shutdown::Write);
            });
            let _ = std::io::copy(&mut server, &mut client);
            let _ = client.shutdown(std::net::Shutdown::Both);
            let _ = server.shutdown(std::net::Shutdown::Both);
        });
    }
}

/// a server on this machine that answers by request target, path and query, as a service with
/// many URLs does: the same answer every time for a target it knows, and 404 for any other.
pub(crate) struct Routes {
    pub(crate) url: String,
    requests: Arc<Mutex<Vec<String>>>,
    answers: Arc<Mutex<std::collections::HashMap<String, Vec<u8>>>>,
    _running: ServerTask,
}

impl Routes {
    /// serves each `(target, response)` pair, where a response is what [`reply`] or [`file`]
    /// builds.
    pub(crate) fn start(routes: Vec<(String, Vec<Part>)>) -> Result<Routes> {
        let answers = Arc::new(Mutex::new(std::collections::HashMap::new()));
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let url = format!("http://{}", listener.local_addr()?);
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = requests.clone();
        let known = answers.clone();
        let running = ServerTask::start(listener, move |listener, stop| {
            let missing = head("404 Not Found", &[("Content-Length", "0")]);
            while let Some((mut stream, _connection)) = stop.accept(&listener) {
                let asked = read_head(&mut stream);
                let target = asked
                    .lines()
                    .next()
                    .and_then(|line| line.split(' ').nth(1))
                    .unwrap_or_default()
                    .to_owned();
                if let Ok(mut seen) = seen.lock() {
                    seen.push(asked.clone());
                }
                let answer = known
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .get(&target)
                    .unwrap_or(&missing)
                    .clone();
                let _ = stream.write_all(&answer);
                let _ = stream.flush();
            }
        })?;
        let served = Routes {
            url,
            requests,
            answers,
            _running: running,
        };
        served.change(routes);
        Ok(served)
    }

    /// answers each `(target, response)` pair from now on, in place of any answer the target had,
    /// as a service does once its files change.
    pub(crate) fn change(&self, routes: Vec<(String, Vec<Part>)>) {
        let mut answers = self.answers.lock().unwrap_or_else(PoisonError::into_inner);
        for (target, parts) in routes {
            let mut bytes = Vec::new();
            for part in parts {
                if let Part::Bytes(part) = part {
                    bytes.extend(part);
                }
            }
            answers.insert(target, bytes);
        }
    }

    /// the head of every request so far, lowercased.
    pub(crate) fn requests(&self) -> Vec<String> {
        self.heads()
            .iter()
            .map(|head| head.to_ascii_lowercase())
            .collect()
    }

    /// the target of every request so far, as sent.
    pub(crate) fn targets(&self) -> Vec<String> {
        self.heads()
            .iter()
            .filter_map(|head| head.lines().next()?.split(' ').nth(1).map(str::to_owned))
            .collect()
    }

    fn heads(&self) -> Vec<String> {
        self.requests
            .lock()
            .map(|requests| requests.clone())
            .unwrap_or_default()
    }
}

/// the next connection that asks for something, with its request head, lowercased; one that
/// closes unasked, as fetchloom's check whether a proxy is up, takes no scripted response.
fn asking(listener: &TcpListener, stop: &Arc<Shutdown>) -> Option<(TcpStream, Connection, String)> {
    loop {
        let (mut stream, connection) = stop.accept(listener)?;
        let head = read_head(&mut stream).to_ascii_lowercase();
        if !head.is_empty() {
            return Some((stream, connection, head));
        }
    }
}

fn read_head(stream: &mut TcpStream) -> String {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        match stream.read(&mut byte) {
            Ok(1) => head.push(byte[0]),
            _ => break,
        }
    }
    String::from_utf8_lossy(&head).into_owned()
}

/// a response: a status line, headers, then exactly `body`, even when it falls short of the
/// length the headers promise.
pub(crate) fn reply(status: &str, headers: &[(&str, &str)], body: &[u8]) -> Vec<Part> {
    let mut bytes = head(status, headers);
    bytes.extend_from_slice(body);
    vec![Part::Bytes(bytes)]
}

/// a status line and headers, ready for a body.
fn head(status: &str, headers: &[(&str, &str)]) -> Vec<u8> {
    let mut head = format!("HTTP/1.1 {status}\r\nConnection: close\r\n");
    for (name, value) in headers {
        let _ = write!(head, "{name}: {value}\r\n");
    }
    head.push_str("\r\n");
    head.into_bytes()
}

/// a whole file with a strong `ETag`.
pub(crate) fn file(body: &[u8], etag: &str) -> Vec<Part> {
    reply(
        "200 OK",
        &[("Content-Length", &body.len().to_string()), ("ETag", etag)],
        body,
    )
}

/// a value that other threads change, and a signal at each change, so a test can wait for what
/// it needs without polling.
#[derive(Default)]
struct Watched<T> {
    value: Mutex<T>,
    changed: Condvar,
}

impl<T> Watched<T> {
    fn change(&self, change: impl FnOnce(&mut T)) {
        change(&mut self.value.lock().unwrap_or_else(PoisonError::into_inner));
        self.changed.notify_all();
    }

    fn read<R>(&self, read: impl FnOnce(&T) -> R) -> R {
        read(&self.value.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// returns once `ready` holds, checked again at each change. the deadline only keeps a test
    /// whose command never gets there from hanging.
    fn wait(&self, what: &str, ready: impl Fn(&T) -> bool) -> Result {
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut value = self.value.lock().unwrap_or_else(PoisonError::into_inner);
        while !ready(&value) {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(format!("timed out waiting for {what}").into());
            }
            value = self
                .changed
                .wait_timeout(value, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        Ok(())
    }
}

/// polls `ready` until it holds, for state nothing announces, such as bytes another process
/// writes; [`Server::wait_for_requests`] and [`Running::wait_for`] need no polling. the deadline
/// only keeps a test from hanging, and names what never happened.
pub(crate) fn wait_until(what: &str, mut ready: impl FnMut() -> Result<bool>) -> Result {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !ready()? {
        if Instant::now() >= deadline {
            return Err(format!("timed out waiting for {what}").into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

/// `command` run until `FETCHLOOM_TEST_HOLD_FINISH` holds it at its mark, so a stop sent now
/// lands past it, with the file whose writing releases it.
#[cfg(unix)]
pub(crate) fn held_at_its_mark(
    sandbox: &Sandbox,
    command: &mut Fetchloom,
) -> Result<(Running, PathBuf)> {
    let release = sandbox.folder("hold")?.join("release");
    command.env("FETCHLOOM_TEST_HOLD_FINISH", &release);
    let running = Running::start(command)?;
    let mut held = release.as_os_str().to_owned();
    held.push(".held");
    wait_until("the run to mark its work as finishing", || {
        Ok(Path::new(&held).exists())
    })?;
    Ok((running, release))
}

/// what a run past its mark says when a ctrl-c lands there: that it finishes, never that it
/// stops, and its exit code 0.
#[cfg(unix)]
pub(crate) fn finished_anyway(running: Running, release: &Path) -> Result<String> {
    running.interrupt()?;
    running.wait_for("Finishing")?;
    std::fs::write(release, b"")?;
    let output = running.finish()?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("  Finishing what it started (ctrl-c again to quit at once)\n"),
        "{stderr}"
    );
    assert!(
        !stderr.contains("Stopping") && !stderr.contains("Stopped"),
        "{stderr}"
    );
    Ok(stderr)
}

/// each server and the proxy runs in a `ServerTask`. the check uses the worker's own handle,
/// since a freed port may at once belong to a test running alongside.
#[test]
fn dropping_a_server_task_closes_the_listener_its_worker_owns() -> Result {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let owned = Arc::new(());
    let held = owned.clone();
    let task = ServerTask::start(listener, move |listener, stop| {
        while stop.accept(&listener).is_some() {}
        drop((listener, held));
    })?;
    assert_eq!(
        Arc::strong_count(&owned),
        2,
        "the worker holds its listener"
    );
    drop(task);
    assert_eq!(
        Arc::strong_count(&owned),
        1,
        "the worker ended before drop returned"
    );
    Ok(())
}
/// a connection to a `Refusing` address is refused at once on every system, not left to time out.
#[test]
fn a_refusing_address_refuses_each_connection() -> Result {
    let refusing = Refusing::new()?;
    let connected = TcpStream::connect_timeout(&refusing.address, Duration::from_secs(10));
    assert!(
        connected
            .as_ref()
            .is_err_and(|error| error.kind() == std::io::ErrorKind::ConnectionRefused),
        "{connected:?}"
    );
    Ok(())
}

#[test]
fn dropping_a_server_closes_a_connection_waiting_for_a_request() -> Result {
    let server = Server::start("/", vec![file(b"ok", "\"v1\"")])?;
    let address = server
        .url
        .trim_start_matches("http://")
        .trim_end_matches('/');
    let mut connection = TcpStream::connect(address)?;
    // macos refuses socket options once the peer has closed, so set it first
    connection.set_read_timeout(Some(Duration::from_secs(1)))?;
    drop(server);
    let closed = connection.read(&mut [0]);
    assert!(
        matches!(closed, Ok(0))
            || closed.is_err_and(|error| matches!(
                error.kind(),
                std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted
            ))
    );
    Ok(())
}

#[test]
fn dropping_a_server_interrupts_an_unreleased_response() -> Result {
    let (started, arrived) = std::sync::mpsc::channel();
    let (_release, held) = std::sync::mpsc::channel();
    let server = Server::start(
        "/",
        vec![vec![
            Part::Action(Box::new(move || {
                let _ = started.send(());
            })),
            Part::Wait(held),
        ]],
    )?;
    let address = server
        .url
        .trim_start_matches("http://")
        .trim_end_matches('/');
    let mut client = TcpStream::connect(address)?;
    client.write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")?;
    arrived.recv_timeout(Duration::from_secs(2))?;
    let (finished, stopped) = std::sync::mpsc::channel();
    let dropping = std::thread::spawn(move || {
        drop(server);
        let _ = finished.send(());
    });
    stopped.recv_timeout(Duration::from_secs(2))?;
    dropping.join().map_err(|_| "server shutdown panicked")?;
    client.set_read_timeout(Some(Duration::from_secs(1)))?;
    assert_eq!(client.read(&mut [0])?, 0);
    Ok(())
}

/// the shared AWS files a command sees are ones that do not exist.
#[test]
fn no_aws_setting_of_the_machine_reaches_a_command() -> Result {
    for name in [
        "AWS_ACCESS_KEY_ID",
        "AWS_SECRET_ACCESS_KEY",
        "AWS_PROFILE",
        "AWS_REGION",
        "AWS_ENDPOINT_URL_S3",
        "aws_config_file",
    ] {
        assert!(leaks(name), "{name}");
    }
    let sandbox = Sandbox::new()?;
    let command = sandbox.command(&["--version"]);
    for name in ["AWS_CONFIG_FILE", "AWS_SHARED_CREDENTIALS_FILE"] {
        let path = command
            .get_envs()
            .find(|(key, _)| *key == name)
            .and_then(|(_, value)| value)
            .ok_or(name)?;
        assert!(!Path::new(path).exists(), "{name}");
    }
    Ok(())
}

/// the command is ended, and the end of its debug log shows where it waited, here on a server
/// that never answers; a command a test spawned keeps the deadline while the test waits. each of
/// the two runs waits out its deadline of 2 s.
#[test]
fn a_command_past_its_deadline_fails_with_its_stderr_and_debug_log() -> Result {
    let sandbox = Sandbox::new()?;
    for started in [false, true] {
        let (_release, held) = std::sync::mpsc::channel();
        let server = Server::start("/held.csv", vec![vec![Part::Wait(held)]])?;
        let mut command = sandbox.command(&["-v", "get", &server.url]);
        command.deadline(Duration::from_secs(2));
        let began = Instant::now();
        let ended = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result {
            if started {
                let mut child = command.stdout(std::process::Stdio::null()).spawn()?;
                child.wait()?;
            } else {
                command.output()?;
            }
            Ok(())
        }));
        let said = match ended {
            Err(panic) => *panic
                .downcast::<String>()
                .map_err(|_| "a panic without words")?,
            Ok(ended) => return Err(format!("the command ended by itself: {ended:?}").into()),
        };
        assert!(began.elapsed() < Duration::from_secs(30), "{said}");
        assert!(
            said.contains(&format!(
                "-v get {}` ran past its deadline of 2 s, so the harness ended it\n",
                server.url
            )),
            "{said}"
        );
        let stderr = if started {
            "its stderr went to the test\n"
        } else {
            "its stderr so far:\n  Resources default: "
        };
        assert!(said.contains(stderr), "{said}");
        let (_, log) = said
            .split_once("the end of its debug log, ")
            .ok_or_else(|| format!("no debug log: {said}"))?;
        assert!(
            log.contains(&format!("download{{url={}}}", server.url)),
            "{said}"
        );
    }
    Ok(())
}

#[test]
fn commands_define_their_output_environment() -> Result {
    let sandbox = Sandbox::new()?;
    let command = sandbox.command(&["--version"]);
    for (name, value) in [("LC_ALL", "C.UTF-8"), ("TERM", "xterm"), ("CLICOLOR", "0")] {
        let actual = command
            .get_envs()
            .find(|(key, _)| *key == name)
            .and_then(|(_, value)| value);
        assert_eq!(actual, Some(std::ffi::OsStr::new(value)), "{name}");
    }
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn terminal_commands_report_failures() -> Result {
    let sandbox = Sandbox::new()?;
    match sandbox.on_a_terminal(&["--not-a-fetchloom-option"]) {
        Ok(None) | Err(_) => Ok(()),
        Ok(Some(screen)) => {
            Err(format!("a failed terminal command returned success: {screen}").into())
        }
    }
}

#[cfg(target_os = "linux")]
#[test]
fn terminal_commands_use_a_known_shell() -> Result {
    let sandbox = Sandbox::new()?;
    if let Some(screen) = sandbox.on_a_terminal(&["--version"])? {
        assert_eq!(
            screen.trim(),
            concat!("fetchloom ", env!("CARGO_PKG_VERSION"))
        );
    }
    Ok(())
}
