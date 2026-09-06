//! A server that answers exactly what a test told it to answer.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    Whole,
    Truncated {
        after: usize,
    },
    Flipped {
        offset: usize,
    },
    Status {
        code: u16,
        retry_after: Option<String>,
    },
    WrongRange {
        shift: u64,
    },
    RangeIgnored,
    LyingLength {
        claimed: u64,
    },
    Stalled {
        after: usize,
    },
    ClosedMidBody {
        after: usize,
    },
    Redirect {
        code: u16,
        location: String,
    },
    Listing {
        format: IndexFormat,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IndexFormat {
    ObjectStore,
    WebDav,
    GeneratedHtml,
    Unrecognized,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Latency {
    every: Duration,
    hosts: Vec<(String, Duration)>,
}

impl Latency {
    #[must_use]
    pub fn every_request(mut self, waiting: Duration) -> Self {
        self.every = waiting;
        self
    }

    #[must_use]
    pub fn host(mut self, host: impl Into<String>, waiting: Duration) -> Self {
        self.hosts.push((host.into(), waiting));
        self
    }

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

#[derive(Debug, Default)]
pub struct Flight {
    current: AtomicUsize,
    peak: AtomicUsize,
}

impl Flight {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    #[must_use]
    pub fn peak(&self) -> usize {
        self.peak.load(Ordering::SeqCst)
    }

    fn enter(recorder: &Arc<Self>) -> InFlight {
        let now = recorder.current.fetch_add(1, Ordering::SeqCst) + 1;
        recorder.peak.fetch_max(now, Ordering::SeqCst);
        InFlight {
            recorder: Arc::clone(recorder),
        }
    }
}

#[derive(Debug)]
pub struct InFlight {
    recorder: Arc<Flight>,
}

impl Drop for InFlight {
    fn drop(&mut self) {
        self.recorder.current.fetch_sub(1, Ordering::SeqCst);
    }
}

const HANDSHAKE_FAILURE: [u8; 7] = [0x15, 0x03, 0x03, 0x00, 0x02, 0x02, 0x28];

#[derive(Clone, Debug)]
pub struct Script {
    pub object: Vec<u8>,
    etags: Vec<String>,
    pub replies: Vec<Reply>,
    pub then: Reply,
    pub accepts_ranges: bool,
    pub last_modified: Option<String>,
    honors_conditionals: bool,
    refuses_tls: bool,
    pub latency: Latency,
    extra_headers: Vec<(String, String)>,
    pub flight: Option<Arc<Flight>>,
}

impl Script {
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
            extra_headers: Vec::new(),
            flight: None,
        }
    }

    #[must_use]
    pub fn reporting(mut self, flight: &Arc<Flight>) -> Self {
        self.flight = Some(Arc::clone(flight));
        self
    }

    #[must_use]
    pub fn refusing_tls(mut self) -> Self {
        self.refuses_tls = true;
        self
    }

    #[must_use]
    pub fn conditional(mut self, honors: bool) -> Self {
        self.honors_conditionals = honors;
        self
    }

    #[must_use]
    pub fn replying(mut self, replies: Vec<Reply>) -> Self {
        self.replies = replies;
        self
    }

    #[must_use]
    pub fn tagged(mut self, etags: Vec<String>) -> Self {
        self.etags = etags;
        self
    }

    #[must_use]
    pub fn ranges(mut self, accepts: bool) -> Self {
        self.accepts_ranges = accepts;
        self
    }

    #[must_use]
    pub fn delayed(mut self, latency: Latency) -> Self {
        self.latency = latency;
        self
    }

    #[must_use]
    pub fn headers(mut self, extra: Vec<(String, String)>) -> Self {
        self.extra_headers = extra;
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Received {
    pub method: String,
    pub target: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Received {
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(held, _)| held == name)
            .map(|(_, value)| value.as_str())
    }
}

#[derive(Debug)]
pub struct TestServer {
    address: SocketAddr,
    received: Arc<Mutex<Vec<Received>>>,
    stop: Arc<AtomicBool>,
}

impl TestServer {
    /// # Errors
    /// Whatever binding a loopback port reports.
    pub fn start(script: Script) -> std::io::Result<Self> {
        Self::start_on("127.0.0.1", script)
    }

    /// # Errors
    /// Whatever binding that address reports, and whatever asking the bound
    /// socket for its port reports.
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

    #[must_use]
    pub fn origin(&self) -> String {
        format!("http://{}", self.address)
    }

    #[must_use]
    pub fn secured_origin(&self) -> String {
        format!("https://{}", self.address)
    }

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
        let _counted = script.flight.as_ref().map(Flight::enter);
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
    let length: usize = headers
        .iter()
        .find(|(named, _)| named == "content-length")
        .and_then(|(_, value)| value.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0_u8; length];
    if length > 0 {
        reader.read_exact(&mut body).ok()?;
    }
    Some(Received {
        method,
        target,
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
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
    headers.extend(script.extra_headers.iter().cloned());
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

fn still_current(script: &Script, request: &Received, etag: Option<&str>) -> bool {
    if let Some(asked) = request.header("if-none-match") {
        return etag.is_some_and(|held| asked.split(',').any(|one| one.trim() == held));
    }
    if let Some(asked) = request.header("if-modified-since") {
        return script.last_modified.as_deref() == Some(asked);
    }
    false
}

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

const OBJECT_STORE_INDEX: &str = concat!(
    r#"<?xml version="1.0" encoding="UTF-8"?>"#,
    r#"<ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/">"#,
    "<Name>bucket</Name><Prefix>set/</Prefix><KeyCount>2</KeyCount>",
    "<IsTruncated>false</IsTruncated>",
    "<Contents><Key>set/one</Key><Size>11</Size></Contents>",
    "<Contents><Key>set/two</Key><Size>22</Size></Contents>",
    "</ListBucketResult>"
);

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
