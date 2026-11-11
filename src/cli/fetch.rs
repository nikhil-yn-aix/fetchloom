//! many downloads at once: a bar for each running one and one for them all, or plain lines now
//! and then off a terminal, and a stop at the first failure that keeps what already arrived for
//! the next run.

mod again;
pub(super) mod data_folder;
pub(super) mod mirrors;

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use tokio::task::JoinSet;
use url::Url;

pub(super) use self::again::Again;
use super::Context;
use crate::archive::{self, Packed};
use crate::cache::Cache;
use crate::digest::Sha256;
use crate::error::{Error, FileFailure, Kind};
use crate::net::{Event, Expect, Fetch, Fetched, Net};
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
    /// what the bar fills to so far. downloads run on several threads and learn sizes at once,
    /// so each addition reads and writes it under one lock.
    length: std::sync::Mutex<u64>,
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
            let known: Vec<u64> = sizes().flatten().collect();
            bar.start(
                (!known.is_empty()).then(|| known.iter().copied().fold(0, u64::saturating_add)),
                0,
            );
            bar
        });
        let length = std::sync::Mutex::new(sizes().flatten().fold(0, u64::saturating_add));
        let sizes = sizes().fold((0, 0), |(total, unknown), size| match size {
            Some(size) => (u64::saturating_add(total, size), unknown),
            None => (total, unknown + 1),
        });
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

    fn sized(&self, bytes: u64) {
        if let Some(bar) = &self.bar {
            let mut length = self
                .length
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *length = length.saturating_add(bytes);
            bar.length(*length);
        }
        if let Some(ticker) = self.ticker.get() {
            ticker.sized(bytes);
        }
    }
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
    /// how far into the file this download is: the bytes it holds, an earlier run's included,
    /// which a stop keeps for the next run. the batch owns it, to say what a stop kept.
    position: &'a AtomicU64,
    /// the furthest point the overall progress counted, so bytes fetched again after a retry or
    /// a fresh start count once.
    counted: AtomicU64,
    /// plain lines while another fetchloom downloads the file, where no bar says so.
    waiting: std::sync::Mutex<Option<Ticker>>,
}

impl<'a> Watch<'a> {
    fn new(
        job: &'a Job,
        output: &'a Output,
        overall: &'a Overall,
        approval: &'a Approval,
        yes: bool,
        position: &'a AtomicU64,
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
            position,
            counted: AtomicU64::new(0),
            waiting: std::sync::Mutex::new(None),
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

    fn waited(&self) -> std::sync::MutexGuard<'_, Option<Ticker>> {
        self.waiting
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// moves the overall progress up to `position`, never back and never twice over a byte.
    fn count(&self, position: u64) {
        let before = self.counted.fetch_max(position, Ordering::Relaxed);
        if position > before {
            self.overall.advance(position - before);
        }
    }

    fn on(&self, event: &Event) -> Result<(), Error> {
        let label = &self.job.label;
        match *event {
            Event::Waiting => {
                let what = format!("{label} · another fetchloom is downloading it");
                self.bar().message("Waiting", what.clone());
                *self.waited() = Some(self.output.ticker("Waiting", what, [None]));
            }
            // no bar yet: the download may still wait for a free slot
            Event::Kept(bytes) => self.position.store(bytes, Ordering::Relaxed),
            Event::Started { total, resumed, .. } => {
                self.waited().take();
                // a size only the server states is checked here, before any byte is written
                if let (Some(cache), Some(total)) = (self.approval.cache.as_deref(), total) {
                    crate::space::cache_holds(cache, [(self.job.label.as_str(), total)])?;
                }
                self.overall.begin(self.output);
                let left = total.map_or(0, |total| total.saturating_sub(resumed));
                if self.job.fetch.expect.size.is_none() {
                    // a disk without room asks instead, and its question names the size too
                    let free = self.approval.cache.as_deref().map(crate::space::free);
                    if let (Some(cache), Some(free)) = (self.approval.cache.as_deref(), free)
                        && !self.yes
                        && crate::space::short(left, free).is_some()
                    {
                        let doing = format!("downloading {}", self.job.label);
                        let free = free.unwrap_or_default();
                        self.approve(|| super::ask_room(self.output, &doing, left, free, cache))?;
                    } else if left > self.approval.above {
                        let size = units::size(left);
                        self.approve(|| super::ask(self.output, self.yes, &self.job.label, &size))?;
                    }
                    if let Some(total) = total
                        && !self.sized.swap(true, Ordering::Relaxed)
                    {
                        self.overall.sized(total);
                    }
                    self.unbounded.store(total.is_none(), Ordering::Relaxed);
                    self.received.store(resumed, Ordering::Relaxed);
                }
                let bar = self.bar();
                bar.message("Downloading", label.clone());
                bar.start(total, resumed);
                self.position.store(resumed, Ordering::Relaxed);
                self.count(resumed);
            }
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
            Event::Bytes(bytes) => {
                self.bar().advance(bytes);
                self.count(self.position.fetch_add(bytes, Ordering::Relaxed) + bytes);
                let received = self.received.fetch_add(bytes, Ordering::Relaxed) + bytes;
                if self.unbounded.load(Ordering::Relaxed) && received > self.approval.above {
                    let size = format!("more than {}", units::size(self.approval.above));
                    self.approve(|| super::ask(self.output, self.yes, &self.job.label, &size))?;
                    self.unbounded.store(false, Ordering::Relaxed);
                }
            }
        }
        Ok(())
    }
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
    let held: Arc<[AtomicU64]> = std::iter::repeat_with(AtomicU64::default)
        .take(count)
        .collect();
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
                held[index].store(0, Ordering::Relaxed);
                results[index] = Some(Ok(fetched));
                done += 1;
                if let Some(bar) = &overall.bar {
                    bar.message("Downloading", format!("{done} of {count} files"));
                }
            }
            // a file that failed on its own leaves nothing the next run goes on from
            Ok((index, Err(err))) if on_its_own(&err) => {
                held[index].store(0, Ordering::Relaxed);
                results[index] = Some(Err(err));
            }
            Ok((index, Err(err))) if apart && err.kind != Kind::Interrupted => {
                results[index] = Some(Err(kept(err, held[index].load(Ordering::Relaxed))));
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
                let bytes = held.iter().map(|bytes| bytes.load(Ordering::Relaxed)).sum();
                return Err(kept(err, bytes));
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
async fn wind_down(tasks: &mut JoinSet<(usize, Result<Fetched, Error>)>, held: &[AtomicU64]) {
    while let Some(joined) = tasks.join_next().await {
        // a finished file, or one failed on its own, leaves nothing to go on from
        if let Ok((index, result)) = joined
            && result.as_ref().map_or_else(on_its_own, |_| true)
        {
            held[index].store(0, Ordering::Relaxed);
        }
    }
}

/// tells a person who stopped, or whose network failed, that the bytes so far are not lost:
/// `bytes`, what the unfinished downloads hold, an earlier run's included.
fn kept(error: Error, bytes: u64) -> Error {
    if bytes == 0 || !matches!(error.kind, Kind::Interrupted | Kind::NetworkUnreachable) {
        return error;
    }
    error.detail(format!(
        "the {} downloaded so far is kept, and the next run continues from there",
        units::size(bytes)
    ))
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
    let mut error = Error::new(
        first.kind,
        format!("{} files could not be downloaded", failed.len()),
    );
    error.failures = failures;
    error.need = failed.iter().find_map(|(_, error)| error.need.clone());
    // files that failed the same way share one line, in the order the first of each failed
    let mut groups: Vec<(&Error, Vec<usize>)> = Vec::new();
    let mut group_of: HashMap<(&str, Option<&str>), usize> = HashMap::new();
    for (index, failure) in &failed {
        let key = (failure.message.as_str(), failure.help.as_deref());
        if let Some(&group) = group_of.get(&key) {
            groups[group].1.push(*index);
        } else {
            group_of.insert(key, groups.len());
            groups.push((failure, vec![*index]));
        }
    }
    for (failure, indexes) in groups.iter().take(NAMED) {
        let label = labels.get(indexes[0]).map_or("", String::as_str);
        error = error.detail(if indexes.len() > 1 {
            format!("{} files: {}", indexes.len(), failure.message)
        } else if failure.message.starts_with(label) {
            failure.message.clone()
        } else {
            format!("{label}: {}", failure.message)
        });
        if shared_help.is_none()
            && let Some(help) = &failure.help
        {
            error = error.detail(format!("help: {help}"));
        }
    }
    if groups.len() > NAMED {
        let rest: usize = groups[NAMED..]
            .iter()
            .map(|(_, indexes)| indexes.len())
            .sum();
        error = error.detail(format!("and {rest} more"));
    }
    Some(error.help(shared_help.unwrap_or_else(|| {
        "downloaded files stay in the cache. fix the errors above, then run the same command again"
            .into()
    })))
}

/// says which file a failure concerns, and for bytes that changed upstream, how to take them on.
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
        help: Some(
            format!("if the new version is intended, run `fetchloom lock --upgrade {dataset}`")
                .into(),
        ),
        ..error
    }
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
        };
        let approval = Approval::new(crate::settings::ASK_ABOVE);
        let position = AtomicU64::new(0);
        let watch = Watch::new(&job, &output, &overall, &approval, false, &position);
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
        let approved = Watch::new(&job, &output, &overall, &fresh, true, &position);
        approved.on(&unknown()).unwrap();
        approved
            .on(&Event::Bytes(2 * crate::settings::ASK_ABOVE))
            .unwrap();
        let another = Watch::new(&job, &output, &overall, &fresh, false, &position);
        another.on(&unknown()).unwrap();
        another
            .on(&Event::Bytes(2 * crate::settings::ASK_ABOVE))
            .unwrap();
        let told = Watch::new(&job, &output, &overall, &fresh, false, &position);
        let small = Event::Started {
            total: Some(10),
            resumed: 0,
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
        let position = AtomicU64::new(0);
        for (total, resumed, needs_approval) in [
            (crate::settings::ASK_ABOVE, 0, false),
            (crate::settings::ASK_ABOVE + 1, 0, true),
            (crate::settings::ASK_ABOVE + 1, 1, false),
        ] {
            let approval = Approval::new(crate::settings::ASK_ABOVE);
            let watch = Watch::new(&job, &output, &overall, &approval, false, &position);
            let result = watch.on(&Event::Started {
                total: Some(total),
                resumed,
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
        };
        let position = AtomicU64::new(0);
        let first = Watch::new(&first, &output, &overall, &approval, false, &position);
        assert_eq!(first.on(&event).unwrap_err().kind, Kind::Unconfirmed);
        let second = Watch::new(&second, &output, &overall, &approval, false, &position);
        assert_eq!(second.on(&event).unwrap_err().kind, Kind::Unconfirmed);
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
        let position = AtomicU64::new(0);
        let watch = Watch::new(&job, &context.output, &overall, &approval, false, &position);
        // another fetchloom downloads the file while this run waits for it
        watch.on(&Event::Waiting).unwrap();
        watch.on(&Event::Kept(0)).unwrap();
        assert!(overall.ticker.get().is_none());
        watch
            .on(&Event::Started {
                total: Some(10),
                resumed: 0,
            })
            .unwrap();
        assert!(overall.ticker.get().is_some());
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
        let position = AtomicU64::new(0);
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
            let watch = Watch::new(&job, &output, &overall, &approval, false, &position);
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
