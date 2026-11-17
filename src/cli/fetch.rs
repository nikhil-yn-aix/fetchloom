//! many downloads at once, with a bar for each and one for all, or plain lines off a terminal. the
//! first failure stops them, keeping what already arrived for the next run.

mod again;
pub(super) mod archived;
pub(super) mod data_folder;
pub(super) mod mirrors;
pub(super) mod reuse;

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
use crate::net::{
    Event, Expect, Fetch, Fetched, Listed, Net, NoVersion, Origin, Resumable, estimate,
};
use crate::output::{Bar, Output, Ticker, Warning};
use crate::source::folder::PAGE_FLAG;
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

/// the first bytes of a file that came as a web page, which tell a page from data.
pub(in crate::cli) const HEAD: usize = 1024;

/// the tags a page may open with that no data file does, as a sign-in redirect's script or
/// refresh does, or a sign-in form does.
const PAGE_TAGS: [&[u8]; 20] = [
    b"<script",
    b"<meta",
    b"<head",
    b"<body",
    b"<title",
    b"<form",
    b"<div",
    b"<p",
    b"<h1",
    b"<h2",
    b"<h3",
    b"<h4",
    b"<h5",
    b"<h6",
    b"<iframe",
    b"<noscript",
    b"<style",
    b"<link",
    b"<svg",
    b"<center",
];

/// whether `head`, a file's first bytes, is html: past a utf-8, utf-16 or utf-32 byte order mark,
/// white space and comments, it opens with `<!doctype html` or a tag only a page opens with, or it
/// holds `<html`, in any case.
pub(in crate::cli) fn reads_as_html(head: &[u8]) -> bool {
    let utf16 = |rest: &[u8], unit: fn([u8; 2]) -> u16| -> Vec<u8> {
        let units = rest.as_chunks::<2>().0.iter().map(|&pair| unit(pair));
        char::decode_utf16(units)
            .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect::<String>()
            .into_bytes()
    };
    let utf32 = |rest: &[u8], unit: fn([u8; 4]) -> u32| -> Vec<u8> {
        rest.as_chunks::<4>()
            .0
            .iter()
            .map(|&four| char::from_u32(unit(four)).unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect::<String>()
            .into_bytes()
    };
    // utf-32's little-endian mark starts as utf-16's does
    let text = match head {
        [0xff, 0xfe, 0, 0, rest @ ..] => utf32(rest, u32::from_le_bytes),
        [0, 0, 0xfe, 0xff, rest @ ..] => utf32(rest, u32::from_be_bytes),
        [0xff, 0xfe, rest @ ..] => utf16(rest, u16::from_le_bytes),
        [0xfe, 0xff, rest @ ..] => utf16(rest, u16::from_be_bytes),
        _ => head.strip_prefix(b"\xef\xbb\xbf").unwrap_or(head).to_vec(),
    }
    .to_ascii_lowercase();
    let mut opening = text.as_slice();
    loop {
        opening = opening.trim_ascii_start();
        let Some(comment) = opening.strip_prefix(b"<!--") else {
            break;
        };
        opening = comment
            .windows(3)
            .position(|window| window == b"-->")
            .map_or(&[], |end| &comment[end + 3..]);
    }
    let opens = |tag: &[u8]| {
        opening.strip_prefix(tag).is_some_and(|rest| {
            rest.first()
                .is_none_or(|&next| next == b'>' || next == b'/' || next.is_ascii_whitespace())
        })
    };
    opening.starts_with(b"<!doctype html")
        || PAGE_TAGS.into_iter().any(opens)
        || text.windows(5).any(|window| window == b"<html")
}

/// how a refusal of a web page says to ask for the page itself, for a download data.toml's entry
/// `dataset` asks for.
pub(in crate::cli) fn page_entry(dataset: &str) -> String {
    format!("add web_page = true to [datasets.{dataset}] in data.toml")
}

/// `error`, which a source gave while resolving data.toml's entry `dataset`, with its help saying
/// to ask for a web page through that entry rather than the flag of an add or a get.
pub(in crate::cli) fn page_in_entry(mut error: Error, dataset: &str) -> Error {
    if let Some(help) = error.help.as_deref()
        && help.contains(PAGE_FLAG)
    {
        let help = help.replace(PAGE_FLAG, &page_entry(dataset));
        error = error.help(help);
    }
    error
}

/// what a download makes of a web page the link of a job answered with, before its bytes are in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::cli) enum PageCall {
    /// the file: the link names a page, data.toml asks for it, or hashes prove its bytes.
    Taken,
    /// a page that names no file under no data file's name: a sign-in page or a folder listing.
    Refused,
    /// a page under a data file's name, which its first bytes tell from data, as a server that
    /// calls a csv a web page sends the csv.
    ByBytes,
    /// a page its server named as a file of another type, which comes with a warning.
    Warned,
}

/// what a download of `job` makes of a web page its link answered with, whose server gave it the
/// name `given` once made safe.
pub(in crate::cli) fn page_call(job: &Job, given: Option<&str>) -> PageCall {
    if job.fetch.page {
        return PageCall::Taken;
    }
    let named = given.unwrap_or(&job.label);
    let extension = std::path::Path::new(named)
        .extension()
        .map(|extension| extension.to_string_lossy().to_ascii_lowercase())
        .filter(|extension| !extension.is_empty());
    let names_a_page = matches!(extension.as_deref(), Some("htm" | "html" | "xhtml"));
    // a page that names no file is a sign-in page, a folder listing or a site's page rather than
    // data, unless the link names a page or hashes prove the bytes
    if given.is_none() && !names_a_page && !job.fetch.expect.proves() {
        if extension.is_none() {
            PageCall::Refused
        } else {
            PageCall::ByBytes
        }
    } else if extension.is_some() && !names_a_page {
        PageCall::Warned
    } else {
        PageCall::Taken
    }
}

/// the refusal of a web page `job`'s link answered with where it expected a file, which `named`
/// says the link's name promised, as `data.csv` does, and `slashed` that came from the link with a
/// `/` added, as a folder's listing does: only then is the folder's link offered.
pub(in crate::cli) fn page_refused(job: &Job, named: bool, slashed: bool) -> Error {
    if let Some(refused) = super::share::page_refused(&job.fetch.url) {
        return refused;
    }
    let link = crate::shown::Shown(&job.fetch.url);
    let asked = match &job.dataset {
        Some(dataset) => page_entry(dataset),
        None => PAGE_FLAG.to_owned(),
    };
    if named {
        // a site's sign-in is kept for its host and port, as `auth login` takes them
        let url = &job.fetch.url;
        let host = match url.port() {
            Some(port) => format!("{}:{port}", crate::net::host(url)),
            None => crate::net::host(url).to_owned(),
        };
        let sign_in = crate::account::Account::Login(host).command();
        return Error::new(
            Kind::SourceInvalid,
            format!("{link} answered with a web page, such as a sign-in page, not a data file"),
        )
        .help(format!("open it in a browser: a page that asks you to sign in needs `{sign_in}`, else look for a download or raw link. if the page itself is the data, {asked}"));
    }
    let url = &job.fetch.url;
    if url.path().ends_with('/') || source::folder::forge(url).is_some() {
        return source::folder::no_listing(url, &asked);
    }
    let mut folder = url.clone();
    folder.set_path(&format!("{}/", url.path()));
    let listing = if slashed && url.query().is_none() && url.fragment().is_none() {
        format!(
            "if it is a folder listing, use its link with a / at the end, {}, which fetchloom reads as one dataset. otherwise ",
            crate::shown::Shown(&folder)
        )
    } else {
        String::new()
    };
    Error::new(
        Kind::SourceInvalid,
        format!("{link} answered with a web page, such as a folder listing, not a file"),
    )
    .help(format!("{listing}open it in a browser, copy the link of each file you want, and give fetchloom those links. if the page itself is the data, {asked}"))
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

/// a question a download asks, whose answer every download of the run then shares: a yes to one
/// says nothing of another.
#[derive(Clone, Copy)]
enum Question {
    /// a download larger than `ask_above`.
    Size,
    /// a download the disk holding the cache has no room for.
    Room,
    /// a download whose stated size takes the cache past `cache_budget`.
    Budget,
    /// downloads of no stated size took all of what `cache_budget` leaves the cache.
    Took,
}

/// the answers a run's downloads share once a download asks, the size above which a download
/// asks, and the cache folder whose disk a download needs room on.
struct Approval {
    answers: [OnceLock<Result<(), Error>>; 4],
    /// one question at a time for those asked each time they come up.
    asking: Mutex<()>,
    /// what the downloads of no stated size received so far, which shares the room among them.
    taken: AtomicU64,
    /// what the servers of the downloads of no stated size said they bring, and the room that
    /// takes in the cache, added up, which `above` and the budget judge together.
    stated: AtomicU64,
    stated_room: AtomicU64,
    above: u64,
    cache: Option<std::path::PathBuf>,
    /// what the disk holding a folder has free, which tests give.
    free: fn(&std::path::Path) -> Option<u64>,
    /// what the cache's files hold and the `cache_budget` setting, when one is set.
    budget: Option<(u64, u64)>,
    /// whether `FETCHLOOM_CACHE_BUDGET` sets the budget, so its question says to raise that.
    by_variable: bool,
    /// whether the run has no project, as `get`, so the budget's help names no dataset.
    projectless: bool,
}

impl Approval {
    fn new(above: u64) -> Approval {
        Approval {
            answers: std::array::from_fn(|_| OnceLock::new()),
            asking: Mutex::new(()),
            taken: AtomicU64::new(0),
            stated: AtomicU64::new(0),
            stated_room: AtomicU64::new(0),
            above,
            cache: None,
            free: crate::space::free,
            budget: None,
            by_variable: false,
            projectless: false,
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
    /// disk has `free` and what such downloads `taken` already: the disk, or the budget when that
    /// leaves less. none when neither is known.
    fn unbounded_room(&self, free: Option<u64>, taken: u64) -> Option<(u64, Bound)> {
        let disk = free.map(|free| (free, Bound::Disk));
        let budget = self.budget.map(|(held, budget)| {
            (
                budget.saturating_sub(held).saturating_sub(taken),
                Bound::Budget { budget, held },
            )
        });
        match (disk, budget) {
            (Some(disk), Some(budget)) if budget.0 < disk.0 => Some(budget),
            (Some(disk), _) => Some(disk),
            (None, budget) => budget,
        }
    }
}

/// what ends a download of no stated size: the free room on the cache's disk, or what the
/// `cache_budget` setting leaves the cache past the bytes its files `held` before the download.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Bound {
    Disk,
    Budget { budget: u64, held: u64 },
}

/// the error, or for a person who can answer the question, of a download of no stated size that
/// took `room`, all that `bound` leaves it on the disk holding `cache`, and what to do: the budget's
/// help names the variable that set it, when one did, and no dataset for a run `projectless`.
fn unbounded(
    label: &str,
    (room, bound): (u64, Bound),
    cache: &std::path::Path,
    (by_variable, projectless): (bool, bool),
) -> (String, String) {
    let (took, help) = match bound {
        Bound::Disk => (
            format!(
                "downloading {label} took the {} the disk holding the cache at {} had free",
                units::size(room),
                units::path(cache)
            ),
            "free space on that disk, or set FETCHLOOM_CACHE to a folder on a bigger one, then run the same command again".to_owned(),
        ),
        Bound::Budget { budget, held } if held >= budget => (
            format!(
                "the cache already held {}, past its {} cache_budget, before downloading {label}",
                units::size(held),
                units::size(budget)
            ),
            cache_budget::refused_help(by_variable, projectless),
        ),
        Bound::Budget { budget, .. } if room >= budget => (
            format!(
                "downloading {label} took all of the {} cache_budget",
                units::size(budget)
            ),
            cache_budget::refused_help(by_variable, projectless),
        ),
        Bound::Budget { budget, .. } => (
            format!(
                "downloading {label} took the {} the {} cache_budget leaves the cache",
                units::size(room),
                units::size(budget)
            ),
            cache_budget::refused_help(by_variable, projectless),
        ),
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
    /// unwatched, what bounds that room, `received` then, and what all such downloads had taken
    /// then: what they take from then on, together, stops the download past it, in
    /// [`Watch::room`].
    free: AtomicU64,
    bound: Mutex<Bound>,
    free_from: AtomicU64,
    taken_from: AtomicU64,
    /// what this download added to what the downloads of no stated size took, this try.
    reported: AtomicU64,
    /// what this download's server said it brings, and the room that takes, as this try added
    /// them to the run's sums.
    stated: AtomicU64,
    stated_room: AtomicU64,
    /// what this download holds for the next run. the batch owns it, to say what a stop kept.
    held: &'a Held,
    /// the bytes an earlier run kept, so a start from the first byte can say they were lost.
    earlier: AtomicU64,
    /// whether the download said it starts over, which it says once.
    restarted: AtomicBool,
    /// whether the bytes kept were found no longer as the download left them on this disk, the
    /// reason a start over that follows gives.
    spoiled: AtomicBool,
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
    /// whether a web page came where the link's name promised a data file, as a sign-in page
    /// does, which the file's first bytes decide once in, since some servers call every file one.
    page: AtomicBool,
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
            taken_from: AtomicU64::new(0),
            reported: AtomicU64::new(0),
            stated: AtomicU64::new(0),
            stated_room: AtomicU64::new(0),
            held,
            earlier: AtomicU64::new(0),
            restarted: AtomicBool::new(false),
            spoiled: AtomicBool::new(false),
            counted: AtomicU64::new(0),
            waiting: std::sync::Mutex::new(None),
            encoded: AtomicBool::new(false),
            arrived: AtomicBool::new(false),
            asked: AtomicU64::new(0),
            named: Mutex::new(None),
            page: AtomicBool::new(false),
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

    /// the run's answer to `asked`, asking `question` when no download asked it before.
    fn approve(
        &self,
        asked: Question,
        question: impl FnOnce() -> Result<(), Error>,
    ) -> Result<(), Error> {
        let answer = &self.approval.answers[asked as usize];
        if let Some(answer) = answer.get() {
            return answer.clone();
        }
        self.waiting_for(|| answer.get_or_init(question).clone())
    }

    /// asks `question` whatever the run answered before, one question at a time.
    fn ask_again(&self, question: impl FnOnce() -> Result<(), Error>) -> Result<(), Error> {
        self.waiting_for(|| {
            let _asking = self
                .approval
                .asking
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            question()
        })
    }

    /// what `decide` answers. the time the download waits for it, asking or while another
    /// download asks, is no time it takes.
    fn waiting_for(&self, decide: impl FnOnce() -> Result<(), Error>) -> Result<(), Error> {
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

    /// `fetched` as [`Watch::answered`] tells it, unless it came as a web page where the link's
    /// name promised a data file and its first bytes in `cache` read as one: a server that calls a
    /// csv a web page sends the csv.
    fn checked(&self, cache: &Cache, fetched: Fetched) -> Result<Fetched, Error> {
        let fetched = self.answered(fetched);
        if !self.page.load(Ordering::Relaxed) {
            return Ok(fetched);
        }
        let mut head = Vec::with_capacity(HEAD);
        if let Some(path) = cache.find(&fetched.digest, Some(fetched.size))
            && let Ok(file) = std::fs::File::open(path)
        {
            let _ =
                std::io::Read::read_to_end(&mut std::io::Read::take(file, HEAD as u64), &mut head);
        }
        if reads_as_html(&head) {
            return Err(self.page_refused(true, false));
        }
        Ok(fetched)
    }

    /// says once that the download starts over from `resumed`, losing bytes an earlier run or try
    /// held, and why, from what the try before allowed and the new answer's `resumable` allows.
    fn restarting(&self, resumed: u64, resumable: Resumable) {
        let earlier = self.earlier.load(Ordering::Relaxed);
        let held = self.held.bytes.load(Ordering::Relaxed);
        let spoiled = self.spoiled.swap(false, Ordering::Relaxed);
        let lost = if resumed >= held {
            return;
        } else if held <= earlier {
            // a resume goes on from the prefix, fetching again any kept range that no longer
            // hashes as recorded: no start over
            if resumed > 0 {
                return;
            }
            let why = if spoiled {
                format!(", since {SPOILED}")
            } else {
                String::new()
            };
            format!(
                "the {} an earlier run kept cannot be continued{why}",
                units::size(earlier)
            )
        } else {
            let why = match (self.held.resumable(), resumable) {
                _ if spoiled => SPOILED.to_owned(),
                (Resumable::Unproven(why), _) => format!(
                    "nothing shows they are the same file: {}",
                    unnamed(why, "the server")
                ),
                (Resumable::No, _) => "the server cannot continue from them".to_owned(),
                (_, Resumable::No | Resumable::Unproven(_)) => {
                    "the server sent the whole file again instead of the rest".to_owned()
                }
                _ => "the file changed on the server".to_owned(),
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

    /// says once that what the download holds cannot be continued, since its link is gone, and
    /// promises none of it to a later run, which gets the file from its new link.
    fn moved(&self) {
        let held = self.held.bytes.load(Ordering::Relaxed);
        if held > 0 && !self.restarted.swap(true, Ordering::Relaxed) {
            self.output.mark(
                "Restarting",
                &format!(
                    "{} · the {} downloaded so far cannot be continued, since its link is gone",
                    self.job.label,
                    units::size(held)
                ),
            );
        }
        self.held.keep(0, Resumable::Maybe);
        self.earlier.store(0, Ordering::Relaxed);
    }

    /// stops a download of no stated size once `received` passes the room it may take, before
    /// writing the bytes that just arrived, so the disk is left near full and the budget kept. a
    /// person at a terminal may let it go on. --yes goes on past the budget, as a yes there does,
    /// and the disk's room still bounds it; at the disk, with --yes or with no one to ask, nothing
    /// else would end a body that never ends, so it stops.
    fn room(&self, received: u64) -> Result<(), Error> {
        let room = self.free.load(Ordering::Relaxed);
        let arrived = received.saturating_sub(self.free_from.load(Ordering::Relaxed));
        let Some(cache) = self.approval.cache.as_deref() else {
            return Ok(());
        };
        if room == u64::MAX {
            return Ok(());
        }
        let taken = self.taking(arrived);
        if crate::space::short(taken, Some(room)).is_none() {
            return Ok(());
        }
        let bound = *self.bound.lock().unwrap_or_else(PoisonError::into_inner);
        let budgeted = matches!(bound, Bound::Budget { .. });
        let told = (self.approval.by_variable, self.approval.projectless);
        let (took, help) = unbounded(&self.job.label, (room, bound), cache, told);
        if budgeted && self.yes {
            self.output.warn_with(
                Warning::CacheOverBudget,
                &format!("{took}; --yes goes on while the disk has room"),
                &cache_budget::freeing_help(told.0, told.1),
            );
            self.on_the_disk(taken, cache);
            return Ok(());
        }
        // the budget is a question no one answered, as when the size is known; the disk is full
        let kind = if budgeted {
            Kind::Unconfirmed
        } else {
            Kind::LocalIo
        };
        let stop = || Err(Error::new(kind, took.clone()).help(help.clone()));
        if self.yes || !self.output.can_ask() {
            return stop();
        }
        let label = &self.job.label;
        let question = || {
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
        };
        // a full disk is full again each time a download reaches it, so it asks each time
        if budgeted {
            self.approve(Question::Took, question)?;
        } else {
            self.ask_again(question)?;
        }
        self.on_the_disk(taken, cache);
        Ok(())
    }

    /// counts this try's `arrived` bytes in what the downloads of no stated size took, and says
    /// what they took together since this download began.
    fn taking(&self, arrived: u64) -> u64 {
        let before = self.reported.swap(arrived, Ordering::Relaxed);
        let added = arrived.saturating_sub(before);
        let taken = self.approval.taken.fetch_add(added, Ordering::Relaxed) + added;
        taken.saturating_sub(self.taken_from.load(Ordering::Relaxed))
    }

    /// once a download of no stated size may go past its room, the room the disk holding `cache`
    /// has free now bounds it, past the `taken` bytes the downloads of no stated size took since
    /// it began.
    fn on_the_disk(&self, taken: u64, cache: &std::path::Path) {
        let free = (self.approval.free)(cache);
        *self.bound.lock().unwrap_or_else(PoisonError::into_inner) = Bound::Disk;
        self.free.store(
            free.map_or(u64::MAX, |free| taken.saturating_add(free)),
            Ordering::Relaxed,
        );
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
        // the size a listing rounds was asked about before the downloads, as a known one is
        let asked = matches!(self.job.fetch.listed, Listed::About(_));
        let free = self.approval.cache.as_deref().map(self.approval.free);
        // the sizes only servers state count together, as the sizes a run knows do
        let together = match total.filter(|_| unknown) {
            Some(total) => self.together(
                if asked { 0 } else { left },
                room_needed(Some(total), 0, self.job.fetch.unpacks()),
            ),
            None => (left, 0),
        };
        // a download that may have found its bytes unchanged was not asked about before
        if unknown || self.job.fetch.held.is_some() {
            let needed = room_needed(total, resumed, unknown && self.job.fetch.unpacks());
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
                self.approve(Question::Room, || {
                    super::ask_room(self.output, &doing, needed, free, cache)
                })?;
            } else if unknown && let Some((past, doing)) = self.past_budget(total, together.1) {
                self.approve(Question::Budget, || self.ask_budget(past, &doing))?;
            } else if !asked && left > self.approval.above {
                let size = units::size(left);
                self.approve(Question::Size, || {
                    super::ask(
                        self.output,
                        self.yes,
                        &self.job.label,
                        &size,
                        self.approval.above,
                    )
                })?;
            } else if !asked && together.0 > self.approval.above {
                let what = self.overall.told_as(&self.label());
                let size = format!("more than {}", units::size(self.approval.above));
                self.approve(Question::Size, || {
                    super::ask(self.output, self.yes, &what, &size, self.approval.above)
                })?;
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
            self.unbounded
                .store(total.is_none() && !asked, Ordering::Relaxed);
            self.received.store(resumed, Ordering::Relaxed);
            // a try that starts over takes again what its last try took
            let mine = self.reported.swap(0, Ordering::Relaxed);
            let taken = self
                .approval
                .taken
                .fetch_sub(mine, Ordering::Relaxed)
                .saturating_sub(mine);
            self.taken_from.store(taken, Ordering::Relaxed);
            // with --yes too: a body of no stated size has no end but this
            let watched = self
                .approval
                .unbounded_room(free.flatten(), taken)
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
    /// a partial download is no cached file, and what does: the download alone, or the run's
    /// downloads of no stated size together, which need `together`. none within it, or when the
    /// server states no size.
    fn past_budget(
        &self,
        total: Option<u64>,
        together: u64,
    ) -> Option<(cache_budget::Past, String)> {
        let (held, budget) = self.approval.budget?;
        let adding = room_needed(Some(total?), 0, self.job.fetch.unpacks());
        if let Some(past) = cache_budget::weigh(Some(budget), adding, || Some(held)) {
            return Some((past, self.label()));
        }
        cache_budget::weigh(Some(budget), together, || Some(held))
            .map(|past| (past, self.overall.told_as(&self.label())))
    }

    /// counts `bytes` and `room`, what this download's server said it brings and the room that
    /// takes, in the run's sums for downloads of no stated size, in place of what its last try
    /// counted, and says the sums.
    fn together(&self, bytes: u64, room: u64) -> (u64, u64) {
        let count = |mine: &AtomicU64, sum: &AtomicU64, now: u64| {
            sum.fetch_sub(mine.swap(now, Ordering::Relaxed), Ordering::Relaxed);
            sum.fetch_add(now, Ordering::Relaxed).saturating_add(now)
        };
        (
            count(&self.stated, &self.approval.stated, bytes),
            count(&self.stated_room, &self.approval.stated_room, room),
        )
    }

    /// the question sync and get ask before downloads they know the size of would take the cache
    /// `past` its budget, here once the server states the size of what `what` names.
    fn ask_budget(&self, past: cache_budget::Past, what: &str) -> Result<(), Error> {
        let doing = format!("downloading {what}");
        let over = cache_budget::Over {
            doing: &doing,
            past,
            freeable: None,
            largest: Vec::new(),
            by_variable: self.approval.by_variable,
            projectless: self.approval.projectless,
        };
        cache_budget::ask(self.output, self.yes, &over)
    }

    /// the refusal of a web page the job's link answered with where it expected a file, which
    /// `named` says the link's name promised, as `data.csv` does, and `slashed` that came from the
    /// link with a `/` added.
    fn page_refused(&self, named: bool, slashed: bool) -> Error {
        page_refused(self.job, named, slashed)
    }

    fn on(&self, event: &Event) -> Result<(), Error> {
        let label = &self.job.label;
        match *event {
            Event::Waiting => {
                let what = format!("{label} · another fetchloom is downloading it");
                self.bar().message("Waiting", what.clone());
                *self.waited() = Some(self.output.ticker("Waiting", what, [None]));
            }
            Event::Lockless => self.held.lockless.store(true, Ordering::Relaxed),
            Event::Spoiled => self.spoiled.store(true, Ordering::Relaxed),
            Event::Moved => self.moved(),
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
            Event::WebPage { ref name, slashed } => {
                let given = name.as_deref().and_then(crate::name::sanitize);
                match page_call(self.job, given.as_deref()) {
                    PageCall::Taken => {}
                    PageCall::Refused => return Err(self.page_refused(false, slashed)),
                    PageCall::ByBytes => self.page.store(true, Ordering::Relaxed),
                    PageCall::Warned => {
                        let named = given.as_deref().unwrap_or(label);
                        self.output.warn_with(Warning::DownloadWebPage, &format!("{named} came back as a web page, not a data file. the link may open a page instead of downloading"), "look for a download or raw link");
                    }
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
                open,
            } => self.bar().retrying((next, of, open), wait, limit.clone()),
            Event::Behind(behind) => self.bar().behind(behind),
            Event::Bytes(bytes) => {
                self.bar().advance(bytes);
                self.count(self.held.bytes.fetch_add(bytes, Ordering::Relaxed) + bytes);
                let received = self.received.fetch_add(bytes, Ordering::Relaxed) + bytes;
                if self.unbounded.load(Ordering::Relaxed) && received > self.approval.above {
                    let size = format!("more than {}", units::size(self.approval.above));
                    self.approve(Question::Size, || {
                        super::ask(
                            self.output,
                            self.yes,
                            &self.job.label,
                            &size,
                            self.approval.above,
                        )
                    })?;
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

/// how long the downloads `jobs`, each a host and what is known of its size, should take, added
/// up by pacing name from each host's remembered speed, its source's pacing and the run's job
/// limits.
pub(super) fn planned<S: Into<estimate::Size>>(
    context: &Context,
    jobs: impl IntoIterator<Item = (String, S)>,
) -> estimate::Total {
    let hosts: Vec<estimate::Host> = hosts(context, jobs).into_values().collect();
    estimate::Total::of(&hosts, limits(context))
}

/// [`planned`]'s downloads by the name each host is paced under, before they are added up.
fn hosts<S: Into<estimate::Size>>(
    context: &Context,
    jobs: impl IntoIterator<Item = (String, S)>,
) -> BTreeMap<String, estimate::Host> {
    let mut hosts: BTreeMap<String, estimate::Host> = BTreeMap::new();
    for (host, size) in jobs {
        let planned = hosts.entry(source::paced_as(&host)).or_default();
        *planned.files.get_or_insert(0) += 1;
        match size.into() {
            estimate::Size::Bytes(size) => add_size(planned, size),
            estimate::Size::About(size) => {
                add_size(planned, size);
                planned.rounded += 1;
            }
            estimate::Size::Unknown => planned.unknown += 1,
        }
    }
    for (name, host) in &mut hosts {
        host.speed = context.speeds.speed(name);
        host.spacing = source::access::spacing(name);
        host.most = source::access::most_at_once(name);
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

/// the run's limits on downloads at once, and on the bytes a second they take together.
pub(super) fn limits(context: &Context) -> estimate::Limits {
    estimate::Limits {
        jobs: context.settings.jobs.value,
        host_jobs: context.settings.host_jobs.value,
        rate: context.settings.rate.value,
    }
}

/// the host a job's requests start at, as its downloads are queued and its speed remembered.
pub(super) fn host_of(job: &Job) -> String {
    crate::net::host(&job.fetch.from).to_ascii_lowercase()
}

/// what is known of the size of a job's file before its download: the size it expects, or about
/// what a folder listing rounds it to.
pub(super) fn size_of(job: &Job) -> estimate::Size {
    estimate::Size::of(job.fetch.expect.size, job.fetch.listed)
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

/// what a download is called before it starts: its label, or for a file that takes the name its
/// server gives where its link names no file, as a drive link's `download` does, its folder and
/// that its server names it, since only the download learns how.
pub(super) fn foreseen(job: &Job) -> String {
    let named_by_link = std::path::Path::new(&job.label).extension().is_some();
    if !job.fetch.named || named_by_link {
        return job.label.clone();
    }
    match job.label.rsplit_once('/') {
        Some((folder, _)) => format!("{folder}/ · named by its server"),
        None => "a file its server names".to_owned(),
    }
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

/// the job at `index`, its fetch carrying the name its progress line shows, which retry lines
/// then name the file by.
fn labelled((index, mut job): (usize, Job)) -> (usize, Job) {
    job.fetch.label = Some(job.label.clone());
    (index, job)
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
    let mut approval = Approval::new(context.settings.ask_above.value).with_room(cache.root());
    approval.projectless = context.projectless;
    let approval = Arc::new(budgeted(context, cache, approval, overall.sizes.1 > 0).await);
    let listings = again::Listings::default();
    // what each unfinished download holds, which a stop keeps for the next run
    let held: Arc<[Held]> = std::iter::repeat_with(Held::default).take(count).collect();
    let mut labels = vec![String::new(); count];
    loop {
        while tasks.len() < in_flight
            && let Some((index, job)) = queue.next().map(labelled)
        {
            labels[index].clone_from(&job.label);
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
            let damaged = context.mirrors.damaged_folder(job.fetch.expect.sha256);
            let (archived, only) = (context.archived.clone(), context.mirrors.mirrors.only);
            tasks.spawn(async move {
                let watch = Watch::new(&job, &output, &overall, &approval, yes, &held[index]);
                // with no bar for them all, a lone download shows its bar while the server
                // takes its time to answer
                if alone {
                    watch.bar();
                }
                let on = |event: Event| watch.on(&event);
                let recovery = Recovery {
                    listings: &listings,
                    archived: &archived,
                    only,
                };
                let fetched = recovering(&net, &cache, &output, recovery, &job, &on).await;
                watch.ended();
                let fetched = fetched.and_then(|found| watch.checked(&cache, found));
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
                (index, fetched.map_err(|err| explain(err, &job, damaged)))
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
            Ok((index, Err(err))) => {
                let failed = (index, err);
                return Err(ended(context, &mut tasks, &held, &labels, &mut results, failed).await);
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

/// where a run looks for a pinned file its recorded link no longer gives.
struct Recovery<'a> {
    listings: &'a again::Listings,
    archived: &'a archived::Archived,
    /// mirror-only mode, which asks no archive.
    only: bool,
}

/// `job`'s file: downloaded from its link, else from where its source lists it now, else from a
/// web archive.
async fn recovering(
    net: &Net,
    cache: &Cache,
    output: &Output,
    recovery: Recovery<'_>,
    job: &Job,
    on: &(dyn Fn(Event) -> Result<(), Error> + Sync),
) -> Result<Fetched, Error> {
    let Recovery {
        listings,
        archived,
        only,
    } = recovery;
    let failed = match net.download(&job.fetch, cache, on).await {
        Err(err) => again::recover(net, cache, output, listings, job, err, on).await,
        fetched => return fetched,
    };
    match failed {
        Err(err) => archived::recover(net, cache, output, archived, only, job, err, on).await,
        fetched => fetched,
    }
}

/// ends every download still running for `failed`, a job's failure no other job goes on after: a
/// stop winds them down, keeping what each holds for the next run, and any other failure aborts
/// them and is joined with those of `results` that failed on their own before it.
async fn ended(
    context: &Context,
    tasks: &mut JoinSet<(usize, Result<Fetched, Error>)>,
    held: &[Held],
    labels: &[String],
    results: &mut [Option<Result<Fetched, Error>>],
    (index, failed): (usize, Error),
) -> Error {
    if failed.kind == Kind::Interrupted {
        // a ctrl-c read as a key at a question reaches no signal handler, so the stop is asked
        // here, and each download winds down keeping what this count promised
        context.stop.ask();
        wind_down(tasks, held).await;
        return kept(failed, held);
    }
    tasks.abort_all();
    ended_by(labels, results, (index, failed), held)
}

/// the error for `failed`, the failure of the job at `index` that stopped every download, joined
/// with those of `results` that failed on their own before it. the joined error takes the first
/// failure's kind, while the downloads a network failure stopped keep what `held` says.
fn ended_by(
    labels: &[String],
    results: &mut [Option<Result<Fetched, Error>>],
    (index, failed): (usize, Error),
    held: &[Held],
) -> Error {
    let network = failed.kind == Kind::NetworkUnreachable;
    let joined = stopped(labels, results, index, failed);
    if network {
        keeping(joined, held)
    } else {
        joined
    }
}

/// the error for `stopping`, the failure of the job at `index` that stops every download, joined
/// with those of `results` that failed on their own before it, so each keeps its line and the
/// first failure in the jobs' order gives the kind and exit code.
fn stopped(
    labels: &[String],
    results: &mut [Option<Result<Fetched, Error>>],
    index: usize,
    stopping: Error,
) -> Error {
    let mut failed: Vec<(usize, Error)> = results
        .iter_mut()
        .enumerate()
        .filter_map(|(at, result)| match result.take() {
            Some(Err(error)) => Some((at, error)),
            _ => None,
        })
        .collect();
    failed.push((index, stopping));
    together(labels, failed).unwrap_or_else(|| Error::new(Kind::LocalIo, "the downloads stopped"))
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
    /// whether the cache's disk cannot lock files, so a stop deletes the bytes whatever the
    /// server allows: no later run could find them.
    lockless: AtomicBool,
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
    keeping(error, held)
}

/// `error` with the lines [`kept`] tells, whatever its kind.
fn keeping(error: Error, held: &[Held]) -> Error {
    let (mut sure, mut maybe, mut lost, mut unproven) = (0_u64, 0_u64, 0_u64, 0_u64);
    let mut lockless = 0_u64;
    let mut reasons = Vec::new();
    for held in held {
        let bytes = held.bytes.load(Ordering::Relaxed);
        if held.lockless.load(Ordering::Relaxed) {
            lockless = lockless.saturating_add(bytes);
            continue;
        }
        let sum = match held.resumable() {
            Resumable::Yes => &mut sure,
            Resumable::Maybe => &mut maybe,
            Resumable::No => &mut lost,
            Resumable::Unproven(why) => {
                if bytes > 0 {
                    reasons.push(why);
                }
                &mut unproven
            }
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
    let unproven_why = |server: &str| {
        let why = match reasons.as_slice() {
            [first, rest @ ..] if rest.iter().all(|why| why == first) => unnamed(*first, server),
            _ => "their servers named no version a resume can check".to_owned(),
        };
        format!("nothing shows it is the same file: {why}")
    };
    let deleted = [
        (
            lost,
            "the server cannot continue from it".to_owned(),
            "its server cannot continue from it".to_owned(),
        ),
        (
            unproven,
            unproven_why("the server"),
            unproven_why("its server"),
        ),
        (lockless, LOCKLESS.to_owned(), LOCKLESS.to_owned()),
    ];
    let mut told = going_on > 0;
    let mut error = error;
    for (bytes, alone, other) in deleted.into_iter().filter(|(bytes, ..)| *bytes > 0) {
        error = error.detail(if told {
            format!("the other {} is deleted, since {other}", units::size(bytes))
        } else {
            format!(
                "the {} downloaded so far is deleted, since {alone}",
                units::size(bytes)
            )
        });
        told = true;
    }
    error
}

/// why a download starts over from bytes it finds no longer as it left them on this disk.
const SPOILED: &str = "they are no longer as the download left them on this disk";

/// why a stop deletes the bytes downloads kept in a cache on a disk that cannot lock files.
const LOCKLESS: &str = "the cache's disk cannot lock files, so no later run could find it";

/// what `server` sent that names no version a request for the rest of a file may name.
fn unnamed(why: NoVersion, server: &str) -> String {
    match why {
        NoVersion::Unsent => format!("{server} sent no ETag or Last-Modified"),
        NoVersion::WeakTag => {
            format!("{server}'s ETag is weak, which cannot vouch for part of a file")
        }
        NoVersion::BadTag => format!("{server}'s ETag is not a quoted tag"),
        NoVersion::FreshDate => format!(
            "{server}'s Last-Modified is less than a minute before its Date, too close to tell versions apart"
        ),
        NoVersion::Undated => {
            format!(
                "{server}'s Last-Modified comes with no Date fetchloom can read to tell its age"
            )
        }
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
/// it. a pinned link with nothing at it now may still be on such a mirror. `damaged` is a folder
/// mirror this run found a damaged copy of the file on.
fn explain(error: Error, job: &Job, damaged: Option<std::path::PathBuf>) -> Error {
    let at_commit = error
        .help
        .as_deref()
        .is_some_and(crate::source::is_unserved);
    if let Some(again) = &job.again
        && (at_commit || error.help.as_deref() == Some(crate::net::MOVED_HELP))
    {
        // a new link as ref would make a record of many files that one file, and a link at a
        // commit never moves
        let on = match &job.dataset {
            Some(dataset) if !again.alone || at_commit => format!(
                "to pin the files its source lists now instead, run `{}`",
                super::project::command(&format!("lock --upgrade {dataset}"))
            ),
            _ => "if the file moved, put its new link as ref in data.toml".to_owned(),
        };
        return error.help(format!("{} {on}", keep_pinned(damaged)));
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
        help: Some(changed_upstream(dataset, damaged).into()),
        ..error
    }
}

/// how pinned bytes a source no longer serves are kept: pushed to a folder mirror, or, when
/// `damaged` is a folder mirror holding a damaged copy, pushed there by a push that checks the
/// copies it finds, since a plain push leaves one of the pinned size as it is.
fn keep_pinned(damaged: Option<std::path::PathBuf>) -> String {
    match damaged {
        Some(root) => format!(
            "to keep the pinned bytes, someone who has them runs `fetchloom push --verify --to {}`, which replaces the damaged copy there, then you run this command again.",
            crate::suggested::shell_word(&root.display().to_string())
        ),
        None => "to keep the pinned bytes, someone who has them runs `fetchloom push --to <folder>`, then you run this command again with `--mirror <folder>`.".to_owned(),
    }
}

/// what to do about pinned bytes of `dataset` that its source no longer serves.
fn changed_upstream(dataset: &str, damaged: Option<std::path::PathBuf>) -> String {
    format!(
        "{} if the new bytes are intended, run `{}`",
        keep_pinned(damaged),
        super::project::command(&format!("lock --upgrade {dataset}"))
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
                rate: None,
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
            ..estimate::Total::default()
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
                ..estimate::Total::default()
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
            Some(
                "fetchloom asks before a download over 10.0 GiB, its ask_above setting: add --yes to download it without being asked"
            )
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
        let budget_help = crate::cli::sync::cache_budget::refused_help(false, false);
        for (budget, yes, room, kind, message, help) in [
            (
                None,
                true,
                1000,
                Kind::LocalIo,
                "downloading big.bin took the 1000 B the disk holding the cache at /cache had free, and its server never said its size",
                "free space on that disk, or set FETCHLOOM_CACHE to a folder on a bigger one, then run the same command again",
            ),
            (
                Some((9000, 8500)),
                false,
                500,
                Kind::Unconfirmed,
                "downloading big.bin took the 500 B the 8.8 KiB cache_budget leaves the cache, and its server never said its size",
                budget_help.as_str(),
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
            assert_eq!(stopped.kind, kind);
            assert_eq!(stopped.message, message);
            assert_eq!(stopped.help.as_deref(), Some(help));
            let sized = Watch::new(&job, &output, &overall, &approval, yes, &held);
            sized.on(&started(Some(10))).unwrap();
            sized.on(&Event::Bytes(2000)).unwrap();
        }
        // --yes goes past the budget as a yes at a terminal does, and the 1,000 bytes the disk
        // had free when it did still end the download
        let mut approval = Approval::new(crate::settings::ASK_ABOVE)
            .with_room(cache)
            .with_budget(9000, 8500);
        approval.free = |_| Some(1000);
        let watch = Watch::new(&job, &output, &overall, &approval, true, &held);
        watch.on(&started(None)).unwrap();
        watch.on(&Event::Bytes(501)).unwrap();
        watch.on(&Event::Bytes(1000)).unwrap();
        let stopped = watch.on(&Event::Bytes(1)).unwrap_err();
        assert_eq!(stopped.kind, Kind::LocalIo);
        assert_eq!(
            stopped.message,
            "downloading big.bin took the 1.5 KiB the disk holding the cache at /cache had free, and its server never said its size"
        );
    }

    /// downloads of no stated size at once share the room the budget or the disk leaves, rather
    /// than each taking all of it; a try that starts over takes back what its last try took.
    #[test]
    fn downloads_of_no_stated_size_share_their_room() {
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
        let (one, two) = (job("one.bin"), job("two.bin"));
        let overall = overall();
        let held = Held::default();
        let unknown = |resumed| Event::Started {
            total: None,
            resumed,
            resumable: Resumable::No,
        };
        let cache = std::path::Path::new("/cache");
        for budget in [Some((1000, 0)), None] {
            let mut approval = Approval::new(crate::settings::ASK_ABOVE).with_room(cache);
            approval.free = |_| Some(1000);
            if let Some((budget, held)) = budget {
                approval = approval.with_budget(budget, held);
            }
            let first = Watch::new(&one, &output, &overall, &approval, false, &held);
            let second = Watch::new(&two, &output, &overall, &approval, false, &held);
            first.on(&unknown(0)).unwrap();
            second.on(&unknown(0)).unwrap();
            first.on(&Event::Bytes(600)).unwrap();
            second.on(&Event::Bytes(400)).unwrap();
            assert!(second.on(&Event::Bytes(1)).is_err(), "{budget:?}");
            if budget.is_none() {
                continue;
            }
            // the first starts over: its 600 bytes no longer count, and its new ones do, beside
            // the 401 the second took
            first.on(&unknown(0)).unwrap();
            first.on(&Event::Bytes(599)).unwrap();
            assert!(first.on(&Event::Bytes(1)).is_err());
        }
    }

    /// a budget an empty cache leaves whole is named once, and the help raises the budget where it
    /// is set: the variable, which wins over the settings file, and `cache gc --now`.
    #[test]
    fn a_stop_at_the_whole_budget_names_it_once() {
        let (said, help) = unbounded(
            "big.bin",
            (
                9000,
                Bound::Budget {
                    budget: 9000,
                    held: 0,
                },
            ),
            std::path::Path::new("/cache"),
            (true, false),
        );
        assert_eq!(
            said,
            "downloading big.bin took all of the 8.8 KiB cache_budget, and its server never said its size"
        );
        assert!(help.contains("`fetchloom cache gc --now`"), "{help}");
        assert!(
            help.contains(", or raise FETCHLOOM_CACHE_BUDGET, then"),
            "{help}"
        );
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

    /// the sizes servers state for a run's downloads of no stated size count together against
    /// `ask_above` and `cache_budget`, as the sizes a sync knows do: three files of 1 MiB at
    /// 2 MiB pass one at a time, and the one that brings them past it asks for them all. a try
    /// that starts over counts its file once.
    #[test]
    fn sizes_only_servers_state_count_together() {
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
        let jobs: Vec<Job> = ["a.bin", "b.bin", "c.bin"]
            .into_iter()
            .map(|name| Job {
                fetch: Fetch::new(Url::parse(&format!("https://example.org/{name}")).unwrap()),
                label: name.to_owned(),
                dataset: None,
                again: None,
            })
            .collect();
        let mut overall = overall();
        overall.what = "3 files".to_owned();
        let held = Held::default();
        let mib = Event::Started {
            total: Some(1 << 20),
            resumed: 0,
            resumable: Resumable::Maybe,
        };
        for (approval, message) in [
            (
                Approval::new(2 << 20),
                "downloading 3 files needs more than 2.0 MiB, and fetchloom cannot ask you here",
            ),
            (
                Approval::new(crate::settings::ASK_ABOVE).with_budget(2 << 20, 0),
                "downloading 3 files would bring the cache to 3.0 MiB, past the 2.0 MiB cache_budget",
            ),
        ] {
            let watches: Vec<Watch> = jobs
                .iter()
                .map(|job| Watch::new(job, &output, &overall, &approval, false, &held))
                .collect();
            watches[0].on(&mib).unwrap();
            watches[0].on(&mib).unwrap();
            watches[1].on(&mib).unwrap();
            let stopped = watches[2].on(&mib).unwrap_err();
            assert_eq!(stopped.kind, Kind::Unconfirmed);
            assert_eq!(stopped.message, message);
        }
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

    /// a size a folder listing rounds was asked about with the run's others before any download
    /// began, so what its server states, or sends with no size stated, asks nothing again, while
    /// `cache_budget` still judges what the server states.
    #[test]
    fn a_rounded_size_asked_about_before_asks_nothing_again() {
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
        let started = |total| Event::Started {
            total,
            resumed: 0,
            resumable: Resumable::Maybe,
        };
        let (overall, held) = (overall(), Held::default());
        let watch_with = |job: &Job, approval: &Approval| {
            let watch = Watch::new(job, &output, &overall, approval, false, &held);
            watch
                .on(&started(Some(2 << 20)))
                .and_then(|()| watch.on(&started(None)))
                .and_then(|()| watch.on(&Event::Bytes(2 << 20)))
        };
        let above = Approval::new(1 << 20);
        assert_eq!(
            watch_with(&job, &above).unwrap_err().kind,
            Kind::Unconfirmed
        );
        job.fetch.listed = Listed::About(2 << 20);
        let asked = Approval::new(1 << 20);
        watch_with(&job, &asked).unwrap();
        // nor does it count among the sizes only servers state, which a file beside it asks about
        let unlisted = Job {
            fetch: Fetch::new(Url::parse("https://example.org/small.bin").unwrap()),
            label: "small.bin".to_owned(),
            dataset: None,
            again: None,
        };
        Watch::new(&unlisted, &output, &overall, &asked, false, &held)
            .on(&started(Some(1 << 19)))
            .unwrap();
        let budget = Approval::new(crate::settings::ASK_ABOVE).with_budget(1 << 20, 0);
        let stopped = watch_with(&job, &budget).unwrap_err();
        assert!(
            stopped.message.contains("cache_budget"),
            "{}",
            stopped.message
        );
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

    /// before it starts, a file its server names is told by its folder alone, since only the
    /// download learns its name, as a dry run of `add --as` shows it.
    #[test]
    fn a_download_its_server_names_is_foreseen_by_its_folder() {
        let job = |named, label: &str| Job {
            fetch: Fetch {
                named,
                ..Fetch::new(Url::parse("https://drive.google.com/uc?id=0B9P1L").unwrap())
            },
            label: label.to_owned(),
            dataset: None,
            again: None,
        };
        assert_eq!(
            foreseen(&job(true, "notes2/download")),
            "notes2/ · named by its server"
        );
        assert_eq!(foreseen(&job(true, "download")), "a file its server names");
        assert_eq!(foreseen(&job(false, "notes2/download")), "notes2/download");
        // a link that names its file is the likely name, which a server seldom changes
        assert_eq!(foreseen(&job(true, "iris/iris.csv")), "iris/iris.csv");
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

    /// a download whose link is gone says once that what it holds cannot be continued, and a stop
    /// then promises none of it, since a later run gets the file from its new link.
    #[test]
    fn a_link_gone_promises_its_bytes_to_no_later_run() {
        let job = Job {
            fetch: Fetch::new(Url::parse("https://example.org/gone.csv").unwrap()),
            label: "iris/data.csv".to_owned(),
            dataset: None,
            again: None,
        };
        let output = Output::new(
            Style {
                color: false,
                unicode: false,
                interactive: false,
                watched: false,
                links: false,
                accessible: false,
            },
            Level::Normal,
            false,
            false,
        );
        let (overall, approval, held) = (overall(), Approval::new(u64::MAX), Held::default());
        let watch = Watch::new(&job, &output, &overall, &approval, true, &held);
        let earlier = Event::Kept {
            bytes: 1700,
            resumable: Resumable::Maybe,
        };
        watch.on(&earlier).unwrap();
        watch.on(&Event::Moved).unwrap();
        let restarting: Vec<String> = output
            .said()
            .into_iter()
            .filter(|line| line.contains("Restarting"))
            .collect();
        assert_eq!(
            restarting,
            [
                " Restarting iris/data.csv - the 1.7 KiB downloaded so far cannot be continued, since its link is gone"
            ]
        );
        drop(watch);
        let stopped = kept(Error::new(Kind::Interrupted, "stopped"), &[held]);
        assert!(stopped.details.is_empty(), "{:?}", stopped.details);
    }

    /// a resume that drops a kept range that no longer hashes as recorded still goes on from the
    /// bytes before it, so it says no start over; one from the first byte does.
    #[test]
    fn only_a_start_from_the_first_byte_over_kept_bytes_says_restarting() {
        let job = Job {
            fetch: Fetch::new(Url::parse("https://example.org/big.bin").unwrap()),
            label: "big/big.bin".to_owned(),
            dataset: None,
            again: None,
        };
        for (resumed, said) in [
            (1000, Vec::<String>::new()),
            (
                0,
                vec![
                    " Restarting big/big.bin - the 1.7 KiB an earlier run kept cannot be continued"
                        .to_owned(),
                ],
            ),
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
                Level::Normal,
                false,
                false,
            );
            let (overall, approval, held) = (overall(), Approval::new(u64::MAX), Held::default());
            let watch = Watch::new(&job, &output, &overall, &approval, true, &held);
            let kept = Event::Kept {
                bytes: 1700,
                resumable: Resumable::Maybe,
            };
            watch.on(&kept).unwrap();
            let started = Event::Started {
                total: Some(3000),
                resumed,
                resumable: Resumable::Yes,
            };
            watch.on(&started).unwrap();
            let restarting: Vec<String> = output
                .said()
                .into_iter()
                .filter(|line| line.contains("Restarting"))
                .collect();
            assert_eq!(restarting, said, "resumed at {resumed}");
        }
    }

    /// output that tells nothing, for tests of what a download decides.
    fn quiet() -> Output {
        let style = Style {
            color: false,
            unicode: false,
            interactive: false,
            watched: false,
            links: false,
            accessible: false,
        };
        Output::new(style, Level::Quiet, false, false)
    }

    /// what a job for `link`, shown as `label` and pinned to `sha256` when given, makes of a web
    /// page answering it that names the file `name`.
    fn on_page(
        link: &str,
        label: &str,
        (name, slashed): (Option<&str>, bool),
        sha256: Option<crate::digest::Sha256>,
    ) -> Result<(), Error> {
        let output = quiet();
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
            slashed,
        })
    }

    /// a web page where the link's name promised a data file is refused once its first bytes read
    /// as html, as a sign-in page's do; a csv a server called a web page is kept.
    #[test]
    fn a_page_named_as_data_is_judged_by_its_bytes() {
        let output = quiet();
        let (overall, held) = (overall(), Held::default());
        let approval = Approval::new(crate::settings::ASK_ABOVE);
        let folder = tempfile::tempdir().unwrap();
        let cache = Cache::open(folder.path().join("cache")).unwrap();
        let job = Job {
            fetch: Fetch::new(Url::parse("http://lab.example/data.csv").unwrap()),
            label: "data.csv".to_owned(),
            dataset: None,
            again: None,
        };
        for (bytes, refused) in [
            (
                &b"\xef\xbb\xbf\n  <!DOCTYPE html><title>Sign in</title>"[..],
                true,
            ),
            (b"<!-- saved --><HTML><form>login</form></HTML>", true),
            (b"<script>window.location='/login'</script>", true),
            (
                b"  <META http-equiv='refresh' content='0;url=/login'>",
                true,
            ),
            (b"<head><title>Sign in</title>", true),
            (b"<body onload='go()'>", true),
            (b"<title>Sign in</title>", true),
            (b"<!-- sso -->\n<script src='/sso.js'></script>", true),
            (
                b"\xff\xfe<\0!\0D\0O\0C\0T\0Y\0P\0E\0 \0h\0t\0m\0l\0>\0",
                true,
            ),
            (b"\xfe\xff\0<\0h\0t\0m\0l\0>", true),
            (b"<form action='/login' method='post'>", true),
            (b"<div class='login'>", true),
            (b"<p>Please sign in</p>", true),
            (b"<h1>Sign in</h1>", true),
            (b"<H3>Sign in</H3>", true),
            (b"<iframe src='/sso'></iframe>", true),
            (b"<noscript>enable scripts</noscript>", true),
            (b"<style>body{}</style>", true),
            (b"<link rel='stylesheet' href='/a.css'>", true),
            (b"<svg xmlns='http://www.w3.org/2000/svg'>", true),
            (
                b"\xff\xfe\0\0<\0\0\0h\0\0\0t\0\0\0m\0\0\0l\0\0\0>\0\0\0",
                true,
            ),
            (b"\0\0\xfe\xff\0\0\0<\0\0\0p\0\0\0>", true),
            (b"<pre>a,b</pre>", false),
            (b"<price>1</price>", false),
            (b"a,b\n1,2\n", false),
            (b"<?xml version=\"1.0\"?><rows/>", false),
            (b"<id>,<name>\n1,iris\n", false),
            (b"<header><row/></header>", false),
            (b"\xff\xfea\0,\0b\0\n\0", false),
        ] {
            let stored = crate::cache::tests::store(&cache, bytes);
            let watch = Watch::new(&job, &output, &overall, &approval, false, &held);
            watch
                .on(&Event::WebPage {
                    name: None,
                    slashed: false,
                })
                .unwrap();
            let fetched = Fetched {
                digest: stored.digest,
                size: stored.size,
                moved: stored.size,
                name: None,
                origin: crate::net::Origin::Network,
                version: None,
                began: None,
                arrived: None,
                asked: std::time::Duration::ZERO,
            };
            let shown = String::from_utf8_lossy(bytes);
            match watch.checked(&cache, fetched) {
                Err(error) => {
                    assert!(refused, "{shown:?}");
                    assert_eq!(
                        error.message,
                        "http://lab.example/data.csv answered with a web page, such as a sign-in page, not a data file"
                    );
                    let advice = error.help.unwrap_or_default();
                    assert!(
                        advice.contains("`fetchloom auth login lab.example`"),
                        "{advice}"
                    );
                }
                Ok(_) => assert!(!refused, "{shown:?} was kept"),
            }
        }
    }

    /// a host that would mean more to a shell, which a link's parser lets through, is quoted in
    /// the sign-in a refusal suggests, so pasting it runs nothing.
    #[test]
    fn the_sign_in_a_page_refusal_suggests_quotes_its_host() {
        let output = quiet();
        let (overall, held) = (overall(), Held::default());
        let approval = Approval::new(crate::settings::ASK_ABOVE);
        let job = Job {
            fetch: Fetch::new(Url::parse("http://x$(id).example/data.csv").unwrap()),
            label: "data.csv".to_owned(),
            dataset: None,
            again: None,
        };
        let watch = Watch::new(&job, &output, &overall, &approval, false, &held);
        let advice = watch
            .page_refused(true, false)
            .told()
            .help
            .unwrap_or_default();
        // windows withholds a name its shells would run and shows a placeholder instead
        let suggested = if cfg!(windows) {
            "`fetchloom auth login \"<name>\"`"
        } else {
            "`fetchloom auth login 'x$(id).example'`"
        };
        assert!(advice.contains(suggested), "{advice}");
    }

    /// a page that names no file, at a link that names none either, is a folder listing or a
    /// site's page, refused before its bytes, one at a folder's link as no listing fetchloom
    /// reads, and the folder's own link, ending in /, is offered where the server sent the link
    /// on to it, as servers send a folder's link without its `/`; a link
    /// that names a page, a file the server names, pinned bytes, and a link named as data, whose
    /// first bytes decide, are not.
    #[test]
    fn a_web_page_that_names_no_file_is_refused_before_its_bytes() {
        let pinned = Some(crate::digest::sha256(b"a pinned listing"));
        let folder = "if it is a folder listing, use its link with a / at the end, http://lab.example/shards/, which fetchloom reads as one dataset. otherwise open it in a browser";
        let page = "answered with a web page, such as a folder listing, not a file";
        let other = "answered with a web page, not a file or a folder listing fetchloom reads";
        let refused = [
            ("http://lab.example/shards", true, page, true),
            ("http://lab.example/shards", false, page, false),
            ("http://lab.example/shards?page=2", true, page, false),
            ("http://lab.example/shards/", false, other, false),
            ("http://lab.example/", false, other, false),
        ];
        for (link, slashed, said, offered) in refused {
            let label = if link.contains("shards") {
                "shards"
            } else {
                "download"
            };
            let error = on_page(link, label, (None, slashed), None).unwrap_err();
            assert_eq!(error.kind, Kind::SourceInvalid);
            assert_eq!(error.message, format!("{link} {said}"));
            let advice = error.help.unwrap_or_default();
            assert!(
                advice.ends_with("if the page itself is the data, add --web-page"),
                "{advice}"
            );
            assert_eq!(advice.starts_with(folder), offered, "{advice}");
        }
        for (link, label, name, sha256) in [
            (
                "http://lab.example/shards/",
                "shards",
                Some("index.html"),
                None,
            ),
            ("http://lab.example/shards/", "shards", None, pinned),
            ("http://lab.example/data.csv", "data.csv", None, None),
            ("http://lab.example/x.ZIP", "x.ZIP", None, None),
            ("http://lab.example/data.csv", "data.csv", None, pinned),
            (
                "http://lab.example/export",
                "export",
                Some("data.csv"),
                None,
            ),
            ("http://lab.example/page.html", "page.html", None, None),
            ("http://lab.example/page.HTM", "page.HTM", None, None),
        ] {
            assert!(
                on_page(link, label, (name, false), sha256).is_ok(),
                "{link} as {label}"
            );
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
            None,
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
            explain(gone(), &job, None).help.as_deref(),
            Some(crate::net::MOVED_HELP)
        );
        job.again = Some(Again {
            resolved: "https://example.org/data.csv".to_owned(),
            path: "data.csv".to_owned(),
            alone: true,
            changed: String::new(),
        });
        assert_eq!(
            explain(gone(), &job, None).help.as_deref(),
            Some(
                "to keep the pinned bytes, someone who has them runs `fetchloom push --to <folder>`, then you run this command again with `--mirror <folder>`. if the file moved, put its new link as ref in data.toml"
            )
        );
        // one file of a record of many: its link as ref would make the dataset that file alone
        job.again = Some(Again {
            resolved: "zenodo:3608990".to_owned(),
            path: "data.csv".to_owned(),
            alone: false,
            changed: String::new(),
        });
        assert_eq!(
            explain(gone(), &job, None).help.as_deref(),
            Some(
                "to keep the pinned bytes, someone who has them runs `fetchloom push --to <folder>`, then you run this command again with `--mirror <folder>`. to pin the files its source lists now instead, run `fetchloom lock --upgrade iris`"
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
        let lockless = Held::default();
        lockless.keep(2 * mib, Resumable::Yes);
        lockless.lockless.store(true, Ordering::Relaxed);
        let other = Held::default();
        other.keep(mib, Resumable::Yes);
        assert_eq!(
            kept(Error::new(Kind::Interrupted, "stopped"), &[lockless, other]).details,
            [
                "the 1.0 MiB downloaded so far is kept, and the next run continues from there",
                "the other 2.0 MiB is deleted, since the cache's disk cannot lock files, so no later run could find it"
            ]
        );
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
        assert_eq!(
            said(&[
                (mib, Resumable::No),
                (2 * mib, Resumable::Unproven(NoVersion::Unsent))
            ]),
            [
                "the 1.0 MiB downloaded so far is deleted, since the server cannot continue from it",
                "the other 2.0 MiB is deleted, since nothing shows it is the same file: its server sent no ETag or Last-Modified"
            ]
        );
        assert_eq!(
            said(&[(2 * mib, Resumable::Unproven(NoVersion::WeakTag))]),
            [
                "the 2.0 MiB downloaded so far is deleted, since nothing shows it is the same file: the server's ETag is weak, which cannot vouch for part of a file"
            ]
        );
        assert_eq!(
            said(&[
                (mib, Resumable::Unproven(NoVersion::FreshDate)),
                (mib, Resumable::Unproven(NoVersion::Unsent))
            ]),
            [
                "the 2.0 MiB downloaded so far is deleted, since nothing shows it is the same file: their servers named no version a resume can check"
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

    /// a server that stops answering ends every download, yet a file gone from its source before
    /// it keeps its line, and being first in the files' order, gives the kind and exit code.
    #[test]
    fn a_failure_that_stops_the_downloads_keeps_the_ones_before_it() {
        let labels: Vec<String> = ["able", "dead", "down", "late"]
            .map(|name| format!("{name}/{name}.csv"))
            .into();
        let gone = Error::new(
            Kind::ReferenceUnresolved,
            "the server has nothing at dead.csv",
        );
        let mut results = vec![None, Some(Err(gone)), None, None];
        let silent = Error::new(Kind::NetworkUnreachable, "the server did not answer");
        let held: Vec<Held> = std::iter::repeat_with(Held::default).take(4).collect();
        held[2].keep(3 << 20, Resumable::Yes);
        let error = ended_by(&labels, &mut results, (2, silent), &held);
        assert_eq!(error.kind, Kind::ReferenceUnresolved);
        let failed: Vec<&str> = error
            .failures
            .iter()
            .map(|failure| failure.file.as_str())
            .collect();
        assert_eq!(failed, ["dead/dead.csv", "down/down.csv"]);
        assert_eq!(
            error.details,
            [
                "dead/dead.csv: the server has nothing at dead.csv",
                "down/down.csv: the server did not answer",
                "the 3.0 MiB downloaded so far is kept, and the next run continues from there",
            ]
        );
    }
}
