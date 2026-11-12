//! many downloads at once: a bar for each running one and one for them all, or plain lines now
//! and then off a terminal, and a stop at the first failure that keeps what already arrived for
//! the next run.

mod again;
pub(super) mod data_folder;
pub(super) mod mirrors;

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use tokio::task::JoinSet;
use url::Url;

pub(super) use self::again::Again;
use super::Context;
use crate::archive::{self, Packed};
use crate::cache::Cache;
use crate::digest::Sha256;
use crate::error::{Error, FileFailure, Kind};
use crate::net::{Event, Expect, Fetch, Fetched, Net, Resumable};
use crate::output::{Bar, Output, Ticker};
use crate::source::{self, Remote};
use crate::units;

/// a file to download, the name the person watching knows it by, and the dataset it belongs to
/// when it is part of a project.
pub(super) struct Job {
    pub(super) fetch: Fetch,
    pub(super) label: String,
    pub(super) dataset: Option<String>,
    /// where to ask again when the file is pinned and its recorded link is dead.
    pub(super) again: Option<Again>,
}

/// the batch facts retained while a validated job waits for an admission slot.
pub(super) struct Description {
    pub(super) label: String,
    pub(super) size: Option<u64>,
    host: String,
}

impl Description {
    /// drops the fetch and recovery copies, keeping one parsed URL for infallible reconstruction.
    pub(super) fn take(job: Job) -> (Self, Url) {
        let host = crate::net::host(&job.fetch.from).to_ascii_lowercase();
        (
            Self {
                label: job.label,
                size: job.fetch.expect.size,
                host,
            },
            job.fetch.url,
        )
    }
}

/// builds a listed download, revalidating unpinned links on later runs. cached members can stand
/// for an archive when `name` identifies one and extraction is enabled.
pub(super) fn listed_job(remote: &Remote, name: &str, extract: bool) -> Job {
    Job {
        fetch: Fetch {
            expect: Expect {
                sha256: remote.sha256,
                md5: remote.md5,
                size: remote.expected_size(),
            },
            revalidate: remote.sha256.is_none() && remote.md5.is_none(),
            unpack: extract && archive::by_name(name) == Some(Packed::Archive),
            ..source::fetch_plan(&remote.url)
        },
        label: remote.path.clone(),
        dataset: None,
        again: None,
    }
}

/// the path a listed file takes once its bytes are known: `path` with `.zip` added when its
/// source sent it as a zip of that one file, so it unpacks as any archive does and data.lock
/// names what its bytes are, else `path` as it is.
pub(super) fn wrapped(cache: &Cache, remote: &Remote, path: String, bytes: &Sha256) -> String {
    let wraps = source::may_wrap(&remote.url)
        && archive::by_name(&path).is_none()
        && archive::wraps(cache, bytes, &path);
    if wraps { format!("{path}.zip") } else { path }
}

/// how all the downloads of one call go together: a bar for them all on a terminal when there
/// are several, and plain lines now and then where no bar shows, once a download starts.
struct Overall {
    bar: Option<Bar>,
    /// made when the first download starts, so a batch another fetchloom filled while this one
    /// waited prints no line about downloading.
    ticker: OnceLock<Ticker>,
    what: String,
    /// the sizes the batch knows before it starts, added up, and how many files it does not.
    sizes: (u64, usize),
    /// the sizes known so far, added up, and how many files have yet to tell theirs: the bar
    /// fills to that sum only once none is left, so it promises no time left before. downloads
    /// run on several threads and learn sizes at once, so each addition reads and writes it under
    /// one lock.
    length: std::sync::Mutex<(u64, usize)>,
}

impl Overall {
    fn new(context: &Context, jobs: &[Job]) -> Overall {
        let what = match jobs {
            [only] => only.label.clone(),
            _ => units::count(jobs.len() as u64, "file", "files"),
        };
        Self::from_sizes(context, jobs.len(), what, || {
            jobs.iter().map(|job| job.fetch.expect.size)
        })
    }

    fn described(context: &Context, descriptions: &[Description]) -> Overall {
        let what = match descriptions {
            [only] => only.label.clone(),
            _ => units::count(descriptions.len() as u64, "file", "files"),
        };
        Self::from_sizes(context, descriptions.len(), what, || {
            descriptions.iter().map(|job| job.size)
        })
    }

    fn from_sizes<I: Iterator<Item = Option<u64>>>(
        context: &Context,
        count: usize,
        what: String,
        sizes: impl Fn() -> I,
    ) -> Overall {
        let bar = (count > 1).then(|| {
            let bar = context
                .output
                .bar("Downloading", units::count(count as u64, "file", "files"));
            let (total, unknown) = sized(sizes());
            bar.start((unknown == 0).then_some(total), 0);
            bar
        });
        let sizes = sized(sizes());
        let length = std::sync::Mutex::new(sizes);
        Overall {
            bar,
            ticker: OnceLock::new(),
            what,
            sizes,
            length,
        }
    }

    /// starts the plain lines, when the first download of the batch starts.
    fn begin(&self, output: &Output) {
        let (total, unknown) = self.sizes;
        self.ticker.get_or_init(|| {
            let sizes = std::iter::once(Some(total)).chain(std::iter::repeat_n(None, unknown));
            output.ticker("Downloading", self.what.clone(), sizes)
        });
    }

    fn advance(&self, bytes: u64) {
        if let Some(bar) = &self.bar {
            bar.advance(bytes);
        }
        if let Some(ticker) = self.ticker.get() {
            ticker.advance(bytes);
        }
    }

    fn kept(&self, bytes: u64) {
        if let Some(bar) = &self.bar {
            bar.kept(bytes);
        }
        if let Some(ticker) = self.ticker.get() {
            ticker.advance(bytes);
        }
    }

    fn sized(&self, bytes: u64) {
        if let Some(bar) = &self.bar {
            let mut length = self
                .length
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let (total, unknown) = &mut *length;
            *total = total.saturating_add(bytes);
            *unknown = unknown.saturating_sub(1);
            if *unknown == 0 {
                bar.length(*total);
            }
        }
        if let Some(ticker) = self.ticker.get() {
            ticker.sized(bytes);
        }
    }
}

/// the bytes a download whose server told `total` needs room for on the disk holding the cache:
/// what is left to transfer from `resumed`, and when it `unpacks` an archive whose members nobody
/// listed, the whole archive once more, the least its files can take. a lower bound: an archive
/// may unpack to much more.
fn room_needed(total: Option<u64>, resumed: u64, unpacks: bool) -> u64 {
    total.map_or(0, |total| {
        let left = total.saturating_sub(resumed);
        if unpacks {
            left.saturating_add(total)
        } else {
            left
        }
    })
}

/// `sizes` added up, and how many of them are not known.
fn sized(sizes: impl Iterator<Item = Option<u64>>) -> (u64, usize) {
    sizes.fold((0, 0), |(total, unknown), size| match size {
        Some(size) => (u64::saturating_add(total, size), unknown),
        None => (total, unknown + 1),
    })
}

/// what the bar for a batch of `count` files says once `done` of them finished, while others
/// may still be under way.
fn batch(done: usize, count: usize) -> String {
    format!(
        "{} · {done} done",
        units::count(count as u64, "file", "files")
    )
}

/// the one answer a run's downloads share once a large download asks, the size above which a
/// download asks, and the cache folder whose disk a download needs room on.
struct Approval {
    answer: OnceLock<Result<(), Error>>,
    above: u64,
    cache: Option<std::path::PathBuf>,
}

impl Approval {
    fn new(above: u64) -> Approval {
        Approval {
            answer: OnceLock::new(),
            above,
            cache: None,
        }
    }

    /// also asks before a download the disk holding `cache` has no room for.
    fn with_room(mut self, cache: &std::path::Path) -> Approval {
        self.cache = Some(cache.to_path_buf());
        self
    }
}

/// one download as it runs: its bar, its part of the overall progress, and the question before
/// a large file whose size only its server tells.
struct Watch<'a> {
    job: &'a Job,
    output: &'a Output,
    overall: &'a Overall,
    /// another file must not proceed while the first file is still waiting for an answer.
    approval: &'a Approval,
    yes: bool,
    bar: OnceLock<Bar>,
    /// whether the size the server told was added to the overall total.
    sized: AtomicBool,
    /// whether neither the source nor the server told the size, so only the bytes received tell
    /// when the download gets large.
    unbounded: AtomicBool,
    received: AtomicU64,
    /// for a download nobody stated the size of, what the disk holding the cache had free when
    /// it started, or `u64::MAX` when nothing is to be watched, and what `received` was then.
    /// bytes that arrive past that free space stop it, as a known size stops before it starts.
    free: AtomicU64,
    free_from: AtomicU64,
    /// what this download holds for the next run. the batch owns it, to say what a stop kept.
    held: &'a Held,
    /// the bytes an earlier run kept, so a start from the first byte can say they were lost.
    earlier: AtomicU64,
    /// whether the download said it starts over, which it says once.
    restarted: AtomicBool,
    /// the furthest point the overall progress counted, so bytes fetched again after a retry or
    /// a fresh start count once.
    counted: AtomicU64,
    /// plain lines while another fetchloom downloads the file, where no bar says so.
    waiting: std::sync::Mutex<Option<Ticker>>,
    /// whether the warning about bytes the server compressed on their way was told.
    encoded: AtomicBool,
}

impl<'a> Watch<'a> {
    fn new(
        job: &'a Job,
        output: &'a Output,
        overall: &'a Overall,
        approval: &'a Approval,
        yes: bool,
        held: &'a Held,
    ) -> Watch<'a> {
        Watch {
            job,
            output,
            overall,
            approval,
            yes,
            bar: OnceLock::new(),
            sized: AtomicBool::new(false),
            unbounded: AtomicBool::new(false),
            received: AtomicU64::new(0),
            free: AtomicU64::new(u64::MAX),
            free_from: AtomicU64::new(0),
            held,
            earlier: AtomicU64::new(0),
            restarted: AtomicBool::new(false),
            counted: AtomicU64::new(0),
            waiting: std::sync::Mutex::new(None),
            encoded: AtomicBool::new(false),
        }
    }

    /// the download's bar, made when it is first needed.
    fn bar(&self) -> &Bar {
        self.bar
            .get_or_init(|| self.output.bar("Downloading", self.job.label.clone()))
    }

    /// the run's one answer, asking `question` when no download asked before.
    fn approve(&self, question: impl FnOnce() -> Result<(), Error>) -> Result<(), Error> {
        if let Some(answer) = self.approval.answer.get() {
            return answer.clone();
        }
        let decide = || self.approval.answer.get_or_init(question).clone();
        // a terminal question must not occupy a network worker while it waits for an answer.
        if tokio::runtime::Handle::try_current().is_ok_and(|handle| {
            handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread
        }) {
            tokio::task::block_in_place(decide)
        } else {
            decide()
        }
    }

    /// says once that the download starts over when it goes on from `resumed`, before the bytes
    /// an earlier run kept, which are then lost.
    fn restarting(&self, resumed: u64) {
        let earlier = self.earlier.load(Ordering::Relaxed);
        if resumed < earlier && !self.restarted.swap(true, Ordering::Relaxed) {
            self.output.mark(
                "Restarting",
                &format!(
                    "{} · the {} an earlier run kept cannot be continued",
                    self.job.label,
                    units::size(earlier)
                ),
            );
        }
    }

    /// stops a download nobody stated the size of once `received`, the bytes it holds, passes
    /// what the disk holding the cache had free when it started: the bytes that just arrived are
    /// not written, so the disk is left near full rather than full. a person may go on anyway.
    fn room(&self, received: u64) -> Result<(), Error> {
        let free = self.free.load(Ordering::Relaxed);
        let arrived = received.saturating_sub(self.free_from.load(Ordering::Relaxed));
        let Some(cache) = self.approval.cache.as_deref() else {
            return Ok(());
        };
        if free == u64::MAX || crate::space::short(arrived, Some(free)).is_none() {
            return Ok(());
        }
        let doing = format!("downloading {}", self.job.label);
        self.approve(|| super::ask_room(self.output, &doing, arrived, free, cache))?;
        self.free.store(u64::MAX, Ordering::Relaxed);
        Ok(())
    }

    fn waited(&self) -> std::sync::MutexGuard<'_, Option<Ticker>> {
        self.waiting
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// tells the batch the size of a download that ended without its size ever told, as one
    /// sent in chunks does, from the bytes it received, so the batch can learn its total.
    fn ended(&self) {
        if self.job.fetch.expect.size.is_none() && !self.sized.swap(true, Ordering::Relaxed) {
            self.overall.sized(self.received.load(Ordering::Relaxed));
        }
    }

    /// moves the overall progress up to `position`, never back and never twice over a byte.
    fn count(&self, position: u64) {
        let before = self.counted.fetch_max(position, Ordering::Relaxed);
        if position > before {
            self.overall.advance(position - before);
        }
    }

    /// moves the overall progress up to `position` with bytes kept from before, which count
    /// toward how far the batch got but not toward its speed.
    fn count_kept(&self, position: u64) {
        let before = self.counted.fetch_max(position, Ordering::Relaxed);
        if position > before {
            self.overall.kept(position - before);
        }
    }

    /// a download that began at `resumed` of `total`: checks the disk has room for it, asks
    /// first where its size calls for it, then shows its bar from there.
    fn started(&self, total: Option<u64>, resumed: u64, resumable: Resumable) -> Result<(), Error> {
        self.waited().take();
        // a size only the server states is checked here, before any byte is written
        if let (Some(cache), Some(total)) = (self.approval.cache.as_deref(), total) {
            crate::space::cache_holds(cache, [(self.job.label.as_str(), total)])?;
        }
        self.overall.begin(self.output);
        let left = total.map_or(0, |total| total.saturating_sub(resumed));
        let unknown = self.job.fetch.expect.size.is_none();
        let free = self.approval.cache.as_deref().map(crate::space::free);
        // a download that may have found its bytes unchanged was not asked about before
        if unknown || self.job.fetch.held.is_some() {
            let needed = room_needed(total, resumed, unknown && self.job.fetch.unpack);
            // a disk without room asks instead, and its question names the size too
            if let (Some(cache), Some(free)) = (self.approval.cache.as_deref(), free)
                && !self.yes
                && crate::space::short(needed, free).is_some()
            {
                let doing = if needed > left {
                    format!("downloading and unpacking {}", self.job.label)
                } else {
                    format!("downloading {}", self.job.label)
                };
                let free = free.unwrap_or_default();
                self.approve(|| super::ask_room(self.output, &doing, needed, free, cache))?;
            } else if left > self.approval.above {
                let size = units::size(left);
                self.approve(|| super::ask(self.output, self.yes, &self.job.label, &size))?;
            }
        }
        if unknown {
            if let Some(total) = total
                && !self.sized.swap(true, Ordering::Relaxed)
            {
                self.overall.sized(total);
            }
            self.unbounded.store(total.is_none(), Ordering::Relaxed);
            self.received.store(resumed, Ordering::Relaxed);
            let watched = free.flatten().filter(|_| total.is_none() && !self.yes);
            self.free
                .store(watched.unwrap_or(u64::MAX), Ordering::Relaxed);
            self.free_from.store(resumed, Ordering::Relaxed);
        }
        self.restarting(resumed);
        let bar = self.bar();
        bar.message("Downloading", self.job.label.clone());
        bar.start(total, resumed);
        self.held.keep(resumed, resumable);
        self.count_kept(resumed);
        Ok(())
    }

    fn on(&self, event: &Event) -> Result<(), Error> {
        let label = &self.job.label;
        match *event {
            Event::Waiting => {
                let what = format!("{label} · another fetchloom is downloading it");
                self.bar().message("Waiting", what.clone());
                *self.waited() = Some(self.output.ticker("Waiting", what, [None]));
            }
            // no new bar: the download may still wait for a free slot. a lone download's bar,
            // already showing, says what it goes on from
            Event::Kept { bytes, resumable } => {
                self.held.keep(bytes, resumable);
                self.earlier.store(bytes, Ordering::Relaxed);
                if let Some(bar) = self.bar.get()
                    && bytes > 0
                {
                    bar.message(
                        "Checking",
                        format!("{label} · {} kept from an earlier run", units::size(bytes)),
                    );
                }
            }
            Event::Started {
                total,
                resumed,
                resumable,
            } => self.started(total, resumed, resumable)?,
            Event::WebPage { ref name } => {
                let given = name.as_deref().and_then(crate::name::sanitize);
                // a page that names no file, at a link whose own name has no extension, is a
                // folder listing or a site's page rather than data; hashes that prove the bytes
                // decide instead
                if given.is_none()
                    && std::path::Path::new(label).extension().is_none()
                    && !self.job.fetch.expect.proves()
                {
                    return Err(Error::new(
                        Kind::SourceInvalid,
                        format!(
                            "{} answered with a web page, such as a folder listing, not a file",
                            crate::net::Shown(&self.job.fetch.url)
                        ),
                    )
                    .help("open it in a browser, copy the link of each file you want, and give fetchloom those links"));
                }
                let name = given.as_deref().unwrap_or(label);
                if std::path::Path::new(name)
                    .extension()
                    .is_some_and(|extension| {
                        !matches!(
                            extension.to_string_lossy().to_ascii_lowercase().as_str(),
                            "htm" | "html" | "xhtml" | ""
                        )
                    })
                {
                    self.output.warn_with(&format!("{name} came back as a web page, not a data file. the link may open a page instead of downloading"), "look for a download or raw link");
                }
            }
            Event::Encoded(ref encoding) => {
                if !compressed_by_name(label, encoding)
                    && !self.encoded.swap(true, Ordering::Relaxed)
                {
                    let (said, help) = encoded(label, encoding);
                    self.output.warn_with(&said, &help);
                }
            }
            Event::Bytes(bytes) => {
                self.bar().advance(bytes);
                self.count(self.held.bytes.fetch_add(bytes, Ordering::Relaxed) + bytes);
                let received = self.received.fetch_add(bytes, Ordering::Relaxed) + bytes;
                if self.unbounded.load(Ordering::Relaxed) && received > self.approval.above {
                    let size = format!("more than {}", units::size(self.approval.above));
                    self.approve(|| super::ask(self.output, self.yes, &self.job.label, &size))?;
                    self.unbounded.store(false, Ordering::Relaxed);
                }
                self.room(received)?;
            }
        }
        Ok(())
    }
}

/// whether the name of the file at `path` already says its bytes are compressed with
/// `encoding`, as `data.csv.gz` does for gzip, so the server's label changes nothing a reader
/// expects.
fn compressed_by_name(path: &str, encoding: &str) -> bool {
    let name = path.to_ascii_lowercase();
    let endings: &[&str] = match encoding {
        "gzip" | "x-gzip" => &[".gz", ".tgz"],
        "br" => &[".br"],
        "zstd" => &[".zst", ".tzst"],
        "compress" | "x-compress" => &[".z"],
        "deflate" => &[".zz"],
        _ => &[],
    };
    endings.iter().any(|ending| name.ends_with(ending))
}

/// the warning for the file at `path`, whose bytes the server compressed with `encoding` on
/// their way, and what to do about it.
fn encoded(path: &str, encoding: &str) -> (String, String) {
    let said = format!(
        "{path} came compressed with {encoding}, as the server sent it with Content-Encoding: {encoding}, and fetchloom keeps the bytes as sent"
    );
    let help = if matches!(encoding, "gzip" | "x-gzip") {
        "decompress it with `gzip -dc`, or read it as gzip, as pandas does with compression=\"gzip\"".to_owned()
    } else {
        format!("decompress it with a tool for {encoding} before reading it")
    };
    (said, help)
}

/// downloads every job within the network's limits and returns what each fetched, in the jobs'
/// order. on linux the finished downloads are flushed together and put in place a group at a
/// time, the last group once every job ended, whether or not every job succeeded.
pub(super) async fn all(
    context: &Context,
    net: &Arc<Net>,
    cache: &Cache,
    jobs: Vec<Job>,
) -> Result<Vec<Fetched>, Error> {
    let labels: Vec<String> = jobs.iter().map(|job| job.label.clone()).collect();
    let batched = defer(cache, jobs.len());
    let results = each(context, net, cache, jobs, false).await;
    let published = publish(cache, batched);
    collected(results, published, &labels)
}

/// admits validated pinned jobs without keeping their full fetch and recovery values in memory.
pub(super) async fn prepared(
    context: &Context,
    net: &Arc<Net>,
    cache: &Cache,
    mut descriptions: Vec<Description>,
    urls: Vec<Url>,
    make: impl Fn(usize, Url) -> Job,
) -> Result<Vec<Fetched>, Error> {
    let count = descriptions.len();
    let batched = defer(cache, count);
    let overall = Arc::new(Overall::described(context, &descriptions));
    let queue = Queue::grouped(urls, |index, _| {
        std::mem::take(&mut descriptions[index].host)
    });
    let jobs = queue.map(|(index, url)| (index, make(index, url)));
    let results = run_each(context, net, cache, count, overall, jobs, false).await;
    let published = publish(cache, batched);
    let labels: Vec<_> = descriptions
        .into_iter()
        .map(|description| description.label)
        .collect();
    collected(results, published, &labels)
}

fn collected(
    results: Result<Vec<Result<Fetched, Error>>, Error>,
    published: Result<(), Error>,
    labels: &[String],
) -> Result<Vec<Fetched>, Error> {
    let mut fetched = Vec::with_capacity(labels.len());
    let mut failed = Vec::new();
    for (index, result) in results?.into_iter().enumerate() {
        match result {
            Ok(found) => fetched.push(found),
            Err(error) => failed.push((index, error)),
        }
    }
    // a cache that could not take the downloads in matters more than which of them failed
    published?;
    if let Some(error) = together(labels, failed) {
        return Err(error);
    }
    Ok(fetched)
}

/// downloads every job as [`all`] does, but a file that fails keeps its failure to itself while
/// the others go on, as several unrelated links need. only an interruption, or a failure of the
/// run itself, stops them all.
pub(super) async fn apart(
    context: &Context,
    net: &Arc<Net>,
    cache: &Cache,
    jobs: Vec<Job>,
) -> Result<Vec<Result<Fetched, Error>>, Error> {
    let batched = defer(cache, jobs.len());
    let results = each(context, net, cache, jobs, true).await;
    let published = publish(cache, batched);
    let results = results?;
    published?;
    Ok(results)
}

/// whether `downloads` wait for the cache's batch flushes. a few, or any without a batch, flush
/// one by one, since one flush of the filesystem also waits for everything other programs wrote
/// there.
fn defer(cache: &Cache, downloads: usize) -> bool {
    downloads > crate::cache::MOST_FLUSHED_ALONE && cache.defer_downloads().unwrap_or(false)
}

fn publish(cache: &Cache, batched: bool) -> Result<(), Error> {
    if batched {
        cache
            .publish_downloads()
            .map_err(|err| Error::io("could not put finished downloads into the cache", &err))
    } else {
        Ok(())
    }
}

struct Queue<T> {
    hosts: VecDeque<VecDeque<(usize, T)>>,
}

impl Queue<Job> {
    fn new(jobs: Vec<Job>) -> Self {
        Self::grouped(jobs, |_, job| {
            crate::net::host(&job.fetch.from).to_ascii_lowercase()
        })
    }
}

impl<T> Queue<T> {
    fn grouped(items: Vec<T>, mut host: impl FnMut(usize, &T) -> String) -> Self {
        let mut hosts = VecDeque::<VecDeque<_>>::new();
        let mut by_host = HashMap::new();
        for (index, job) in items.into_iter().enumerate() {
            let host = host(index, &job);
            let next = hosts.len();
            let group = *by_host.entry(host).or_insert(next);
            if group == hosts.len() {
                hosts.push_back(VecDeque::new());
            }
            hosts[group].push_back((index, job));
        }
        Self { hosts }
    }
}

impl<T> Iterator for Queue<T> {
    type Item = (usize, T);

    fn next(&mut self) -> Option<Self::Item> {
        let mut host = self.hosts.pop_front()?;
        let job = host.pop_front();
        if !host.is_empty() {
            self.hosts.push_back(host);
        }
        job
    }
}

/// what each job fetched, or why it failed on its own, in the jobs' order. `apart` keeps every
/// failure but an interruption with its job.
async fn each(
    context: &Context,
    net: &Arc<Net>,
    cache: &Cache,
    jobs: Vec<Job>,
    apart: bool,
) -> Result<Vec<Result<Fetched, Error>>, Error> {
    let count = jobs.len();
    let overall = Arc::new(Overall::new(context, &jobs));
    let queue = Queue::new(jobs);
    run_each(context, net, cache, count, overall, queue, apart).await
}

async fn run_each(
    context: &Context,
    net: &Arc<Net>,
    cache: &Cache,
    count: usize,
    overall: Arc<Overall>,
    mut queue: impl Iterator<Item = (usize, Job)>,
    apart: bool,
) -> Result<Vec<Result<Fetched, Error>>, Error> {
    let alone = count == 1;
    let mut results: Vec<Option<Result<Fetched, Error>>> =
        std::iter::repeat_with(|| None).take(count).collect();
    let mut tasks = JoinSet::new();
    // more tasks than download slots keep every slot busy, and few enough keep memory flat
    let in_flight = context.settings.jobs.value * 2;
    let mut done = 0;
    // unknown source sizes share one answer, once headers or received bytes show a large file.
    let approval =
        Arc::new(Approval::new(context.settings.ask_above.value).with_room(cache.root()));
    let listings = again::Listings::default();
    // what each unfinished download holds, which a stop keeps for the next run
    let held: Arc<[Held]> = std::iter::repeat_with(Held::default).take(count).collect();
    loop {
        while tasks.len() < in_flight
            && let Some((index, job)) = queue.next()
        {
            let (net, cache, output, overall, approval, listings, held) = (
                net.clone(),
                cache.clone(),
                context.output.clone(),
                overall.clone(),
                approval.clone(),
                listings.clone(),
                held.clone(),
            );
            let yes = context.yes;
            tasks.spawn(async move {
                let watch = Watch::new(&job, &output, &overall, &approval, yes, &held[index]);
                // with no bar for them all, a lone download shows its bar while the server
                // takes its time to answer
                if alone {
                    watch.bar();
                }
                let on = |event: Event| watch.on(&event);
                let fetched = match net.download(&job.fetch, &cache, &on).await {
                    Err(err) => {
                        again::recover(&net, &cache, &output, &listings, &job, err, &on).await
                    }
                    fetched => fetched,
                };
                watch.ended();
                if let Ok(found) = &fetched
                    && found
                        .name
                        .as_deref()
                        .and_then(crate::name::sanitize)
                        .is_none()
                    && let Some(name) = std::path::Path::new(&job.label)
                        .file_name()
                        .and_then(|name| name.to_str())
                    && !name.is_empty()
                    && name.bytes().all(|byte| byte.is_ascii_digit())
                {
                    output.warn_with(
                        &format!("the server did not name {name}"),
                        "try the article or DOI link, which names its files",
                    );
                }
                (index, fetched.map_err(|err| explain(err, &job)))
            });
        }
        let Some(joined) = tasks.join_next().await else {
            break;
        };
        match joined {
            Ok((index, Ok(fetched))) => {
                held[index].bytes.store(0, Ordering::Relaxed);
                results[index] = Some(Ok(fetched));
                done += 1;
                if let Some(bar) = &overall.bar {
                    bar.message("Downloading", batch(done, count));
                }
            }
            // a file that failed on its own leaves nothing the next run goes on from
            Ok((index, Err(err))) if on_its_own(&err) => {
                held[index].bytes.store(0, Ordering::Relaxed);
                results[index] = Some(Err(err));
            }
            Ok((index, Err(err))) if apart && err.kind != Kind::Interrupted => {
                results[index] = Some(Err(kept(err, std::slice::from_ref(&held[index]))));
            }
            Ok((_, Err(err))) => {
                if err.kind == Kind::Interrupted {
                    // a ctrl-c read as a key at a question reaches no signal handler, so the stop
                    // is asked here too. every download then winds down and keeps what it
                    // received; one cut off would lose bytes this count already promised
                    context.stop.ask();
                    wind_down(&mut tasks, &held).await;
                } else {
                    tasks.abort_all();
                }
                return Err(kept(err, &held));
            }
            Err(err) => {
                tasks.abort_all();
                return Err(Error::new(
                    Kind::LocalIo,
                    format!("a download stopped unexpectedly: {err}"),
                ));
            }
        }
    }
    Ok(results.into_iter().flatten().collect())
}

/// waits for the downloads still running to wind down from a stop, leaving in `held` what each
/// one that stopped unfinished keeps for the next run.
async fn wind_down(tasks: &mut JoinSet<(usize, Result<Fetched, Error>)>, held: &[Held]) {
    while let Some(joined) = tasks.join_next().await {
        // a finished file, or one failed on its own, leaves nothing to go on from
        if let Ok((index, result)) = joined
            && result.as_ref().map_or_else(on_its_own, |_| true)
        {
            held[index].bytes.store(0, Ordering::Relaxed);
        }
    }
}

/// what one unfinished download holds for the next run: how far into the file it is, an earlier
/// run's bytes included, and whether a later run can go on from there.
#[derive(Default)]
struct Held {
    bytes: AtomicU64,
    resumable: Mutex<Resumable>,
}

impl Held {
    fn keep(&self, bytes: u64, resumable: Resumable) {
        self.bytes.store(bytes, Ordering::Relaxed);
        *self
            .resumable
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = resumable;
    }

    fn resumable(&self) -> Resumable {
        *self
            .resumable
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

/// tells a person who stopped, or whose network failed, what the bytes so far are worth: what
/// the unfinished downloads in `held` keep, an earlier run's included, and whether the next run
/// goes on from them, which only the servers that send parts of a file allow.
fn kept(error: Error, held: &[Held]) -> Error {
    if !matches!(error.kind, Kind::Interrupted | Kind::NetworkUnreachable) {
        return error;
    }
    let (mut sure, mut maybe, mut lost) = (0_u64, 0_u64, 0_u64);
    for held in held {
        let bytes = held.bytes.load(Ordering::Relaxed);
        let sum = match held.resumable() {
            Resumable::Yes => &mut sure,
            Resumable::Maybe => &mut maybe,
            Resumable::No => &mut lost,
        };
        *sum = sum.saturating_add(bytes);
    }
    let going_on = sure.saturating_add(maybe);
    let error = match (going_on, maybe) {
        (0, _) => error,
        (_, 0) => error.detail(format!(
            "the {} downloaded so far is kept, and the next run continues from there",
            units::size(going_on)
        )),
        _ => error.detail(format!(
            "the {} downloaded so far is kept, and the next run continues from there if the server allows it",
            units::size(going_on)
        )),
    };
    match (lost, going_on) {
        (0, _) => error,
        (_, 0) => error.detail(format!(
            "the server cannot continue from the {} downloaded so far, so the next run starts over",
            units::size(lost)
        )),
        _ => error.detail(format!(
            "the other {} starts over in the next run, since its server cannot continue from it",
            units::size(lost)
        )),
    }
}

/// how many failed files one error names. the rest are counted.
pub(super) const NAMED: usize = 10;

/// whether a failure concerns its own file only, so the other downloads go on and every such
/// file is named at the end. a failure of the network, the disk or the person stops them all.
fn on_its_own(error: &Error) -> bool {
    matches!(
        error.kind,
        Kind::ReferenceUnresolved
            | Kind::IntegrityMismatch
            | Kind::AuthRequired
            | Kind::NetworkRefused
            | Kind::SourceInvalid
    )
}

/// one error for the files that failed on their own: the failure itself when it is the only
/// one, else a line naming each file and what happened to it, in the jobs' order.
pub(super) fn together(labels: &[String], mut failed: Vec<(usize, Error)>) -> Option<Error> {
    failed.sort_by_key(|(index, _)| *index);
    let failures = failed
        .iter()
        .map(|(index, error)| FileFailure {
            file: labels.get(*index).cloned().unwrap_or_default(),
            kind: error.kind.name(),
            message: error.message.clone(),
            details: error.details.clone(),
            help: error.help.as_deref().map(str::to_owned),
        })
        .collect();
    if failed.len() <= 1 {
        return failed.pop().map(|(_, mut error)| {
            error.failures = failures;
            error
        });
    }
    let (_, first) = failed.first()?;
    // one refusal for every file, as a competition whose rules are not accepted gives, reads
    // best said once, and it keeps the account the sign-in offer needs
    if failed
        .iter()
        .all(|(_, error)| error.message == first.message && error.help == first.help)
    {
        let count = failed.len();
        let (_, mut error) = failed.swap_remove(0);
        error.failures = failures;
        return Some(error.detail(format!("the same for all {count} files")));
    }
    let shared_help = failed
        .iter()
        .all(|(_, error)| error.help == first.help)
        .then(|| first.help.clone())
        .flatten();
    let told: Vec<Told<'_>> = failed
        .iter()
        .map(|(index, failure)| {
            let label = labels.get(*index).cloned().unwrap_or_default();
            Told {
                alone: if failure.message.starts_with(&label) {
                    failure.message.clone()
                } else {
                    format!("{label}: {}", failure.message)
                },
                label,
                error: failure,
            }
        })
        .collect();
    let mut error = Error::new(
        first.kind,
        format!("{} files could not be downloaded", failed.len()),
    );
    error.details = alike(&told, shared_help.is_some());
    error.failures = failures;
    error.need = failed.iter().find_map(|(_, error)| error.need.clone());
    Some(error.help(shared_help.unwrap_or_else(|| {
        "downloaded files stay in the cache. fix the errors above, then run the same command again"
            .into()
    })))
}

/// one of several failures as their one error tells it: `alone` is its whole line, naming what
/// failed, and `label` names what failed in the list under a failure alike.
pub(super) struct Told<'a> {
    pub(super) alone: String,
    pub(super) label: String,
    pub(super) error: &'a Error,
}

/// the lines one error gives for `failed`, in their order. failures alike, of one kind with one
/// help and the same words but for the link or file each names, are told once: the first one's
/// line, then what the others failed on, each named once, then their help, unless every failure
/// has the one help the error gives itself, as `shared` says. the first [`NAMED`] failures are
/// named, and the rest are counted.
pub(super) fn alike(failed: &[Told<'_>], shared: bool) -> Vec<String> {
    let mut groups: Vec<Vec<&Told<'_>>> = Vec::new();
    let mut group_of: HashMap<(&str, Option<&str>, String), usize> = HashMap::new();
    for told in failed.iter().take(NAMED) {
        let error = told.error;
        let key = (
            error.kind.name(),
            error.help.as_deref(),
            cause(&error.message, &told.label),
        );
        let next = groups.len();
        let group = *group_of.entry(key).or_insert(next);
        if group == next {
            groups.push(Vec::new());
        }
        groups[group].push(told);
    }
    let mut lines = Vec::new();
    for group in &groups {
        let Some((first, others)) = group.split_first() else {
            continue;
        };
        lines.push(first.alone.clone());
        if !others.is_empty() {
            lines.push("the same for:".to_owned());
            lines.extend(others.iter().map(|told| format!("  {}", listed(told))));
        }
        if !shared && let Some(help) = &first.error.help {
            lines.push(format!("help: {help}"));
        }
    }
    if failed.len() > NAMED {
        lines.push(format!("and {} more", failed.len() - NAMED));
    }
    lines
}

/// how the list under a failure alike names `told`: what failed, then where it was asked for,
/// when its message names an address other than that.
fn listed(told: &Told<'_>) -> String {
    let mut addresses: Vec<&str> = Vec::new();
    for word in told.error.message.split(' ') {
        let address = word.trim_end_matches([':', ',', ';', '.', ')']);
        if address.contains("://") && address != told.label && !addresses.contains(&address) {
            addresses.push(address);
        }
    }
    if addresses.is_empty() {
        told.label.clone()
    } else {
        format!("{} \u{b7} {}", told.label, addresses.join(" "))
    }
}

/// `message` without what tells one failure from others alike: `label`, the link or file it is
/// about, and each address in it, such as the one that file was asked for at.
fn cause(message: &str, label: &str) -> String {
    let unnamed = if label.is_empty() {
        message.to_owned()
    } else {
        message.replace(label, "\u{0}")
    };
    unnamed
        .split(' ')
        .map(|word| if word.contains("://") { "\u{0}" } else { word })
        .collect::<Vec<_>>()
        .join(" ")
}

/// says which file a failure concerns, and for bytes that changed upstream, the two ways on: the
/// pinned bytes from a folder mirror someone pushed them to, or the new bytes by an upgrade,
/// which only a person may choose, so the help offers it in words no fix takes.
fn explain(error: Error, job: &Job) -> Error {
    // an MD5 mismatch comes with its own help, since the source contradicts itself
    let dataset = job.dataset.as_ref().filter(|_| {
        error.kind == Kind::IntegrityMismatch
            && error.help.is_none()
            && job.fetch.expect.sha256.is_some()
    });
    let Some(dataset) = dataset else {
        return if error.kind == Kind::IntegrityMismatch {
            error.or_help("run the same command again later. if it keeps happening, ask the dataset's owner to check the file and its published size or SHA-256")
        } else {
            error
        };
    };
    Error {
        message: format!("{} is not the file data.lock records", job.label),
        help: Some(changed_upstream(dataset).into()),
        ..error
    }
}

/// what to do about pinned bytes of `dataset` that its source no longer serves.
fn changed_upstream(dataset: &str) -> String {
    format!(
        "to keep the pinned bytes, someone who has them runs `fetchloom push --to <folder>`, then you run `fetchloom sync --mirror <folder>`. if the new bytes are intended, run `fetchloom lock --upgrade {dataset}`"
    )
}

#[cfg(test)]
mod tests {
    use url::Url;

    use super::*;
    use crate::output::{Level, Style};

    /// a batch with no bar, as off a terminal, whose files left their sizes unknown.
    fn overall() -> Overall {
        Overall {
            bar: None,
            ticker: OnceLock::new(),
            what: String::new(),
            sizes: (0, 0),
            length: std::sync::Mutex::default(),
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn only_a_run_of_many_downloads_waits_for_one_flush() {
        use crate::cache::MOST_FLUSHED_ALONE;

        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path().join("cache")).unwrap();

        assert!(!defer(&cache, MOST_FLUSHED_ALONE));
        assert!(defer(&cache, MOST_FLUSHED_ALONE + 1));
        cache.publish_downloads().unwrap();
    }

    #[test]
    fn host_interleaving_keeps_job_identity_and_per_host_order() {
        let jobs: Vec<_> = [
            "slow.example",
            "SLOW.example:8443",
            "slow.example",
            "fast.example",
            "fast.example",
            "other.example",
        ]
        .into_iter()
        .enumerate()
        .map(|(index, host)| Job {
            fetch: Fetch::new(Url::parse(&format!("https://{host}/{index}")).unwrap()),
            label: index.to_string(),
            dataset: None,
            again: None,
        })
        .collect();
        let indexes: Vec<_> = Queue::new(jobs)
            .map(|(index, job)| {
                assert_eq!(job.label, index.to_string());
                index
            })
            .collect();
        assert_eq!(indexes, [0, 3, 5, 1, 4, 2]);
    }

    #[test]
    fn prepared_descriptions_keep_initial_hosts_sizes_and_input_indexes() {
        let (mut descriptions, urls): (Vec<_>, Vec<_>) = [
            "slow.example",
            "SLOW.example:8443",
            "fast.example",
            "fast.example",
        ]
        .into_iter()
        .enumerate()
        .map(|(index, host)| {
            let mut fetch =
                Fetch::new(Url::parse(&format!("https://storage.example/{index}")).unwrap());
            fetch.from = Url::parse(&format!("https://{host}/{index}")).unwrap();
            fetch.expect.size = (index != 0).then_some(0);
            Description::take(Job {
                fetch,
                label: index.to_string(),
                dataset: None,
                again: None,
            })
        })
        .unzip();
        assert_eq!(descriptions[0].size, None);
        assert_eq!(descriptions[1].size, Some(0));
        let queue = Queue::grouped(urls, |index, _| {
            std::mem::take(&mut descriptions[index].host)
        });
        let got: Vec<_> = queue
            .map(|(index, url)| {
                assert_eq!(descriptions[index].label, index.to_string());
                assert_eq!(url.as_str(), format!("https://storage.example/{index}"));
                index
            })
            .collect();
        assert_eq!(got, [0, 2, 1, 3]);
        assert!(
            descriptions
                .iter()
                .all(|description| description.host.is_empty())
        );
    }

    #[test]
    fn prepared_job_construction_stays_within_scheduler_admission() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path().join("cache")).unwrap();
        let mut context = Context::new(Output::new(
            Style {
                color: false,
                unicode: false,
                interactive: false,
                watched: false,
                links: false,
            },
            Level::Quiet,
            false,
            false,
        ));
        context.settings.jobs.value = 2;
        context.offline = crate::net::Offline::Flag;
        let net = Arc::new(context.net());
        let (descriptions, urls): (Vec<_>, Vec<_>) = (0..32)
            .map(|index| {
                Description::take(Job {
                    fetch: Fetch::new(Url::parse(&format!("https://example.org/{index}")).unwrap()),
                    label: index.to_string(),
                    dataset: None,
                    again: None,
                })
            })
            .unzip();
        let made = std::cell::Cell::new(0);
        let result = crate::source::recorded::block_on(prepared(
            &context,
            &net,
            &cache,
            descriptions,
            urls,
            |index, url| {
                made.set(made.get() + 1);
                Job {
                    fetch: Fetch::new(url),
                    label: index.to_string(),
                    dataset: None,
                    again: None,
                }
            },
        ));
        assert_eq!(result.unwrap_err().kind, Kind::OfflineMissing);
        assert!(
            made.get() <= context.settings.jobs.value * 2,
            "{} jobs were admitted",
            made.get()
        );
        assert_eq!(context.work.counts().requests, 0);
    }

    #[test]
    fn a_download_of_unknown_length_asks_once_it_passes_the_limit() {
        let style = Style {
            color: false,
            unicode: true,
            interactive: false,
            watched: false,
            links: false,
        };
        let output = Output::new(style, Level::Quiet, false, false);
        let job = Job {
            fetch: Fetch::new(Url::parse("https://example.org/big.bin").unwrap()),
            label: "big.bin".to_owned(),
            dataset: None,
            again: None,
        };
        let overall = overall();
        let unknown = || Event::Started {
            total: None,
            resumed: 0,
            resumable: Resumable::Maybe,
        };
        let approval = Approval::new(crate::settings::ASK_ABOVE);
        let held = Held::default();
        let watch = Watch::new(&job, &output, &overall, &approval, false, &held);
        watch.on(&unknown()).unwrap();
        watch.on(&Event::Bytes(crate::settings::ASK_ABOVE)).unwrap();
        let stopped = watch.on(&Event::Bytes(1)).unwrap_err();
        assert_eq!(stopped.kind, Kind::Unconfirmed);
        assert_eq!(
            stopped.message,
            "downloading big.bin needs more than 1.0 GiB, and fetchloom cannot ask you here"
        );
        assert_eq!(
            stopped.help.as_deref(),
            Some("add --yes to download it without being asked")
        );
        assert_eq!(
            watch.on(&Event::Bytes(1)).unwrap_err().kind,
            Kind::Unconfirmed
        );
        let fresh = Approval::new(crate::settings::ASK_ABOVE);
        let approved = Watch::new(&job, &output, &overall, &fresh, true, &held);
        approved.on(&unknown()).unwrap();
        approved
            .on(&Event::Bytes(2 * crate::settings::ASK_ABOVE))
            .unwrap();
        let another = Watch::new(&job, &output, &overall, &fresh, false, &held);
        another.on(&unknown()).unwrap();
        another
            .on(&Event::Bytes(2 * crate::settings::ASK_ABOVE))
            .unwrap();
        let told = Watch::new(&job, &output, &overall, &fresh, false, &held);
        let small = Event::Started {
            total: Some(10),
            resumed: 0,
            resumable: Resumable::Maybe,
        };
        told.on(&small).unwrap();
        told.on(&Event::Bytes(2 * crate::settings::ASK_ABOVE))
            .unwrap();
    }

    #[test]
    fn an_unknown_drive_size_asks_when_the_server_states_a_large_remainder() {
        let output = Output::new(
            Style {
                color: false,
                unicode: false,
                interactive: false,
                watched: false,
                links: false,
            },
            Level::Quiet,
            false,
            false,
        );
        let job = Job {
            fetch: Fetch::new(
                Url::parse("https://drive.google.com/uc?id=1cKSdgtWrPgvEsBGmjWALOH33taGyVXKb")
                    .unwrap(),
            ),
            label: "file.txt".to_owned(),
            dataset: None,
            again: None,
        };
        let overall = overall();
        let held = Held::default();
        for (total, resumed, needs_approval) in [
            (crate::settings::ASK_ABOVE, 0, false),
            (crate::settings::ASK_ABOVE + 1, 0, true),
            (crate::settings::ASK_ABOVE + 1, 1, false),
        ] {
            let approval = Approval::new(crate::settings::ASK_ABOVE);
            let watch = Watch::new(&job, &output, &overall, &approval, false, &held);
            let result = watch.on(&Event::Started {
                total: Some(total),
                resumed,
                resumable: Resumable::Maybe,
            });
            if needs_approval {
                assert_eq!(result.unwrap_err().kind, Kind::Unconfirmed);
            } else {
                result.unwrap();
            }
        }
    }

    #[test]
    fn refusing_a_size_question_does_not_approve_another_file() {
        let output = Output::new(
            Style {
                color: false,
                unicode: false,
                interactive: false,
                watched: false,
                links: false,
            },
            Level::Quiet,
            false,
            false,
        );
        let job = |name: &str| Job {
            fetch: Fetch::new(Url::parse(&format!("https://example.org/{name}")).unwrap()),
            label: name.to_owned(),
            dataset: None,
            again: None,
        };
        let first = job("first.bin");
        let second = job("second.bin");
        let overall = overall();
        let approval = Approval::new(crate::settings::ASK_ABOVE);
        let event = Event::Started {
            total: Some(crate::settings::ASK_ABOVE + 1),
            resumed: 0,
            resumable: Resumable::Maybe,
        };
        let held = Held::default();
        let first = Watch::new(&first, &output, &overall, &approval, false, &held);
        assert_eq!(first.on(&event).unwrap_err().kind, Kind::Unconfirmed);
        let second = Watch::new(&second, &output, &overall, &approval, false, &held);
        assert_eq!(second.on(&event).unwrap_err().kind, Kind::Unconfirmed);
    }

    /// a download whose bytes may turn out unchanged is not asked about before it starts, so a
    /// large one asks once the server sends it, though its size was known before.
    #[test]
    fn a_download_that_may_be_unchanged_asks_once_the_server_sends_it() {
        let output = Output::new(
            Style {
                color: false,
                unicode: false,
                interactive: false,
                watched: false,
                links: false,
            },
            Level::Quiet,
            false,
            false,
        );
        let mut job = Job {
            fetch: Fetch::new(Url::parse("https://example.org/big.bin").unwrap()),
            label: "big.bin".to_owned(),
            dataset: None,
            again: None,
        };
        job.fetch.expect.size = Some(crate::settings::ASK_ABOVE + 1);
        let started = Event::Started {
            total: Some(crate::settings::ASK_ABOVE + 1),
            resumed: 0,
            resumable: Resumable::Maybe,
        };
        let overall = overall();
        let held = Held::default();
        let approval = Approval::new(crate::settings::ASK_ABOVE);
        // a known size was asked about before the download started
        Watch::new(&job, &output, &overall, &approval, false, &held)
            .on(&started)
            .unwrap();
        job.fetch.held = Some(crate::digest::sha256(b"the bytes fetched before"));
        let approval = Approval::new(crate::settings::ASK_ABOVE);
        let watch = Watch::new(&job, &output, &overall, &approval, false, &held);
        assert_eq!(watch.on(&started).unwrap_err().kind, Kind::Unconfirmed);
    }

    #[test]
    fn a_batch_says_nothing_until_a_download_starts() {
        let context = Context::new(Output::new(
            Style {
                color: false,
                unicode: false,
                interactive: false,
                watched: false,
                links: false,
            },
            Level::Quiet,
            false,
            false,
        ));
        let job = Job {
            fetch: Fetch::new(Url::parse("https://example.org/big.bin").unwrap()),
            label: "big/big.bin".to_owned(),
            dataset: None,
            again: None,
        };
        let overall = Overall::new(&context, std::slice::from_ref(&job));
        let approval = Approval::new(crate::settings::ASK_ABOVE);
        let held = Held::default();
        let watch = Watch::new(&job, &context.output, &overall, &approval, false, &held);
        // another fetchloom downloads the file while this run waits for it
        watch.on(&Event::Waiting).unwrap();
        watch
            .on(&Event::Kept {
                bytes: 0,
                resumable: Resumable::Maybe,
            })
            .unwrap();
        assert!(overall.ticker.get().is_none());
        watch
            .on(&Event::Started {
                total: Some(10),
                resumed: 0,
                resumable: Resumable::Maybe,
            })
            .unwrap();
        assert!(overall.ticker.get().is_some());
    }

    /// a batch whose sizes are not all known has no total to fill to, so its bar promises no
    /// time left, until the last size is told; each size counts once.
    #[test]
    fn a_batch_has_a_total_only_once_every_size_is_known() {
        let context = Context::new(Output::new(
            Style {
                color: false,
                unicode: false,
                interactive: true,
                watched: false,
                links: false,
            },
            Level::Normal,
            false,
            false,
        ));
        let job = |size| {
            let mut fetch = Fetch::new(Url::parse("https://example.org/a.bin").unwrap());
            fetch.expect.size = size;
            Job {
                fetch,
                label: "a/a.bin".to_owned(),
                dataset: None,
                again: None,
            }
        };
        let jobs = [job(Some(100)), job(None), job(Some(50)), job(None)];
        let overall = Overall::new(&context, &jobs);
        let total = || overall.bar.as_ref().and_then(Bar::total);
        assert_eq!(total(), None, "150 B is not what the batch fills to");
        overall.sized(10);
        assert_eq!(total(), None);
        overall.sized(20);
        assert_eq!(total(), Some(180));
        assert_eq!(
            batch(1, 12),
            "12 files · 1 done",
            "four more may be under way"
        );
    }

    /// an archive whose size only its server told, and which is unpacked once it is in, needs
    /// room for what is left to transfer and for its files once more, at least the archive's
    /// size, when nobody listed them; a file kept as it came, or not an archive, needs only the
    /// transfer.
    #[test]
    fn an_archive_whose_server_told_its_size_needs_room_to_unpack_it() {
        const MB: u64 = 1_000_000;
        assert_eq!(room_needed(Some(MB), 0, true), 2 * MB);
        assert_eq!(room_needed(Some(MB), 400_000, true), 1_600_000);
        assert_eq!(room_needed(Some(MB), 0, false), MB);
        assert_eq!(
            room_needed(None, 5, true),
            0,
            "received bytes tell the rest"
        );
        assert_eq!(room_needed(Some(u64::MAX), 0, true), u64::MAX);
    }

    /// a lone download that finds bytes an earlier run kept says so on its bar while it gets
    /// ready to go on from them, before the server answers, rather than an empty bar.
    #[test]
    fn a_kept_part_is_told_before_the_download_starts() {
        let context = Context::new(Output::new(
            Style {
                color: false,
                unicode: false,
                interactive: true,
                watched: false,
                links: false,
            },
            Level::Normal,
            false,
            false,
        ));
        let job = Job {
            fetch: Fetch::new(Url::parse("https://example.org/big.bin").unwrap()),
            label: "big/big.bin".to_owned(),
            dataset: None,
            again: None,
        };
        let overall = Overall::new(&context, std::slice::from_ref(&job));
        let approval = Approval::new(crate::settings::ASK_ABOVE);
        let held = Held::default();
        let watch = Watch::new(&job, &context.output, &overall, &approval, false, &held);
        watch.bar();
        watch
            .on(&Event::Kept {
                bytes: 900,
                resumable: Resumable::Yes,
            })
            .unwrap();
        assert_eq!(
            watch.bar().told().as_deref(),
            Some("big/big.bin · 900 B kept from an earlier run")
        );
    }

    #[test]
    fn a_web_page_that_names_no_file_is_refused_before_its_bytes() {
        let output = Output::new(
            Style {
                color: false,
                unicode: false,
                interactive: false,
                watched: false,
                links: false,
            },
            Level::Quiet,
            false,
            false,
        );
        let overall = overall();
        let approval = Approval::new(crate::settings::ASK_ABOVE);
        let held = Held::default();
        let pinned = Some(crate::digest::sha256(b"a pinned listing"));
        for (link, label, name, sha256, refused) in [
            ("http://lab.example/shards/", "shards", None, None, true),
            ("http://lab.example/", "download", None, None, true),
            (
                "http://lab.example/shards/",
                "shards",
                Some("index.html"),
                None,
                false,
            ),
            ("http://lab.example/shards/", "shards", None, pinned, false),
            (
                "http://lab.example/page.html",
                "page.html",
                None,
                None,
                false,
            ),
            ("http://lab.example/data.csv", "data.csv", None, None, false),
        ] {
            let job = Job {
                fetch: Fetch {
                    expect: Expect {
                        sha256,
                        ..Expect::default()
                    },
                    ..Fetch::new(Url::parse(link).unwrap())
                },
                label: label.to_owned(),
                dataset: None,
                again: None,
            };
            let watch = Watch::new(&job, &output, &overall, &approval, false, &held);
            let page = Event::WebPage {
                name: name.map(str::to_owned),
            };
            match watch.on(&page) {
                Err(error) if refused => {
                    assert_eq!(error.kind, Kind::SourceInvalid);
                    assert_eq!(
                        error.message,
                        format!(
                            "{link} answered with a web page, such as a folder listing, not a file"
                        )
                    );
                    assert_eq!(
                        error.help.as_deref(),
                        Some(
                            "open it in a browser, copy the link of each file you want, and give fetchloom those links"
                        )
                    );
                }
                result => assert_eq!(result.is_err(), refused, "{link} as {label}"),
            }
        }
    }

    #[test]
    fn an_unpinned_download_is_not_described_as_locked() {
        let job = Job {
            fetch: Fetch {
                expect: crate::net::Expect {
                    sha256: Some(crate::digest::sha256(b"expected")),
                    ..crate::net::Expect::default()
                },
                ..Fetch::new(Url::parse("https://example.org/data.csv").unwrap())
            },
            label: "data.csv".to_owned(),
            dataset: None,
            again: None,
        };
        let error = explain(
            Error::new(Kind::IntegrityMismatch, "the file differs"),
            &job,
        );
        assert_eq!(error.message, "the file differs");
        assert!(error.help.unwrap().contains("published size or SHA-256"));
    }

    /// a stop promises the next run goes on only from bytes their server lets a later run go on
    /// from, says when a server never said whether it would, and says what starts over instead.
    #[test]
    fn a_stop_promises_to_go_on_only_where_a_server_allows() {
        let mib = 1 << 20;
        let said = |held: &[(u64, Resumable)]| {
            let held: Vec<Held> = held
                .iter()
                .map(|&(bytes, resumable)| {
                    let one = Held::default();
                    one.keep(bytes, resumable);
                    one
                })
                .collect();
            kept(Error::new(Kind::Interrupted, "stopped"), &held).details
        };
        assert_eq!(
            said(&[(3 * mib, Resumable::Yes), (0, Resumable::No)]),
            ["the 3.0 MiB downloaded so far is kept, and the next run continues from there"]
        );
        assert_eq!(
            said(&[(3 * mib, Resumable::Yes), (mib, Resumable::Maybe)]),
            [
                "the 4.0 MiB downloaded so far is kept, and the next run continues from there if the server allows it"
            ]
        );
        assert_eq!(
            said(&[(3 * mib, Resumable::No)]),
            [
                "the server cannot continue from the 3.0 MiB downloaded so far, so the next run starts over"
            ]
        );
        assert_eq!(
            said(&[(3 * mib, Resumable::Yes), (mib, Resumable::No)]),
            [
                "the 3.0 MiB downloaded so far is kept, and the next run continues from there",
                "the other 1.0 MiB starts over in the next run, since its server cannot continue from it"
            ]
        );
        assert_eq!(said(&[(0, Resumable::Yes)]), Vec::<String>::new());
        let other = Held::default();
        other.keep(mib, Resumable::Yes);
        let mismatch = kept(Error::new(Kind::IntegrityMismatch, "x"), &[other]);
        assert_eq!(
            mismatch.details,
            Vec::<String>::new(),
            "only a stop or a network failure"
        );
    }

    #[test]
    fn different_download_failures_keep_their_recovery_steps() {
        let labels = vec!["a.csv".to_owned(), "b.csv".to_owned()];
        let failed = vec![
            (
                0,
                Error::new(Kind::AuthRequired, "access refused").help("set HF_TOKEN"),
            ),
            (
                1,
                Error::new(Kind::ReferenceUnresolved, "file missing").help("check its link"),
            ),
        ];
        let error = together(&labels, failed).unwrap();
        assert_eq!(
            error.details,
            [
                "a.csv: access refused",
                "help: set HF_TOKEN",
                "b.csv: file missing",
                "help: check its link",
            ]
        );
        assert_eq!(
            error.help.as_deref(),
            Some(
                "downloaded files stay in the cache. fix the errors above, then run the same command again"
            )
        );
    }
}
