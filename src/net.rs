//! the network: bounded HTTP work, polite retries, resumable downloads, and rules about where a
//! request may go and which credentials go with it.

mod address;
mod api;
mod download;
mod failure;
mod google;
mod headers;
mod pace;
mod rate;
#[cfg(all(unix, not(target_vendor = "apple"), not(target_os = "android")))]
mod roots;
mod shown;
pub(crate) mod sign;

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use reqwest::StatusCode;
use reqwest::header::{self, HeaderName, HeaderValue};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use url::Url;

pub(crate) use address::Allowed;
pub(crate) use api::{Api, Response};
pub(crate) use download::{Divert, Event, Expect, Expired, Fetch, Fetched, Origin, Resumable};
pub(crate) use failure::{MOST_RETRIES, MOVED_HELP, RETRIES, offline as offline_missing};
#[cfg(debug_assertions)]
pub(crate) use google::test_token_endpoint;
pub(crate) use google::{Authorized, Google};
pub(crate) use rate::Rate;
pub(crate) use shown::{Shown, mask_query, secret};

use self::failure::Failure;
use crate::error::{Error, Kind};
use crate::stop::Stop;
use crate::work::Work;

/// how long a connection may take to open, unless `FETCHLOOM_TIMEOUT` asks for longer.
const CONNECT: Duration = Duration::from_secs(45);
const REDIRECTS: usize = 10;
/// an error's text with the causes beneath it, which some errors, such as reqwest's builder
/// error, leave out of their own.
fn with_causes(err: &dyn std::error::Error) -> String {
    let mut text = err.to_string();
    let mut cause = err.source();
    while let Some(current) = cause {
        let said = current.to_string();
        if !text.contains(&said) {
            text = format!("{text}: {said}");
        }
        cause = current.source();
    }
    text
}

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
    /// a request to `host` failed for `reason` and goes out again after `wait`, as try `next`
    /// of `of`.
    Retrying {
        host: String,
        reason: String,
        wait: Duration,
        next: u32,
        of: u32,
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

    /// what to do offline about a new link, whose SHA-256 only a download can teach, as a
    /// project or a command line gives links. `get`, which pins nothing, words its own.
    pub(crate) fn unlearned_help(self) -> String {
        format!(
            "{} to pin it. `{}` reuses a file fetched before",
            self.retry(),
            crate::cli::command("get <link> --offline")
        )
    }

    /// what to do when offline work needs bytes the cache lacks: go online, or use a cache that
    /// holds `them`, as a CI job restores one.
    pub(crate) fn missing_help(self, them: &str) -> String {
        format!(
            "{} to download {them}, or set FETCHLOOM_CACHE to a cache folder that already holds {them}",
            self.retry()
        )
    }
}

pub(crate) struct Options {
    pub(crate) offline: Offline,
    /// downloads running at once, across all hosts.
    pub(crate) jobs: usize,
    pub(crate) credentials: Vec<Credential>,
    /// when a large download is split across streams.
    pub(crate) split: Splitting,
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
    /// which requests to S3 buckets go signed, and with which keys.
    signing: crate::source::Signing,
    /// which requests to cloud storage buckets carry a google token.
    google: Google,
    /// buckets that refused this run an old version of an object whose version as it is now
    /// matched its pin: they likely refuse every old version, so their objects are asked for as
    /// they are now first.
    refuse_versions: Mutex<std::collections::HashSet<crate::source::Bucket>>,
    /// in mirror-only mode, the web mirrors' addresses, each ending in `/`: a request goes only
    /// below one of them, and only as a mirror's request.
    mirrors_only: Option<Vec<Url>>,
    jobs: Arc<Semaphore>,
    hosts: Mutex<HashMap<String, Arc<Semaphore>>>,
    paces: pace::Paces,
    rate: Option<Arc<Rate>>,
    split: Split,
    /// how long a request waits for an answer, after its connection opens, and how long an
    /// answer may stay silent.
    idle: Duration,
    connect: Duration,
    /// requests running at once to one host, so no server sees fetchloom as a flood.
    host_jobs: usize,
    /// tries after the first that a request gets once it stops making progress.
    retries: u32,
    /// the most the wait before a first retry takes, which tests of broken answers set to zero.
    first_wait: Duration,
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
    /// a client for each lane, and for each reach its lookups keep, each made when first
    /// needed, where [`Wire::slot`] puts it.
    clients: [OnceLock<Result<reqwest::Client, Error>>; 4],
    /// the cookies every client sends, so a sign-in cookie reaches the added streams too.
    cookies: Arc<reqwest::cookie::Jar>,
    typed: address::Typed,
    offline: bool,
    connect: Duration,
    proxies: address::Proxies,
}

/// which client sends a request.
#[derive(Clone, Copy)]
enum Lane {
    Shared,
    /// for file bodies and added ranges: HTTP/1.1, so each request has a connection of its own
    /// instead of sharing one with every other request to the host.
    Apart,
}

impl Wire {
    /// the proxies, once the settings are known to name proxies, and no client yet.
    fn new(typed: &address::Typed, offline: bool, connect: Duration) -> Result<Wire, Error> {
        let proxies = address::Proxies::read(&|name| std::env::var_os(name))?;
        // the process may already have a provider, which is fine
        let _ = rustls::crypto::ring::default_provider().install_default();
        Ok(Wire {
            clients: std::array::from_fn(|_| OnceLock::new()),
            cookies: Arc::default(),
            typed: typed.clone(),
            offline,
            connect,
            proxies,
        })
    }

    fn client(&self, lane: Lane, reach: address::Reach) -> Result<reqwest::Client, Error> {
        let builder = reqwest::Client::builder()
            .user_agent(concat!("fetchloom/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(self.connect)
            .dns_resolver(Arc::new(address::Resolver::new(
                self.typed.clone(),
                self.offline,
                reach,
            )))
            // `send` follows redirects itself, so that each hop is checked before it goes out
            .redirect(reqwest::redirect::Policy::none())
            // earthdata's archives sign a person in through a redirect to urs.earthdata.nasa.gov,
            // which hands back a cookie the archive then asks for. cookies live for this run only
            .cookie_provider(self.cookies.clone());
        let builder = match lane {
            Lane::Shared => builder,
            Lane::Apart => builder.http1_only(),
        };
        #[cfg(all(unix, not(target_vendor = "apple"), not(target_os = "android")))]
        let builder = builder.tls_backend_preconfigured(roots::tls(matches!(lane, Lane::Apart))?);
        builder.build().map_err(|err| {
            Error::new(
                Kind::NetworkUnreachable,
                format!("could not start the network client: {}", with_causes(&err)),
            )
        })
    }

    /// the client of `lane` for a request to `url`: one whose lookups may keep private addresses
    /// when the origin of `url` is trusted, else one whose lookups keep public addresses alone.
    fn lane(&self, lane: Lane, url: &Url) -> Result<&reqwest::Client, Error> {
        let reach = if self.typed.trusts(url) {
            address::Reach::Trusted
        } else {
            address::Reach::Public
        };
        self.clients[Wire::slot(lane, reach)]
            .get_or_init(|| self.client(lane, reach))
            .as_ref()
            .map_err(Clone::clone)
    }

    /// where [`Wire::clients`] keeps the client of `lane` and `reach`.
    fn slot(lane: Lane, reach: address::Reach) -> usize {
        match (lane, reach) {
            (Lane::Shared, address::Reach::Public) => 0,
            (Lane::Shared, address::Reach::Trusted) => 1,
            (Lane::Apart, address::Reach::Public) => 2,
            (Lane::Apart, address::Reach::Trusted) => 3,
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
            typed: address::Typed::default(),
            credentials,
            signing: crate::source::Signing::default(),
            google: Google::default(),
            refuse_versions: Mutex::default(),
            mirrors_only: None,
            jobs: Arc::new(Semaphore::new(options.jobs.max(1))),
            hosts: Mutex::new(HashMap::new()),
            paces: pace::Paces::default(),
            rate: None,
            idle: crate::settings::TIMEOUT,
            connect: CONNECT,
            host_jobs: crate::settings::HOST_JOBS,
            retries: RETRIES,
            first_wait: failure::FIRST_WAIT,
            split: download::starting(options.split),
            work,
            stop,
            notify,
        }
    }

    /// the client, made once even when the first requests start together. a client that could
    /// not be made stays an error, since nothing about a retry would differ.
    fn wire(&self) -> Result<&Wire, Error> {
        self.wire
            .get_or_init(|| Wire::new(&self.typed, self.offline.enabled(), self.connect))
            .as_ref()
            .map_err(Clone::clone)
    }

    /// trusts the origin of a URL a person wrote, so it may be a private address such as a lab
    /// server. only the test endpoints debug builds read need it.
    #[cfg(debug_assertions)]
    pub(crate) fn typed(&self, url: &Url) {
        self.typed.add(url);
    }

    /// trusts origins a person gave on the command line or in the settings, such as a dataverse
    /// install on a university network or a mirror on a lab server, each alone: another port or
    /// scheme of its host gets no more trust than a host in data.toml, which a cloned project
    /// could point at this machine's network.
    pub(crate) fn trust(&self, origins: impl IntoIterator<Item = url::Origin>) {
        self.typed.allow(origins);
    }

    /// signs requests for the S3 buckets `signing` lists, once S3 refused them without a key.
    pub(crate) fn with_signing(mut self, signing: crate::source::Signing) -> Net {
        self.signing = signing;
        self
    }

    /// sends a google token with requests for the cloud storage buckets `google` lists, once
    /// cloud storage refused them without one.
    pub(crate) fn with_google(mut self, google: Google) -> Net {
        self.google = google;
        self
    }

    /// sends requests to the web mirrors at `bases` alone, each ending in `/`, and only for what
    /// a mirror gives, as the `mirrors_only` setting asks.
    pub(crate) fn with_mirrors_only(mut self, bases: Vec<Url>) -> Net {
        self.mirrors_only = Some(bases);
        self
    }

    /// refuses `url` in mirror-only mode unless it is a `mirror`'s request below a mirror's
    /// address: the same scheme, host and port, and a path inside the mirror's. a hop a redirect
    /// gave names the origin that sent it, which may be a mirror on the same host.
    fn may_ask(&self, url: &Url, mirror: bool, sent_by: Option<&Url>) -> Result<(), Failure> {
        let Some(bases) = &self.mirrors_only else {
            return Ok(());
        };
        let below =
            |base: &Url| url.origin() == base.origin() && url.path().starts_with(base.path());
        if mirror && bases.iter().any(below) {
            return Ok(());
        }
        let asked = match sent_by {
            Some(from) => format!(
                "{}, where {} sent it",
                url.origin().ascii_serialization(),
                from.origin().ascii_serialization()
            ),
            None => host(url).to_owned(),
        };
        Err(Failure::Final(
            Error::new(
                Kind::NetworkRefused,
                format!(
                    "fetchloom will not ask {asked}, since mirrors_only lets it ask its mirrors alone"
                ),
            )
            .help("ask whoever keeps the mirrors to add what is missing, or turn off mirrors_only in the settings file or FETCHLOOM_MIRRORS_ONLY"),
        ))
    }

    /// the hosts a credential goes to, for tests.
    #[cfg(test)]
    pub(crate) fn credential_hosts(&self) -> Vec<&str> {
        self.credentials
            .iter()
            .map(|key| key.host.as_str())
            .collect()
    }

    pub(crate) fn offline(&self) -> Offline {
        self.offline
    }

    /// [`Net::send_on`] for what a source gives, on the shared client.
    #[cfg(test)]
    async fn send(
        &self,
        url: &Url,
        shape: impl Fn(reqwest::RequestBuilder) -> reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, Failure> {
        self.send_on(Lane::Shared, url, false, shape).await
    }

    /// [`Net::send_on`] for a file body, which a `mirror` may be asked for in mirror-only mode.
    async fn send_body(
        &self,
        url: &Url,
        mirror: bool,
        shape: impl Fn(reqwest::RequestBuilder) -> reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, Failure> {
        let response = self.send_on(Lane::Apart, url, mirror, shape).await?;
        tracing::debug!(host = host(url), version = ?response.version(), "file response transport");
        Ok(response)
    }

    /// [`Net::send_on`] on a connection of the request's own, for the added streams of a split
    /// download.
    async fn send_apart(
        &self,
        url: &Url,
        mirror: bool,
        shape: impl Fn(reqwest::RequestBuilder) -> reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, Failure> {
        self.send_on(Lane::Apart, url, mirror, shape).await
    }

    /// a GET to `url`, shaped by `shape`, with redirects followed one hop at a time, so that each
    /// hop is checked the way its connection will be made before it is sent, and waits while its
    /// host's rate limit is used up. a credential goes only to its own host, and never once a
    /// redirect has left the first one, except a saved login, which answers its own host on any
    /// hop as curl's does. a request for an S3 bucket that `s3_buckets` lists goes again once,
    /// signed, when S3 refuses it without a key, and signed from the start once it did; keys that
    /// a program could not give fail the request rather than let it go without them, and a
    /// redirect never carries the signature on. a request for a cloud storage bucket that
    /// `gcs_buckets` lists does the same with a google token, and goes once more with a new
    /// token when cloud storage refuses one with a 401. in mirror-only mode every hop must be a
    /// `mirror`'s request below a mirror's address, checked before any name is looked up.
    async fn send_on(
        &self,
        lane: Lane,
        url: &Url,
        mirror: bool,
        shape: impl Fn(reqwest::RequestBuilder) -> reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, Failure> {
        self.may_ask(url, mirror, None)?;
        let refused = |url: &Url, reason| Failure::Final(failure::refused(url, &reason));
        // the bucket is judged on the link as written, before a test server stands in for it
        let bucket = self.signing.locate(url);
        let stored = self.google.locate(url);
        #[cfg(debug_assertions)]
        let local = api::stand_in(url).inspect(|local| self.typed(local));
        #[cfg(debug_assertions)]
        let url = local.as_ref().unwrap_or(url);
        let wire = self.wire().map_err(Failure::Final)?;
        let mut hop = url.clone();
        let mut sent_by: Option<Url> = None;
        let mut redirects = 0;
        let mut name = None;
        let mut first_origin = true;
        let mut asked_signed = false;
        loop {
            self.may_ask(&hop, mirror, sent_by.as_ref())?;
            wire.proxies
                .check(&hop, &self.typed)
                .await
                .map_err(|reason| refused(&hop, reason))?;
            let carrier = wire.proxies.carrier(&hop);
            // each hop picks its client, since only a trusted origin's lookups keep private
            // addresses
            let client = wire.lane(lane, &hop).map_err(Failure::Final)?;
            first_origin &= hop.origin() == url.origin();
            // a signature goes with the first request alone, never after a redirect
            let located = bucket.as_ref().filter(|_| redirects == 0);
            let signer = self
                .signing
                .signer(located, SystemTime::now(), &self.stop)
                .await
                .map_err(Failure::Final)?;
            let tokened = self.google_bearer(stored.as_ref(), redirects).await?;
            let ticket = self.pace(&hop).await.map_err(Failure::Final)?;
            let request = match &signer {
                Some(signer) => {
                    tracing::debug!(host = host(&hop), "signing the request with an AWS key");
                    signed(
                        shape(client.get(hop.clone())),
                        signer.headers(&hop, SystemTime::now()),
                    )
                    .map_err(Failure::Final)?
                }
                None => shape(self.request(client, &hop, first_origin)).headers(tokened.headers()),
            };
            // counted as it goes: one refused before is no request, and a redirect's hop is part
            // of the request it follows
            if redirects == 0 {
                Work::add(&self.work.requests, 1);
            }
            let mut response = self.sent(wire, &hop, carrier.as_ref(), request).await?;
            let limits = pace::limits(response.status(), response.headers(), SystemTime::now());
            pace::Paces::heard(ticket, limits, Instant::now());
            if let Some(located) = located
                && signer.is_none()
                && !asked_signed
                && matches!(
                    response.status(),
                    StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
                )
            {
                let region = response
                    .headers()
                    .get("x-amz-bucket-region")
                    .and_then(|value| value.to_str().ok());
                if self
                    .signing
                    .refused(located, region)
                    .map_err(Failure::Final)?
                {
                    tracing::debug!(url = %Shown(&hop), "S3 refused the request without a key, so it goes again signed");
                    asked_signed = true;
                    continue;
                }
            }
            if let Some(located) = located {
                response
                    .extensions_mut()
                    .insert(located.keyed(signer.as_ref()));
            }
            if self.google_again(&tokened, &mut response).await? {
                continue;
            }
            let Some(next) = redirect_target(&hop, &response) else {
                if let Some(name) = name {
                    response.extensions_mut().insert(RedirectName(name));
                }
                return Ok(response);
            };
            name = headers::file_name(response.headers()).or(name);
            redirects += 1;
            if redirects > REDIRECTS {
                let reason = address::Refused::new(format!("more than {REDIRECTS} redirects"));
                return Err(refused(url, reason));
            }
            self.typed
                .check(&next)
                .map_err(|reason| refused(&next, reason))?;
            tracing::debug!(from = host(&hop), to = host(&next), "redirect");
            sent_by = Some(std::mem::replace(&mut hop, next));
        }
    }

    /// sends `request` for `hop`, which `carrier` carries when a proxy does: the answer, or why
    /// none came. a proxy that cannot be reached is named rather than the site behind it.
    async fn sent(
        &self,
        wire: &Wire,
        hop: &Url,
        carrier: Option<&address::Carrier>,
        request: reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, Failure> {
        let deadline = self.connect + self.idle;
        let response = tokio::select! {
            sent = tokio::time::timeout(deadline, request.send()) => match sent {
                Err(_) => return Err(failure::transient(format!("{} did not answer within {} s", host(hop), deadline.as_secs()))),
                Ok(Err(err)) => return Err(unanswered(wire, hop, carrier, &err, self.offline).await),
                Ok(Ok(response)) => response,
            },
            (carrier, reason) = watched(wire, carrier) => {
                return Err(Failure::Final(failure::proxy_down(hop, carrier, &reason)));
            }
            () = self.stop.asked() => return Err(Failure::Final(failure::interrupted(hop))),
        };
        if let Some(carrier) = carrier {
            wire.proxies.reached(carrier);
        }
        Ok(response)
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
            if let Some(wait) = hold.told(now) {
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
        let closed = |_| failure::interrupted(url);
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
            .or_insert_with(|| Arc::new(Semaphore::new(self.host_jobs)))
            .clone()
    }
}

/// `request` with the headers that sign it. the authorization and the session token are kept
/// out of logs as secrets are.
fn signed(
    mut request: reqwest::RequestBuilder,
    headers: Vec<(&'static str, String)>,
) -> Result<reqwest::RequestBuilder, Error> {
    for (name, value) in headers {
        let Ok(mut value) = HeaderValue::from_str(&value) else {
            return Err(Error::new(
                Kind::AuthRequired,
                "an AWS key or session token holds a character no request header can carry, such as a line break",
            )
            .help("check the AWS keys in the environment or the profile"));
        };
        value.set_sensitive(matches!(name, "authorization" | "x-amz-security-token"));
        request = request.header(name, value);
    }
    Ok(request)
}

/// waits while a request through `carrier` has no answer, ending only once the proxy is found
/// unreachable, with why. without a proxy it never ends.
async fn watched<'c>(
    wire: &Wire,
    carrier: Option<&'c address::Carrier>,
) -> (&'c address::Carrier, String) {
    match carrier {
        Some(carrier) => (carrier, wire.proxies.watch(carrier).await),
        None => std::future::pending().await,
    }
}

/// a request that failed before any answer came. when the proxy carrying it cannot be reached,
/// the proxy is named rather than the site, and no other try is made.
async fn unanswered(
    wire: &Wire,
    url: &Url,
    carrier: Option<&address::Carrier>,
    err: &reqwest::Error,
    offline: Offline,
) -> Failure {
    if let Some(carrier) = carrier
        && failure::before_proxy(err)
        && let Some(reason) = wire.proxies.unreachable(carrier).await
    {
        return Failure::Final(failure::proxy_down(url, carrier, &reason));
    }
    failure::classify(url, carrier, err, offline)
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
mod scripted;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_that_sends_nothing_loads_no_tls_roots() {
        let options = Options {
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
            offline: Offline::No,
            jobs: 1,
            credentials: vec![credential("new"), credential("old")],
            split: Splitting::Never,
        };
        let net = Net::new(options, Arc::default(), Arc::default(), Arc::new(|_| {}));
        let url = Url::parse("https://huggingface.co/api/datasets/org/data").unwrap();
        let request = net
            .request(
                net.wire().unwrap().lane(Lane::Shared, &url).unwrap(),
                &url,
                true,
            )
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
    /// an AWS key or session token holding a character no header can carry, such as a line break,
    /// is refused before the request goes, rather than sent without the header it spoils.
    #[test]
    fn a_signature_no_request_header_can_carry_is_refused() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let headers = vec![
            ("x-amz-date", "20241001T100000Z".to_owned()),
            ("x-amz-security-token", "first line\nsecond line".to_owned()),
        ];
        let request = client.get("https://bucket.s3.amazonaws.com/data.csv");
        let Err(error) = signed(request, headers) else {
            panic!("a session token with a line break was taken");
        };
        assert_eq!(error.kind, Kind::AuthRequired);
        assert_eq!(
            error.message,
            "an AWS key or session token holds a character no request header can carry, such as a line break"
        );
        assert_eq!(
            error.help.as_deref(),
            Some("check the AWS keys in the environment or the profile")
        );
    }

    #[test]
    fn both_clients_keep_cookies_in_one_jar() {
        use crate::net::scripted::{reply, serve};
        use crate::source::recorded::block_on;

        let (url, asked) = serve(vec![
            reply("200 OK", &[("Set-Cookie", "session=abc; Path=/")], b""),
            reply("200 OK", &[], b""),
        ]);
        let options = Options {
            offline: Offline::No,
            jobs: 1,
            credentials: Vec::new(),
            split: Splitting::WhenSlow,
        };
        let net = Net::new(options, Arc::default(), Arc::default(), Arc::new(|_| {}));
        net.typed(&url);
        assert!(block_on(net.send(&url, |request| request)).is_ok());
        assert!(block_on(net.send_body(&url, false, |request| request)).is_ok());
        let heads = asked.lock().unwrap();
        assert!(
            heads[1]
                .to_ascii_lowercase()
                .contains("cookie: session=abc"),
            "{}",
            heads[1]
        );
    }

    #[test]
    fn an_error_names_the_causes_its_own_text_leaves_out() {
        #[derive(Debug)]
        struct Layer(&'static str, Option<Box<Layer>>);
        impl std::fmt::Display for Layer {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.0)
            }
        }
        impl std::error::Error for Layer {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                self.1.as_deref().map(|layer| layer as _)
            }
        }
        let root = Layer("No CA certificates were loaded from the system", None);
        let middle = Layer("builder error", Some(Box::new(root)));
        assert_eq!(
            with_causes(&middle),
            "builder error: No CA certificates were loaded from the system"
        );
        // a cause the text already holds is not said twice
        let echoed = Layer(
            "could not connect: refused",
            Some(Box::new(Layer("refused", None))),
        );
        assert_eq!(with_causes(&echoed), "could not connect: refused");
    }

    #[test]
    fn request_slots_are_shared_across_paths_and_ports_of_a_host() {
        for (jobs, host_jobs) in [(1, 4), (8, 4), (32, 16)] {
            let net = Net::new(
                Options {
                    offline: Offline::No,
                    jobs,
                    credentials: Vec::new(),
                    split: Splitting::Never,
                },
                Arc::default(),
                Arc::default(),
                Arc::new(|_| {}),
            );
            let net = if host_jobs == crate::settings::HOST_JOBS {
                net
            } else {
                net.with_host_jobs(host_jobs)
            };
            let url = Url::parse("https://example.org/one").unwrap();
            assert_eq!(net.slots(&url).available_permits(), host_jobs);
            let held: Vec<_> = std::iter::from_fn(|| net.try_permit(&url)).collect();
            assert_eq!(held.len(), host_jobs.min(jobs));
            let other = Url::parse("https://EXAMPLE.org:8443/two").unwrap();
            assert!(net.try_permit(&other).is_none());
        }
    }

    #[test]
    fn finished_bodies_release_slots_before_the_publication_future_runs() {
        use crate::source::recorded::block_on;

        let net = Net::new(
            Options {
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

    fn mirror_only(bases: &[&str]) -> Net {
        let options = Options {
            offline: Offline::No,
            jobs: 1,
            credentials: Vec::new(),
            split: Splitting::Never,
        };
        Net::new(options, Arc::default(), Arc::default(), Arc::new(|_| {}))
            .with_mirrors_only(bases.iter().map(|base| Url::parse(base).unwrap()).collect())
    }

    /// in mirror-only mode a request goes to a mirror's address alone, and only as a mirror's:
    /// any other is refused before a name is looked up, so a host that does not exist is refused
    /// rather than unknown.
    #[test]
    fn mirror_only_refuses_other_hosts_before_any_lookup() {
        use crate::source::recorded::block_on;

        let net = mirror_only(&["https://mirror.invalid/fetchloom/"]);
        for (link, mirror) in [
            ("https://source.invalid/data.csv", false),
            ("https://mirror.invalid/fetchloom/objects/ab/cd", false),
            ("https://mirror.invalid/admin", true),
            ("https://mirror.invalid:8443/fetchloom/objects/ab/cd", true),
            ("http://mirror.invalid/fetchloom/objects/ab/cd", true),
        ] {
            let url = Url::parse(link).unwrap();
            let sent = block_on(net.send_on(Lane::Shared, &url, mirror, |request| request));
            let Err(Failure::Final(error)) = sent else {
                panic!("{link} was not refused");
            };
            assert_eq!(
                error.kind,
                Kind::NetworkRefused,
                "{link}: {}",
                error.message
            );
            assert!(error.message.contains("mirrors_only"), "{}", error.message);
        }
    }

    #[test]
    fn mirror_only_refuses_a_redirect_off_the_mirror() {
        use crate::net::scripted::{reply, serve};
        use crate::source::recorded::block_on;

        let (url, asked) = serve(vec![
            reply("302 Found", &[("Location", "/elsewhere/file")], b""),
            reply("200 OK", &[], b"never"),
        ]);
        let base = url.join("m/").unwrap();
        let net = mirror_only(&[base.as_str()]);
        net.typed(&url);
        let marker = base.join("fetchloom-mirror").unwrap();
        let sent = block_on(net.send_on(Lane::Shared, &marker, true, |request| request));
        let Err(Failure::Final(error)) = sent else {
            panic!("the redirect off the mirror was followed");
        };
        assert_eq!(error.kind, Kind::NetworkRefused);
        assert_eq!(asked.lock().unwrap().len(), 1);
    }
}
