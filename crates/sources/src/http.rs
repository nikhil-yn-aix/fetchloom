//! The source that fetches over HTTP and HTTPS.
//!
//! Blocking throughout, because the Source seam's body is a reader and this
//! workspace has no asynchronous runtime. One agent per host holds that host's
//! connection pool, so a connection is never opened per request. Redirects are
//! followed here rather than inside the client, because the credential drop is
//! a contract and has to be decided and reported rather than inherited.

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
    ByteRange, ListingEntry, Source, SourceIdentity, SourceMetadata,
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
            ErrorKind::NetworkStatus,
            format!(
                "ask for a location that settles, because the source redirected more than {} times without answering",
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
            ));
        }

        Ok(SourceMetadata {
            location: SafeUrl::new(location),
            host: Host::new(Origin::of(location)?.host()),
            size: header(&answer, "content-length").and_then(|value| value.parse().ok()),
            content: None,
            interop: None,
            identity: identity_of(header(&answer, "etag").as_deref()),
            supports_ranges: header(&answer, "accept-ranges")
                .is_some_and(|value| value.split(',').any(|unit| unit.trim() == "bytes")),
            time_to_first_byte,
            retry_after: header(&answer, "retry-after")
                .as_deref()
                .and_then(parse_retry_after),
        })
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

fn status_failure(location: &str, status: u16, retry_after: Option<String>) -> Error {
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

fn transport_failure(location: &str, reason: &dyn std::fmt::Display) -> Error {
    let text = reason.to_string();
    let lowered = text.to_lowercase();
    let kind = if lowered.contains("timed out") || lowered.contains("timeout") {
        ErrorKind::NetworkTimeout
    } else if lowered.contains("tls") || lowered.contains("certificate") {
        ErrorKind::NetworkTls
    } else {
        ErrorKind::NetworkRefused
    };
    let retryable = kind != ErrorKind::NetworkTls;
    Error::new(
        kind,
        format!("try the source again, because the request did not complete: {text}"),
    )
    .with_source(location)
    .with_retryable(retryable)
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
