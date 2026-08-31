//! The source that fetches over HTTP and HTTPS.

use std::collections::HashMap;
use std::io::Read;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use fetchloom_engine::credential::Credential;
use fetchloom_engine::degrade::{Degradation, DegradeQueue};
use fetchloom_engine::error::{Error, ErrorKind};
use fetchloom_engine::redact::SafeUrl;
use fetchloom_engine::reference::Host;
use fetchloom_engine::seam::source::{
    ByteRange, ListingEntry, Revalidated, Served, Source, SourceIdentity, SourceMetadata, Validator,
};

use fetchloom_engine::limits::Limits;
use fetchloom_engine::work::WorkCounter;

use crate::index;
use crate::origin::Origin;

/// The bytes of one object, streamed as they arrive.
pub struct HttpBody {
    reader: Box<dyn Read + Send + Sync>,
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
    ///
    /// Takes the bounds every request obeys and where the run counts the
    /// requests it issues.
    #[must_use]
    pub fn new(limits: Limits, work: Arc<WorkCounter>) -> Self {
        Self {
            agents: Mutex::new(HashMap::new()),
            degradations: DegradeQueue::new(),
            limits,
            work,
        }
    }

    /// Removes and returns every fallback performed since the last call.
    #[must_use]
    pub fn take_degradations(&self) -> Vec<Degradation> {
        self.degradations.take()
    }

    fn agent(&self, host: &str) -> ureq::Agent {
        let limits = self.limits;
        let mut held = self.agents.lock().unwrap_or_else(PoisonError::into_inner);
        held.entry(host.to_owned())
            .or_insert_with(|| build_agent(&limits))
            .clone()
    }

    fn send(
        &self,
        method: Method,
        location: &str,
        range: Option<ByteRange>,
        credential: Option<&Credential>,
    ) -> Result<ureq::http::Response<ureq::Body>, Error> {
        self.send_conditional(method, location, range, credential, &Validator::default())
    }

    fn send_conditional(
        &self,
        method: Method,
        location: &str,
        range: Option<ByteRange>,
        credential: Option<&Credential>,
        asking: &Validator,
    ) -> Result<ureq::http::Response<ureq::Body>, Error> {
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
                request = request.header("Authorization", credential.value.expose());
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
                return Ok(answer);
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
enum Method {
    /// A bounded metadata request.
    Head,
    /// A request for bytes.
    Get,
}

fn build_agent(limits: &Limits) -> ureq::Agent {
    let _ = rustls::crypto::ring::default_provider().install_default();
    ureq::Agent::config_builder()
        .max_redirects(0)
        .http_status_as_error(false)
        .timeout_connect(Some(limits.connect_timeout))
        .timeout_recv_response(Some(limits.response_timeout))
        .timeout_recv_body(Some(limits.idle_timeout))
        .max_idle_connections_per_host(limits.connections_per_host)
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

impl Source for HttpSource {
    type Body = HttpBody;

    fn probe(
        &self,
        location: &str,
        credential: Option<&Credential>,
    ) -> Result<SourceMetadata, Error> {
        let started = Instant::now();
        let answer = self.send(Method::Head, location, None, credential)?;
        let time_to_first_byte = started.elapsed();
        let status = answer.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(status_failure(
                location,
                status,
                header(&answer, "retry-after"),
                credential,
            ));
        }

        Ok(SourceMetadata {
            location: SafeUrl::new(location),
            host: Host::new(Origin::of(location)?.host()),
            size: header(&answer, "content-length").and_then(|value| value.parse().ok()),
            content: None,
            interop: None,
            identity: identity_of(header(&answer, "etag").as_deref()),
            last_modified: header(&answer, "last-modified"),
            supports_ranges: header(&answer, "accept-ranges")
                .is_some_and(|value| value.split(',').any(|unit| unit.trim() == "bytes")),
            time_to_first_byte,
            retry_after: header(&answer, "retry-after")
                .as_deref()
                .and_then(parse_retry_after),
        })
    }

    fn revalidate(
        &self,
        location: &str,
        validator: &Validator,
        credential: Option<&Credential>,
    ) -> Result<Revalidated<Self::Body>, Error> {
        let started = Instant::now();
        let answer = self.send_conditional(Method::Get, location, None, credential, validator)?;
        let time_to_first_byte = started.elapsed();
        let status = answer.status().as_u16();
        if status == 304 {
            return Ok(Revalidated::Unchanged);
        }
        if !(200..300).contains(&status) {
            return Err(status_failure(
                location,
                status,
                header(&answer, "retry-after"),
                credential,
            ));
        }
        let metadata = SourceMetadata {
            location: SafeUrl::new(location),
            host: Host::new(Origin::of(location)?.host()),
            size: header(&answer, "content-length").and_then(|value| value.parse().ok()),
            content: None,
            interop: None,
            identity: identity_of(header(&answer, "etag").as_deref()),
            last_modified: header(&answer, "last-modified"),
            supports_ranges: header(&answer, "accept-ranges")
                .is_some_and(|value| value.split(',').any(|unit| unit.trim() == "bytes")),
            time_to_first_byte,
            retry_after: header(&answer, "retry-after")
                .as_deref()
                .and_then(parse_retry_after),
        };
        Ok(Revalidated::Changed(Box::new(Served {
            metadata,
            body: HttpBody {
                reader: Box::new(answer.into_body().into_reader()),
            },
        })))
    }

    fn fetch(
        &self,
        location: &str,
        range: Option<ByteRange>,
        credential: Option<&Credential>,
    ) -> Result<Self::Body, Error> {
        let answer = self.send(Method::Get, location, range, credential)?;
        let status = answer.status().as_u16();

        if status == 416 {
            return Err(Error::new(
                ErrorKind::SourceUnsupportedRange,
                format!(
                    "ask for the whole object, because the source cannot serve the span that was asked for and reported its length as {}",
                    header(&answer, "content-range").unwrap_or_else(|| "unknown".to_owned())
                ),
            )
            .with_source(location));
        }
        if !(200..300).contains(&status) {
            return Err(status_failure(
                location,
                status,
                header(&answer, "retry-after"),
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
            let served = header(&answer, "content-range").and_then(|value| first_byte_of(&value));
            if served != Some(range.start) {
                return Err(Error::new(
                    ErrorKind::IntegrityRangeMismatch,
                    format!(
                        "name a source that serves the span it is asked for, because this one was asked for the bytes from {} and answered with {}",
                        range.start,
                        header(&answer, "content-range")
                            .unwrap_or_else(|| "no span at all".to_owned())
                    ),
                )
                .with_source(location));
            }
        }

        Ok(HttpBody {
            reader: Box::new(answer.into_body().into_reader()),
        })
    }

    fn list(
        &self,
        location: &str,
        credential: Option<&Credential>,
    ) -> Result<Vec<ListingEntry>, Error> {
        let answer = self.send(Method::Get, location, None, credential)?;
        let status = answer.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(status_failure(
                location,
                status,
                header(&answer, "retry-after"),
                credential,
            ));
        }
        let body = answer
            .into_body()
            .into_with_config()
            .limit(self.limits.listing_bytes)
            .read_to_string()
            .map_err(|reason| transport_failure(location, &reason))?;
        index::parse(location, status, &body)
    }
}

fn header<T>(answer: &ureq::http::Response<T>, name: &str) -> Option<String> {
    answer
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

fn identity_of(etag: Option<&str>) -> SourceIdentity {
    match etag {
        Some(tag) if tag.starts_with("W/") => SourceIdentity::WeakValidator(tag.to_owned()),
        Some(tag) => SourceIdentity::StrongValidator(tag.to_owned()),
        None => SourceIdentity::None,
    }
}

fn parse_retry_after(value: &str) -> Option<Duration> {
    value.trim().parse().ok().map(Duration::from_secs)
}

fn status_failure(
    location: &str,
    status: u16,
    retry_after: Option<String>,
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
    let retryable = matches!(status, 408 | 429 | 500 | 502 | 503 | 504);
    let action = match retry_after {
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
    Error::new(ErrorKind::NetworkStatus, action)
        .with_source(location)
        .with_retryable(retryable)
}

fn transport_failure(location: &str, reason: &ureq::Error) -> Error {
    let (kind, retryable, action) = classify(reason);
    Error::new(kind, format!("{action}: {reason}"))
        .with_source(location)
        .with_retryable(retryable)
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
///
/// A failed handshake reaches the caller as an io failure holding the rustls
/// failure that caused it, so the question is asked of the value and never of
/// the message it prints.
fn unsecurable(reason: &std::io::Error) -> bool {
    reason
        .get_ref()
        .is_some_and(|held| held.downcast_ref::<rustls::Error>().is_some())
}

/// Returns whether a name lookup failed in a way another attempt cannot fix.
///
/// A resolver that has not heard of a name and one that could not be reached
/// are different answers, and only the first of them will be the same on the
/// next attempt. Windows numbers the two apart. Unix folds every lookup failure
/// into one io failure, so the test is the message the standard library itself
/// writes around `getaddrinfo`, which keeps the two apart in its text.
fn unresolvable(reason: &std::io::Error) -> bool {
    if matches!(reason.raw_os_error(), Some(11_001 | 11_004)) {
        return true;
    }
    let text = reason.to_string();
    text.contains("failed to lookup address information")
        && !text.contains("Temporary failure in name resolution")
}

/// Reads the first byte offset a partial response says it is serving.
///
/// Takes the value of a `Content-Range` header. Returns nothing when the value
/// names no satisfied span.
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
