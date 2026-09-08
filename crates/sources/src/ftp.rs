//! FTP and FTPS, spoken here rather than taken as a dependency.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::{Duration, Instant};

use fetchloom_engine::credential::Credential;
use fetchloom_engine::degrade::{Degradation, DegradeQueue};
use fetchloom_engine::error::{Error, ErrorKind};
use fetchloom_engine::limits::Limits;
use fetchloom_engine::redact::SafeUrl;
use fetchloom_engine::reference::Host;
use fetchloom_engine::seam::source::{
    ByteRange, Cost, Listing, ListingEntry, Revalidated, Served, Serves, Source, SourceIdentity,
    SourceMetadata, Validator,
};
use fetchloom_engine::work::WorkCounter;

const PLAIN: &str = "ftp://";
const SECURED: &str = "ftps://";
const CONTROL_PORT: u16 = 21;
const ANONYMOUS: &str = "anonymous";
const ANONYMOUS_PASSWORD: &str = "anonymous@";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Securing {
    Required,
    Attempted,
}

struct Where {
    host: String,
    port: u16,
    path: String,
    securing: Securing,
}

pub enum Wire {
    Plain(TcpStream),
    Secured(Box<crate::tls::Secured>),
    Handed,
}

impl Read for Wire {
    fn read(&mut self, into: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Self::Plain(stream) => stream.read(into),
            Self::Secured(stream) => stream.read(into),
            Self::Handed => Ok(0),
        }
    }
}

impl Write for Wire {
    fn write(&mut self, from: &[u8]) -> std::io::Result<usize> {
        match self {
            Self::Plain(stream) => stream.write(from),
            Self::Secured(stream) => stream.write(from),
            Self::Handed => Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe)),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Self::Plain(stream) => stream.flush(),
            Self::Secured(stream) => stream.flush(),
            Self::Handed => Ok(()),
        }
    }
}

fn unreadable(reference: &str) -> Error {
    Error::new(
        ErrorKind::ReferenceUnresolved,
        format!(
            "write it as ftp://host/path or ftps://host/path, because {} names no host and path this build can reach",
            SafeUrl::new(reference)
        ),
    )
    .with_source(reference)
}

fn refused(location: &str, reason: &str) -> Error {
    Error::new(ErrorKind::NetworkRefused, reason.to_owned()).with_source(location)
}

fn socket_failure(location: &str, reason: &std::io::Error) -> Error {
    Error::new(
        ErrorKind::NetworkTimeout,
        format!(
            "try the source again, because the connection to {} stopped answering: {reason}",
            SafeUrl::new(location)
        ),
    )
    .with_source(location)
}

fn insecure(location: &str, said: &str) -> Error {
    Error::new(
        ErrorKind::NetworkTls,
        format!(
            "reach it over ftp:// only if the bytes may travel in the clear, because {} would not secure the control connection: {said}",
            SafeUrl::new(location)
        ),
    )
    .with_source(location)
}

fn absent(location: &str, said: &str) -> Error {
    Error::new(
        ErrorKind::ReferenceUnresolved,
        format!(
            "name a path the server holds, because {} answered {said}",
            SafeUrl::new(location)
        ),
    )
    .with_source(location)
}

fn outside(location: &str, named: &str) -> Error {
    Error::new(
        ErrorKind::ReferenceUnresolved,
        format!(
            "name a directory that lists its own members, because {} listed {} as a member, which is not one path under the directory",
            SafeUrl::new(location),
            SafeUrl::new(named)
        ),
    )
    .with_source(location)
}

fn parsed(reference: &str) -> Result<Where, Error> {
    let (rest, securing) = match (
        reference.strip_prefix(SECURED),
        reference.strip_prefix(PLAIN),
    ) {
        (Some(rest), _) => (rest, Securing::Required),
        (None, Some(rest)) => (rest, Securing::Attempted),
        (None, None) => return Err(unreadable(reference)),
    };
    let (authority, path) = match rest.split_once('/') {
        Some((authority, path)) => (authority, path),
        None => (rest, ""),
    };
    if authority.is_empty() || authority.contains('@') {
        return Err(unreadable(reference));
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => (host, port.parse().map_err(|_| unreadable(reference))?),
        None => (authority, CONTROL_PORT),
    };
    if host.is_empty() {
        return Err(unreadable(reference));
    }
    Ok(Where {
        host: host.to_owned(),
        port,
        path: path.to_owned(),
        securing,
    })
}

fn is_under_the_directory(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('/')
        && !name.contains('\\')
        && !name.contains('/')
        && name != "."
        && name != ".."
}

struct Reply {
    code: u16,
    text: String,
}

impl Reply {
    fn is(&self, first: u16) -> bool {
        self.code / 100 == first
    }
}

struct Control {
    reader: BufReader<Wire>,
    peer: IpAddr,
    host: String,
    location: String,
    features: Vec<String>,
    secured: bool,
}

impl Control {
    fn open(at: &Where, limits: &Limits, location: &str) -> Result<Self, Error> {
        let first = (at.host.as_str(), at.port)
            .to_socket_addrs()
            .map_err(|reason| {
                refused(
                    location,
                    &format!(
                        "name a host that resolves, because {} did not: {reason}",
                        at.host
                    ),
                )
            })?
            .next()
            .ok_or_else(|| {
                refused(
                    location,
                    &format!(
                        "name a host that resolves, because {} resolved to nothing",
                        at.host
                    ),
                )
            })?;
        let stream = TcpStream::connect_timeout(&first, limits.connect_timeout).map_err(
            |reason| {
                refused(
                    location,
                    &format!(
                        "try the source again, because the control connection to {} was refused: {reason}",
                        SafeUrl::new(location)
                    ),
                )
            },
        )?;
        stream
            .set_read_timeout(Some(limits.response_timeout))
            .map_err(|reason| socket_failure(location, &reason))?;
        stream
            .set_write_timeout(Some(limits.response_timeout))
            .map_err(|reason| socket_failure(location, &reason))?;
        let mut control = Self {
            reader: BufReader::new(Wire::Plain(stream)),
            peer: first.ip(),
            host: at.host.clone(),
            location: location.to_owned(),
            features: Vec::new(),
            secured: false,
        };
        let greeting = control.read_reply()?;
        if !greeting.is(2) {
            return Err(refused(
                location,
                &format!(
                    "try the source again, because {} answered the connection with {}",
                    SafeUrl::new(location),
                    greeting.code
                ),
            ));
        }
        Ok(control)
    }

    fn read_reply(&mut self) -> Result<Reply, Error> {
        let first = self.read_line()?;
        let code = code_of(&first).ok_or_else(|| {
            refused(
                &self.location,
                &format!(
                    "try the source again, because {} answered something that is not an FTP reply",
                    SafeUrl::new(&self.location)
                ),
            )
        })?;
        let mut text = first.clone();
        if first.as_bytes().get(3) == Some(&b'-') {
            let ending = format!("{code} ");
            loop {
                let line = self.read_line()?;
                text.push('\n');
                text.push_str(&line);
                if line.starts_with(&ending) {
                    break;
                }
            }
        }
        Ok(Reply { code, text })
    }

    fn read_line(&mut self) -> Result<String, Error> {
        let mut line = String::new();
        let read = self
            .reader
            .read_line(&mut line)
            .map_err(|reason| socket_failure(&self.location, &reason))?;
        if read == 0 {
            return Err(refused(
                &self.location,
                &format!(
                    "try the source again, because {} closed the control connection",
                    SafeUrl::new(&self.location)
                ),
            ));
        }
        Ok(line.trim_end_matches(['\r', '\n']).to_owned())
    }

    fn send(&mut self, verb: &str, argument: &str) -> Result<Reply, Error> {
        let line = if argument.is_empty() {
            format!("{verb}\r\n")
        } else {
            format!("{verb} {argument}\r\n")
        };
        let wire = self.reader.get_mut();
        wire.write_all(line.as_bytes())
            .map_err(|reason| socket_failure(&self.location, &reason))?;
        wire.flush()
            .map_err(|reason| socket_failure(&self.location, &reason))?;
        self.read_reply()
    }

    fn upgrade(&mut self) -> Result<(), String> {
        if !self.reader.buffer().is_empty() {
            return Err(
                "the server sent more before the handshake than it was asked for, and a reply \
                 buffered ahead of TLS is a response injection"
                    .to_owned(),
            );
        }
        let Wire::Plain(stream) = std::mem::replace(self.reader.get_mut(), Wire::Handed) else {
            return Err("the control connection was already secured".to_owned());
        };
        let secured = crate::tls::secured(stream, &self.host)?;
        *self.reader.get_mut() = Wire::Secured(Box::new(secured));
        self.secured = true;
        Ok(())
    }

    fn learn_features(&mut self) -> Result<(), Error> {
        let answered = self.send("FEAT", "")?;
        if answered.is(2) {
            self.features = answered
                .text
                .lines()
                .skip(1)
                .map(|line| line.trim().to_ascii_uppercase())
                .collect();
        }
        Ok(())
    }

    fn offers(&self, feature: &str) -> bool {
        self.features
            .iter()
            .any(|held| held == feature || held.starts_with(&format!("{feature} ")))
    }

    fn log_in(&mut self, credential: Option<&Credential>) -> Result<(), Error> {
        let (name, password) = match credential.and_then(Credential::bearer) {
            Some(token) => match token.split_once(':') {
                Some((name, password)) => (name.to_owned(), password.to_owned()),
                None => (token.to_owned(), String::new()),
            },
            None => (ANONYMOUS.to_owned(), ANONYMOUS_PASSWORD.to_owned()),
        };
        let answered = self.send("USER", &name)?;
        if answered.is(2) {
            return Ok(());
        }
        if !answered.is(3) {
            return Err(self.rejected(answered.code, "the user name"));
        }
        let answered = self.send("PASS", &password)?;
        if answered.is(2) {
            return Ok(());
        }
        Err(self.rejected(answered.code, "the password"))
    }

    fn rejected(&self, code: u16, what: &str) -> Error {
        Error::new(
            ErrorKind::PolicyCredentialMissing,
            format!(
                "set a credential for {}, because the server answered {code} to {what}",
                SafeUrl::new(&self.location)
            ),
        )
        .with_source(&self.location)
    }

    fn passive(&mut self) -> Result<SocketAddr, Error> {
        let answered = self.send("PASV", "")?;
        if !answered.is(2) {
            return Err(refused(
                &self.location,
                &format!(
                    "try the source again, because {} answered {} to a passive data connection",
                    SafeUrl::new(&self.location),
                    answered.code
                ),
            ));
        }
        let (advertised, port) = passive_address(&answered.text).ok_or_else(|| {
            refused(
                &self.location,
                &format!(
                    "try the source again, because {} answered a passive reply this build cannot read",
                    SafeUrl::new(&self.location)
                ),
            )
        })?;
        if advertised != self.peer {
            return Err(refused(
                &self.location,
                &format!(
                    "reach it from a network where the server names its own address, because {} pointed the data connection at {advertised} while the control connection is talking to {}, and a data connection is never opened to a host the control connection is not already talking to",
                    SafeUrl::new(&self.location),
                    self.peer
                ),
            ));
        }
        Ok(SocketAddr::new(self.peer, port))
    }

    fn data(&self, at: SocketAddr, limits: &Limits) -> Result<Wire, Error> {
        let stream = TcpStream::connect_timeout(&at, limits.connect_timeout).map_err(|reason| {
            refused(
                &self.location,
                &format!(
                    "try the source again, because the data connection to {} was refused: {reason}",
                    SafeUrl::new(&self.location)
                ),
            )
        })?;
        stream
            .set_read_timeout(Some(limits.idle_timeout))
            .map_err(|reason| socket_failure(&self.location, &reason))?;
        if !self.secured {
            return Ok(Wire::Plain(stream));
        }
        let secured = crate::tls::secured(stream, &self.host)
            .map_err(|reason| insecure(&self.location, &reason))?;
        Ok(Wire::Secured(Box::new(secured)))
    }
}

fn code_of(line: &str) -> Option<u16> {
    let head = line.get(..3)?;
    if !head.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    head.parse().ok()
}

fn passive_address(text: &str) -> Option<(IpAddr, u16)> {
    let inside = text.split_once('(')?.1.split_once(')')?.0;
    let numbers: Vec<u16> = inside
        .split(',')
        .map(|part| part.trim().parse().ok())
        .collect::<Option<Vec<u16>>>()?;
    let [one, two, three, four, high, low] = numbers.as_slice() else {
        return None;
    };
    let quad = [
        u8::try_from(*one).ok()?,
        u8::try_from(*two).ok()?,
        u8::try_from(*three).ok()?,
        u8::try_from(*four).ok()?,
    ];
    Some((
        IpAddr::from(quad),
        high.checked_mul(256)?.checked_add(*low)?,
    ))
}

pub struct FtpBody {
    data: Wire,
    control: Option<Control>,
    counted: Arc<WorkCounter>,
}

impl Read for FtpBody {
    fn read(&mut self, into: &mut [u8]) -> std::io::Result<usize> {
        let read = self.data.read(into)?;
        if read == 0 {
            if let Some(mut control) = self.control.take() {
                let _ = control.read_reply();
                let _ = control.send("QUIT", "");
            }
        } else {
            self.counted.read_bytes(read as u64);
        }
        Ok(read)
    }
}

pub struct FtpSource {
    limits: Limits,
    work: Arc<WorkCounter>,
    degradations: DegradeQueue,
}

impl FtpSource {
    #[must_use]
    pub fn new(limits: Limits, work: Arc<WorkCounter>) -> Self {
        Self {
            limits,
            work,
            degradations: DegradeQueue::new(),
        }
    }

    fn connected(
        &self,
        location: &str,
        credential: Option<&Credential>,
    ) -> Result<(Control, Where), Error> {
        let at = parsed(location)?;
        let mut control = Control::open(&at, &self.limits, location)?;
        self.work.issued_request();
        control.learn_features()?;
        let secured = self.secure(&mut control, &at, location)?;
        if !secured && credential.is_some() {
            return Err(Error::new(
                ErrorKind::PolicyCredentialInvalid,
                format!(
                    "reach it as ftps://{} instead, because a credential was resolved for that host and the control connection could not be secured, so the password would travel in the clear",
                    at.host
                ),
            )
            .with_source(location));
        }
        control.log_in(credential)?;
        let answered = control.send("TYPE", "I")?;
        if !answered.is(2) {
            return Err(refused(
                location,
                &format!(
                    "try the source again, because {} answered {} to a binary transfer",
                    SafeUrl::new(location),
                    answered.code
                ),
            ));
        }
        Ok((control, at))
    }

    fn secure(&self, control: &mut Control, at: &Where, location: &str) -> Result<bool, Error> {
        let answered = control.send("AUTH", "TLS")?;
        if !answered.is(2) {
            if at.securing == Securing::Required {
                return Err(insecure(location, &answered.text));
            }
            self.degradations.record(
                format!("a secured control connection to {}", at.host),
                "a control connection in the clear".to_owned(),
                format!(
                    "the server answered {} to AUTH TLS, so every command and every byte travels unencrypted",
                    answered.code
                ),
            );
            return Ok(false);
        }
        if let Err(reason) = control.upgrade() {
            return Err(insecure(location, &reason));
        }
        let answered = control.send("PBSZ", "0")?;
        if !answered.is(2) {
            return Err(insecure(location, &answered.text));
        }
        let answered = control.send("PROT", "P")?;
        if !answered.is(2) {
            return Err(insecure(location, &answered.text));
        }
        Ok(true)
    }

    fn described(
        control: &mut Control,
        at: &Where,
        location: &str,
    ) -> Result<SourceMetadata, Error> {
        let answered = control.send("SIZE", &at.path)?;
        if !answered.is(2) {
            return Err(absent(location, &answered.text));
        }
        let size = answered
            .text
            .split_whitespace()
            .nth(1)
            .and_then(|held| held.parse().ok());
        let modified = control.send("MDTM", &at.path)?;
        let identity = modified
            .is(2)
            .then(|| modified.text.split_whitespace().nth(1).map(str::to_owned))
            .flatten()
            .map_or(SourceIdentity::None, SourceIdentity::WeakValidator);
        Ok(SourceMetadata {
            location: SafeUrl::new(location),
            host: Host::new(at.host.clone()),
            size,
            content: None,
            interop: None,
            identity,
            last_modified: None,
            supports_ranges: control.offers("REST STREAM") || control.offers("REST"),
            time_to_first_byte: Duration::ZERO,
            retry_after: None,
            cost: Cost::default(),
        })
    }

    fn read_data(&self, control: &mut Control, verb: &str, path: &str) -> Result<Vec<u8>, Error> {
        let at = control.passive()?;
        let stream = control.data(at, &self.limits)?;
        let answered = control.send(verb, path)?;
        if !answered.is(1) {
            return Err(absent(&control.location, &answered.text));
        }
        self.work.issued_request();
        let mut read = Vec::new();
        stream
            .take(self.limits.listing_bytes)
            .read_to_end(&mut read)
            .map_err(|reason| socket_failure(&control.location, &reason))?;
        let ending = control.read_reply()?;
        if !ending.is(2) {
            return Err(absent(&control.location, &ending.text));
        }
        Ok(read)
    }

    fn walked(
        &self,
        control: &mut Control,
        base: &str,
        directory: &str,
        location: &str,
        entries: &mut Vec<ListingEntry>,
    ) -> Result<(), Error> {
        let asked = if directory.is_empty() {
            base.to_owned()
        } else {
            format!("{}/{directory}", base.trim_end_matches('/'))
        };
        let read = match self.read_data(control, "MLSD", &asked) {
            Ok(read) => read,
            Err(reason) if reason.kind() == ErrorKind::ReferenceUnresolved => {
                self.degradations.record(
                    "MLSD, the machine-readable listing".to_owned(),
                    "LIST, the listing meant for a person".to_owned(),
                    format!(
                        "{} would not answer MLSD, so a name and a size are read out of an ls line",
                        SafeUrl::new(location)
                    ),
                );
                self.read_data(control, "LIST", &asked)?
            }
            Err(reason) => return Err(reason),
        };
        let text = String::from_utf8_lossy(&read).into_owned();
        let mut folders = Vec::new();
        let allowed = usize::try_from(self.limits.listing_entries).unwrap_or(usize::MAX);
        for line in text.lines() {
            let Some(member) = member_of(line) else {
                continue;
            };
            if !is_under_the_directory(&member.name) {
                return Err(outside(location, &member.name));
            }
            let path = if directory.is_empty() {
                member.name.clone()
            } else {
                format!("{directory}/{}", member.name)
            };
            if member.is_directory {
                folders.push(path);
                continue;
            }
            if entries.len() >= allowed {
                return Err(Error::new(
                    ErrorKind::ResourceLimit,
                    format!(
                        "ask for a narrower directory, because {} holds more than the {} entries a run reads",
                        SafeUrl::new(location),
                        self.limits.listing_entries
                    ),
                )
                .with_source(location));
            }
            entries.push(ListingEntry {
                location: SafeUrl::new(&format!("{}/{path}", location.trim_end_matches('/'))),
                path,
                size: member.size,
                content: None,
                interop: None,
            });
        }
        for folder in folders {
            self.walked(control, base, &folder, location, entries)?;
        }
        Ok(())
    }
}

struct Member {
    name: String,
    size: Option<u64>,
    is_directory: bool,
}

fn member_of(line: &str) -> Option<Member> {
    if line.contains(';') {
        return machine_member(line);
    }
    human_member(line)
}

fn machine_member(line: &str) -> Option<Member> {
    let (facts, name) = line.split_once("; ")?;
    let mut size = None;
    let mut kind = String::new();
    for fact in facts.split(';') {
        let Some((named, value)) = fact.split_once('=') else {
            continue;
        };
        match named.trim().to_ascii_lowercase().as_str() {
            "size" => size = value.trim().parse().ok(),
            "type" => kind = value.trim().to_ascii_lowercase(),
            _ => {}
        }
    }
    if kind == "cdir" || kind == "pdir" {
        return None;
    }
    Some(Member {
        name: name.trim_end().to_owned(),
        size,
        is_directory: kind == "dir",
    })
}

fn after_the_columns(line: &str, columns: usize) -> Option<&str> {
    let mut rest = line;
    for _ in 0..columns {
        rest = rest.trim_start();
        let ends = rest.find(char::is_whitespace)?;
        rest = rest.get(ends..)?;
    }
    let name = rest.trim_start();
    (!name.is_empty()).then_some(name)
}

fn human_member(line: &str) -> Option<Member> {
    let first = line.chars().next()?;
    if first == 'l' {
        return None;
    }
    let fields: Vec<&str> = line.split_whitespace().collect();
    let [
        _permissions,
        _links,
        _owner,
        _group,
        size,
        _month,
        _day,
        _when,
        ..,
    ] = fields.as_slice()
    else {
        return None;
    };
    let name = after_the_columns(line, 8)?.trim_end().to_owned();
    if name == "." || name == ".." {
        return None;
    }
    Some(Member {
        name,
        size: size.parse().ok(),
        is_directory: first == 'd',
    })
}

impl Source for FtpSource {
    type Body = FtpBody;

    fn serves(&self, reference: &str) -> Option<Serves> {
        let at = parsed(reference).ok()?;
        Some(if at.path.is_empty() || at.path.ends_with('/') {
            Serves::Container
        } else {
            Serves::Object
        })
    }

    fn take_degradations(&self) -> Vec<Degradation> {
        self.degradations.take()
    }

    fn probe(
        &self,
        location: &str,
        credential: Option<&Credential>,
    ) -> Result<SourceMetadata, Error> {
        let started = Instant::now();
        let (mut control, at) = self.connected(location, credential)?;
        let mut described = Self::described(&mut control, &at, location)?;
        described.time_to_first_byte = started.elapsed();
        let _ = control.send("QUIT", "");
        Ok(described)
    }

    fn fetch(
        &self,
        location: &str,
        range: Option<ByteRange>,
        credential: Option<&Credential>,
        _resuming: Option<&str>,
    ) -> Result<Served<Self::Body>, Error> {
        let started = Instant::now();
        let (mut control, at) = self.connected(location, credential)?;
        let mut metadata = Self::described(&mut control, &at, location)?;
        let opened_at = control.passive()?;
        let data = control.data(opened_at, &self.limits)?;
        if let Some(range) = range
            && range.start > 0
        {
            let answered = control.send("REST", &range.start.to_string())?;
            if !answered.is(3) {
                return Err(Error::new(
                    ErrorKind::SourceUnsupportedRange,
                    format!(
                        "fetch it whole, because {} answered {} to a restart offset",
                        SafeUrl::new(location),
                        answered.code
                    ),
                )
                .with_source(location));
            }
        }
        let answered = control.send("RETR", &at.path)?;
        if !answered.is(1) {
            return Err(absent(location, &answered.text));
        }
        self.work.issued_request();
        metadata.time_to_first_byte = started.elapsed();
        Ok(Served {
            metadata,
            body: FtpBody {
                data,
                control: Some(control),
                counted: Arc::clone(&self.work),
            },
        })
    }

    fn revalidate(
        &self,
        location: &str,
        validator: &Validator,
        credential: Option<&Credential>,
    ) -> Result<Revalidated<Self::Body>, Error> {
        let _ = validator;
        Ok(Revalidated::Changed(Box::new(
            self.fetch(location, None, credential, None)?,
        )))
    }

    fn list(&self, location: &str, credential: Option<&Credential>) -> Result<Listing, Error> {
        let (mut control, at) = self.connected(location, credential)?;
        let mut entries = Vec::new();
        self.walked(&mut control, &at.path, "", location, &mut entries)?;
        let _ = control.send("QUIT", "");
        entries.sort_by(|one, other| one.path.as_bytes().cmp(other.path.as_bytes()));
        Ok(Listing {
            entries,
            skipped: 0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{code_of, human_member, machine_member, parsed, passive_address};

    #[test]
    fn a_passive_reply_names_the_quad_and_the_port_the_two_bytes_make() {
        let Some((host, port)) = passive_address("227 Entering Passive Mode (192,168,0,1,19,136)")
        else {
            unreachable!("a passive reply in the shape RFC 959 states was not read")
        };
        assert_eq!(host.to_string(), "192.168.0.1");
        assert_eq!(port, 19 * 256 + 136);
    }

    #[test]
    fn a_reply_code_is_the_first_three_digits_and_nothing_else_is_one() {
        assert_eq!(code_of("226 transfer complete"), Some(226));
        assert_eq!(code_of("211-Features:"), Some(211));
        assert_eq!(code_of(" 226 leading space"), None);
        assert_eq!(code_of("ok"), None);
    }

    #[test]
    fn an_mlsd_line_states_its_own_type_and_size() {
        let Some(file) = machine_member("type=file;size=1234;modify=20200102030405; reads.txt")
        else {
            unreachable!("a file line was not read")
        };
        assert_eq!(file.name, "reads.txt");
        assert_eq!(file.size, Some(1234));
        assert!(!file.is_directory);
        let Some(folder) = machine_member("type=dir;modify=20200102030405; deep") else {
            unreachable!("a directory line was not read")
        };
        assert!(folder.is_directory);
        assert!(machine_member("type=cdir;modify=20200102030405; .").is_none());
    }

    #[test]
    fn an_ls_line_gives_up_a_name_and_a_size_without_a_date_being_parsed() {
        let Some(file) = human_member("-rw-r--r--   1 ftp ftp         1234 Jan  2  2020 reads.txt")
        else {
            unreachable!("an ls line was not read")
        };
        assert_eq!(file.name, "reads.txt");
        assert_eq!(file.size, Some(1234));
        assert!(!file.is_directory);
        let Some(folder) = human_member("drwxr-xr-x   2 ftp ftp         4096 Jan  2 03:04 deep")
        else {
            unreachable!("an ls directory line was not read")
        };
        assert!(folder.is_directory);
        assert!(
            human_member("lrwxrwxrwx   1 ftp ftp    9 Jan  2  2020 link -> reads.txt").is_none(),
            "a symbolic link was read as a member, and a link is not one"
        );
        let Some(named_like_a_column) =
            human_member("-rw-r--r--   1 ftp ftp           12 Jan  2  2020 ftp")
        else {
            unreachable!("a file named after one of the columns was not read")
        };
        assert_eq!(
            named_like_a_column.name, "ftp",
            "the name was taken from the first place the text appeared rather than from after the columns"
        );
    }

    #[test]
    fn a_port_written_after_the_host_is_the_one_the_control_connection_uses() {
        let Ok(at) = parsed("ftps://host.example:2121/pub/x.bin") else {
            unreachable!("a reference naming a port was not read")
        };
        assert_eq!(at.host, "host.example");
        assert_eq!(at.port, 2121);
        assert_eq!(at.path, "pub/x.bin");
        assert!(parsed("ftp://user:secret@host/x").is_err());
        assert!(parsed("sftp://host/x").is_err());
    }
}
