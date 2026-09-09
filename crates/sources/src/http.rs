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

pub struct HttpBody {
    reader: Box<dyn Read + Send + Sync>,
}

impl HttpBody {
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

#[derive(Default)]
pub struct HttpSource {
    agents: Mutex<HashMap<String, ureq::Agent>>,
    degradations: DegradeQueue,
    limits: Limits,
    work: Arc<WorkCounter>,
}

impl HttpSource {
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
        self.resuming(method, location, range, credential, None)
    }

    pub(crate) fn resuming(
        &self,
        method: Method,
        location: &str,
        range: Option<ByteRange>,
        credential: Option<&Credential>,
        resuming: Option<&str>,
    ) -> Result<(ureq::http::Response<ureq::Body>, String), Error> {
        self.sent(Sending {
            method,
            location,
            range,
            credential,
            asking: &Validator::default(),
            body: None,
            resuming,
        })
    }

    pub(crate) fn send_conditional(
        &self,
        method: Method,
        location: &str,
        range: Option<ByteRange>,
        credential: Option<&Credential>,
        asking: &Validator,
    ) -> Result<(ureq::http::Response<ureq::Body>, String), Error> {
        self.sent(Sending {
            method,
            location,
            range,
            credential,
            asking,
            body: None,
            resuming: None,
        })
    }

    pub(crate) fn send_json(
        &self,
        location: &str,
        body: &str,
        credential: Option<&Credential>,
    ) -> Result<(ureq::http::Response<ureq::Body>, String), Error> {
        self.sent(Sending {
            method: Method::Post,
            location,
            range: None,
            credential,
            asking: &Validator::default(),
            body: Some(body),
            resuming: None,
        })
    }

    fn sent(
        &self,
        sending: Sending<'_>,
    ) -> Result<(ureq::http::Response<ureq::Body>, String), Error> {
        let Sending {
            method,
            location,
            range,
            credential,
            asking,
            body,
            resuming,
        } = sending;
        fetchloom_engine::network::allowed(location)?;
        let start = Origin::of(location)?;
        let mut current = location.to_owned();
        let mut carried = credential;

        let mut followed = false;
        for _ in 0..=self.limits.redirects {
            let here = Origin::of(&current)?;
            if followed && !(stays_on_this_machine(&here) && stays_on_this_machine(&start)) {
                check_addresses(&here)?;
            }
            if start.secured() && !here.secured() {
                return Err(Error::new(
                    ErrorKind::NetworkTls,
                    format!(
                        "ask the source for a location that stays on https, because the request for {start} was redirected to {here}, which leaves TLS, and a run does not continue in the clear"
                    ),
                )
                .with_source(location));
            }
            if carried.is_some() && !here.secured() && !stays_on_this_machine(&here) {
                return Err(Error::new(
                    ErrorKind::PolicyCredentialInvalid,
                    format!(
                        "reach it as https://{} instead, because a credential was resolved for that host and {here} is not secured, so the credential would travel in the clear",
                        here.host()
                    ),
                )
                .with_source(location));
            }
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
            let asked = Asked {
                credential: carried,
                location: &current,
                host: here.host(),
                method,
                range,
                asking,
                resuming,
            };
            self.work.issued_request();
            let answer = match method {
                Method::Head => decorated(agent.head(&current), &asked).call(),
                Method::Get => decorated(agent.get(&current), &asked).call(),
                Method::Post => decorated(agent.post(&current), &asked)
                    .header("Content-Type", "application/json")
                    .send(body.unwrap_or_default()),
            }
            .map_err(|reason| transport_failure(&current, &reason))?;

            let status = answer.status().as_u16();
            let Some(next) = redirect_target(&answer, status) else {
                return Ok((answer, current));
            };
            current = Origin::join(&current, &next)?;
            followed = true;
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

#[derive(Clone, Copy)]
struct Sending<'a> {
    method: Method,
    location: &'a str,
    range: Option<ByteRange>,
    credential: Option<&'a Credential>,
    asking: &'a Validator,
    body: Option<&'a str>,
    resuming: Option<&'a str>,
}

struct Asked<'a> {
    credential: Option<&'a Credential>,
    location: &'a str,
    host: &'a str,
    method: Method,
    range: Option<ByteRange>,
    asking: &'a Validator,
    resuming: Option<&'a str>,
}

fn decorated<Any>(
    mut request: ureq::RequestBuilder<Any>,
    asked: &Asked<'_>,
) -> ureq::RequestBuilder<Any> {
    if let Some(credential) = asked.credential {
        for (name, value) in
            authorizing_headers(credential, asked.location, asked.host, asked.method)
        {
            request = request.header(name, &value);
        }
    }
    if let Some(range) = asked.range {
        request = request.header(
            "Range",
            &format!("bytes={}-{}", range.start, range.end.saturating_sub(1)),
        );
    }
    if let Some(tag) = asked.resuming
        && asked.range.is_some()
    {
        request = request.header("If-Range", tag);
    }
    if let Some(tag) = asked.asking.etag.as_deref() {
        request = request.header("If-None-Match", tag);
    }
    if let Some(since) = asked.asking.last_modified.as_deref() {
        request = request.header("If-Modified-Since", since);
    }
    request
}

#[derive(Clone, Copy)]
pub(crate) enum Method {
    Head,
    Get,
    Post,
}

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
                Method::Post => "POST",
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
    let config = ureq::Agent::config_builder()
        .max_redirects(0)
        .user_agent(USER_AGENT)
        .http_status_as_error(false)
        .timeout_connect(Some(limits.connect_timeout))
        .timeout_recv_response(Some(limits.response_timeout))
        .timeout_recv_body(Some(limits.idle_timeout))
        .max_idle_connections_per_host(
            usize::try_from(fetchloom_engine::tuning::TRANSFERS_CEILING)
                .unwrap_or(usize::MAX)
                .max(limits.connections_per_host),
        )
        .max_idle_age(limits.idle_connection_age)
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .build(),
        )
        .build();
    ureq::Agent::new_with_config(config)
}

const USER_AGENT: &str = concat!("fetchloom/", env!("CARGO_PKG_VERSION"));

fn redirect_target<T>(answer: &ureq::http::Response<T>, status: u16) -> Option<String> {
    if !matches!(status, 301 | 302 | 303 | 307 | 308) {
        return None;
    }
    header(answer, "location")
}

pub(crate) struct CommonFields {
    pub size: Option<u64>,
    pub last_modified: Option<String>,
    pub supports_ranges: bool,
    pub retry_after: Option<Duration>,
}

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
        resuming: Option<&str>,
    ) -> Result<Served<Self::Body>, Error> {
        let started = Instant::now();
        let (answer, served) = self.resuming(Method::Get, location, range, credential, resuming)?;
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

const UNRESOLVED: &str =
    "check the host name, because nothing on this machine can resolve it to an address";

const RAN_OUT: &str = "try the source again, because the request ran out of time";

const OVERSIZED: &str =
    "ask for a narrower prefix, because the answer is larger than the bound this run reads";

fn classify(reason: &ureq::Error) -> (ErrorKind, bool, &'static str) {
    match reason {
        ureq::Error::Timeout(_) => (ErrorKind::NetworkTimeout, true, RAN_OUT),
        ureq::Error::Tls(_) | ureq::Error::Pem(_) | ureq::Error::Rustls(_) => (
            ErrorKind::NetworkTls,
            false,
            "trust the source or name one you already trust, because the connection could not be secured",
        ),
        ureq::Error::HostNotFound => (ErrorKind::NetworkRefused, false, UNRESOLVED),
        ureq::Error::BodyExceedsLimit(_) => (ErrorKind::ResourceLimit, false, OVERSIZED),
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

fn unsecurable(reason: &std::io::Error) -> bool {
    reason
        .get_ref()
        .is_some_and(|held| held.downcast_ref::<rustls::Error>().is_some())
}

fn unresolvable(reason: &std::io::Error) -> bool {
    if matches!(reason.raw_os_error(), Some(11_001 | 11_004)) {
        return true;
    }
    let text = reason.to_string();
    text.contains("failed to lookup address information")
        && !text.contains("Temporary failure in name resolution")
}

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

pub(crate) fn is_over_http(reference: &str) -> bool {
    reference.starts_with("http://") || reference.starts_with("https://")
}

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

fn check_addresses(origin: &Origin) -> Result<(), Error> {
    use std::net::ToSocketAddrs;

    let host = origin.host();
    let Ok(found) = (host, origin.port()).to_socket_addrs() else {
        return Ok(());
    };
    for address in found {
        fetchloom_engine::address::allowed(host, address.ip())?;
    }
    Ok(())
}

fn stays_on_this_machine(origin: &Origin) -> bool {
    use std::net::ToSocketAddrs;

    let Ok(found) = (origin.host(), origin.port()).to_socket_addrs() else {
        return false;
    };
    let mut any = false;
    for address in found {
        any = true;
        if !address.ip().is_loopback() {
            return false;
        }
    }
    any
}

#[cfg(test)]
mod tests {
    use super::{ErrorKind, Limits, build_agent, classify};

    #[test]
    fn the_idle_pool_holds_every_connection_a_run_may_open_to_one_host() {
        let kept = build_agent(&Limits::default())
            .config()
            .max_idle_connections_per_host();
        let most =
            usize::try_from(fetchloom_engine::tuning::TRANSFERS_CEILING).unwrap_or(usize::MAX);
        assert!(
            kept >= most,
            "a run may open {most} connections to one host and the pool keeps {kept} of them, so the rest hand back their handshake"
        );
    }

    #[test]
    fn a_name_nothing_resolves_is_terminal() {
        let (kind, retryable, action) = classify(&ureq::Error::HostNotFound);
        assert_eq!(kind, ErrorKind::NetworkRefused);
        assert!(
            !retryable,
            "a name no resolver has heard of will not be heard of on the next attempt"
        );
        assert!(action.contains("resolve"), "the action was {action}");
    }

    #[test]
    fn a_socket_that_ran_out_of_time_is_worth_another_attempt() {
        let ran_out = std::io::Error::from(std::io::ErrorKind::TimedOut);
        let (kind, retryable, _) = classify(&ureq::Error::Io(ran_out));
        assert_eq!(kind, ErrorKind::NetworkTimeout);
        assert!(
            retryable,
            "a request that ran out of time may not next time"
        );
    }
}
