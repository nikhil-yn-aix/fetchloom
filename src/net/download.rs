//! one file from a URL into the cache. resume what can be resumed, retry what may still succeed,
//! hash every byte on its way to disk, and check the bytes against what was expected.

use std::fs;
use std::io;

use bytes::Bytes;
use reqwest::StatusCode;
use reqwest::header;
use serde::Serialize;
use url::Url;

use super::failure::{self, Failure, Retry};
use super::headers::{self, if_range};
use super::{IDLE, Net, Splitting, host};
use crate::cache::{Cache, Claim, Partial, PartialWriter, Stored, UrlRecord, Validators};
use crate::digest::{Md5, Sha256};
use crate::error::{Error, Kind};
use crate::units;
use crate::work::Work;

mod split;

pub(crate) use split::{SLOW, SMALLEST_PIECE, SPLIT_ABOVE, WINDOW};

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

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Expect {
    pub(crate) sha256: Option<Sha256>,
    pub(crate) md5: Option<Md5>,
    pub(crate) size: Option<u64>,
}

impl Expect {
    /// whether the download must also compute an MD5. bytes that match a pinned SHA-256 match
    /// the MD5 recorded beside it, and computing both costs over three times the hashing alone.
    pub(crate) fn needs_md5(&self) -> bool {
        self.md5.is_some() && self.sha256.is_none()
    }

    /// whether these hashes prove a file's bytes, so saved bytes may be continued, or pieces from
    /// several answers joined, without a validator. zenodo states an MD5 and sends no validators.
    pub(crate) fn proves(&self) -> bool {
        self.sha256.is_some() || self.md5.is_some()
    }
}

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
    /// the bytes a download would produce, when the expectations already identify them and the
    /// cache holds them.
    pub(crate) fn cached(&self, cache: &Cache) -> Option<Fetched> {
        let expect = &self.expect;
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
            (None, Some(size)) if holds(cache, self, &digest, Some(size)) => size,
            (None, _) => return None,
        };
        Some(Fetched {
            digest,
            size,
            name: None,
            origin: Origin::Cache,
        })
    }

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
    WebPage {
        name: Option<String>,
    },
    Bytes(u64),
}

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
    /// keep the ranges an earlier split download finished past `offset`.
    keep: bool,
}

/// how one stream of an answer ended.
enum Run {
    /// every byte arrived.
    Whole(PartialWriter),
    /// the stream moved less than the slow rate over a window, and hands what it wrote and the
    /// rest of its answer over to be split. `rate` is what it moved, in bytes a second.
    Slow {
        writer: PartialWriter,
        response: reqwest::Response,
        total: u64,
        rate: f64,
    },
}

pub(crate) type Listener<'a> = &'a (dyn Fn(Event) -> Result<(), Error> + Sync);

/// how much a stream gathers before the writer thread takes it. a 16 KiB chunk at a time woke
/// the writer for each, which cost more than the download on a fast link.
const BATCH: usize = 256 * 1024;

/// chunks on their way to the writer thread, handed over together without copying.
#[derive(Default)]
struct Batch {
    chunks: Vec<Bytes>,
    bytes: usize,
}

impl Batch {
    fn push(&mut self, chunk: Bytes) {
        self.bytes += chunk.len();
        self.chunks.push(chunk);
    }

    fn full(&self) -> bool {
        self.bytes >= BATCH
    }

    /// sends what was gathered. an error means the writer stopped.
    async fn flush(&mut self, sender: &tokio::sync::mpsc::Sender<Vec<Bytes>>) -> Result<(), ()> {
        if self.chunks.is_empty() {
            return Ok(());
        }
        self.bytes = 0;
        sender
            .send(std::mem::take(&mut self.chunks))
            .await
            .map_err(|_| ())
    }
}

impl Net {
    /// puts the file into the cache, downloading only what the cache does not already hold.
    /// `on` sees every event and may stop the download by returning an error.
    pub(crate) async fn download(
        &self,
        fetch: &Fetch,
        cache: &Cache,
        on: Listener<'_>,
    ) -> Result<Fetched, Error> {
        // one span per file, so a timeline shows each download as a bar with its link
        let span = tracing::debug_span!("download", url = %crate::net::Shown(&fetch.url));
        tracing::Instrument::instrument(self.download_in_span(fetch, cache, on), span).await
    }

    async fn download_in_span(
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
                .ok_or_else(|| {
                    // a link fetched before has a record, so its bytes are the ones missing
                    let pinned = fetch.expect.sha256.is_some()
                        || cache.url_record(fetch.url.as_str()).is_some();
                    failure::offline(&fetch.url, self.offline, pinned)
                });
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
        // a file stops being split once a server answered a range badly, or bytes from several
        // answers did not match
        let mut split = true;
        let mut retry = Retry::new(self, &fetch.url);
        loop {
            match self
                .attempt(&from, fetch, partial, validators.as_ref(), on, &mut split)
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
                    // bytes from two connections may not belong together, so a resumed or split
                    // download that does not match starts over once, as one stream, before it
                    // counts as a mismatch
                    Err(err) if resumed && !restarted => {
                        tracing::debug!(%err, "a download in pieces did not match, starting over");
                        restarted = true;
                        split = false;
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
                Err(Failure::Limited { reason }) => {
                    retry
                        .limited(reason, known.is_some())
                        .await
                        .map_err(|err| progress_kept(err, partial))?;
                }
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

    fn in_cache(&self, fetch: &Fetch, cache: &Cache) -> Option<Fetched> {
        let fetched = fetch.cached(cache)?;
        Work::add(&self.work.cache_hits, 1);
        Some(fetched)
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
                // this run may hold the link itself, finished and waiting for its batch flush
                Ok(Claim::Busy)
                    if cache
                        .publish_download_of(url.as_str())
                        .map_err(|err| local(&err))? =>
                {
                    waited = true;
                    continue;
                }
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
        split: &mut bool,
    ) -> Result<Answer, Failure> {
        let pinned = fetch.expect.proves();
        let earlier = partial
            .resumable()
            .filter(|(_, validators)| pinned || if_range(validators).is_some());
        Work::add(&self.work.requests, 1);
        let response = self.send(from, conditions(earlier.as_ref(), known)).await?;
        let status = response.status();
        tracing::debug!(url = %super::Shown(from), status = status.as_u16(), from_byte = earlier.as_ref().map(|(received, _)| *received), "response");
        let head = response.headers().clone();
        let response = match self.instead(from, fetch, response).await? {
            Ok(response) => response,
            Err(next) => return Ok(Answer::Divert(next)),
        };
        let validators = headers::validators(&head);
        if status == StatusCode::NOT_MODIFIED && earlier.is_none() && known.is_some() {
            return Ok(Answer::NotModified);
        }
        let Some(offset) = starting_offset(from, status, &head, earlier.as_ref(), &validators)?
        else {
            return Ok(Answer::StartOver);
        };
        let total = headers::total_length(&head, status);
        let name = describe_response(&response, &fetch.url, on)?;
        let proven = pinned || if_range(&validators).is_some();
        let splits = *split && self.may_split(total, status, &head, proven);
        let body = Body {
            offset,
            total,
            validators: validators.clone(),
            md5: fetch.expect.needs_md5(),
            keep: splits,
        };
        let Some(writer) = open(partial, body).await? else {
            return Ok(Answer::StartOver);
        };
        let kept = writer.kept();
        on(Event::Started {
            total,
            resumed: offset + kept,
        })
        .map_err(Failure::Final)?;
        if offset + kept > 0 {
            Work::add(&self.work.resumes, 1);
        }
        // ranges kept past the prefix leave gaps that only the split path fills
        let at_once = self.split.when == Splitting::Always || kept > 0;
        let watch = total.filter(|_| splits);
        // a stream held back by FETCHLOOM_RATE looks slow, and more streams share the same cap
        let monitor = watch.filter(|_| self.rate.is_none());
        let (writer, response, total, slow) = match watch {
            Some(total) if at_once => (writer, response, total, None),
            _ => match self
                .stream(from, writer, response, total, on, monitor)
                .await?
            {
                Run::Whole(writer) => {
                    return Ok(Answer::Stored {
                        stored: finish(writer).await?,
                        validators,
                        name,
                        resumed: offset + kept > 0,
                    });
                }
                Run::Slow {
                    writer,
                    response,
                    total,
                    rate,
                } => (writer, response, total, Some(rate)),
            },
        };
        let job = split::Job {
            from,
            key: &fetch.from,
            validators: &validators,
            total,
            on,
            slow,
        };
        let (writer, pieced) = match self.split(job, writer, response, split).await? {
            split::Streamed::Whole { writer, pieced } => (*writer, pieced),
            split::Streamed::StartOver => return Ok(Answer::StartOver),
        };
        Ok(Answer::Stored {
            stored: finish(writer).await?,
            validators,
            name,
            resumed: offset + kept > 0 || pieced,
        })
    }

    /// whether a file of `total` bytes may come in ranges from several answers: large enough, from
    /// a server that serves ranges, with proof the pieces belong to one version.
    fn may_split(
        &self,
        total: Option<u64>,
        status: StatusCode,
        head: &reqwest::header::HeaderMap,
        proven: bool,
    ) -> bool {
        self.split.when != Splitting::Never
            && total.is_some_and(|total| total >= self.split.above)
            && (status == StatusCode::PARTIAL_CONTENT || headers::accepts_ranges(head))
            && proven
    }

    /// the answer itself when it brings the file. otherwise what it means instead: the URL a
    /// page's handler says the file is at, or an expired link to try again or a refusal.
    async fn instead(
        &self,
        from: &Url,
        fetch: &Fetch,
        response: reqwest::Response,
    ) -> Result<Result<reqwest::Response, Url>, Failure> {
        let status = response.status();
        if let Some(divert) = fetch.divert
            && is_page(status, response.headers())
        {
            let url = response.url().clone();
            let page = self.page(from, response).await?;
            return divert(&url, status.as_u16(), &page)
                .map(Err)
                .map_err(Failure::Final);
        }
        if let Some(expired) = fetch.expired
            && status == StatusCode::FORBIDDEN
            && headers::is_xml(response.headers())
        {
            let head = response.headers().clone();
            if self.expired_link(from, response, expired).await? {
                return Err(failure::transient(format!(
                    "{} supplied an expired download link",
                    host(from)
                )));
            }
            return Err(failure::refusal(from, status, &head));
        }
        Ok(Ok(response))
    }

    /// moves the body to a writer thread, so the network never waits on the disk. with `watch`,
    /// the length of a file that may split, it measures its rate over each window and hands over
    /// to the split path once one window moved less than the slow rate, while enough is left for
    /// another stream to take a piece. the answer is then read exactly up to what the writer
    /// holds.
    async fn stream(
        &self,
        url: &Url,
        mut writer: PartialWriter,
        mut response: reqwest::Response,
        total: Option<u64>,
        on: Listener<'_>,
        watch: Option<u64>,
    ) -> Result<Run, Failure> {
        let reached = writer.written();
        let (sender, mut receiver) = tokio::sync::mpsc::channel::<Vec<Bytes>>(16);
        let writing = tokio::task::spawn_blocking(move || -> io::Result<PartialWriter> {
            while let Some(batch) = receiver.blocking_recv() {
                for chunk in &batch {
                    writer.write_chunk(writer.written(), chunk)?;
                }
            }
            writer.save()?;
            Ok(writer)
        });
        let body = self
            .read_stream(url, &mut response, &sender, reached, on, watch)
            .await;
        drop(sender);
        let writer = writing
            .await
            .map_err(|err| Failure::Final(local(&io::Error::other(err))))?
            .map_err(|err| Failure::Final(local(&err)))?;
        if let Some((total, rate)) = body? {
            tracing::debug!(
                url = %super::Shown(url),
                rate,
                at = writer.written(),
                "one stream is slow, so the download splits"
            );
            return Ok(Run::Slow {
                writer,
                response,
                total,
                rate,
            });
        }
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
        Ok(Run::Whole(writer))
    }

    async fn read_stream(
        &self,
        url: &Url,
        response: &mut reqwest::Response,
        sender: &tokio::sync::mpsc::Sender<Vec<Bytes>>,
        reached: u64,
        on: Listener<'_>,
        watch: Option<u64>,
    ) -> Result<Option<(u64, f64)>, Failure> {
        let mut batch = Batch::default();
        let read = self
            .read_batches(url, response, sender, &mut batch, reached, on, watch)
            .await;
        // whatever arrived is kept, so a resume or a split goes on from the last byte received
        let _ = batch.flush(sender).await;
        read
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "the stream's state, passed as it was"
    )]
    async fn read_batches(
        &self,
        url: &Url,
        response: &mut reqwest::Response,
        sender: &tokio::sync::mpsc::Sender<Vec<Bytes>>,
        batch: &mut Batch,
        mut reached: u64,
        on: Listener<'_>,
        watch: Option<u64>,
    ) -> Result<Option<(u64, f64)>, Failure> {
        let window = self.split.window;
        let lap = tokio::time::sleep(window);
        tokio::pin!(lap);
        let (mut since, mut moved) = (std::time::Instant::now(), 0u64);
        // the idle deadline moves with every chunk without touching the timer, which is reset
        // only when it fires early
        let mut idle = tokio::time::Instant::now() + self.idle;
        let quiet = tokio::time::sleep_until(idle);
        tokio::pin!(quiet);
        let stopped = self.stop.asked();
        tokio::pin!(stopped);
        loop {
            let next = tokio::select! {
                next = response.chunk() => next,
                () = &mut quiet => {
                    if tokio::time::Instant::now() < idle {
                        quiet.as_mut().reset(idle);
                        continue;
                    }
                    break Err(failure::transient(format!(
                        "{} sent nothing for {} s",
                        host(url),
                        self.idle.as_secs()
                    )));
                }
                () = &mut lap, if watch.is_some() => {
                    let now = std::time::Instant::now();
                    let seconds = now.duration_since(since).as_secs_f64().max(f64::EPSILON);
                    #[expect(clippy::cast_precision_loss, reason = "a rate needs no exact count")]
                    let rate = moved as f64 / seconds;
                    #[expect(clippy::cast_precision_loss, reason = "a rate needs no exact count")]
                    let slow = rate < self.split.slow as f64;
                    if let Some(total) = watch
                        && slow
                        && total.saturating_sub(reached) >= 2 * self.split.piece
                    {
                        break Ok(Some((total, rate)));
                    }
                    (since, moved) = (now, 0);
                    lap.as_mut().reset(tokio::time::Instant::now() + window);
                    continue;
                }
                () = &mut stopped => break Err(Failure::Final(failure::interrupted(url))),
            };
            match next {
                Err(err) => {
                    break Err(failure::transient(format!(
                        "the connection to {} broke: {err}",
                        host(url)
                    )));
                }
                Ok(None) => break Ok(None),
                Ok(Some(chunk)) => {
                    idle = tokio::time::Instant::now() + self.idle;
                    if let Err(err) = self.throttle(url, chunk.len()).await {
                        break Err(err);
                    }
                    let length = chunk.len() as u64;
                    Work::add(&self.work.bytes_down, length);
                    (reached, moved) = (reached + length, moved + length);
                    if let Err(err) = on(Event::Bytes(length)) {
                        break Err(Failure::Final(err));
                    }
                    batch.push(chunk);
                    // a closed channel means the writer failed, and its own error says why
                    if batch.full() && batch.flush(sender).await.is_err() {
                        break Ok(None);
                    }
                }
            }
        }
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
                    self.throttle(url, chunk.len()).await?;
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

fn describe_response(
    response: &reqwest::Response,
    url: &Url,
    on: Listener<'_>,
) -> Result<Option<String>, Failure> {
    let name = headers::file_name(response.headers()).or_else(|| {
        let path = crate::name::from_url(url).unwrap_or_default();
        if std::path::Path::new(&path).extension().is_some() {
            return None;
        }
        response
            .extensions()
            .get::<super::RedirectName>()
            .map(|name| name.0.clone())
    });
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
    if headers::is_html(response.headers()) {
        on(Event::WebPage { name: name.clone() }).map_err(Failure::Final)?;
    }
    Ok(name)
}

/// accepts a full body or a range that continues the same representation at the saved offset.
fn starting_offset(
    from: &Url,
    status: StatusCode,
    head: &header::HeaderMap,
    earlier: Option<&(u64, Validators)>,
    validators: &Validators,
) -> Result<Option<u64>, Failure> {
    match status {
        StatusCode::OK => Ok(Some(0)),
        // a server that ignores If-Range sends the rest of whatever version it holds now
        StatusCode::PARTIAL_CONTENT => match (earlier, headers::range_start(head)) {
            (Some((received, recorded)), Some(start))
                if start == *received && !headers::changed(recorded, validators) =>
            {
                Ok(Some(*received))
            }
            _ => Ok(None),
        },
        StatusCode::RANGE_NOT_SATISFIABLE if earlier.is_some() => Ok(None),
        status => Err(failure::refusal(from, status, head)),
    }
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
        .help("run the same command again later. if it keeps happening, the copy on the server may be damaged"));
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

/// the headers that ask for the rest of an earlier download, which `If-Range` keeps to the
/// version it came from, or else for the file only when it changed since `known`.
fn conditions<'a>(
    earlier: Option<&'a (u64, Validators)>,
    known: Option<&'a Validators>,
) -> impl Fn(reqwest::RequestBuilder) -> reqwest::RequestBuilder + 'a {
    move |mut request| {
        if let Some((received, validators)) = earlier {
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
    }
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
        keep,
    } = body;
    let partial = partial.clone();
    let opened =
        tokio::task::spawn_blocking(move || partial.writer(offset, total, validators, md5, keep))
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

/// puts the finished file into the cache on a blocking thread, since that flushes it.
async fn finish(writer: PartialWriter) -> Result<Stored, Failure> {
    tokio::task::spawn_blocking(move || writer.finish())
        .await
        .map_err(|err| Failure::Final(local(&io::Error::other(err))))?
        .map_err(|err| Failure::Final(local(&err)))
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
