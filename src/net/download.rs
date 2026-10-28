//! one file from a URL into the cache. resume what can be resumed, retry what may still succeed,
//! and hash every byte on its way to disk.

use std::io::{self, Write};
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use reqwest::StatusCode;
use reqwest::header::{self, HeaderMap};
use serde::Serialize;
use url::Url;

use super::{CONNECT, IDLE, Net, RESPONSE, address, disposition, host};
use crate::cache::{Cache, Claim, Partial, PartialWriter, Stored, UrlRecord, Validators};
use crate::digest::Sha256;
use crate::error::{Error, Kind};
use crate::units;
use crate::work::Work;

pub(crate) const ATTEMPTS: u32 = 5;
const LONGEST_WAIT: Duration = Duration::from_secs(60);
/// how often to look again while another process downloads the same URL.
const POLL: Duration = Duration::from_millis(250);

/// what a download reports while it runs, for progress bars and questions.
pub(crate) enum Event {
    /// another fetchloom on this machine is downloading the same URL, and this run waits for it.
    Waiting,
    /// the server answered and bytes are about to flow. `total` counts the whole file.
    Started {
        total: Option<u64>,
        resumed: u64,
        name: Option<String>,
    },
    Bytes(u64),
    Retrying {
        attempt: u32,
        wait: Duration,
        reason: String,
    },
}

/// where the bytes came from this time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Origin {
    /// downloaded now, perhaps continuing an earlier download.
    Network,
    /// the server said the copy in the cache is still current.
    NotModified,
    /// taken from the cache without asking: offline, or just downloaded by another fetchloom.
    Cache,
}

/// a URL's bytes, ready in the cache.
pub(crate) struct Fetched {
    pub(crate) digest: Sha256,
    pub(crate) size: u64,
    pub(crate) path: PathBuf,
    /// the file name the server gave, when it gave one.
    pub(crate) name: Option<String>,
    pub(crate) origin: Origin,
}

enum Answer {
    NotModified,
    Stored {
        stored: Stored,
        validators: Validators,
        name: Option<String>,
    },
}

enum Failure {
    Final(Error),
    Transient {
        reason: String,
        retry_after: Option<Duration>,
    },
    /// the server will not continue the earlier download, so it starts over from the first byte.
    StartOver,
}

/// where a response body starts in the file, how long the whole file is, and how the server
/// identified it.
struct Body {
    offset: u64,
    total: Option<u64>,
    validators: Validators,
}

pub(crate) type Listener<'a> = &'a (dyn Fn(Event) -> Result<(), Error> + Sync);

impl Net {
    /// downloads `url` into the cache, or confirms that the copy the cache holds is current.
    /// `on` sees every event and may stop the download by returning an error.
    pub(crate) async fn download(
        &self,
        url: &Url,
        cache: &Cache,
        on: Listener<'_>,
    ) -> Result<Fetched, Error> {
        let known = cached(cache, url);
        if self.offline {
            return known
                .map(|(record, path)| fetched(record, path, Origin::Cache))
                .ok_or_else(|| offline(url));
        }
        self.typed
            .check(url)
            .map_err(|reason| refused(url, &reason))?;
        let (partial, waited) = self.claim(url, cache, on).await?;
        if waited && let Some((record, path)) = cached(cache, url) {
            return Ok(fetched(record, path, Origin::Cache));
        }
        let mut attempt = 0;
        loop {
            attempt += 1;
            let validators = known.as_ref().map(|(record, _)| &record.validators);
            match self.attempt(url, &partial, validators, on).await {
                Ok(Answer::NotModified) => {
                    return known
                        .map(|(record, path)| fetched(record, path, Origin::NotModified))
                        .ok_or_else(|| {
                            Error::new(
                                Kind::NetworkStatus,
                                format!(
                                    "{} answered 304 to a request that did not ask for it",
                                    host(url)
                                ),
                            )
                        });
                }
                Ok(Answer::Stored {
                    stored,
                    validators,
                    name,
                }) => {
                    return Ok(self.remember(url, cache, stored, validators, name));
                }
                Err(Failure::Final(err)) => return Err(progress_kept(err, &partial)),
                Err(Failure::StartOver) => {
                    tracing::debug!(
                        "the server will not continue the earlier download, starting over"
                    );
                    partial.discard();
                }
                Err(Failure::Transient {
                    reason,
                    retry_after,
                }) => {
                    if attempt >= ATTEMPTS {
                        return Err(progress_kept(
                            gave_up(url, &reason, known.is_some()),
                            &partial,
                        ));
                    }
                    let wait =
                        backoff(attempt, retry_after).map_err(|asked| rate_limited(url, asked))?;
                    tracing::debug!(attempt, ?wait, %reason, "retrying");
                    Work::add(&self.work.retries, 1);
                    on(Event::Retrying {
                        attempt,
                        wait,
                        reason,
                    })?;
                    tokio::select! {
                        () = tokio::time::sleep(wait) => {}
                        () = self.stop.asked() => return Err(progress_kept(interrupted(url), &partial)),
                    }
                }
            }
        }
    }

    /// claims the download of `url`, waiting while another fetchloom downloads the same URL.
    /// says whether it had to wait.
    async fn claim(
        &self,
        url: &Url,
        cache: &Cache,
        on: Listener<'_>,
    ) -> Result<(Partial, bool), Error> {
        let mut waited = false;
        loop {
            match cache.claim(url.as_str()) {
                Ok(Claim::Owned(partial)) => return Ok((partial, waited)),
                Ok(Claim::Busy) if !waited => {
                    on(Event::Waiting)?;
                    waited = true;
                }
                Ok(Claim::Busy) => {}
                Err(err) => return Err(local(&err)),
            }
            tokio::select! {
                () = tokio::time::sleep(POLL) => {}
                () = self.stop.asked() => return Err(interrupted(url)),
            }
        }
    }

    async fn attempt(
        &self,
        url: &Url,
        partial: &Partial,
        known: Option<&Validators>,
        on: Listener<'_>,
    ) -> Result<Answer, Failure> {
        let earlier = partial
            .resumable()
            .filter(|(_, validators)| if_range(validators).is_some());
        let mut request = self.client.get(url.clone());
        if let Some((received, validators)) = &earlier {
            request = request.header(header::RANGE, format!("bytes={received}-"));
            if let Some(check) = if_range(validators) {
                request = request.header(header::IF_RANGE, check);
            }
        } else if let Some(known) = known {
            if let Some(etag) = &known.etag {
                request = request.header(header::IF_NONE_MATCH, etag);
            }
            if let Some(date) = &known.last_modified {
                request = request.header(header::IF_MODIFIED_SINCE, date);
            }
        }
        Work::add(&self.work.requests, 1);
        let response = tokio::select! {
            sent = tokio::time::timeout(CONNECT + RESPONSE, request.send()) => match sent {
                Err(_) => return Err(transient(format!("{} did not answer within {} s", host(url), (CONNECT + RESPONSE).as_secs()))),
                Ok(Err(err)) => return Err(classify(url, &err)),
                Ok(Ok(response)) => response,
            },
            () = self.stop.asked() => return Err(Failure::Final(interrupted(url))),
        };
        let status = response.status();
        tracing::debug!(
            status = status.as_u16(),
            from = earlier.as_ref().map(|(received, _)| *received),
            "response"
        );
        let headers = response.headers().clone();
        let offset = match status {
            StatusCode::NOT_MODIFIED if earlier.is_none() && known.is_some() => {
                return Ok(Answer::NotModified);
            }
            StatusCode::OK => 0,
            StatusCode::PARTIAL_CONTENT => match (&earlier, range_start(&headers)) {
                (Some((received, _)), Some(start)) if start == *received => *received,
                _ => return Err(Failure::StartOver),
            },
            StatusCode::RANGE_NOT_SATISFIABLE if earlier.is_some() => {
                return Err(Failure::StartOver);
            }
            status => return Err(refusal(url, status, &headers)),
        };
        let total = total_length(&headers, status);
        let name = disposition::file_name(&headers);
        on(Event::Started {
            total,
            resumed: offset,
            name: name.clone(),
        })
        .map_err(Failure::Final)?;
        if offset > 0 {
            Work::add(&self.work.resumes, 1);
        }
        let validators = validators_of(&headers);
        let body = Body {
            offset,
            total,
            validators: validators.clone(),
        };
        let writer = self.stream(url, partial, response, body, on).await?;
        let stored = tokio::task::spawn_blocking(move || writer.finish())
            .await
            .map_err(|err| Failure::Final(local(&io::Error::other(err))))?
            .map_err(|err| Failure::Final(local(&err)))?;
        Ok(Answer::Stored {
            stored,
            validators,
            name,
        })
    }

    /// moves the body to a writer thread, so the network never waits on the disk.
    async fn stream(
        &self,
        url: &Url,
        partial: &Partial,
        mut response: reqwest::Response,
        body: Body,
        on: Listener<'_>,
    ) -> Result<PartialWriter, Failure> {
        let Body {
            offset,
            total,
            validators,
        } = body;
        let (sender, mut receiver) = tokio::sync::mpsc::channel::<Bytes>(16);
        let partial = partial.clone();
        let writing = tokio::task::spawn_blocking(move || -> io::Result<PartialWriter> {
            let mut writer = partial.writer(offset, total, validators)?;
            while let Some(chunk) = receiver.blocking_recv() {
                writer.write_all(&chunk)?;
            }
            writer.save()?;
            Ok(writer)
        });
        let body = loop {
            let next = tokio::select! {
                next = tokio::time::timeout(IDLE, response.chunk()) => next,
                () = self.stop.asked() => break Err(Failure::Final(interrupted(url))),
            };
            match next {
                Err(_) => {
                    break Err(transient(format!(
                        "{} sent nothing for {} s",
                        host(url),
                        IDLE.as_secs()
                    )));
                }
                Ok(Err(err)) => {
                    break Err(transient(format!(
                        "the connection to {} broke: {err}",
                        host(url)
                    )));
                }
                Ok(Ok(None)) => break Ok(()),
                Ok(Ok(Some(chunk))) => {
                    let length = chunk.len() as u64;
                    Work::add(&self.work.bytes_down, length);
                    if let Err(err) = on(Event::Bytes(length)) {
                        break Err(Failure::Final(err));
                    }
                    // a closed channel means the writer failed, and its own error says why
                    if sender.send(chunk).await.is_err() {
                        break Ok(());
                    }
                }
            }
        };
        drop(sender);
        let writer = writing
            .await
            .map_err(|err| Failure::Final(local(&io::Error::other(err))))?
            .map_err(|err| Failure::Final(local(&err)))?;
        body?;
        if let Some(total) = total
            && writer.written() != total
        {
            return Err(transient(format!(
                "{} closed the connection after {} of {}",
                host(url),
                units::size(writer.written()),
                units::size(total)
            )));
        }
        Ok(writer)
    }

    /// counts a finished download and records what the URL served, so a later run can ask the
    /// server whether it changed instead of downloading it again.
    fn remember(
        &self,
        url: &Url,
        cache: &Cache,
        stored: Stored,
        validators: Validators,
        name: Option<String>,
    ) -> Fetched {
        if stored.new {
            Work::add(&self.work.objects_written, 1);
        }
        let record = UrlRecord {
            url: url.as_str().to_owned(),
            sha256: stored.digest,
            size: stored.size,
            name: name.clone(),
            validators,
        };
        // the record only spares a later download, so failing to write it costs nothing more
        if let Err(err) = cache.save_url_record(&record) {
            tracing::debug!(%err, "could not record what the URL served");
        }
        Fetched {
            digest: stored.digest,
            size: stored.size,
            path: stored.path,
            name,
            origin: Origin::Network,
        }
    }
}

/// what the cache holds for `url` from an earlier download, when the bytes are still there.
fn cached(cache: &Cache, url: &Url) -> Option<(UrlRecord, PathBuf)> {
    let record = cache.url_record(url.as_str())?;
    let path = cache.find(&record.sha256, record.size)?;
    Some((record, path))
}

fn fetched(record: UrlRecord, path: PathBuf, origin: Origin) -> Fetched {
    Fetched {
        digest: record.sha256,
        size: record.size,
        path,
        name: record.name,
        origin,
    }
}

fn refusal(url: &Url, status: StatusCode, headers: &HeaderMap) -> Failure {
    let code = status.as_u16();
    match status {
        StatusCode::NOT_FOUND | StatusCode::GONE => Failure::Final(
            Error::new(
                Kind::ReferenceUnresolved,
                format!("{} has nothing at {url} ({code})", host(url)),
            )
            .help("check the link in a browser. it may have moved or been removed"),
        ),
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Failure::Final(
            Error::new(
                Kind::AuthRequired,
                format!("{} refused access to {url} ({code})", host(url)),
            )
            .help("if the link needs a login, check that it opens in a browser while signed in"),
        ),
        StatusCode::REQUEST_TIMEOUT
        | StatusCode::TOO_EARLY
        | StatusCode::TOO_MANY_REQUESTS
        | StatusCode::INTERNAL_SERVER_ERROR
        | StatusCode::BAD_GATEWAY
        | StatusCode::SERVICE_UNAVAILABLE
        | StatusCode::GATEWAY_TIMEOUT => Failure::Transient {
            reason: format!("{} answered {code}", host(url)),
            retry_after: retry_after(headers),
        },
        _ => Failure::Final(Error::new(
            Kind::NetworkStatus,
            format!("{} answered {code} for {url}", host(url)),
        )),
    }
}

fn classify(url: &Url, err: &reqwest::Error) -> Failure {
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

fn transient(reason: String) -> Failure {
    Failure::Transient {
        reason,
        retry_after: None,
    }
}

fn offline(url: &Url) -> Error {
    Error::new(
        Kind::OfflineMissing,
        format!("offline, and {url} is not in the cache"),
    )
    .help("run the same command without --offline to download it")
}

fn local(err: &io::Error) -> Error {
    Error::new(
        Kind::LocalIo,
        format!("could not write to the cache: {err}"),
    )
}

fn refused(url: &Url, reason: &address::Refused) -> Error {
    Error::new(
        Kind::NetworkRefused,
        format!("fetchloom will not follow {url}: {reason}"),
    )
    .help("only links typed on the command line may reach this machine or its local network")
}

fn interrupted(url: &Url) -> Error {
    Error::new(
        Kind::Interrupted,
        format!("stopped while downloading from {}", host(url)),
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

/// tells a person who stopped, or whose network failed, that the bytes so far are not lost.
fn progress_kept(error: Error, partial: &Partial) -> Error {
    if !matches!(error.kind, Kind::Interrupted | Kind::NetworkUnreachable) {
        return error;
    }
    match partial.resumable() {
        Some((received, _)) => error.detail(format!(
            "the {} downloaded so far is kept, and the next run continues from there",
            units::size(received)
        )),
        None => error,
    }
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

fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    let value = headers.get(header::RETRY_AFTER)?.to_str().ok()?.trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    let when = httpdate::parse_http_date(value).ok()?;
    Some(when.duration_since(SystemTime::now()).unwrap_or_default())
}

/// the validator `If-Range` can trust: a strong `ETag`, else a modification date.
fn if_range(validators: &Validators) -> Option<&str> {
    validators
        .etag
        .as_deref()
        .filter(|tag| !tag.starts_with("W/"))
        .or(validators.last_modified.as_deref())
}

fn validators_of(headers: &HeaderMap) -> Validators {
    let text = |name| {
        headers
            .get(name)
            .and_then(|value: &header::HeaderValue| value.to_str().ok())
            .map(str::to_owned)
    };
    Validators {
        etag: text(header::ETAG),
        last_modified: text(header::LAST_MODIFIED),
    }
}

/// the first byte of a `Content-Range: bytes 100-199/200` answer.
fn range_start(headers: &HeaderMap) -> Option<u64> {
    let value = headers.get(header::CONTENT_RANGE)?.to_str().ok()?;
    let (range, _) = value.strip_prefix("bytes ")?.split_once('/')?;
    range.split_once('-')?.0.trim().parse().ok()
}

/// the size of the whole file, from a range answer or from the length of a full one.
fn total_length(headers: &HeaderMap, status: StatusCode) -> Option<u64> {
    if status == StatusCode::PARTIAL_CONTENT {
        let value = headers.get(header::CONTENT_RANGE)?.to_str().ok()?;
        value.rsplit_once('/')?.1.trim().parse().ok()
    } else {
        headers
            .get(header::CONTENT_LENGTH)?
            .to_str()
            .ok()?
            .trim()
            .parse()
            .ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(header::HeaderName, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(name.clone(), value.parse().unwrap());
        }
        map
    }

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
    fn retry_after_reads_seconds() {
        assert_eq!(
            retry_after(&headers(&[(header::RETRY_AFTER, "7")])),
            Some(Duration::from_secs(7))
        );
        assert_eq!(retry_after(&headers(&[])), None);
    }

    #[test]
    fn ranges_and_lengths() {
        let partial = headers(&[(header::CONTENT_RANGE, "bytes 100-199/200")]);
        assert_eq!(range_start(&partial), Some(100));
        assert_eq!(
            total_length(&partial, StatusCode::PARTIAL_CONTENT),
            Some(200)
        );
        let full = headers(&[(header::CONTENT_LENGTH, "4177")]);
        assert_eq!(total_length(&full, StatusCode::OK), Some(4177));
    }

    #[test]
    fn only_strong_etags_and_dates_guard_a_resume() {
        let weak = Validators {
            etag: Some("W/\"a\"".to_owned()),
            last_modified: None,
        };
        assert_eq!(if_range(&weak), None);
        let dated = Validators {
            etag: Some("W/\"a\"".to_owned()),
            last_modified: Some("Tue, 01 Oct 2024 10:00:00 GMT".to_owned()),
        };
        assert_eq!(if_range(&dated), Some("Tue, 01 Oct 2024 10:00:00 GMT"));
        let strong = Validators {
            etag: Some("\"a\"".to_owned()),
            last_modified: None,
        };
        assert_eq!(if_range(&strong), Some("\"a\""));
    }
}
