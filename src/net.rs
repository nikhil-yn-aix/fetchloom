//! the network: bounded HTTP work, polite retries, resumable downloads, and rules about where a
//! request may go and which credentials go with it.

mod address;
mod api;
mod download;
mod failure;
mod headers;
mod pace;
mod rate;
mod shown;

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use reqwest::StatusCode;
use reqwest::header::{self, HeaderName, HeaderValue};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use url::Url;

pub(crate) use api::{Api, Response};
pub(crate) use download::{Divert, Event, Expect, Expired, Fetch, Fetched, Origin};
pub(crate) use failure::ATTEMPTS;
pub(crate) use rate::Rate;
pub(crate) use shown::{Shown, mask_query, secret};

use self::failure::Failure;
use crate::error::{Error, Kind};
use crate::exp::Experiments;
use crate::stop::Stop;
use crate::work::Work;

const CONNECT: Duration = Duration::from_secs(45);
const RESPONSE: Duration = Duration::from_secs(30);
const IDLE: Duration = Duration::from_secs(30);
const REDIRECTS: usize = 10;
/// requests running at once to one host, so no server sees fetchloom as a flood.
const PER_HOST: usize = 4;
const LINK_STREAMS: usize = 8;
/// whether the logs already said a key cannot be sent, which they say once a run.
static UNSENDABLE_TOLD: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// a token from where its source keeps it, sent only to its own host and port and only over
/// https.
#[derive(Clone)]
pub(crate) struct Credential {
    /// the host, with `:port` when the key belongs to another port than 443.
    pub(crate) host: String,
    pub(crate) header: &'static str,
    pub(crate) value: String,
    /// sent on every hop that reaches its host, as curl sends a `.netrc` login, rather than only
    /// before a redirect leaves the first origin. only a login a person tied to a host gets this.
    pub(crate) every_hop: bool,
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
            Self::Flag => "run it again without --offline",
            Self::Environment => "unset FETCHLOOM_OFFLINE, then run the same command",
            Self::Both => "unset FETCHLOOM_OFFLINE, then run it again without --offline",
        }
    }
}

pub(crate) struct Options {
    pub(crate) offline: Offline,
    /// downloads running at once, across all hosts.
    pub(crate) jobs: usize,
    pub(crate) credentials: Vec<Credential>,
    /// when a large download is split across streams.
    pub(crate) split: Splitting,
    /// experiment settings read once for this run.
    pub(crate) exp: Experiments,
}

/// when a large download that may split across streams does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Splitting {
    /// every file comes over one stream.
    Never,
    /// the file comes over one stream until that stream proves slow.
    WhenSlow,
    /// the file splits from its first answer on.
    Always,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Split {
    pub(crate) when: Splitting,
    /// the smallest file split.
    pub(crate) above: u64,
    /// the smallest piece a stream takes from another.
    pub(crate) piece: u64,
    /// how long a rate is measured before and after a stream is added.
    pub(crate) window: Duration,
    /// bytes a second below which one stream, over a window, hands over to be split.
    pub(crate) slow: u64,
}

impl Split {
    #[expect(clippy::cast_precision_loss, reason = "a rate is an estimate")]
    fn hand_over(self, rate: f64, remaining: u64) -> bool {
        rate < self.slow as f64 && remaining >= 2 * self.piece
    }
}

type Permits = (OwnedSemaphorePermit, OwnedSemaphorePermit);

pub(crate) struct Net {
    /// made on the first request, so a run that never goes online loads no TLS roots and reads no
    /// proxy settings.
    wire: OnceLock<Result<Wire, Error>>,
    offline: Offline,
    typed: address::Typed,
    credentials: Vec<Key>,
    jobs: Arc<Semaphore>,
    hosts: Mutex<HashMap<String, Arc<Semaphore>>>,
    paces: pace::Paces,
    rate: Option<Arc<Rate>>,
    split: Split,
    exp: Experiments,
    idle: Duration,
    work: Arc<Work>,
    stop: Arc<Stop>,
    notify: Notify,
}

/// a credential as requests carry it: to one host and port.
struct Key {
    host: String,
    port: u16,
    name: HeaderName,
    value: HeaderValue,
    every_hop: bool,
}

/// the host and port a credential's host names: lowercase, with port 443 unless it names
/// another. none when it is no plain host.
fn key_origin(host: &str) -> Option<(String, u16)> {
    let url = Url::parse(&format!("https://{host}/")).ok()?;
    let plain = url.username().is_empty()
        && url.password().is_none()
        && url.path() == "/"
        && url.query().is_none()
        && url.fragment().is_none();
    if !plain {
        return None;
    }
    Some((url.host_str()?.to_owned(), url.port_or_known_default()?))
}

/// the HTTP clients and the proxies their requests go through.
struct Wire {
    client: reqwest::Client,
    /// for file bodies and added ranges: HTTP/1.1, so each request has a connection of
    /// its own instead of sharing one with every other request to the host. made when first
    /// needed.
    apart: OnceLock<Result<reqwest::Client, Error>>,
    /// the cookies both clients send, so a sign-in cookie reaches the added streams too.
    cookies: Arc<reqwest::cookie::Jar>,
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
        let cookies = Arc::new(reqwest::cookie::Jar::default());
        Ok(Wire {
            client: Wire::client(typed, offline, &cookies, false)?,
            apart: OnceLock::new(),
            cookies,
            typed: typed.clone(),
            offline,
            proxies: address::Proxies::from_system(),
        })
    }

    fn client(
        typed: &address::Typed,
        offline: bool,
        cookies: &Arc<reqwest::cookie::Jar>,
        apart: bool,
    ) -> Result<reqwest::Client, Error> {
        let builder = reqwest::Client::builder()
            .user_agent(concat!("fetchloom/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(CONNECT)
            .dns_resolver(Arc::new(address::Resolver::new(typed.clone(), offline)))
            // `send` follows redirects itself, so that each hop is checked before it goes out
            .redirect(reqwest::redirect::Policy::none())
            // earthdata's archives sign a person in through a redirect to urs.earthdata.nasa.gov,
            // which hands back a cookie the archive then asks for. cookies live for this run only
            .cookie_provider(cookies.clone());
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
                .get_or_init(|| Wire::client(&self.typed, self.offline, &self.cookies, true))
                .as_ref()
                .map_err(Clone::clone),
        }
    }
}

impl Net {
    pub(crate) fn new(options: Options, work: Arc<Work>, stop: Arc<Stop>, notify: Notify) -> Net {
        // one value per header, host and port: the first, so a key just signed in with wins over
        // an older one for the same host
        let mut seen = std::collections::HashSet::new();
        let credentials = options
            .credentials
            .into_iter()
            .filter_map(|credential| {
                let (host, port) = key_origin(&credential.host)?;
                let name = HeaderName::from_static(credential.header);
                let Ok(mut value) = HeaderValue::from_str(&credential.value) else {
                    // the host and header only; the value is the secret
                    if !UNSENDABLE_TOLD.swap(true, std::sync::atomic::Ordering::Relaxed) {
                        tracing::warn!(
                            host,
                            header = credential.header,
                            "a key was found but cannot be sent, since no header holds its value"
                        );
                    }
                    return None;
                };
                value.set_sensitive(true);
                Some(Key {
                    host,
                    port,
                    name,
                    value,
                    every_hop: credential.every_hop,
                })
            })
            .filter(|key| seen.insert((key.host.clone(), key.port, key.name.clone())))
            .collect();
        Net {
            wire: OnceLock::new(),
            offline: options.offline,
            exp: options.exp,
            typed: address::Typed::default(),
            credentials,
            jobs: Arc::new(Semaphore::new(options.jobs.max(1))),
            hosts: Mutex::new(HashMap::new()),
            paces: pace::Paces::default(),
            rate: None,
            idle: IDLE,
            split: Split {
                when: options.split,
                above: download::SPLIT_ABOVE,
                piece: download::SMALLEST_PIECE,
                window: download::WINDOW,
                slow: download::SLOW,
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
    /// server. only the test endpoints debug builds read need it.
    #[cfg(debug_assertions)]
    pub(crate) fn typed(&self, url: &Url) {
        self.typed.add(url);
    }

    /// trusts the hosts data.toml names, since a person wrote them too.
    pub(crate) fn trust(&self, manifest_hosts: impl IntoIterator<Item = String>) {
        for host in manifest_hosts {
            self.typed.add_host(&host);
        }
    }

    /// the hosts a credential goes to, for tests.
    #[cfg(test)]
    pub(crate) fn credential_hosts(&self) -> Vec<&str> {
        self.credentials
            .iter()
            .map(|key| key.host.as_str())
            .collect()
    }

    /// the experiment settings supplied for this run.
    pub(crate) fn exp(&self) -> &Experiments {
        &self.exp
    }

    pub(crate) fn offline(&self) -> Offline {
        self.offline
    }

    /// a GET to `url`, shaped by `shape`, with redirects followed one hop at a time, so that each
    /// hop is checked the way its connection will be made before it is sent, and waits while its
    /// host's rate limit is used up. a credential goes only to its own host, and never once a
    /// redirect has left the first one, except a saved login, which answers its own host on any
    /// hop as curl's does.
    async fn send(
        &self,
        url: &Url,
        shape: impl Fn(reqwest::RequestBuilder) -> reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, Failure> {
        self.send_on(Lane::Shared, url, shape).await
    }

    fn link_ranges(&self) -> bool {
        self.exp().on("NET_SPLIT_LINK_RATE")
            && self.split.when != Splitting::Never
            && self.rate.is_none()
    }

    fn probe_split(&self, validators: &crate::cache::Validators) -> bool {
        self.link_ranges() && headers::if_range(validators).is_some()
    }

    async fn send_body(
        &self,
        url: &Url,
        shape: impl Fn(reqwest::RequestBuilder) -> reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, Failure> {
        let response = self.send_on(Lane::Apart, url, shape).await?;
        tracing::debug!(host = host(url), version = ?response.version(), "file response transport");
        Ok(response)
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
        #[cfg(debug_assertions)]
        let local = api::test_bucket(url);
        #[cfg(debug_assertions)]
        let url = if let Some(local) = &local {
            self.typed(local);
            local
        } else {
            url
        };
        let wire = self.wire().map_err(Failure::Final)?;
        let client = wire.lane(lane).map_err(Failure::Final)?;
        let mut hop = url.clone();
        let mut redirects = 0;
        let mut name = None;
        let mut first_origin = true;
        loop {
            wire.proxies
                .check(&hop, &self.typed)
                .await
                .map_err(|reason| refused(&hop, reason))?;
            first_origin &= hop.origin() == url.origin();
            let ticket = self.pace(&hop).await.map_err(Failure::Final)?;
            let request = shape(self.request(client, &hop, first_origin));
            let mut response = tokio::select! {
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
                if let Some(name) = name {
                    response.extensions_mut().insert(RedirectName(name));
                }
                return Ok(response);
            };
            name = headers::file_name(response.headers()).or(name);
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
    /// host and port, over https only. a key for port 443 never goes to another port of its
    /// host, as curl's CVE-2022-27776 let one.
    fn request(
        &self,
        client: &reqwest::Client,
        url: &Url,
        credentials: bool,
    ) -> reqwest::RequestBuilder {
        let mut request = client.get(url.clone());
        if url.scheme() == "https"
            && let Some(host) = url.host_str()
        {
            let port = url.port_or_known_default();
            for key in &self.credentials {
                if (credentials || key.every_hop)
                    && port == Some(key.port)
                    && host.eq_ignore_ascii_case(&key.host)
                {
                    // the header name only; the value is the secret
                    tracing::debug!(host, header = %key.name, "sending a stored credential");
                    request = request.header(key.name.clone(), key.value.clone());
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
    async fn permit(&self, url: &Url) -> Result<Permits, Error> {
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

    async fn keep_permit(&self, url: &Url, permits: &mut Option<Permits>) -> Result<(), Error> {
        if permits.is_none() {
            *permits = Some(self.permit(url).await?);
        }
        Ok(())
    }

    async fn after_body<T>(
        &self,
        permits: &mut Option<Permits>,
        finish: impl Future<Output = T>,
    ) -> T {
        drop(permits.take());
        finish.await
    }

    /// a slot for this host and one overall, when both are free and nothing waits for them, so
    /// an added stream never holds up another file.
    fn try_permit(&self, url: &Url) -> Option<Permits> {
        let host = self.slots(url).try_acquire_owned().ok()?;
        let all = self.jobs.clone().try_acquire_owned().ok()?;
        Some((host, all))
    }

    fn slots(&self, url: &Url) -> Arc<Semaphore> {
        let mut hosts = self.hosts.lock().unwrap_or_else(PoisonError::into_inner);
        hosts
            .entry(host(url).to_ascii_lowercase())
            .or_insert_with(|| {
                let cap = if self.link_ranges() {
                    LINK_STREAMS
                } else {
                    PER_HOST
                };
                Arc::new(Semaphore::new(cap))
            })
            .clone()
    }
}

/// a redirect may name the download even when the final storage response does not.
#[derive(Clone, Debug)]
struct RedirectName(String);

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
            exp: crate::exp::Experiments::default(),
            offline: Offline::No,
            jobs: 1,
            credentials: Vec::new(),
            split: Splitting::WhenSlow,
        };
        let net = Net::new(options, Arc::default(), Arc::default(), Arc::new(|_| {}));
        assert!(net.wire.get().is_none());
        net.wire().unwrap();
        assert!(net.wire.get().is_some());
    }

    #[test]
    fn a_host_gets_one_value_per_header_and_the_first_wins() {
        let credential = |value: &str| Credential {
            host: "HuggingFace.co".to_owned(),
            header: "authorization",
            value: value.to_owned(),
            every_hop: false,
        };
        let options = Options {
            exp: crate::exp::Experiments::default(),
            offline: Offline::No,
            jobs: 1,
            credentials: vec![credential("new"), credential("old")],
            split: Splitting::Never,
        };
        let net = Net::new(options, Arc::default(), Arc::default(), Arc::new(|_| {}));
        let url = Url::parse("https://huggingface.co/api/datasets/org/data").unwrap();
        let request = net
            .request(&net.wire().unwrap().client, &url, true)
            .build()
            .unwrap();
        let sent: Vec<_> = request
            .headers()
            .get_all(header::AUTHORIZATION)
            .iter()
            .collect();
        assert_eq!(sent, ["new"]);
    }

    /// a sign-in cookie, as earthdata hands out, reaches the added streams of a split download
    /// too.
    #[test]
    fn both_clients_keep_cookies_in_one_jar() {
        use crate::source::recorded::block_on;
        use std::io::{Read, Write};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        let serving = std::thread::spawn(move || {
            let mut heads = Vec::new();
            for cookie in ["Set-Cookie: session=abc; Path=/\r\n", ""] {
                let (mut stream, _) = listener.accept().unwrap();
                let mut head = Vec::new();
                let mut byte = [0; 1];
                while !head.ends_with(b"\r\n\r\n")
                    && stream.read(&mut byte).is_ok_and(|read| read == 1)
                {
                    head.push(byte[0]);
                }
                heads.push(String::from_utf8_lossy(&head).to_ascii_lowercase());
                let answer = format!(
                    "HTTP/1.1 200 OK\r\n{cookie}Content-Length: 0\r\nConnection: close\r\n\r\n"
                );
                stream.write_all(answer.as_bytes()).unwrap();
            }
            heads
        });
        let options = Options {
            exp: crate::exp::Experiments::default(),
            offline: Offline::No,
            jobs: 1,
            credentials: Vec::new(),
            split: Splitting::WhenSlow,
        };
        let net = Net::new(options, Arc::default(), Arc::default(), Arc::new(|_| {}));
        net.typed(&url);
        assert!(block_on(net.send(&url, |request| request)).is_ok());
        assert!(block_on(net.send_body(&url, |request| request)).is_ok());
        let heads = serving.join().unwrap();
        assert!(heads[1].contains("cookie: session=abc"), "{}", heads[1]);
    }

    #[test]
    fn request_slots_are_shared_across_paths_and_ports_of_a_host() {
        for jobs in [1, 8] {
            let net = Net::new(
                Options {
                    exp: Experiments::default(),
                    offline: Offline::No,
                    jobs,
                    credentials: Vec::new(),
                    split: Splitting::Never,
                },
                Arc::default(),
                Arc::default(),
                Arc::new(|_| {}),
            );
            let url = Url::parse("https://example.org/one").unwrap();
            assert_eq!(net.slots(&url).available_permits(), PER_HOST);
            let held: Vec<_> = std::iter::from_fn(|| net.try_permit(&url)).collect();
            assert_eq!(held.len(), PER_HOST.min(jobs));
            let other = Url::parse("https://EXAMPLE.org:8443/two").unwrap();
            assert!(net.try_permit(&other).is_none());
        }
    }

    #[test]
    fn finished_bodies_release_slots_before_the_publication_future_runs() {
        use crate::source::recorded::block_on;

        let net = Net::new(
            Options {
                exp: Experiments::default(),
                offline: Offline::No,
                jobs: 2,
                credentials: Vec::new(),
                split: Splitting::Never,
            },
            Arc::default(),
            Arc::default(),
            Arc::new(|_| {}),
        );
        let url = Url::parse("https://example.org/file").unwrap();
        block_on(async {
            let mut permits = Some(net.permit(&url).await.unwrap());
            assert_eq!(net.jobs.available_permits(), 1);
            assert_eq!(net.slots(&url).available_permits(), 3);
            let value = net
                .after_body(&mut permits, async {
                    assert_eq!(net.jobs.available_permits(), 2);
                    assert_eq!(net.slots(&url).available_permits(), 4);
                    "published"
                })
                .await;
            assert_eq!(value, "published");
            assert!(permits.is_none());
        });
    }
}
