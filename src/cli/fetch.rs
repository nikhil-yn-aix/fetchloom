//! many downloads at once, with a bar for each and one for all, or plain lines off a terminal. the
//! first failure stops them, keeping what already arrived for the next run.

mod again;
pub(super) mod data_folder;
pub(super) mod mirrors;

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use tokio::task::JoinSet;
use url::Url;

pub(super) use self::again::Again;
use super::Context;
use super::sync::cache_budget;
use crate::archive::{self, Packed};
use crate::cache::Cache;
use crate::digest::Sha256;
use crate::error::{Error, FileFailure, Kind};
use crate::net::{Event, Expect, Fetch, Fetched, Net, Origin, Resumable, estimate};
use crate::output::{Bar, Output, Ticker, Warning};
use crate::source::{self, Remote};
use crate::stop::Stop;
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
    pub(super) host: String,
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
            named: remote.named_by_server,
            ..source::fetch_plan(&remote.url)
        },
        label: remote.path.clone(),
        dataset: None,
        again: None,
    }
}

/// the path a listed file takes once its bytes are known: `path`, with `.zip` added when its source
/// sent a zip of that one file, so it unpacks as any archive and data.lock names its bytes.
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
    /// the sizes known so far, added up, and how many files have yet to tell theirs: the bar takes
    /// that sum as its length only once none is left, so it promises no time left before then.
    length: std::sync::Mutex<(u64, usize)>,
    /// the downloads by host as [`planned`] counts them, each size added as its download tells it,
    /// so the line said once every size is known has a time too.
    hosts: std::sync::Mutex<BTreeMap<String, estimate::Host>>,
    limits: estimate::Limits,
    /// the run's stop, told how many downloads still receive and how many the disk keeps, so a
    /// first ctrl-c with only the disk left lets the run finish. `waiting` counts those not begun.
    stop: Arc<Stop>,
    waiting: AtomicUsize,
    /// the batch is one download, which its lines call by its own name.
    alone: bool,
}

impl Overall {
    fn new(context: &Context, jobs: &[Job]) -> Overall {
        let what = match jobs {
            [only] => only.label.clone(),
            _ => units::count(jobs.len() as u64, "file", "files"),
        };
        let hosts = hosts(
            context,
            jobs.iter().map(|job| (host_of(job), job.fetch.expect.size)),
        );
        Self::from_sizes(context, (jobs.len(), what, hosts), || {
            jobs.iter().map(|job| job.fetch.expect.size)
        })
    }

    fn described(context: &Context, descriptions: &[Description]) -> Overall {
        let what = match descriptions {
            [only] => only.label.clone(),
            _ => units::count(descriptions.len() as u64, "file", "files"),
        };
        let hosts = hosts(
            context,
            descriptions
                .iter()
                .map(|description| (description.host.clone(), description.size)),
        );
        Self::from_sizes(context, (descriptions.len(), what, hosts), || {
            descriptions.iter().map(|job| job.size)
        })
    }

    fn from_sizes<I: Iterator<Item = Option<u64>>>(
        context: &Context,
        (count, what, hosts): (usize, String, BTreeMap<String, estimate::Host>),
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
        context.stop.expect(count);
        Overall {
            bar,
            ticker: OnceLock::new(),
            what,
            sizes,
            length,
            hosts: std::sync::Mutex::new(hosts),
            limits: limits(context),
            stop: context.stop.clone(),
            waiting: AtomicUsize::new(count),
            alone: count == 1,
        }
    }

    /// what the plain lines call the batch, whose lone download is called `label` now.
    fn told_as(&self, label: &str) -> String {
        if self.alone {
            label.to_owned()
        } else {
            self.what.clone()
        }
    }

    /// starts the plain lines, when the first download of the batch starts, as `label` now.
    fn begin(&self, output: &Output, label: &str) {
        let (total, unknown) = self.sizes;
        self.ticker.get_or_init(|| {
            let sizes = std::iter::once(Some(total)).chain(std::iter::repeat_n(None, unknown));
            output.ticker("Downloading", self.told_as(label), sizes)
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

    /// adds the size a download from `host` just told.
    fn sized(&self, host: &str, bytes: u64) {
        let known = {
            let mut length = self
                .length
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let (total, unknown) = &mut *length;
            *total = total.saturating_add(bytes);
            *unknown = unknown.saturating_sub(1);
            (*unknown == 0).then_some(*total)
        };
        if let (Some(bar), Some(total)) = (&self.bar, known) {
            bar.length(total);
        }
        let time = self
            .timed(host, bytes, known.is_some())
            .map(estimate::Time::words);
        if let Some(ticker) = self.ticker.get() {
            ticker.sized(bytes, time.as_deref());
        }
    }

    /// counts `bytes` among `host`'s downloads, and once every size is `known`, how long the
    /// batch should take.
    fn timed(&self, host: &str, bytes: u64, known: bool) -> Option<estimate::Time> {
        let mut hosts = self
            .hosts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(planned) = hosts.get_mut(&source::paced_as(host)) {
            planned.unknown = planned.unknown.saturating_sub(1);
            add_size(planned, bytes);
        }
        if !known {
            return None;
        }
        let hosts: Vec<estimate::Host> = hosts.values().cloned().collect();
        estimate::time(&hosts, self.limits)
    }
}

impl Drop for Overall {
    fn drop(&mut self) {
        for _ in 0..*self.waiting.get_mut() {
            self.stop.ended(false);
        }
    }
}

/// the room a download whose server told `total` needs on the cache's disk: what is left after
/// `resumed`, and the whole archive again when it `unpacks`, a lower bound for its files.
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

/// the bar's words for a batch of `count` files once `done` of them finished.
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
    /// what the disk holding a folder has free, which tests give.
    free: fn(&std::path::Path) -> Option<u64>,
    /// what the cache's files hold and the `cache_budget` setting, when one is set.
    budget: Option<(u64, u64)>,
    /// whether `FETCHLOOM_CACHE_BUDGET` sets the budget, so its question says to raise that.
    by_variable: bool,
}

impl Approval {
    fn new(above: u64) -> Approval {
        Approval {
            answer: OnceLock::new(),
            above,
            cache: None,
            free: crate::space::free,
            budget: None,
            by_variable: false,
        }
    }

    /// also asks before a download the disk holding `cache` has no room for.
    fn with_room(mut self, cache: &std::path::Path) -> Approval {
        self.cache = Some(cache.to_path_buf());
        self
    }

    /// stops a download of no stated size before it takes the cache past `budget`, as the
    /// `cache_budget` setting gives it, from the `held` bytes the cache's files take now, and has
    /// one whose server states a size past it ask first.
    fn with_budget(mut self, budget: u64, held: u64) -> Approval {
        self.budget = Some((held, budget));
        self
    }

    /// the most a download of no stated size may take, with what bounds it, from what the cache's
    /// disk has `free`: the disk, or the budget when that leaves less. none when neither is known.
    fn unbounded_room(&self, free: Option<u64>) -> Option<(u64, Bound)> {
        let disk = free.map(|free| (free, Bound::Disk));
        let budget = self
            .budget
            .map(|(held, budget)| (budget.saturating_sub(held), Bound::Budget(budget)));
        match (disk, budget) {
            (Some(disk), Some(budget)) if budget.0 < disk.0 => Some(budget),
            (Some(disk), _) => Some(disk),
            (None, budget) => budget,
        }
    }
}

/// what ends a download of no stated size: the free room on the cache's disk, or what the
/// `cache_budget` setting, of these bytes, leaves the cache.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Bound {
    Disk,
    Budget(u64),
}

/// the error, or for a person who can answer the question, of a download of no stated size that
/// took `room`, all that `bound` leaves it on the disk holding `cache`.
fn unbounded(
    label: &str,
    (room, bound): (u64, Bound),
    cache: &std::path::Path,
) -> (String, &'static str) {
    let took = match bound {
        Bound::Disk => format!(
            "downloading {label} took the {} the disk holding the cache at {} had free",
            units::size(room),
            units::path(cache)
        ),
        Bound::Budget(budget) => format!(
            "downloading {label} took the {} the {} cache_budget leaves the cache",
            units::size(room),
            units::size(budget)
        ),
    };
    let help = match bound {
        Bound::Disk => {
            "free space on that disk, or set FETCHLOOM_CACHE to a folder on a bigger one, then run the same command again"
        }
        Bound::Budget(_) => {
            "free room with `fetchloom cache gc`, or raise cache_budget with `fetchloom settings set cache_budget <size>`, then run the same command again"
        }
    };
    (format!("{took}, and its server never said its size"), help)
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
    /// for a download of no stated size, the room it may take as it started, `u64::MAX` when
    /// unwatched, what bounds that room, and `received` then: bytes past it stop the download,
    /// in [`Watch::room`].
    free: AtomicU64,
    bound: Mutex<Bound>,
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
    /// whether every byte is in and the disk keeps the file, as the run's stop counts it.
    arrived: AtomicBool,
    /// nanoseconds the download waited for a person to say whether it may go on.
    asked: AtomicU64,
    /// what the progress line calls the file once its server named it, for a file that takes
    /// that name.
    named: Mutex<Option<String>>,
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
        let _ = overall
            .waiting
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |waiting| {
                waiting.checked_sub(1)
            });
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
            bound: Mutex::new(Bound::Disk),
            free_from: AtomicU64::new(0),
            held,
            earlier: AtomicU64::new(0),
            restarted: AtomicBool::new(false),
            counted: AtomicU64::new(0),
            waiting: std::sync::Mutex::new(None),
            encoded: AtomicBool::new(false),
            arrived: AtomicBool::new(false),
            asked: AtomicU64::new(0),
            named: Mutex::new(None),
        }
    }

    /// what the download is called on its progress line: its label, or once its server named a
    /// file that takes that name, the label with that name in place of its last part.
    fn label(&self) -> String {
        self.named
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .unwrap_or_else(|| self.job.label.clone())
    }

    /// shows the name the server gave a file that takes it, in place of the label's last part.
    fn renamed(&self, given: &str) {
        let Some(name) = crate::name::sanitize(given).filter(|_| self.job.fetch.named) else {
            return;
        };
        let label = match self.job.label.rsplit_once('/') {
            Some((folder, _)) => format!("{folder}/{name}"),
            None => name,
        };
        if let Some(bar) = self.bar.get() {
            bar.message("Downloading", label.clone());
        }
        *self.named.lock().unwrap_or_else(PoisonError::into_inner) = Some(label);
    }

    /// the download's bar, made when it is first needed.
    fn bar(&self) -> &Bar {
        self.bar
            .get_or_init(|| self.output.bar("Downloading", self.job.label.clone()))
    }

    /// the run's one answer, asking `question` when no download asked before. the time the
    /// download waits for it, asking or while another download asks, is no time it takes.
    fn approve(&self, question: impl FnOnce() -> Result<(), Error>) -> Result<(), Error> {
        if let Some(answer) = self.approval.answer.get() {
            return answer.clone();
        }
        let decide = || self.approval.answer.get_or_init(question).clone();
        let waiting = std::time::Instant::now();
        // a terminal question must not occupy a network worker while it waits for an answer
        let answer = if tokio::runtime::Handle::try_current().is_ok_and(|handle| {
            handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread
        }) {
            tokio::task::block_in_place(decide)
        } else {
            decide()
        };
        let waited = waiting.elapsed();
        if let Some(bar) = self.bar.get() {
            bar.answered(waited);
        }
        let waited = u64::try_from(waited.as_nanos()).unwrap_or(u64::MAX);
        self.asked.fetch_add(waited, Ordering::Relaxed);
        answer
    }

    /// `fetched` with the time the download waited for a person's answer.
    fn answered(&self, mut fetched: Fetched) -> Fetched {
        fetched.asked = std::time::Duration::from_nanos(self.asked.load(Ordering::Relaxed));
        fetched
    }

    /// says once that the download starts over from `resumed`, losing bytes an earlier run or try
    /// held, and why, from what the try before allowed and the new answer's `resumable` allows.
    fn restarting(&self, resumed: u64, resumable: Resumable) {
        let earlier = self.earlier.load(Ordering::Relaxed);
        let held = self.held.bytes.load(Ordering::Relaxed);
        let lost = if resumed >= held {
            return;
        } else if held <= earlier {
            format!(
                "the {} an earlier run kept cannot be continued",
                units::size(earlier)
            )
        } else {
            let why = match (self.held.resumable(), resumable) {
                (Resumable::No, _) => "the server cannot continue from them",
                (_, Resumable::No) => "the server sends the whole file instead of the rest",
                _ => "the file changed on the server",
            };
            format!(
                "the {} downloaded so far cannot be continued, since {why}",
                units::size(held)
            )
        };
        if !self.restarted.swap(true, Ordering::Relaxed) {
            self.output
                .mark("Restarting", &format!("{} · {lost}", self.job.label));
        }
    }

    /// stops a download of no stated size once `received` passes the room it may take, before
    /// writing the bytes that just arrived, so the disk is left near full and the budget kept. a
    /// person at a terminal may let it go on; with --yes, or with no one to ask, nothing else
    /// would end a body that never ends, so it stops.
    fn room(&self, received: u64) -> Result<(), Error> {
        let room = self.free.load(Ordering::Relaxed);
        let arrived = received.saturating_sub(self.free_from.load(Ordering::Relaxed));
        let Some(cache) = self.approval.cache.as_deref() else {
            return Ok(());
        };
        if room == u64::MAX || crate::space::short(arrived, Some(room)).is_none() {
            return Ok(());
        }
        let bound = *self.bound.lock().unwrap_or_else(PoisonError::into_inner);
        let (took, help) = unbounded(&self.job.label, (room, bound), cache);
        let stop = || Err(Error::new(Kind::LocalIo, took.clone()).help(help));
        if self.yes || !self.output.can_ask() {
            return stop();
        }
        let label = &self.job.label;
        self.approve(|| {
            let cancel = format!("the download of {label}");
            match self
                .output
                .confirm(&format!("{took}. go on anyway?"))
                .map_err(|error| error.cancelling(&cancel))?
            {
                Some(true) => Ok(()),
                Some(false) => Err(Error::new(
                    Kind::Unconfirmed,
                    format!("did not finish downloading {label}"),
                )
                .cancelled(cancel)),
                None => stop(),
            }
        })?;
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
            self.overall
                .sized(&host_of(self.job), self.received.load(Ordering::Relaxed));
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
        // a try that starts over after every byte was in receives again
        if self.arrived.swap(false, Ordering::SeqCst) {
            self.overall.stop.arrived(true);
        }
        // a size only the server states is checked here, before any byte is written
        if let (Some(cache), Some(total)) = (self.approval.cache.as_deref(), total) {
            crate::space::cache_holds(cache, [(self.job.label.as_str(), total)])?;
        }
        let left = total.map_or(0, |total| total.saturating_sub(resumed));
        let unknown = self.job.fetch.expect.size.is_none();
        let free = self.approval.cache.as_deref().map(self.approval.free);
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
            } else if unknown && let Some(past) = self.past_budget(total) {
                self.approve(|| self.ask_budget(past))?;
            } else if left > self.approval.above {
                let size = units::size(left);
                self.approve(|| super::ask(self.output, self.yes, &self.job.label, &size))?;
            }
        }
        // only once a person said yes, so a no never follows a line saying the download began
        self.overall.begin(self.output, &self.label());
        if unknown {
            if let Some(total) = total
                && !self.sized.swap(true, Ordering::Relaxed)
            {
                self.overall.sized(&host_of(self.job), total);
            }
            self.unbounded.store(total.is_none(), Ordering::Relaxed);
            self.received.store(resumed, Ordering::Relaxed);
            // with --yes too: a body of no stated size has no end but this
            let watched = self
                .approval
                .unbounded_room(free.flatten())
                .filter(|_| total.is_none());
            if let Some((_, bound)) = watched {
                *self.bound.lock().unwrap_or_else(PoisonError::into_inner) = bound;
            }
            self.free.store(
                watched.map_or(u64::MAX, |(room, _)| room),
                Ordering::Relaxed,
            );
            self.free_from.store(resumed, Ordering::Relaxed);
        }
        self.restarting(resumed, resumable);
        let bar = self.bar();
        bar.message("Downloading", self.label());
        bar.start(total, resumed);
        self.held.keep(resumed, resumable);
        self.count_kept(resumed);
        Ok(())
    }

    /// how a download whose size only its server states, as `total`, would take the cache past
    /// the `cache_budget` setting, the whole file and an archive's files beside it counting, since
    /// a partial download is no cached file. none within it, or when the server states no size.
    fn past_budget(&self, total: Option<u64>) -> Option<cache_budget::Past> {
        let (held, budget) = self.approval.budget?;
        let adding = room_needed(Some(total?), 0, self.job.fetch.unpack);
        cache_budget::weigh(Some(budget), adding, || Some(held))
    }

    /// the question sync and get ask before downloads they know the size of would take the cache
    /// `past` its budget, here once the server states the size.
    fn ask_budget(&self, past: cache_budget::Past) -> Result<(), Error> {
        let doing = format!("downloading {}", self.label());
        let over = cache_budget::Over {
            doing: &doing,
            past,
            freeable: None,
            largest: Vec::new(),
            by_variable: self.approval.by_variable,
        };
        cache_budget::ask(self.output, self.yes, &over)
    }

    /// the refusal of a web page the job's link answered with where it expected a file, which
    /// `named` says the link's name promised, as `data.csv` does.
    fn page_refused(&self, named: bool) -> Error {
        let link = crate::shown::Shown(&self.job.fetch.url);
        let asked = match &self.job.dataset {
            Some(dataset) => format!("add web_page = true to [datasets.{dataset}] in data.toml"),
            None => "add --web-page".to_owned(),
        };
        if named {
            // a site's sign-in is kept for its host and port, as `auth login` takes them
            let url = &self.job.fetch.url;
            let host = match url.port() {
                Some(port) => format!("{}:{port}", crate::net::host(url)),
                None => crate::net::host(url).to_owned(),
            };
            return Error::new(
                Kind::SourceInvalid,
                format!("{link} answered with a web page, such as a sign-in page, not a data file"),
            )
            .help(format!("open it in a browser: a page that asks you to sign in needs `fetchloom auth login {host}`, else look for a download or raw link. if the page itself is the data, {asked}"));
        }
        Error::new(
            Kind::SourceInvalid,
            format!("{link} answered with a web page, such as a folder listing, not a file"),
        )
        .help(format!("open it in a browser, copy the link of each file you want, and give fetchloom those links. if the page itself is the data, {asked}"))
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
            Event::WebPage { .. } if self.job.fetch.page => {}
            Event::WebPage { ref name } => {
                let given = name.as_deref().and_then(crate::name::sanitize);
                let named = given.as_deref().unwrap_or(label);
                let extension = std::path::Path::new(named)
                    .extension()
                    .map(|extension| extension.to_string_lossy().to_ascii_lowercase())
                    .filter(|extension| !extension.is_empty());
                let names_a_page = matches!(extension.as_deref(), Some("htm" | "html" | "xhtml"));
                // a page that names no file is a sign-in page, a folder listing or a site's page
                // rather than data, unless the link names a page or hashes prove the bytes
                if given.is_none() && !names_a_page && !self.job.fetch.expect.proves() {
                    return Err(self.page_refused(extension.is_some()));
                }
                if extension.is_some() && !names_a_page {
                    self.output.warn_with(Warning::DownloadWebPage, &format!("{named} came back as a web page, not a data file. the link may open a page instead of downloading"), "look for a download or raw link");
                }
            }
            Event::Named(ref given) => self.renamed(given),
            Event::Encoded(ref encoding) => {
                if !compressed_by_name(label, encoding)
                    && !self.encoded.swap(true, Ordering::Relaxed)
                {
                    let (said, help) = encoded(label, encoding);
                    self.output
                        .warn_with(Warning::DownloadEncoded, &said, &help);
                }
            }
            Event::Arrived => {
                if !self.arrived.swap(true, Ordering::SeqCst) {
                    self.overall.stop.arrived(false);
                }
            }
            Event::Retrying {
                next,
                of,
                wait,
                ref limit,
            } => self.bar().retrying(next, of, wait, limit.clone()),
            Event::Behind(behind) => self.bar().behind(behind),
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

impl Drop for Watch<'_> {
    fn drop(&mut self) {
        self.overall.stop.ended(*self.arrived.get_mut());
    }
}

/// whether the name at `path` already says its bytes are compressed with `encoding`, as
/// `data.csv.gz` does gzip, so the server's label changes nothing a reader expects.
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

/// how long the downloads `jobs`, each a host and a size if known, should take, added up by pacing
/// name from each host's remembered speed, its source's pacing and the run's job limits.
pub(super) fn planned(
    context: &Context,
    jobs: impl IntoIterator<Item = (String, Option<u64>)>,
) -> estimate::Total {
    let hosts: Vec<estimate::Host> = hosts(context, jobs).into_values().collect();
    estimate::Total::of(&hosts, limits(context))
}

/// [`planned`]'s downloads by the name each host is paced under, before they are added up.
fn hosts(
    context: &Context,
    jobs: impl IntoIterator<Item = (String, Option<u64>)>,
) -> BTreeMap<String, estimate::Host> {
    let mut hosts: BTreeMap<String, estimate::Host> = BTreeMap::new();
    for (host, size) in jobs {
        let planned = hosts.entry(source::paced_as(&host)).or_default();
        *planned.files.get_or_insert(0) += 1;
        match size {
            Some(size) => add_size(planned, size),
            None => planned.unknown += 1,
        }
    }
    for (name, host) in &mut hosts {
        host.speed = context.speeds.speed(name);
        host.spacing = source::spacing(name);
        host.most = source::most_at_once(name);
    }
    hosts
}

/// counts a file of `size` among `host`'s bytes.
fn add_size(host: &mut estimate::Host, size: u64) {
    host.bytes = host.bytes.saturating_add(size);
    host.largest = host.largest.max(size);
}

/// the downloads at `links` a sync may ask for, each by its host and size. a link a request is
/// refused before it is sent, as a private address `allowed_origins` does not list, is left out,
/// with a warning for each origin, so nothing says it downloads.
pub(super) fn askable(
    context: &Context,
    links: Vec<(Url, Option<u64>)>,
) -> Vec<(String, Option<u64>)> {
    if links.is_empty() {
        return Vec::new();
    }
    let net = context.net();
    let mut warned = std::collections::HashSet::new();
    links
        .into_iter()
        .filter_map(|(link, size)| {
            let Some(refusal) = net.refusal(&link) else {
                return Some((crate::net::host(&link).to_ascii_lowercase(), size));
            };
            if warned.insert(link.origin()) {
                context.output.warn_detailed(
                    Warning::NetworkPrivateOrigin,
                    &refusal.message,
                    refusal.help.as_deref(),
                    std::iter::empty::<&str>(),
                );
            }
            None
        })
        .collect()
}

/// the run's limits on downloads at once.
pub(super) fn limits(context: &Context) -> estimate::Limits {
    estimate::Limits {
        jobs: context.settings.jobs.value,
        host_jobs: context.settings.host_jobs.value,
    }
}

/// the host a job's requests start at, as its downloads are queued and its speed remembered.
pub(super) fn host_of(job: &Job) -> String {
    crate::net::host(&job.fetch.from).to_ascii_lowercase()
}

/// says, before downloads start, what `total` brings and about how long it takes, as `Downloading
/// 3 files · 1.2 GiB · about 3 min`. a total with no size known has nothing to say yet.
pub(super) fn tell_planned(output: &Output, what: &str, total: &estimate::Total) {
    tell_total(output, "Downloading", what, total, output.shows_bars());
}

/// after the downloads [`tell_planned`] told as `what` and `total`, a line when `fetched` holds
/// none from the network: the cache had every file, or its source said it had not changed.
pub(super) fn tell_found(
    output: &Output,
    what: &str,
    total: &estimate::Total,
    fetched: &[Fetched],
) {
    if is_told(total)
        && fetched
            .iter()
            .all(|fetched| fetched.origin != Origin::Network)
    {
        output.step("Found", &format!("{what} in the cache"));
    }
}

/// [`tell_planned`] for a run that would download, as a dry run or `status` tells it: once
/// there are several files, or a time to add to the one file's own line.
pub(super) fn tell_would(output: &Output, what: &str, total: &estimate::Total) {
    if total.files > 1 || total.time.is_some() {
        tell_total(output, "Would download", what, total, false);
    }
}

/// what a dry run would download: a line for each of `downloads` while there are few, then
/// their `total`, which says how to list them when there were too many.
pub(super) fn tell_would_fetch<D: std::borrow::Borrow<super::sync::place::Download>>(
    output: &Output,
    downloads: &[D],
    total: &estimate::Total,
) {
    let labels = downloads
        .iter()
        .map(|download| download.borrow().path.as_str());
    let what = called(labels, total.files);
    if !one_by_one(output, downloads.len()) {
        output.step(
            "Would download",
            &format!("{what} · {} · `-v` lists them", total.facts(false)),
        );
        return;
    }
    for download in downloads.iter().map(std::borrow::Borrow::borrow) {
        let size = download
            .size
            .map_or_else(|| "size unknown".to_owned(), units::size);
        output.step("Would fetch", &format!("{} · {size}", download.path));
    }
    tell_would(output, &what, total);
}

/// `live` says a bar will show the time once the downloads start.
fn tell_total(output: &Output, verb: &str, what: &str, total: &estimate::Total, live: bool) {
    if is_told(total) {
        output.step(verb, &format!("{what} · {}", total.facts(live)));
    }
}

/// whether a total of downloads is told before they start: some file with a known size.
fn is_told(total: &estimate::Total) -> bool {
    total.files > 0 && total.is_sized()
}

/// the most downloads, or datasets, a run tells on a line each; past it one line counts them.
const LISTED: usize = 40;

/// whether a run tells each of `count` things on a line of its own: a few, or any number at `-v`.
pub(super) fn one_by_one(output: &Output, count: usize) -> bool {
    count <= LISTED || output.shows_details()
}

/// what `files` downloads are called before they start: the one file's name, or a count.
pub(super) fn called<'a>(mut labels: impl Iterator<Item = &'a str>, files: u64) -> String {
    match (files, labels.next()) {
        (1, Some(only)) => only.to_owned(),
        _ => units::count(files, "file", "files"),
    }
}

/// downloads every job within the network's limits, returning what each fetched in order. on linux
/// finished downloads are flushed and named a group at a time, the last once every job ended.
pub(super) async fn all(
    context: &Context,
    net: &Arc<Net>,
    cache: &Cache,
    jobs: Vec<Job>,
) -> Result<Vec<Fetched>, Error> {
    let labels: Vec<String> = jobs.iter().map(|job| job.label.clone()).collect();
    let batched = defer(cache, jobs.len(), context.settings.machine.open_files);
    let results = naming_meanwhile(cache, batched, each(context, net, cache, jobs, false)).await;
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
    let batched = defer(cache, count, context.settings.machine.open_files);
    let overall = Arc::new(Overall::described(context, &descriptions));
    let queue = Queue::grouped(urls, |index, _| {
        std::mem::take(&mut descriptions[index].host)
    });
    let jobs = queue.map(|(index, url)| (index, make(index, url)));
    let results = naming_meanwhile(
        cache,
        batched,
        run_each(context, net, cache, count, overall, jobs, false),
    )
    .await;
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

/// [`all`], but a file that fails keeps its failure to itself while the others go on, as unrelated
/// links need. only an interruption or a failure of the run itself stops them all.
pub(super) async fn apart(
    context: &Context,
    net: &Arc<Net>,
    cache: &Cache,
    jobs: Vec<Job>,
) -> Result<Vec<Result<Fetched, Error>>, Error> {
    let batched = defer(cache, jobs.len(), context.settings.machine.open_files);
    let results = naming_meanwhile(cache, batched, each(context, net, cache, jobs, true)).await;
    let published = publish(cache, batched);
    let results = results?;
    published?;
    Ok(results)
}

/// whether `downloads` wait for the cache's batch flushes, given `open_files`. a few flush one by
/// one, since a filesystem flush also waits for everything other programs wrote there.
fn defer(cache: &Cache, downloads: usize, open_files: Option<u64>) -> bool {
    downloads > crate::volume::MOST_FLUSHED_ALONE
        && cache.defer_downloads(open_files).unwrap_or(false)
}

/// how often finished downloads that waited long enough for their group take their names.
const NAMING: std::time::Duration = std::time::Duration::from_millis(250);

/// runs `work` while the downloads that waited long enough in the open batch take their names,
/// so another run that wants one of them does not wait for this run's longest download.
async fn naming_meanwhile<T>(
    cache: &Cache,
    batched: bool,
    work: impl std::future::Future<Output = T>,
) -> T {
    if !batched {
        return work.await;
    }
    let naming = tokio::spawn({
        let cache = cache.clone();
        async move {
            loop {
                tokio::time::sleep(NAMING).await;
                let cache = cache.clone();
                // a failure stays with the batch, which reports it when it ends
                let _ = tokio::task::spawn_blocking(move || {
                    cache.publish_due(std::time::Instant::now())
                })
                .await;
            }
        }
    });
    let done = work.await;
    naming.abort();
    done
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

/// items taken a host at a time, each host in turn, so one host's many items never hold back
/// another's.
pub(super) struct Queue<T> {
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
    /// `items` by the host `host` names for each, given its place among them.
    pub(super) fn grouped(items: Vec<T>, mut host: impl FnMut(usize, &T) -> String) -> Self {
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
    // unknown source sizes share one answer, once headers or received bytes show a large file
    let approval = Approval::new(context.settings.ask_above.value).with_room(cache.root());
    let approval = Arc::new(budgeted(context, cache, approval, overall.sizes.1 > 0).await);
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
                let fetched = fetched.map(|found| watch.answered(found));
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
                        Warning::DownloadUnnamed,
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
                    // is asked here, and each download winds down keeping what this count promised
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

/// `approval`, bounding downloads of no stated size by what the `cache_budget` setting leaves
/// the cache, when one is set and the batch has `unknown` sizes. the cache's files count as its
/// disk stores them, as `fetchloom cache` holds them against the budget. reading them takes a
/// scan, so a batch whose sizes are all known skips it.
async fn budgeted(context: &Context, cache: &Cache, approval: Approval, unknown: bool) -> Approval {
    let Some(budget) = context.settings.cache_budget.value.filter(|_| unknown) else {
        return approval;
    };
    let (scanned, threads, stop) = (cache.clone(), context.threads, context.stop.clone());
    let held = tokio::task::spawn_blocking(move || {
        cache_budget::held(&scanned, threads, &stop).map(|(held, _)| held)
    })
    .await
    .ok()
    .flatten();
    let approval = Approval {
        by_variable: context.settings.cache_budget.origin == crate::settings::Origin::Environment,
        ..approval
    };
    // a cache whose files cannot be read is taken to fit, as sync's budget check takes it
    match held {
        Some(held) => approval.with_budget(budget, held),
        None => approval,
    }
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

/// tells a person who stopped, or whose network failed, what the unfinished downloads in `held`
/// keep, an earlier run's bytes included, and that bytes no later run can go on from are deleted.
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
            "the {} downloaded so far is deleted, since the server cannot continue from it",
            units::size(lost)
        )),
        _ => error.detail(format!(
            "the other {} is deleted, since its server cannot continue from it",
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
        let count = units::grouped(failed.len() as u64);
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
        format!(
            "{} could not be downloaded",
            units::count(failed.len() as u64, "file", "files")
        ),
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

/// the lines one error gives for `failed`, in order. failures alike, of one kind and help with the
/// same words but for what each names, are told once, then each other named, then their help unless
/// `shared`. the first [`NAMED`] failures are named and the rest counted.
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
        lines.push(format!(
            "and {} more",
            units::grouped((failed.len() - NAMED) as u64)
        ));
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

/// says which file a failure concerns, and for pinned bytes that changed upstream, the ways on: a
/// folder mirror someone pushed them to, or an upgrade only a person may choose, so no fix takes
/// it. a pinned link with nothing at it now may still be on such a mirror.
fn explain(error: Error, job: &Job) -> Error {
    if job.again.is_some() && error.help.as_deref() == Some(crate::net::MOVED_HELP) {
        return error.help(GONE_HELP);
    }
    // an MD5 mismatch comes with its own help, since the source contradicts itself, unless
    // data.lock pinned that MD5, as `add --no-sync` does, and the file changed since
    let pinned_md5 = job.again.is_some() && job.fetch.expect.sha256.is_none();
    let dataset = job.dataset.as_ref().filter(|_| {
        error.kind == Kind::IntegrityMismatch
            && (pinned_md5 || (error.help.is_none() && job.fetch.expect.sha256.is_some()))
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

/// what to do about a file data.lock pins whose link has nothing at it now.
const GONE_HELP: &str = "to keep the pinned bytes, someone who has them runs `fetchloom push --to <folder>`, then you run this command again with `--mirror <folder>`. if the file moved, put its new link as ref in data.toml";

/// what to do about pinned bytes of `dataset` that its source no longer serves.
fn changed_upstream(dataset: &str) -> String {
    format!(
        "to keep the pinned bytes, someone who has them runs `fetchloom push --to <folder>`, then you run this command again with `--mirror <folder>`. if the new bytes are intended, run `fetchloom lock --upgrade {dataset}`"
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
            hosts: std::sync::Mutex::default(),
            limits: estimate::Limits {
                jobs: 1,
                host_jobs: 1,
            },
            stop: Arc::default(),
            waiting: AtomicUsize::new(0),
            alone: false,
        }
    }

    /// downloads told before they start, as a retry of an add tells them, end on a line even when
    /// the cache or the source's "unchanged" answered for every file and none came over the network.
    #[test]
    fn downloads_that_fetched_nothing_say_where_the_files_were() {
        let fetched = |origin| Fetched {
            digest: crate::digest::sha256(b"rows"),
            size: 51_456,
            moved: 0,
            name: None,
            origin,
            version: None,
            began: None,
            arrived: None,
            asked: std::time::Duration::ZERO,
        };
        let total = estimate::Total {
            files: 4,
            bytes: 205_824,
            sizes_unknown: 0,
            time: None,
        };
        let sizeless = estimate::Total {
            sizes_unknown: 4,
            ..total
        };
        let cached = [fetched(Origin::NotModified), fetched(Origin::Cache)];
        let downloaded = [fetched(Origin::Cache), fetched(Origin::Network)];
        for (total, fetched, said) in [
            (
                &total,
                &cached[..],
                vec!["      Found 4 files in the cache"],
            ),
            (&total, &downloaded[..], Vec::new()),
            (&sizeless, &cached[..], Vec::new()),
        ] {
            let output = Output::new(
                Style {
                    color: false,
                    unicode: true,
                    interactive: false,
                    watched: false,
                    links: false,
                    accessible: false,
                },
                Level::Normal,
                false,
                false,
            );
            tell_found(&output, "4 files", total, fetched);
            assert_eq!(output.said(), said, "{total:?}");
        }
    }

    /// a dry run names each of a few dozen downloads, then their total; past that the total alone
    /// says how many, and `-v` names each.
    #[test]
    fn a_dry_run_of_many_downloads_counts_them_on_one_line() {
        for (files, level, lines) in [
            (40, Level::Normal, 41),
            (41, Level::Normal, 1),
            (41, Level::Verbose, 42),
        ] {
            let output = Output::new(
                Style {
                    color: false,
                    unicode: false,
                    interactive: false,
                    watched: false,
                    links: false,
                    accessible: false,
                },
                level,
                false,
                false,
            );
            let downloads: Vec<super::super::sync::place::Download> = (0..files)
                .map(|number| super::super::sync::place::Download {
                    path: format!("set/{number:02}.csv"),
                    size: Some(10),
                })
                .collect();
            let total = estimate::Total {
                files,
                bytes: files * 10,
                sizes_unknown: 0,
                time: None,
            };
            tell_would_fetch(&output, &downloads, &total);
            let said = output.said();
            assert_eq!(said.len(), lines, "{files}: {said:?}");
            if lines == 1 {
                assert_eq!(
                    said[0],
                    "      Would download 41 files - 410 B - `-v` lists them"
                );
            } else {
                assert_eq!(said[0], "      Would fetch set/00.csv - 10 B");
            }
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn only_a_run_of_many_downloads_waits_for_one_flush() {
        use crate::volume::MOST_FLUSHED_ALONE;

        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path().join("cache")).unwrap();

        assert!(!defer(&cache, MOST_FLUSHED_ALONE, None));
        assert!(defer(&cache, MOST_FLUSHED_ALONE + 1, None));
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
                accessible: false,
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
            accessible: false,
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
            "downloading big.bin needs more than 10.0 GiB, and fetchloom cannot ask you here"
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

    /// a body of no stated size stops at the room it may take, with --yes too and where no one
    /// can answer, naming what bounds it: the disk's free room, or `cache_budget` when that leaves
    /// less. a stated size was weighed against the disk before the download began.
    #[test]
    fn a_download_of_no_stated_size_stops_at_the_room_it_may_take() {
        let output = Output::new(
            Style {
                color: false,
                unicode: false,
                interactive: false,
                watched: false,
                links: false,
                accessible: false,
            },
            Level::Quiet,
            false,
            false,
        );
        let job = Job {
            fetch: Fetch::new(Url::parse("https://example.org/big.bin").unwrap()),
            label: "big.bin".to_owned(),
            dataset: None,
            again: None,
        };
        let overall = overall();
        let held = Held::default();
        let cache = std::path::Path::new("/cache");
        let started = |total| Event::Started {
            total,
            resumed: 0,
            resumable: Resumable::No,
        };
        for (budget, yes, room, message, help) in [
            (
                None,
                true,
                1000,
                "downloading big.bin took the 1000 B the disk holding the cache at /cache had free, and its server never said its size",
                "free space on that disk, or set FETCHLOOM_CACHE to a folder on a bigger one, then run the same command again",
            ),
            (
                Some((9000, 8500)),
                false,
                500,
                "downloading big.bin took the 500 B the 8.8 KiB cache_budget leaves the cache, and its server never said its size",
                "free room with `fetchloom cache gc`, or raise cache_budget with `fetchloom settings set cache_budget <size>`, then run the same command again",
            ),
        ] {
            let mut approval = Approval::new(crate::settings::ASK_ABOVE).with_room(cache);
            approval.free = |_| Some(1000);
            if let Some((budget, held)) = budget {
                approval = approval.with_budget(budget, held);
            }
            let watch = Watch::new(&job, &output, &overall, &approval, yes, &held);
            watch.on(&started(None)).unwrap();
            watch.on(&Event::Bytes(room)).unwrap();
            let stopped = watch.on(&Event::Bytes(1)).unwrap_err();
            assert_eq!(stopped.kind, Kind::LocalIo);
            assert_eq!(stopped.message, message);
            assert_eq!(stopped.help.as_deref(), Some(help));
            let sized = Watch::new(&job, &output, &overall, &approval, yes, &held);
            sized.on(&started(Some(10))).unwrap();
            sized.on(&Event::Bytes(2000)).unwrap();
        }
    }

    /// a size only the server states is held against `cache_budget` before any byte is written,
    /// as sync and get hold the sizes they know: 18 bytes cached under a 1 KiB budget leave 1,006,
    /// and an archive's files count again. past it, a run no one can answer stops, and --yes goes
    /// on with a warning.
    #[test]
    fn a_size_the_server_states_is_held_against_the_budget() {
        let output = || {
            let style = Style {
                color: false,
                unicode: false,
                interactive: false,
                watched: false,
                links: false,
                accessible: false,
            };
            Output::new(style, Level::Normal, false, false)
        };
        let job = |unpack| {
            let mut fetch = Fetch::new(Url::parse("https://example.org/big.bin").unwrap());
            fetch.unpack = unpack;
            Job {
                fetch,
                label: "big.bin".to_owned(),
                dataset: None,
                again: None,
            }
        };
        let overall = overall();
        let held = Held::default();
        let started = |total| Event::Started {
            total: Some(total),
            resumed: 0,
            resumable: Resumable::No,
        };
        for (total, unpack, past) in [
            (1006, false, false),
            (1007, false, true),
            (600, false, false),
            (600, true, true),
        ] {
            let (output, job) = (output(), job(unpack));
            let approval = Approval::new(crate::settings::ASK_ABOVE).with_budget(1024, 18);
            let watch = Watch::new(&job, &output, &overall, &approval, false, &held);
            let result = watch.on(&started(total));
            assert_eq!(result.is_err(), past, "{total} {unpack}");
        }
        let (output, job) = (output(), job(false));
        let approval = Approval::new(crate::settings::ASK_ABOVE).with_budget(1024, 18);
        let watch = Watch::new(&job, &output, &overall, &approval, false, &held);
        let stopped = watch.on(&started(3000)).unwrap_err();
        assert_eq!(stopped.kind, Kind::Unconfirmed);
        assert_eq!(
            stopped.message,
            "downloading big.bin would bring the cache to 2.9 KiB, past the 1.0 KiB cache_budget"
        );
        let advice = stopped.help.unwrap_or_default();
        assert!(
            advice.contains("`fetchloom settings set cache_budget <size>`"),
            "{advice}"
        );
        let approval = Approval::new(crate::settings::ASK_ABOVE).with_budget(1024, 18);
        let watch = Watch::new(&job, &output, &overall, &approval, true, &held);
        watch.on(&started(3000)).unwrap();
        let said = output.said();
        assert!(
            said.iter().any(|line| line.starts_with(
                "warning: downloading big.bin brings the cache to 2.9 KiB, past the 1.0 KiB cache_budget\n"
            )),
            "{said:?}"
        );
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
                accessible: false,
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
                accessible: false,
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

    /// a download that may be unchanged is never asked about up front, even with its size known.
    #[test]
    fn a_download_that_may_be_unchanged_asks_once_the_server_sends_it() {
        let output = Output::new(
            Style {
                color: false,
                unicode: false,
                interactive: false,
                watched: false,
                links: false,
                accessible: false,
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
                accessible: false,
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

    /// plain lines never say a refused download began, however long a person took to say no.
    #[test]
    fn a_download_its_question_stops_never_says_it_began() {
        let context = Context::new(Output::new(
            Style {
                color: false,
                unicode: false,
                interactive: false,
                watched: false,
                links: false,
                accessible: true,
            },
            Level::Normal,
            false,
            false,
        ));
        let job = Job {
            fetch: Fetch::new(Url::parse("https://example.org/titanic.csv").unwrap()),
            label: "titanic.csv".to_owned(),
            dataset: None,
            again: None,
        };
        let overall = Overall::new(&context, std::slice::from_ref(&job));
        let approval = Approval::new(1024);
        let held = Held::default();
        let watch = Watch::new(&job, &context.output, &overall, &approval, false, &held);
        let refused = watch
            .on(&Event::Started {
                total: Some(57_000),
                resumed: 0,
                resumable: Resumable::Maybe,
            })
            .unwrap_err();
        assert_eq!(refused.kind, Kind::Unconfirmed);
        assert!(overall.ticker.get().is_none());
    }

    /// a bar whose sizes are not all known promises no time left, and each size counts once.
    #[test]
    fn a_batch_has_a_total_only_once_every_size_is_known() {
        let context = Context::new(Output::new(
            Style {
                color: false,
                unicode: false,
                interactive: true,
                watched: false,
                links: false,
                accessible: false,
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
        overall.sized("example.org", 10);
        assert_eq!(total(), None);
        overall.sized("example.org", 20);
        assert_eq!(total(), Some(180));
        assert_eq!(
            batch(1, 12),
            "12 files · 1 done",
            "four more may be under way"
        );
    }

    /// an archive needs room for what is left to transfer plus its files again, at least its size
    /// if unlisted. a file kept as it came, or no archive, needs only the transfer.
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

    /// a file that takes the name its server gives, as a drive link's does, shows that name on
    /// its progress line once the server answers, in its dataset's folder; another keeps the
    /// name it was asked by.
    #[test]
    fn a_download_named_by_its_server_shows_that_name() {
        let context = Context::new(Output::new(
            Style {
                color: false,
                unicode: false,
                interactive: true,
                watched: false,
                links: false,
                accessible: false,
            },
            Level::Normal,
            false,
            false,
        ));
        for (named, shown) in [
            (true, "gdrive-spam/spam.txt"),
            (false, "gdrive-spam/download"),
        ] {
            let job = Job {
                fetch: Fetch {
                    named,
                    ..Fetch::new(Url::parse("https://drive.google.com/uc?id=0B9P1L").unwrap())
                },
                label: "gdrive-spam/download".to_owned(),
                dataset: Some("gdrive-spam".to_owned()),
                again: None,
            };
            let overall = Overall::new(&context, std::slice::from_ref(&job));
            let approval = Approval::new(crate::settings::ASK_ABOVE);
            let held = Held::default();
            let watch = Watch::new(&job, &context.output, &overall, &approval, false, &held);
            watch.bar();
            watch.on(&Event::Named("spam.txt".to_owned())).unwrap();
            watch
                .on(&Event::Started {
                    total: Some(5),
                    resumed: 0,
                    resumable: Resumable::Maybe,
                })
                .unwrap();
            assert_eq!(watch.bar().told().as_deref(), Some(shown), "named: {named}");
            assert_eq!(overall.told_as(&watch.label()), shown, "named: {named}");
        }
    }

    /// a lone download's bar shows the kept bytes before the server answers, not an empty bar.
    #[test]
    fn a_kept_part_is_told_before_the_download_starts() {
        let context = Context::new(Output::new(
            Style {
                color: false,
                unicode: false,
                interactive: true,
                watched: false,
                links: false,
                accessible: false,
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

    /// what a job for `link`, shown as `label` and pinned to `sha256` when given, makes of a web
    /// page answering it that names the file `name`.
    fn on_page(
        link: &str,
        label: &str,
        name: Option<&str>,
        sha256: Option<crate::digest::Sha256>,
    ) -> Result<(), Error> {
        let style = Style {
            color: false,
            unicode: false,
            interactive: false,
            watched: false,
            links: false,
            accessible: false,
        };
        let output = Output::new(style, Level::Quiet, false, false);
        let overall = overall();
        let approval = Approval::new(crate::settings::ASK_ABOVE);
        let held = Held::default();
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
        watch.on(&Event::WebPage {
            name: name.map(str::to_owned),
        })
    }

    #[test]
    fn a_web_page_that_names_no_file_is_refused_before_its_bytes() {
        let pinned = Some(crate::digest::sha256(b"a pinned listing"));
        let listing = "answered with a web page, such as a folder listing, not a file";
        let sign_in = "answered with a web page, such as a sign-in page, not a data file";
        for (link, label, name, sha256, refused) in [
            (
                "http://lab.example/shards/",
                "shards",
                None,
                None,
                Some(listing),
            ),
            ("http://lab.example/", "download", None, None, Some(listing)),
            (
                "http://lab.example/data.csv",
                "data.csv",
                None,
                None,
                Some(sign_in),
            ),
            (
                "http://lab.example/x.ZIP",
                "x.ZIP",
                None,
                None,
                Some(sign_in),
            ),
            (
                "http://lab.example/shards/",
                "shards",
                Some("index.html"),
                None,
                None,
            ),
            ("http://lab.example/shards/", "shards", None, pinned, None),
            (
                "http://lab.example/data.csv",
                "data.csv",
                None,
                pinned,
                None,
            ),
            (
                "http://lab.example/export",
                "export",
                Some("data.csv"),
                None,
                None,
            ),
            (
                "http://lab.example/page.html",
                "page.html",
                None,
                None,
                None,
            ),
            ("http://lab.example/page.HTM", "page.HTM", None, None, None),
        ] {
            match (on_page(link, label, name, sha256), refused) {
                (Err(error), Some(said)) => {
                    assert_eq!(error.kind, Kind::SourceInvalid);
                    assert_eq!(error.message, format!("{link} {said}"));
                    let advice = error.help.unwrap_or_default();
                    assert!(
                        advice.ends_with("if the page itself is the data, add --web-page"),
                        "{advice}"
                    );
                    assert_eq!(
                        advice.contains("`fetchloom auth login lab.example`"),
                        said == sign_in,
                        "{advice}"
                    );
                }
                (result, refused) => {
                    assert_eq!(result.is_err(), refused.is_some(), "{link} as {label}");
                }
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

    /// a new link keeps the help its command words.
    #[test]
    fn a_pinned_file_with_nothing_at_its_link_names_the_mirror_route() {
        let gone = || {
            Error::new(Kind::ReferenceUnresolved, "127.0.0.1 has nothing at it")
                .help(crate::net::MOVED_HELP)
        };
        let mut job = Job {
            fetch: Fetch::new(Url::parse("https://example.org/data.csv").unwrap()),
            label: "iris/data.csv".to_owned(),
            dataset: Some("iris".to_owned()),
            again: None,
        };
        assert_eq!(
            explain(gone(), &job).help.as_deref(),
            Some(crate::net::MOVED_HELP)
        );
        job.again = Some(Again {
            resolved: "https://example.org/data.csv".to_owned(),
            path: "data.csv".to_owned(),
            alone: true,
        });
        assert_eq!(
            explain(gone(), &job).help.as_deref(),
            Some(
                "to keep the pinned bytes, someone who has them runs `fetchloom push --to <folder>`, then you run this command again with `--mirror <folder>`. if the file moved, put its new link as ref in data.toml"
            )
        );
    }

    /// the stop says when a server never said whether a run may go on, and what starts over.
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
            ["the 3.0 MiB downloaded so far is deleted, since the server cannot continue from it"]
        );
        assert_eq!(
            said(&[(3 * mib, Resumable::Yes), (mib, Resumable::No)]),
            [
                "the 3.0 MiB downloaded so far is kept, and the next run continues from there",
                "the other 1.0 MiB is deleted, since its server cannot continue from it"
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

    #[test]
    fn many_download_failures_are_counted_with_thousands_apart() {
        let labels: Vec<String> = (0..1200).map(|n| format!("f{n}.csv")).collect();
        let refused = |n| (n, Error::new(Kind::AuthRequired, "refused").help("sign in"));
        let error = together(&labels, (0..1200).map(refused).collect()).unwrap();
        assert_eq!(error.details, ["the same for all 1,200 files"]);
        let missing = |n| {
            let error = Error::new(Kind::ReferenceUnresolved, format!("f{n}.csv is missing"));
            (n, error.help("check its link"))
        };
        let error = together(&labels, (0..1200).map(missing).collect()).unwrap();
        assert_eq!(error.message, "1,200 files could not be downloaded");
        assert_eq!(error.details.last().unwrap(), "and 1,190 more");
    }
}
