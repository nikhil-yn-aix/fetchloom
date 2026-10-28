//! the network: one HTTP client, polite retries, resumable downloads, and rules about where a
//! request may go and which credentials go with it.

mod address;
mod api;
mod download;
mod failure;
mod headers;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use reqwest::header::{HeaderName, HeaderValue};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use url::Url;

pub(crate) use api::{Api, Response};
pub(crate) use download::{Divert, Event, Expect, Fetch, Fetched, Origin};
pub(crate) use failure::ATTEMPTS;

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
    client: reqwest::Client,
    offline: bool,
    typed: address::Typed,
    credentials: Vec<(&'static str, HeaderName, HeaderValue)>,
    jobs: Arc<Semaphore>,
    hosts: Mutex<HashMap<String, Arc<Semaphore>>>,
    work: Arc<Work>,
    stop: Arc<Stop>,
    notify: Notify,
}

impl Net {
    pub(crate) fn new(
        options: Options,
        work: Arc<Work>,
        stop: Arc<Stop>,
        notify: Notify,
    ) -> Result<Net, Error> {
        // the process may already have a provider, which is fine
        let _ = rustls::crypto::ring::default_provider().install_default();
        let typed = address::Typed::default();
        let for_redirects = typed.clone();
        let client = reqwest::Client::builder()
            .user_agent(concat!("fetchloom/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(CONNECT)
            .dns_resolver(Arc::new(address::Resolver::new(
                typed.clone(),
                options.offline,
            )))
            .redirect(reqwest::redirect::Policy::custom(move |attempt| {
                if attempt.previous().len() >= REDIRECTS {
                    attempt.error(address::Refused(format!("more than {REDIRECTS} redirects")))
                } else if let Err(refused) = for_redirects.check(attempt.url()) {
                    attempt.error(refused)
                } else {
                    attempt.follow()
                }
            }))
            .build()
            .map_err(|err| {
                Error::new(
                    Kind::NetworkUnreachable,
                    format!("could not start the network client: {err}"),
                )
            })?;
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
        Ok(Net {
            client,
            offline: options.offline,
            typed,
            credentials,
            jobs: Arc::new(Semaphore::new(options.jobs.max(1))),
            hosts: Mutex::new(HashMap::new()),
            work,
            stop,
            notify,
        })
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

    /// a GET with the user agent, and the credential for this exact host when there is one.
    fn request(&self, url: &Url) -> reqwest::RequestBuilder {
        let mut request = self.client.get(url.clone());
        let secure = url.scheme() == "https" && url.port_or_known_default() == Some(443);
        if secure && let Some(host) = url.host_str() {
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
