//! the network: one HTTP client, polite retries, resumable downloads, and rules about where a
//! request may go and which credentials go with it.

mod address;
mod api;
mod download;
mod failure;
mod headers;
mod pace;
mod shown;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use reqwest::StatusCode;
use reqwest::header::{self, HeaderName, HeaderValue};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use url::Url;

pub(crate) use api::{Api, Response};
pub(crate) use download::{Divert, Event, Expect, Expired, Fetch, Fetched, Origin};
pub(crate) use failure::ATTEMPTS;
pub(crate) use shown::{Shown, mask_query, secret};

use self::failure::Failure;
use crate::error::{Error, Kind};
use crate::stop::Stop;
use crate::work::Work;

const CONNECT: Duration = Duration::from_secs(45);
const RESPONSE: Duration = Duration::from_secs(30);
const IDLE: Duration = Duration::from_secs(30);
const REDIRECTS: usize = 10;
/// requests running at once to one host, so no server sees fetchloom as a flood.
const PER_HOST: usize = 4;

/// a token from where its source keeps it, sent only to its own host and only over https.
pub(crate) struct Credential {
    pub(crate) host: &'static str,
    pub(crate) header: &'static str,
    pub(crate) value: String,
}

/// something the person watching should hear about while the network works.
pub(crate) enum Notice {
    Retrying {
        reason: String,
        wait: Duration,
        next: u32,
    },
    /// requests to `host` wait for its rate limit to reset. `quota` and `window` are the limit,
    /// when the host stated them.
    Waiting {
        host: String,
        quota: Option<u64>,
        window: Option<Duration>,
        wait: Duration,
    },
}

pub(crate) type Notify = Arc<dyn Fn(Notice) + Send + Sync>;

/// the settings that enabled offline mode, kept for recovery instructions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Offline {
    No,
    Flag,
    Environment,
    Both,
}

impl Offline {
    pub(crate) fn from_settings(flag: bool, environment: bool) -> Offline {
        match (flag, environment) {
            (false, false) => Self::No,
            (true, false) => Self::Flag,
            (false, true) => Self::Environment,
            (true, true) => Self::Both,
        }
    }

    pub(crate) fn enabled(self) -> bool {
        self != Self::No
    }

    pub(crate) fn retry(self) -> &'static str {
        match self {
            Self::No => "run the same command when you are online",
            Self::Flag => "run the same command without --offline",
            Self::Environment => "unset FETCHLOOM_OFFLINE, then run the same command",
            Self::Both => "remove --offline and unset FETCHLOOM_OFFLINE, then run the same command",
        }
    }
}

pub(crate) struct Options {
    pub(crate) offline: Offline,
    /// downloads running at once, across all hosts.
    pub(crate) jobs: usize,
    pub(crate) credentials: Vec<Credential>,
    /// whether a large download may be split across streams.
    pub(crate) split: bool,
}

/// how a large download is split across streams.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Split {
    pub(crate) enabled: bool,
    /// the smallest file split.
    pub(crate) above: u64,
    /// the smallest piece a stream takes from another.
    pub(crate) piece: u64,
    /// how long a rate is measured before and after a stream is added.
    pub(crate) window: Duration,
}

pub(crate) struct Net {
    /// made on the first request, so a run that never goes online loads no TLS roots and reads no
    /// proxy settings.
    wire: OnceLock<Result<Wire, Error>>,
    offline: Offline,
    typed: address::Typed,
    credentials: Vec<(&'static str, HeaderName, HeaderValue)>,
    jobs: Arc<Semaphore>,
    hosts: Mutex<HashMap<String, Arc<Semaphore>>>,
    paces: pace::Paces,
    split: Split,
    work: Arc<Work>,
    stop: Arc<Stop>,
    notify: Notify,
}

/// the HTTP clients and the proxies their requests go through.
struct Wire {
    client: reqwest::Client,
    /// for the added streams of a split download: HTTP/1.1, so each request has a connection of
    /// its own instead of sharing one with every other request to the host. made when first
    /// needed.
    apart: OnceLock<Result<reqwest::Client, Error>>,
    typed: address::Typed,
    offline: bool,
    proxies: address::Proxies,
}

/// which client sends a request.
#[derive(Clone, Copy)]
enum Lane {
    Shared,
    Apart,
}

impl Wire {
    fn new(typed: &address::Typed, offline: bool) -> Result<Wire, Error> {
        // the process may already have a provider, which is fine
        let _ = rustls::crypto::ring::default_provider().install_default();
        Ok(Wire {
            client: Wire::client(typed, offline, false)?,
            apart: OnceLock::new(),
            typed: typed.clone(),
            offline,
            proxies: address::Proxies::from_system(),
        })
    }

    fn client(
        typed: &address::Typed,
        offline: bool,
        apart: bool,
    ) -> Result<reqwest::Client, Error> {
        let builder = reqwest::Client::builder()
            .user_agent(concat!("fetchloom/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(CONNECT)
            .dns_resolver(Arc::new(address::Resolver::new(typed.clone(), offline)))
            // `send` follows redirects itself, so that each hop is checked before it goes out
            .redirect(reqwest::redirect::Policy::none());
        let builder = if apart { builder.http1_only() } else { builder };
        builder.build().map_err(|err| {
            Error::new(
                Kind::NetworkUnreachable,
                format!("could not start the network client: {err}"),
            )
        })
    }

    fn lane(&self, lane: Lane) -> Result<&reqwest::Client, Error> {
        match lane {
            Lane::Shared => Ok(&self.client),
            Lane::Apart => self
                .apart
                .get_or_init(|| Wire::client(&self.typed, self.offline, true))
                .as_ref()
                .map_err(Clone::clone),
        }
    }
}

impl Net {
    pub(crate) fn new(options: Options, work: Arc<Work>, stop: Arc<Stop>, notify: Notify) -> Net {
        let credentials = options
            .credentials
            .into_iter()
            .filter_map(|credential| {
                let name = HeaderName::from_static(credential.header);
                let mut value = HeaderValue::from_str(&credential.value).ok()?;
                value.set_sensitive(true);
                Some((credential.host, name, value))
            })
            .collect();
        Net {
            wire: OnceLock::new(),
            offline: options.offline,
            typed: address::Typed::default(),
            credentials,
            jobs: Arc::new(Semaphore::new(options.jobs.max(1))),
            hosts: Mutex::new(HashMap::new()),
            paces: pace::Paces::default(),
            split: Split {
                enabled: options.split,
                above: download::SPLIT_ABOVE,
                piece: download::SMALLEST_PIECE,
                window: download::WINDOW,
            },
            work,
            stop,
            notify,
        }
    }

    /// the client, made once even when the first requests start together. a client that could
    /// not be made stays an error, since nothing about a retry would differ.
    fn wire(&self) -> Result<&Wire, Error> {
        self.wire
            .get_or_init(|| Wire::new(&self.typed, self.offline.enabled()))
            .as_ref()
            .map_err(Clone::clone)
    }

    /// marks the host of a URL a person wrote, so it may be a private address such as a lab
    /// server.
    pub(crate) fn typed(&self, url: &Url) {
        self.typed.add(url);
    }

    /// trusts the hosts data.toml names, since a person wrote them too.
    pub(crate) fn trust(&self, manifest_hosts: impl IntoIterator<Item = String>) {
        for host in manifest_hosts {
            self.typed.add_host(&host);
        }
    }

    pub(crate) fn offline(&self) -> Offline {
        self.offline
    }

    /// a GET to `url`, shaped by `shape`, with redirects followed one hop at a time, so that each
    /// hop is checked the way its connection will be made before it is sent, and waits while its
    /// host's rate limit is used up. a credential goes only to its own host, and never once a
    /// redirect has left the first one.
    async fn send(
        &self,
        url: &Url,
        shape: impl Fn(reqwest::RequestBuilder) -> reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, Failure> {
        self.send_on(Lane::Shared, url, shape).await
    }

    /// [`Net::send`] on a connection of the request's own, for the added streams of a split
    /// download.
    async fn send_apart(
        &self,
        url: &Url,
        shape: impl Fn(reqwest::RequestBuilder) -> reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, Failure> {
        self.send_on(Lane::Apart, url, shape).await
    }

    async fn send_on(
        &self,
        lane: Lane,
        url: &Url,
        shape: impl Fn(reqwest::RequestBuilder) -> reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, Failure> {
        let refused = |url: &Url, reason| Failure::Final(failure::refused(url, &reason));
        let wire = self.wire().map_err(Failure::Final)?;
        let client = wire.lane(lane).map_err(Failure::Final)?;
        let mut hop = url.clone();
        let mut redirects = 0;
        let mut first_origin = true;
        loop {
            wire.proxies
                .check(&hop, &self.typed)
                .await
                .map_err(|reason| refused(&hop, reason))?;
            first_origin &= hop.origin() == url.origin();
            let ticket = self.pace(&hop).await.map_err(Failure::Final)?;
            let request = shape(self.request(client, &hop, first_origin));
            let response = tokio::select! {
                sent = tokio::time::timeout(CONNECT + RESPONSE, request.send()) => match sent {
                    Err(_) => return Err(failure::transient(format!("{} did not answer within {} s", host(&hop), (CONNECT + RESPONSE).as_secs()))),
                    Ok(Err(err)) => return Err(failure::classify(&hop, &err, self.offline)),
                    Ok(Ok(response)) => response,
                },
                () = self.stop.asked() => return Err(Failure::Final(failure::interrupted(&hop))),
            };
            let limits = pace::limits(response.status(), response.headers(), SystemTime::now());
            pace::Paces::heard(ticket, limits, Instant::now());
            let Some(next) = redirect_target(&hop, &response) else {
                return Ok(response);
            };
            redirects += 1;
            if redirects > REDIRECTS {
                let reason = address::Refused(format!("more than {REDIRECTS} redirects"));
                return Err(refused(url, reason));
            }
            self.typed
                .check(&next)
                .map_err(|reason| refused(&next, reason))?;
            tracing::debug!(from = host(&hop), to = host(&next), "redirect");
            hop = next;
        }
    }

    /// a GET with the user agent and, when `credentials` allows, the credential for this exact
    /// host.
    fn request(
        &self,
        client: &reqwest::Client,
        url: &Url,
        credentials: bool,
    ) -> reqwest::RequestBuilder {
        let mut request = client.get(url.clone());
        let secure = url.scheme() == "https" && url.port_or_known_default() == Some(443);
        if credentials
            && secure
            && let Some(host) = url.host_str()
        {
            for (owner, name, value) in &self.credentials {
                if host.eq_ignore_ascii_case(owner) {
                    request = request.header(name.clone(), value.clone());
                }
            }
        }
        request
    }

    /// waits while a rate limit of the URL's host is used up, telling the person watching once
    /// for each reset, then counts the request as on its way.
    async fn pace(&self, url: &Url) -> Result<pace::Ticket, Error> {
        let name = host(url).to_ascii_lowercase();
        loop {
            let now = Instant::now();
            let hold = match self.paces.admit(&name, now) {
                Ok(ticket) => return Ok(ticket),
                Err(hold) => hold,
            };
            let wait = hold.until.saturating_duration_since(now);
            tracing::debug!(host = name, ?wait, "waiting for a rate limit to reset");
            if hold.announce && wait >= Duration::from_millis(500) {
                (self.notify)(Notice::Waiting {
                    host: host(url).to_owned(),
                    quota: hold.quota,
                    window: hold.window,
                    wait,
                });
            }
            tokio::select! {
                () = tokio::time::sleep_until(hold.until.into()) => {}
                () = self.stop.asked() => return Err(failure::interrupted(url)),
            }
        }
    }

    /// waits for a free slot for this host, then for one overall.
    async fn permit(
        &self,
        url: &Url,
    ) -> Result<(OwnedSemaphorePermit, OwnedSemaphorePermit), Error> {
        let per_host = self.slots(url);
        let closed = |_| Error::new(Kind::Interrupted, "stopped");
        let wait = async {
            let host = per_host.acquire_owned().await.map_err(closed)?;
            let all = self.jobs.clone().acquire_owned().await.map_err(closed)?;
            Ok((host, all))
        };
        tokio::select! {
            permits = wait => permits,
            () = self.stop.asked() => Err(failure::interrupted(url)),
        }
    }

    /// a slot for this host and one overall, when both are free and nothing waits for them, so
    /// an added stream never holds up another file.
    fn try_permit(&self, url: &Url) -> Option<(OwnedSemaphorePermit, OwnedSemaphorePermit)> {
        let host = self.slots(url).try_acquire_owned().ok()?;
        let all = self.jobs.clone().try_acquire_owned().ok()?;
        Some((host, all))
    }

    fn slots(&self, url: &Url) -> Arc<Semaphore> {
        let mut hosts = self.hosts.lock().unwrap_or_else(PoisonError::into_inner);
        hosts
            .entry(host(url).to_ascii_lowercase())
            .or_insert_with(|| Arc::new(Semaphore::new(PER_HOST)))
            .clone()
    }
}

/// the host of a URL as people say it, for messages.
pub(crate) fn host(url: &Url) -> &str {
    url.host_str().unwrap_or("the server")
}

/// where `response` redirects a request for `from`, when it is a redirect to an http or https
/// link.
fn redirect_target(from: &Url, response: &reqwest::Response) -> Option<Url> {
    let redirect = matches!(
        response.status(),
        StatusCode::MOVED_PERMANENTLY
            | StatusCode::FOUND
            | StatusCode::SEE_OTHER
            | StatusCode::TEMPORARY_REDIRECT
            | StatusCode::PERMANENT_REDIRECT
    );
    let location = response.headers().get(header::LOCATION)?.to_str().ok()?;
    let next = from.join(location).ok()?;
    (redirect && matches!(next.scheme(), "http" | "https")).then_some(next)
}

#[cfg(test)]
mod credential_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_that_sends_nothing_loads_no_tls_roots() {
        let options = Options {
            offline: Offline::No,
            jobs: 1,
            credentials: Vec::new(),
            split: true,
        };
        let net = Net::new(options, Arc::default(), Arc::default(), Arc::new(|_| {}));
        assert!(net.wire.get().is_none());
        net.wire().unwrap();
        assert!(net.wire.get().is_some());
    }
}
