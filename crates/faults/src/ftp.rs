//! An FTP server that answers exactly what a test told it to answer.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

#[derive(Clone, Debug, Default)]
pub struct FtpScript {
    files: BTreeMap<String, Vec<u8>>,
    directories: Vec<String>,
    speaks_mlsd: bool,
    speaks_rest: bool,
    speaks_tls: bool,
    named_in_listing: BTreeMap<String, Vec<String>>,
    pasv_host: Option<[u8; 4]>,
    stated_size: BTreeMap<String, u64>,
}

impl FtpScript {
    #[must_use]
    pub fn serving(path: &str, bytes: Vec<u8>) -> Self {
        let mut script = Self {
            speaks_mlsd: true,
            speaks_rest: true,
            ..Self::default()
        };
        script.files.insert(trimmed(path).to_owned(), bytes);
        script
    }

    #[must_use]
    pub fn empty() -> Self {
        Self {
            speaks_mlsd: true,
            speaks_rest: true,
            ..Self::default()
        }
    }

    #[must_use]
    pub fn holding(mut self, path: &str, bytes: Vec<u8>) -> Self {
        self.files.insert(trimmed(path).to_owned(), bytes);
        self
    }

    #[must_use]
    pub fn with_directory(mut self, path: &str) -> Self {
        self.directories.push(trimmed(path).to_owned());
        self
    }

    #[must_use]
    pub fn refusing_mlsd(mut self) -> Self {
        self.speaks_mlsd = false;
        self
    }

    #[must_use]
    pub fn refusing_rest(mut self) -> Self {
        self.speaks_rest = false;
        self
    }

    #[must_use]
    pub fn offering_tls(mut self) -> Self {
        self.speaks_tls = true;
        self
    }

    #[must_use]
    pub fn naming_in_listing(mut self, directory: &str, names: Vec<String>) -> Self {
        self.named_in_listing
            .insert(trimmed(directory).to_owned(), names);
        self
    }

    #[must_use]
    pub fn advertising_data_host(mut self, quad: [u8; 4]) -> Self {
        self.pasv_host = Some(quad);
        self
    }

    #[must_use]
    pub fn stating_size(mut self, path: &str, size: u64) -> Self {
        self.stated_size.insert(trimmed(path).to_owned(), size);
        self
    }
}

fn trimmed(path: &str) -> &str {
    path.trim_start_matches('/')
}

#[derive(Debug)]
pub struct FtpTestServer {
    address: SocketAddr,
    spoken: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
}

impl FtpTestServer {
    /// # Errors
    /// Whatever binding a loopback port reports.
    pub fn start(script: FtpScript) -> std::io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let address = listener.local_addr()?;
        let spoken = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let served = Arc::new(script);
        let heard = Arc::clone(&spoken);
        let ending = Arc::clone(&stop);
        std::thread::spawn(move || {
            for connection in listener.incoming() {
                if ending.load(Ordering::Relaxed) {
                    return;
                }
                let Ok(connection) = connection else {
                    return;
                };
                let script = Arc::clone(&served);
                let heard = Arc::clone(&heard);
                std::thread::spawn(move || {
                    let _ = converse(connection, &script, &heard);
                });
            }
        });
        Ok(Self {
            address,
            spoken,
            stop,
        })
    }

    #[must_use]
    pub fn origin(&self) -> String {
        format!("ftp://{}", self.address)
    }

    #[must_use]
    pub fn secured_origin(&self) -> String {
        format!("ftps://{}", self.address)
    }

    #[must_use]
    pub fn spoken(&self) -> Vec<String> {
        self.spoken
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl Drop for FtpTestServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect(self.address);
    }
}

struct Pending {
    listener: TcpListener,
}

fn converse(
    control: TcpStream,
    script: &FtpScript,
    heard: &Arc<Mutex<Vec<String>>>,
) -> std::io::Result<()> {
    let mut writer = control.try_clone()?;
    let mut reader = BufReader::new(control);
    write!(writer, "220 fetchloom fault server ready\r\n")?;
    writer.flush()?;
    let mut pending: Option<Pending> = None;
    let mut offset: u64 = 0;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Ok(());
        }
        let line = line.trim_end_matches(['\r', '\n']).to_owned();
        if line.is_empty() {
            continue;
        }
        heard
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(line.clone());
        let (verb, argument) = match line.split_once(' ') {
            Some((verb, rest)) => (verb.to_ascii_uppercase(), rest.trim().to_owned()),
            None => (line.to_ascii_uppercase(), String::new()),
        };
        match verb.as_str() {
            "USER" => reply(&mut writer, "331 send a password")?,
            "PASS" => reply(&mut writer, "230 logged in")?,
            "AUTH" if script.speaks_tls => {
                reply(&mut writer, "234 proceed, though this server speaks no TLS")?;
                return Ok(());
            }
            "AUTH" => reply(&mut writer, "500 AUTH not understood")?,
            "TYPE" => reply(&mut writer, "200 type set")?,
            "FEAT" => features(&mut writer, script)?,
            "PWD" => reply(&mut writer, "257 \"/\" is the current directory")?,
            "SIZE" => match sized(script, &argument) {
                Some(size) => reply(&mut writer, &format!("213 {size}"))?,
                None => reply(&mut writer, "550 no such file")?,
            },
            "MDTM" => reply(&mut writer, "213 20200102030405")?,
            "REST" if !script.speaks_rest => reply(&mut writer, "502 REST not implemented")?,
            "REST" => {
                offset = argument.parse().unwrap_or(0);
                reply(&mut writer, "350 restarting")?;
            }
            "PASV" => {
                let listener = TcpListener::bind(("127.0.0.1", 0))?;
                let port = listener.local_addr()?.port();
                let quad = script.pasv_host.unwrap_or([127, 0, 0, 1]);
                reply(
                    &mut writer,
                    &format!(
                        "227 Entering Passive Mode ({},{},{},{},{},{})",
                        quad[0],
                        quad[1],
                        quad[2],
                        quad[3],
                        port / 256,
                        port % 256
                    ),
                )?;
                pending = Some(Pending { listener });
            }
            "RETR" => {
                let Some(bytes) = script.files.get(trimmed(&argument)) else {
                    reply(&mut writer, "550 no such file")?;
                    continue;
                };
                let start = usize::try_from(offset)
                    .unwrap_or(usize::MAX)
                    .min(bytes.len());
                offset = 0;
                send_data(&mut writer, &mut pending, &bytes[start..])?;
            }
            "MLSD" if !script.speaks_mlsd => {
                reply(&mut writer, "500 MLSD not understood")?;
            }
            "MLSD" => {
                let body = machine_listing(script, &argument);
                send_data(&mut writer, &mut pending, body.as_bytes())?;
            }
            "LIST" => {
                let body = human_listing(script, &argument);
                send_data(&mut writer, &mut pending, body.as_bytes())?;
            }
            "QUIT" => {
                reply(&mut writer, "221 goodbye")?;
                return Ok(());
            }
            _ => reply(&mut writer, "500 not understood")?,
        }
    }
}

fn reply(writer: &mut TcpStream, line: &str) -> std::io::Result<()> {
    write!(writer, "{line}\r\n")?;
    writer.flush()
}

fn features(writer: &mut TcpStream, script: &FtpScript) -> std::io::Result<()> {
    let mut text = String::from("211-Features:\r\n SIZE\r\n MDTM\r\n");
    if script.speaks_rest {
        text.push_str(" REST STREAM\r\n");
    }
    if script.speaks_mlsd {
        text.push_str(" MLST size*;type*;modify*;\r\n");
    }
    if script.speaks_tls {
        text.push_str(" AUTH TLS\r\n PBSZ\r\n PROT\r\n");
    }
    text.push_str("211 End\r\n");
    writer.write_all(text.as_bytes())?;
    writer.flush()
}

fn sized(script: &FtpScript, path: &str) -> Option<u64> {
    let path = trimmed(path);
    if let Some(stated) = script.stated_size.get(path) {
        return Some(*stated);
    }
    script.files.get(path).map(|bytes| bytes.len() as u64)
}

fn send_data(
    writer: &mut TcpStream,
    pending: &mut Option<Pending>,
    bytes: &[u8],
) -> std::io::Result<()> {
    let Some(waiting) = pending.take() else {
        return reply(writer, "425 use PASV first");
    };
    reply(writer, "150 opening data connection")?;
    let (mut data, _) = waiting.listener.accept()?;
    data.write_all(bytes)?;
    data.flush()?;
    drop(data);
    reply(writer, "226 transfer complete")
}

fn under(directory: &str, path: &str) -> Option<String> {
    let prefix = if directory.is_empty() {
        String::new()
    } else {
        format!("{directory}/")
    };
    path.strip_prefix(&prefix)?
        .split('/')
        .next()
        .map(str::to_owned)
}

fn members(script: &FtpScript, directory: &str) -> Vec<(String, Option<u64>)> {
    let directory = trimmed(directory).trim_end_matches('/');
    let mut found: BTreeMap<String, Option<u64>> = BTreeMap::new();
    for (path, bytes) in &script.files {
        let Some(head) = under(directory, path) else {
            continue;
        };
        let whole = if directory.is_empty() {
            head.clone()
        } else {
            format!("{directory}/{head}")
        };
        if whole == *path {
            found.insert(head, Some(bytes.len() as u64));
        } else {
            found.insert(head, None);
        }
    }
    for held in &script.directories {
        if let Some(head) = under(directory, held) {
            found.insert(head, None);
        }
    }
    found.into_iter().collect()
}

fn machine_listing(script: &FtpScript, directory: &str) -> String {
    if let Some(named) = script
        .named_in_listing
        .get(trimmed(directory).trim_end_matches('/'))
    {
        return named.iter().fold(String::new(), |mut held, name| {
            held.push_str("type=file;size=4; ");
            held.push_str(name);
            held.push_str("\r\n");
            held
        });
    }
    members(script, directory)
        .into_iter()
        .map(|(name, size)| match size {
            Some(size) => format!("type=file;size={size};modify=20200102030405; {name}\r\n"),
            None => format!("type=dir;modify=20200102030405; {name}\r\n"),
        })
        .collect()
}

fn human_listing(script: &FtpScript, directory: &str) -> String {
    if let Some(named) = script
        .named_in_listing
        .get(trimmed(directory).trim_end_matches('/'))
    {
        return named.iter().fold(String::new(), |mut held, name| {
            held.push_str("-rw-r--r--   1 ftp ftp            4 Jan  2  2020 ");
            held.push_str(name);
            held.push_str("\r\n");
            held
        });
    }
    members(script, directory)
        .into_iter()
        .map(|(name, size)| match size {
            Some(size) => {
                format!("-rw-r--r--   1 ftp ftp {size:>12} Jan  2  2020 {name}\r\n")
            }
            None => format!("drwxr-xr-x   2 ftp ftp         4096 Jan  2  2020 {name}\r\n"),
        })
        .collect()
}
