//! a server on this machine that keeps objects as S3 does, for bucket mirrors: GET, HEAD and PUT of
//! an object, and uploads in parts. it checks each request's signature as S3 does and that its
//! body hashes to the x-amz-content-sha256 it was signed with, and refuses requests without a key.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex, PoisonError};

use hmac::{Hmac, KeyInit, Mac};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_decode_str, percent_encode};

use super::server::ServerTask;
use super::{Result, hex, sha256_hex};

/// what escapes in a signed path or query: everything but the unreserved characters.
const UNRESERVED: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~');

/// a private bucket's server, reached through `FETCHLOOM_TEST_HOSTS`, which keeps each object by
/// the path it was asked at.
pub(crate) struct S3 {
    pub(crate) url: String,
    kept: Arc<Mutex<Kept>>,
    _running: ServerTask,
}

#[derive(Default)]
struct Kept {
    secret: String,
    objects: HashMap<String, Vec<u8>>,
    /// open uploads in parts by id: the object's path and its parts by number.
    uploads: HashMap<String, (String, BTreeMap<u32, Vec<u8>>)>,
    started: u32,
    /// `METHOD target key` of each request, with `-` for one without a key.
    asked: Vec<String>,
    /// what a request got wrong, which no test expects.
    faults: Vec<String>,
    refused: Option<String>,
    failing: Option<u32>,
}

/// one request as it arrived.
struct Asked {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Asked {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

impl S3 {
    /// a bucket server that takes keys signed with `secret`.
    pub(crate) fn start(secret: &str) -> Result<S3> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let url = format!("http://{}", listener.local_addr()?);
        let kept = Arc::new(Mutex::new(Kept {
            secret: secret.to_owned(),
            ..Kept::default()
        }));
        let state = Arc::clone(&kept);
        let running = ServerTask::start(listener, move |listener, stop| {
            while let Some((mut stream, _connection)) = stop.accept(&listener) {
                let Some(asked) = read(&mut stream) else {
                    continue;
                };
                let answer = state
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .answer(&asked);
                let _ = stream.write_all(&answer);
                let _ = stream.flush();
            }
        })?;
        Ok(S3 {
            url,
            kept,
            _running: running,
        })
    }

    fn kept(&self) -> std::sync::MutexGuard<'_, Kept> {
        self.kept.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// refuses every request signed with the key id `key`, as a bucket policy that grants it
    /// nothing does.
    pub(crate) fn refuse(&self, key: &str) {
        self.kept().refused = Some(key.to_owned());
    }

    /// fails every upload of the part numbered `number` with a 500.
    pub(crate) fn fail_part(&self, number: u32) {
        self.kept().failing = Some(number);
    }

    /// `METHOD target key` of each request so far, `-` for a request without a key.
    pub(crate) fn asked(&self) -> Vec<String> {
        self.kept().asked.clone()
    }

    pub(crate) fn faults(&self) -> Vec<String> {
        self.kept().faults.clone()
    }

    pub(crate) fn object(&self, path: &str) -> Option<Vec<u8>> {
        self.kept().objects.get(path).cloned()
    }

    /// the paths of every object kept.
    pub(crate) fn paths(&self) -> Vec<String> {
        let mut paths: Vec<String> = self.kept().objects.keys().cloned().collect();
        paths.sort();
        paths
    }

    /// uploads in parts started and neither finished nor taken back.
    pub(crate) fn open_uploads(&self) -> usize {
        self.kept().uploads.len()
    }
}

impl Kept {
    fn answer(&mut self, asked: &Asked) -> Vec<u8> {
        let key = match self.signed_by(asked) {
            Ok(key) => key,
            Err(fault) => {
                self.faults.push(fault);
                return reply("403 Forbidden", &[], &error("SignatureDoesNotMatch"));
            }
        };
        self.asked.push(format!(
            "{} {} {}",
            asked.method,
            asked.target,
            key.as_deref().unwrap_or("-")
        ));
        let Some(key) = key else {
            return reply(
                "403 Forbidden",
                &[("x-amz-bucket-region", "us-east-1")],
                &error("AccessDenied"),
            );
        };
        if self.refused.as_deref() == Some(key.as_str()) {
            return reply("403 Forbidden", &[], &error("AccessDenied"));
        }
        let payload = asked.header("x-amz-content-sha256").unwrap_or_default();
        if payload != sha256_hex(&asked.body) {
            self.faults.push(format!(
                "{} {}: the body does not hash to {payload}",
                asked.method, asked.target
            ));
            return reply("400 Bad Request", &[], &error("XAmzContentSHA256Mismatch"));
        }
        let (path, query) = asked
            .target
            .split_once('?')
            .unwrap_or((asked.target.as_str(), ""));
        let pairs: HashMap<&str, &str> = query
            .split('&')
            .filter(|pair| !pair.is_empty())
            .map(|pair| pair.split_once('=').unwrap_or((pair, "")))
            .collect();
        match (asked.method.as_str(), pairs.get("uploadId")) {
            ("GET", None) => match self.objects.get(path) {
                Some(body) => reply("200 OK", &[], body),
                None => reply("404 Not Found", &[], &error("NoSuchKey")),
            },
            ("HEAD", None) => match self.objects.get(path) {
                Some(body) => {
                    let length = body.len().to_string();
                    head("200 OK", &[("Content-Length", &length)])
                }
                None => head("404 Not Found", &[("Content-Length", "0")]),
            },
            ("PUT", None) => {
                self.objects.insert(path.to_owned(), asked.body.clone());
                reply(
                    "200 OK",
                    &[("ETag", &format!("\"{}\"", &payload[..32]))],
                    b"",
                )
            }
            ("POST", None) if pairs.contains_key("uploads") => {
                self.started += 1;
                let id = format!("upload{}", self.started);
                self.uploads
                    .insert(id.clone(), (path.to_owned(), BTreeMap::new()));
                let body = format!(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<InitiateMultipartUploadResult><Bucket>b</Bucket><Key>k</Key><UploadId>{id}</UploadId></InitiateMultipartUploadResult>"
                );
                reply("200 OK", &[], body.as_bytes())
            }
            ("PUT", Some(id)) => {
                let number = pairs
                    .get("partNumber")
                    .and_then(|number| number.parse().ok())
                    .unwrap_or_default();
                self.part(id, number, asked, payload)
            }
            ("POST", Some(id)) => self.complete(id, asked),
            ("DELETE", Some(id)) => {
                self.uploads.remove(*id);
                reply("204 No Content", &[], b"")
            }
            _ => reply("400 Bad Request", &[], &error("InvalidRequest")),
        }
    }

    /// keeps part `number` of the upload `id`, unless that number is set to fail.
    fn part(&mut self, id: &str, number: u32, asked: &Asked, payload: &str) -> Vec<u8> {
        if self.failing == Some(number) {
            return reply("500 Internal Server Error", &[], &error("InternalError"));
        }
        let Some((_, parts)) = self.uploads.get_mut(id) else {
            return reply("404 Not Found", &[], &error("NoSuchUpload"));
        };
        parts.insert(number, asked.body.clone());
        reply(
            "200 OK",
            &[("ETag", &format!("\"{}\"", &payload[..32]))],
            b"",
        )
    }

    /// joins the parts of the upload `id` that `asked` lists, each in its place, into its object.
    fn complete(&mut self, id: &str, asked: &Asked) -> Vec<u8> {
        let Some((path, parts)) = self.uploads.remove(id) else {
            return reply("404 Not Found", &[], &error("NoSuchUpload"));
        };
        let document = String::from_utf8_lossy(&asked.body).into_owned();
        let mut whole = Vec::new();
        for (index, part) in document.split("<Part>").skip(1).enumerate() {
            let named: Option<u32> = between(part, "PartNumber").and_then(|text| text.parse().ok());
            let place = u32::try_from(index + 1).ok();
            match named.and_then(|number| parts.get(&number)) {
                Some(body) if named == place => whole.extend_from_slice(body),
                _ => {
                    self.faults
                        .push(format!("a part out of place in {document}"));
                    return reply("400 Bad Request", &[], &error("InvalidPart"));
                }
            }
        }
        self.objects.insert(path, whole);
        reply(
            "200 OK",
            &[],
            b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<CompleteMultipartUploadResult><Key>k</Key></CompleteMultipartUploadResult>",
        )
    }

    /// the key id that signed `asked`, checked as S3 checks it, or none for a request without
    /// one; what is wrong with a signature that does not hold.
    fn signed_by(&self, asked: &Asked) -> std::result::Result<Option<String>, String> {
        let Some(authorization) = asked.header("authorization") else {
            return Ok(None);
        };
        let wrong = |what: &str| format!("{} {}: {what}", asked.method, asked.target);
        let rest = authorization
            .strip_prefix("AWS4-HMAC-SHA256 Credential=")
            .ok_or_else(|| wrong("not a signature"))?;
        let (credential, rest) = rest
            .split_once(", SignedHeaders=")
            .ok_or_else(|| wrong("no signed headers"))?;
        let (names, signature) = rest
            .split_once(", Signature=")
            .ok_or_else(|| wrong("no signature"))?;
        let (key, scope) = credential
            .split_once('/')
            .ok_or_else(|| wrong("no scope"))?;
        let names: Vec<&str> = names.split(';').collect();
        for needed in ["host", "x-amz-content-sha256", "x-amz-date"] {
            if !names.contains(&needed) {
                return Err(wrong(&format!("{needed} is not signed")));
            }
        }
        let (path, query) = asked
            .target
            .split_once('?')
            .unwrap_or((asked.target.as_str(), ""));
        let path: Vec<String> = path.split('/').map(escaped).collect();
        let mut pairs: Vec<(String, String)> = query
            .split('&')
            .filter(|pair| !pair.is_empty())
            .map(|pair| {
                let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
                (escaped(name), escaped(value))
            })
            .collect();
        pairs.sort();
        let query: Vec<String> = pairs
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect();
        let mut canonical = format!(
            "{}\n{}\n{}\n",
            asked.method,
            path.join("/"),
            query.join("&")
        );
        for name in &names {
            let value = asked
                .header(name)
                .ok_or_else(|| wrong(&format!("no {name} header")))?;
            let _ = writeln!(canonical, "{name}:{}", value.trim());
        }
        let payload = asked.header("x-amz-content-sha256").unwrap_or_default();
        let _ = write!(canonical, "\n{}\n{payload}", names.join(";"));
        let date = asked.header("x-amz-date").unwrap_or_default();
        let to_sign = format!(
            "AWS4-HMAC-SHA256\n{date}\n{scope}\n{}",
            sha256_hex(canonical.as_bytes())
        );
        let mut signing = format!("AWS4{}", self.secret).into_bytes();
        for part in scope.split('/') {
            signing = mac(&signing, part);
        }
        if hex(&mac(&signing, &to_sign)) != signature {
            return Err(wrong(&format!(
                "the signature does not hold for\n{canonical}"
            )));
        }
        Ok(Some(key.to_owned()))
    }
}

/// the text between `<name>` and `</name>` in `text`.
fn between<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let start = text.find(&format!("<{name}>"))? + name.len() + 2;
    let end = text[start..].find(&format!("</{name}>"))? + start;
    Some(&text[start..end])
}

fn escaped(text: &str) -> String {
    let bytes: Vec<u8> = percent_decode_str(text).collect();
    percent_encode(&bytes, UNRESERVED).to_string()
}

/// the HMAC-SHA256 of `text` with `key`, which takes a key of any length.
fn mac(key: &[u8], text: &str) -> Vec<u8> {
    let Ok(mut mac) = <Hmac<sha2::Sha256> as KeyInit>::new_from_slice(key) else {
        return Vec::new();
    };
    mac.update(text.as_bytes());
    mac.finalize().into_bytes().to_vec()
}

/// S3's error document for `code`.
fn error(code: &str) -> Vec<u8> {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Error><Code>{code}</Code><Message>{code}</Message></Error>"
    )
    .into_bytes()
}

/// reads one request, its head and its `Content-Length` body.
fn read(stream: &mut std::net::TcpStream) -> Option<Asked> {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        match stream.read(&mut byte) {
            Ok(1) => head.push(byte[0]),
            _ => return None,
        }
    }
    let head = String::from_utf8_lossy(&head).into_owned();
    let mut lines = head.lines();
    let mut first = lines.next()?.split(' ');
    let (method, target) = (first.next()?.to_owned(), first.next()?.to_owned());
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    let length: usize = headers
        .iter()
        .find(|(name, _)| name == "content-length")
        .and_then(|(_, value)| value.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0; length];
    stream.read_exact(&mut body).ok()?;
    Some(Asked {
        method,
        target,
        headers,
        body,
    })
}

/// a whole answer: its status, headers and exactly `body`.
fn reply(status: &str, headers: &[(&str, &str)], body: &[u8]) -> Vec<u8> {
    let length = body.len().to_string();
    let mut all = vec![("Content-Length", length.as_str())];
    all.extend_from_slice(headers);
    let mut bytes = head(status, &all);
    bytes.extend_from_slice(body);
    bytes
}

/// a status line and headers alone, as a HEAD is answered.
fn head(status: &str, headers: &[(&str, &str)]) -> Vec<u8> {
    let mut head = format!("HTTP/1.1 {status}\r\nConnection: close\r\n");
    for (name, value) in headers {
        let _ = write!(head, "{name}: {value}\r\n");
    }
    head.push_str("\r\n");
    head.into_bytes()
}
