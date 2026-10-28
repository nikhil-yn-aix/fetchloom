//! what can go wrong with a request, which failures deserve another try, and how long to wait.

use std::time::{Duration, SystemTime};

use reqwest::StatusCode;
use reqwest::header::{self, HeaderMap};
use url::Url;

use super::{Net, Notice, address, host};
use crate::error::{Error, Kind};
use crate::units;
use crate::work::Work;

/// how many times a request is tried before giving up.
pub(crate) const ATTEMPTS: u32 = 5;
const LONGEST_WAIT: Duration = Duration::from_secs(60);

pub(super) enum Failure {
    Final(Error),
    Transient {
        reason: String,
        retry_after: Option<Duration>,
    },
}

/// counts the tries of one request and waits between them, as politely as the server asks.
pub(super) struct Retry<'a> {
    net: &'a Net,
    url: &'a Url,
    attempt: u32,
}

impl<'a> Retry<'a> {
    pub(super) fn new(net: &'a Net, url: &'a Url) -> Retry<'a> {
        Retry {
            net,
            url,
            attempt: 1,
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
        if self.attempt >= ATTEMPTS {
            return Err(gave_up(self.url, &reason, has_copy));
        }
        let wait = backoff(self.attempt, asked).map_err(|asked| rate_limited(self.url, asked))?;
        self.attempt += 1;
        tracing::debug!(attempt = self.attempt, ?wait, %reason, "retrying");
        Work::add(&self.net.work.retries, 1);
        (self.net.notify)(Notice::Retrying {
            reason,
            wait,
            next: self.attempt,
        });
        tokio::select! {
            () = tokio::time::sleep(wait) => Ok(()),
            () = self.net.stop.asked() => Err(interrupted(self.url)),
        }
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
pub(super) fn refusal(url: &Url, status: StatusCode, headers: &HeaderMap) -> Failure {
    let code = status.as_u16();
    if is_transient(status) {
        return Failure::Transient {
            reason: format!("{} answered {code}", host(url)),
            retry_after: retry_after(headers),
        };
    }
    let error = match status {
        StatusCode::NOT_FOUND | StatusCode::GONE => Error::new(
            Kind::ReferenceUnresolved,
            format!("{} has nothing at {url} ({code})", host(url)),
        )
        .help("check the link in a browser. it may have moved or been removed"),
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Error::new(
            Kind::AuthRequired,
            format!("{} refused access to {url} ({code})", host(url)),
        )
        .help("if the link needs a login, check that it opens in a browser while signed in"),
        _ => Error::new(
            Kind::NetworkStatus,
            format!("{} answered {code} for {url}", host(url)),
        ),
    };
    Failure::Final(error)
}

/// a request that failed before any answer came, sorted into what to retry and what not to.
pub(super) fn classify(url: &Url, err: &reqwest::Error) -> Failure {
    let mut cause: Option<&(dyn std::error::Error + 'static)> = Some(err);
    while let Some(current) = cause {
        if let Some(reason) = current.downcast_ref::<address::Refused>() {
            return Failure::Final(refused(url, reason));
        }
        if current.is::<address::Offline>() {
            return Failure::Final(offline(url));
        }
        cause = current.source();
    }
    if err.is_timeout() {
        transient(format!("the request to {} timed out", host(url)))
    } else if err.is_connect() {
        transient(format!("could not connect to {}", host(url)))
    } else {
        transient(format!("the request to {} failed: {err}", host(url)))
    }
}

pub(super) fn offline(url: &Url) -> Error {
    Error::new(
        Kind::OfflineMissing,
        format!("offline, and {url} is not in the cache"),
    )
    .help("run the same command without --offline to download it")
}

pub(super) fn refused(url: &Url, reason: &address::Refused) -> Error {
    Error::new(
        Kind::NetworkRefused,
        format!("fetchloom will not follow {url}: {reason}"),
    )
    .help("only links typed on the command line may reach this machine or its local network")
}

pub(super) fn interrupted(url: &Url) -> Error {
    Error::new(
        Kind::Interrupted,
        format!("stopped while talking to {}", host(url)),
    )
}

fn gave_up(url: &Url, reason: &str, has_copy: bool) -> Error {
    let error = Error::new(
        Kind::NetworkUnreachable,
        format!("gave up on {} after {ATTEMPTS} tries: {reason}", host(url)),
    );
    if has_copy {
        error.help("check the connection, or add --offline to use the copy downloaded earlier")
    } else {
        error.help("check the connection and run the same command again")
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
    fn backoff_honors_the_server_and_refuses_long_waits() {
        for attempt in 1..=6 {
            let wait = backoff(attempt, None).unwrap();
            assert!(wait <= LONGEST_WAIT);
        }
        assert_eq!(
            backoff(1, Some(Duration::from_secs(5))).unwrap(),
            Duration::from_secs(5)
        );
        assert_eq!(
            backoff(1, Some(Duration::from_secs(600))),
            Err(Duration::from_secs(600))
        );
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
}
