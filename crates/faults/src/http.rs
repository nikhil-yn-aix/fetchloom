//! A server that answers exactly what a test told it to answer.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

/// What a server does with one request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    /// Serve the object, honoring a range when one was asked for.
    Whole,
    /// Serve the object but stop after this many bytes, then close.
    Truncated {
        /// How many bytes of the body reach the client.
        after: usize,
    },
    /// Serve the object with the byte at this offset inverted.
    Flipped {
        /// Which byte of the object is wrong.
        offset: usize,
    },
    /// Serve a status and no body.
    Status {
        /// The status code.
        code: u16,
        /// What to put in `Retry-After`, when anything.
        retry_after: Option<String>,
    },
    /// Accept the range and serve a different span of the object.
    WrongRange {
        /// How far the served span is moved from the span asked for.
        shift: u64,
    },
    /// Accept the range and serve the whole object with a two hundred.
    RangeIgnored,
    /// Serve the object under a `Content-Length` that is not its length.
    LyingLength {
        /// The length the response claims.
        claimed: u64,
    },
    /// Serve this many bytes and then never write or close again.
    Stalled {
        /// How many bytes reach the client before the connection goes quiet.
        after: usize,
    },
    /// Serve this many bytes and then close without finishing.
    ClosedMidBody {
        /// How many bytes reach the client before the connection closes.
        after: usize,
    },
    /// Redirect.
    Redirect {
        /// The status code.
        code: u16,
        /// The value of the `Location` header.
        location: String,
    },
    /// Serve a directory index.
    Listing {
        /// Which format the index is written in.
        format: IndexFormat,
    },
}

/// A directory index format a listing is served in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IndexFormat {
    /// The XML an object store returns for a list request.
    ObjectStore,
    /// The multi-status XML a `WebDAV` `PROPFIND` returns.
    WebDav,
    /// The HTML index a common web server generates.
    GeneratedHtml,
    /// A body in no recognized format.
    Unrecognized,
}

/// The latency a server charges before it answers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Latency {
    every: Duration,
    hosts: Vec<(String, Duration)>,
}

impl Latency {
    /// Returns the latency with this much charged to every request.
    #[must_use]
    pub fn every_request(mut self, waiting: Duration) -> Self {
        self.every = waiting;
        self
    }

    /// Returns the latency with this much charged to requests naming one host.
    #[must_use]
    pub fn host(mut self, host: impl Into<String>, waiting: Duration) -> Self {
        self.hosts.push((host.into(), waiting));
        self
    }

    /// Returns how long a request waits before it is answered.
    #[must_use]
    pub fn before(&self, request: &Received) -> Duration {
        let named = request.header("host").unwrap_or_default().to_lowercase();
        let host = self
            .hosts
            .iter()
            .find(|(held, _)| held.to_lowercase() == named)
            .map_or(Duration::ZERO, |(_, waiting)| *waiting);
        self.every.saturating_add(host)
    }
}

/// A TLS alert record carrying a fatal handshake failure.
const HANDSHAKE_FAILURE: [u8; 7] = [0x15, 0x03, 0x03, 0x00, 0x02, 0x02, 0x28];

/// What a test told one server to do.
#[derive(Clone, Debug)]
pub struct Script {
    /// The bytes the object holds.
    pub object: Vec<u8>,
    /// The entity tag each request is answered with, in order.
    pub etags: Vec<String>,
    /// What each request for bytes is answered with, in order.
    pub replies: Vec<Reply>,
    /// What every request after the script is answered with.
    pub then: Reply,
    /// Whether the server advertises range support.
    pub accepts_ranges: bool,
    /// The last modified value every response carries, when the server states
    /// one.
    pub last_modified: Option<String>,
    /// Whether the server answers a conditional request whose validator still
    /// matches with a three hundred and four.
    pub honors_conditionals: bool,
    /// Whether the server answers a secured connection with a refusal instead
    /// of a handshake.
    pub refuses_tls: bool,
    /// The latency charged before any answer is written.
    pub latency: Latency,
}

impl Script {
    /// Builds a script that serves the object whole, forever.
    #[must_use]
    pub fn serving(object: Vec<u8>) -> Self {
        Self {
            object,
            etags: Vec::new(),
            replies: Vec::new(),
            then: Reply::Whole,
            accepts_ranges: true,
            last_modified: None,
            honors_conditionals: true,
            refuses_tls: false,
            latency: Latency::default(),
        }
    }

    /// Returns the script with every secured connection refused at the
    /// handshake.
    #[must_use]
    pub fn refusing_tls(mut self) -> Self {
        self.refuses_tls = true;
        self
    }

    /// Returns the script with a last modified value on every response.
    #[must_use]
    pub fn modified_at(mut self, value: impl Into<String>) -> Self {
        self.last_modified = Some(value.into());
        self
    }

    /// Returns the script with conditional requests answered or ignored.
    #[must_use]
    pub fn conditional(mut self, honors: bool) -> Self {
        self.honors_conditionals = honors;
        self
    }

    /// Returns the script with these replies used, in order, before `then`.
    #[must_use]
    pub fn replying(mut self, replies: Vec<Reply>) -> Self {
        self.replies = replies;
        self
    }

    /// Returns the script with these entity tags used, in order.
    #[must_use]
    pub fn tagged(mut self, etags: Vec<String>) -> Self {
        self.etags = etags;
        self
    }

    /// Returns the script with range support advertised or withheld.
    #[must_use]
    pub fn ranges(mut self, accepts: bool) -> Self {
        self.accepts_ranges = accepts;
        self
    }

    /// Returns the script with this latency charged before every answer.
    #[must_use]
    pub fn delayed(mut self, latency: Latency) -> Self {
        self.latency = latency;
        self
    }
}

/// One request a server received.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Received {
    /// The method.
    pub method: String,
    /// The target, as it appeared on the request line.
    pub target: String,
    /// Every header, lowercased, in the order they arrived.
    pub headers: Vec<(String, String)>,
}

impl Received {
    /// Returns the value of a header, when the request carried it.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(held, _)| held == name)
            .map(|(_, value)| value.as_str())
    }
}

/// A server a test drives.
#[derive(Debug)]
pub struct TestServer {
    address: SocketAddr,
    received: Arc<Mutex<Vec<Received>>>,
    stop: Arc<AtomicBool>,
}

impl TestServer {
    /// Starts a server on IPv4 loopback, on a port the platform chooses.
    ///
    /// # Errors
    ///
    /// Fails when no port can be bound.
    pub fn start(script: Script) -> std::io::Result<Self> {
        Self::start_on("127.0.0.1", script)
    }

    /// Starts a server on the loopback address given, on a port the platform
    /// chooses, for a test that needs two hosts rather than two ports.
    ///
    /// # Errors
    ///
    /// Fails when no port can be bound.
    pub fn start_on(loopback: &str, script: Script) -> std::io::Result<Self> {
        let listener = TcpListener::bind((loopback, 0))?;
        let address = listener.local_addr()?;
        let received = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));

        let served = Arc::new(script);
        let counter = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&received);
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
                let counter = Arc::clone(&counter);
                let seen = Arc::clone(&seen);
                let ending = Arc::clone(&ending);
                std::thread::spawn(move || {
                    serve(&connection, &script, &counter, &seen, &ending);
                });
            }
        });

        Ok(Self {
            address,
            received,
            stop,
        })
    }

    /// Returns the base the test points a client at.
    #[must_use]
    pub fn origin(&self) -> String {
        format!("http://{}", self.address)
    }

    /// Returns the base a test that means to speak TLS points a client at.
    #[must_use]
    pub fn secured_origin(&self) -> String {
        format!("https://{}", self.address)
    }

    /// Returns every request the server has received, in order.
    #[must_use]
    pub fn received(&self) -> Vec<Received> {
        self.received
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect(self.address);
    }
}

fn serve(
    connection: &TcpStream,
    script: &Script,
    counter: &AtomicUsize,
    seen: &Mutex<Vec<Received>>,
    ending: &AtomicBool,
) {
    if script.refuses_tls {
        let mut hello = [0_u8; 1024];
        let mut reading = connection;
        let _ = reading.read(&mut hello);
        let mut writing = connection;
        let _ = writing.write_all(&HANDSHAKE_FAILURE);
        let _ = writing.flush();
        std::thread::sleep(std::time::Duration::from_millis(200));
        return;
    }
    let mut reader = BufReader::new(connection);
    loop {
        let Some(request) = read_request(&mut reader) else {
            return;
        };
        seen.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(request.clone());
        std::thread::sleep(script.latency.before(&request));

        let serving_bytes = request.method != "HEAD";
        let index = counter.load(Ordering::SeqCst);
        if serving_bytes {
            counter.fetch_add(1, Ordering::SeqCst);
        }
        let reply = if serving_bytes {
            script
                .replies
                .get(index)
                .cloned()
                .unwrap_or_else(|| script.then.clone())
        } else {
            let answered = metadata_reply(script.replies.get(index));
            if answered != Reply::Whole {
                counter.fetch_add(1, Ordering::SeqCst);
            }
            answered
        };
        let etag = script
            .etags
            .get(index)
            .cloned()
            .or_else(|| script.etags.last().cloned());

        let close = request.header("connection") == Some("close");
        let mut writer = connection;
        if !answer(
            &mut writer,
            script,
            &request,
            &reply,
            etag.as_deref(),
            ending,
        ) || close
        {
            return;
        }
    }
}

fn read_request(reader: &mut BufReader<&TcpStream>) -> Option<Received> {
    let mut line = String::new();
    if reader.read_line(&mut line).ok()? == 0 {
        return None;
    }
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_owned();
    let target = parts.next()?.to_owned();

    let mut headers = Vec::new();
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).ok()? == 0 {
            return None;
        }
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        let (name, value) = header.split_once(':')?;
        headers.push((name.trim().to_lowercase(), value.trim().to_owned()));
    }
    Some(Received {
        method,
        target,
        headers,
    })
}

fn answer(
    writer: &mut impl Write,
    script: &Script,
    request: &Received,
    reply: &Reply,
    etag: Option<&str>,
    ending: &AtomicBool,
) -> bool {
    let asked = request.header("range").and_then(parse_range);
    if script.honors_conditionals && still_current(script, request, etag) {
        return not_modified(writer, script, etag);
    }
    if request.method == "HEAD" {
        return metadata_only(writer, script, reply, etag, asked);
    }
    match reply {
        Reply::Status { code, retry_after } => status(writer, *code, retry_after.as_deref()),
        Reply::Redirect { code, location } => redirect(writer, *code, location),
        Reply::Listing { format } => index(writer, *format),
        Reply::RangeIgnored => whole(writer, script, etag, &script.object.clone()),
        Reply::WrongRange { shift } => wrong_range(writer, script, etag, asked, *shift),
        Reply::LyingLength { claimed } => body(writer, script, etag, asked, None, Some(*claimed)),
        Reply::Truncated { after } | Reply::ClosedMidBody { after } => {
            body(writer, script, etag, asked, Some(*after), None);
            false
        }
        Reply::Stalled { after } => {
            if body(writer, script, etag, asked, Some(*after), None) {
                while !ending.load(Ordering::Relaxed) {
                    std::thread::park_timeout(std::time::Duration::from_millis(50));
                }
            }
            false
        }
        Reply::Flipped { offset } => flipped(writer, script, etag, asked, *offset),
        Reply::Whole => body(writer, script, etag, asked, None, None),
    }
}

fn status(writer: &mut impl Write, code: u16, retry_after: Option<&str>) -> bool {
    let mut headers = vec![("Content-Length".to_owned(), "0".to_owned())];
    if let Some(after) = retry_after {
        headers.push(("Retry-After".to_owned(), after.to_owned()));
    }
    write_head(writer, code, &headers) && flush(writer)
}

fn redirect(writer: &mut impl Write, code: u16, location: &str) -> bool {
    write_head(
        writer,
        code,
        &[
            ("Location".to_owned(), location.to_owned()),
            ("Content-Length".to_owned(), "0".to_owned()),
        ],
    ) && flush(writer)
}

fn index(writer: &mut impl Write, format: IndexFormat) -> bool {
    let body = listing(format);
    let status = if matches!(format, IndexFormat::WebDav) {
        207
    } else {
        200
    };
    write_head(
        writer,
        status,
        &[
            ("Content-Type".to_owned(), content_type(format).to_owned()),
            ("Content-Length".to_owned(), body.len().to_string()),
        ],
    ) && writer.write_all(&body).is_ok()
        && flush(writer)
}

fn wrong_range(
    writer: &mut impl Write,
    script: &Script,
    etag: Option<&str>,
    asked: Option<Span>,
    shift: u64,
) -> bool {
    let Some(asked) = asked else {
        return whole(writer, script, etag, &script.object.clone());
    };
    let start = asked.0.saturating_add(shift);
    let sent = script.object.get(usize(start)..).unwrap_or_default();
    partial(writer, script, etag, sent, start) && flush(writer)
}

fn flipped(
    writer: &mut impl Write,
    script: &Script,
    etag: Option<&str>,
    asked: Option<Span>,
    offset: usize,
) -> bool {
    let mut object = script.object.clone();
    if let Some(byte) = object.get_mut(offset) {
        *byte = !*byte;
    }
    let changed = Script {
        object,
        ..script.clone()
    };
    body(writer, &changed, etag, asked, None, None)
}

fn body(
    writer: &mut impl Write,
    script: &Script,
    etag: Option<&str>,
    asked: Option<Span>,
    stop_after: Option<usize>,
    claimed: Option<u64>,
) -> bool {
    let whole = served(script, asked).to_vec();
    let length = claimed.unwrap_or(whole.len() as u64);
    let sent = match stop_after {
        Some(after) => whole.get(..after).unwrap_or(&whole),
        None => whole.as_slice(),
    };
    write_head(
        writer,
        status_for(asked),
        &head(script, etag, length, asked, whole.len()),
    ) && writer.write_all(sent).is_ok()
        && flush(writer)
}

fn whole(writer: &mut impl Write, script: &Script, etag: Option<&str>, body: &[u8]) -> bool {
    write_head(writer, 200, &head(script, etag, body.len() as u64, None, 0))
        && writer.write_all(body).is_ok()
        && flush(writer)
}

fn partial(
    writer: &mut impl Write,
    script: &Script,
    etag: Option<&str>,
    body: &[u8],
    start: u64,
) -> bool {
    let mut headers = head(
        script,
        etag,
        body.len() as u64,
        Some((start, None)),
        body.len(),
    );
    headers.retain(|(name, _)| name != "Content-Range");
    headers.push((
        "Content-Range".to_owned(),
        format!(
            "bytes {start}-{}/{}",
            start + body.len() as u64 - 1,
            script.object.len()
        ),
    ));
    write_head(writer, 206, &headers) && writer.write_all(body).is_ok()
}

/// The span a request asked for: the first byte, and the last when it named
/// one.
type Span = (u64, Option<u64>);

fn served(script: &Script, asked: Option<Span>) -> &[u8] {
    let Some((start, end)) = asked else {
        return script.object.as_slice();
    };
    let from = usize(start);
    let to = end.map_or(script.object.len(), |last| {
        usize(last.saturating_add(1)).min(script.object.len())
    });
    script.object.get(from..to.max(from)).unwrap_or_default()
}

fn status_for(asked: Option<Span>) -> u16 {
    if asked.is_some() { 206 } else { 200 }
}

fn head(
    script: &Script,
    etag: Option<&str>,
    length: u64,
    asked: Option<Span>,
    served_length: usize,
) -> Vec<(String, String)> {
    let mut headers = vec![("Content-Length".to_owned(), length.to_string())];
    if script.accepts_ranges {
        headers.push(("Accept-Ranges".to_owned(), "bytes".to_owned()));
    }
    if let Some(etag) = etag {
        headers.push(("ETag".to_owned(), etag.to_owned()));
    }
    if let Some(value) = script.last_modified.as_deref() {
        headers.push(("Last-Modified".to_owned(), value.to_owned()));
    }
    if let Some((start, _)) = asked {
        headers.push((
            "Content-Range".to_owned(),
            format!(
                "bytes {start}-{}/{}",
                start + served_length as u64 - 1,
                script.object.len()
            ),
        ));
    }
    headers
}

fn write_head(writer: &mut impl Write, code: u16, headers: &[(String, String)]) -> bool {
    let mut head = format!("HTTP/1.1 {code} {}\r\n", reason(code));
    for (name, value) in headers {
        head.push_str(name);
        head.push_str(": ");
        head.push_str(value);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    writer.write_all(head.as_bytes()).is_ok()
}

fn flush(writer: &mut impl Write) -> bool {
    writer.flush().is_ok()
}

/// Reads the span a request asked for.
fn parse_range(value: &str) -> Option<(u64, Option<u64>)> {
    let span = value.trim().strip_prefix("bytes=")?;
    let (start, end) = span.split_once('-')?;
    let start = start.trim().parse().ok()?;
    let end = end.trim();
    let end = if end.is_empty() {
        None
    } else {
        Some(end.parse().ok()?)
    };
    Some((start, end))
}

/// Reports whether a conditional request's validator still matches.
fn still_current(script: &Script, request: &Received, etag: Option<&str>) -> bool {
    if let Some(asked) = request.header("if-none-match") {
        return etag.is_some_and(|held| asked.split(',').any(|one| one.trim() == held));
    }
    if let Some(asked) = request.header("if-modified-since") {
        return script.last_modified.as_deref() == Some(asked);
    }
    false
}

/// Answers a conditional request whose validator still matches.
fn not_modified(writer: &mut impl Write, script: &Script, etag: Option<&str>) -> bool {
    let mut headers = vec![("Content-Length".to_owned(), "0".to_owned())];
    if let Some(etag) = etag {
        headers.push(("ETag".to_owned(), etag.to_owned()));
    }
    if let Some(value) = script.last_modified.as_deref() {
        headers.push(("Last-Modified".to_owned(), value.to_owned()));
    }
    write_head(writer, 304, &headers) && flush(writer)
}

fn usize(value: u64) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

fn reason(code: u16) -> &'static str {
    match code {
        200 => "OK",
        206 => "Partial Content",
        207 => "Multi-Status",
        301 => "Moved Permanently",
        302 => "Found",
        304 => "Not Modified",
        307 => "Temporary Redirect",
        308 => "Permanent Redirect",
        403 => "Forbidden",
        404 => "Not Found",
        416 => "Range Not Satisfiable",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Unknown",
    }
}

fn content_type(format: IndexFormat) -> &'static str {
    match format {
        IndexFormat::ObjectStore | IndexFormat::WebDav => "application/xml",
        IndexFormat::GeneratedHtml => "text/html",
        IndexFormat::Unrecognized => "application/octet-stream",
    }
}

fn listing(format: IndexFormat) -> Vec<u8> {
    match format {
        IndexFormat::ObjectStore => OBJECT_STORE_INDEX.as_bytes().to_vec(),
        IndexFormat::WebDav => WEBDAV_INDEX.as_bytes().to_vec(),
        IndexFormat::GeneratedHtml => GENERATED_HTML_INDEX.as_bytes().to_vec(),
        IndexFormat::Unrecognized => UNRECOGNIZED_INDEX.as_bytes().to_vec(),
    }
}

/// The XML an object store returns for a list request.
const OBJECT_STORE_INDEX: &str = concat!(
    r#"<?xml version="1.0" encoding="UTF-8"?>"#,
    r#"<ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/">"#,
    "<Name>bucket</Name><Prefix>set/</Prefix><KeyCount>2</KeyCount>",
    "<IsTruncated>false</IsTruncated>",
    "<Contents><Key>set/one</Key><Size>11</Size></Contents>",
    "<Contents><Key>set/two</Key><Size>22</Size></Contents>",
    "</ListBucketResult>"
);

/// The multi-status XML a `WebDAV` `PROPFIND` returns.
const WEBDAV_INDEX: &str = concat!(
    r#"<?xml version="1.0" encoding="utf-8"?>"#,
    r#"<D:multistatus xmlns:D="DAV:">"#,
    "<D:response><D:href>/set/one</D:href><D:propstat><D:prop>",
    "<D:getcontentlength>11</D:getcontentlength></D:prop>",
    "<D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>",
    "<D:response><D:href>/set/two</D:href><D:propstat><D:prop>",
    "<D:getcontentlength>22</D:getcontentlength></D:prop>",
    "<D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>",
    "</D:multistatus>"
);

/// The HTML index a common web server generates.
const GENERATED_HTML_INDEX: &str = concat!(
    "<html><head><title>Index of /set/</title></head><body>",
    "<h1>Index of /set/</h1><pre>",
    r#"<a href="../">../</a>"#,
    "\n",
    r#"<a href="one">one</a>                    01-Jan-2026 00:00     11"#,
    "\n",
    r#"<a href="two">two</a>                    01-Jan-2026 00:00     22"#,
    "\n</pre></body></html>"
);

/// A body in no recognized format.
const UNRECOGNIZED_INDEX: &str = "one 11\ntwo 22\n";

fn metadata_reply(next: Option<&Reply>) -> Reply {
    match next {
        Some(Reply::Status { code, retry_after }) => Reply::Status {
            code: *code,
            retry_after: retry_after.clone(),
        },
        Some(Reply::Redirect { code, location }) => Reply::Redirect {
            code: *code,
            location: location.clone(),
        },
        _ => Reply::Whole,
    }
}

/// Answers a metadata request, which carries every header and no body.
fn metadata_only(
    writer: &mut impl Write,
    script: &Script,
    reply: &Reply,
    etag: Option<&str>,
    asked: Option<Span>,
) -> bool {
    match reply {
        Reply::Status { code, retry_after } => status(writer, *code, retry_after.as_deref()),
        Reply::Redirect { code, location } => redirect(writer, *code, location),
        _ => {
            let whole = served(script, asked).to_vec();
            write_head(
                writer,
                status_for(asked),
                &head(script, etag, whole.len() as u64, asked, whole.len()),
            ) && flush(writer)
        }
    }
}
