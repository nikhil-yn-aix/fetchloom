//! what every end to end test needs: a clean folder, a private cache and home, and a server that
//! answers with exactly the bytes a test scripts.

use std::fmt::Write as _;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub(crate) type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

/// a working folder, a cache and a home of its own, so no test sees another's state or the
/// machine's.
pub(crate) struct Sandbox {
    root: tempfile::TempDir,
}

impl Sandbox {
    pub(crate) fn new() -> Result<Sandbox> {
        let root = tempfile::tempdir()?;
        for folder in ["work", "home"] {
            std::fs::create_dir(root.path().join(folder))?;
        }
        Ok(Sandbox { root })
    }

    pub(crate) fn work(&self) -> PathBuf {
        self.root.path().join("work")
    }

    pub(crate) fn cache(&self) -> PathBuf {
        self.root.path().join("cache")
    }

    /// another folder in the sandbox, such as a second clone of a project.
    pub(crate) fn folder(&self, name: &str) -> Result<PathBuf> {
        let path = self.root.path().join(name);
        std::fs::create_dir_all(&path)?;
        Ok(path)
    }

    /// the command, run in `folder` instead of the work folder.
    pub(crate) fn command_in(&self, folder: &std::path::Path, args: &[&str]) -> Command {
        let mut command = self.command(args);
        command.current_dir(folder);
        command
    }

    pub(crate) fn command(&self, args: &[&str]) -> Command {
        let home = self.root.path().join("home");
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

    /// snapshot settings that hide what changes from run to run: this sandbox's path, ports and
    /// timings. a rate shows only once a download takes a second, which a busy machine or a
    /// retry wait can cause, so rates are dropped whole. backslashes become slashes so windows
    /// matches the other platforms.
    pub(crate) fn filters(&self) -> insta::Settings {
        let mut settings = insta::Settings::clone_current();
        settings.add_filter(
            &literal(&self.root.path().display().to_string()),
            "[sandbox]",
        );
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
