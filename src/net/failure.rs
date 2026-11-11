//! what can go wrong with a request, which failures deserve another try, and how long to wait.

use std::time::{Duration, SystemTime};

use reqwest::StatusCode;
use reqwest::header::{self, HeaderMap};
use url::Url;

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
/// how many times a request waits for a rate limit to reset before those waits spend retries.
const LIMITED: u32 = 10;

pub(super) enum Failure {
    Final(Error),
    Transient {
        reason: String,
        retry_after: Option<Duration>,
    },
    /// the host's rate limit is used up and resets within [`pace::LONGEST`]. the next try waits
    /// for the reset before it is sent, and spends none of the request's retries.
    Limited {
        reason: String,
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
}

impl<'a> Retry<'a> {
    pub(super) fn new(net: &'a Net, url: &'a Url) -> Retry<'a> {
        Retry {
            net,
            url,
            attempt: 1,
            tries: 1,
            limited: 0,
        }
    }

    /// a try moved the download on, so the tries count again from the first, up to
    /// [`MOST_TRIES`] in all.
    pub(super) fn progressed(&mut self) {
        self.attempt = 1;
    }

    /// counts a try a rate limit refused. the next one waits for the limit to reset and does not
    /// spend a retry, unless the host keeps refusing, but it is still one of the tries.
    pub(super) async fn limited(&mut self, reason: String, has_copy: bool) -> Result<(), Error> {
        if self.limited >= LIMITED {
            return self.wait(reason, None, has_copy).await;
        }
        self.limited += 1;
        self.tries += 1;
        tracing::debug!(%reason, "rate limited, trying again once the limit resets");
        Work::add(&self.net.work.retries, 1);
        Ok(())
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
            let error = gave_up(self.url, self.tries, &reason, has_copy);
            if self.limited == 0 {
                return Err(error);
            }
            return Err(error.detail(format!(
                "{} of them waited for {}'s rate limit to reset",
                self.limited,
                host(self.url)
            )));
        }
        let wait = backoff(self.attempt, asked).map_err(|asked| rate_limited(self.url, asked))?;
        self.attempt += 1;
        self.tries += 1;
        tracing::debug!(attempt = self.attempt, ?wait, %reason, "retrying");
        Work::add(&self.net.work.retries, 1);
        (self.net.notify)(Notice::Retrying {
            host: host(self.url).to_owned(),
            reason,
            wait,
            next: self.attempt,
            of: self.net.retries + 1,
        });
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

/// a status that ends the download: a missing file, a refusal, or a transient failure to retry.
/// a 429 whose limit resets soon enough is waited out; one that resets later asks for that wait,
/// which fails as a rate limit.
pub(super) fn refusal(url: &Url, status: StatusCode, headers: &HeaderMap) -> Failure {
    let code = status.as_u16();
    if is_transient(status) {
        let reason = format!("{} answered {code}", host(url));
        if status == StatusCode::TOO_MANY_REQUESTS {
            let wait = pace::used_up(&pace::limits(status, headers, SystemTime::now()));
            if wait.is_some_and(|wait| wait <= pace::LONGEST) {
                return Failure::Limited { reason };
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
    if status == StatusCode::FORBIDDEN
        && let Some(error) = crate::source::refused_download(url)
    {
        return Failure::Final(error);
    }
    let error = match status {
        StatusCode::NOT_FOUND | StatusCode::GONE => Error::new(
            Kind::ReferenceUnresolved,
            format!("{} has nothing at {} ({code})", host(url), Shown(url)),
        )
        .help("check the link in a browser. if it moved, give the new link on the command line or as ref in data.toml"),
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

/// a proxy on the way to `url` asked for a login, which no other try would give it. a proxy
/// whose `scheme` asks for a name and password takes them in its address; any other, as one
/// asking for NTLM or Negotiate, or a tunnel's refusal, which names none, most likely wants a
/// windows sign-in.
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
pub(super) fn classify(url: &Url, err: &reqwest::Error, mode: Offline) -> Failure {
    let mut cause: Option<&(dyn std::error::Error + 'static)> = Some(err);
    while let Some(current) = cause {
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
        transient(format!("could not connect to {}", host(url)))
    } else {
        transient(format!(
            "the request to {} failed: {}",
            host(url),
            without_link(err)
        ))
    }
}

/// the error's text, with the link reqwest puts in it shown the way messages show links.
fn without_link(err: &reqwest::Error) -> String {
    let text = err.to_string();
    match err.url() {
        Some(link) => text.replace(link.as_str(), &Shown(link).to_string()),
        None => text,
    }
}

pub(super) fn offline(url: &Url, mode: Offline, pinned: bool) -> Error {
    if !pinned {
        // a new link has no fingerprint yet, so no cached file can stand in for it
        return Error::new(
            Kind::OfflineMissing,
            format!(
                "offline, and a new link must be downloaded once to learn its SHA-256: {}",
                Shown(url)
            ),
        )
        .help(format!(
            "{} to pin it. `fetchloom get <link> --offline` reuses a file fetched before",
            mode.retry()
        ));
    }
    Error::new(
        Kind::OfflineMissing,
        format!("offline, and {} is not in the cache", Shown(url)),
    )
    .help(mode.missing_help("it"))
}

pub(super) fn refused(url: &Url, reason: &address::Refused) -> Error {
    Error::new(
        Kind::NetworkRefused,
        format!("fetchloom will not follow {}: {reason}", Shown(url)),
    )
    .help(format!(
        "if you trust that server, give its link on the command line, or add {} to allowed_origins in the settings file or FETCHLOOM_ALLOWED_ORIGINS",
        url.origin().ascii_serialization()
    ))
}

pub(super) fn interrupted(url: &Url) -> Error {
    Error::new(
        Kind::Interrupted,
        format!("stopped while talking to {}", host(url)),
    )
}

fn gave_up(url: &Url, tries: u32, reason: &str, has_copy: bool) -> Error {
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
    let proxy = [
        "HTTPS_PROXY",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
        "HTTP_PROXY",
        "http_proxy",
    ]
    .into_iter()
    .find(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()));
    let error = Error::new(Kind::NetworkUnreachable, message);
    let check = match proxy {
        Some(name) => format!("check the connection and the proxy {name} names"),
        None => "check the connection".to_owned(),
    };
    if has_copy {
        error.help(format!(
            "{check}, or add --offline to use the copy downloaded earlier"
        ))
    } else {
        error.help(format!(
            "{check} and run the same command again, with FETCHLOOM_RETRIES=10 to keep trying for about 2 minutes"
        ))
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

/// how long to wait before the next try: a random share of a doubling ceiling, never less than
/// the server asked for. a server asking for more than a minute is an error, not a wait.
fn backoff(attempt: u32, asked: Option<Duration>) -> Result<Duration, Duration> {
    if let Some(asked) = asked
        && asked > LONGEST_WAIT
    {
        return Err(asked);
    }
    let ceiling = Duration::from_secs(1 << attempt.saturating_sub(1).min(6)).min(LONGEST_WAIT);
    let jitter = ceiling.mul_f64(fastrand::f64());
    Ok(jitter.max(asked.unwrap_or_default()))
}

pub(super) fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    let value = headers.get(header::RETRY_AFTER)?.to_str().ok()?.trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    let when = httpdate::parse_http_date(value).ok()?;
    Some(when.duration_since(SystemTime::now()).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let mut headers = HeaderMap::new();
        assert_eq!(retry_after(&headers), None);
        headers.insert(header::RETRY_AFTER, "7".parse().unwrap());
        assert_eq!(retry_after(&headers), Some(Duration::from_secs(7)));
        headers.insert(
            header::RETRY_AFTER,
            "Tue, 01 Oct 2024 10:00:00 GMT".parse().unwrap(),
        );
        assert_eq!(
            retry_after(&headers),
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
        let mut headers = HeaderMap::new();
        for value in ["nonsense", "-1", "1.5", "18446744073709551616", ""] {
            headers.insert(header::RETRY_AFTER, value.parse().unwrap());
            assert_eq!(retry_after(&headers), None, "{value}");
        }
        headers.insert(
            header::RETRY_AFTER,
            header::HeaderValue::from_bytes(b"\xff").unwrap(),
        );
        assert_eq!(retry_after(&headers), None);
        headers.insert(header::RETRY_AFTER, "  60  ".parse().unwrap());
        assert_eq!(retry_after(&headers), Some(Duration::from_secs(60)));
        let before = SystemTime::now();
        let seconds = before
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 120;
        let deadline = SystemTime::UNIX_EPOCH + Duration::from_secs(seconds);
        headers.insert(
            header::RETRY_AFTER,
            httpdate::fmt_http_date(deadline).parse().unwrap(),
        );
        let wait = retry_after(&headers).unwrap();
        let after = SystemTime::now();
        assert!(wait <= deadline.duration_since(before).unwrap());
        assert!(wait >= deadline.duration_since(after).unwrap_or_default());
    }

    #[test]
    fn backoff_accepts_sixty_seconds_but_never_exceeds_it() {
        for attempt in [0, 1, 7, 100, u32::MAX] {
            assert!(backoff(attempt, None).unwrap() <= Duration::from_secs(60));
            assert_eq!(
                backoff(attempt, Some(Duration::from_secs(60))),
                Ok(Duration::from_secs(60))
            );
            assert_eq!(
                backoff(attempt, Some(Duration::from_millis(60_001))),
                Err(Duration::from_millis(60_001))
            );
        }
        // a server's shorter wait is kept as it asked
        assert_eq!(
            backoff(1, Some(Duration::from_secs(5))),
            Ok(Duration::from_secs(5))
        );
    }

    #[test]
    fn exhausting_retries_offers_the_copy_or_more_tries() {
        let url = Url::parse("https://example.org/list").unwrap();
        for has_copy in [false, true] {
            let error = gave_up(&url, RETRIES + 1, "connection broke", has_copy);
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
            match block_on(retry.limited("example.org answered 429".to_owned(), false)) {
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

    #[test]
    fn running_out_of_tries_says_whose_side_the_trouble_is_on() {
        let url = Url::parse("https://example.org/list").unwrap();
        let limited = gave_up(&url, RETRIES + 1, "example.org answered 429", false);
        assert_eq!(limited.kind, Kind::NetworkRateLimited);
        assert!(limited.help.unwrap().contains("limiting requests"));
        let down = gave_up(&url, RETRIES + 1, "example.org answered 503", true);
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
}
