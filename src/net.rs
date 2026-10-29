//! the network: one HTTP client, polite retries, resumable downloads, and rules about where a
//! request may go and which credentials go with it.

mod address;
mod api;
mod download;
mod failure;
mod headers;
mod shown;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Duration;

use reqwest::StatusCode;
use reqwest::header::{self, HeaderName, HeaderValue};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use url::Url;

pub(crate) use api::{Api, Response};
pub(crate) use download::{Divert, Event, Expect, Expired, Fetch, Fetched, Origin};
pub(crate) use failure::ATTEMPTS;
pub(crate) use shown::{Shown, secret};

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
}

pub(crate) type Notify = Arc<dyn Fn(Notice) + Send + Sync>;

pub(crate) struct Options {
    pub(crate) offline: bool,
    /// downloads running at once, across all hosts.
    pub(crate) jobs: usize,
    pub(crate) credentials: Vec<Credential>,
}

pub(crate) struct Net {
    /// made on the first request, so a run that never goes online loads no TLS roots and reads no
    /// proxy settings.
    wire: OnceLock<Result<Wire, Error>>,
    offline: bool,
    typed: address::Typed,
    credentials: Vec<(&'static str, HeaderName, HeaderValue)>,
    jobs: Arc<Semaphore>,
    hosts: Mutex<HashMap<String, Arc<Semaphore>>>,
    work: Arc<Work>,
    stop: Arc<Stop>,
    notify: Notify,
}

/// the HTTP client and the proxies its requests go through.
struct Wire {
    client: reqwest::Client,
    proxies: address::Proxies,
}

impl Wire {
    fn new(typed: &address::Typed, offline: bool) -> Result<Wire, Error> {
        // the process may already have a provider, which is fine
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = reqwest::Client::builder()
            .user_agent(concat!("fetchloom/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(CONNECT)
            .dns_resolver(Arc::new(address::Resolver::new(typed.clone(), offline)))
            // `send` follows redirects itself, so that each hop is checked before it goes out
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|err| {
                Error::new(
                    Kind::NetworkUnreachable,
                    format!("could not start the network client: {err}"),
                )
            })?;
        Ok(Wire {
            client,
            proxies: address::Proxies::from_system(),
        })
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
            work,
            stop,
            notify,
        }
    }

    /// the client, made once even when the first requests start together. a client that could
    /// not be made stays an error, since nothing about a retry would differ.
    fn wire(&self) -> Result<&Wire, Error> {
        self.wire
            .get_or_init(|| Wire::new(&self.typed, self.offline))
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

    pub(crate) fn is_offline(&self) -> bool {
        self.offline
    }

    /// a GET to `url`, shaped by `shape`, with redirects followed one hop at a time, so that each
    /// hop is checked the way its connection will be made before it is sent. a credential goes
    /// only to its own host, and never once a redirect has left the first one.
    async fn send(
        &self,
        url: &Url,
        shape: impl Fn(reqwest::RequestBuilder) -> reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, Failure> {
        let refused = |url: &Url, reason| Failure::Final(failure::refused(url, &reason));
        let wire = self.wire().map_err(Failure::Final)?;
        let mut hop = url.clone();
        let mut redirects = 0;
        let mut first_origin = true;
        loop {
            wire.proxies
                .check(&hop, &self.typed)
                .await
                .map_err(|reason| refused(&hop, reason))?;
            first_origin &= hop.origin() == url.origin();
            let request = shape(self.request(&wire.client, &hop, first_origin));
            let response = tokio::select! {
                sent = tokio::time::timeout(CONNECT + RESPONSE, request.send()) => match sent {
                    Err(_) => return Err(failure::transient(format!("{} did not answer within {} s", host(&hop), (CONNECT + RESPONSE).as_secs()))),
                    Ok(Err(err)) => return Err(failure::classify(&hop, &err)),
                    Ok(Ok(response)) => response,
                },
                () = self.stop.asked() => return Err(Failure::Final(failure::interrupted(&hop))),
            };
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

    /// waits for a free slot for this host, then for one overall.
    async fn permit(
        &self,
        url: &Url,
    ) -> Result<(OwnedSemaphorePermit, OwnedSemaphorePermit), Error> {
        let per_host = {
            let mut hosts = self.hosts.lock().unwrap_or_else(PoisonError::into_inner);
            hosts
                .entry(host(url).to_ascii_lowercase())
                .or_insert_with(|| Arc::new(Semaphore::new(PER_HOST)))
                .clone()
        };
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
            offline: false,
            jobs: 1,
            credentials: Vec::new(),
        };
        let net = Net::new(options, Arc::default(), Arc::default(), Arc::new(|_| {}));
        assert!(net.wire.get().is_none());
        net.wire().unwrap();
        assert!(net.wire.get().is_some());
    }
}
