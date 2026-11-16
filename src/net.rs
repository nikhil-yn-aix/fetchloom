//! the network: bounded HTTP work, polite retries, resumable downloads, and rules about where a
//! request may go and which credentials go with it.

mod address;
mod api;
mod azure;
mod cookies;
mod download;
mod endpoint;
pub(crate) mod estimate;
mod failure;
pub(crate) mod google;
mod headers;
mod metadata;
mod opened;
mod pace;
mod rate;
mod relink;
#[cfg(all(unix, not(target_vendor = "apple"), not(target_os = "android")))]
mod roots;
mod rules;
pub(crate) mod s3;
pub(crate) mod sign;
pub(crate) mod site;
pub(crate) mod speeds;

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, LazyLock, Mutex, OnceLock, PoisonError};
use std::time::{Duration, SystemTime};

use reqwest::StatusCode;
use reqwest::header::{self, HeaderName, HeaderValue};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use url::Url;

pub(crate) use address::{Allowed, is_private, looks_internal};
#[cfg(test)]
pub(crate) use api::too_large as too_large_answer;
pub(crate) use api::{Api, Extra, MOST_FILES, Response, Through};
#[cfg(test)]
pub(crate) use azure::Given;
#[cfg(debug_assertions)]
pub(crate) use azure::test_identity_endpoint;
pub(crate) use azure::{Azure, Sent};
pub(crate) use download::{
    Divert, Event, Expect, Expired, Fetch, Fetched, LFS_LARGEST, Origin, Probed, Resumable,
};
pub(crate) use endpoint::Endpoint;
pub(crate) use failure::{MOST_RETRIES, MOVED_HELP, RETRIES, offline as offline_missing};
#[cfg(debug_assertions)]
pub(crate) use google::test_token_endpoint;
pub(crate) use google::{Authorized, DriveAnswered, Google, Private};
pub(crate) use headers::NoVersion;
pub(crate) use rate::Rate;
pub(crate) use rules::{Relink, Rules};

use self::failure::Failure;
use crate::auth::Credential;
use crate::error::{Error, Kind};
use crate::shown::Shown;
use crate::stop::Stop;
use crate::work::Work;

/// how long a connection may take to open, unless `FETCHLOOM_TIMEOUT` asks for longer.
const CONNECT: Duration = Duration::from_secs(45);
const REDIRECTS: usize = 10;
/// the most of a refusal's body read to learn why it refused: S3's error documents take a few
/// hundred bytes.
const LARGEST_REFUSAL: usize = 64 << 10;
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

/// something the person watching should hear about while the network works.
pub(crate) enum Notice {
    /// a request to `host` for `what`, the name a file's progress line shows when the caller gave
    /// one, else its link's last part, failed for `reason` and goes out again after `wait`, as try
    /// `next` of `of`, `waited` of the tries before it having waited for the host's rate limit.
    Retrying {
        host: String,
        what: String,
        reason: String,
        wait: Duration,
        next: u32,
        of: u32,
        waited: u32,
        /// the file came on in an earlier try, which earned the tries back, so more follow
        /// while it keeps coming and `of` counts only those without progress.
        open: bool,
    },
    /// requests to `host` wait for its rate limit to reset. `quota` and `window` are the limit,
    /// when the host stated them.
    Waiting {
        host: String,
        quota: Option<u64>,
        window: Option<Duration>,
        wait: Duration,
    },
    /// this machine's network is gone, and requests wait up to `longest` for it to come back.
    NetworkGone { longest: Duration },
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
            crate::suggested::command("get <link> --offline")
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

/// requests running at once to one host, so no server sees fetchloom as a flood.
pub(crate) const HOST_JOBS: usize = 4;
/// how long a request may wait for an answer, or a download stay silent, before it is tried again.
pub(crate) const TIMEOUT: Duration = Duration::from_secs(30);

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

struct Holding<'a> {
    key: &'a Url,
    permits: &'a mut Option<Permits>,
    owned: bool,
}

impl<'a> Holding<'a> {
    fn new(key: &'a Url, permits: &'a mut Option<Permits>) -> Self {
        let owned = permits.is_some();
        Self {
            key,
            permits,
            owned,
        }
    }

    fn borrow(&mut self) -> Holding<'_> {
        Holding {
            key: self.key,
            permits: &mut *self.permits,
            owned: self.owned,
        }
    }
}

pub(crate) struct Net {
    /// made on the first request, so a run that never goes online loads no TLS roots and reads no
    /// proxy settings.
    wire: OnceLock<Result<Wire, Error>>,
    offline: Offline,
    metadata: api::Memo,
    typed: address::Typed,
    /// the keys requests may carry, looked for when the first request needs one, so a run that
    /// sends none reads no key files and asks no credential store.
    credentials: Arc<LazyLock<Vec<Key>, FindKeys>>,
    /// the tokens, headers and cookies people saved for websites, looked for as `credentials`.
    sites: site::Sites,
    /// how each download hashes its bytes, which the run's settings give.
    hashing: crate::cache::Hashing,
    /// what the sources say about requests to their hosts.
    rules: Rules,
    /// which requests to S3 buckets go signed, and with which keys.
    signing: s3::Signing,
    /// which requests to cloud storage buckets carry a google token.
    google: Google,
    /// which requests to azure containers carry a SAS.
    azure: Azure,
    /// buckets that refused an old version this run, though the object as it is now matched its
    /// pin: they likely refuse every old version, so their objects are asked for as they are now.
    refuse_versions: Mutex<std::collections::HashSet<s3::Bucket>>,
    /// in mirror-only mode, the web mirrors' addresses, each ending in `/`: a request goes only
    /// below one of them, and only as a mirror's request.
    mirrors_only: Option<Vec<Url>>,
    /// the S3 mirrors' buckets, which a mirror's request in mirror-only mode may also ask, in
    /// whichever region S3 keeps them.
    mirror_buckets: Vec<s3::Bucket>,
    /// the mirror of huggingface.co `HF_ENDPOINT` names, where every request for huggingface.co
    /// goes instead.
    hf_endpoint: Option<Endpoint>,
    jobs: Arc<Semaphore>,
    hosts: Mutex<HashMap<String, Arc<Semaphore>>>,
    paces: pace::Paces,
    /// what the run heard from any host, which tells a lost network from one host down.
    outage: failure::Outage,
    /// how long requests wait for a lost network before their tries count again.
    outage_wait: Duration,
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
    /// how much further a try must take its file to count toward no limit, which tests set
    /// smaller than a real download moves.
    moving: u64,
    /// the most files one dataset may list, which every listing asks through [`Api`].
    most_files: usize,
    work: Arc<Work>,
    stop: Arc<Stop>,
    notify: Notify,
    /// what this run's transfers tell of each host's speed, which the run writes as it ends.
    heard: Arc<speeds::Run>,
    #[cfg(test)]
    lookups: Option<address::Lookups>,
}

/// how long an answer took from its request to its headers, every hop of it, without the waits
/// for a host's pace before each.
#[derive(Clone, Copy)]
struct Answering(Duration);

/// how a run finds the keys requests may carry, once the first request needs them.
type FindKeys = Box<dyn FnOnce() -> Vec<Key> + Send>;

/// a credential as requests carry it: to one host and port.
struct Key {
    host: String,
    port: u16,
    name: HeaderName,
    value: HeaderValue,
    every_hop: bool,
    every_port: bool,
}

/// `credentials` as requests carry them. one value per header, host and port: the first, so a
/// key just signed in with wins over an older one for the same host.
fn keys(credentials: Vec<Credential>) -> Vec<Key> {
    let mut seen = std::collections::HashSet::new();
    credentials
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
                every_port: credential.every_port,
            })
        })
        .filter(|key| seen.insert((key.host.clone(), key.port, key.every_port, key.name.clone())))
        .collect()
}

/// the host and port a credential's host names: lowercase, with port 443 unless it names
/// another. none when it is no plain host.
fn key_origin(host: &str) -> Option<(String, u16)> {
    let url = crate::auth::plain_host(&format!("https://{host}/"))?;
    Some((url.host_str()?.to_owned(), url.port_or_known_default()?))
}

/// the HTTP clients and the proxies their requests go through.
struct Wire {
    /// a client for each lane, and for each reach its lookups keep, each made when first
    /// needed, where [`Wire::slot`] puts it.
    clients: [OnceLock<Result<reqwest::Client, Error>>; 4],
    /// the cookies every client sends, so a sign-in cookie reaches the added streams too.
    cookies: Arc<cookies::Cookies>,
    typed: address::Typed,
    offline: bool,
    connect: Duration,
    proxies: Arc<address::Proxies>,
    #[cfg(test)]
    lookups: Option<address::Lookups>,
}

/// which client sends a request.
#[derive(Clone, Copy)]
enum Lane {
    Shared,
    Metadata,
    /// for file bodies and added ranges: HTTP/1.1, so each request has a connection of its own
    /// instead of sharing one with every other request to the host.
    Apart,
}

impl Wire {
    /// the proxies, once the settings are known to name proxies, and no client yet.
    fn new(
        typed: &address::Typed,
        offline: bool,
        connect: Duration,
        cookies: cookies::Cookies,
    ) -> Result<Wire, Error> {
        let proxies = Arc::new(address::Proxies::read(&|name| std::env::var_os(name))?);
        // the process may already have a provider, which is fine
        let _ = rustls::crypto::ring::default_provider().install_default();
        let (by, through) = (typed.clone(), Arc::clone(&proxies));
        let cookies = cookies.carrying(move |url| site::carried(&by, &through, url));
        Ok(Wire {
            clients: std::array::from_fn(|_| OnceLock::new()),
            cookies: Arc::new(cookies),
            typed: typed.clone(),
            offline,
            connect,
            proxies,
            #[cfg(test)]
            lookups: None,
        })
    }

    fn client(&self, lane: Lane, reach: address::Reach) -> Result<reqwest::Client, Error> {
        let builder = reqwest::Client::builder()
            .user_agent(concat!("fetchloom/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(self.connect)
            .connector_layer(opened::Watching)
            .dns_resolver(Arc::new(self.resolver(reach)))
            // `send` follows redirects itself, so that each hop is checked before it goes out
            .redirect(reqwest::redirect::Policy::none())
            // earthdata's archives sign a person in through a redirect to urs.earthdata.nasa.gov,
            // which hands back a cookie the archive then asks for. cookies live for this run only
            .cookie_provider(self.cookies.clone());
        let builder = match lane {
            Lane::Shared | Lane::Metadata => builder,
            Lane::Apart => builder.http1_only(),
        };
        let builder = if self.proxies.direct() {
            builder.no_proxy()
        } else {
            builder
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

    /// the lookups of a client whose `reach` they keep.
    fn resolver(&self, reach: address::Reach) -> address::Resolver {
        let resolver = address::Resolver::new(self.typed.clone(), self.offline, reach);
        #[cfg(test)]
        let resolver = resolver.answering(self.lookups.clone());
        resolver
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
            (Lane::Shared | Lane::Metadata, address::Reach::Public) => 0,
            (Lane::Shared | Lane::Metadata, address::Reach::Trusted) => 1,
            (Lane::Apart, address::Reach::Public) => 2,
            (Lane::Apart, address::Reach::Trusted) => 3,
        }
    }
}

/// what a bucket answered one of a push's requests: its status, its headers and its body.
pub(crate) struct Answered {
    pub(crate) status: u16,
    pub(crate) headers: reqwest::header::HeaderMap,
    pub(crate) body: Vec<u8>,
}

impl Net {
    pub(crate) fn new(options: Options, work: Arc<Work>, stop: Arc<Stop>, notify: Notify) -> Net {
        let given = options.credentials;
        Net {
            wire: OnceLock::new(),
            offline: options.offline,
            metadata: api::Memo::new(crate::machine::LEAST_MEMORY / 16),
            typed: address::Typed::default(),
            credentials: Arc::new(LazyLock::new(Box::new(move || keys(given)))),
            sites: site::Sites::default(),
            hashing: crate::cache::Hashing::default(),
            rules: Rules::default(),
            signing: s3::Signing::default(),
            google: Google::default(),
            azure: Azure::default(),
            refuse_versions: Mutex::default(),
            mirrors_only: None,
            mirror_buckets: Vec::new(),
            hf_endpoint: None,
            jobs: Arc::new(Semaphore::new(options.jobs.max(1))),
            hosts: Mutex::new(HashMap::new()),
            paces: pace::Paces::default(),
            outage: failure::Outage::default(),
            outage_wait: failure::LONGEST_OUTAGE,
            rate: None,
            idle: TIMEOUT,
            connect: CONNECT,
            host_jobs: HOST_JOBS,
            retries: RETRIES,
            first_wait: failure::FIRST_WAIT,
            moving: failure::MOVING,
            most_files: MOST_FILES,
            split: download::starting(options.split),
            work,
            stop,
            notify,
            heard: Arc::default(),
            #[cfg(test)]
            lookups: None,
        }
    }

    /// the network, telling `heard` how fast each host answers and sends.
    pub(crate) fn hearing(mut self, heard: Arc<speeds::Run>) -> Net {
        self.heard = heard;
        self
    }

    /// the keys requests may carry, from `find` in place of those the options gave, looked for
    /// when the first request needs one.
    pub(crate) fn finding_credentials(
        mut self,
        find: impl FnOnce() -> Vec<Credential> + Send + 'static,
    ) -> Net {
        self.credentials = Arc::new(LazyLock::new(Box::new(move || keys(find()))));
        self
    }

    /// the client, made once even when the first requests start together. a client that could
    /// not be made stays an error, since nothing about a retry would differ.
    fn wire(&self) -> Result<&Wire, Error> {
        self.wire
            .get_or_init(|| {
                let keys = Arc::clone(&self.credentials);
                let cookies = self.sites.cookies(move |host| {
                    keys.iter().any(|key| host.eq_ignore_ascii_case(&key.host))
                });
                let wire = Wire::new(&self.typed, self.offline.enabled(), self.connect, cookies);
                #[cfg(test)]
                let wire = wire.map(|wire| Wire {
                    lookups: self.lookups.clone(),
                    ..wire
                });
                wire
            })
            .as_ref()
            .map_err(Clone::clone)
    }

    /// trusts the origin of a URL a person wrote, so it may be a private address such as a lab
    /// server. only the test endpoints debug builds read need it.
    #[cfg(debug_assertions)]
    pub(crate) fn typed(&self, url: &Url) {
        self.typed.add(url);
    }

    /// trusts origins a person gave on the command line or in the settings, such as a lab server,
    /// each alone: another port or scheme of its host is trusted no more than a host in data.toml.
    pub(crate) fn trust(&self, origins: impl IntoIterator<Item = url::Origin>) {
        self.typed.allow(origins);
    }

    /// follows the sources' `rules` about requests to their hosts.
    pub(crate) fn with_rules(mut self, rules: Rules) -> Net {
        self.paces = pace::Paces::spaced_by(rules.spacing);
        self.rules = rules;
        self
    }

    /// the name requests to `url`'s host are paced and counted under, which hosts limited
    /// together share, as arXiv's do.
    pub(crate) fn paced(&self, url: &Url) -> String {
        self.rules.paced(url)
    }

    /// hashes each download as `hashing` says, the run's batches and checksum processors.
    pub(crate) fn with_hashing(mut self, hashing: crate::cache::Hashing) -> Net {
        self.hashing = hashing;
        self
    }

    /// signs requests for the S3 buckets `signing` lists, once S3 refused them without a key.
    pub(crate) fn with_signing(mut self, signing: s3::Signing) -> Net {
        self.signing = signing;
        self
    }

    /// sends a google token with requests for the cloud storage buckets `google` lists, once
    /// cloud storage refused them without one.
    pub(crate) fn with_google(mut self, google: Google) -> Net {
        self.google = google;
        self.metadata.invalidate();
        self
    }

    /// sends a SAS with requests for the azure containers `azure` lists, once azure refused
    /// them without one.
    pub(crate) fn with_azure(mut self, azure: Azure) -> Net {
        self.azure = azure;
        self.metadata.invalidate();
        self
    }

    /// sends requests to the web mirrors at `bases` alone, each ending in `/`, and only for what
    /// a mirror gives, as the `mirrors_only` setting asks.
    pub(crate) fn with_mirrors_only(mut self, bases: Vec<Url>) -> Net {
        self.mirrors_only = Some(bases);
        self
    }

    /// lets a mirror's request in mirror-only mode ask the S3 mirrors' `buckets` too.
    pub(crate) fn with_mirror_buckets(mut self, buckets: Vec<s3::Bucket>) -> Net {
        self.mirror_buckets = buckets;
        self
    }

    /// sends every request for huggingface.co to `endpoint` instead, whose origin is trusted as a
    /// mirror a person named is: on a private address, and in mirror-only mode. a request there
    /// carries only the keys saved for its own host, never hugging face's.
    pub(crate) fn with_hf_endpoint(mut self, endpoint: Option<Endpoint>) -> Net {
        if let Some(endpoint) = &endpoint {
            self.typed.allow([endpoint.origin()]);
        }
        self.hf_endpoint = endpoint;
        self
    }

    /// where a request for `url` goes: below the mirror `HF_ENDPOINT` names, for a link to
    /// huggingface.co.
    fn through_endpoint(&self, url: &Url) -> Option<Url> {
        self.hf_endpoint.as_ref()?.rewrite(url)
    }

    /// refuses `url` in mirror-only mode unless it is a `mirror`'s request below a mirror's
    /// address, the same origin and a path inside it, or in an S3 mirror's bucket. a refused
    /// redirect names its sender.
    fn may_ask(&self, url: &Url, mirror: bool, sent_by: Option<&Url>) -> Result<(), Failure> {
        let Some(bases) = &self.mirrors_only else {
            return Ok(());
        };
        if self
            .hf_endpoint
            .as_ref()
            .is_some_and(|endpoint| endpoint.holds(url))
        {
            return Ok(());
        }
        let below =
            |base: &Url| url.origin() == base.origin() && url.path().starts_with(base.path());
        let in_bucket =
            || s3::Bucket::holding(url).is_some_and(|bucket| self.mirror_buckets.contains(&bucket));
        if mirror && (bases.iter().any(below) || in_bucket()) {
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
            .help("ask whoever keeps the mirrors to add what is missing, or run `fetchloom settings unset mirrors_only` or unset FETCHLOOM_MIRRORS_ONLY, whichever turned it on, to ask sources too"),
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

    /// answers and bytes this run received from any host: when it has not changed between two
    /// moments, the network was silent between them.
    fn heard(&self) -> u64 {
        self.outage.answers().wrapping_add(
            self.work
                .bytes_down
                .load(std::sync::atomic::Ordering::Relaxed),
        )
    }

    /// [`Net::send_on`] for what a source gives, on the shared client.
    #[cfg(test)]
    async fn send(
        &self,
        url: &Url,
        shape: impl Fn(reqwest::RequestBuilder) -> reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, Failure> {
        self.send_on(Lane::Shared, url, false, shape, None).await
    }

    /// [`Net::send_on`] for a file body, which a `mirror` may be asked for in mirror-only mode.
    async fn send_body(
        &self,
        url: &Url,
        mirror: bool,
        shape: impl Fn(reqwest::RequestBuilder) -> reqwest::RequestBuilder,
        holding: Option<Holding<'_>>,
    ) -> Result<reqwest::Response, Failure> {
        let response = self
            .send_on(Lane::Apart, url, mirror, shape, holding)
            .await?;
        tracing::debug!(host = host(url), version = ?response.version(), "file response transport");
        Ok(response)
    }

    /// one request a push sends to an S3 bucket a person named, and its answer, whose body may
    /// hold at most `most` bytes. `make` builds the request on the client for `url`, or in debug
    /// builds for the test server standing in for it, and signs it for that link. no redirect is
    /// followed and no stored credential added. the request fails once `moved`, the bytes of its
    /// body sent so far, stays still as long as an answer may take, so a large body may take its
    /// time. a failure a retry may get past is tried again, as a download's is, and a stop ends
    /// it; unless `heeding` is false, as for the request that cleans up after a stop, which goes
    /// once whatever is asked and leaves its answer unread.
    ///
    /// # Errors
    ///
    /// offline, an address this run may not ask, no answer after the retries, a body past
    /// `most`, or a stop.
    pub(crate) async fn send_to_bucket(
        &self,
        url: &Url,
        heeding: bool,
        most: usize,
        moved: &std::sync::atomic::AtomicU64,
        make: impl Fn(&reqwest::Client, &Url) -> Result<reqwest::RequestBuilder, Error>,
    ) -> Result<Answered, Error> {
        if self.offline.enabled() {
            return Err(Error::new(
                Kind::OfflineMissing,
                format!("offline, and this needs an answer from {}", host(url)),
            )
            .help(self.offline.retry()));
        }
        self.typed
            .check(url)
            .map_err(|reason| failure::refused(url, &reason))?;
        #[cfg(debug_assertions)]
        let local = api::stand_in(url).inspect(|local| self.typed(local));
        #[cfg(debug_assertions)]
        let url = local.as_ref().unwrap_or(url);
        let wire = self.wire()?;
        if !heeding {
            let (client, _) = self
                .checked_client(wire, Lane::Apart, url)
                .await
                .map_err(|failure| failure.into_error(Kind::NetworkUnreachable))?;
            let request = make(client, url)?;
            Work::add(&self.work.requests, 1);
            let deadline = self.connect + self.idle;
            let response = tokio::time::timeout(deadline, request.send())
                .await
                .map_err(|_| {
                    Error::new(
                        Kind::NetworkUnreachable,
                        format!(
                            "{} did not answer within {} s",
                            host(url),
                            deadline.as_secs()
                        ),
                    )
                })?
                .map_err(|err| {
                    Error::new(
                        Kind::NetworkUnreachable,
                        format!("could not reach {}: {}", host(url), with_causes(&err)),
                    )
                })?;
            return Ok(Answered {
                status: response.status().as_u16(),
                headers: response.headers().clone(),
                body: Vec::new(),
            });
        }
        let mut retry = failure::Retry::new(self, url);
        loop {
            let permits = self.permit(url).await?;
            let tried = async {
                let (client, carrier) = self.checked_client(wire, Lane::Apart, url).await?;
                let request = make(client, url).map_err(Failure::Final)?;
                Work::add(&self.work.requests, 1);
                let quiet = self.connect + self.idle;
                let mut response = tokio::select! {
                    sent = request.send() => match sent {
                        Ok(response) => response,
                        Err(err) => return Err(unanswered(wire, url, carrier.as_ref(), &err, self.offline).await),
                    },
                    () = still(moved, quiet) => return Err(failure::transient(format!("{} took nothing and said nothing for {} s", host(url), quiet.as_secs()))),
                    () = self.stop.asked() => return Err(Failure::Final(failure::interrupted(url))),
                };
                self.outage.answered();
                if failure::is_transient(response.status()) {
                    return Err(failure::refusal(
                        url,
                        response.status(),
                        response.headers(),
                        self.rules.account_at,
                    ));
                }
                let too_large = || {
                    Error::new(
                        Kind::SourceInvalid,
                        format!("{} sent more than a short answer", host(url)),
                    )
                };
                let body = self
                    .read_body(url, &mut response, "the answer", most, too_large, |_| {
                        std::future::ready(Ok(()))
                    })
                    .await?;
                Ok(Answered {
                    status: response.status().as_u16(),
                    headers: response.headers().clone(),
                    body,
                })
            };
            let failure = match tried.await {
                Ok(answered) => return Ok(answered),
                Err(failure) => failure,
            };
            drop(permits);
            retry.after(failure, false).await?;
        }
    }

    /// a GET to `url`, shaped by `shape`, following redirects one checked hop at a time and
    /// waiting out its host's rate limit. a credential goes to its own host alone, and past a
    /// redirect only when it answers every hop, as a saved login does; the answer is marked
    /// [`Carried`] when one went with any hop. the trust boundaries section of
    /// docs/dev/architecture/network.md has the rest.
    #[expect(
        clippy::too_many_lines,
        reason = "each hop's checks, credentials and answer stay in order in one loop"
    )]
    async fn send_on(
        &self,
        lane: Lane,
        url: &Url,
        mirror: bool,
        shape: impl Fn(reqwest::RequestBuilder) -> reqwest::RequestBuilder,
        mut holding: Option<Holding<'_>>,
    ) -> Result<reqwest::Response, Failure> {
        // before the address check and the keys, so both judge the mirror's own host
        let mirrored = self.through_endpoint(url);
        let url = mirrored.as_ref().unwrap_or(url);
        self.may_ask(url, mirror, None)?;
        // the bucket is judged on the link as written, before a test server stands in for it
        let bucket = self.signing.locate(url);
        let stored = self.google.locate(url);
        let container = self.azure.locate(url);
        // judged on the link as written too, before a test server stands in for it
        let in_the_clear = (!self.carries(url)).then(|| s3::in_the_clear(url));
        // a link its source serves only with a key goes nowhere without one
        if let Some(refusal) = (self.rules.keyless)(url).filter(|_| !self.sends_key(url)) {
            return Err(Failure::Final(refusal));
        }
        // stored keys, the origin they stay on and a source's media type are judged on each hop
        // as written, before a test server stands in for it, as on the host itself
        let written = url;
        #[cfg(debug_assertions)]
        let local = api::stand_in(url).inspect(|local| self.typed(local));
        #[cfg(debug_assertions)]
        let url = local.as_ref().unwrap_or(url);
        let wire = self.wire().map_err(Failure::Final)?;
        let mut hop = url.clone();
        let mut written_hop = written.clone();
        let mut sent_by: Option<Url> = None;
        let mut redirects = 0;
        let mut name = None;
        let mut first_origin = true;
        let mut asked_signed = false;
        let mut renewed = false;
        let (mut site_sent, mut answering, mut keyed) = (None, Duration::ZERO, false);
        loop {
            self.may_ask(&hop, mirror, sent_by.as_ref())?;
            let (client, carrier) = self.checked_client(wire, lane, &hop).await?;
            first_origin &= written_hop.origin() == written.origin();
            // a signature goes with the first request alone, never after a redirect
            let located = bucket.as_ref().filter(|_| redirects == 0);
            let signer = self.signer_for(located, in_the_clear.as_ref()).await?;
            let tokened = self.google_bearer(stored.as_ref(), redirects).await?;
            let blobbed = self.azure_hop(container.as_ref(), redirects).await?;
            let ticket = self
                .pace_holding(&hop, holding.as_mut())
                .await
                .map_err(Failure::Final)?;
            let credentials = (signer.as_ref(), &tokened, &blobbed);
            let (request, sends_key) = self.hop_request(
                client,
                (&hop, &written_hop),
                &shape,
                first_origin,
                credentials,
            )?;
            keyed |= sends_key;
            // counted as it goes: one refused before is no request, and a redirect's hop is part
            // of the request it follows
            if redirects == 0 {
                Work::add(&self.work.requests, 1);
            }
            let mut response = self
                .sent(
                    wire,
                    &hop,
                    carrier.as_ref(),
                    request,
                    matches!(lane, Lane::Metadata),
                )
                .await?;
            let limits = pace::limits(response.status(), response.headers(), SystemTime::now());
            let at = tokio::time::Instant::now().into_std();
            answering += at.saturating_duration_since(ticket.sent);
            let answer = pace::Answered {
                status: response.status(),
                sent: ticket.sent,
                at,
                share: fastrand::f64(),
            };
            self.paces.answered(&self.paced(&hop), &answer);
            pace::Paces::heard(ticket, limits, at);
            self.mark_site(&mut response, &hop, first_origin, &mut site_sent);
            if !renewed
                && self
                    .keys_expired(signer.as_ref(), &hop, &mut response)
                    .await?
            {
                renewed = true;
                continue;
            }
            if let Some(located) = located
                && signer.is_none()
                && !asked_signed
                && self
                    .signing
                    .refused_by(located, &response)
                    .map_err(Failure::Final)?
            {
                tracing::debug!(url = %Shown(&hop), "S3 refused the request without a key, so it goes again signed");
                asked_signed = true;
                continue;
            }
            if let Some(located) = located {
                response
                    .extensions_mut()
                    .insert(located.keyed(signer.as_ref()));
            }
            if self.google_again(&tokened, &mut response).await?
                || self.azure_again(&blobbed, &mut response).await?
            {
                continue;
            }
            let Some((next, written_next)) =
                self.next_hop(url, &hop, &response, &mut redirects, &mut name)?
            else {
                if let Some(name) = name {
                    response.extensions_mut().insert(RedirectName(name));
                }
                if keyed {
                    response.extensions_mut().insert(Carried);
                }
                response.extensions_mut().insert(Answering(answering));
                return Ok(response);
            };
            written_hop = written_next;
            sent_by = Some(std::mem::replace(&mut hop, next));
        }
    }

    /// the keys and region that sign `located`'s request, when any do, refused when the request
    /// would carry them in the clear, as `in_the_clear` says.
    async fn signer_for(
        &self,
        located: Option<&s3::Located>,
        in_the_clear: Option<&Error>,
    ) -> Result<Option<s3::Signer>, Failure> {
        let signer = self
            .signing
            .signer(located, SystemTime::now(), &self.stop)
            .await
            .map_err(Failure::Final)?;
        match (signer, in_the_clear) {
            (Some(_), Some(error)) => Err(Failure::Final(error.clone())),
            (signer, _) => Ok(signer),
        }
    }

    async fn checked_client<'w>(
        &self,
        wire: &'w Wire,
        lane: Lane,
        hop: &Url,
    ) -> Result<(&'w reqwest::Client, Option<address::Carrier>), Failure> {
        wire.proxies
            .check(hop, &self.typed)
            .await
            .map_err(|reason| Failure::Final(failure::refused(hop, &reason)))?;
        let carrier = wire.proxies.carrier(hop);
        let client = wire.lane(lane, hop).map_err(Failure::Final)?;
        Ok((client, carrier))
    }

    /// where `response`, the answer to `hop`, sends the request next, once checked: the link to
    /// ask, and the link as written, which a test server's stand-in replaces in debug builds.
    fn next_hop(
        &self,
        from: &Url,
        hop: &Url,
        response: &reqwest::Response,
        redirects: &mut usize,
        name: &mut Option<String>,
    ) -> Result<Option<(Url, Url)>, Failure> {
        let Some(next) = redirect_target(hop, response) else {
            return Ok(None);
        };
        if !matches!(next.scheme(), "http" | "https") {
            return Err(Failure::Final(elsewhere(hop, &next)));
        }
        *name = headers::file_name(response.headers()).or_else(|| name.take());
        *redirects += 1;
        if *redirects > REDIRECTS {
            let reason = address::Refused::new(format!("more than {REDIRECTS} redirects"));
            return Err(Failure::Final(failure::refused(from, &reason)));
        }
        self.typed
            .check(&next)
            .map_err(|reason| Failure::Final(failure::refused(&next, &reason)))?;
        tracing::debug!(from = host(hop), to = host(&next), "redirect");
        #[cfg(debug_assertions)]
        if let Some(local) = api::stand_in(&next).inspect(|local| self.typed(local)) {
            return Ok(Some((local, next)));
        }
        Ok(Some((next.clone(), next)))
    }

    /// sends `request` for `hop`, which `carrier` carries when a proxy does: the answer, or why
    /// none came. a proxy that cannot be reached is named rather than the site behind it.
    async fn sent(
        &self,
        wire: &Wire,
        hop: &Url,
        carrier: Option<&address::Carrier>,
        request: reqwest::RequestBuilder,
        metadata: bool,
    ) -> Result<reqwest::Response, Failure> {
        let sent = metadata
            .then(|| api::Sent::capture(wire, hop, &request))
            .flatten();
        let opening = Arc::new(opened::Opening::default());
        let asked = tokio::time::Instant::now();
        let mut response = tokio::select! {
            sent = opening.watching(request.send()) => match sent {
                Err(err) => return Err(unanswered(wire, hop, carrier, &err, self.offline).await),
                Ok(response) => response,
            },
            () = overdue(&opening, asked, self.idle) => {
                return Err(failure::transient(format!("{} did not answer within {} s", host(hop), self.idle.as_secs())));
            }
            (carrier, reason) = watched(wire, carrier) => {
                return Err(Failure::Final(failure::proxy_down(hop, carrier, &reason)));
            }
            () = self.stop.asked() => return Err(Failure::Final(failure::interrupted(hop))),
        };
        self.outage.answered();
        if let Some(carrier) = carrier {
            wire.proxies.reached(carrier);
        }
        if let Some(sent) = sent {
            response.extensions_mut().insert(sent);
        }
        Ok(response)
    }

    /// the request for `hop`, as `shape` makes it: signed by `signer`, or else with the stored
    /// credential of its host as `written` names it while on the `first_origin`, a google token
    /// and an azure credential, and in the media type its source serves the link's bytes in, and
    /// whether any credential of this machine goes with it.
    fn hop_request(
        &self,
        client: &reqwest::Client,
        (hop, written): (&Url, &Url),
        shape: &impl Fn(reqwest::RequestBuilder) -> reqwest::RequestBuilder,
        first_origin: bool,
        (signer, tokened, blobbed): (
            Option<&s3::Signer>,
            &google::Tokened<'_>,
            &azure::Blobbed<'_>,
        ),
    ) -> Result<(reqwest::RequestBuilder, bool), Failure> {
        if let Some(signer) = signer {
            tracing::debug!(host = host(hop), "signing the request with an AWS key");
            let request = signed(
                shape(client.get(hop.clone())),
                signer.headers(hop, SystemTime::now()),
            )
            .map_err(Failure::Final)?;
            return Ok((request, true));
        }
        let (request, stored) = self.request(client, &blobbed.url(hop), written, first_origin);
        let request = shape(request)
            .headers(tokened.headers())
            .headers(blobbed.headers());
        let request = match (self.rules.accept)(written) {
            Some(accept) => request.headers(header::HeaderMap::from_iter([(
                header::ACCEPT,
                HeaderValue::from_static(accept),
            )])),
            None => request,
        };
        let request = blobbed
            .signed(request, SystemTime::now())
            .map_err(Failure::Final)?;
        Ok((
            request,
            stored || tokened.has_credential() || blobbed.has_credential(),
        ))
    }

    /// whether S3 refused `response` because the keys a program gave for `signer` expired, which
    /// are then forgotten. the refusal's body is read to learn why, then put back to read again.
    async fn keys_expired(
        &self,
        signer: Option<&s3::Signer>,
        hop: &Url,
        response: &mut reqwest::Response,
    ) -> Result<bool, Failure> {
        let Some(signer) = signer.filter(|signer| signer.renews()) else {
            return Ok(false);
        };
        if !matches!(
            response.status(),
            StatusCode::BAD_REQUEST | StatusCode::FORBIDDEN
        ) {
            return Ok(false);
        }
        let taken = std::mem::replace(response, http::Response::new(Vec::new()).into());
        let body;
        (*response, body) = self.read_back(taken, LARGEST_REFUSAL).await?;
        if !s3::Signer::says_expired(response.status().as_u16(), &body) {
            return Ok(false);
        }
        tracing::debug!(url = %Shown(hop), "S3 said the keys expired, so the request goes again with new ones");
        signer.expired().await;
        Ok(true)
    }

    /// the body of `response`, up to `most` bytes, and `response` made again around them with its
    /// status, headers and extensions, so whoever reads it next reads the same answer, cut there.
    async fn read_back(
        &self,
        mut response: reqwest::Response,
        most: usize,
    ) -> Result<(reqwest::Response, Vec<u8>), Failure> {
        use reqwest::ResponseBuilderExt as _;
        let url = response.url().clone();
        let mut body = Vec::new();
        while body.len() < most {
            let chunk = tokio::select! {
                chunk = tokio::time::timeout(self.idle, response.chunk()) => chunk,
                () = self.stop.asked() => return Err(Failure::Final(failure::interrupted(&url))),
            };
            match chunk {
                Err(_) => {
                    return Err(failure::transient(format!(
                        "{} sent nothing for {} s",
                        host(&url),
                        self.idle.as_secs()
                    )));
                }
                Ok(Err(err)) => {
                    return Err(failure::transient(format!(
                        "the connection to {} broke: {err}",
                        host(&url)
                    )));
                }
                Ok(Ok(None)) => break,
                Ok(Ok(Some(chunk))) => body.extend_from_slice(&chunk),
            }
        }
        body.truncate(most);
        let (mut parts, _) = http::Response::from(response).into_parts();
        let marked = http::Response::builder().url(url).body(()).map_err(|err| {
            Failure::Final(Error::new(
                Kind::LocalIo,
                format!("could not keep an answer to read again: {err}"),
            ))
        })?;
        parts.extensions.extend(marked.into_parts().0.extensions);
        let again = http::Response::from_parts(parts, body.clone());
        Ok((reqwest::Response::from(again), body))
    }

    /// a GET for `url` with, when `credentials` allows, the credential for this exact host and
    /// port, over https, or plain http where [`Net::carries`] allows, so a key for port 443 never
    /// reaches another port (curl's CVE-2022-27776); only a `.netrc` line without a port answers
    /// every port, as curl reads one. a website's own saved credential wins over a stored one for
    /// the same header. a stored key is judged on `written`, the link as written, which a test
    /// server's stand-in replaces in debug builds. it says whether a stored credential or a
    /// website's went with the request.
    fn request(
        &self,
        client: &reqwest::Client,
        url: &Url,
        written: &Url,
        credentials: bool,
    ) -> (reqwest::RequestBuilder, bool) {
        let mut request = client.get(url.clone());
        let site = self.site_headers(url, credentials);
        let mut carried = !site.is_empty();
        for key in self.stored_keys(written, credentials, &site) {
            // the header name only; the value is the secret
            tracing::debug!(host = key.host, header = %key.name, "sending a stored credential");
            request = request.header(key.name.clone(), key.value.clone());
            carried = true;
        }
        for (name, value) in site {
            request = request.header(name, value);
        }
        (request, carried)
    }

    /// the stored keys a request for `url` carries, when `credentials` allows, beside `site`, the
    /// headers a website's saved credential adds, whose names win.
    fn stored_keys(
        &self,
        url: &Url,
        credentials: bool,
        site: &[(HeaderName, HeaderValue)],
    ) -> Vec<&Key> {
        let mut keys: Vec<&Key> = Vec::new();
        let Some(host) = url.host_str().filter(|_| self.carries(url)) else {
            return keys;
        };
        let port = url.port_or_known_default();
        // a key for this very port goes before one for every port of the host
        for every_port in [false, true] {
            for key in self
                .credentials
                .iter()
                .filter(|key| key.every_port == every_port)
            {
                if (credentials || key.every_hop)
                    && (every_port || port == Some(key.port))
                    && host.eq_ignore_ascii_case(&key.host)
                    && !site.iter().any(|(name, _)| *name == key.name)
                    && !keys.iter().any(|sent| sent.name == key.name)
                {
                    keys.push(key);
                }
            }
        }
        keys
    }

    /// whether a request that starts at `url` carries a key, as [`Net::request`] sends one: a
    /// stored one, or a website's saved credential.
    pub(crate) fn sends_key(&self, url: &Url) -> bool {
        self.site_of(url).is_some() || !self.stored_keys(url, true, &[]).is_empty()
    }

    /// waits while a rate limit of the URL's host is used up, telling the person watching once
    /// for each reset, then counts the request as on its way.
    async fn pace(&self, url: &Url) -> Result<pace::Ticket, Error> {
        self.pace_holding(url, None).await
    }

    async fn pace_holding(
        &self,
        url: &Url,
        mut holding: Option<&mut Holding<'_>>,
    ) -> Result<pace::Ticket, Error> {
        let name = self.paced(url);
        loop {
            self.stop.check()?;
            if self.paces.busy(&name) {
                return Err(failure::busy(url));
            }
            if let Some(holding) = holding.as_mut()
                && holding.owned
                && holding.permits.is_none()
            {
                self.keep_permit(holding.key, holding.permits).await?;
            }
            let now = tokio::time::Instant::now().into_std();
            let hold = match self.paces.admit(&name, now) {
                Ok(ticket) => return Ok(ticket),
                Err(hold) => hold,
            };
            if let Some(holding) = holding.as_mut() {
                Self::release_for_wait(holding.permits);
            }
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

    fn release_for_wait(permits: &mut Option<Permits>) {
        drop(permits.take());
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
        self.stop.check()?;
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
        let name = self.paced(url);
        let most = (self.rules.most_at_once)(&name)
            .map_or(self.host_jobs, |most| most.min(self.host_jobs));
        let mut hosts = self.hosts.lock().unwrap_or_else(PoisonError::into_inner);
        hosts
            .entry(name)
            .or_insert_with(|| Arc::new(Semaphore::new(most)))
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

/// resolves once the answer to a request `asked` then is overdue, `idle` after its connection
/// opened as `opening` tells, and never while that connection is still opening, which the
/// connect time bounds. a connection opening is looked at again each second.
async fn overdue(opening: &opened::Opening, asked: tokio::time::Instant, idle: Duration) {
    loop {
        match opening.answer_by(asked, idle) {
            Some(by) if tokio::time::Instant::now() >= by => return,
            Some(by) => tokio::time::sleep_until(by).await,
            None => tokio::time::sleep(Duration::from_secs(1)).await,
        }
    }
}

/// resolves once `moved` has not changed for `quiet`, checked each `quiet`.
async fn still(moved: &std::sync::atomic::AtomicU64, quiet: Duration) {
    let mut seen = moved.load(std::sync::atomic::Ordering::Relaxed);
    loop {
        tokio::time::sleep(quiet).await;
        let now = moved.load(std::sync::atomic::Ordering::Relaxed);
        if now == seen {
            return;
        }
        seen = now;
    }
}

/// a request that failed before any answer came. when the proxy carrying it cannot be reached,
/// even one that answered before, the proxy is named rather than the site, and no other try is
/// made.
async fn unanswered(
    wire: &Wire,
    url: &Url,
    carrier: Option<&address::Carrier>,
    err: &reqwest::Error,
    offline: Offline,
) -> Failure {
    if let Some(carrier) = carrier
        && failure::before_proxy(err)
        && let Some(reason) = wire.proxies.unreachable_now(carrier).await
    {
        return Failure::Final(failure::proxy_down(url, carrier, &reason));
    }
    failure::classify(url, carrier, err, offline)
}

/// a redirect may name the download even when the final storage response does not.
#[derive(Clone, Debug)]
struct RedirectName(String);

/// marks an answer whose request, or a hop before it, carried a credential of this machine: a
/// saved key, a `.netrc` login, a site's sign-in or the cookies it set, an S3 signature or a
/// google or azure credential. a download it brings is marked in the cache, so push and the web
/// archives treat its file as one that may not be public.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Carried;

/// the host of a URL as people say it, for messages, and as requests to it are counted, paced and
/// grouped: without the closing dot a fully qualified name may end with, which names the same
/// host, so `export.arxiv.org.` is paced as arXiv.
pub(crate) fn host(url: &Url) -> &str {
    url.host_str()
        .map_or("the server", |host| host.trim_end_matches('.'))
}

/// where `response` redirects a request for `from`, when it is a redirect to a link.
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
    redirect.then(|| from.join(location).ok()).flatten()
}

/// the refusal of a redirect from `hop` to `next`, a link that is not http or https, such as
/// file:// or data:, which no try would follow. the link it names is left out, as a data: link
/// may hold a whole file.
fn elsewhere(hop: &Url, next: &Url) -> Error {
    let slashes = if next.has_authority() { "//" } else { "" };
    Error::new(
        Kind::NetworkRefused,
        format!(
            "fetchloom will not follow {}: {} sent it on to a {}:{slashes} link, and fetchloom downloads over http and https alone",
            Shown(hop),
            host(hop),
            next.scheme()
        ),
    )
    .help("open the link in a browser to see where it leads, or ask its owner for a direct download link")
}

#[cfg(test)]
mod credential_tests;
#[cfg(test)]
mod scripted;

#[cfg(test)]
mod tests {
    use super::*;

    /// arXiv's terms ask for one request at a time, so its hosts share one slot whatever the
    /// settings allow, while another host takes as many as they allow.
    #[test]
    fn arxivs_hosts_share_one_slot() {
        let options = Options {
            offline: Offline::No,
            jobs: 8,
            credentials: Vec::new(),
            split: Splitting::WhenSlow,
        };
        let net = Net::new(options, Arc::default(), Arc::default(), Arc::new(|_| {}))
            .with_rules(crate::source::rules());
        let link = |text: &str| Url::parse(text).unwrap();
        let held = net.try_permit(&link("https://export.arxiv.org/api/query"));
        assert!(held.is_some());
        assert!(
            net.try_permit(&link("https://oaipmh.arxiv.org/oai"))
                .is_none()
        );
        drop(held);
        assert!(
            net.try_permit(&link("https://oaipmh.arxiv.org/oai"))
                .is_some()
        );
        assert_eq!(
            net.slots(&link("https://zenodo.org/api/records/1"))
                .available_permits(),
            HOST_JOBS
        );
    }

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
            every_port: false,
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
                &url,
                true,
            )
            .0
            .build()
            .unwrap();
        let sent: Vec<_> = request
            .headers()
            .get_all(header::AUTHORIZATION)
            .iter()
            .collect();
        assert_eq!(sent, ["new"]);
    }

    /// a bad signature is refused before the request goes, not sent without the header it spoils.
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

    /// a sign-in cookie, as earthdata hands out, reaches a split download's added streams too.
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
        assert!(block_on(net.send_body(&url, false, |request| request, None)).is_ok());
        let heads = asked.lock().unwrap();
        assert!(
            heads[1]
                .to_ascii_lowercase()
                .contains("cookie: session=abc"),
            "{}",
            heads[1]
        );
    }

    /// a server that takes the connection and never answers fails the try once the run's
    /// timeout passes, not once the time a connection may take to open has passed too.
    #[test]
    fn a_server_that_never_answers_fails_the_try_after_the_timeout() {
        use crate::source::recorded::block_on;

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = Url::parse(&format!(
            "http://{}/stallhead/s.csv",
            listener.local_addr().unwrap()
        ))
        .unwrap();
        // holds the connection open without a word until the test ends
        std::thread::spawn(move || {
            let held = listener.accept();
            std::thread::sleep(Duration::from_mins(2));
            drop(held);
        });
        let options = Options {
            offline: Offline::No,
            jobs: 1,
            credentials: Vec::new(),
            split: Splitting::Never,
        };
        let mut net = Net::new(options, Arc::default(), Arc::default(), Arc::new(|_| {}));
        net.idle = Duration::from_secs(1);
        net.connect = Duration::from_hours(1);
        net.typed(&url);
        let sent = block_on(async {
            tokio::time::timeout(Duration::from_secs(30), net.send(&url, |request| request)).await
        });
        let Ok(Err(Failure::Transient { reason, .. })) = sent else {
            panic!("the try outlasted its timeout");
        };
        assert_eq!(reason, "127.0.0.1 did not answer within 1 s");
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
            let net = if host_jobs == HOST_JOBS {
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

    /// a host that does not exist is refused, never reported unknown.
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
            let sent = block_on(net.send_on(Lane::Shared, &url, mirror, |request| request, None));
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

    /// a redirect to a link that is not http or https is refused at once, as fetchloom's own
    /// rule, never as a status to try again later.
    #[test]
    fn a_redirect_to_another_scheme_is_refused_and_says_why() {
        use crate::net::scripted::{reply, serve};
        use crate::source::recorded::block_on;

        for (to, said) in [
            ("file:///etc/passwd", "file://"),
            ("ftp://lab.example/a.csv", "ftp://"),
            ("data:text/plain,hello", "data:"),
        ] {
            let (url, asked) = serve(vec![reply("302 Found", &[("Location", to)], b"")]);
            let net = Net::new(
                Options {
                    offline: Offline::No,
                    jobs: 1,
                    credentials: Vec::new(),
                    split: Splitting::Never,
                },
                Arc::default(),
                Arc::default(),
                Arc::new(|_| {}),
            );
            net.typed(&url);
            let Err(Failure::Final(error)) = block_on(net.send(&url, |request| request)) else {
                panic!("the redirect to {to} was not refused");
            };
            assert_eq!(error.kind, Kind::NetworkRefused);
            assert_eq!(error.kind.exit_code(), 10);
            assert_eq!(
                error.message,
                format!(
                    "fetchloom will not follow {url}: 127.0.0.1 sent it on to a {said} link, and fetchloom downloads over http and https alone"
                )
            );
            assert_eq!(asked.lock().unwrap().len(), 1);
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
        let sent = block_on(net.send_on(Lane::Shared, &marker, true, |request| request, None));
        let Err(Failure::Final(error)) = sent else {
            panic!("the redirect off the mirror was followed");
        };
        assert_eq!(error.kind, Kind::NetworkRefused);
        assert_eq!(asked.lock().unwrap().len(), 1);
    }
}

#[cfg(test)]
mod waits_tests;
