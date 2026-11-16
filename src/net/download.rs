//! one file from a URL into the cache. resume what can be resumed, retry what may still succeed,
//! hash every byte on its way to disk, and check the bytes against what was expected.

use std::fs;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use bytes::Bytes;
use reqwest::StatusCode;
use reqwest::header;
use serde::Serialize;
use url::Url;

use super::failure::{self, Failure, Retry};
use super::headers::{self, NoVersion, if_range};
use super::{Net, Splitting, host};
use crate::cache::{Cache, Claim, Partial, PartialWriter, Stored, UrlRecord, Validators};
use crate::digest::{Md5, Sha256};
use crate::error::{Error, Kind};
use crate::units;
use crate::work::Work;

mod probe;
mod split;

pub(crate) use probe::Probed;
pub(crate) use split::starting;

/// how often to look again while another fetchloom downloads the same URL.
const POLL: Duration = Duration::from_millis(250);
/// pages a server may send before the file, such as a confirmation form.
const DIVERSIONS: u32 = 3;
/// the largest page read to find where the file is.
const LARGEST_PAGE: usize = 4 * 1024 * 1024;

/// turns a page sent instead of the file, such as a confirmation form, into the file's URL or an
/// error such as a download quota, given the page's URL after redirects, its status and its text.
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
    fn needs_md5(&self) -> bool {
        self.md5.is_some() && self.sha256.is_none()
    }

    /// whether these hashes prove a file's bytes, so saved bytes may be continued, or pieces from
    /// several answers joined, without a validator. zenodo states an MD5 and sends no validators.
    pub(crate) fn proves(&self) -> bool {
        self.sha256.is_some() || self.md5.is_some()
    }

    /// whether the hashes alone decide whether the bytes are the file: a pinned SHA-256, or a
    /// stated MD5 with the size. no validator then matters: the final check catches other bytes.
    pub(crate) fn pins(&self) -> bool {
        self.sha256.is_some() || (self.md5.is_some() && self.size.is_some())
    }
}

#[derive(Clone)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each says one thing of the download apart from the others"
)]
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
    /// the bytes a revalidated download may find unchanged, in the cache or a caller's file such
    /// as `get -o` wrote. when the url's record names them, the server is asked whether they
    /// changed, and a large file asks about its size only once the server sends it.
    pub(crate) held: Option<Sha256>,
    /// the bytes are an archive the caller unpacks, so the files it unpacked to stand for it in
    /// the cache once its own object is gone.
    pub(crate) unpack: bool,
    /// the bytes are a mirror's copy, which mirror-only mode lets the request ask for.
    pub(crate) mirror: bool,
    /// the person asked for the web page itself, so one is the file rather than a mistake.
    pub(crate) page: bool,
    /// the file takes the name its server gives, as a drive link's does, so its progress line
    /// shows that name once the server answers.
    pub(crate) named: bool,
    /// what retry lines call the file: the name its progress line shows. without one they name
    /// the link's last part, which may be an id, a hash or `content`.
    pub(crate) label: Option<String>,
}

impl Fetch {
    /// the bytes a download would produce, when the expectations already identify them and the
    /// cache holds them.
    pub(crate) fn cached(&self, cache: &Cache) -> Option<Fetched> {
        let expect = &self.expect;
        let (digest, size, version) = match (expect.sha256, expect.md5) {
            (Some(digest), _) => (digest, expect.size, None),
            (None, Some(md5)) => {
                let stated = cache.stated(&md5, self.url.as_str())?;
                if expect.size.is_some_and(|expected| expected != stated.size) {
                    return None;
                }
                (stated.digest, Some(stated.size), stated.version)
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
            moved: 0,
            name: None,
            origin: Origin::Cache,
            version,
            began: None,
            arrived: None,
            asked: Duration::ZERO,
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
            held: None,
            unpack: false,
            mirror: false,
            page: false,
            named: false,
            label: None,
        }
    }

    /// what the url served the last time it was downloaded, when those bytes are at hand: in the
    /// cache, or as the ones `held` names.
    pub(crate) fn remembered(&self, cache: &Cache) -> Option<UrlRecord> {
        let record = cache.url_record(self.url.as_str())?;
        (self.held == Some(record.sha256) || holds(cache, self, &record.sha256, Some(record.size)))
            .then_some(record)
    }
}

/// what a download reports while it runs, for progress bars and questions.
pub(crate) enum Event {
    /// another fetchloom on this machine is downloading the same URL, and this one waits for it.
    Waiting,
    /// the cache's disk cannot lock files, so the download keeps its bytes in a file of its own
    /// that no later run can find, and a stop deletes them.
    Lockless,
    /// the bytes kept are no longer as the download left them on this disk, as when they no
    /// longer hash as recorded or were made read-only, so it starts over from the first byte.
    Spoiled,
    /// the file's link is gone, so it is looked up again and comes from a new link, which goes on
    /// from nothing kept of the old one: no later run asks the old link again.
    Moved,
    /// an earlier run kept `bytes` of the file, which the download goes on from when the server
    /// allows it, and what its record says a later run can do with them.
    Kept {
        bytes: u64,
        resumable: Resumable,
    },
    /// the server answered and bytes are about to flow. `total` counts the whole file, and
    /// `resumable` says whether a later run could go on from the bytes this one keeps.
    Started {
        total: Option<u64>,
        resumed: u64,
        resumable: Resumable,
    },
    WebPage {
        name: Option<String>,
    },
    /// the server named the file, by its answer's `Content-Disposition` or a redirect's, before
    /// any byte flows.
    Named(String),
    /// the server compressed the bytes on their way, as `Content-Encoding` names, lowercased.
    /// fetchloom keeps them as sent, since a hash published for the file matches those.
    Encoded(String),
    Bytes(u64),
    /// every byte is in, and the disk keeps the file now.
    Arrived,
    /// the disk is behind taking the bytes that arrived, so the download waits for it, or no
    /// longer does.
    Behind(bool),
    /// a try failed, and try `next` of at most `of` goes out after `wait`, once the rate limit of
    /// the host `limit` names resets when it names one. `open` says the file came on in an
    /// earlier try, so more tries follow while it keeps coming.
    Retrying {
        next: u32,
        of: u32,
        wait: Duration,
        limit: Option<String>,
        open: bool,
    },
}

/// whether a later run can go on from a stopped download's bytes: it needs proof the rest is the
/// same file, a pinned hash or a validator [`if_range`] can send, and a server that sends parts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Resumable {
    /// the server said it sends parts of a file, or sent one, and the bytes are proven.
    Yes,
    /// the bytes are proven, but the server has not said whether it sends parts of a file.
    #[default]
    Maybe,
    /// the server said it sends no parts, so a later run starts over.
    No,
    /// nothing proves the bytes are one file, neither a hash a pin or the source states nor a
    /// version the server named, for the reason it holds, so a later run starts over.
    Unproven(NoVersion),
}

impl Resumable {
    /// what an answer of `status` with `head` allows, for bytes proven to be one file unless
    /// `unproven` says why not. asked for the rest of kept bytes with validators `asked`, a whole
    /// answer of that version means the server sends no parts, whatever its `Accept-Ranges` says,
    /// since the next run asks it the same; one of another version is a restart that says nothing
    /// about parts.
    fn answered(
        status: StatusCode,
        head: &header::HeaderMap,
        unproven: Option<NoVersion>,
        asked: Option<&Validators>,
    ) -> Resumable {
        if let Some(why) = unproven {
            return Resumable::Unproven(why);
        }
        let ignored = status == StatusCode::OK
            && asked.is_some_and(|kept| headers::same_version(kept, &headers::validators(head)));
        if ignored {
            Resumable::No
        } else if status == StatusCode::PARTIAL_CONTENT || headers::accepts_ranges(head) {
            Resumable::Yes
        } else if head.contains_key(header::ACCEPT_RANGES) {
            Resumable::No
        } else {
            Resumable::Maybe
        }
    }

    /// what bytes an earlier run kept allow before any server answers: whatever the record
    /// proves, since the record does not say whether the server sends parts.
    fn kept(unproven: Option<NoVersion>) -> Resumable {
        unproven.map_or(Resumable::Maybe, Resumable::Unproven)
    }
}

/// why bytes under `validators` are not proven one file, unless a hash `expect` states proves
/// them or the validators name a version a request for the rest may name.
fn unproven(expect: &Expect, validators: &Validators) -> Option<NoVersion> {
    if expect.proves() {
        return None;
    }
    headers::version(validators).err()
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
    /// the bytes this run received for the file: less than `size` when an earlier run's bytes
    /// were kept, more when a try started over, none when the cache or the server's 304 answered.
    pub(crate) moved: u64,
    /// the file name the server gave, when it gave one.
    pub(crate) name: Option<String>,
    pub(crate) origin: Origin,
    /// the S3 version the bytes were, when their bucket keeps versions.
    pub(crate) version: Option<String>,
    /// when the download's first request went out, none when nothing was downloaded.
    pub(crate) began: Option<Instant>,
    /// when the last byte this run received arrived, before the disk kept the file. none when
    /// nothing was received.
    pub(crate) arrived: Option<Instant>,
    /// how long the download waited for a person to say whether it may go on, as its size
    /// asks, which its time leaves out.
    pub(crate) asked: Duration,
}

impl Fetched {
    /// how long the downloads among `fetched` took from `started`: until their last byte arrived,
    /// so a slow disk does not count against the link, and until now, which counts it. each leaves
    /// out its wait for a person's answer; with nothing received, both are the time since then.
    pub(crate) fn transfer<'a>(
        fetched: impl IntoIterator<Item = &'a Fetched>,
        started: Instant,
    ) -> units::Took {
        Fetched::transfer_until(fetched, started, Instant::now())
    }

    /// how long the downloads among `fetched` took themselves, from the first one's first request
    /// to the last byte, for downloads that ran beside others: the time before belongs to the
    /// files ahead of them and the time after to the files behind and the disk they share.
    pub(crate) fn transfer_alone<'a>(
        fetched: impl IntoIterator<Item = &'a Fetched>,
    ) -> units::Took {
        let mut span: Option<(Instant, Instant)> = None;
        for fetched in fetched {
            let (Some(began), Some(arrived)) = (fetched.began, fetched.arrived) else {
                continue;
            };
            let arrived = arrived.checked_sub(fetched.asked).unwrap_or(arrived);
            span = Some(span.map_or((began, arrived), |(first, last)| {
                (first.min(began), last.max(arrived))
            }));
        }
        units::Took::from(span.map_or(Duration::ZERO, |(first, last)| {
            last.saturating_duration_since(first)
        }))
    }

    /// [`Fetched::transfer`], with `now` as the moment it ends.
    fn transfer_until<'a>(
        fetched: impl IntoIterator<Item = &'a Fetched>,
        started: Instant,
        now: Instant,
    ) -> units::Took {
        let all = now.saturating_duration_since(started);
        // the latest last byte without each one's wait for an answer, and the latest that came
        let latest = fetched
            .into_iter()
            .filter_map(|fetched| {
                let arrived = fetched.arrived?.saturating_duration_since(started);
                Some((arrived.saturating_sub(fetched.asked), arrived))
            })
            .reduce(|(own, came), (other, then)| (own.max(other), came.max(then)));
        match latest {
            Some((arrived, came)) => units::Took {
                arrived,
                all: arrived.saturating_add(all.saturating_sub(came)),
            },
            None => units::Took { arrived: all, all },
        }
    }
}

enum Answer {
    NotModified,
    Stored {
        received: Received,
        validators: Validators,
        told: Told,
        resumed: bool,
    },
    /// the earlier download cannot be continued, by the server or from the bytes kept, so it
    /// starts over from the first byte.
    StartOver,
    /// the server sent a page, and the page says where the file is.
    Divert(Url),
    /// the link the source gave only for a while expired, so the source is asked for a fresh one.
    Relink(Box<dyn super::Relink>),
}

/// what an answer said of the bytes it brought: the file name it gave, and whether they came as
/// a web page.
#[derive(Default)]
struct Told {
    name: Option<String>,
    page: bool,
}

/// what an answer asks for in place of being read as the file.
enum Instead {
    /// the link a page points to.
    Divert(Url),
    /// a fresh link from the source, for one that expired.
    Relink(Box<dyn super::Relink>),
}

/// splitting and request ownership across this file's retries.
struct Transfer {
    split: bool,
    permits: Option<super::Permits>,
    /// a request for the rest got a part of the same file from its first byte, the whole file
    /// again, so the try that starts over tells it as an answer that sends no parts.
    sent_whole: bool,
}

/// where a response starts, its total size and the server's version proof.
struct Body {
    offset: u64,
    total: Option<u64>,
    validators: Validators,
    md5: bool,
    /// keep the ranges an earlier split download finished past `offset`.
    keep: bool,
    /// take the bytes before `offset` of a part no record names, which a stated hash checks once
    /// the file is in.
    unrecorded: bool,
    hashing: crate::cache::Hashing,
}

trait Chunks: Send {
    fn chunk(
        &mut self,
    ) -> impl std::future::Future<Output = Result<Option<Bytes>, reqwest::Error>> + Send;
}

impl Chunks for reqwest::Response {
    async fn chunk(&mut self) -> Result<Option<Bytes>, reqwest::Error> {
        self.chunk().await
    }
}

/// a claimed download until it ends. one that fails, or whose run drops it, forgets bytes no
/// later run could go on from, which would only take room on the disk until a gc.
struct Unfinished<'a> {
    partial: &'a Partial,
    /// whether the last answer, or the bytes an earlier run kept, let a later run go on.
    continuable: AtomicBool,
    done: bool,
}

impl Drop for Unfinished<'_> {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        // no later run can find a part kept without a lock, and gc never sweeps one
        if *self.continuable.get_mut() && !self.partial.lockless() {
            self.partial.discard_unstarted();
        } else {
            self.partial.discard();
        }
    }
}

/// how one stream of an answer ended.
enum Run {
    /// every byte arrived.
    Whole(PartialWriter),
    /// a measured stream hands its accepted bytes and remaining answer to the range scheduler.
    Handover {
        writer: PartialWriter,
        response: reqwest::Response,
        total: u64,
        rate: f64,
    },
}

pub(crate) type Listener<'a> = &'a (dyn Fn(Event) -> Result<(), Error> + Sync);

/// the least a body moves while the timeout passes waiting on its server, so one that sends a
/// byte now and then counts as stalled, as curl's --speed-limit does, at a pace any working link
/// beats.
const LEAST_MOVED: u64 = 1024;

/// what a body moved over the time its server took to send it, judged in windows as long as the
/// timeout, so a body that trickles in fails a try as one that goes silent does. time this
/// machine spends, on the rate cap, the disk or a question, is no time the server took.
#[derive(Default)]
pub(super) struct Trickle {
    waited: Duration,
    moved: u64,
}

impl Trickle {
    /// counts `bytes` that took the server `waited` to send. a window as long as `timeout` that
    /// moved less than [`LEAST_MOVED`] gives back what it moved. a wait as long as the timeout
    /// itself starts a new window: only a process stopped past its deadline, as a sleeping
    /// laptop's is, finds bytes after one, and they are no trickle.
    pub(super) fn heard(
        &mut self,
        bytes: u64,
        waited: Duration,
        timeout: Duration,
    ) -> Result<(), u64> {
        if waited >= timeout {
            *self = Trickle::default();
            return Ok(());
        }
        self.waited += waited;
        self.moved = self.moved.saturating_add(bytes);
        if self.waited < timeout {
            return Ok(());
        }
        let moved = std::mem::take(self).moved;
        if moved < LEAST_MOVED {
            Err(moved)
        } else {
            Ok(())
        }
    }
}

/// a body that moved only `moved` bytes from `url` while the `timeout` passed.
pub(super) fn trickled(url: &Url, moved: u64, timeout: Duration) -> Failure {
    failure::transient(format!(
        "{} sent only {} in {} s",
        host(url),
        units::size(moved),
        timeout.as_secs()
    ))
}

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

/// how far into the file the bytes a download holds reach, so a try that got further than any
/// before earns the tries back, while one that only fetches the same bytes again does not.
#[derive(Default)]
struct Reach {
    held: AtomicU64,
    furthest: AtomicU64,
    moved: AtomicU64,
}

impl Reach {
    fn hear(&self, event: &Event) {
        match event {
            Event::Started { resumed, .. } => self.held.store(*resumed, Ordering::Relaxed),
            Event::Bytes(bytes) => {
                self.held.fetch_add(*bytes, Ordering::Relaxed);
                self.moved.fetch_add(*bytes, Ordering::Relaxed);
            }
            Event::Waiting
            | Event::Lockless
            | Event::Spoiled
            | Event::Moved
            | Event::Kept { .. }
            | Event::WebPage { .. }
            | Event::Named(_)
            | Event::Encoded(_)
            | Event::Arrived
            | Event::Behind(_)
            | Event::Retrying { .. } => {}
        }
    }

    /// how much further than any try before the last one reached.
    fn further(&self) -> u64 {
        let held = self.held.load(Ordering::Relaxed);
        held.saturating_sub(self.furthest.fetch_max(held, Ordering::Relaxed))
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
        let span = tracing::debug_span!("download", url = %crate::shown::Shown(&fetch.url));
        let result =
            tracing::Instrument::instrument(self.download_in_span(fetch, cache, on), span).await;
        if !fetch.mirror
            && result
                .as_ref()
                .is_err_and(|error| error.kind == Kind::ReferenceUnresolved)
        {
            self.metadata.invalidate();
        }
        result
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
            fetch.remembered(cache)
        } else {
            None
        };
        if self.offline.enabled() {
            let Some(record) = known else {
                // a link fetched before has a record, so its bytes are the ones missing
                let pinned =
                    fetch.expect.sha256.is_some() || cache.url_record(fetch.url.as_str()).is_some();
                return Err(failure::offline(&fetch.url, self.offline, pinned));
            };
            return recalled(record, Origin::Cache, on);
        }
        // a link the source gives only for a while is asked for afresh by the stable one
        let relinked;
        let fetch = match (self.rules.relink)(&fetch.from) {
            Some(relink) => {
                relinked = Fetch {
                    from: self.relinked(&*relink, None).await?,
                    ..fetch.clone()
                };
                &relinked
            }
            None => fetch,
        };
        self.typed
            .check(&fetch.from)
            .map_err(|reason| failure::refused(&fetch.from, &reason))?;
        // a pinned S3 version may be gone, or kept from a reader without a key, while the object
        // there now is still the pinned file, and the pinned hash decides whether it is
        let versioned = fetch
            .expect
            .sha256
            .and_then(|_| super::s3::unversioned(&fetch.from));
        let Some((current, version)) = versioned else {
            return self.drive_download(fetch, cache, known, on).await;
        };
        let now = Fetch {
            url: fetch.url.clone(),
            from: current,
            divert: fetch.divert,
            expired: fetch.expired,
            expect: fetch.expect,
            revalidate: false,
            held: None,
            unpack: fetch.unpack,
            mirror: fetch.mirror,
            page: fetch.page,
            named: fetch.named,
            label: fetch.label.clone(),
        };
        let bucket = super::s3::Bucket::holding(&fetch.from);
        if bucket
            .as_ref()
            .is_some_and(|bucket| self.refuses_versions(bucket))
        {
            let differs = match self.claim_and_download(&now, cache, None, on).await {
                Err(error) if error.kind != Kind::Interrupted => error,
                done => return done,
            };
            tracing::debug!(%differs, "the object there now is not the pinned file, so its pinned version is asked for");
            return match self.claim_and_download(fetch, cache, known, on).await {
                Err(refused) if unreadable(&refused) => Err(unread(differs, refused, &version)),
                done => done,
            };
        }
        let refused = match self.claim_and_download(fetch, cache, known, on).await {
            Err(error) if unreadable(&error) => error,
            done => return done,
        };
        tracing::debug!(%refused, "the pinned version cannot be read, so the object there now is asked for");
        match self.claim_and_download(&now, cache, None, on).await {
            Ok(fetched) => {
                // a 404 says one version is gone, while 401 and 403 say the bucket keeps its old
                // versions from this reader
                if refused.kind == Kind::AuthRequired
                    && let Some(bucket) = bucket
                {
                    self.refuse_versions
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .insert(bucket);
                }
                Ok(fetched)
            }
            Err(error) => Err(unread(error, refused, &version)),
        }
    }

    /// whether `bucket` refused this run an old version whose object as it is now matched.
    fn refuses_versions(&self, bucket: &super::s3::Bucket) -> bool {
        self.refuse_versions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(bucket)
    }

    pub(in crate::net) async fn claim_and_download(
        &self,
        fetch: &Fetch,
        cache: &Cache,
        known: Option<UrlRecord>,
        on: Listener<'_>,
    ) -> Result<Fetched, Error> {
        let (partial, waited) = self.claim(&fetch.url, cache, on).await?;
        // another run can publish between the lookup and a claim that never reports contention
        if let Some(found) = self.in_cache(fetch, cache) {
            return Ok(found);
        }
        if waited && let Some(record) = fetch.revalidate.then(|| fetch.remembered(cache)).flatten()
        {
            return recalled(record, Origin::Cache, on);
        }
        if partial.lockless() {
            on(Event::Lockless)?;
        }
        let mut unfinished = Unfinished {
            partial: &partial,
            continuable: AtomicBool::new(true),
            done: false,
        };
        let continuable =
            |resumable: &Resumable| !matches!(resumable, Resumable::No | Resumable::Unproven(_));
        let heard = |event: Event| {
            if let Event::Kept { resumable, .. } = &event {
                let kept = continuable(resumable);
                unfinished.continuable.store(kept, Ordering::Relaxed);
            }
            let started = match &event {
                Event::Started { resumable, .. } => Some(continuable(resumable)),
                _ => None,
            };
            on(event)?;
            // a start refused at its question cut no byte, so the verdict before it still holds
            if let Some(started) = started {
                unfinished.continuable.store(started, Ordering::Relaxed);
            }
            Ok(())
        };
        let done = self.transfer(fetch, cache, &partial, known, &heard).await;
        unfinished.done = done.is_ok();
        done
    }

    /// tries one download until its bytes are in the cache and checked.
    #[expect(
        clippy::too_many_lines,
        reason = "every kind of answer a try gets is handled in one place"
    )]
    async fn transfer(
        &self,
        fetch: &Fetch,
        cache: &Cache,
        partial: &Partial,
        known: Option<UrlRecord>,
        on: Listener<'_>,
    ) -> Result<Fetched, Error> {
        let mut whole = kept_before(fetch, partial, on)?;
        let validators = known.as_ref().map(|record| record.validators.clone());
        let mut from = fetch.from.clone();
        let mut diversions = 0;
        let mut relinks = 0;
        let mut restarted = false;
        // a file stops being split once a server answered a range badly, or bytes from several
        // answers did not match
        let mut state = Transfer {
            split: true,
            permits: match whole {
                Some(_) => None,
                None => Some(self.permit(&fetch.from).await?),
            },
            sent_whole: false,
        };
        let began = Instant::now();
        let mut retry = Retry::downloading(self, fetch, on);
        let reach = Reach::default();
        let watched = |event: Event| {
            reach.hear(&event);
            on(event)
        };
        loop {
            let answer = if let Some(kept) = whole.take() {
                let told = Told {
                    name: partial.named(),
                    page: false,
                };
                self.finish_kept(fetch, partial, kept, told, &watched).await
            } else {
                self.keep_permit(&fetch.from, &mut state.permits).await?;
                self.attempt(
                    &from,
                    fetch,
                    partial,
                    validators.as_ref(),
                    &watched,
                    &mut state,
                )
                .await
            };
            retry.moved(reach.further());
            match answer {
                Ok(Answer::NotModified) => {
                    let Some(record) = known else {
                        return Err(Error::new(
                            Kind::NetworkStatus,
                            format!(
                                "{} answered 304 to a request that did not ask for it",
                                host(&from)
                            ),
                        ));
                    };
                    return recalled(record, Origin::NotModified, on);
                }
                Ok(Answer::Stored {
                    received: Received { stored, arrived },
                    validators,
                    told,
                    resumed,
                }) => match verify(&fetch.url, &fetch.expect, &stored) {
                    Ok(()) => {
                        let moved = reach.moved.load(Ordering::Relaxed);
                        let mut fetched =
                            self.remember(fetch, cache, &stored, validators, told, moved);
                        (fetched.began, fetched.arrived) = (Some(began), Some(arrived));
                        return Ok(fetched);
                    }
                    // bytes from two connections may not belong together, so a resumed or split
                    // download that does not match starts over once, as one stream
                    Err(err) if resumed && !restarted => {
                        tracing::debug!(%err, "a download in pieces did not match, starting over");
                        restarted = true;
                        state.split = false;
                        partial.discard();
                    }
                    Err(err) => return Err(err),
                },
                Ok(Answer::StartOver) => {
                    tracing::debug!("the earlier download cannot be continued, starting over");
                    partial.discard();
                }
                Ok(Answer::Divert(next)) => from = self.divert(fetch, next, &mut diversions)?,
                Ok(Answer::Relink(relink)) => {
                    // asking the source takes slots of its own, which these may be all of
                    Self::release_for_wait(&mut state.permits);
                    from = self.refreshed(&*relink, &from, &mut relinks).await?;
                }
                Err(Failure::Final(err)) => return Err(err),
                Err(Failure::Limited { reason, wait }) => {
                    Self::release_for_wait(&mut state.permits);
                    retry.limited(reason, wait, known.is_some()).await?;
                }
                Err(Failure::Transient {
                    reason,
                    retry_after,
                    unreachable,
                }) => {
                    Self::release_for_wait(&mut state.permits);
                    if unreachable {
                        retry.unreachable(reason, known.is_some()).await?;
                    } else {
                        retry.wait(reason, retry_after, known.is_some()).await?;
                    }
                }
            }
        }
    }

    /// the link a page pointed to, once it is checked, unless pages have pointed elsewhere too
    /// often already.
    fn divert(&self, fetch: &Fetch, next: Url, diversions: &mut u32) -> Result<Url, Error> {
        *diversions += 1;
        if *diversions > DIVERSIONS {
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
        tracing::debug!(next = %crate::shown::Shown(&next), "the page points elsewhere");
        Ok(next)
    }

    fn in_cache(&self, fetch: &Fetch, cache: &Cache) -> Option<Fetched> {
        let Some(fetched) = fetch.cached(cache) else {
            // false means the cache had nothing to look the file up by
            let stated = fetch.expect.proves();
            tracing::debug!(stated_hash = stated, "not in the cache");
            return None;
        };
        tracing::debug!(
            sha256 = fetched.digest.short(),
            size = fetched.size,
            "in the cache, no download"
        );
        Work::add(&self.work.downloads_skipped, 1);
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
                Ok(Claim::Busy) => {
                    let cache = cache.clone();
                    let waiting_for = url.as_str().to_owned();
                    let published = tokio::task::spawn_blocking(move || {
                        if cache.publish_download_of(&waiting_for)? {
                            return Ok(true);
                        }
                        cache.publish_waiting_downloads()
                    })
                    .await
                    .map_err(|err| local(&io::Error::other(err)))?
                    .map_err(|err| local(&err))?;
                    if published {
                        waited = true;
                        continue;
                    }
                    if !waited {
                        on(Event::Waiting)?;
                        waited = true;
                    }
                }
                Err(err) => return Err(local(&err)),
            }
            tokio::select! {
                () = tokio::time::sleep(POLL) => {}
                () = self.stop.asked() => return Err(failure::interrupted(url)),
            }
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "response checks and the writer handover stay in order"
    )]
    async fn attempt(
        &self,
        from: &Url,
        fetch: &Fetch,
        partial: &Partial,
        known: Option<&Validators>,
        on: Listener<'_>,
        state: &mut Transfer,
    ) -> Result<Answer, Failure> {
        let earlier = partial
            .resumable()
            .filter(|(_, validators)| fetch.expect.proves() || if_range(validators).is_some())
            .or_else(|| unrecorded(fetch, partial));
        let length = earlier.as_ref().and_then(|_| partial.length());
        // bytes kept that are all of the file ask for their last byte alone: an answer of the same
        // version says they still are, and names the file as a whole answer does
        let whole = earlier
            .as_ref()
            .is_some_and(|(received, _)| *received > 0 && length == Some(*received));
        let asked = earlier
            .as_ref()
            .map(|(received, validators)| (received - u64::from(whole), validators.clone()));
        // the bytes up to a range a split kept, and none of it, which the server would send on
        // until the answer is let go
        let until = asked
            .as_ref()
            .filter(|_| !whole)
            .and_then(|_| partial.next_kept());
        let shape = conditions(asked.as_ref(), until, known);
        let holding = super::Holding::new(&fetch.from, &mut state.permits);
        let response = self
            .send_body(from, fetch.mirror, shape, Some(holding))
            .await?;
        let answer_at = Instant::now();
        let host = self.paced(&fetch.from);
        if let Some(super::Answering(waited)) = response.extensions().get() {
            self.heard.answered(&host, *waited);
        }
        let status = response.status();
        tracing::debug!(url = %crate::shown::Shown(from), status = status.as_u16(), from_byte = asked.as_ref().map(|(start, _)| *start), "response");
        let head = response.headers().clone();
        let response = match self.instead(from, fetch, response).await? {
            Ok(response) => response,
            Err(Instead::Divert(next)) => return Ok(Answer::Divert(next)),
            Err(Instead::Relink(relink)) => return Ok(Answer::Relink(relink)),
        };
        let validators = answered(&head, status, earlier.as_ref());
        if status == StatusCode::NOT_MODIFIED && earlier.is_none() && known.is_some() {
            return Ok(Answer::NotModified);
        }
        if let Some((received, kept)) = &earlier
            && let Some(size) = confirmed(
                status,
                &head,
                (*received, kept),
                length,
                (&validators, fetch.expect.proves()),
            )
        {
            // a refusal's headers tell of the refusal, not of the file
            let describes = status != StatusCode::RANGE_NOT_SATISFIABLE;
            let named = if describes {
                describe_response(&response, &fetch.url, on)?
            } else {
                None
            };
            let told = Told {
                name: named.or_else(|| partial.named()),
                page: describes && headers::is_html(&head),
            };
            refuse_other_length(fetch, Some(size))?;
            // the bytes are all here, so the slots go to other files before the check
            drop(state.permits.take());
            return self
                .finish_kept(fetch, partial, (size, kept.clone()), told, on)
                .await;
        }
        if let Some((received, kept)) = &earlier
            && let Some(stated) = length.filter(|stated| stated > received)
            && status == StatusCode::RANGE_NOT_SATISFIABLE
            && headers::total_length(&head, StatusCode::PARTIAL_CONTENT) == Some(*received)
            && !other_version(kept, &validators)
        {
            // no answer this server gives can tell which length is the file's
            partial.discard();
            return Err(Failure::Final(two_lengths(&fetch.url, stated, *received)));
        }
        let Some(offset) =
            self.starting_offset(from, status, &head, asked.as_ref(), length, &validators)?
        else {
            state.sent_whole = status == StatusCode::PARTIAL_CONTENT
                && headers::range_start(&head) == Some(0)
                && earlier
                    .as_ref()
                    .is_some_and(|(_, kept)| !other_version(kept, &validators));
            return Ok(Answer::StartOver);
        };
        let total = headers::total_length(&head, status);
        let told = Told {
            name: describe_response(&response, &fetch.url, on)?,
            page: headers::is_html(&head),
        };
        refuse_other_length(fetch, total)?;
        let unproven = unproven(&fetch.expect, &validators);
        let proven = unproven.is_none();
        let splits = state.split && self.may_split(total, status, &head, proven);
        tracing::debug!(
            splits,
            total,
            status = status.as_u16(),
            proven,
            "split decision"
        );
        let body = Body {
            offset,
            total,
            validators: validators.clone(),
            md5: fetch.expect.needs_md5(),
            // ranges a split kept past the prefix stay whether or not this answer splits
            keep: proven
                && status == StatusCode::PARTIAL_CONTENT
                && earlier.is_some()
                && total.is_some(),
            unrecorded: fetch.expect.proves(),
            hashing: self.hashing.clone(),
        };
        let resumable = if std::mem::take(&mut state.sent_whole) {
            Resumable::No
        } else {
            Resumable::answered(
                status,
                &head,
                unproven,
                earlier.as_ref().map(|(_, kept)| kept),
            )
        };
        // a start from the first byte may ask a question first, which must find the bytes an
        // earlier run or try kept as they were: they are cut only once it goes on
        if offset == 0 {
            self.started(on, total, 0, resumable)?;
        }
        let Some(mut writer) = open(partial, body).await? else {
            on(Event::Spoiled).map_err(Failure::Final)?;
            return Ok(Answer::StartOver);
        };
        if let Some(name) = &told.name {
            writer.name(name);
        }
        let kept = writer.kept();
        if offset > 0 {
            self.started(on, total, offset + kept, resumable)?;
        }
        // ranges kept past the prefix leave gaps that only the split path fills, which adds no
        // stream to an answer that may not split, as does an answer that ends before the file
        let short = headers::range_end(&head)
            .is_some_and(|last| total.is_some_and(|total| last.saturating_add(1) < total));
        let gaps = kept > 0 || (short && status == StatusCode::PARTIAL_CONTENT);
        let at_once = self.split.when == Splitting::Always || gaps;
        let watch = total.filter(|_| splits || gaps);
        // a stream held back by FETCHLOOM_RATE looks slow, and more streams share the same cap
        let monitor = watch.filter(|_| self.rate.is_none());
        let capped = bounded(fetch, offset, on);
        let (writer, response, total, measured) = match watch {
            Some(total) if at_once => (writer, response, total, None),
            _ => match self
                .stream(from, writer, response, total, &capped, monitor)
                .await?
            {
                Run::Whole(mut writer) => {
                    self.refuse_pointer(fetch, &mut writer)?;
                    self.heard.received(
                        &host,
                        writer.written().saturating_sub(offset),
                        answer_at.elapsed(),
                    );
                    on(Event::Arrived).map_err(Failure::Final)?;
                    return Ok(Answer::Stored {
                        received: self.after_body(&mut state.permits, finish(writer)).await?,
                        validators,
                        told,
                        resumed: offset + kept > 0,
                    });
                }
                Run::Handover {
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
            pinned: fetch.expect.pins(),
            mirror: fetch.mirror,
            grow: splits,
            total,
            on,
            measured,
        };
        let (writer, pieced) = match self
            .split(job, writer, response, &mut state.split, &mut state.permits)
            .await?
        {
            split::Streamed::Whole { writer, pieced } => (*writer, pieced),
            split::Streamed::StartOver => return Ok(Answer::StartOver),
        };
        on(Event::Arrived).map_err(Failure::Final)?;
        Ok(Answer::Stored {
            received: self.after_body(&mut state.permits, finish(writer)).await?,
            validators,
            told,
            resumed: offset + kept > 0 || pieced,
        })
    }

    /// tells `on` that the answer's bytes are about to flow, `resumed` of them kept already, and
    /// counts a download that goes on from kept bytes.
    fn started(
        &self,
        on: Listener<'_>,
        total: Option<u64>,
        resumed: u64,
        resumable: Resumable,
    ) -> Result<(), Failure> {
        on(Event::Started {
            total,
            resumed,
            resumable,
        })
        .map_err(Failure::Final)?;
        if resumed > 0 {
            Work::add(&self.work.resumes, 1);
        }
        Ok(())
    }

    /// the answer for bytes an earlier run kept, `size` of them under `validators`, that are all
    /// of the file: opened as a resume opens them, which checks them against their record, and
    /// put in the cache with no byte more. a later run would do the same.
    async fn finish_kept(
        &self,
        fetch: &Fetch,
        partial: &Partial,
        (size, validators): (u64, Validators),
        told: Told,
        on: Listener<'_>,
    ) -> Result<Answer, Failure> {
        let body = Body {
            offset: size,
            total: Some(size),
            validators: validators.clone(),
            md5: fetch.expect.needs_md5(),
            keep: false,
            unrecorded: fetch.expect.proves(),
            hashing: self.hashing.clone(),
        };
        let Some(mut writer) = open(partial, body).await? else {
            on(Event::Spoiled).map_err(Failure::Final)?;
            return Ok(Answer::StartOver);
        };
        self.started(on, Some(size), size, Resumable::Yes)?;
        self.refuse_pointer(fetch, &mut writer)?;
        on(Event::Arrived).map_err(Failure::Final)?;
        Ok(Answer::Stored {
            received: finish(writer).await?,
            validators,
            told,
            resumed: true,
        })
    }

    /// refuses a file small enough to be a git-lfs pointer that is one, naming the link that
    /// serves the file it stands for.
    fn refuse_pointer(&self, fetch: &Fetch, writer: &mut PartialWriter) -> Result<(), Failure> {
        if writer.written() < LFS_LARGEST
            && is_lfs_pointer(
                &writer
                    .head(LFS_POINTER.len())
                    .map_err(|err| Failure::Final(local(&err)))?,
            )
        {
            return Err(Failure::Final(lfs_pointer(
                &fetch.url,
                (self.rules.lfs_link)(&fetch.url),
            )));
        }
        Ok(())
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

    /// the answer itself when it brings the file, else what it means: the URL a page's handler
    /// finds, a link the source gives for a while that expired, an expired link to try again, or a
    /// refusal that says whether an S3 key went with it.
    async fn instead(
        &self,
        from: &Url,
        fetch: &Fetch,
        response: reqwest::Response,
    ) -> Result<Result<reqwest::Response, Instead>, Failure> {
        let status = response.status();
        if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN)
            && let Some(keyed) = response.extensions().get::<super::s3::Keyed>()
        {
            return Err(Failure::Final(keyed.refusal(from)));
        }
        if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN)
            && let Some(sent) = response.extensions().get::<super::site::Sent>()
            && sent.answered()
        {
            return Err(Failure::Final(sent.refusal(
                response.url(),
                status,
                response.headers(),
            )));
        }
        if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN)
            && let Some(authorized) = response.extensions().get::<super::Authorized>()
        {
            return Err(Failure::Final(authorized.refusal(from)));
        }
        if matches!(
            status,
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN | StatusCode::NOT_FOUND
        ) && let Some(answered) = response.extensions().get::<super::DriveAnswered>().cloned()
        {
            let body = self.page(from, response).await?;
            return Err(answered.failure(status.as_u16(), body.as_bytes()));
        }
        if let Some(divert) = fetch.divert
            && is_page(status, response.headers())
        {
            let url = response.url().clone();
            let page = self.page(from, response).await?;
            return divert(&url, status.as_u16(), &page)
                .map(|next| Err(Instead::Divert(next)))
                .map_err(Failure::Final);
        }
        if let Some(error) =
            super::site::sign_in_page(&response, response.url(), &fetch.url, fetch.page)
        {
            return Err(Failure::Final(error));
        }
        if let Some(expired) = fetch.expired
            && matches!(status, StatusCode::FORBIDDEN | StatusCode::BAD_REQUEST)
            && headers::is_xml(response.headers())
        {
            let head = response.headers().clone();
            if self.expired_link(from, response, expired).await? {
                if let Some(relink) = (self.rules.relink)(&fetch.url) {
                    return Ok(Err(Instead::Relink(relink)));
                }
                return Err(failure::transient(format!(
                    "{} supplied an expired download link",
                    host(from)
                )));
            }
            return Err(self.status_refusal(from, status, &head));
        }
        if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN)
            && let Some(error) = self.refused(from, response.url(), status, response.headers())
        {
            return Err(Failure::Final(error));
        }
        // a source's link keeps its source's own refusal, whatever host refused it
        if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN)
            && response.url().origin() != from.origin()
            && (self.rules.refused_download)(from, status.as_u16()).is_none()
            && crate::account::Account::from_host(&host(from).to_ascii_lowercase()).is_none()
        {
            let carried = response.extensions().get::<super::site::Sent>().is_some();
            return Err(Failure::Final(failure::moved_refusal(
                from,
                response.url(),
                status,
                response.headers(),
                carried,
            )));
        }
        Ok(Ok(response))
    }

    /// batches a single body on a writer thread, measuring eligible downloads before handing
    /// their accepted offset and remaining response to the range scheduler.
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
                url = %crate::shown::Shown(url),
                rate,
                at = writer.written(),
                "one stream is slow, so the download splits"
            );
            return Ok(Run::Handover {
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
        response: &mut impl Chunks,
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
        response: &mut impl Chunks,
        sender: &tokio::sync::mpsc::Sender<Vec<Bytes>>,
        batch: &mut Batch,
        mut reached: u64,
        on: Listener<'_>,
        watch: Option<u64>,
    ) -> Result<Option<(u64, f64)>, Failure> {
        let window = self.split.window;
        let lap = tokio::time::sleep(window);
        tokio::pin!(lap);
        let (mut since, mut moved) = (Instant::now(), 0u64);
        // the idle deadline moves with every chunk without touching the timer, which is reset
        // only when it fires early
        let mut idle = tokio::time::Instant::now() + self.idle;
        let quiet = tokio::time::sleep_until(idle);
        tokio::pin!(quiet);
        let stopped = self.stop.asked();
        tokio::pin!(stopped);
        let (mut trickle, mut asked) = (Trickle::default(), None);
        loop {
            let waiting_since = *asked.get_or_insert_with(tokio::time::Instant::now);
            let next = tokio::select! {
                next = response.chunk() => next,
                () = &mut quiet => {
                    if tokio::time::Instant::now() < idle {
                        quiet.as_mut().reset(idle);
                        continue;
                    }
                    // a process stopped past its deadline, as a sleeping laptop's is, finds the
                    // deadline and the server's waiting bytes ready together
                    match tokio::time::timeout(Duration::ZERO, response.chunk()).await {
                        Ok(next) => next,
                        Err(_) => break Err(failure::transient(format!(
                            "{} sent nothing for {} s",
                            host(url),
                            self.idle.as_secs()
                        ))),
                    }
                }
                () = &mut lap, if watch.is_some() => {
                    let now = Instant::now();
                    let seconds = now.duration_since(since).as_secs_f64().max(f64::EPSILON);
                    #[expect(clippy::cast_precision_loss, reason = "a rate needs no exact count")]
                    let rate = moved as f64 / seconds;
                    if let Some(total) = watch
                        && self.split.hand_over(rate, total.saturating_sub(reached))
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
                Err(err) => break Err(failure::broke(url, "the file", &err)),
                Ok(None) => break Ok(None),
                Ok(Some(chunk)) => {
                    asked = None;
                    let length = chunk.len() as u64;
                    // judged before the rate cap waits, and failed only once its bytes are kept,
                    // as every byte that arrived is
                    let stalled = trickle
                        .heard(length, waiting_since.elapsed(), self.idle)
                        .err();
                    if let Err(err) = self.throttle(url, chunk.len()).await {
                        break Err(err);
                    }
                    Work::add(&self.work.bytes_down, length);
                    (reached, moved) = (reached + length, moved + length);
                    if let Err(err) = on(Event::Bytes(length)) {
                        break Err(Failure::Final(err));
                    }
                    batch.push(chunk);
                    if let Some(moved) = stalled {
                        break Err(trickled(url, moved, self.idle));
                    }
                    if batch.full() {
                        // a writer with every slot taken is the disk falling behind, which the
                        // bar tells apart from a server that sends nothing
                        let behind = sender.capacity() == 0;
                        if behind {
                            let _ = on(Event::Behind(true));
                        }
                        let flushed = batch.flush(sender).await;
                        if behind {
                            let _ = on(Event::Behind(false));
                        }
                        // a closed channel means the writer failed, and its own error says why
                        if flushed.is_err() {
                            break Ok(None);
                        }
                    }
                    // waiting on this machine, for the rate cap, the writer or an answer to a
                    // question, is no silence from the server
                    idle = tokio::time::Instant::now() + self.idle;
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
        match self.page(url, response).await {
            Ok(text) => Ok(expired(&text)),
            Err(Failure::Final(error)) if error.kind == Kind::Interrupted => {
                Err(Failure::Final(error))
            }
            // without a readable expiry message, the original 403 remains an access refusal
            Err(_) => Ok(false),
        }
    }

    /// reads a page a server sent instead of the file, stopping when ctrl-c is pressed.
    async fn page(&self, url: &Url, mut response: reqwest::Response) -> Result<String, Failure> {
        let page = self
            .read_body(
                url,
                &mut response,
                "the page",
                LARGEST_PAGE,
                || format!("{} sent a page where the file was expected", host(url)),
                |bytes| async move {
                    self.throttle(url, bytes).await?;
                    Work::add(&self.work.bytes_down, bytes as u64);
                    Ok(())
                },
            )
            .await?;
        Ok(String::from_utf8_lossy(&page).into_owned())
    }

    /// counts a finished download, and records what it proved for later runs.
    fn remember(
        &self,
        fetch: &Fetch,
        cache: &Cache,
        stored: &Stored,
        validators: Validators,
        told: Told,
        moved: u64,
    ) -> Fetched {
        if stored.new {
            self.work.wrote(&stored.digest);
        }
        // these records only spare later downloads, so failing to write one costs nothing more.
        // only an MD5 these bytes were hashed to may stand for them, and only for this link
        let version = validators.version.clone();
        let stated = crate::cache::Stated {
            digest: stored.digest,
            size: stored.size,
            version: version.clone(),
        };
        if let Some(md5) = stored.md5.filter(|got| fetch.expect.md5 == Some(*got))
            && let Err(err) = cache.save_stated_md5(&md5, fetch.url.as_str(), &stated)
        {
            tracing::debug!(%err, "could not record the stated md5");
        }
        if fetch.revalidate {
            let record = UrlRecord {
                url: fetch.url.as_str().to_owned(),
                sha256: stored.digest,
                size: stored.size,
                name: told.name.clone(),
                validators,
                page: told.page,
            };
            if let Err(err) = cache.save_url_record(&record) {
                tracing::debug!(%err, "could not record what the URL served");
            }
        }
        Fetched {
            digest: stored.digest,
            size: stored.size,
            moved,
            name: told.name,
            origin: Origin::Network,
            version,
            began: None,
            arrived: None,
            asked: Duration::ZERO,
        }
    }
}

fn describe_response(
    response: &reqwest::Response,
    url: &Url,
    on: Listener<'_>,
) -> Result<Option<String>, Failure> {
    let name = served_name(response, url).map_err(Failure::Final)?;
    if headers::is_html(response.headers()) {
        on(Event::WebPage { name: name.clone() }).map_err(Failure::Final)?;
    }
    if let Some(given) = &name {
        on(Event::Named(given.clone())).map_err(Failure::Final)?;
    }
    if let Some(encoding) = headers::encoding(response.headers()) {
        on(Event::Encoded(encoding)).map_err(Failure::Final)?;
    }
    Ok(name)
}

/// the name the server gives the file it answers the link `url` with: the one its answer states,
/// else, where the link names no file, the one a redirect gave. a name holding a bidi control
/// character is refused.
fn served_name(response: &reqwest::Response, url: &Url) -> Result<Option<String>, Error> {
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
        return Err(Error::new(
            Kind::SourceInvalid,
            format!(
                "the server supplied a file name with a bidi control character: \"{}\"",
                given.escape_debug()
            ),
        )
        .help("ask the file's publisher to remove the control character from its name"));
    }
    Ok(name)
}

/// how an answer identified its bytes. a range continuing kept bytes is of the version its
/// `If-Range` named, so a validator it leaves off is the one the kept bytes came with: RFC 9110
/// lets a 206 leave `Last-Modified` off, and some servers leave every one off. a date goes with
/// the `Last-Modified` it dates.
fn answered(
    head: &header::HeaderMap,
    status: StatusCode,
    earlier: Option<&(u64, Validators)>,
) -> Validators {
    let mut validators = headers::validators(head);
    if status == StatusCode::PARTIAL_CONTENT
        && let Some((_, kept)) = earlier
    {
        if validators.etag.is_none() {
            validators.etag.clone_from(&kept.etag);
        }
        if validators.last_modified.is_none() {
            validators.last_modified.clone_from(&kept.last_modified);
            validators.date.clone_from(&kept.date);
        }
        if validators.version.is_none() {
            validators.version.clone_from(&kept.version);
        }
    }
    validators
}

/// the size of the file when an answer says the bytes kept, `received` of them under validators
/// `kept`, still are all of it: the last of them sent alone, as asked for when the record's stated
/// `length` says they are all of it, a refusal of the range past bytes of no stated length that
/// states their size, either of the same version, or the whole file as long as the bytes kept from
/// a server that ignored the range: of the version `If-Range` named, or under hashes that check
/// the bytes once they are in, as `hashed` says.
fn confirmed(
    status: StatusCode,
    head: &header::HeaderMap,
    (received, kept): (u64, &Validators),
    length: Option<u64>,
    (validators, hashed): (&Validators, bool),
) -> Option<u64> {
    if status == StatusCode::OK {
        let total =
            headers::total_length(head, status).filter(|total| *total == received && *total > 0)?;
        let named = if_range(validators).is_some_and(|named| if_range(kept) == Some(named))
            && !headers::changed(kept, validators);
        return ((hashed || named) && length.is_none_or(|length| length == total)).then_some(total);
    }
    // a refused range states the size as a part does. a part of a length its server does not
    // know is of the kept length when it names the kept version itself: one version, one length
    let total = match headers::total_length(head, StatusCode::PARTIAL_CONTENT) {
        Some(total) => total,
        None if status == StatusCode::PARTIAL_CONTENT
            && headers::length_unknown(head)
            && if_range(&headers::validators(head))
                .is_some_and(|named| if_range(kept) == Some(named)) =>
        {
            received
        }
        None => return None,
    };
    let total = Some(total).filter(|total| *total == received && *total > 0)?;
    let alone = match (status, length) {
        (StatusCode::PARTIAL_CONTENT, Some(length)) => {
            length == total && headers::range_start(head) == Some(total - 1)
        }
        (StatusCode::RANGE_NOT_SATISFIABLE, None) => true,
        _ => false,
    };
    (alone && !other_version(kept, validators)).then_some(total)
}

impl Net {
    /// accepts a full body or a range that continues the same representation at the saved offset,
    /// of a file `length` long when the kept bytes' answer stated that.
    fn starting_offset(
        &self,
        from: &Url,
        status: StatusCode,
        head: &header::HeaderMap,
        earlier: Option<&(u64, Validators)>,
        length: Option<u64>,
        validators: &Validators,
    ) -> Result<Option<u64>, Failure> {
        match status {
            StatusCode::OK => Ok(Some(0)),
            // a server that ignores If-Range sends the rest of whatever version it holds now, and
            // one that names no version on its part can still show another by its length
            StatusCode::PARTIAL_CONTENT => match (earlier, headers::range_start(head)) {
                (Some((received, recorded)), Some(start))
                    if start == *received
                        && !other_version(recorded, validators)
                        && length.is_none_or(|length| {
                            headers::total_length(head, status) == Some(length)
                        }) =>
                {
                    Ok(Some(*received))
                }
                (Some(_), _) => Ok(None),
                // a range nobody asked for is the file only when it runs from the first byte to the
                // stated last; anything else would be asked for again and again, or kept short
                (None, Some(0))
                    if headers::total_length(head, status)
                        .zip(headers::range_end(head))
                        .is_some_and(|(total, end)| end.checked_add(1) == Some(total)) =>
                {
                    Ok(Some(0))
                }
                (None, _) => Err(self.status_refusal(from, status, head)),
            },
            StatusCode::RANGE_NOT_SATISFIABLE if earlier.is_some() => Ok(None),
            status => Err(self.status_refusal(from, status, head)),
        }
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
    let shown = crate::shown::Shown(url);
    if let Some(size) = expect.size
        && size != stored.size
    {
        return Err(wrong_size(url, size, Some(stored.size)));
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
            expected.hex(),
            stored.digest.hex()
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

/// how a git-lfs pointer starts: the few lines a repository keeps in place of a file git-lfs
/// stores apart, which a raw link to that file serves.
const LFS_POINTER: &[u8] = b"version https://git-lfs.github.com/spec/v1";
/// files at least this large are never taken for a pointer, which keeps a few hundred bytes, so
/// the files people mean to download are never read for one.
const LFS_LARGEST: u64 = 1024;

/// whether a small file whose first bytes are `head` is a git-lfs pointer.
fn is_lfs_pointer(head: &[u8]) -> bool {
    head.starts_with(LFS_POINTER)
}

/// the error for `url`, which answered with a git-lfs pointer rather than the file, with `link`,
/// where its site serves the file itself, when its source knows.
fn lfs_pointer(url: &Url, link: Option<String>) -> Error {
    let error = Error::new(
        Kind::SourceInvalid,
        format!(
            "{} sent a git-lfs pointer, not the file it stands for",
            crate::shown::Shown(url)
        ),
    );
    match link {
        Some(link) => error.help(format!(
            "git-lfs keeps that file apart from the repository: use {link} instead"
        )),
        None => error.help(
            "git-lfs keeps that file apart from the repository: use the link its site gives to download the file itself",
        ),
    }
}

/// a pinned size bounds the download, so an answer stating another length is refused unread.
fn refuse_other_length(fetch: &Fetch, total: Option<u64>) -> Result<(), Failure> {
    match (fetch.expect.size, total) {
        (Some(size), Some(total)) if size != total => {
            Err(Failure::Final(wrong_size(&fetch.url, size, Some(total))))
        }
        _ => Ok(()),
    }
}

/// `on` for a body starting at `offset`, refusing bytes past the pinned size, which bounds a body
/// whose answer states no length.
fn bounded<'a>(
    fetch: &'a Fetch,
    offset: u64,
    on: Listener<'a>,
) -> impl Fn(Event) -> Result<(), Error> + Sync + 'a {
    let taken = AtomicU64::new(offset);
    move |event| {
        if let (Event::Bytes(bytes), Some(size)) = (&event, fetch.expect.size)
            && taken.fetch_add(*bytes, Ordering::Relaxed) + bytes > size
        {
            return Err(wrong_size(&fetch.url, size, None));
        }
        on(event)
    }
}

/// the error for `got` bytes, or for more than `expected` when the count is not known, where
/// `expected` were pinned.
fn wrong_size(url: &Url, expected: u64, got: Option<u64>) -> Error {
    let (sent, detail) = match got {
        Some(got) => (
            units::size(got),
            format!("expected {expected} bytes, got {got}"),
        ),
        None => (
            format!("more than {}", units::size(expected)),
            format!("expected {expected} bytes, got more"),
        ),
    };
    Error::new(
        Kind::IntegrityMismatch,
        format!(
            "{} sent {sent} for {}, where {} were expected",
            host(url),
            crate::shown::Shown(url),
            units::size(expected)
        ),
    )
    .detail(detail)
}

/// the error for a server that said the file at `url` is `stated` bytes long, sent `received` of
/// them, then said those are all of it.
fn two_lengths(url: &Url, stated: u64, received: u64) -> Error {
    Error::new(
        Kind::IntegrityMismatch,
        format!(
            "{} said the file is {} and sent {}, then said that is all of {}",
            host(url),
            units::size(stated),
            units::size(received),
            crate::shown::Shown(url)
        ),
    )
    .detail(format!(
        "stated {stated} bytes, sent {received}, then Content-Range: bytes */{received}"
    ))
    .help("the server states two lengths for the file, so neither can be trusted: ask its owner to fix the length it states, or download the file from another link")
}

/// whether a pinned version could not be read: refused, 401 or 403, or gone, 404 or 410.
fn unreadable(error: &Error) -> bool {
    matches!(error.kind, Kind::AuthRequired | Kind::ReferenceUnresolved)
}

/// the error for a pinned `version` its bucket `refused`, when the object there now failed with
/// `error`: other bytes say so beside the refusal, and any other failure leaves the refusal.
fn unread(error: Error, refused: Error, version: &str) -> Error {
    if error.kind == Kind::IntegrityMismatch {
        error.detail(format!(
            "its pinned version {version} could not be read: {}",
            refused.message
        ))
    } else {
        refused
    }
}

/// whether the cache holds these bytes: as an object, or, for an archive the caller unpacks, as
/// every file it unpacked to.
fn holds(cache: &Cache, fetch: &Fetch, digest: &Sha256, size: Option<u64>) -> bool {
    cache.find(digest, size).is_some() || fetch.unpack && cache.holds_unpacked(digest, None)
}

/// the bytes `record` says the URL served, which `origin` brings back without a new answer.
/// bytes that came as a web page are told as one again, so the rule that took or refused them
/// then decides now.
fn recalled(record: UrlRecord, origin: Origin, on: Listener<'_>) -> Result<Fetched, Error> {
    if record.page {
        on(Event::WebPage {
            name: record.name.clone(),
        })?;
    }
    Ok(fetched(record, origin))
}

fn fetched(record: UrlRecord, origin: Origin) -> Fetched {
    Fetched {
        digest: record.sha256,
        size: record.size,
        moved: 0,
        name: record.name,
        origin,
        version: record.validators.version,
        began: None,
        arrived: None,
        asked: Duration::ZERO,
    }
}

fn local(err: &io::Error) -> Error {
    crate::cache::unwritten(err)
}

/// tells `on` what an earlier run kept, which counts as kept from the start, before any server
/// answers. those bytes come back when they are all of a file a pin checks, which then needs no
/// request, nor a slot to send one.
fn kept_before(
    fetch: &Fetch,
    partial: &Partial,
    on: Listener<'_>,
) -> Result<Option<(u64, Validators)>, Error> {
    let Some((received, validators)) = partial.resumable().or_else(|| unrecorded(fetch, partial))
    else {
        return Ok(None);
    };
    on(Event::Kept {
        bytes: received + partial.kept_past(),
        resumable: Resumable::kept(unproven(&fetch.expect, &validators)),
    })?;
    let stated = partial.length().or(fetch.expect.size);
    let whole = fetch.expect.pins() && received > 0 && stated == Some(received);
    Ok(whole.then_some((received, validators)))
}

/// the bytes of a part no record names, as kept bytes no server named, for a download whose stated
/// hash checks them once the file is in: nothing else can.
fn unrecorded(fetch: &Fetch, partial: &Partial) -> Option<(u64, Validators)> {
    if !fetch.expect.proves() {
        return None;
    }
    partial
        .unrecorded()
        .map(|bytes| (bytes, Validators::default()))
}

/// whether an answer states another version than kept bytes came with. bytes kept with no `ETag`
/// or `Last-Modified`, which only a stated hash proves, name no version for one to differ from.
fn other_version(kept: &Validators, stated: &Validators) -> bool {
    (kept.etag.is_some() || kept.last_modified.is_some()) && headers::changed(kept, stated)
}

/// the headers that ask for the rest of an earlier download, up to the byte `until` when bytes
/// past it are kept too, which `If-Range` keeps to the version it came from, or else for the file
/// only when it changed since `known`.
fn conditions<'a>(
    earlier: Option<&'a (u64, Validators)>,
    until: Option<u64>,
    known: Option<&'a Validators>,
) -> impl Fn(reqwest::RequestBuilder) -> reqwest::RequestBuilder + 'a {
    move |mut request| {
        if let Some((received, validators)) = earlier {
            let last = until.map_or(String::new(), |until| (until - 1).to_string());
            request = request.header(header::RANGE, format!("bytes={received}-{last}"));
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

/// opens the writing end of a download on a blocking thread, since continuing one hashes the kept
/// bytes again. none when those are not the bytes recorded, so the download starts over.
async fn open(partial: &Partial, body: Body) -> Result<Option<PartialWriter>, Failure> {
    let Body {
        offset,
        total,
        validators,
        md5,
        keep,
        unrecorded,
        hashing,
    } = body;
    let partial = partial.clone();
    let opened = tokio::task::spawn_blocking(move || {
        partial.writer_with(offset, total, validators, md5, (keep, unrecorded), &hashing)
    })
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

/// a file whose every byte arrived, put into the cache.
struct Received {
    stored: Stored,
    /// when the last byte arrived, before the flush, which a slow disk can make long.
    arrived: Instant,
}

/// puts the finished file into the cache on a blocking thread, since that flushes it. awaited as
/// soon as the body ends, it takes that moment as when the last byte arrived.
async fn finish(writer: PartialWriter) -> Result<Received, Failure> {
    let arrived = Instant::now();
    let stored = tokio::task::spawn_blocking(move || {
        // stands in for a flush a slow disk makes long
        #[cfg(debug_assertions)]
        crate::stop::hold("FETCHLOOM_TEST_HOLD_FLUSH");
        writer.finish()
    })
    .await
    .map_err(|err| Failure::Final(local(&io::Error::other(err))))?
    .map_err(|err| Failure::Final(local(&err)))?;
    Ok(Received { stored, arrived })
}

#[cfg(test)]
mod tests;
