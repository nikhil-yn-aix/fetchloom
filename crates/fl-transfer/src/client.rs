//! The HTTP client: one connection pool, timeouts that bound silence, redirects followed by
//! hand so every hop is checked, and credentials sent only to their own origin.

use std::error::Error as _;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime};

use reqwest::header::{self, HeaderName, HeaderValue};
use reqwest::redirect::Policy;
use reqwest::{Certificate, RequestBuilder, Response};
use tracing::Instrument;

use crate::address::is_public;
use crate::counters::Counters;
use crate::credentials::Credentials;
use crate::error::TransferError;
use crate::hosts::Hosts;
use crate::resolve::{Refusal, Resolver, Shared};
use crate::retry::{self, Next};
use crate::url::SafeUrl;

const USER_AGENT: &str = concat!("fetchloom/", env!("CARGO_PKG_VERSION"));

/// Redirects followed for one request at most.
const REDIRECTS: usize = 10;

/// Variables that name proxies, whose hosts are trusted like a typed host.
const PROXY_VARIABLES: [&str; 6] = [
    "HTTPS_PROXY",
    "https_proxy",
    "HTTP_PROXY",
    "http_proxy",
    "ALL_PROXY",
    "all_proxy",
];

/// How a client connects.
#[derive(Clone, Debug)]
pub struct Config {
    /// The longest wait to reach a host, across all its addresses.
    pub connect: Duration,
    /// The longest wait for a response's headers once connected. It is enforced together with
    /// the connect timeout, as one bound around sending the request.
    pub response: Duration,
    /// The longest gap between two pieces of a body.
    pub idle: Duration,
    /// Transfers running at once.
    pub jobs: usize,
    /// Transfers running at once against one host.
    pub per_host: usize,
    /// Make no request at all, not even a DNS lookup.
    pub offline: bool,
    /// Certificates, in DER, trusted instead of the system's. Empty trusts the system's.
    pub trust_only: Vec<Vec<u8>>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(45),
            response: Duration::from_secs(30),
            idle: Duration::from_secs(30),
            jobs: 8,
            per_host: 4,
            offline: false,
            trust_only: Vec::new(),
        }
    }
}

/// Where a client reads the time and waits, so tests can wait without sleeping.
pub trait Timer: Send + Sync {
    /// The present moment.
    fn now(&self) -> SystemTime;

    /// Waits for `duration`.
    fn sleep(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send>>;
}

/// The system clock and tokio's sleep.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemTimer;

impl Timer for SystemTimer {
    fn now(&self) -> SystemTime {
        SystemTime::now()
    }

    fn sleep(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(tokio::time::sleep(duration))
    }
}

/// An HTTP client for downloads.
pub struct Client {
    http: reqwest::Client,
    resolver: Arc<Resolver>,
    credentials: Arc<dyn Credentials>,
    pub(crate) timer: Arc<dyn Timer>,
    rng: Mutex<fastrand::Rng>,
    pub(crate) config: Config,
    pub(crate) counters: Counters,
    hosts: Arc<Hosts>,
}

/// One request to make: where, and from which byte.
pub(crate) struct Ask<'a> {
    pub(crate) url: &'a SafeUrl,
    pub(crate) from: u64,
    pub(crate) to: Option<u64>,
    pub(crate) if_range: Option<&'a str>,
    pub(crate) attempt: u32,
    pub(crate) typed: bool,
}

/// A response, and how it was reached.
pub(crate) struct Answer {
    pub(crate) response: Response,
    pub(crate) url: SafeUrl,
    pub(crate) credentialed: bool,
    pub(crate) span: tracing::Span,
}

/// A failed attempt.
#[derive(Debug)]
pub(crate) struct Failure {
    pub(crate) error: TransferError,
    pub(crate) transient: bool,
    pub(crate) retry_after: Option<Duration>,
}

impl Failure {
    pub(crate) const fn last(error: TransferError) -> Self {
        Self {
            error,
            transient: false,
            retry_after: None,
        }
    }

    pub(crate) const fn again(error: TransferError) -> Self {
        Self {
            error,
            transient: true,
            retry_after: None,
        }
    }
}

impl Client {
    /// A client with the system clock, a random seed for backoff, and proxies from the
    /// environment trusted as typed hosts.
    ///
    /// # Errors
    ///
    /// Returns [`TransferError::Setup`] when TLS or the pool cannot be set up.
    pub fn new(config: Config, credentials: Arc<dyn Credentials>) -> Result<Self, TransferError> {
        let proxies = proxy_hosts(|name| std::env::var(name).ok());
        Self::with_timer(
            config,
            credentials,
            Arc::new(SystemTimer),
            fastrand::u64(..),
            proxies,
        )
    }

    /// A client with an injected clock, backoff seed and trusted proxy hosts.
    ///
    /// # Errors
    ///
    /// Returns [`TransferError::Setup`] when TLS or the pool cannot be set up.
    pub fn with_timer(
        config: Config,
        credentials: Arc<dyn Credentials>,
        timer: Arc<dyn Timer>,
        seed: u64,
        proxies: Vec<String>,
    ) -> Result<Self, TransferError> {
        let resolver = Arc::new(Resolver::new(config.offline, proxies));
        let mut builder = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .redirect(Policy::none())
            .referer(false)
            .connect_timeout(config.connect)
            .pool_max_idle_per_host(config.per_host)
            .dns_resolver(Shared(Arc::clone(&resolver)));
        if !config.trust_only.is_empty() {
            let certificates = config
                .trust_only
                .iter()
                .map(|der| Certificate::from_der(der))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| TransferError::Setup(error.to_string()))?;
            builder = builder.tls_certs_only(certificates);
        }
        let http = builder
            .build()
            .map_err(|error| TransferError::Setup(error.to_string()))?;
        Ok(Self {
            http,
            resolver,
            credentials,
            timer,
            rng: Mutex::new(fastrand::Rng::with_seed(seed)),
            hosts: Hosts::new(config.per_host),
            config,
            counters: Counters::default(),
        })
    }

    /// Trusts `host` as if the user had typed it, so its private addresses are allowed: for
    /// hosts the user configured, such as an adapter's record server.
    pub fn trust(&self, host: &str) {
        self.resolver.typed(host);
    }

    /// The work this client has done.
    #[must_use]
    pub const fn counters(&self) -> &Counters {
        &self.counters
    }

    /// The connections per host this client allows, shared with a scheduler that starts its
    /// downloads, so split downloads add range requests only where a host has room.
    #[must_use]
    pub fn hosts(&self) -> Arc<Hosts> {
        Arc::clone(&self.hosts)
    }

    /// Waits before the next attempt after `failures` failed ones, or says why there is none.
    pub(crate) async fn pause(
        &self,
        failures: u32,
        failure: Failure,
        url: &SafeUrl,
    ) -> Result<(), TransferError> {
        if !failure.transient {
            return Err(failure.error);
        }
        let next = {
            let mut rng = self.rng.lock().unwrap_or_else(PoisonError::into_inner);
            retry::next(failures, failure.retry_after, &mut rng)
        };
        match next {
            Next::Wait(wait) => {
                self.counters.retry();
                self.timer.sleep(wait).await;
                Ok(())
            }
            Next::GiveUp => Err(failure.error),
            Next::TooLong(wait) => Err(TransferError::RateLimited {
                host: url.host().unwrap_or_default().to_owned(),
                wait,
            }),
        }
    }

    /// Sends one GET and follows its redirects, checking every hop.
    pub(crate) async fn send(&self, ask: &Ask<'_>) -> Result<Answer, Failure> {
        let first = ask.url;
        if self.config.offline {
            return Err(Failure::last(TransferError::Offline { url: first.clone() }));
        }
        if ask.typed
            && let Some(host) = first.host()
        {
            self.resolver.typed(host);
        }
        let home = first.origin();
        let mut url = first.clone();
        let mut previous: Option<SafeUrl> = None;
        for _ in 0..=REDIRECTS {
            self.check_hop(&url, previous.as_ref())?;
            let credentialed = previous.is_none() || url.origin() == home;
            let (request, credentialed) = self.request(&url, ask, credentialed)?;
            let (response, span) = self.exchange(request, &url, ask).await?;
            match redirect(&response) {
                Some(location) => {
                    let next = url.join(location).map_err(|_| {
                        Failure::last(protocol(&url, "redirected to a location that is not a URL"))
                    })?;
                    previous = Some(std::mem::replace(&mut url, next));
                }
                None => {
                    return Ok(Answer {
                        response,
                        url,
                        credentialed,
                        span,
                    });
                }
            }
        }
        Err(Failure::last(protocol(
            &url,
            "redirected more than 10 times",
        )))
    }

    fn check_hop(&self, url: &SafeUrl, previous: Option<&SafeUrl>) -> Result<(), Failure> {
        if !matches!(url.scheme(), "http" | "https") {
            return Err(Failure::last(insecure(
                url,
                "only http and https are fetched",
            )));
        }
        if previous.is_some_and(|previous| previous.scheme() == "https" && url.scheme() == "http") {
            return Err(Failure::last(insecure(
                url,
                "a redirect from https to http is not followed",
            )));
        }
        let typed = url.host().is_some_and(|host| self.resolver.is_typed(host));
        if let Some(ip) = url.ip()
            && !typed
            && !is_public(ip)
        {
            return Err(Failure::last(TransferError::Refused { url: url.clone() }));
        }
        Ok(())
    }

    fn request(
        &self,
        url: &SafeUrl,
        ask: &Ask<'_>,
        own_origin: bool,
    ) -> Result<(RequestBuilder, bool), Failure> {
        let mut request = self.http.get(url.to_request());
        let mut credentialed = false;
        if let Some((user, password)) = url.user_info() {
            if url.scheme() != "https" {
                return Err(Failure::last(insecure(
                    url,
                    "a URL with a user or password is only fetched over https",
                )));
            }
            request = request.basic_auth(user, password);
            credentialed = true;
        } else if own_origin
            && url.scheme() == "https"
            && let Some(origin) = url.origin()
            && let Some(found) = self
                .credentials
                .find(&origin)
                .map_err(|error| Failure::last(error.into()))?
        {
            let name = HeaderName::from_bytes(found.header.as_bytes()).map_err(|_| {
                Failure::last(TransferError::Setup(
                    "a credential header name is not valid".to_owned(),
                ))
            })?;
            let mut value = HeaderValue::from_str(found.value.expose()).map_err(|_| {
                Failure::last(TransferError::Setup(
                    "a credential is not a valid header value".to_owned(),
                ))
            })?;
            value.set_sensitive(true);
            request = request.header(name, value);
            credentialed = true;
        }
        if ask.from > 0 || ask.to.is_some() {
            let to = ask.to.map(|to| to.to_string()).unwrap_or_default();
            request = request.header(header::RANGE, format!("bytes={}-{to}", ask.from));
            if let Some(validator) = ask.if_range {
                request = request.header(header::IF_RANGE, validator);
            }
        }
        Ok((request, credentialed))
    }

    async fn exchange(
        &self,
        request: RequestBuilder,
        url: &SafeUrl,
        ask: &Ask<'_>,
    ) -> Result<(Response, tracing::Span), Failure> {
        let host = url.host().unwrap_or_default();
        let span = tracing::debug_span!(
            "request",
            host,
            attempt = ask.attempt,
            from = ask.from,
            status = tracing::field::Empty,
            bytes = tracing::field::Empty,
        );
        self.counters.request();
        let limit = self.config.connect + self.config.response;
        let sent = tokio::time::timeout(limit, request.send())
            .instrument(span.clone())
            .await;
        let response = match sent {
            Err(_) => {
                return Err(Failure::again(TransferError::Timeout {
                    host: host.to_owned(),
                    during: "connecting and waiting for a response",
                    limit,
                }));
            }
            Ok(Err(error)) => return Err(self.failure(&error, url, "waiting for a response")),
            Ok(Ok(response)) => response,
        };
        span.record("status", response.status().as_u16());
        if url.ip().is_none()
            && let Some(address) = response.remote_addr()
        {
            self.resolver.answered(host, address.ip());
        }
        Ok((response, span))
    }

    /// What a failed request or body read means.
    pub(crate) fn failure(
        &self,
        error: &reqwest::Error,
        url: &SafeUrl,
        during: &'static str,
    ) -> Failure {
        let host = url.host().unwrap_or_default().to_owned();
        let mut source = error.source();
        while let Some(cause) = source {
            if let Some(refusal) = cause.downcast_ref::<Refusal>() {
                return Failure::last(match refusal {
                    Refusal::Offline(_) => TransferError::Offline { url: url.clone() },
                    Refusal::Private(_) => TransferError::Refused { url: url.clone() },
                });
            }
            source = cause.source();
        }
        if error.is_timeout() {
            let (during, limit) = if error.is_connect() {
                ("connecting", self.config.connect)
            } else {
                (during, self.config.idle)
            };
            return Failure::again(TransferError::Timeout {
                host,
                during,
                limit,
            });
        }
        Failure::again(TransferError::Unreachable {
            host,
            reason: describe(error),
        })
    }
}

fn redirect(response: &Response) -> Option<&str> {
    if !response.status().is_redirection() {
        return None;
    }
    response
        .headers()
        .get(header::LOCATION)
        .and_then(|value| value.to_str().ok())
}

fn describe(error: &reqwest::Error) -> String {
    let mut text = String::new();
    let mut cause: Option<&dyn std::error::Error> = error.source();
    while let Some(inner) = cause {
        text = inner.to_string();
        cause = inner.source();
    }
    if text.is_empty() {
        "the connection failed".to_owned()
    } else {
        text
    }
}

pub(crate) fn protocol(url: &SafeUrl, reason: &str) -> TransferError {
    TransferError::Protocol {
        url: url.clone(),
        reason: reason.to_owned(),
    }
}

fn insecure(url: &SafeUrl, reason: &'static str) -> TransferError {
    TransferError::Insecure {
        url: url.clone(),
        reason,
    }
}

/// The hosts of the proxies named in the environment, read through `read`.
fn proxy_hosts(read: impl Fn(&str) -> Option<String>) -> Vec<String> {
    PROXY_VARIABLES
        .iter()
        .filter_map(|name| read(name))
        .filter_map(|value| {
            let value = if value.contains("://") {
                value
            } else {
                format!("http://{value}")
            };
            SafeUrl::parse(&value).ok()?.host().map(str::to_owned)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_hosts_come_from_every_proxy_variable() {
        let hosts = proxy_hosts(|name| match name {
            "HTTPS_PROXY" => Some("http://user:pw@proxy.lab:3128".to_owned()),
            "all_proxy" => Some("10.0.0.5:8080".to_owned()),
            _ => None,
        });
        assert_eq!(hosts, ["proxy.lab", "10.0.0.5"]);
    }
}
