//! one file from a URL into the cache. resume what can be resumed, retry what may still succeed,
//! hash every byte on its way to disk, and check the bytes against what was expected.

use std::fs;
use std::io::{self, Write};

use bytes::Bytes;
use reqwest::StatusCode;
use reqwest::header;
use serde::Serialize;
use url::Url;

use super::failure::{self, Failure, Retry};
use super::headers::{self, if_range};
use super::{IDLE, Net, host};
use crate::cache::{Cache, Claim, Partial, PartialWriter, Stored, UrlRecord, Validators};
use crate::digest::{Md5, Sha256};
use crate::error::{Error, Kind};
use crate::units;
use crate::work::Work;

/// how often to look again while another fetchloom downloads the same URL.
const POLL: std::time::Duration = std::time::Duration::from_millis(250);
/// pages a server may send before the file, such as a confirmation form.
const DIVERSIONS: u32 = 3;
/// the largest page read to find where the file is.
const LARGEST_PAGE: usize = 4 * 1024 * 1024;

/// turns a page a server sent instead of the file, such as a confirmation form, into the URL that
/// serves the file, or into an error saying why there is none, such as a download quota. it reads
/// the page with the URL the page came from, after redirects, and its status.
pub(crate) type Divert = fn(&Url, u16, &str) -> Result<Url, Error>;

/// whether a forbidden XML answer names an expired source-issued download link.
pub(crate) type Expired = fn(&str) -> bool;

/// what a download must turn out to be, when something already says so.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Expect {
    pub(crate) sha256: Option<Sha256>,
    pub(crate) md5: Option<Md5>,
    pub(crate) size: Option<u64>,
}

/// one file to download.
pub(crate) struct Fetch {
    /// the URL the file is known by. it names the unfinished download and what the URL served.
    pub(crate) url: Url,
    /// where to ask first, when that differs from `url`.
    pub(crate) from: Url,
    pub(crate) divert: Option<Divert>,
    pub(crate) expired: Option<Expired>,
    pub(crate) expect: Expect,
    /// ask the server whether the file changed since the last download, instead of trusting the
    /// cache. for links fetched outside a project, where nothing pins the bytes.
    pub(crate) revalidate: bool,
    /// the bytes are an archive the caller unpacks, so the files it unpacked to stand for it in
    /// the cache once its own object is gone.
    pub(crate) unpack: bool,
}

impl Fetch {
    pub(crate) fn new(url: Url) -> Fetch {
        Fetch {
            from: url.clone(),
            url,
            divert: None,
            expired: None,
            expect: Expect::default(),
            revalidate: false,
            unpack: false,
        }
    }
}

/// what a download reports while it runs, for progress bars and questions.
pub(crate) enum Event {
    /// another fetchloom on this machine is downloading the same URL, and this one waits for it.
    Waiting,
    /// the server answered and bytes are about to flow. `total` counts the whole file.
    Started {
        total: Option<u64>,
        resumed: u64,
    },
    Bytes(u64),
}

/// where the bytes came from this time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Origin {
    /// downloaded now, perhaps continuing an earlier download.
    Network,
    /// the server said the copy in the cache is still current.
    NotModified,
    /// taken from the cache without asking.
    Cache,
}

/// a file's bytes, ready in the cache: as an object, or for an archive the caller unpacks, as
/// the files it unpacked to.
#[derive(Debug)]
pub(crate) struct Fetched {
    pub(crate) digest: Sha256,
    pub(crate) size: u64,
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
        resumed: bool,
    },
    /// the earlier download cannot be continued, by the server or from the bytes kept, so it
    /// starts over from the first byte.
    StartOver,
    /// the server sent a page, and the page says where the file is.
    Divert(Url),
}

/// where a response body starts in the file, how long the whole file is, and how the server
/// identified it.
struct Body {
    offset: u64,
    total: Option<u64>,
    validators: Validators,
    md5: bool,
}

pub(crate) type Listener<'a> = &'a (dyn Fn(Event) -> Result<(), Error> + Sync);

impl Net {
    /// puts the file into the cache, downloading only what the cache does not already hold.
    /// `on` sees every event and may stop the download by returning an error.
    pub(crate) async fn download(
        &self,
        fetch: &Fetch,
        cache: &Cache,
        on: Listener<'_>,
    ) -> Result<Fetched, Error> {
        if let Some(found) = self.in_cache(fetch, cache) {
            return Ok(found);
        }
        let known = if fetch.revalidate {
            remembered(cache, fetch)
        } else {
            None
        };
        if self.offline.enabled() {
            return known
                .map(|record| fetched(record, Origin::Cache))
                .ok_or_else(|| failure::offline(&fetch.url, self.offline));
        }
        self.typed
            .check(&fetch.from)
            .map_err(|reason| failure::refused(&fetch.from, &reason))?;
        let (partial, waited) = self.claim(&fetch.url, cache, on).await?;
        if waited {
            // the other download may have finished what this one came for
            if let Some(found) = self.in_cache(fetch, cache) {
                return Ok(found);
            }
            if let Some(record) = fetch.revalidate.then(|| remembered(cache, fetch)).flatten() {
                return Ok(fetched(record, Origin::Cache));
            }
        }
        let _permits = self.permit(&fetch.from).await?;
        self.transfer(fetch, cache, &partial, known, on).await
    }

    /// tries one download until its bytes are in the cache and checked.
    async fn transfer(
        &self,
        fetch: &Fetch,
        cache: &Cache,
        partial: &Partial,
        known: Option<UrlRecord>,
        on: Listener<'_>,
    ) -> Result<Fetched, Error> {
        let validators = known.as_ref().map(|record| record.validators.clone());
        let mut from = fetch.from.clone();
        let mut diversions = 0;
        let mut restarted = false;
        let mut retry = Retry::new(self, &fetch.url);
        loop {
            match self
                .attempt(&from, fetch, partial, validators.as_ref(), on)
                .await
            {
                Ok(Answer::NotModified) => {
                    return known
                        .map(|record| fetched(record, Origin::NotModified))
                        .ok_or_else(|| {
                            Error::new(
                                Kind::NetworkStatus,
                                format!(
                                    "{} answered 304 to a request that did not ask for it",
                                    host(&from)
                                ),
                            )
                        });
                }
                Ok(Answer::Stored {
                    stored,
                    validators,
                    name,
                    resumed,
                }) => match verify(&fetch.url, &fetch.expect, &stored) {
                    Ok(()) => return Ok(self.remember(fetch, cache, &stored, validators, name)),
                    // bytes from two connections may not belong together, so a resumed download
                    // that does not match starts over once before it counts as a mismatch
                    Err(err) if resumed && !restarted => {
                        tracing::debug!(%err, "a resumed download did not match, starting over");
                        restarted = true;
                        partial.discard();
                    }
                    Err(err) => return Err(err),
                },
                Ok(Answer::StartOver) => {
                    tracing::debug!("the earlier download cannot be continued, starting over");
                    partial.discard();
                }
                Ok(Answer::Divert(next)) => {
                    diversions += 1;
                    if diversions > DIVERSIONS {
                        return Err(Error::new(
                            Kind::SourceInvalid,
                            format!(
                                "{} kept sending pages instead of the file",
                                host(&fetch.url)
                            ),
                        ));
                    }
                    self.typed
                        .check(&next)
                        .map_err(|reason| failure::refused(&next, &reason))?;
                    tracing::debug!(%next, "the page points elsewhere");
                    from = next;
                }
                Err(Failure::Final(err)) => return Err(progress_kept(err, partial)),
                Err(Failure::Transient {
                    reason,
                    retry_after,
                }) => {
                    retry
                        .wait(reason, retry_after, known.is_some())
                        .await
                        .map_err(|err| progress_kept(err, partial))?;
                }
            }
        }
    }

    /// the bytes a download would produce, when the expectations already identify them and the
    /// cache holds them.
    fn in_cache(&self, fetch: &Fetch, cache: &Cache) -> Option<Fetched> {
        let expect = &fetch.expect;
        let (digest, size) = match (expect.sha256, expect.md5) {
            (Some(digest), _) => (digest, expect.size),
            (None, Some(md5)) => {
                let (digest, size) = cache.stated_md5(&md5)?;
                if expect.size.is_some_and(|expected| expected != size) {
                    return None;
                }
                (digest, Some(size))
            }
            (None, None) => return None,
        };
        let size = match (cache.find(&digest, size), size) {
            (Some(_), Some(size)) => size,
            (Some(path), None) => fs::metadata(&path).ok()?.len(),
            (None, Some(size)) if holds(cache, fetch, &digest, Some(size)) => size,
            (None, _) => return None,
        };
        Work::add(&self.work.cache_hits, 1);
        Some(Fetched {
            digest,
            size,
            name: None,
            origin: Origin::Cache,
        })
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
                () = self.stop.asked() => return Err(failure::interrupted(url)),
            }
        }
    }

    async fn attempt(
        &self,
        from: &Url,
        fetch: &Fetch,
        partial: &Partial,
        known: Option<&Validators>,
        on: Listener<'_>,
    ) -> Result<Answer, Failure> {
        // a pinned hash proves the result, so even a server without validators may continue
        let pinned = fetch.expect.sha256.is_some();
        let earlier = partial
            .resumable()
            .filter(|(_, validators)| pinned || if_range(validators).is_some());
        let conditions = |mut request: reqwest::RequestBuilder| {
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
            request
        };
        Work::add(&self.work.requests, 1);
        let response = self.send(from, conditions).await?;
        let status = response.status();
        tracing::debug!(url = %super::Shown(from), status = status.as_u16(), from_byte = earlier.as_ref().map(|(received, _)| *received), "response");
        let head = response.headers().clone();
        if let Some(divert) = fetch.divert
            && is_page(status, &head)
        {
            let url = response.url().clone();
            let page = self.page(from, response).await?;
            return divert(&url, status.as_u16(), &page)
                .map(Answer::Divert)
                .map_err(Failure::Final);
        }
        if let Some(expired) = fetch.expired
            && status == StatusCode::FORBIDDEN
            && headers::is_xml(&head)
        {
            if self.expired_link(from, response, expired).await? {
                return Err(failure::transient(format!(
                    "{} supplied an expired download link",
                    host(from)
                )));
            }
            return Err(failure::refusal(from, status, &head));
        }
        let offset = match status {
            StatusCode::NOT_MODIFIED if earlier.is_none() && known.is_some() => {
                return Ok(Answer::NotModified);
            }
            StatusCode::OK => 0,
            StatusCode::PARTIAL_CONTENT => match (&earlier, headers::range_start(&head)) {
                (Some((received, _)), Some(start)) if start == *received => *received,
                _ => return Ok(Answer::StartOver),
            },
            StatusCode::RANGE_NOT_SATISFIABLE if earlier.is_some() => return Ok(Answer::StartOver),
            status => return Err(failure::refusal(from, status, &head)),
        };
        let total = headers::total_length(&head, status);
        let name = server_name(&head)?;
        let validators = headers::validators(&head);
        let body = Body {
            offset,
            total,
            validators: validators.clone(),
            md5: fetch.expect.md5.is_some(),
        };
        let Some(writer) = open(partial, body).await? else {
            return Ok(Answer::StartOver);
        };
        on(Event::Started {
            total,
            resumed: offset,
        })
        .map_err(Failure::Final)?;
        if offset > 0 {
            Work::add(&self.work.resumes, 1);
        }
        let writer = self.stream(from, writer, response, total, on).await?;
        let stored = tokio::task::spawn_blocking(move || writer.finish())
            .await
            .map_err(|err| Failure::Final(local(&io::Error::other(err))))?
            .map_err(|err| Failure::Final(local(&err)))?;
        Ok(Answer::Stored {
            stored,
            validators,
            name,
            resumed: offset > 0,
        })
    }

    /// moves the body to a writer thread, so the network never waits on the disk.
    async fn stream(
        &self,
        url: &Url,
        mut writer: PartialWriter,
        mut response: reqwest::Response,
        total: Option<u64>,
        on: Listener<'_>,
    ) -> Result<PartialWriter, Failure> {
        let (sender, mut receiver) = tokio::sync::mpsc::channel::<Bytes>(16);
        let writing = tokio::task::spawn_blocking(move || -> io::Result<PartialWriter> {
            while let Some(chunk) = receiver.blocking_recv() {
                writer.write_all(&chunk)?;
            }
            writer.save()?;
            Ok(writer)
        });
        let body = loop {
            let next = tokio::select! {
                next = tokio::time::timeout(IDLE, response.chunk()) => next,
                () = self.stop.asked() => break Err(Failure::Final(failure::interrupted(url))),
            };
            match next {
                Err(_) => {
                    break Err(failure::transient(format!(
                        "{} sent nothing for {} s",
                        host(url),
                        IDLE.as_secs()
                    )));
                }
                Ok(Err(err)) => {
                    break Err(failure::transient(format!(
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
            return Err(failure::transient(format!(
                "{} closed the connection after {} of {}",
                host(url),
                units::size(writer.written()),
                units::size(total)
            )));
        }
        Ok(writer)
    }

    async fn expired_link(
        &self,
        url: &Url,
        response: reqwest::Response,
        expired: Expired,
    ) -> Result<bool, Failure> {
        let page = tokio::select! {
            page = self.page(url, response) => page,
            () = self.stop.asked() => return Err(Failure::Final(failure::interrupted(url))),
        };
        match page {
            Ok(text) => Ok(expired(&text)),
            Err(Failure::Final(error)) if error.kind == Kind::Interrupted => {
                Err(Failure::Final(error))
            }
            // without a readable expiry message, the original 403 remains an access refusal.
            Err(_) => Ok(false),
        }
    }

    /// reads a page a server sent instead of the file.
    async fn page(&self, url: &Url, mut response: reqwest::Response) -> Result<String, Failure> {
        let mut page = Vec::new();
        loop {
            match tokio::time::timeout(IDLE, response.chunk()).await {
                Err(_) => {
                    return Err(failure::transient(format!(
                        "{} sent nothing for {} s",
                        host(url),
                        IDLE.as_secs()
                    )));
                }
                Ok(Err(err)) => {
                    return Err(failure::transient(format!(
                        "the connection to {} broke: {err}",
                        host(url)
                    )));
                }
                Ok(Ok(None)) => return Ok(String::from_utf8_lossy(&page).into_owned()),
                Ok(Ok(Some(chunk))) => {
                    Work::add(&self.work.bytes_down, chunk.len() as u64);
                    page.extend_from_slice(&chunk);
                    if page.len() > LARGEST_PAGE {
                        return Err(Failure::Final(Error::new(
                            Kind::SourceInvalid,
                            format!("{} sent a page where the file was expected", host(url)),
                        )));
                    }
                }
            }
        }
    }

    /// counts a finished download, and records what it proved for later runs.
    fn remember(
        &self,
        fetch: &Fetch,
        cache: &Cache,
        stored: &Stored,
        validators: Validators,
        name: Option<String>,
    ) -> Fetched {
        if stored.new {
            Work::add(&self.work.objects_written, 1);
        }
        // these records only spare later downloads, so failing to write one costs nothing more
        if let Some(md5) = fetch.expect.md5
            && let Err(err) = cache.save_stated_md5(&md5, &stored.digest, stored.size)
        {
            tracing::debug!(%err, "could not record the stated md5");
        }
        if fetch.revalidate {
            let record = UrlRecord {
                url: fetch.url.as_str().to_owned(),
                sha256: stored.digest,
                size: stored.size,
                name: name.clone(),
                validators,
            };
            if let Err(err) = cache.save_url_record(&record) {
                tracing::debug!(%err, "could not record what the URL served");
            }
        }
        Fetched {
            digest: stored.digest,
            size: stored.size,
            name,
            origin: Origin::Network,
        }
    }
}

fn server_name(head: &header::HeaderMap) -> Result<Option<String>, Failure> {
    let name = headers::file_name(head);
    if let Some(given) = name.as_deref()
        && given.contains(crate::name::is_bidi_control)
    {
        return Err(Failure::Final(
            Error::new(
                Kind::SourceInvalid,
                format!(
                    "the server supplied a file name with a bidi control character: \"{}\"",
                    given.escape_debug()
                ),
            )
            .help("ask the file's publisher to remove the control character from its name"),
        ));
    }
    Ok(name)
}

/// whether an answer is a page in place of the file: HTML that names no file to save, sent as a
/// success or as a refusal that asking again would not change.
fn is_page(status: StatusCode, head: &header::HeaderMap) -> bool {
    let settled = status == StatusCode::OK
        || (status.is_client_error()
            && !failure::is_transient(status)
            && status != StatusCode::RANGE_NOT_SATISFIABLE);
    settled && headers::is_html(head) && headers::file_name(head).is_none()
}

/// checks a finished download against what was expected of it.
fn verify(url: &Url, expect: &Expect, stored: &Stored) -> Result<(), Error> {
    let shown = super::Shown(url);
    if let Some(size) = expect.size
        && size != stored.size
    {
        return Err(Error::new(
            Kind::IntegrityMismatch,
            format!(
                "{} sent {} for {shown}, where {} were expected",
                host(url),
                units::size(stored.size),
                units::size(size)
            ),
        )
        .detail(format!("expected {size} bytes, got {}", stored.size)));
    }
    // a pinned SHA-256 decides first: bytes it rejects changed upstream, which the caller knows
    // how to fix, while an MD5 alone that disagrees means the source contradicts itself
    if let Some(expected) = expect.sha256
        && expected != stored.digest
    {
        return Err(Error::new(
            Kind::IntegrityMismatch,
            format!("{} sent other bytes than expected for {shown}", host(url)),
        )
        .detail(format!(
            "expected sha256 {}, got {}",
            expected.short(),
            stored.digest.short()
        )));
    }
    if let (Some(stated), Some(got)) = (expect.md5, stored.md5)
        && stated != got
    {
        return Err(Error::new(
            Kind::IntegrityMismatch,
            format!(
                "{} sent bytes whose MD5 is not the one it states for {shown}",
                host(url)
            ),
        )
        .detail(format!("stated md5 {stated}, got {got}"))
        .help("try again later. if it keeps happening, the copy on the server may be damaged"));
    }
    Ok(())
}

/// what the cache holds for the fetch's URL from an earlier download, when the bytes are still
/// there.
fn remembered(cache: &Cache, fetch: &Fetch) -> Option<UrlRecord> {
    let record = cache.url_record(fetch.url.as_str())?;
    holds(cache, fetch, &record.sha256, Some(record.size)).then_some(record)
}

/// whether the cache holds these bytes: as an object, or, for an archive the caller unpacks, as
/// every file it unpacked to.
fn holds(cache: &Cache, fetch: &Fetch, digest: &Sha256, size: Option<u64>) -> bool {
    cache.find(digest, size).is_some() || fetch.unpack && cache.holds_unpacked(digest, None)
}

fn fetched(record: UrlRecord, origin: Origin) -> Fetched {
    Fetched {
        digest: record.sha256,
        size: record.size,
        name: record.name,
        origin,
    }
}

fn local(err: &io::Error) -> Error {
    Error::io("could not write to the cache", err)
}

/// opens the writing end of a download on a blocking thread, since continuing one hashes the
/// bytes kept so far again. none when those bytes are not the ones recorded, so the download
/// starts over.
async fn open(partial: &Partial, body: Body) -> Result<Option<PartialWriter>, Failure> {
    let Body {
        offset,
        total,
        validators,
        md5,
    } = body;
    let partial = partial.clone();
    let opened =
        tokio::task::spawn_blocking(move || partial.writer(offset, total, validators, md5))
            .await
            .map_err(|err| Failure::Final(local(&io::Error::other(err))))?;
    match opened {
        Ok(writer) => Ok(Some(writer)),
        Err(err) if err.kind() == io::ErrorKind::InvalidData => {
            tracing::debug!(%err, "the earlier download cannot be continued");
            Ok(None)
        }
        Err(err) => Err(Failure::Final(local(&err))),
    }
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

#[cfg(test)]
mod tests;
