//! The source that fetches over HTTP and HTTPS.

use std::collections::HashMap;
use std::io::Read;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use fetchloom_engine::credential::{Credential, Necessity};
use fetchloom_engine::degrade::{Degradation, DegradeQueue};
use fetchloom_engine::error::{Error, ErrorKind};
use fetchloom_engine::redact::SafeUrl;
use fetchloom_engine::reference::Host;
use fetchloom_engine::seam::source::{
    ByteRange, Cost, Listing, Revalidated, Served, Serves, Source, SourceIdentity, SourceMetadata,
    Validator,
};

use fetchloom_engine::limits::Limits;
use fetchloom_engine::work::WorkCounter;

use crate::index;
use crate::origin::Origin;

/// The bytes of one object, streamed as they arrive.
pub struct HttpBody {
    reader: Box<dyn Read + Send + Sync>,
}

impl HttpBody {
    /// Wraps a reader as the streamed bytes of one object.
    pub(crate) fn new(reader: impl Read + Send + Sync + 'static) -> Self {
        Self {
            reader: Box::new(reader),
        }
    }
}

impl std::fmt::Debug for HttpBody {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("HttpBody")
    }
}

impl Read for HttpBody {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.reader.read(buffer)
    }
}

/// A source reached over HTTP or HTTPS.
#[derive(Default)]
pub struct HttpSource {
    agents: Mutex<HashMap<String, ureq::Agent>>,
    degradations: DegradeQueue,
    limits: Limits,
    work: Arc<WorkCounter>,
}

impl HttpSource {
    /// Builds a source holding no connections yet.
    #[must_use]
    pub fn new(limits: Limits, work: Arc<WorkCounter>) -> Self {
        Self {
            agents: Mutex::new(HashMap::new()),
            degradations: DegradeQueue::new(),
            limits,
            work,
        }
    }

    fn agent(&self, host: &str) -> ureq::Agent {
        let limits = self.limits;
        let mut held = self.agents.lock().unwrap_or_else(PoisonError::into_inner);
        held.entry(host.to_owned())
            .or_insert_with(|| build_agent(&limits))
            .clone()
    }

    /// Returns the bounds this source was built with.
    pub(crate) fn limits(&self) -> Limits {
        self.limits
    }

    pub(crate) fn send(
        &self,
        method: Method,
        location: &str,
        range: Option<ByteRange>,
        credential: Option<&Credential>,
    ) -> Result<(ureq::http::Response<ureq::Body>, String), Error> {
        self.send_conditional(method, location, range, credential, &Validator::default())
    }

    pub(crate) fn send_conditional(
        &self,
        method: Method,
        location: &str,
        range: Option<ByteRange>,
        credential: Option<&Credential>,
        asking: &Validator,
    ) -> Result<(ureq::http::Response<ureq::Body>, String), Error> {
        fetchloom_engine::network::allowed(location)?;
        let start = Origin::of(location)?;
        let mut current = location.to_owned();
        let mut carried = credential;

        for _ in 0..=self.limits.redirects {
            let here = Origin::of(&current)?;
            if carried.is_some() && here != start {
                self.degradations.record(
                    "the credential sent to the source that was asked for",
                    "no credential",
                    format!(
                        "the request was redirected from {start} to {here}, which is another origin, so the credential was dropped"
                    ),
                );
                carried = None;
            }

            let agent = self.agent(here.host());
            let mut request = match method {
                Method::Head => agent.head(&current),
                Method::Get => agent.get(&current),
            };
            if let Some(credential) = carried {
                for (name, value) in authorizing_headers(credential, &current, here.host(), method)
                {
                    request = request.header(name, &value);
                }
            }
            if let Some(range) = range {
                request = request.header(
                    "Range",
                    &format!("bytes={}-{}", range.start, range.end.saturating_sub(1)),
                );
            }
            if let Some(tag) = asking.etag.as_deref() {
                request = request.header("If-None-Match", tag);
            }
            if let Some(since) = asking.last_modified.as_deref() {
                request = request.header("If-Modified-Since", since);
            }
            self.work.issued_request();
            let answer = request
                .call()
                .map_err(|reason| transport_failure(&current, &reason))?;

            let status = answer.status().as_u16();
            let Some(next) = redirect_target(&answer, status) else {
                return Ok((answer, current));
            };
            current = Origin::join(&current, &next)?;
        }

        Err(Error::new(
            ErrorKind::ResourceLimit,
            format!(
                "ask for a location that settles, because the source redirected more than the {} times a run follows",
                self.limits.redirects
            ),
        )
        .with_source(location))
    }
}

/// Which request a send is making.
#[derive(Clone, Copy)]
pub(crate) enum Method {
    /// A bounded metadata request.
    Head,
    /// A request for bytes.
    Get,
}

/// Splits a location into the path and query a signature is computed over.
fn path_and_query(location: &str) -> (String, String) {
    let after_scheme = location
        .split_once("://")
        .map_or(location, |(_, rest)| rest);
    let from_path = after_scheme.find('/').map_or("/", |at| &after_scheme[at..]);
    let without_fragment = from_path.split('#').next().unwrap_or(from_path);
    match without_fragment.split_once('?') {
        Some((path, query)) => (path.to_owned(), query.to_owned()),
        None => (without_fragment.to_owned(), String::new()),
    }
}

/// Returns the headers whatever the credential's shape authorizes a request
/// with, which is a value the credential supplies or a signature computed over
/// the request itself.
fn authorizing_headers(
    credential: &Credential,
    location: &str,
    host: &str,
    method: Method,
) -> Vec<(&'static str, String)> {
    if let Some(value) = credential.bearer() {
        return vec![("Authorization", value.to_owned())];
    }
    let Some(keys) = credential.signing() else {
        return Vec::new();
    };
    let (path, query) = path_and_query(location);
    let signed = crate::signing::sign(
        keys,
        "s3",
        &crate::signing::Request {
            method: match method {
                Method::Head => "HEAD",
                Method::Get => "GET",
            },
            path: &path,
            query: &query,
            host,
            payload: crate::signing::EMPTY_PAYLOAD,
        },
        &crate::signing::SigningTime::now(),
    );
    let mut headers = vec![
        ("Authorization", signed.authorization),
        ("x-amz-date", signed.date),
        ("x-amz-content-sha256", signed.content_digest),
    ];
    if let Some(token) = signed.security_token {
        headers.push(("x-amz-security-token", token));
    }
    headers
}

fn build_agent(limits: &Limits) -> ureq::Agent {
    let _ = rustls_graviola::default_provider().install_default();
    ureq::Agent::config_builder()
        .max_redirects(0)
        .http_status_as_error(false)
        .timeout_connect(Some(limits.connect_timeout))
        .timeout_recv_response(Some(limits.response_timeout))
        .timeout_recv_body(Some(limits.idle_timeout))
        .max_idle_connections_per_host(limits.connections_per_host)
        .max_idle_age(limits.idle_connection_age)
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .build(),
        )
        .build()
        .into()
}

fn redirect_target<T>(answer: &ureq::http::Response<T>, status: u16) -> Option<String> {
    if !matches!(status, 301 | 302 | 303 | 307 | 308) {
        return None;
    }
    header(answer, "location")
}

/// The fields common to every metadata response, independent of what
/// identifies the bytes or who is billed for them.
pub(crate) struct CommonFields {
    /// The length in bytes, when the source states it.
    pub size: Option<u64>,
    /// The last modified value the source gave, when it gave one.
    pub last_modified: Option<String>,
    /// Whether the source can serve part of the object.
    pub supports_ranges: bool,
    /// How long the source asked to be left alone for, when it asked.
    pub retry_after: Option<Duration>,
}

/// Reads the fields common to every metadata response out of one answer.
pub(crate) fn common_fields<T>(answer: &ureq::http::Response<T>) -> CommonFields {
    CommonFields {
        size: header(answer, "content-length").and_then(|value| value.parse().ok()),
        last_modified: header(answer, "last-modified"),
        supports_ranges: header(answer, "accept-ranges")
            .is_some_and(|value| value.split(',').any(|unit| unit.trim() == "bytes")),
        retry_after: header(answer, "retry-after")
            .as_deref()
            .and_then(parse_retry_after),
    }
}

/// Checks a fetch response against the range it was asked to serve.
///
/// # Errors
///
/// Fails when the source could not serve the span, refused the request, or
/// answered with a different span than the one asked for.
pub(crate) fn check_fetch_status(
    location: &str,
    range: Option<ByteRange>,
    answer: &ureq::http::Response<ureq::Body>,
    credential: Option<&Credential>,
) -> Result<(), Error> {
    let status = answer.status().as_u16();
    if status == 416 {
        return Err(Error::new(
            ErrorKind::SourceUnsupportedRange,
            format!(
                "ask for the whole object, because the source cannot serve the span that was asked for and reported its length as {}",
                header(answer, "content-range").unwrap_or_else(|| "unknown".to_owned())
            ),
        )
        .with_source(location));
    }
    if !(200..300).contains(&status) {
        return Err(status_failure(
            location,
            status,
            header(answer, "retry-after").as_deref(),
            credential,
        ));
    }
    if let Some(range) = range {
        if status != 206 {
            return Err(Error::new(
                ErrorKind::SourceUnsupportedRange,
                "start the transfer from zero, because the source ignored the range that was asked for and answered with the whole object",
            )
            .with_source(location));
        }
        let first = header(answer, "content-range").and_then(|value| first_byte_of(&value));
        if first != Some(range.start) {
            return Err(Error::new(
                ErrorKind::IntegrityRangeMismatch,
                format!(
                    "name a source that serves the span it is asked for, because this one was asked for the bytes from {} and answered with {}",
                    range.start,
                    header(answer, "content-range").unwrap_or_else(|| "no span at all".to_owned())
                ),
            )
            .with_source(location));
        }
    }
    Ok(())
}

impl Source for HttpSource {
    type Body = HttpBody;

    fn serves(&self, reference: &str) -> Option<Serves> {
        (is_over_http(reference) && !reference.ends_with('/')).then_some(Serves::Object)
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
        let (answer, served) = self.send(Method::Head, location, None, credential)?;
        let time_to_first_byte = started.elapsed();
        let status = answer.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(status_failure(
                location,
                status,
                header(&answer, "retry-after").as_deref(),
                credential,
            ));
        }

        let fields = common_fields(&answer);
        Ok(SourceMetadata {
            location: SafeUrl::new(&served),
            host: Host::new(Origin::of(&served)?.host()),
            size: fields.size,
            content: None,
            interop: None,
            identity: identity_of(header(&answer, "etag").as_deref()),
            last_modified: fields.last_modified,
            supports_ranges: fields.supports_ranges,
            time_to_first_byte,
            retry_after: fields.retry_after,
            cost: Cost::default(),
        })
    }

    fn revalidate(
        &self,
        location: &str,
        validator: &Validator,
        credential: Option<&Credential>,
    ) -> Result<Revalidated<Self::Body>, Error> {
        let started = Instant::now();
        let (answer, served) =
            self.send_conditional(Method::Get, location, None, credential, validator)?;
        let time_to_first_byte = started.elapsed();
        let status = answer.status().as_u16();
        if status == 304 {
            return Ok(Revalidated::Unchanged);
        }
        if !(200..300).contains(&status) {
            return Err(status_failure(
                location,
                status,
                header(&answer, "retry-after").as_deref(),
                credential,
            ));
        }
        let fields = common_fields(&answer);
        let metadata = SourceMetadata {
            location: SafeUrl::new(&served),
            host: Host::new(Origin::of(&served)?.host()),
            size: fields.size,
            content: None,
            interop: None,
            identity: identity_of(header(&answer, "etag").as_deref()),
            last_modified: fields.last_modified,
            supports_ranges: fields.supports_ranges,
            time_to_first_byte,
            retry_after: fields.retry_after,
            cost: Cost::default(),
        };
        Ok(Revalidated::Changed(Box::new(Served {
            metadata,
            body: HttpBody::new(answer.into_body().into_reader()),
        })))
    }

    fn fetch(
        &self,
        location: &str,
        range: Option<ByteRange>,
        credential: Option<&Credential>,
    ) -> Result<Served<Self::Body>, Error> {
        let started = Instant::now();
        let (answer, served) = self.send(Method::Get, location, range, credential)?;
        let time_to_first_byte = started.elapsed();
        check_fetch_status(location, range, &answer, credential)?;

        let fields = common_fields(&answer);
        Ok(Served {
            metadata: SourceMetadata {
                location: SafeUrl::new(&served),
                host: Host::new(Origin::of(&served)?.host()),
                size: fields.size,
                content: None,
                interop: None,
                identity: identity_of(header(&answer, "etag").as_deref()),
                last_modified: fields.last_modified,
                supports_ranges: fields.supports_ranges,
                time_to_first_byte,
                retry_after: fields.retry_after,
                cost: Cost::default(),
            },
            body: HttpBody::new(answer.into_body().into_reader()),
        })
    }

    fn list(&self, location: &str, credential: Option<&Credential>) -> Result<Listing, Error> {
        let (answer, _served) = self.send(Method::Get, location, None, credential)?;
        let status = answer.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(status_failure(
                location,
                status,
                header(&answer, "retry-after").as_deref(),
                credential,
            ));
        }
        let body = answer
            .into_body()
            .into_with_config()
            .limit(self.limits.listing_bytes)
            .read_to_string()
            .map_err(|reason| transport_failure(location, &reason))?;
        let listing = index::parse(location, status, &body)?;
        let allowed = usize::try_from(self.limits.listing_entries).unwrap_or(usize::MAX);
        if listing.entries.len() > allowed {
            return Err(Error::new(
                ErrorKind::ResourceLimit,
                format!(
                    "ask for a narrower prefix, because the index at {} lists more than the {} entries a run reads",
                    SafeUrl::new(location),
                    self.limits.listing_entries
                ),
            )
            .with_source(location));
        }
        Ok(listing)
    }
}

pub(crate) fn header<T>(answer: &ureq::http::Response<T>, name: &str) -> Option<String> {
    answer
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

pub(crate) fn identity_of(etag: Option<&str>) -> SourceIdentity {
    match etag {
        Some(tag) if tag.starts_with("W/") => SourceIdentity::WeakValidator(tag.to_owned()),
        Some(tag) => SourceIdentity::StrongValidator(tag.to_owned()),
        None => SourceIdentity::None,
    }
}

fn parse_retry_after(value: &str) -> Option<Duration> {
    value.trim().parse().ok().map(Duration::from_secs)
}

pub(crate) fn status_failure(
    location: &str,
    status: u16,
    retry_after: Option<&str>,
    presented: Option<&Credential>,
) -> Error {
    if let Some(credential) = presented
        && matches!(status, 401 | 403)
    {
        return Error::new(
            ErrorKind::PolicyCredentialInvalid,
            format!(
                "renew the credential for {} or widen its scope, because the source answered {status} to the one it was given",
                credential.host
            ),
        )
        .with_source(location);
    }
    if matches!(status, 401 | 403) {
        let host =
            Origin::of(location).map_or_else(|_| String::new(), |origin| origin.host().to_owned());
        let help = crate::help::help_for(&host, Necessity::Required);
        return Error::new(
            ErrorKind::PolicyCredentialMissing,
            format!(
                "{}, because {} answered {status} without one",
                help.placement,
                SafeUrl::new(location)
            ),
        )
        .with_source(location);
    }
    let retryable = matches!(status, 408 | 429 | 500 | 502 | 503 | 504);
    let action = match &retry_after {
        Some(wait) => format!(
            "the source answered {status} and asked to be left alone for {wait} seconds before another request"
        ),
        None if retryable => {
            format!(
                "try the source again, because it answered {status}, which another attempt may get past"
            )
        }
        None => format!(
            "name a source that can serve the object, because this one answered {status}, which will not change without the source or the request changing"
        ),
    };
    let failure = Error::new(ErrorKind::NetworkStatus, action)
        .with_source(location)
        .with_retryable(retryable);
    let asked = retry_after
        .and_then(parse_retry_after)
        .map(|wait| failure.clone().with_retry_after(wait));
    match asked {
        Some(carrying) => carrying,
        None if status == 429 => failure.rate_limited(),
        None => failure,
    }
}

pub(crate) fn transport_failure(location: &str, reason: &ureq::Error) -> Error {
    let (kind, retryable, action) = classify(reason);
    Error::new(kind, format!("{action}: {}", detail(reason)))
        .with_source(location)
        .with_retryable(retryable)
}

/// What a transport failure may say about itself.
///
/// The transport writes the location it was given into several of its own
/// messages, so its text is never interpolated. Each variant that says
/// something about the connection rather than about the address says it here,
/// and every other one says nothing.
fn detail(reason: &ureq::Error) -> String {
    match reason {
        ureq::Error::Io(socket) => socket.to_string(),
        ureq::Error::Timeout(what) => what.to_string(),
        ureq::Error::HostNotFound => "the host name resolved to no address".to_owned(),
        ureq::Error::ConnectionFailed => "the connection could not be opened".to_owned(),
        ureq::Error::TooManyRedirects => "the source redirected too many times".to_owned(),
        ureq::Error::RedirectFailed => "the redirect could not be followed".to_owned(),
        ureq::Error::TlsRequired => "the transport was unsecured".to_owned(),
        ureq::Error::BodyExceedsLimit(limit) => {
            format!("the body is larger than the {limit} byte limit")
        }
        ureq::Error::LargeResponseHeader(seen, allowed) => {
            format!("a response header of {seen} bytes is over the {allowed} allowed")
        }
        ureq::Error::Rustls(why) => why.to_string(),
        _ => "the transport gave no reason that is safe to repeat".to_owned(),
    }
}

/// What to tell someone whose host name went nowhere.
const UNRESOLVED: &str =
    "check the host name, because nothing on this machine can resolve it to an address";

/// What to tell someone whose request ran out of time.
const RAN_OUT: &str = "try the source again, because the request ran out of time";

/// Decides what a transport failure is and whether another attempt could work.
fn classify(reason: &ureq::Error) -> (ErrorKind, bool, &'static str) {
    match reason {
        ureq::Error::Timeout(_) => (ErrorKind::NetworkTimeout, true, RAN_OUT),
        ureq::Error::Tls(_) | ureq::Error::Pem(_) | ureq::Error::Rustls(_) => (
            ErrorKind::NetworkTls,
            false,
            "trust the source or name one you already trust, because the connection could not be secured",
        ),
        ureq::Error::HostNotFound => (ErrorKind::NetworkRefused, false, UNRESOLVED),
        ureq::Error::Io(socket) if unsecurable(socket) => (
            ErrorKind::NetworkTls,
            false,
            "trust the source or name one you already trust, because the connection could not be secured",
        ),
        ureq::Error::Io(socket) if unresolvable(socket) => {
            (ErrorKind::NetworkRefused, false, UNRESOLVED)
        }
        ureq::Error::Io(socket) if socket.kind() == std::io::ErrorKind::TimedOut => {
            (ErrorKind::NetworkTimeout, true, RAN_OUT)
        }
        _ => (
            ErrorKind::NetworkRefused,
            true,
            "try the source again, because the request did not complete",
        ),
    }
}

/// Returns whether a socket failure was the handshake and not the socket.
fn unsecurable(reason: &std::io::Error) -> bool {
    reason
        .get_ref()
        .is_some_and(|held| held.downcast_ref::<rustls::Error>().is_some())
}

/// Returns whether a name lookup failed in a way another attempt cannot fix.
fn unresolvable(reason: &std::io::Error) -> bool {
    if matches!(reason.raw_os_error(), Some(11_001 | 11_004)) {
        return true;
    }
    let text = reason.to_string();
    text.contains("failed to lookup address information")
        && !text.contains("Temporary failure in name resolution")
}

/// Reads the first byte offset a partial response says it is serving.
fn first_byte_of(value: &str) -> Option<u64> {
    value
        .trim()
        .strip_prefix("bytes ")?
        .split('-')
        .next()?
        .trim()
        .parse()
        .ok()
}

/// Reports whether a reference names a location reached over HTTP.
pub(crate) fn is_over_http(reference: &str) -> bool {
    reference.starts_with("http://") || reference.starts_with("https://")
}

/// Reports whether this platform's certificate trust store can be loaded, which
/// is what a source needs before it can secure a connection.
///
/// Makes no request, so it answers the same question offline.
#[must_use]
pub fn trust_store_loads() -> bool {
    let _ = rustls_graviola::default_provider().install_default();
    ureq::Agent::config_builder()
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .build(),
        )
        .build();
    true
}
