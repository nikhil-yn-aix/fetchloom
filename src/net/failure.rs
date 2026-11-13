//! what can go wrong with a request, which failures deserve another try, and how long to wait.

use std::time::{Duration, SystemTime};

use reqwest::StatusCode;
use reqwest::header::{self, HeaderMap};
use url::Url;

use super::download::{Event, Listener};
use super::{Net, Notice, Offline, Shown, address, host, pace};
use crate::error::{Error, Kind};
use crate::units;
use crate::work::Work;

/// tries after the first that a request gets once it stops making progress, when
/// `FETCHLOOM_RETRIES` says nothing.
pub(crate) const RETRIES: u32 = 4;
/// the most retries `FETCHLOOM_RETRIES` may ask for. each wait is a random part of a ceiling
/// that doubles up to a minute, so this many ride out about eight minutes without network.
pub(crate) const MOST_RETRIES: u32 = 20;
/// tries one request gets in all, however often progress earned tries back, so a server that
/// sends a few bytes before every break still ends.
const MOST_TRIES: u32 = 50;
const LONGEST_WAIT: Duration = Duration::from_secs(60);
/// the most the wait before a first retry takes; each retry after it may wait twice as long.
pub(super) const FIRST_WAIT: Duration = Duration::from_secs(1);
/// how many times a request waits for a rate limit to reset before those waits spend retries.
const LIMITED: u32 = 10;

pub(super) enum Failure {
    Final(Error),
    Transient {
        reason: String,
        retry_after: Option<Duration>,
    },
    /// the host's rate limit is used up and resets within [`pace::LONGEST`], after `wait`. the
    /// next try waits for the reset before it is sent, and spends none of the request's retries.
    Limited {
        reason: String,
        wait: Duration,
    },
}

/// counts the tries of one request and waits between them, as politely as the server asks.
pub(super) struct Retry<'a> {
    net: &'a Net,
    url: &'a Url,
    /// tries since the last one that made progress.
    attempt: u32,
    /// tries in all.
    tries: u32,
    limited: u32,
    /// told of each wait for another try, as a download's bar shows it.
    on: Option<Listener<'a>>,
}

impl<'a> Retry<'a> {
    pub(super) fn new(net: &'a Net, url: &'a Url) -> Retry<'a> {
        Retry {
            net,
            url,
            attempt: 1,
            tries: 1,
            limited: 0,
            on: None,
        }
    }

    /// tells `on` of each wait for another try.
    pub(super) fn telling(mut self, on: Listener<'a>) -> Retry<'a> {
        self.on = Some(on);
        self
    }

    /// which try comes next, of how many it may get while none makes progress: the retries
    /// left, and while it is `waiting` for the host's rate limit, the waits for it left too. a
    /// host refusing for load is asked nothing more after a few refusals in a row, which caps it.
    fn numbered(&self, waiting: bool) -> (u32, u32) {
        let retries = (self.net.retries + 1).saturating_sub(self.attempt);
        let waits = if waiting {
            LIMITED.saturating_sub(self.limited)
        } else {
            0
        };
        let mut most = (self.tries + retries + waits).min(MOST_TRIES);
        if let Some(left) = self.net.paces.refusals_left(&super::paced(self.url)) {
            most = most.min(self.tries + left.saturating_sub(1));
        }
        (self.tries, most.max(self.tries))
    }

    /// tells the bar that the next try goes out after `wait`, for the rate limit of `limit`'s
    /// host when it names one.
    fn tell(&self, wait: Duration, limit: Option<String>) {
        if let Some(on) = self.on {
            let (next, of) = self.numbered(limit.is_some());
            let _ = on(Event::Retrying {
                next,
                of,
                wait,
                limit,
            });
        }
    }

    /// a try moved the download on, so the tries count again from the first, up to
    /// [`MOST_TRIES`] in all.
    pub(super) fn progressed(&mut self) {
        self.attempt = 1;
    }

    /// counts a try a rate limit refused. the next one waits for the limit to reset and does not
    /// spend a retry, unless the host keeps refusing, but it is still one of the tries.
    pub(super) async fn limited(
        &mut self,
        reason: String,
        wait: Duration,
        has_copy: bool,
    ) -> Result<(), Error> {
        if self.limited >= LIMITED {
            // never sooner than the limit resets, which a try sent earlier would wait for
            return self.wait(reason, Some(wait), has_copy).await;
        }
        self.limited += 1;
        self.tries += 1;
        tracing::debug!(%reason, ?wait, "rate limited, trying again once the limit resets");
        Work::add(&self.net.work.retries, 1);
        if self.on.is_none() {
            // the next try's request waits for the reset, and tells of it
            return Ok(());
        }
        // a download's bar tells of each wait, so a log hears of the first alone
        if self.limited == 1 {
            (self.net.notify)(Notice::Waiting {
                host: host(self.url).to_owned(),
                quota: None,
                window: None,
                wait,
            });
        }
        self.tell(wait, Some(host(self.url).to_owned()));
        tokio::select! {
            () = tokio::time::sleep(wait) => Ok(()),
            () = self.net.stop.asked() => Err(interrupted(self.url)),
        }
    }

    /// waits before the next try, or says why there is none. `has_copy` says the cache holds an
    /// earlier download, which `--offline` would use.
    pub(super) async fn wait(
        &mut self,
        reason: String,
        asked: Option<Duration>,
        has_copy: bool,
    ) -> Result<(), Error> {
        if self.attempt > self.net.retries || self.tries >= MOST_TRIES {
            let through = self
                .net
                .wire()
                .ok()
                .and_then(|wire| wire.proxies.carrier(self.url));
            let error = gave_up(self.url, self.tries, &reason, has_copy, through.as_ref());
            if self.limited == 0 {
                return Err(error);
            }
            return Err(error.detail(format!(
                "{} of them waited for {}'s rate limit to reset",
                self.limited,
                host(self.url)
            )));
        }
        // a host that refused for load pauses its next request, and the wait says how long
        let paced = super::paced(self.url);
        if self.net.paces.busy(&paced) {
            return Err(busy(self.url));
        }
        let now = tokio::time::Instant::now().into_std();
        let asked = asked.max(self.net.paces.pause(&paced, now));
        let wait = backoff(self.net.first_wait, self.attempt, asked)
            .map_err(|asked| rate_limited(self.url, asked))?;
        self.attempt += 1;
        self.tries += 1;
        tracing::debug!(attempt = self.attempt, ?wait, %reason, "retrying");
        Work::add(&self.net.work.retries, 1);
        let (next, of) = self.numbered(false);
        (self.net.notify)(Notice::Retrying {
            host: host(self.url).to_owned(),
            reason,
            wait,
            next,
            of,
        });
        self.tell(wait, None);
        tokio::select! {
            () = tokio::time::sleep(wait) => Ok(()),
            () = self.net.stop.asked() => Err(interrupted(self.url)),
        }
    }
}

impl Net {
    /// how often a request that stops making progress is tried again, as `FETCHLOOM_RETRIES`
    /// sets it.
    pub(crate) fn with_retries(mut self, retries: u32) -> Net {
        self.retries = retries;
        self
    }

    /// how many requests go to one host at once, as `FETCHLOOM_HOST_JOBS` sets it.
    pub(crate) fn with_host_jobs(mut self, host_jobs: usize) -> Net {
        self.host_jobs = host_jobs.max(1);
        self
    }

    /// how long a request waits for an answer and an answer may stay silent, as
    /// `FETCHLOOM_TIMEOUT` sets it. a connection gets at least the usual time to open.
    pub(crate) fn with_timeout(mut self, timeout: std::time::Duration) -> Net {
        self.idle = timeout;
        self.connect = super::CONNECT.max(timeout);
        self
    }
}

/// the error for a request to a host that asks its requests to start apart, and that refused so
/// many in a row for load that the run asks it nothing more.
pub(super) fn busy(url: &Url) -> Error {
    Error::new(
        Kind::NetworkRateLimited,
        format!(
            "{} refused one request after another because it is busy, so this run asks it nothing more",
            host(url)
        ),
    )
    .help("run the same command again later")
}

/// whether a status says the same request may succeed later.
pub(super) fn is_transient(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::REQUEST_TIMEOUT
            | StatusCode::TOO_EARLY
            | StatusCode::TOO_MANY_REQUESTS
            | StatusCode::INTERNAL_SERVER_ERROR
            | StatusCode::BAD_GATEWAY
            | StatusCode::SERVICE_UNAVAILABLE
            | StatusCode::GATEWAY_TIMEOUT
    )
}

pub(super) fn transient(reason: String) -> Failure {
    Failure::Transient {
        reason,
        retry_after: None,
    }
}

/// a body from `url` that stopped short of `what` it held. reqwest calls every such break "error
/// decoding response body", which reads as a fault in the bytes, so the log alone keeps its words.
pub(super) fn broke(url: &Url, what: &str, err: &reqwest::Error) -> Failure {
    tracing::debug!(error = %super::with_causes(err), "the body stopped short");
    transient(format!(
        "the connection to {} broke partway through {what}",
        host(url)
    ))
}

/// a status that ends the download, or a transient failure to retry. a 429 whose limit resets
/// soon is waited out; one that resets later asks for that wait, which fails as a rate limit.
pub(super) fn refusal(url: &Url, status: StatusCode, headers: &HeaderMap) -> Failure {
    let code = status.as_u16();
    if is_transient(status) {
        let reason = format!("{} answered {code}", host(url));
        if status == StatusCode::TOO_MANY_REQUESTS {
            let wait = pace::used_up(&pace::limits(status, headers, SystemTime::now()));
            if let Some(wait) = wait.filter(|wait| *wait <= pace::LONGEST) {
                return Failure::Limited { reason, wait };
            }
            return Failure::Transient {
                reason,
                retry_after: wait.or_else(|| retry_after(headers)),
            };
        }
        return Failure::Transient {
            reason,
            retry_after: retry_after(headers),
        };
    }
    if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN)
        && let Some(error) = crate::source::refused_download(url, code)
    {
        return Failure::Final(error);
    }
    let error = match status {
        StatusCode::NOT_FOUND | StatusCode::GONE => Error::new(
            Kind::ReferenceUnresolved,
            format!("{} has nothing at {} ({code})", host(url), Shown(url)),
        )
        .help(MOVED_HELP),
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
            let host = host(url).to_ascii_lowercase();
            // a server that asks for a username and password takes the login a person saves
            // for its host, as curl and wget would send from .netrc
            let asks_login = headers
                .get(reqwest::header::WWW_AUTHENTICATE)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.trim_start().to_ascii_lowercase().starts_with("basic"));
            let account = crate::auth::Account::from_host(&host)
                .or_else(|| asks_login.then(|| crate::auth::Account::Login(host.clone())));
            let error = Error::new(
                Kind::AuthRequired,
                format!("{host} refused access to {} ({code})", Shown(url)),
            );
            match account {
                Some(_) if url.scheme() != "https" => error.help(
                    "fetchloom sends a key only over https, so none goes with this http link: ask its owner for an https one",
                ),
                Some(account) => {
                    let command = account.command();
                    error
                        .help(format!("if it needs an account, sign in with `{command}`, then run the same command again"))
                        .needs(account, None)
                }
                None => error.help("check that the link opens in a browser. if it needs a login, ask its owner for a public link to pass to `fetchloom add` or `fetchloom get`"),
            }
        }
        StatusCode::PROXY_AUTHENTICATION_REQUIRED => proxy_login(
            url,
            headers
                .get(header::PROXY_AUTHENTICATE)
                .and_then(|value| value.to_str().ok()),
        ),
        _ => Error::new(
            Kind::NetworkStatus,
            format!("{} answered {code} for {}", host(url), Shown(url)),
        ),
    };
    Failure::Final(error)
}

/// a proxy on the way to `url` asked for a login, which no other try would give it. any `scheme`
/// but basic, or none, as a tunnel's refusal gives, most likely wants a windows sign-in.
fn proxy_login(url: &Url, scheme: Option<&str>) -> Error {
    let message = match address::proxy_of(url) {
        Some(proxy) => format!(
            "the proxy at {proxy} asked for a login before it would reach {}",
            host(url)
        ),
        None => format!(
            "a proxy asked for a login before it would reach {}",
            host(url)
        ),
    };
    let basic = scheme.is_some_and(|scheme| {
        scheme
            .trim_start()
            .get(..5)
            .is_some_and(|word| word.eq_ignore_ascii_case("basic"))
    });
    let help = if basic {
        "give it a name and password in its address, as http://name:password@proxy.example.org:8080, in HTTPS_PROXY and HTTP_PROXY"
    } else {
        "it likely wants your windows sign-in, which fetchloom cannot send: run a local relay such as px or cntlm, and point HTTPS_PROXY and HTTP_PROXY at it"
    };
    Error::new(Kind::NetworkUnreachable, message).help(help)
}

/// the proxy that carries requests for `url` cannot be reached, for `reason`. no other try would
/// get further, and the site behind it is not to blame, so the help is about the proxy setting.
pub(super) fn proxy_down(url: &Url, carrier: &address::Carrier, reason: &str) -> Error {
    let (named, check) = if let Some(variable) = carrier.variable {
        (
            format!("which {variable} names"),
            format!("check {variable}, or unset {variable} to connect without a proxy"),
        )
    } else {
        let variable = if url.scheme() == "https" {
            "HTTPS_PROXY"
        } else {
            "HTTP_PROXY"
        };
        (
            "which the system's proxy settings name".to_owned(),
            format!(
                "check the system's proxy settings, or set {variable} to the proxy this network needs"
            ),
        )
    };
    Error::new(
        Kind::NetworkUnreachable,
        format!(
            "could not reach the proxy at {}, {named}: {reason}",
            carrier.shown
        ),
    )
    .help(format!(
        "{check}. `fetchloom help environment` lists the proxy settings"
    ))
}

/// whether `err` ended a request before the proxy carrying it answered. a tunnel it refused or
/// asked a login for was answered: hyper-util keeps those errors private, so their words tell.
pub(super) fn before_proxy(err: &reqwest::Error) -> bool {
    if !err.is_connect() {
        return false;
    }
    let mut cause: Option<&(dyn std::error::Error + 'static)> = Some(err);
    while let Some(current) = cause {
        let text = current.to_string();
        if text.starts_with("tunnel error: ") {
            return text == "tunnel error: failed to create underlying connection";
        }
        cause = current.source();
    }
    true
}

/// the certificate error inside `error`, however many `io::Error`s wrap it. a direct connection
/// gets two, from tokio-rustls and hyper-rustls, and std's `source()` steps over both.
fn untrusted<'e>(mut error: &'e (dyn std::error::Error + 'static)) -> Option<&'e rustls::Error> {
    loop {
        if let Some(tls) = error.downcast_ref::<rustls::Error>() {
            return matches!(tls, rustls::Error::InvalidCertificate(_)).then_some(tls);
        }
        error = error.downcast_ref::<std::io::Error>()?.get_ref()?;
    }
}

/// a request that failed before any answer came, sorted into what to retry and what not to.
/// `carrier` is the proxy that carried it, when one did.
pub(super) fn classify(
    url: &Url,
    carrier: Option<&address::Carrier>,
    err: &reqwest::Error,
    mode: Offline,
) -> Failure {
    let mut cause: Option<&(dyn std::error::Error + 'static)> = Some(err);
    while let Some(current) = cause {
        if let Some(parse) = current
            .downcast_ref::<hyper::Error>()
            .filter(|err| err.is_parse())
        {
            return garbled(url, carrier, &parse.to_string());
        }
        if let Some(reason) = current.downcast_ref::<address::Refused>() {
            return Failure::Final(refused(url, reason));
        }
        if current.is::<address::Offline>() {
            return Failure::Final(offline(url, mode, true));
        }
        if let Some(unknown) = current.downcast_ref::<address::Unknown>() {
            return Failure::Final(
                Error::new(
                    Kind::NetworkUnreachable,
                    format!("could not find {}: {unknown}", Shown(url)),
                )
                .help("check the link for a typo, and that this machine is online"),
            );
        }
        // a proxy refusing a tunnel without a login does so on every try. hyper-util keeps that
        // error's type private, so its own words are matched, never the link's
        if current.to_string() == "tunnel error: proxy authorization required" {
            return Failure::Final(proxy_login(url, None));
        }
        // a certificate the system does not trust fails the same way on every try. matched by
        // type: the text of the outer error holds the link, which may well say "certificate"
        if let Some(tls) = untrusted(current) {
            return Failure::Final(
                Error::new(
                    Kind::NetworkUnreachable,
                    format!("{} sent a certificate this machine does not trust: {tls}", host(url)),
                )
                .help("check the link and this machine's clock. behind a company proxy that inspects https, its certificate belongs in the system's certificate store"),
            );
        }
        cause = current.source();
    }
    if err.is_timeout() {
        transient(format!("the request to {} timed out", host(url)))
    } else if err.is_connect() {
        transient(unconnected(url, err))
    } else {
        transient(format!(
            "the request to {} failed: {}",
            host(url),
            without_link(err)
        ))
    }
}

/// why no connection to `url`'s host and port was made, naming both, since the port is often
/// the clue: one that refused it has nothing listening there, or a firewall turns it away.
fn unconnected(url: &Url, err: &reqwest::Error) -> String {
    let at = match url.port_or_known_default() {
        Some(port) => format!("{}:{port}", host(url)),
        None => host(url).to_owned(),
    };
    let mut cause: Option<&(dyn std::error::Error + 'static)> = Some(err);
    while let Some(current) = cause {
        if current
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == std::io::ErrorKind::ConnectionRefused)
        {
            return format!("{at} refused the connection");
        }
        cause = current.source();
    }
    format!("could not connect to {at}")
}

/// a reply that is not valid HTTP, with what the parser `said` of it: final from a server, which
/// sends the same reply every time, but tried again through a proxy, which can recover.
fn garbled(url: &Url, carrier: Option<&address::Carrier>, said: &str) -> Failure {
    if let Some(carrier) = carrier {
        return transient(format!(
            "the reply for {} through the proxy at {} is not valid HTTP: {said}",
            host(url),
            carrier.shown
        ));
    }
    Failure::Final(
        Error::new(
            Kind::NetworkStatus,
            format!("{} sent a reply that is not valid HTTP", host(url)),
        )
        .detail(said.to_owned())
        .help("check the link, its http or https and its port too. if the link is right, the server is broken, and only whoever runs it can fix it"),
    )
}

/// the error's text, with the link reqwest puts in it shown the way messages show links.
fn without_link(err: &reqwest::Error) -> String {
    let text = err.to_string();
    match err.url() {
        Some(link) => text.replace(link.as_str(), &Shown(link).to_string()),
        None => text,
    }
}

/// what to do about a link with nothing at it, as a project or a command line gives links.
/// `get`, which keeps no project, words its own.
pub(crate) const MOVED_HELP: &str = "check the link in a browser. if it moved, give the new link on the command line or as ref in data.toml";

pub(crate) fn offline(url: &Url, mode: Offline, pinned: bool) -> Error {
    if !pinned {
        // a new link has no fingerprint yet, so no cached file can stand in for it
        return Error::new(
            Kind::OfflineMissing,
            format!(
                "offline, and a new link must be downloaded once to learn its SHA-256: {}",
                Shown(url)
            ),
        )
        .help(mode.unlearned_help());
    }
    Error::new(
        Kind::OfflineMissing,
        format!("offline, and {} is not in the cache", Shown(url)),
    )
    .help(mode.missing_help("it"))
}

pub(super) fn refused(url: &Url, reason: &address::Refused) -> Error {
    let error = Error::new(
        Kind::NetworkRefused,
        format!("fetchloom will not follow {}: {reason}", Shown(url)),
    );
    let Some(allowed) = &reason.allowed else {
        return error.help("open the link in a browser to see where it leads");
    };
    let remedy = allowed.remedy(&url.origin());
    let trusted = &reason.trusted_on_host;
    if trusted.is_empty() {
        return error.help(format!("if you trust that server, {remedy}"));
    }
    let (are, covered) = if trusted.len() == 1 {
        ("is", "that origin")
    } else {
        ("are", "those origins")
    };
    error
        .detail(format!(
            "only {} {are} trusted on {}, and trust covers {covered} alone",
            units::listed(trusted, "and"),
            host(url)
        ))
        .help(format!("to reach it too, {remedy}"))
}

pub(super) fn interrupted(url: &Url) -> Error {
    Error::new(
        Kind::Interrupted,
        format!("stopped while talking to {}", host(url)),
    )
}

/// the error once the tries for `url` ran out, `through` the proxy that carries it, if one does.
fn gave_up(
    url: &Url,
    tries: u32,
    reason: &str,
    has_copy: bool,
    through: Option<&address::Carrier>,
) -> Error {
    let message = format!(
        "gave up on {} after {}: {reason}",
        host(url),
        units::count(u64::from(tries), "try", "tries")
    );
    let later = if has_copy {
        "run the same command again later, or add --offline to use the copy downloaded earlier"
    } else {
        "run the same command again later"
    };
    // the last answer says whose side the trouble is on
    let status = reason
        .rsplit_once("answered ")
        .and_then(|(_, code)| code.parse::<u16>().ok());
    if let Some((at, _)) = reason.split_once(" refused the connection") {
        return Error::new(Kind::NetworkUnreachable, message).help(format!(
            "nothing is listening at {at}, or a firewall turned the connection away: check that the server runs and the link names its port, then {later}"
        ));
    }
    match status {
        Some(429) => {
            return Error::new(Kind::NetworkRateLimited, message)
                .help(format!("{} is limiting requests; {later}", host(url)));
        }
        Some(500..=599) => {
            return Error::new(Kind::NetworkStatus, message)
                .help(format!("{} is having trouble; {later}", host(url)));
        }
        _ => {}
    }
    let error = Error::new(Kind::NetworkUnreachable, message);
    let check = match through.map(|carrier| carrier.variable) {
        Some(Some(variable)) => format!("check the connection and the proxy {variable} names"),
        Some(None) => "check the connection and the system's proxy settings".to_owned(),
        None => "check the connection".to_owned(),
    };
    let help = if has_copy {
        format!("{check}, or add --offline to use the copy downloaded earlier")
    } else {
        format!(
            "{check} and run the same command again, with FETCHLOOM_RETRIES=10 to keep trying for about 2 minutes"
        )
    };
    match through {
        Some(_) => error.help(format!(
            "{help}. `fetchloom help environment` lists the proxy settings"
        )),
        None => error.help(help),
    }
}

fn rate_limited(url: &Url, asked: Duration) -> Error {
    Error::new(
        Kind::NetworkRateLimited,
        format!(
            "{} asked to wait {} before the next request",
            host(url),
            units::duration(asked)
        ),
    )
    .help("run the same command again later")
}

/// how long to wait before the next try: a random share of a ceiling that doubles each try from
/// `first`, never less than the server asked. a server asking for over a minute is an error.
fn backoff(first: Duration, attempt: u32, asked: Option<Duration>) -> Result<Duration, Duration> {
    if let Some(asked) = asked
        && asked > LONGEST_WAIT
    {
        return Err(asked);
    }
    let ceiling = first
        .saturating_mul(1 << attempt.saturating_sub(1).min(6))
        .min(LONGEST_WAIT);
    let jitter = ceiling.mul_f64(fastrand::f64());
    Ok(jitter.max(asked.unwrap_or_default()))
}

pub(super) fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    retry_after_with(headers, SystemTime::now)
}

fn retry_after_with(headers: &HeaderMap, now: impl FnOnce() -> SystemTime) -> Option<Duration> {
    let value = headers.get(header::RETRY_AFTER)?.to_str().ok()?.trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    let when = httpdate::parse_http_date(value).ok()?;
    Some(when.duration_since(now()).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// a refusal also says how to reach this origin.
    #[test]
    fn a_refusal_names_every_origin_trusted_on_its_host() {
        let url = Url::parse("http://lab.local:9090/a.zip").unwrap();
        let mut reason = address::Refused::new("lab.local resolves only to private addresses");
        reason.allowed = Some(address::Allowed::default());
        reason.trusted_on_host = vec![
            "http://lab.local".to_owned(),
            "http://lab.local:8080".to_owned(),
            "https://lab.local".to_owned(),
        ];
        let error = refused(&url, &reason);
        assert_eq!(
            error.details,
            [
                "only http://lab.local, http://lab.local:8080 and https://lab.local are trusted on lab.local, and trust covers those origins alone"
            ]
        );
        assert_eq!(
            error.help.as_deref(),
            Some(
                "to reach it too, put `allowed_origins = [\"http://lab.local:9090\"]` in the settings file, or set `FETCHLOOM_ALLOWED_ORIGINS=http://lab.local:9090` for one run"
            )
        );
    }

    #[test]
    fn a_permanent_http_status_says_what_to_do_without_retrying() {
        let url = Url::parse("https://example.org/data.csv").unwrap();
        for code in [400, 402, 418, 422, 451] {
            let status = StatusCode::from_u16(code).unwrap();
            let Failure::Final(error) = refusal(&url, status, &HeaderMap::new()) else {
                panic!("a permanent HTTP {code} must not be retried");
            };
            assert_eq!(error.kind, Kind::NetworkStatus);
            let help = error.help.as_deref();
            assert!(
                help.is_some_and(|text| text.contains("browser")),
                "HTTP {code} needs a next step, got {help:?}"
            );
            assert!(!help.unwrap().contains("retry"), "{help:?}");
        }
    }

    #[test]
    fn retry_after_reads_seconds_and_dates() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_hours(1);
        let mut headers = HeaderMap::new();
        assert_eq!(retry_after_with(&headers, || now), None);
        headers.insert(header::RETRY_AFTER, "7".parse().unwrap());
        assert_eq!(
            retry_after_with(&headers, || now),
            Some(Duration::from_secs(7))
        );
        headers.insert(
            header::RETRY_AFTER,
            "Thu, 01 Jan 1970 00:00:00 GMT".parse().unwrap(),
        );
        assert_eq!(
            retry_after_with(&headers, || now),
            Some(Duration::ZERO),
            "a date in the past means now"
        );
    }

    #[test]
    fn only_statuses_that_can_recover_are_retried() {
        let url = Url::parse("https://example.org/list").unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(header::RETRY_AFTER, "3".parse().unwrap());
        for code in [408, 425, 500, 502, 503, 504] {
            let status = StatusCode::from_u16(code).unwrap();
            assert!(is_transient(status), "{code}");
            let Failure::Transient {
                reason,
                retry_after,
            } = refusal(&url, status, &headers)
            else {
                panic!("HTTP {code} must retry");
            };
            assert!(reason.contains(&code.to_string()));
            assert_eq!(retry_after, Some(Duration::from_secs(3)));
        }
        for (code, kind) in [
            (401, Kind::AuthRequired),
            (403, Kind::AuthRequired),
            (404, Kind::ReferenceUnresolved),
            (410, Kind::ReferenceUnresolved),
            (501, Kind::NetworkStatus),
            (505, Kind::NetworkStatus),
        ] {
            let status = StatusCode::from_u16(code).unwrap();
            assert!(!is_transient(status), "{code}");
            let Failure::Final(error) = refusal(&url, status, &headers) else {
                panic!("HTTP {code} must not retry");
            };
            assert_eq!(error.kind, kind);
            assert!(error.message.contains(&code.to_string()));
            assert!(error.help.is_some());
        }
    }

    #[test]
    fn a_429_uses_the_reset_budget_or_falls_back_to_retry_after() {
        let url = Url::parse("https://example.org/list").unwrap();
        assert!(is_transient(StatusCode::TOO_MANY_REQUESTS));
        let mut headers = HeaderMap::new();
        assert!(matches!(
            refusal(&url, StatusCode::TOO_MANY_REQUESTS, &headers),
            Failure::Transient {
                retry_after: None,
                ..
            }
        ));
        headers.insert(header::RETRY_AFTER, "120".parse().unwrap());
        assert!(matches!(
            refusal(&url, StatusCode::TOO_MANY_REQUESTS, &headers),
            Failure::Limited { .. }
        ));
        headers.insert(header::RETRY_AFTER, "121".parse().unwrap());
        let Failure::Transient { retry_after, .. } =
            refusal(&url, StatusCode::TOO_MANY_REQUESTS, &headers)
        else {
            panic!("a reset beyond the rate limit budget must not wait");
        };
        assert_eq!(retry_after, Some(Duration::from_secs(121)));
    }

    #[test]
    fn retry_after_ignores_invalid_values_and_reads_a_future_http_date() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_hours(1);
        let mut headers = HeaderMap::new();
        for value in ["nonsense", "-1", "1.5", "18446744073709551616", ""] {
            headers.insert(header::RETRY_AFTER, value.parse().unwrap());
            assert_eq!(retry_after_with(&headers, || now), None, "{value}");
        }
        headers.insert(
            header::RETRY_AFTER,
            header::HeaderValue::from_bytes(b"\xff").unwrap(),
        );
        assert_eq!(retry_after_with(&headers, || now), None);
        headers.insert(header::RETRY_AFTER, "  60  ".parse().unwrap());
        assert_eq!(
            retry_after_with(&headers, || now),
            Some(Duration::from_secs(60))
        );
        headers.insert(
            header::RETRY_AFTER,
            "Thu, 01 Jan 1970 01:02:00 GMT".parse().unwrap(),
        );
        assert_eq!(
            retry_after_with(&headers, || now),
            Some(Duration::from_mins(2))
        );
    }

    #[test]
    fn backoff_accepts_sixty_seconds_but_never_exceeds_it() {
        for attempt in [0, 1, 7, 100, u32::MAX] {
            assert!(backoff(FIRST_WAIT, attempt, None).unwrap() <= Duration::from_secs(60));
            assert_eq!(
                backoff(FIRST_WAIT, attempt, Some(Duration::from_secs(60))),
                Ok(Duration::from_secs(60))
            );
            assert_eq!(
                backoff(FIRST_WAIT, attempt, Some(Duration::from_millis(60_001))),
                Err(Duration::from_millis(60_001))
            );
        }
        // a server's shorter wait is kept as it asked, also where tests wait nothing of their own
        for first in [FIRST_WAIT, Duration::ZERO] {
            assert_eq!(
                backoff(first, 1, Some(Duration::from_secs(5))),
                Ok(Duration::from_secs(5))
            );
        }
    }

    /// the error's help also says what a refusal means.
    #[test]
    fn a_refused_connection_names_its_host_and_port() {
        let refusing = crate::test_ports::Refusing::new().unwrap();
        let port = refusing.address.port();
        let url = Url::parse(&format!("http://127.0.0.1:{port}/x.csv")).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        // fetchloom installs the crypto provider only once it goes online, and a test may run
        // in a process of its own; no proxy, so the refusal is the port's own
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let err = runtime
            .block_on(client.get(url.clone()).send())
            .unwrap_err();
        let Failure::Transient { reason, .. } = classify(&url, None, &err, Offline::No) else {
            panic!("a refusal is tried again")
        };
        assert_eq!(reason, format!("127.0.0.1:{port} refused the connection"));
        let error = gave_up(&url, 5, &reason, false, None);
        assert_eq!(
            error.help.as_deref(),
            Some(
                format!("nothing is listening at 127.0.0.1:{port}, or a firewall turned the connection away: check that the server runs and the link names its port, then run the same command again later")
                    .as_str()
            )
        );
    }

    #[test]
    fn exhausting_retries_offers_the_copy_or_more_tries() {
        let url = Url::parse("https://example.org/list").unwrap();
        for has_copy in [false, true] {
            let error = gave_up(&url, RETRIES + 1, "connection broke", has_copy, None);
            assert_eq!(error.kind, Kind::NetworkUnreachable);
            assert!(error.message.contains("5 tries"));
            assert!(error.message.contains("connection broke"));
            let help = error.help.unwrap();
            assert_eq!(help.contains("--offline"), has_copy, "{help}");
            assert_eq!(help.contains("FETCHLOOM_RETRIES"), !has_copy, "{help}");
        }
    }

    #[test]
    fn a_rate_limited_request_that_gives_up_counts_every_try() {
        use crate::net::{Options, Splitting};
        use crate::source::recorded::block_on;
        use std::sync::Arc;

        let url = Url::parse("https://example.org/file").unwrap();
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
        )
        .with_retries(0);
        let mut retry = Retry::new(&net, &url);
        // a server that answers every request with a 429 whose limit resets at once
        let mut sent = 1;
        let error = loop {
            match block_on(retry.limited(
                "example.org answered 429".to_owned(),
                Duration::ZERO,
                false,
            )) {
                Ok(()) => sent += 1,
                Err(error) => break error,
            }
        };
        assert_eq!(sent, LIMITED + 1);
        assert!(
            error.message.contains(&format!("after {sent} tries:")),
            "{}",
            error.message
        );
        assert_eq!(
            error.details,
            ["10 of them waited for example.org's rate limit to reset"]
        );
    }

    #[test]
    fn a_certificate_error_is_found_however_deep_io_errors_wrap_it() {
        use std::io;
        let tls = || rustls::Error::InvalidCertificate(rustls::CertificateError::UnknownIssuer);
        // the shape of a direct https connection: tokio-rustls, then hyper-rustls
        let direct = io::Error::other(io::Error::new(io::ErrorKind::InvalidData, tls()));
        assert!(super::untrusted(&direct).is_some());
        let proxied = io::Error::new(io::ErrorKind::InvalidData, tls());
        assert!(super::untrusted(&proxied).is_some());
        let other = io::Error::other(io::Error::new(
            io::ErrorKind::InvalidData,
            rustls::Error::DecryptError,
        ));
        assert!(
            super::untrusted(&other).is_none(),
            "only certificates count"
        );
        let reset = io::Error::new(io::ErrorKind::ConnectionReset, "reset by peer");
        assert!(super::untrusted(&reset).is_none());
    }

    /// the request ends at once, with words for a person behind a proxy that inspects https. the
    /// server's certificate, from tests/fixtures/tls, signs itself, so no system trusts it.
    #[test]
    fn a_certificate_this_machine_does_not_trust_ends_the_request_and_says_why() {
        use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
        let _ = rustls::crypto::ring::default_provider().install_default();
        let fixture = |name: &str| {
            let tls = crate::test_paths::workspace().join("tests/fixtures/tls");
            std::fs::read(tls.join(name)).unwrap()
        };
        let config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from(fixture("self-signed.der"))],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(fixture("self-signed.key.der"))),
            )
            .unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = Url::parse(&format!(
            "https://{}/data.csv",
            listener.local_addr().unwrap()
        ))
        .unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let connection = rustls::ServerConnection::new(std::sync::Arc::new(config)).unwrap();
            let mut tls = rustls::StreamOwned::new(connection, stream);
            // the handshake runs until the client refuses the certificate
            let _ = std::io::Read::read(&mut tls, &mut [0; 1]);
        });
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let err = crate::source::recorded::block_on(client.get(url.clone()).send()).unwrap_err();
        server.join().unwrap();
        let Failure::Final(error) = classify(&url, None, &err, Offline::No) else {
            panic!("an untrusted certificate would be tried again: {err:?}");
        };
        assert_eq!(error.kind, Kind::NetworkUnreachable);
        assert!(
            error
                .message
                .starts_with("127.0.0.1 sent a certificate this machine does not trust: "),
            "{}",
            error.message
        );
        assert_eq!(
            error.help.as_deref(),
            Some(
                "check the link and this machine's clock. behind a company proxy that inspects https, its certificate belongs in the system's certificate store"
            )
        );
    }

    /// the error gives no retry advice, and a proxy from the system's settings comes with the
    /// variable that would replace them.
    #[test]
    fn a_proxy_that_cannot_be_reached_is_named_with_where_it_was_set() {
        let url = Url::parse("https://data.example.org/iris.csv").unwrap();
        let system = address::Proxies::with_lookup("http://system.lab:8080", |_, _| {
            Box::pin(async { None })
        });
        let carrier = system.carrier(&url).unwrap();
        let error = proxy_down(&url, &carrier, "it refused the connection");
        assert_eq!(error.kind, Kind::NetworkUnreachable);
        assert_eq!(
            error.message,
            "could not reach the proxy at http://system.lab:8080, which the system's proxy settings name: it refused the connection"
        );
        assert_eq!(
            error.help.as_deref(),
            Some(
                "check the system's proxy settings, or set HTTPS_PROXY to the proxy this network needs. `fetchloom help environment` lists the proxy settings"
            )
        );
    }

    /// the setting named is the one for the link's scheme, with where the proxy settings are
    /// listed. a link the proxy does not carry, as one `NO_PROXY` names, says nothing of a proxy.
    #[test]
    fn running_out_of_tries_through_a_proxy_names_its_setting() {
        let url = Url::parse("http://data.example.org/iris.csv").unwrap();
        let system = address::Proxies::with_lookup("http://system.lab:8080", |_, _| {
            Box::pin(async { None })
        });
        let carrier = system.carrier(&url);
        let help = |through| {
            gave_up(&url, RETRIES + 1, "connection broke", false, through)
                .help
                .unwrap_or_default()
        };
        let proxied = help(carrier.as_ref());
        assert!(
            proxied.starts_with("check the connection and the system's proxy settings and run the same command again")
                && proxied.ends_with("`fetchloom help environment` lists the proxy settings"),
            "{proxied}"
        );
        let direct = help(None);
        assert!(!direct.contains("proxy"), "{direct}");
    }

    /// the reply here is a status line with a control character, which a server sends the same
    /// every time. the error names the host and does not blame this machine's connection.
    #[test]
    fn a_reply_that_is_not_http_is_the_servers_and_is_asked_once() {
        use crate::net::api::Api;
        use crate::net::scripted::serve;
        use crate::net::{Options, Splitting};

        let garbled = b"HTTP/1.1 200 \x01bad\r\nContent-Length: 2\r\n\r\nok".to_vec();
        let (url, asked) = serve(vec![garbled; (RETRIES + 1) as usize]);
        let net = Net::new(
            Options {
                offline: Offline::No,
                jobs: 1,
                credentials: Vec::new(),
                split: Splitting::Never,
            },
            std::sync::Arc::default(),
            std::sync::Arc::default(),
            std::sync::Arc::new(|_| {}),
        );
        net.typed(&url);
        let error = crate::source::recorded::block_on(net.get(&url)).unwrap_err();
        assert_eq!(error.kind, Kind::NetworkStatus, "{}", error.message);
        assert_eq!(
            error.message,
            "127.0.0.1 sent a reply that is not valid HTTP"
        );
        let help = error.help.unwrap_or_default();
        assert!(!help.contains("connection"), "{help}");
        assert_eq!(asked.lock().unwrap().len(), 1);
    }

    /// the proxy may have sent the bad reply and can recover, and the retry names the proxy.
    #[test]
    fn a_reply_that_is_not_http_through_a_proxy_is_tried_again() {
        let url = Url::parse("http://data.example.org/iris.csv").unwrap();
        let system = address::Proxies::with_lookup("http://system.lab:8080", |_, _| {
            Box::pin(async { None })
        });
        let carrier = system.carrier(&url);
        let Failure::Transient { reason, .. } = garbled(&url, carrier.as_ref(), "bad status")
        else {
            panic!("a reply through a proxy must be tried again");
        };
        assert_eq!(
            reason,
            "the reply for data.example.org through the proxy at http://system.lab:8080 is not valid HTTP: bad status"
        );
    }

    #[test]
    fn running_out_of_tries_says_whose_side_the_trouble_is_on() {
        let url = Url::parse("https://example.org/list").unwrap();
        let limited = gave_up(&url, RETRIES + 1, "example.org answered 429", false, None);
        assert_eq!(limited.kind, Kind::NetworkRateLimited);
        assert!(limited.help.unwrap().contains("limiting requests"));
        let down = gave_up(&url, RETRIES + 1, "example.org answered 503", true, None);
        assert_eq!(down.kind, Kind::NetworkStatus);
        let help = down.help.unwrap();
        assert!(
            help.contains("having trouble") && help.contains("--offline"),
            "{help}"
        );
    }

    #[test]
    fn retry_waits_can_be_interrupted_without_waiting_for_the_deadline() {
        use crate::net::{Options, Splitting};
        use crate::source::recorded::block_on;
        use std::sync::Arc;

        let url = Url::parse("https://example.org/list").unwrap();
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
        net.stop.ask();
        let mut retry = Retry::new(&net, &url);
        let error = block_on(retry.wait("busy".to_owned(), Some(Duration::from_secs(60)), false))
            .unwrap_err();
        assert_eq!(error.kind, Kind::Interrupted);
        assert_eq!(net.work.counts().retries, 1);
        retry.attempt = RETRIES + 1;
        let error = block_on(retry.wait("still busy".to_owned(), None, true)).unwrap_err();
        assert_eq!(error.kind, Kind::NetworkUnreachable);
        assert!(error.help.unwrap().contains("--offline"));
        assert_eq!(net.work.counts().retries, 1);
    }

    #[test]
    fn progress_earns_tries_back_but_never_past_the_most_in_all() {
        use crate::net::{Options, Splitting};
        use crate::source::recorded::block_on;
        use std::sync::Arc;

        let url = Url::parse("https://example.org/file").unwrap();
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
        net.stop.ask();
        let mut retry = Retry::new(&net, &url);
        retry.attempt = RETRIES + 1;
        retry.progressed();
        let error = block_on(retry.wait("broke".to_owned(), None, false)).unwrap_err();
        assert_eq!(error.kind, Kind::Interrupted, "progress earned another try");
        retry.progressed();
        retry.tries = MOST_TRIES;
        let error = block_on(retry.wait("broke".to_owned(), None, false)).unwrap_err();
        assert_eq!(error.kind, Kind::NetworkUnreachable);
        assert!(
            error.message.contains(&format!("after {MOST_TRIES} tries")),
            "{}",
            error.message
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_request_gets_the_retries_the_setting_allows() {
        use crate::net::{Notice, Options, Splitting};
        use std::sync::{Arc, Mutex};

        let url = Url::parse("https://example.org/file").unwrap();
        for retries in [0, 2, MOST_RETRIES] {
            let heard = Arc::new(Mutex::new(Vec::new()));
            let kept = heard.clone();
            let net = Net::new(
                Options {
                    offline: Offline::No,
                    jobs: 1,
                    credentials: Vec::new(),
                    split: Splitting::Never,
                },
                Arc::default(),
                Arc::default(),
                Arc::new(move |notice| {
                    if let Notice::Retrying { host, next, of, .. } = notice {
                        kept.lock().unwrap().push((host, next, of));
                    }
                }),
            )
            .with_retries(retries);
            let mut retry = Retry::new(&net, &url);
            let error = loop {
                if let Err(error) = retry.wait("broke".to_owned(), None, false).await {
                    break error;
                }
            };
            let tries = retries + 1;
            let said = if tries == 1 {
                "after 1 try:".to_owned()
            } else {
                format!("after {tries} tries:")
            };
            assert!(error.message.contains(&said), "{}", error.message);
            let heard = heard.lock().unwrap();
            let expected: Vec<_> = (2..=tries)
                .map(|next| ("example.org".to_owned(), next, tries))
                .collect();
            assert_eq!(*heard, expected);
        }
    }

    /// arXiv answered 503 without a `Retry-After`, so the retry waits the pause the refusal set
    /// rather than a second, and says so; once it refuses four in a row, no request goes.
    #[tokio::test(start_paused = true)]
    async fn a_paced_host_refusing_for_load_is_waited_for_then_asked_nothing_more() {
        use crate::net::{Notice, Options, Splitting};
        use std::sync::{Arc, Mutex};

        let url = Url::parse("https://export.arxiv.org/api/query").unwrap();
        let heard = Arc::new(Mutex::new(Vec::new()));
        let kept = heard.clone();
        let net = Net::new(
            Options {
                offline: Offline::No,
                jobs: 1,
                credentials: Vec::new(),
                split: Splitting::Never,
            },
            Arc::default(),
            Arc::default(),
            Arc::new(move |notice| {
                if let Notice::Retrying { wait, .. } = notice {
                    kept.lock().unwrap().push(wait);
                }
            }),
        );
        let name = super::super::paced(&url);
        let now = || tokio::time::Instant::now().into_std();
        let answer = |status, sent, share| pace::Answered {
            status,
            sent,
            at: now(),
            share,
        };
        let sent = net.pace(&url).await.unwrap().sent;
        net.paces
            .answered(&name, &answer(StatusCode::SERVICE_UNAVAILABLE, sent, 1.0));
        let mut retry = Retry::new(&net, &url);
        retry
            .wait("export.arxiv.org answered 503".to_owned(), None, false)
            .await
            .unwrap();
        assert_eq!(*heard.lock().unwrap(), [Duration::from_secs(6)]);
        // each refused request is sent once the pause the last one set ends
        for _ in 0..3 {
            let sent = net.pace(&url).await.unwrap().sent;
            net.paces
                .answered(&name, &answer(StatusCode::TOO_MANY_REQUESTS, sent, 0.0));
        }
        let stopped = retry
            .wait("export.arxiv.org answered 429".to_owned(), None, false)
            .await
            .unwrap_err();
        assert_eq!(heard.lock().unwrap().len(), 1, "no wait before stopping");
        let refused = net.pace(&url).await.unwrap_err();
        for error in [stopped, refused] {
            assert_eq!(error.kind, Kind::NetworkRateLimited);
            assert_eq!(
                error.message,
                "export.arxiv.org refused one request after another because it is busy, so this run asks it nothing more"
            );
        }
    }

    /// a paced host refusing for load is asked nothing more after four refusals in a row, so each
    /// retry is numbered of four, whatever the retries would allow.
    #[tokio::test(start_paused = true)]
    async fn a_paced_host_refusing_for_load_numbers_its_tries_of_four() {
        use crate::net::{Notice, Options, Splitting};
        use std::sync::{Arc, Mutex};

        let url = Url::parse("https://export.arxiv.org/api/query").unwrap();
        let heard = Arc::new(Mutex::new(Vec::new()));
        let kept = heard.clone();
        let net = Net::new(
            Options {
                offline: Offline::No,
                jobs: 1,
                credentials: Vec::new(),
                split: Splitting::Never,
            },
            Arc::default(),
            Arc::default(),
            Arc::new(move |notice| {
                if let Notice::Retrying { next, of, .. } = notice {
                    kept.lock().unwrap().push((next, of));
                }
            }),
        )
        .with_retries(10);
        let name = super::super::paced(&url);
        let mut retry = Retry::new(&net, &url);
        let refused = loop {
            let sent = net.pace(&url).await.unwrap().sent;
            let answer = pace::Answered {
                status: StatusCode::TOO_MANY_REQUESTS,
                sent,
                at: tokio::time::Instant::now().into_std(),
                share: 0.0,
            };
            net.paces.answered(&name, &answer);
            if let Err(error) = retry.wait("429".to_owned(), None, false).await {
                break error;
            }
        };
        assert_eq!(refused.kind, Kind::NetworkRateLimited);
        assert_eq!(*heard.lock().unwrap(), [(2, 4), (3, 4), (4, 4)]);
    }

    /// after one wait for the host's rate limit, the count still comes from the retries, since the
    /// host may never set that limit again.
    #[tokio::test(start_paused = true)]
    async fn a_broken_try_is_numbered_by_the_retries_left() {
        use crate::net::{Notice, Options, Splitting};
        use std::sync::{Arc, Mutex};

        let url = Url::parse("https://example.org/file").unwrap();
        let heard = Arc::new(Mutex::new(Vec::new()));
        let kept = heard.clone();
        let net = Net::new(
            Options {
                offline: Offline::No,
                jobs: 1,
                credentials: Vec::new(),
                split: Splitting::Never,
            },
            Arc::default(),
            Arc::default(),
            Arc::new(move |notice| {
                if let Notice::Retrying { next, of, .. } = notice {
                    kept.lock().unwrap().push((next, of));
                }
            }),
        );
        let mut retry = Retry::new(&net, &url);
        retry
            .limited("limited".to_owned(), Duration::from_secs(1), false)
            .await
            .unwrap();
        retry.wait("broke".to_owned(), None, false).await.unwrap();
        assert_eq!(*heard.lock().unwrap(), [(3, RETRIES + 2)]);
    }
}
