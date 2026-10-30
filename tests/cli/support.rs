//! what every end to end test needs: a clean folder, a private cache and home, and a server that
//! answers with exactly the bytes a test scripts.

use std::fmt::Write as _;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

pub(crate) type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

/// a working folder, a cache and a home of its own, so no test sees another's state or the
/// machine's.
pub(crate) struct Sandbox {
    /// the folder as fetchloom sees it. unix resolves links in a working folder, as getcwd does,
    /// so a temporary folder reached through a link shows by its real path.
    root: PathBuf,
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

impl Sandbox {
    pub(crate) fn new() -> Result<Sandbox> {
        let folder = tempfile::tempdir()?;
        let root = if cfg!(unix) {
            std::fs::canonicalize(folder.path())?
        } else {
            folder.path().to_path_buf()
        };
        for name in ["work", "home"] {
            std::fs::create_dir(root.join(name))?;
        }
        Ok(Sandbox {
            root,
            _folder: folder,
        })
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

    /// `name` for a snapshot that says how files were placed, or its variant for a drive that
    /// makes neither link, where every file is copied.
    pub(crate) fn placed_story(&self, name: &str) -> Result<String> {
        let links = self.links()?;
        Ok(if links.reflink || links.hardlink {
            name.to_owned()
        } else {
            format!("{name}_on_a_drive_without_links")
        })
    }

    /// waits until a file written in the sandbox now carries a later time than `time`. a
    /// filesystem stamps times in steps, two seconds on FAT32, so a file written right after
    /// another can carry the same time.
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
    pub(crate) fn command_in(&self, folder: &std::path::Path, args: &[&str]) -> Command {
        let mut command = self.command(args);
        command.current_dir(folder);
        command
    }

    pub(crate) fn command(&self, args: &[&str]) -> Command {
        let home = self.root.join("home");
        let mut command = Command::new(env!("CARGO_BIN_EXE_fetchloom"));
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
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .env("APPDATA", &home)
            .env("LOCALAPPDATA", &home);
        command
    }

    pub(crate) fn run(&self, args: &[&str]) -> Result<Output> {
        Ok(self.command(args).output()?)
    }

    /// what a person at a terminal sees from the command: stdout and stderr both on a new pseudo
    /// terminal that `script` opens, read back as text. none when `script` is not installed.
    #[cfg(target_os = "linux")]
    pub(crate) fn on_a_terminal(&self, args: &[&str]) -> Result<Option<String>> {
        let inner = self.command(args);
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
        match script.output() {
            Ok(output) => Ok(Some(
                String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n"),
            )),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err.into()),
        }
    }

    /// snapshot settings that hide what changes from run to run: this sandbox's path, ports and
    /// timings. a rate shows only once a download takes a second, which a busy machine or a
    /// retry wait can cause, so rates are dropped whole. backslashes become slashes so windows
    /// matches the other platforms.
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

pub(crate) fn text(bytes: &[u8]) -> Result<String> {
    Ok(String::from_utf8(bytes.to_vec())?)
}

/// deletes the file at `path` as a person can: where the filesystem refuses to remove a read-only
/// file, as FAT32 and exFAT do on windows, the flag is cleared first. elsewhere the flag is left
/// alone, since a placed hardlink shares it with the cached object.
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

/// makes `link` name the folder `target` the way a user can without admin: a junction on
/// windows, a symlink elsewhere. says whether the filesystem allowed it.
pub(crate) fn link_folder(target: &Path, link: &Path) -> bool {
    #[cfg(windows)]
    let made = Command::new("cmd")
        .args(["/c", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .output()
        .is_ok_and(|output| output.status.success());
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(target, link).is_ok();
    made
}

/// whether a variable of the machine running the tests could change what fetchloom does.
fn leaks(name: &str) -> bool {
    let name = name.to_ascii_uppercase();
    name.starts_with("FETCHLOOM_")
        || name.starts_with("ONEDRIVE")
        || name.starts_with("KAGGLE_")
        || name.starts_with("HF_")
        || name.ends_with("_PROXY")
        || name.ends_with("_TOKEN")
        || matches!(
            name.as_str(),
            "NO_COLOR" | "CLICOLOR_FORCE" | "XDG_CACHE_HOME" | "XDG_CONFIG_HOME"
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
}

/// a server on this machine that answers each new connection with the next scripted response,
/// byte for byte, then hangs up. it can do what real servers do and polite fakes cannot, such as
/// stopping halfway through a body.
pub(crate) struct Server {
    pub(crate) url: String,
    requests: Arc<Mutex<Vec<String>>>,
}

impl Server {
    pub(crate) fn start(path: &str, responses: Vec<Vec<Part>>) -> Result<Server> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let url = format!("http://{}{path}", listener.local_addr()?);
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = requests.clone();
        std::thread::spawn(move || {
            for response in responses {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let head = read_head(&mut stream);
                if let Ok(mut seen) = seen.lock() {
                    seen.push(head.to_ascii_lowercase());
                }
                for part in response {
                    match part {
                        Part::Bytes(bytes) => {
                            let _ = stream.write_all(&bytes);
                            let _ = stream.flush();
                        }
                        Part::Pause(pause) => std::thread::sleep(pause),
                    }
                }
            }
        });
        Ok(Server { url, requests })
    }

    /// serves `body` with a strong `ETag`: slowly on the first connection, then at full speed and
    /// from the asked range on the second.
    pub(crate) fn slow_file(path: &str, body: Vec<u8>) -> Result<Server> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let url = format!("http://{}{path}", listener.local_addr()?);
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = requests.clone();
        std::thread::spawn(move || {
            for slow in [true, false] {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let asked = read_head(&mut stream).to_ascii_lowercase();
                let start = asked.lines().find_map(|line| {
                    let start = line.strip_prefix("range: bytes=")?.strip_suffix('-')?;
                    start.parse::<usize>().ok()
                });
                if let Ok(mut seen) = seen.lock() {
                    seen.push(asked);
                }
                let rest = &body[start.unwrap_or_default()..];
                let length = rest.len().to_string();
                let mut headers = vec![("Content-Length", length.as_str()), ("ETag", "\"v1\"")];
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
                for chunk in rest.chunks(64 * 1024) {
                    if stream.write_all(chunk).is_err() {
                        break;
                    }
                    if slow {
                        std::thread::sleep(Duration::from_millis(100));
                    }
                }
            }
        });
        Ok(Server { url, requests })
    }

    /// the head of every request so far, lowercased.
    pub(crate) fn requests(&self) -> Vec<String> {
        self.requests
            .lock()
            .map(|requests| requests.clone())
            .unwrap_or_default()
    }
}

/// a server on this machine for one large file, as file servers send one: to any number of
/// connections at once, with a strong `ETag`, byte ranges and `If-Range`, and each answer paced
/// to a rate of its own, as when loss holds every connection of a far link to a few Mbit/s.
pub(crate) struct Ranged {
    pub(crate) url: String,
    site: Arc<Site>,
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
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let site = shared.clone();
                std::thread::spawn(move || site.answer(stream));
            }
        });
        Ok(Ranged { url, site })
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
    fn answer(&self, mut stream: TcpStream) {
        let request = read_head(&mut stream).to_ascii_lowercase();
        if let Ok(mut requests) = self.requests.lock() {
            requests.push(request.clone());
        }
        let now = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.most.fetch_max(now, Ordering::SeqCst);
        self.respond(&mut stream, &request);
        self.active.fetch_sub(1, Ordering::SeqCst);
    }

    fn respond(&self, stream: &mut TcpStream, request: &str) {
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
                std::thread::sleep(due.saturating_duration_since(Instant::now()));
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
}

impl Proxy {
    pub(crate) fn start() -> Result<Proxy> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let url = format!("http://{}", listener.local_addr()?);
        let asked = Arc::new(Mutex::new(Vec::new()));
        let seen = asked.clone();
        std::thread::spawn(move || {
            for client in listener.incoming().flatten() {
                let seen = seen.clone();
                std::thread::spawn(move || relay(client, &seen));
            }
        });
        Ok(Proxy { url, asked })
    }

    /// the first line of every request so far.
    pub(crate) fn asked(&self) -> Vec<String> {
        self.asked
            .lock()
            .map(|asked| asked.clone())
            .unwrap_or_default()
    }
}

/// passes one client's request on, then copies bytes both ways until a side hangs up.
fn relay(mut client: TcpStream, seen: &Mutex<Vec<String>>) {
    let head = read_head(&mut client);
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
    let _ = match forwarded {
        Some(head) => server.write_all(head.as_bytes()),
        None => client.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n"),
    };
    if let (Ok(mut from_client), Ok(mut to_server)) = (client.try_clone(), server.try_clone()) {
        std::thread::spawn(move || std::io::copy(&mut from_client, &mut to_server));
        let _ = std::io::copy(&mut server, &mut client);
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
