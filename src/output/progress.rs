//! progress while downloads and unpacking run: bars on a terminal a person watches, and plain
//! lines now and then anywhere else.

use std::collections::VecDeque;
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use indicatif::{
    MultiProgress, ProgressBar, ProgressDrawTarget, ProgressState, ProgressStyle, TermLike,
};

use crate::units;

/// the rate needs a few seconds of samples before the time left means anything.
const SETTLE: Duration = Duration::from_secs(3);
/// a time left reaching further than this many times the time watched so far is a guess, until
/// [`STEADY`] has passed.
const REACH: u32 = 60;
const STEADY: Duration = Duration::from_secs(30);
/// how long a download receives nothing before its bar drops a time left that no longer holds,
/// and then before it says it receives nothing instead of showing a fading rate.
const STILL: Duration = Duration::from_secs(1);
const STALL: Duration = Duration::from_secs(2);
/// how far into a transfer its rate starts over, once, so bytes that arrive at once when it
/// begins, as those a server sent while this run checked what an earlier run kept, are no speed.
const RERATE: Duration = Duration::from_secs(1);
/// how often the point a bar's rate is measured from moves on, so the rate covers the last 10 to
/// 20 s of the transfer and follows a link that speeds up or slows down.
const WINDOW: Duration = Duration::from_secs(10);
const TICK: Duration = Duration::from_millis(100);
/// how long a batch runs before its first plain line, unless it waits on the network, so quick
/// work on this machine adds no line at all.
const MOMENT: Duration = Duration::from_secs(2);
/// the same for a test that waits on that line, so it waits a tenth of a second, not two.
#[cfg(debug_assertions)]
const TEST_MOMENT: Duration = Duration::from_millis(100);
/// how often a batch that is still running says how far it got.
const EVERY: Duration = Duration::from_secs(10);

/// when a batch first says what it does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum First {
    /// as it begins: a wait on the network, such as a source looking up a link.
    Now,
    /// as soon as it knows every file's size, which a download may learn only from the answer,
    /// or after a moment.
    Sized,
    /// after a moment.
    Moment,
}

/// when a batch of `verb` first says what it does: one that waits on the network at once, so a
/// person reading plain lines hears that it began, and other work after a moment.
fn first(verb: &str) -> First {
    match verb {
        "Downloading" => First::Sized,
        "Resolving" | "Searching" => First::Now,
        _ => First::Moment,
    }
}

/// one download's bar, or nothing when bars are off. it clears itself when dropped.
pub(crate) struct Bar {
    drawn: Option<ProgressBar>,
    color: bool,
    unicode: bool,
    moved: Arc<Moved>,
    /// for a bar that joins the terminal's bars only once the command has lasted a moment, as an
    /// import's, its place among the bars waiting to join.
    later: Option<Arc<Later>>,
}

/// when a bar last moved, so a download that stopped receiving can say so, and whether the run
/// is stopping, which a bar says instead.
struct Moved {
    born: Instant,
    /// milliseconds from `born` to the last bytes or answer.
    at: AtomicU64,
    /// set once for every bar of the run, at the first ctrl-c.
    stopping: Arc<AtomicBool>,
    /// bytes an earlier run or attempt kept, which count toward how far the bar got but never
    /// toward its speed or time left: indicatif counts only the rest.
    kept: AtomicU64,
    /// the try a download waits to make after one failed, until a try gets an answer.
    retry: Mutex<Option<Retrying>>,
    /// milliseconds from `born` to the start of the transfer, and whether its rate started over
    /// [`RERATE`] into it.
    started: AtomicU64,
    rerated: AtomicBool,
    /// whether the download waits for the disk to take what arrived, so its silence is the
    /// disk's rather than the network's.
    behind: AtomicBool,
    /// when, and at how many bytes moved, the rate is measured from, the older first, each
    /// moving on every [`WINDOW`].
    marks: Mutex<[(Instant, u64); 2]>,
    /// whether the bar leaves its time left to the bar of its batch, so a screen shows one.
    untimed: AtomicBool,
    /// about how long the transfer takes, as fetchloom foresaw it from the speeds it remembers,
    /// which the bar shows until it can tell its own time left.
    foreseen: Mutex<Option<String>>,
    /// the most room the bar's counters took so far, which they keep.
    room: Mutex<Room>,
}

/// a download's next try, as its bar tells it.
struct Retrying {
    next: u32,
    of: u32,
    /// the file came on in an earlier try, so the count has no last try.
    open: bool,
    at: Instant,
    /// the host whose rate limit the try waits for, if it waits for one.
    limit: Option<String>,
}

impl Moved {
    fn new(stopping: Arc<AtomicBool>) -> Moved {
        Moved {
            born: Instant::now(),
            at: AtomicU64::new(0),
            stopping,
            kept: AtomicU64::new(0),
            retry: Mutex::new(None),
            started: AtomicU64::new(0),
            rerated: AtomicBool::new(false),
            behind: AtomicBool::new(false),
            marks: Mutex::new([(Instant::now(), 0); 2]),
            untimed: AtomicBool::new(false),
            foreseen: Mutex::new(None),
            room: Mutex::new(Room::default()),
        }
    }

    /// measures the rate from `now`, with `moved` bytes moved so far.
    fn mark(&self, now: Instant, moved: u64) {
        *self.marks.lock().unwrap_or_else(PoisonError::into_inner) = [(now, moved); 2];
    }

    /// bytes a second over this transfer's last 10 to 20 s, with `moved` bytes moved by `now`.
    fn rate(&self, moved: u64, now: Instant) -> f64 {
        let mut marks = self.marks.lock().unwrap_or_else(PoisonError::into_inner);
        if now.saturating_duration_since(marks[1].0) >= WINDOW {
            *marks = [marks[1], (now, moved)];
        }
        let (since, from) = marks[0];
        let seconds = now.saturating_duration_since(since).as_secs_f64();
        if seconds <= 0.0 {
            return 0.0;
        }
        #[expect(clippy::cast_precision_loss, reason = "a rate needs no exact count")]
        let bytes = moved.saturating_sub(from) as f64;
        bytes / seconds
    }

    /// marks `now` as the start of a transfer, whose rate has yet to start over.
    fn restart(&self, now: Instant) {
        let millis = now.saturating_duration_since(self.born).as_millis();
        self.started
            .store(u64::try_from(millis).unwrap_or(u64::MAX), Ordering::Relaxed);
        self.rerated.store(false, Ordering::Relaxed);
        self.mark(now, 0);
    }

    /// whether the rate starts over at `now`: once, [`RERATE`] into the transfer.
    fn rerate(&self, now: Instant) -> bool {
        let since = now
            .saturating_duration_since(self.born)
            .saturating_sub(Duration::from_millis(self.started.load(Ordering::Relaxed)));
        since >= RERATE && !self.rerated.swap(true, Ordering::Relaxed)
    }

    /// what the bar says of a try it waits to make, if it waits for one.
    fn retrying(&self) -> Option<String> {
        let retry = self.retry.lock().unwrap_or_else(PoisonError::into_inner);
        let retry = retry.as_ref()?;
        let left = retry.at.saturating_duration_since(Instant::now());
        Some(next_try(
            (retry.next, retry.of, retry.open),
            left,
            retry.limit.as_deref(),
        ))
    }

    /// how far the bar got and what it fills to, from indicatif's count of the bytes moved.
    fn shown(&self, moved: u64, length: Option<u64>) -> (u64, Option<u64>) {
        let kept = self.kept.load(Ordering::Relaxed);
        (
            moved.saturating_add(kept),
            length.map(|length| length.saturating_add(kept)),
        )
    }

    fn now(&self) {
        self.heard_at(Instant::now());
    }

    /// marks `at` as when the last bytes or answer came.
    fn heard_at(&self, at: Instant) {
        let millis = at.saturating_duration_since(self.born).as_millis();
        self.at
            .store(u64::try_from(millis).unwrap_or(u64::MAX), Ordering::Relaxed);
    }

    /// when the last bytes or answer came.
    fn last(&self) -> Instant {
        self.born + Duration::from_millis(self.at.load(Ordering::Relaxed))
    }

    /// the rate and the time left of a transfer `length` bytes long with `moved` bytes moved, by
    /// `now`: the rate up to the last bytes, which a link sending them in bursts leaves steady
    /// between them, and the time left from then, so it only counts down until more come.
    fn pace(&self, moved: u64, length: Option<u64>, now: Instant) -> (f64, Duration) {
        let last = self.last().min(now);
        let rate = self.rate(moved, last);
        // a transfer under way never reads `0 s left`, however long since its last bytes
        let left = length
            .filter(|_| rate > 0.0)
            .map_or(Duration::ZERO, |length| {
                time_left(length.saturating_sub(moved), rate)
                    .saturating_sub(now.saturating_duration_since(last))
                    .max(Duration::from_nanos(1))
            });
        (rate, left)
    }

    /// what a bar saying `verb` knows of its time left, `left` its own: none in a batch, whose bar
    /// tells it, and the time foreseen only while it downloads, never while it waits or checks.
    fn timed(&self, verb: &str, left: Duration) -> Timed {
        let foreseen = (verb == "Downloading")
            .then(|| {
                self.foreseen
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone()
            })
            .flatten();
        Timed {
            left: (!self.untimed.load(Ordering::Relaxed)).then_some(left),
            foreseen,
        }
    }

    /// widens `counters` to the most room each took on this bar, which it keeps: the count, the
    /// rate and a time left only ever widen the line, so the cut in its name stays put as they
    /// narrow, while a stall's or a try's words take their own room only as long as they show.
    fn hold(&self, counters: &mut Counters) {
        let mut held = self.room.lock().unwrap_or_else(PoisonError::into_inner);
        let room = counters.room;
        held.count = held.count.max(room.count);
        held.done = held.done.max(room.done);
        held.rate = held.rate.max(room.rate);
        if counters.timed {
            held.then = held.then.max(room.then);
        }
        counters.room = Room {
            then: room.then.max(held.then),
            ..*held
        };
    }

    /// how long since the bar last moved.
    fn quiet(&self) -> Duration {
        self.born
            .elapsed()
            .saturating_sub(Duration::from_millis(self.at.load(Ordering::Relaxed)))
    }
}

impl Bar {
    pub(super) fn hidden() -> Bar {
        Bar {
            drawn: None,
            color: false,
            unicode: false,
            moved: Arc::new(Moved::new(Arc::default())),
            later: None,
        }
    }

    /// a bar on `bars` that says the run is stopping once `stopping` is set.
    pub(super) fn add(
        bars: &MultiProgress,
        verb: &'static str,
        message: String,
        (color, unicode): (bool, bool),
        stopping: Arc<AtomicBool>,
    ) -> Bar {
        let moved = Arc::new(Moved::new(stopping));
        let bar = bars.add(ProgressBar::no_length().with_message(message));
        bar.set_style(style(verb, color, unicode, &bar.message(), moved.clone()));
        bar.enable_steady_tick(TICK);
        Bar {
            drawn: Some(bar),
            color,
            unicode,
            moved,
            later: None,
        }
    }

    pub(crate) fn message(&self, verb: &'static str, text: String) {
        if let Some(bar) = &self.drawn {
            bar.set_style(style(
                verb,
                self.color,
                self.unicode,
                &text,
                self.moved.clone(),
            ));
            bar.set_message(text);
        }
    }

    /// a failed try, after which try `next` of at most `of`, or of no last one while the file
    /// keeps coming when `open`, goes out after `wait`, once the rate limit of the host `limit`
    /// names resets when it names one.
    pub(crate) fn retrying(
        &self,
        (next, of, open): (u32, u32, bool),
        wait: Duration,
        limit: Option<String>,
    ) {
        if self.drawn.is_some() {
            *self
                .moved
                .retry
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = Some(Retrying {
                next,
                of,
                open,
                at: Instant::now() + wait,
                limit,
            });
        }
    }

    /// the bar as drawn, to redraw it without keeping it.
    pub(super) fn weak(&self) -> Option<indicatif::WeakProgressBar> {
        self.drawn.as_ref().map(ProgressBar::downgrade)
    }

    /// whether the download waits for the disk to take the bytes that arrived.
    pub(crate) fn behind(&self, behind: bool) {
        self.moved.behind.store(behind, Ordering::Relaxed);
    }

    /// leaves the time left to the bar of the batch this download is one of, whose time covers
    /// every download in it, so a screen never shows a batch ending before one of its files.
    pub(crate) fn untimed(&self) {
        self.moved.untimed.store(true, Ordering::Relaxed);
    }

    /// says about how long the transfer takes, as `about 2 min`, until the bar can tell its own
    /// time left.
    pub(crate) fn foreseen(&self, time: String) {
        *self
            .moved
            .foreseen
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(time);
    }

    /// a transfer of `total` bytes that goes on from `resumed`, which it does not rate.
    pub(crate) fn start(&self, total: Option<u64>, resumed: u64) {
        if let Some(bar) = &self.drawn {
            // the answer is news, before the wait for it ends, so no frame between calls it quiet
            self.moved.now();
            self.moved
                .retry
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take();
            self.moved.kept.store(resumed, Ordering::Relaxed);
            if let Some(total) = total {
                bar.set_length(total.saturating_sub(resumed));
            }
            // back to 0, which indicatif reads as a fresh start of its estimate, with the time
            // this transfer takes counted from now, not from an earlier try or a check
            bar.set_position(0);
            bar.reset_eta();
            bar.reset_elapsed();
            self.moved.now();
            self.moved.restart(Instant::now());
        }
    }

    /// a wait of `waited` for a person's answer, which the bar counts neither as a pause in the
    /// data nor toward the transfer's time or rate.
    pub(crate) fn answered(&self, waited: Duration) {
        if let Some(bar) = &self.drawn {
            self.moved.now();
            bar.set_elapsed(bar.elapsed().saturating_sub(waited));
            self.moved.mark(Instant::now(), bar.position());
        }
    }

    /// `bytes` that count toward how far the bar got but were not moved now, as the part an
    /// earlier run kept of one file among several.
    pub(crate) fn kept(&self, bytes: u64) {
        if let Some(bar) = &self.drawn {
            self.moved.kept.fetch_add(bytes, Ordering::Relaxed);
            if let Some(length) = bar.length() {
                bar.set_length(length.saturating_sub(bytes));
            }
        }
    }

    pub(crate) fn advance(&self, bytes: u64) {
        if let Some(bar) = &self.drawn {
            bar.inc(bytes);
            self.moved.now();
            let now = Instant::now();
            if self.moved.rerate(now) {
                self.moved.mark(now, bar.position());
            }
        }
    }

    /// the text beside the bar's verb, for tests.
    #[cfg(test)]
    pub(crate) fn told(&self) -> Option<String> {
        self.drawn.as_ref().map(ProgressBar::message)
    }

    /// whether the bar tells a time left of its own, and about how long it was foreseen to take,
    /// for tests.
    #[cfg(test)]
    pub(crate) fn timing(&self) -> (bool, Option<String>) {
        (
            !self.moved.untimed.load(Ordering::Relaxed),
            self.moved
                .foreseen
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone(),
        )
    }

    /// what the bar fills to as it draws it, none while it has no length, for tests.
    #[cfg(test)]
    pub(crate) fn total(&self) -> Option<u64> {
        let drawn = self.drawn.as_ref()?;
        self.moved.shown(drawn.position(), drawn.length()).1
    }

    /// what the bar fills to, as a size learned during the downloads changes it.
    pub(crate) fn length(&self, total: u64) {
        if let Some(bar) = &self.drawn {
            bar.set_length(total.saturating_sub(self.moved.kept.load(Ordering::Relaxed)));
        }
    }
}

impl Drop for Bar {
    fn drop(&mut self) {
        if let Some(later) = &self.later {
            later.end();
        }
        if let Some(bar) = &self.drawn {
            bar.finish_and_clear();
        }
    }
}

/// a bar drawn nowhere until it joins the terminal's bars.
pub(super) struct Later {
    bar: ProgressBar,
    bars: MultiProgress,
    /// whether the bar joined the terminal's bars or ended, after which it never joins.
    settled: Mutex<bool>,
}

impl Later {
    /// puts the bar among the terminal's bars, ticking, unless it joined or ended.
    fn show(&self) {
        let mut settled = self.settled.lock().unwrap_or_else(PoisonError::into_inner);
        if !*settled {
            *settled = true;
            self.bars.add(self.bar.clone());
            self.bar.enable_steady_tick(TICK);
        }
    }

    fn end(&self) {
        *self.settled.lock().unwrap_or_else(PoisonError::into_inner) = true;
    }
}

/// how often at most the bars whose time came join the terminal's, so many bars due close
/// together wake the thread adding them no more than 20 times a second.
const JOINS_EVERY: Duration = Duration::from_millis(50);

/// bars of work that usually ends in a moment, such as an import's copying, which join the
/// terminal's bars only at a time they are given, so work that ends before draws no frame. a bar
/// that comes and goes redraws every bar on the terminal, a flash a person cannot read.
#[derive(Default)]
pub(super) struct Joining {
    waiting: Mutex<Waiting>,
}

/// the bars waiting to join, each with when it joins, the earliest first, and whether a thread
/// adds them.
#[derive(Default)]
struct Waiting {
    due: VecDeque<(Instant, Weak<Later>)>,
    adding: bool,
}

impl Waiting {
    /// the bars whose time came by `now`, taken off the list.
    fn take_due(&mut self, now: Instant) -> Vec<Weak<Later>> {
        let count = self.due.iter().take_while(|(at, _)| *at <= now).count();
        self.due.drain(..count).map(|(_, later)| later).collect()
    }

    /// how long after `now` the next bar joins, at least [`JOINS_EVERY`], or none when no bar
    /// waits.
    fn next(&self, now: Instant) -> Option<Duration> {
        let (at, _) = self.due.front()?;
        Some(at.saturating_duration_since(now).max(JOINS_EVERY))
    }
}

impl Joining {
    /// a bar like [`Bar::add`]'s that counts unseen and joins `bars` at `due`, kept in order of
    /// when each joins.
    pub(super) fn bar_at(
        self: &Arc<Self>,
        bars: &MultiProgress,
        (verb, message): (&'static str, String),
        (color, unicode): (bool, bool),
        stopping: Arc<AtomicBool>,
        due: Instant,
    ) -> Bar {
        let moved = Arc::new(Moved::new(stopping));
        let bar =
            ProgressBar::with_draw_target(None, ProgressDrawTarget::hidden()).with_message(message);
        bar.set_style(style(verb, color, unicode, &bar.message(), moved.clone()));
        let later = Arc::new(Later {
            bar: bar.clone(),
            bars: bars.clone(),
            settled: Mutex::new(false),
        });
        let mut waiting = self.waiting.lock().unwrap_or_else(PoisonError::into_inner);
        let at = waiting.due.partition_point(|(when, _)| *when <= due);
        waiting.due.insert(at, (due, Arc::downgrade(&later)));
        if !waiting.adding {
            let joining = self.clone();
            // without a thread the bar never joins, as one too quick to see
            waiting.adding = std::thread::Builder::new()
                .name("bars".to_owned())
                .spawn(move || joining.add_when_due())
                .is_ok();
        }
        Bar {
            drawn: Some(bar),
            color,
            unicode,
            moved,
            later: Some(later),
        }
    }

    /// adds each waiting bar as its time comes, and ends once none waits.
    fn add_when_due(&self) {
        loop {
            let mut waiting = self.waiting.lock().unwrap_or_else(PoisonError::into_inner);
            let now = Instant::now();
            let due = waiting.take_due(now);
            let next = waiting.next(now);
            if due.is_empty() && next.is_none() {
                waiting.adding = false;
                return;
            }
            drop(waiting);
            for later in due.iter().filter_map(Weak::upgrade) {
                later.show();
            }
            if let Some(wait) = next {
                std::thread::sleep(wait);
            }
        }
    }
}

fn style(
    verb: &'static str,
    color: bool,
    unicode: bool,
    message: &str,
    moved: Arc<Moved>,
) -> ProgressStyle {
    let name = super::plain(message).into_owned();
    let tone = super::toned(color, super::Tone::Work);
    let heading = format!("{tone}{verb:>11}{tone:#}");
    ProgressStyle::with_template("{line}")
        .unwrap_or_else(|_| ProgressStyle::default_spinner())
        .with_key(
            "line",
            move |state: &ProgressState, out: &mut dyn fmt::Write| {
                let (done, total) = moved.shown(state.pos(), state.len());
                let bar = matches!(verb, "Downloading" | "Unpacking" | "Checking").then(|| {
                    total.map_or_else(
                        || (meter(0, 1, color), None),
                        |total| (meter(done, total, color), Some(percent(done, total))),
                    )
                });
                // unpacking and checking read local files, whose pauses mean nothing
                let quiet = (verb == "Downloading").then(|| moved.quiet());
                let stopping = moved.stopping.load(Ordering::Relaxed);
                let behind = quiet.filter(|_| moved.behind.load(Ordering::Relaxed));
                let mut counters = waiting(done, total, moved.retrying(), behind)
                    .filter(|_| !stopping)
                    .unwrap_or_else(|| {
                        // this run's bytes over its own time, as the time left is
                        let (rate, left) = moved.pace(state.pos(), state.len(), Instant::now());
                        let timed = moved.timed(verb, left);
                        measure((done, total), rate, state.elapsed(), timed, quiet, stopping)
                    });
                // a transfer's counters keep the room they grow to, so its name's cut stays put
                if bar.is_some() {
                    counters.steady(total);
                    moved.hold(&mut counters);
                }
                let width = usize::from(console::Term::stderr().size().1);
                let meter = bar.as_ref().map(|(bar, percent)| (bar.as_str(), *percent));
                let line = render(&name, &heading, meter, &counters, width, color);
                let _ = out.write_str(&super::shown(&line, unicode));
            },
        )
}

/// how far `done` got of `total`, in whole percent, rounded down, so a bar short of its end never
/// reads `100%`.
fn percent(done: u64, total: u64) -> u8 {
    if total == 0 {
        return 100;
    }
    u8::try_from(u128::from(done.min(total)) * 100 / u128::from(total)).unwrap_or(100)
}

fn meter(done: u64, total: u64, color: bool) -> String {
    let filled = if total == 0 {
        20
    } else {
        usize::try_from(u128::from(done.min(total)) * 20 / u128::from(total)).unwrap_or(20)
    };
    let (fill, empty) = if color { ("━", "━") } else { ("#", "-") };
    let tone = super::paint(color, super::tinted(super::Tone::Work));
    let dim = super::paint(color, anstyle::Style::new().dimmed());
    format!(
        "{tone}{}{tone:#}{dim}{}{dim:#}",
        fill.repeat(filled),
        empty.repeat(20 - filled),
    )
}

/// the fewest columns a name keeps beside a bar, beside a percent, and at the last.
const NAME_ROOMY: usize = 24;
const NAME_LEAST: usize = 16;
const NAME_SHORTEST: usize = 8;

/// the room a transfer keeps for its rate, as wide as `1023.9 KiB/s`, and for its time left, as
/// wide as `59 s left`, before they show, so its line keeps one shape while it runs.
const RATE_WIDEST: usize = 12;
const LEFT_WIDEST: usize = 9;

/// a bar's counters apart, so a narrow line lets the least useful go first, each with the room it
/// keeps, which a transfer's counters grow to.
#[derive(Debug, Default, PartialEq, Eq)]
struct Counters {
    /// how far the bar got: `98.1/234.5 MiB`, or the bytes alone with no total.
    count: Option<String>,
    /// the rate, the first counter to give way.
    rate: Option<String>,
    /// what comes after: the time left, a stall, a try to come, a stop, or how long a wait lasted.
    then: Option<String>,
    room: Room,
    /// whether what comes after is a time left.
    timed: bool,
}

/// the room a bar's counters keep: the count, whole and as its bytes alone, the rate and what
/// comes after.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Room {
    count: usize,
    done: usize,
    rate: usize,
    then: usize,
}

/// which form of a bar's count a line keeps.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Count {
    Whole,
    /// the bytes done alone, as `98.1 MiB`.
    Done,
    Uncounted,
}

impl Counters {
    /// counters with `count`, a `rate` and what comes `then`, each given the room it takes now.
    fn new(count: Option<String>, rate: Option<String>, then: Option<String>) -> Counters {
        let width = |part: &Option<String>| part.as_deref().map_or(0, console::measure_text_width);
        let room = Room {
            count: width(&count),
            done: width(&count.as_deref().map(done_alone)),
            rate: width(&rate),
            then: width(&then),
        };
        Counters {
            count,
            rate,
            then,
            room,
            timed: false,
        }
    }

    /// gives a transfer's counters the room they grow to, with `total` its length when known:
    /// the count at its widest, at its end or below one of its unit, as `0.94/6.0 MiB` is, whole
    /// and as its bytes alone, and the rate and the time left at theirs.
    fn steady(&mut self, total: Option<u64>) {
        if let Some(total) = total {
            for count in [fraction(total, total), fraction(0, total)] {
                let done = console::measure_text_width(&done_alone(&count));
                self.room.count = self.room.count.max(console::measure_text_width(&count));
                self.room.done = self.room.done.max(done);
            }
            self.room.then = self.room.then.max(LEFT_WIDEST);
        }
        self.room.rate = self.room.rate.max(RATE_WIDEST);
    }

    /// what a line shows of them, with `rate`, the count in its `count` form and what comes
    /// `then`, and the room that takes, a transfer's counters yet to come with theirs.
    fn kept(&self, rate: bool, count: Count, then: bool) -> (String, usize) {
        let count = match count {
            Count::Whole => Some((self.count.clone(), self.room.count)),
            Count::Done => Some((self.count.as_deref().map(done_alone), self.room.done)),
            Count::Uncounted => None,
        };
        let parts: Vec<(Option<String>, usize)> = [
            count,
            rate.then(|| (self.rate.clone(), self.room.rate)),
            then.then(|| (self.then.clone(), self.room.then)),
        ]
        .into_iter()
        .flatten()
        .filter(|(part, room)| part.is_some() || *room > 0)
        .collect();
        let room = parts.iter().map(|(_, room)| room + 3).sum::<usize>();
        let shown: Vec<String> = parts.into_iter().filter_map(|(part, _)| part).collect();
        (shown.join(" · "), room.saturating_sub(3))
    }

    /// every counter, as a line wide enough shows them, for tests.
    #[cfg(test)]
    fn said(&self) -> String {
        self.kept(true, Count::Whole, true).0
    }
}

/// the bytes a count says were done, with the unit they share with its total: `98.1 MiB` of
/// `98.1/234.5 MiB`, or the count as it is without a total.
fn done_alone(count: &str) -> String {
    count.split_once('/').map_or_else(
        || count.to_owned(),
        |(done, total)| match total.rsplit_once(' ') {
            Some((_, unit)) if !done.contains(' ') => format!("{done} {unit}"),
            _ => done.to_owned(),
        },
    )
}

/// what of a bar shows in place of its meter: the meter, its percent, or neither.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mark {
    Meter,
    Percent,
    Unmarked,
}

/// the least a name keeps that is the whole name.
const WHOLE: usize = usize::MAX;

/// a bar's line at `width` columns, its progress kept before its name: as the terminal narrows,
/// the name is cut in the middle first, then the verb goes, then the bar gives way to its percent,
/// then the counters go from the least useful, the rate first; the time left and the percent stay
/// longest. a name alone is cut to fit.
fn render(
    name: &str,
    verb: &str,
    meter: Option<(&str, Option<u8>)>,
    counters: &Counters,
    width: usize,
    color: bool,
) -> String {
    use Count::{Done, Uncounted, Whole};
    use Mark::{Meter, Percent, Unmarked};
    // what a line keeps: the verb, its mark, the rate, its count, what comes then, and the least
    // its name keeps
    const LINES: [(bool, Mark, bool, Count, bool, usize); 11] = [
        (true, Meter, true, Whole, true, WHOLE),
        (true, Meter, true, Whole, true, NAME_ROOMY),
        (false, Meter, true, Whole, true, NAME_ROOMY),
        (false, Percent, true, Whole, true, WHOLE),
        (false, Percent, true, Whole, true, NAME_LEAST),
        (false, Percent, false, Whole, true, NAME_LEAST),
        (false, Percent, false, Whole, true, NAME_SHORTEST),
        (false, Percent, false, Done, true, NAME_SHORTEST),
        (false, Percent, false, Uncounted, true, NAME_SHORTEST),
        (false, Percent, false, Uncounted, false, NAME_SHORTEST),
        (false, Unmarked, false, Uncounted, false, NAME_SHORTEST),
    ];
    let dim = super::paint(color, anstyle::Style::new().dimmed());
    let tone = super::paint(color, super::tinted(super::Tone::Work));
    let named = console::measure_text_width(name);
    for (with_verb, mark, rate, count, then, least) in LINES {
        let mark = match (mark, meter) {
            (Meter, Some((bar, _))) => Some(bar.to_owned()),
            (Percent, Some((_, Some(percent)))) => Some(format!("{tone}{percent:>3}%{tone:#}")),
            _ => None,
        };
        let (amount, kept) = counters.kept(rate, count, then);
        let verb = with_verb.then_some(verb);
        let amount = (!amount.is_empty()).then(|| format!("{dim}{amount}{dim:#}"));
        // what the line takes beside the name, each part with the space that parts it from the
        // next, the counters at the room they keep
        let taken = verb.map_or(0, |verb| console::measure_text_width(verb) + 1)
            + mark
                .as_deref()
                .map_or(0, |mark| console::measure_text_width(mark) + 1)
            + if kept > 0 { kept + 1 } else { 0 };
        let room = width.saturating_sub(taken);
        if room < least.min(named) {
            continue;
        }
        let shown = middle(name, room);
        return [
            verb.map(str::to_owned),
            Some(shown).filter(|shown| !shown.is_empty()),
            mark,
            amount,
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ");
    }
    middle(name, width)
}

fn middle(name: &str, width: usize) -> String {
    if console::measure_text_width(name) <= width {
        return name.to_owned();
    }
    if width == 0 {
        return String::new();
    }
    let kept = width - 1;
    let head = console::truncate_str(name, kept / 2, "");
    let mut tail = "";
    for (offset, _) in name.char_indices().rev() {
        let candidate = &name[offset..];
        if console::measure_text_width(candidate) > kept - kept / 2 {
            break;
        }
        tail = candidate;
    }
    format!("{head}~{tail}")
}

/// what a bar knows of its time left: its own, unless the bar of its batch tells it, and about
/// how long fetchloom foresaw the transfer taking, shown until the bar can tell its own.
struct Timed {
    left: Option<Duration>,
    foreseen: Option<String>,
}

/// the counters of a bar that got `done` of `total`: the rate once it settles and the time left
/// once it holds, `no data` once `quiet`, which local work leaves none, passes [`STALL`], the disk
/// once every byte is in, and `stopping` alone. a wait with nothing counted says how long it has
/// lasted; a transfer shows its bytes, rate and time left instead.
fn measure(
    (done, total): (u64, Option<u64>),
    rate: f64,
    elapsed: Duration,
    timed: Timed,
    quiet: Option<Duration>,
    stopping: bool,
) -> Counters {
    let count = (done > 0).then(|| counted(done, total));
    if stopping {
        return Counters::new(count, None, Some("stopping".to_owned()));
    }
    if let Some(quiet) = quiet {
        if every_byte_in(done, total) {
            let then = if quiet >= MOMENT {
                format!("waiting for the disk · {}", units::seconds(quiet))
            } else {
                "waiting for the disk".to_owned()
            };
            return Counters::new(count, None, Some(then));
        }
        if quiet >= STALL {
            let then = format!("no data for {}", units::seconds(quiet));
            return Counters::new(count, None, Some(then));
        }
    }
    let settled = done > 0 && rate > 0.0 && elapsed >= SETTLE;
    // a download that just went quiet still has a rate, but no time left it can keep to
    let still = quiet.is_some_and(|quiet| quiet >= STILL);
    let left = timed.left.filter(|left| {
        settled
            && !still
            && total.is_some()
            && (elapsed >= STEADY || *left <= elapsed.saturating_mul(REACH))
    });
    let rate = settled.then(|| units::speed(rate));
    // before its length is known, a bar has yet to start the transfer it foresaw
    let foreseen = timed
        .foreseen
        .filter(|_| timed.left.is_some() && total.is_some() && !still)
        .map(|foreseen| format!("{foreseen} left"));
    let (then, timed) = match left {
        Some(left) => (
            Some(format!("{} left", units::seconds(whole_up(left)))),
            true,
        ),
        None if foreseen.is_some() => (foreseen, true),
        None if done == 0 && elapsed >= MOMENT && !still => (Some(units::seconds(elapsed)), false),
        None => (None, false),
    };
    Counters {
        timed,
        ..Counters::new(count, rate, then)
    }
}

/// how long the `rest` of a transfer takes at `rate` bytes a second, the longest duration when a
/// rate slow enough makes it too long to count.
fn time_left(rest: u64, rate: f64) -> Duration {
    #[expect(
        clippy::cast_precision_loss,
        reason = "a time left needs no exact count"
    )]
    let rest = rest as f64;
    Duration::try_from_secs_f64(rest / rate).unwrap_or(Duration::MAX)
}

/// `wait` in whole seconds, rounded up, so time still to come never reads 0 s.
fn whole_up(wait: Duration) -> Duration {
    Duration::from_secs(
        wait.as_secs()
            .saturating_add(u64::from(wait.subsec_nanos() > 0)),
    )
}

/// what a bar says while it waits on something other than the server's bytes: its next try after
/// one failed, as [`next_try`] says it, or the disk, `behind` for that long taking what arrived.
fn waiting(
    done: u64,
    total: Option<u64>,
    retry: Option<String>,
    behind: Option<Duration>,
) -> Option<Counters> {
    let what = match (retry, behind) {
        (Some(next), _) => next,
        (None, Some(quiet)) if quiet >= STALL && !every_byte_in(done, total) => {
            format!("waiting for the disk · {}", units::seconds(quiet))
        }
        _ => return None,
    };
    Some(Counters::new(
        (done > 0).then(|| counted(done, total)),
        None,
        Some(what),
    ))
}

/// how far a bar got: `done` of `total` when it is known.
fn counted(done: u64, total: Option<u64>) -> String {
    total.map_or_else(|| units::size(done), |total| fraction(done, total))
}

/// what a bar says of the try its download waits to make, `left` from now: which try of how
/// many, and whose rate limit it waits for when `limit` names a host.
fn next_try((next, of, open): (u32, u32, bool), left: Duration, limit: Option<&str>) -> String {
    let mut parts = Vec::new();
    if let Some(host) = limit {
        parts.push(format!("waiting for {host}'s rate limit"));
    }
    parts.push(units::try_count(next, of, open));
    if !left.is_zero() {
        parts.push(format!("next in {}", units::seconds(whole_up(left))));
    }
    parts.join(" · ")
}

/// whether `done` reached a known, nonzero `total`: a download then has every byte in, and what
/// is left is the disk's.
fn every_byte_in(done: u64, total: Option<u64>) -> bool {
    total.is_some_and(|total| total > 0 && done >= total)
}

/// how far a bar got of `total`, both in the total's unit, with two decimals below one of it.
fn fraction(done: u64, total: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let total = units::size(total);
    let power = total
        .rsplit_once(' ')
        .and_then(|(_, unit)| UNITS.iter().position(|known| *known == unit))
        .unwrap_or_default();
    if power == 0 {
        return format!("{done}/{total}");
    }
    let divisor = 1024_u128.pow(u32::try_from(power).unwrap_or_default());
    let hundredths = (u128::from(done) * 100 + divisor / 2) / divisor;
    if hundredths < 100 {
        return format!("0.{hundredths:02}/{total}");
    }
    let tenths = (u128::from(done) * 10 + divisor / 2) / divisor;
    format!("{}.{}/{total}", tenths / 10, tenths % 10)
}

/// where bars draw: stderr, with the cursor back at the start of the last bar's line after each
/// frame. indicatif pads lines to the last column, so a ^C echoed at a stop went onto a line of
/// its own, which clearing the bars then cleared, leaving their last frame on screen.
pub(super) fn terminal() -> ProgressDrawTarget {
    let rested = Rested {
        term: console::Term::buffered_stderr(),
        drawn: AtomicBool::new(false),
    };
    ProgressDrawTarget::term_like_with_hz(Box::new(rested), 20)
}

#[derive(Debug)]
struct Rested {
    term: console::Term,
    /// whether a frame was written since the last flush, which leaves the cursor past its end.
    drawn: AtomicBool,
}

impl TermLike for Rested {
    fn width(&self) -> u16 {
        self.term.size().1
    }

    fn height(&self) -> u16 {
        self.term.size().0
    }

    fn move_cursor_up(&self, n: usize) -> std::io::Result<()> {
        self.term.move_cursor_up(n)
    }

    fn move_cursor_down(&self, n: usize) -> std::io::Result<()> {
        self.term.move_cursor_down(n)
    }

    fn move_cursor_right(&self, n: usize) -> std::io::Result<()> {
        self.term.move_cursor_right(n)
    }

    fn move_cursor_left(&self, n: usize) -> std::io::Result<()> {
        self.term.move_cursor_left(n)
    }

    fn write_line(&self, s: &str) -> std::io::Result<()> {
        self.term.write_line(s)
    }

    fn write_str(&self, s: &str) -> std::io::Result<()> {
        if !s.is_empty() {
            self.drawn.store(true, Ordering::Relaxed);
        }
        self.term.write_str(s)
    }

    fn clear_line(&self) -> std::io::Result<()> {
        self.term.clear_line()
    }

    /// ends what indicatif drew or cleared; after a frame, the cursor goes back to its line's
    /// start.
    fn flush(&self) -> std::io::Result<()> {
        if self.drawn.swap(false, Ordering::Relaxed) {
            self.term.write_str("\r")?;
        }
        self.term.flush()
    }
}

/// a step before a command's own work, such as taking the project lock: at a terminal a live line
/// names it once it has lasted [`UNTOLD`], so the screen is never blank for long.
pub(crate) struct Setup {
    ended: Arc<(Mutex<bool>, Condvar)>,
    thread: Option<JoinHandle<()>>,
}

/// how long a step before a command's own work goes untold at a terminal.
pub(super) const UNTOLD: Duration = Duration::from_secs(1);

impl Setup {
    pub(super) fn quiet() -> Setup {
        Setup {
            ended: Arc::default(),
            thread: None,
        }
    }

    /// shows `verb` and `what` on `bars` once the step has lasted `untold`, usually [`UNTOLD`],
    /// in the bar's `look`, until the step ends.
    pub(super) fn after(
        untold: Duration,
        bars: &MultiProgress,
        (verb, what): (&'static str, String),
        look: (bool, bool),
        stopping: Arc<AtomicBool>,
    ) -> Setup {
        let ended: Arc<(Mutex<bool>, Condvar)> = Arc::default();
        let watched = ended.clone();
        let bars = bars.clone();
        let began = Instant::now();
        // without a thread the step goes untold, which costs nothing but the line
        let thread = std::thread::Builder::new()
            .name("setup".to_owned())
            .spawn(move || {
                let (over, wake) = &*watched;
                let ended = over.lock().unwrap_or_else(PoisonError::into_inner);
                let (ended, _) = wake
                    .wait_timeout_while(ended, untold, |ended| !*ended)
                    .unwrap_or_else(PoisonError::into_inner);
                if *ended {
                    return;
                }
                let bar = Bar::add(&bars, verb, what, look, stopping);
                if let Some(drawn) = &bar.drawn {
                    drawn.set_elapsed(began.elapsed());
                }
                let _ended = wake
                    .wait_while(ended, |ended| !*ended)
                    .unwrap_or_else(PoisonError::into_inner);
                drop(bar);
            })
            .ok();
        Setup { ended, thread }
    }
}

impl Drop for Setup {
    fn drop(&mut self) {
        let (over, wake) = &*self.ended;
        *over.lock().unwrap_or_else(PoisonError::into_inner) = true;
        wake.notify_all();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// keeps a wait visible until its owner finishes.
pub(crate) struct Wait {
    pub(super) _bar: Bar,
    pub(super) _ticker: Ticker,
}

/// a batch told in plain lines where no bar shows it: what it does, when [`first`] says, then how
/// far it got every 10 s, never a line per file. it stops when dropped.
pub(crate) struct Ticker {
    shared: Option<Arc<Shared>>,
    thread: Option<JoinHandle<()>>,
}

/// how a batch tells a line: its verb, then the rest.
type Say = Box<dyn Fn(&str, &str) + Send + Sync>;

struct Shared {
    verb: &'static str,
    what: String,
    first: First,
    done: AtomicU64,
    total: AtomicU64,
    /// files whose size only their download tells, so the total is not known until they start.
    unknown: AtomicUsize,
    /// whether the batch said what it does, which it says once.
    begun: AtomicBool,
    finished: Mutex<bool>,
    wake: Condvar,
    say: Say,
}

impl Shared {
    /// the total of the batch's sizes, once every one is known.
    fn total(&self) -> Option<u64> {
        (self.unknown.load(Ordering::Relaxed) == 0).then(|| self.total.load(Ordering::Relaxed))
    }

    /// says what the batch does, unless it has, ending with `time` when one is known. whether it
    /// said so now.
    fn begin(&self, time: Option<&str>) -> bool {
        if self.begun.swap(true, Ordering::Relaxed) {
            return false;
        }
        let (verb, text) = report(
            self.verb,
            &self.what,
            (0, self.total()),
            Duration::ZERO,
            true,
        );
        match time {
            Some(time) => (self.say)(verb, &format!("{text} · {time}")),
            None => (self.say)(verb, &text),
        }
        true
    }
}

/// whether a batch's plain lines said what it does, shared with a step that may say it first.
#[derive(Clone)]
pub(crate) struct Begun(Option<Arc<Shared>>);

impl Begun {
    /// whether this is the first to say what the batch does, which its plain lines then never
    /// say: always, where the batch has none.
    pub(crate) fn first(&self) -> bool {
        self.0
            .as_ref()
            .is_none_or(|shared| !shared.begun.swap(true, Ordering::Relaxed))
    }
}

/// how long a batch runs before its first plain line, or none for no lines. debug builds take
/// `FETCHLOOM_TEST_TICKER` from tests: `off` drops the lines and a slow disk's time in a
/// download's closing line, any other value waits 0.1 s.
#[cfg_attr(
    not(debug_assertions),
    expect(
        clippy::unnecessary_wraps,
        reason = "only a debug build's tests turn the lines off"
    )
)]
pub(super) fn moment() -> Option<Duration> {
    #[cfg(debug_assertions)]
    if let Some(value) = std::env::var_os("FETCHLOOM_TEST_TICKER") {
        return (value != "off").then_some(TEST_MOMENT);
    }
    Some(MOMENT)
}

impl Ticker {
    pub(super) fn quiet() -> Ticker {
        Ticker {
            shared: None,
            thread: None,
        }
    }

    /// tells `verb` and `what` through `say` once the batch lasts `moment`, its known sizes adding
    /// up to `total`, with `unknown` more files whose downloads have yet to tell theirs. `planned`
    /// says the run told a download batch's plan, which then said it began; otherwise a batch
    /// whose sizes are all known says it began itself, with `time` when there are grounds.
    pub(super) fn start(
        verb: &'static str,
        what: String,
        (total, unknown): (u64, usize),
        (moment, planned, time): (Duration, bool, Option<String>),
        say: impl Fn(&str, &str) + Send + Sync + 'static,
    ) -> Ticker {
        Ticker::paced(
            verb,
            what,
            (total, unknown),
            (moment, EVERY),
            (planned, time),
            say,
        )
    }

    fn paced(
        verb: &'static str,
        what: String,
        (total, unknown): (u64, usize),
        pace: (Duration, Duration),
        (planned, time): (bool, Option<String>),
        say: impl Fn(&str, &str) + Send + Sync + 'static,
    ) -> Ticker {
        let shared = Arc::new(Shared {
            verb,
            what,
            first: first(verb),
            done: AtomicU64::new(0),
            total: AtomicU64::new(total),
            unknown: AtomicUsize::new(unknown),
            begun: AtomicBool::new(false),
            finished: Mutex::new(false),
            wake: Condvar::new(),
            say: Box::new(say),
        });
        // said before the work starts, so even a batch that ends at once has said it began. a
        // download batch whose sizes are all known said so in its plan before it started, where
        // the run told its plan, which --json hides
        let sized = shared.first == First::Sized && unknown == 0;
        if shared.first == First::Now || (sized && !planned) {
            shared.begin(time.as_deref());
        } else if sized {
            shared.begun.store(true, Ordering::Relaxed);
        }
        let watched = shared.clone();
        // without a thread the batch runs on untold, which costs nothing but the lines
        let thread = std::thread::Builder::new()
            .name("ticker".to_owned())
            .spawn(move || tick(&watched, pace))
            .ok();
        Ticker {
            shared: Some(shared),
            thread,
        }
    }

    pub(crate) fn advance(&self, bytes: u64) {
        if let Some(shared) = &self.shared {
            shared.done.fetch_add(bytes, Ordering::Relaxed);
        }
    }

    /// what says whether the batch said what it does, for a step that may say it first.
    pub(crate) fn begun(&self) -> Begun {
        Begun(self.shared.clone())
    }

    /// adds the size a download just told; the one that makes its batch's size known says what the
    /// batch does, with `time` when there are grounds, unless it told none, as a refused one does.
    pub(crate) fn sized(&self, bytes: u64, time: Option<&str>) {
        if let Some(shared) = &self.shared
            && shared
                .unknown
                .try_update(Ordering::Relaxed, Ordering::Relaxed, |left| {
                    left.checked_sub(1)
                })
                .is_ok()
        {
            let _ = shared
                .total
                .try_update(Ordering::Relaxed, Ordering::Relaxed, |total| {
                    Some(total.saturating_add(bytes))
                });
            if shared.first == First::Sized && bytes > 0 && shared.total().is_some() {
                shared.begin(time);
            }
        }
    }
}

impl Drop for Ticker {
    fn drop(&mut self) {
        if let Some(shared) = &self.shared {
            *shared
                .finished
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = true;
            shared.wake.notify_all();
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// waits out the batch, saying what it does after `moment` unless it said so sooner, and how
/// far it got every `every`.
fn tick(shared: &Shared, (moment, every): (Duration, Duration)) {
    let started = Instant::now();
    let mut next = moment;
    loop {
        let finished = shared
            .finished
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let wait = next.saturating_sub(started.elapsed());
        let (finished, _) = shared
            .wake
            .wait_timeout_while(finished, wait, |finished| !*finished)
            .unwrap_or_else(PoisonError::into_inner);
        if *finished {
            return;
        }
        drop(finished);
        // a batch that said what it does before its moment next says how far it got
        if shared.begin(None) || next == moment {
            next = every;
            continue;
        }
        let done = shared.done.load(Ordering::Relaxed);
        let (verb, text) = report(
            shared.verb,
            &shared.what,
            (done, shared.total()),
            started.elapsed(),
            false,
        );
        (shared.say)(verb, &text);
        next += every;
    }
}

/// the line a batch tells once it has run `elapsed`: what it does the `first` time, then how far it
/// got of `total`, or once every byte is in, that a download waits on the disk or an unpack ends.
fn report(
    verb: &'static str,
    what: &str,
    (done, total): (u64, Option<u64>),
    elapsed: Duration,
    first: bool,
) -> (&'static str, String) {
    if first {
        return (
            verb,
            match total {
                Some(total) => format!("{what} · {}", units::size(total)),
                None => what.to_owned(),
            },
        );
    }
    let elapsed = units::seconds(elapsed);
    if verb == "Downloading" && every_byte_in(done, total) {
        return (
            "Waiting",
            format!("for the disk to finish writing {what} · {elapsed}"),
        );
    }
    if verb == "Unpacking" && every_byte_in(done, total) {
        return ("Finishing", format!("{what} · {elapsed}"));
    }
    (
        verb,
        match total {
            Some(total) => format!(
                "{} of {} · {elapsed}",
                units::size(done.min(total)),
                units::size(total)
            ),
            // a wait with no bytes to count, such as a source taking its time to answer
            None if done == 0 => format!("{what} · {elapsed}"),
            None => format!("{} · {elapsed}", units::size(done)),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::screen::Screen;

    /// [`measure`]'s counters as one line, with `left` the bar's own time left.
    fn measured(
        done: u64,
        total: Option<u64>,
        rate: f64,
        elapsed: Duration,
        left: Duration,
        quiet: Option<Duration>,
        stopping: bool,
    ) -> String {
        let timed = Timed {
            left: Some(left),
            foreseen: None,
        };
        measure((done, total), rate, elapsed, timed, quiet, stopping).said()
    }

    /// a wait for a person's answer counts neither as data that stopped coming nor in the bar's
    /// time: 4 s of waiting in a bar born 5 s ago leave it neither quiet nor 2 s old.
    #[test]
    fn a_bar_leaves_out_the_wait_for_an_answer() {
        let five = Duration::from_secs(5);
        let born = Instant::now()
            .checked_sub(five)
            .unwrap_or_else(Instant::now);
        let moved = Arc::new(Moved {
            born,
            ..Moved::new(Arc::default())
        });
        let drawn = ProgressBar::hidden();
        drawn.set_elapsed(five);
        let bar = Bar {
            drawn: Some(drawn),
            color: false,
            unicode: false,
            moved,
            later: None,
        };
        assert!(bar.moved.quiet() >= STALL, "{:?}", bar.moved.quiet());
        bar.answered(Duration::from_secs(4));
        assert!(bar.moved.quiet() < STILL, "{:?}", bar.moved.quiet());
        let elapsed = bar.drawn.as_ref().map(ProgressBar::elapsed);
        assert!(
            elapsed.is_some_and(|elapsed| elapsed < MOMENT),
            "{elapsed:?}"
        );
    }

    type Heard = Arc<Mutex<Vec<String>>>;

    fn ear() -> (
        Heard,
        std::sync::mpsc::Receiver<()>,
        impl Fn(&str, &str) + Send + 'static,
    ) {
        let heard = Heard::default();
        let kept = heard.clone();
        let (announced, reported) = std::sync::mpsc::channel();
        let say = move |verb: &str, text: &str| {
            kept.lock().unwrap().push(format!("{verb} {text}"));
            let _ = announced.send(());
        };
        (heard, reported, say)
    }

    /// a bar resuming from 900 kept bytes and moving 100 rates and times only the 100.
    #[test]
    fn a_resumed_bar_rates_only_the_bytes_this_run_moved() {
        let bars = MultiProgress::with_draw_target(indicatif::ProgressDrawTarget::hidden());
        let bar = Bar::add(
            &bars,
            "Downloading",
            "big.bin".to_owned(),
            (false, false),
            Arc::default(),
        );
        bar.start(Some(1000), 900);
        bar.advance(100);
        let counted = bar
            .drawn
            .as_ref()
            .map(|drawn| (drawn.position(), drawn.length()));
        assert_eq!(counted, Some((100, Some(100))));
        assert_eq!(bar.moved.shown(100, Some(100)), (1000, Some(1000)));
        bar.start(Some(1000), 0);
        let restarted = bar
            .drawn
            .as_ref()
            .map(|drawn| (drawn.position(), drawn.length()));
        assert_eq!(
            restarted,
            Some((0, Some(1000))),
            "a fresh start keeps nothing"
        );
    }

    #[test]
    fn a_long_setup_step_gets_a_line_and_a_short_one_none() {
        let screen = Screen::default();
        let bars = MultiProgress::with_draw_target(ProgressDrawTarget::term_like(Box::new(
            screen.clone(),
        )));
        let hour = Duration::from_secs(3600);
        let look = (false, false);
        drop(Setup::after(
            hour,
            &bars,
            ("Locking", "the project".to_owned()),
            look,
            Arc::default(),
        ));
        assert_eq!(screen.text(), "");
        let setup = Setup::after(
            Duration::ZERO,
            &bars,
            ("Opening", "the cache".to_owned()),
            look,
            Arc::default(),
        );
        assert!(screen.shows("Opening the cache"), "no line for a long step");
        drop(setup);
    }

    /// waiting for each bar's next tick once kept a busy machine showing `9 s left` for 6 s.
    #[test]
    fn a_stop_shows_on_every_bar_at_once() {
        let style = super::super::Style {
            color: false,
            unicode: false,
            interactive: false,
            watched: false,
            links: false,
            accessible: false,
        };
        let mut output =
            super::super::Output::new(style, super::super::Level::Normal, false, false);
        let screen = Screen::default();
        output.bars = Some(MultiProgress::with_draw_target(
            ProgressDrawTarget::term_like(Box::new(screen.clone())),
        ));
        let bar = output.bar("Downloading", "mid.bin".to_owned());
        if let Some(drawn) = &bar.drawn {
            drawn.disable_steady_tick();
        }
        bar.start(Some(1000), 0);
        bar.advance(452);
        let before = screen.text().len();
        output.alone().stopping();
        let drawn = screen.text().split_off(before);
        assert!(drawn.contains("452/1000 B - stopping"), "{drawn}");
    }

    /// a bar can appear before its transfer, as when it waits for the server or checks kept bytes.
    #[test]
    fn a_transfer_counts_its_time_from_its_start_not_from_its_bar() {
        let bars = MultiProgress::with_draw_target(indicatif::ProgressDrawTarget::hidden());
        let bar = Bar::add(
            &bars,
            "Downloading",
            "res.bin".to_owned(),
            (false, false),
            Arc::default(),
        );
        let drawn = bar
            .drawn
            .clone()
            .unwrap()
            .with_elapsed(Duration::from_secs(5));
        let asked = Instant::now();
        bar.start(Some(300), 152);
        let shown = drawn.elapsed();
        assert!(shown <= asked.elapsed(), "{shown:?}");
    }

    /// counting the first burst once read as `58.5 MiB/s` from a server sending 38.1.
    #[test]
    fn the_rate_starts_over_once_a_second_into_a_transfer() {
        let moved = Moved::new(Arc::default());
        let begun = Instant::now();
        moved.restart(begun);
        assert!(!moved.rerate(begun + Duration::from_millis(900)));
        assert!(moved.rerate(begun + RERATE));
        assert!(!moved.rerate(begun + 2 * RERATE), "once a transfer");
        let mib = 1024 * 1024;
        let second = Duration::from_secs(1);
        // 7 MiB arrived at once, then 38 MiB a second: the rate starts over a second in
        moved.mark(begun + second, 7 * mib);
        let rate = moved.rate(7 * mib + 76 * mib, begun + 3 * second);
        assert!((rate - 38.0 * 1_048_576.0).abs() < 1.0, "{rate}");
    }

    /// the rate covers the last 10 to 20 s, so a link slowing from 38 MiB a second to 19 shows 19.
    #[test]
    fn the_rate_follows_a_link_that_slows_within_twenty_seconds() {
        let moved = Moved::new(Arc::default());
        let begun = Instant::now();
        moved.restart(begun);
        let mib = 1024 * 1024;
        let second = Duration::from_secs(1);
        let fast = moved.rate(38 * mib * 11, begun + 11 * second);
        assert!((fast - 38.0 * 1_048_576.0).abs() < 1.0, "{fast}");
        let slower = moved.rate(38 * mib * 11 + 19 * mib * 20, begun + 31 * second);
        assert!((slower - 19.0 * 1_048_576.0).abs() < 1.0, "{slower}");
    }

    #[test]
    fn bars_show_zero_partial_full_and_clamped_fractions() {
        for (done, total, expected) in [
            (0, 100, "--------------------"),
            (25, 100, "#####---------------"),
            (50, 100, "##########----------"),
            (100, 100, "####################"),
            (150, 100, "####################"),
            (0, 0, "####################"),
            (u64::MAX, u64::MAX, "####################"),
        ] {
            let bar = meter(done, total, false);
            assert_eq!(bar, expected);
            assert!(!bar.contains('\u{1b}'));
        }
        assert_eq!(
            meter(25, 100, true),
            "\u{1b}[36m━━━━━\u{1b}[0m\u{1b}[2m━━━━━━━━━━━━━━━\u{1b}[0m"
        );
    }

    /// counters as `amount` shows them, ` · ` between them: the count first where the first is a
    /// size, a rate, which ends in `/s`, then what comes after, the last.
    fn counters(amount: &str) -> Counters {
        let parts: Vec<&str> = amount
            .split(" · ")
            .filter(|part| !part.is_empty())
            .collect();
        let rate = parts.iter().find(|part| part.ends_with("/s"));
        let count = parts
            .first()
            .filter(|part| part.contains('B') && !part.ends_with("/s"));
        let then = parts
            .last()
            .filter(|part| Some(*part) != count && Some(*part) != rate);
        Counters::new(
            count.map(|part| (*part).to_owned()),
            rate.map(|part| (*part).to_owned()),
            then.map(|part| (*part).to_owned()),
        )
    }

    /// [`render`] of a transfer with `bar` as its meter, `percent` done, and `amount` its counters,
    /// keeping the room they grow to for a total of `total`.
    fn drawn(
        name: &str,
        (bar, percent): (&str, u8),
        (amount, total): (&str, u64),
        width: usize,
        color: bool,
    ) -> String {
        let mut counters = counters(amount);
        counters.steady(Some(total));
        render(
            name,
            "Downloading",
            Some((bar, Some(percent))),
            &counters,
            width,
            color,
        )
    }

    #[test]
    fn bars_keep_the_name_and_fit_at_every_fraction() {
        let mib = 1024 * 1024;
        for color in [false, true] {
            for done in [0, 1, 25, 50, 99, 100, 150] {
                let bar = meter(done, 100, color);
                let shown = (bar.as_str(), percent(done, 100));
                for width in [0, 1, 7, 8, 12, 20, 40, 60, 80, 120] {
                    let amount = ("12.3/39.4 MiB · 4.1 MiB/s · 7 s left", 39 * mib);
                    let line = drawn("iris.csv", shown, amount, width, color);
                    assert!(
                        console::measure_text_width(&line) <= width,
                        "{width}: {line}"
                    );
                    if width >= 8 {
                        assert!(line.contains("iris.csv"), "{width}: {line}");
                    }
                    if !color {
                        assert!(!line.contains('\u{1b}'));
                    }
                }
            }
        }
    }

    #[test]
    fn narrow_progress_keeps_the_file_name() {
        let amount = ("1.0/4.0 MiB · 512 KiB/s · 6 s left", 4 << 20);
        let bar = ("#####---------------", 25);
        for width in [128, 100, 80, 60, 40, 20, 12, 8] {
            let line = drawn("iris.csv", bar, amount, width, false);
            assert!(line.contains("iris.csv"), "{width}: {line}");
            assert!(
                console::measure_text_width(&line) <= width,
                "{width}: {line}"
            );
        }
        assert_eq!(drawn("iris.csv", bar, amount, 8, false), "iris.csv");
        let wide = drawn("iris.csv", bar, amount, 128, false);
        assert!(wide.contains("512 KiB/s"), "{wide}");
        assert!(wide.contains("6 s left"), "{wide}");
    }

    /// a counter keeps its unit, and the time left outlasts the rate and the count.
    #[test]
    fn narrow_progress_never_shows_a_number_without_its_unit() {
        let amount = counters("1.0/4.0 MiB · 512 KiB/s · 6 s left");
        for width in 0..=80 {
            let line = render("iris.csv", "Downloading", None, &amount, width, false);
            let counter = line.rsplit(' ').next().unwrap_or_default();
            assert!(
                !counter.starts_with(|c: char| c.is_ascii_digit()),
                "{width}: {line}"
            );
        }
        assert_eq!(
            render("iris.csv", "Downloading", None, &amount, 28, false),
            "iris.csv 1.0 MiB · 6 s left"
        );
        assert_eq!(
            render("iris.csv", "Downloading", None, &amount, 17, false),
            "iris.csv 6 s left"
        );
        assert_eq!(
            render(
                "iris.csv",
                "Downloading",
                None,
                &counters("512 KiB/4.0 MiB · 6 s left"),
                28,
                false
            ),
            "iris.csv 512 KiB · 6 s left"
        );
    }

    /// a hugging face shard's line at 80 and 40 columns once showed no bar, no percent and no time
    /// left: the name is cut in the middle first, the verb goes, the bar gives way to its percent,
    /// then the rate, then the count, and the time left stays.
    #[test]
    fn narrow_progress_cuts_the_name_before_the_progress() {
        let name = "tinystories1/data/train-00001-of-00004-5852b56a2bd28fd9.parquet";
        let bar = ("#################---", 89);
        let amount = ("211.3/236.7 MiB · 31.5 MiB/s · 1 s left", 248_200_000);
        let at = |width| drawn(name, bar, amount, width, false);
        let wide = at(160);
        assert_eq!(wide, format!("Downloading {name} {} {}", bar.0, amount.0));
        let roomy = at(120);
        assert!(roomy.starts_with("Downloading tinystories1/"), "{roomy}");
        assert!(roomy.contains('~') && roomy.contains(bar.0), "{roomy}");
        let eighty = at(80);
        assert!(
            eighty.starts_with("tinystories1/") && eighty.contains('~'),
            "{eighty}"
        );
        assert!(eighty.ends_with(&format!(" 89% {}", amount.0)), "{eighty}");
        let forty = at(40);
        assert!(forty.ends_with(" 89% 211.3 MiB · 1 s left"), "{forty}");
        for width in [160, 120, 80, 40] {
            assert!(
                console::measure_text_width(&at(width)) <= width,
                "{width}: {}",
                at(width)
            );
        }
    }

    /// the cut in a name once moved as the numbers beside it changed width, so the name jumped
    /// with each frame: a transfer's counters keep the room they grow to.
    #[test]
    fn a_names_cut_stays_put_while_its_counters_grow() {
        let name = "imdbtest/plain_text/test-00000-of-00001.parquet";
        let total = 20_447_232;
        let cut = |amount: &str| {
            let line = drawn(name, ("#---", 15), (amount, total), 80, false);
            line.split(' ').next().unwrap_or_default().to_owned()
        };
        let early = cut("2.9/19.5 MiB");
        assert_eq!(early, cut("3.3/19.5 MiB · 979.7 KiB/s · 17 s left"));
        assert_eq!(early, cut("13.3/19.5 MiB · 1022.9 KiB/s · 7 s left"));
        assert!(early.contains('~'), "{early}");
    }

    /// a 6.0 MiB download once moved its name's cut as `0.94/6.0 MiB` became `1.0/6.0 MiB` at 80
    /// columns, and `0.13 MiB` became `1.0 MiB` at 40, and a time left under a minute moved it
    /// too: each counter keeps the most room it took on its bar, and a stall's words take theirs
    /// only while they show.
    #[test]
    fn a_names_cut_stays_put_while_its_counters_narrow() {
        let name = "tinystories1-data-train-00001-of-00004-5852b56a2bd28fd9.parquet";
        let total = 6 << 20;
        for width in [80, 40] {
            let moved = Moved::new(Arc::default());
            let cut = |done: u64, then: Option<&str>, timed: bool| {
                let mut counters = Counters {
                    timed,
                    ..Counters::new(
                        (done > 0).then(|| fraction(done, total)),
                        Some("1.0 MiB/s".to_owned()),
                        then.map(str::to_owned),
                    )
                };
                counters.steady(Some(total));
                moved.hold(&mut counters);
                let meter = Some(("#---", Some(15)));
                let line = render(name, "Downloading", meter, &counters, width, false);
                line.split(' ').next().unwrap_or_default().to_owned()
            };
            // as a link once read at 40 columns, its time foreseen from its first frame
            let first = cut(0, Some("under 10 s left"), true);
            assert!(first.contains('~'), "{width}: {first}");
            for (done, then) in [
                (130 << 10, "under 10 s left"),
                (1 << 20, "1 min 04 s left"),
                (2 << 20, "59 s left"),
            ] {
                assert_eq!(first, cut(done, Some(then), true), "{width}: {done} {then}");
            }
            cut(3 << 20, Some("no data for 1 min 05 s"), false);
            let after = cut(4 << 20, Some("3 s left"), true);
            assert_eq!(first, after, "{width}: after a stall");
        }
    }

    #[test]
    fn narrow_progress_shortens_long_names_in_the_middle() {
        for (width, expected) in [
            (12, "speci~stq.gz"),
            (8, "spe~q.gz"),
            (5, "sp~gz"),
            (1, "~"),
            (0, ""),
        ] {
            let line = drawn(
                "specimen_replicate_01.fastq.gz",
                ("#####---------------", 25),
                ("1.0/4.0 MiB · 512 KiB/s · 6 s left", 4 << 20),
                width,
                false,
            );
            assert_eq!(line, expected, "{width}");
            assert!(
                console::measure_text_width(&line) <= width,
                "{width}: {line}"
            );
        }
    }

    #[test]
    fn progress_width_counts_unicode_columns() {
        for name in [
            "測定_非常に長い試料名.csv",
            "mesures_e\u{301}chantillon.csv",
        ] {
            for (bar, amount) in [
                (
                    Some(("\x1b[36m━━━━━━━━━━━━━━━━━━━━\x1b[0m", Some(25))),
                    "1.0/4.0 MiB · 6 s left",
                ),
                (None, "1.0 MiB · 6 s left"),
                (None, ""),
            ] {
                let amount = counters(amount);
                // the verb, and the bar or nothing, each with its space
                let around = 12 + if bar.is_some() { 21 } else { 0 };
                for width in 0..=100 {
                    let line = render(
                        name,
                        "\x1b[36mDownloading\x1b[0m",
                        bar,
                        &amount,
                        width,
                        true,
                    );
                    assert!(
                        console::measure_text_width(&line) <= width,
                        "{width}: {line}"
                    );
                    assert!(!line.contains('\n'), "{width}: {line}");
                    let beside = around
                        + console::measure_text_width(name)
                        + 1
                        + console::measure_text_width(&amount.said());
                    if width >= beside {
                        assert!(line.contains(name), "{width}: {line}");
                    } else if width > 0 {
                        assert!(line.contains(name) || line.contains('~'), "{width}: {line}");
                    }
                }
            }
        }
    }

    /// even the quickest batch says it began: a download once its size is known, a lookup at once.
    /// a download whose sizes were all known before it started said so in its plan.
    #[test]
    fn a_network_batch_tells_its_start_at_once() {
        let pace = (Duration::from_mins(1), Duration::from_mins(2));
        let (heard, _reported, say) = ear();
        drop(Ticker::paced(
            "Downloading",
            "3 files".to_owned(),
            (300, 0),
            pace,
            (true, None),
            say,
        ));
        assert!(heard.lock().unwrap().is_empty());
        // where no plan was told, as under --json, the batch says it began itself
        let (heard, _reported, say) = ear();
        drop(Ticker::paced(
            "Downloading",
            "3 files".to_owned(),
            (300, 0),
            pace,
            (false, None),
            say,
        ));
        assert_eq!(*heard.lock().unwrap(), ["Downloading 3 files · 300 B"]);
        let (heard, _reported, say) = ear();
        let ticker = Ticker::paced(
            "Downloading",
            "iris.csv".to_owned(),
            (0, 1),
            pace,
            (true, None),
            say,
        );
        assert!(heard.lock().unwrap().is_empty());
        ticker.sized(32, None);
        assert_eq!(*heard.lock().unwrap(), ["Downloading iris.csv · 32 B"]);
        drop(ticker);
        // a download refused before its body tells no size, and never began
        let (heard, _reported, say) = ear();
        let ticker = Ticker::paced(
            "Downloading",
            "iris.csv".to_owned(),
            (0, 1),
            pace,
            (true, None),
            say,
        );
        ticker.sized(0, None);
        drop(ticker);
        assert!(heard.lock().unwrap().is_empty());
        for verb in ["Resolving", "Searching"] {
            let (heard, _reported, say) = ear();
            drop(Ticker::paced(
                verb,
                "iris".to_owned(),
                (0, 1),
                pace,
                (true, None),
                say,
            ));
            assert_eq!(*heard.lock().unwrap(), [format!("{verb} iris")]);
        }
    }

    /// once the last size is known, so is how long the batch takes, which its start line says.
    #[test]
    fn a_download_batch_says_its_time_once_its_sizes_are_known() {
        let pace = (Duration::from_mins(1), Duration::from_mins(2));
        let (heard, _reported, say) = ear();
        let ticker = Ticker::paced(
            "Downloading",
            "2 files".to_owned(),
            (0, 2),
            pace,
            (true, None),
            say,
        );
        ticker.sized(32, None);
        assert!(heard.lock().unwrap().is_empty());
        ticker.sized(64, Some("under 10 s"));
        drop(ticker);
        assert_eq!(
            *heard.lock().unwrap(),
            ["Downloading 2 files · 96 B · under 10 s"]
        );
    }

    /// what a batch does is said once, by whichever says it first: a step told at once keeps the
    /// batch's plain lines from saying it again, and a step after those lines says nothing.
    #[test]
    fn what_a_batch_does_is_said_once_by_whoever_says_it_first() {
        let hour = Duration::from_secs(3600);
        let (heard, _reported, say) = ear();
        let ticker = Ticker::paced(
            "Placing",
            "10 files".to_owned(),
            (0, 1),
            (hour, hour),
            (false, None),
            say,
        );
        let begun = ticker.begun();
        assert!(begun.first());
        assert!(!ticker.shared.as_ref().unwrap().begin(None));
        assert!(!begun.first());
        drop(ticker);
        assert!(heard.lock().unwrap().is_empty());

        let (heard, _reported, say) = ear();
        let ticker = Ticker::paced(
            "Placing",
            "10 files".to_owned(),
            (0, 1),
            (hour, hour),
            (false, None),
            say,
        );
        assert!(ticker.shared.as_ref().unwrap().begin(None));
        assert!(!ticker.begun().first());
        drop(ticker);
        assert_eq!(*heard.lock().unwrap(), ["Placing 10 files"]);

        assert!(Ticker::quiet().begun().first());
    }

    /// work on this machine says nothing until it lasts a moment.
    #[test]
    fn a_quick_batch_adds_no_line() {
        let (heard, _reported, say) = ear();
        let pace = (Duration::from_secs(5), Duration::from_secs(10));
        drop(Ticker::paced(
            "Checking",
            "3 files".to_owned(),
            (300, 0),
            pace,
            (true, None),
            say,
        ));
        assert!(heard.lock().unwrap().is_empty());
    }

    #[test]
    fn a_long_batch_says_what_it_does_then_how_far_it_got() {
        let (heard, reported, say) = ear();
        let pace = (Duration::from_millis(30), Duration::from_millis(90));
        let ticker = Ticker::paced(
            "Downloading",
            "3 files".to_owned(),
            (200, 1),
            pace,
            (true, None),
            say,
        );
        ticker.sized(100, None);
        ticker.sized(7, None);
        ticker.advance(150);
        for _ in 0..2 {
            reported.recv_timeout(Duration::from_mins(1)).unwrap();
        }
        drop(ticker);
        let heard = heard.lock().unwrap();
        assert_eq!(heard[0], "Downloading 3 files · 300 B", "{heard:?}");
        assert!(heard.len() >= 2, "{heard:?}");
        assert!(
            heard[1..]
                .iter()
                .all(|line| line.starts_with("Downloading 150 B of 300 B · ")),
            "{heard:?}"
        );
    }

    /// two decimals below one unit stop a jump from `1002.0 MiB/2.0 GiB` to `1.0/2.0 GiB`.
    #[test]
    fn a_fraction_counts_in_the_unit_of_its_total() {
        let mib = 1024 * 1024;
        assert_eq!(fraction(1024, 4096), "1.0/4.0 KiB");
        assert_eq!(fraction(512, 4096), "0.50/4.0 KiB");
        assert_eq!(fraction(1, 4), "1/4 B");
        assert_eq!(fraction(0, 0), "0/0 B");
        assert_eq!(fraction(1002 * mib, 2048 * mib), "0.98/2.0 GiB");
        assert_eq!(fraction(1946 * mib, 2048 * mib), "1.9/2.0 GiB");
        assert_eq!(fraction(0, 2048 * mib), "0.00/2.0 GiB");
        assert_eq!(fraction(1023 * mib + mib / 2, 2048 * mib), "1.0/2.0 GiB");
        assert_eq!(fraction(9 * 1024, 136 * mib), "0.01/136.0 MiB");
    }

    /// a wait with nothing counted says how long it has lasted, while a transfer once showed
    /// `1.1/4.2 MiB · 2 s`, its time so far read as time left: a transfer shows no bare time.
    #[test]
    fn a_bar_shows_how_long_it_has_waited_past_two_seconds() {
        let second = Duration::from_secs(1);
        let busy = Some(Duration::ZERO);
        assert_eq!(
            measured(0, None, 0.0, second, Duration::ZERO, busy, false),
            ""
        );
        assert_eq!(
            measured(1024, None, 512.0, 12 * second, Duration::ZERO, busy, false),
            "1.0 KiB · 512 B/s"
        );
        assert_eq!(
            measured(0, Some(4096), 0.0, 12 * second, Duration::ZERO, busy, false),
            "12 s"
        );
        assert_eq!(
            measured(1024, Some(4096), 0.0, second, Duration::ZERO, busy, false),
            "1.0/4.0 KiB"
        );
        assert_eq!(
            measured(
                1024,
                Some(4096),
                512.0,
                12 * second,
                6 * second,
                busy,
                false
            ),
            "1.0/4.0 KiB · 512 B/s · 6 s left"
        );
    }

    #[test]
    fn a_download_that_stopped_receiving_says_so_instead_of_a_fading_rate() {
        let second = Duration::from_secs(1);
        assert_eq!(
            measured(
                1024,
                Some(4096),
                300.0,
                40 * second,
                10 * second,
                Some(12 * second),
                false
            ),
            "1.0/4.0 KiB · no data for 12 s"
        );
        assert_eq!(
            measured(
                0,
                Some(4096),
                0.0,
                9 * second,
                Duration::ZERO,
                Some(9 * second),
                false
            ),
            "no data for 9 s"
        );
        assert_eq!(
            measured(
                1024,
                Some(4096),
                512.0,
                12 * second,
                6 * second,
                Some(2 * second),
                false
            ),
            "1.0/4.0 KiB · no data for 2 s",
            "two seconds of nothing is a stall to tell"
        );
        assert_eq!(
            measured(
                1024,
                Some(4096),
                512.0,
                12 * second,
                second / 2,
                Some(second + second / 2),
                false
            ),
            "1.0/4.0 KiB · 512 B/s",
            "a second of nothing drops a time left it cannot keep to"
        );
        assert_eq!(
            measured(
                1024,
                Some(4096),
                512.0,
                12 * second,
                6 * second,
                Some(second / 2),
                false
            ),
            "1.0/4.0 KiB · 512 B/s · 6 s left",
            "a gap of a moment is ordinary"
        );
    }

    /// a batch ending in 15 s once had a file of it needing 29 s on the line under it: a file's bar
    /// in a batch tells no time left, the batch's bar does. until a bar can tell its own, it says
    /// about how long fetchloom foresaw the transfer taking.
    #[test]
    fn one_time_left_shows_for_a_batch_and_a_foreseen_one_until_then() {
        let second = Duration::from_secs(1);
        let mib = 1024 * 1024;
        let rate = 1.9 * 1_048_576.0;
        let at = |elapsed, left: Option<Duration>| {
            let timed = Timed {
                left,
                foreseen: Some("about 20 s".to_owned()),
            };
            measure(
                (8 * mib, Some(34 * mib)),
                rate,
                elapsed,
                timed,
                Some(Duration::ZERO),
                false,
            )
            .said()
        };
        assert_eq!(
            at(4 * second, Some(15 * second)),
            "8.0/34.0 MiB · 1.9 MiB/s · 15 s left"
        );
        assert_eq!(
            at(2 * second, Some(15 * second)),
            "8.0/34.0 MiB · about 20 s left",
            "a rate yet to settle has the time foreseen"
        );
        assert_eq!(
            at(4 * second, None),
            "8.0/34.0 MiB · 1.9 MiB/s",
            "a file in a batch"
        );
        assert_eq!(at(2 * second, None), "8.0/34.0 MiB");
        let lengthless = Timed {
            left: Some(Duration::ZERO),
            foreseen: Some("about 20 s".to_owned()),
        };
        let busy = Some(Duration::ZERO);
        assert_eq!(
            measure((0, None), 0.0, second, lengthless, busy, false).said(),
            "",
            "a bar yet to learn its length"
        );
    }

    /// a bar that waits for another fetchloom or checks the bytes an earlier run kept is no
    /// transfer, so it never shows the time its download was foreseen to take.
    #[test]
    fn a_foreseen_time_shows_only_while_a_bar_downloads() {
        let moved = Moved::new(Arc::default());
        *moved.foreseen.lock().unwrap() = Some("about 20 s".to_owned());
        let second = Duration::from_secs(1);
        let downloading = moved.timed("Downloading", second);
        assert_eq!(downloading.foreseen.as_deref(), Some("about 20 s"));
        assert_eq!(downloading.left, Some(second));
        for verb in ["Waiting", "Checking"] {
            assert_eq!(moved.timed(verb, second).foreseen, None, "{verb}");
        }
        moved.untimed.store(true, Ordering::Relaxed);
        assert_eq!(moved.timed("Downloading", second).left, None);
    }

    /// a link sending its bytes in bursts once had its time left climb between them, 17 s to 20 s
    /// in 0.3 s: the rate runs up to the last bytes, and the time left counts down from them.
    #[test]
    fn a_bursty_links_time_left_only_counts_down() {
        let moved = Moved::new(Arc::default());
        let begun = Instant::now();
        moved.restart(begun);
        let second = Duration::from_secs(1);
        moved.mark(begun + second, 0);
        let length = Some(20 * 1024 * 1024);
        let mut lefts = Vec::new();
        // 400 KiB every 0.4 s from a second in, looked at every 0.1 s
        for burst in 1..=10_u32 {
            let arrived = begun + second + burst * Duration::from_millis(400);
            moved.heard_at(arrived);
            for look in 0..4 {
                let now = arrived + look * Duration::from_millis(100);
                let (_, left) = moved.pace(u64::from(burst) * 400 * 1024, length, now);
                lefts.push(left);
            }
        }
        assert!(lefts.windows(2).all(|pair| pair[1] <= pair[0]), "{lefts:?}");
        assert!(
            lefts[0] > Duration::from_secs(19) && lefts[39] > Duration::from_secs(15),
            "{lefts:?}"
        );
    }

    /// the bar never says no data came, which would send a person to check the network.
    #[test]
    fn a_download_held_by_the_disk_says_so() {
        let second = Duration::from_secs(1);
        let mib = 1024 * 1024;
        assert_eq!(
            waiting(680 * mib, Some(800 * mib), None, Some(3 * second))
                .map(|counters| counters.said())
                .as_deref(),
            Some("680.0/800.0 MiB · waiting for the disk · 3 s")
        );
        assert_eq!(
            waiting(680 * mib, Some(800 * mib), None, Some(second)),
            None,
            "a moment behind is ordinary"
        );
        assert_eq!(waiting(680 * mib, Some(800 * mib), None, None), None);
        assert_eq!(
            waiting(0, None, Some("try 2 of 5".to_owned()), Some(3 * second))
                .map(|counters| counters.said())
                .as_deref(),
            Some("try 2 of 5")
        );
    }

    /// a full bar adds how long it has waited after a moment, never saying no data came.
    #[test]
    fn a_full_bar_says_it_waits_for_the_disk() {
        let second = Duration::from_secs(1);
        assert_eq!(
            measured(
                57,
                Some(57),
                4.0,
                15 * second,
                Duration::ZERO,
                Some(12 * second),
                false
            ),
            "57/57 B · waiting for the disk · 12 s"
        );
        assert_eq!(
            measured(
                57,
                Some(57),
                40.0,
                second,
                Duration::ZERO,
                Some(Duration::ZERO),
                false
            ),
            "57/57 B · waiting for the disk"
        );
    }

    /// the first line says what a batch does, the later ones how far it got.
    #[test]
    fn a_plain_line_says_a_download_waits_for_the_disk_once_every_byte_is_in() {
        let ten = Duration::from_secs(10);
        let told = |verb, done, total| report(verb, "iris.csv", (done, total), ten, false);
        assert_eq!(
            report("Downloading", "iris.csv", (57, Some(57)), ten, true),
            ("Downloading", "iris.csv · 57 B".to_owned())
        );
        assert_eq!(
            told("Downloading", 20, Some(57)),
            ("Downloading", "20 B of 57 B · 10 s".to_owned())
        );
        assert_eq!(
            told("Downloading", 57, Some(57)),
            (
                "Waiting",
                "for the disk to finish writing iris.csv · 10 s".to_owned()
            )
        );
        assert_eq!(
            told("Downloading", 0, Some(0)),
            ("Downloading", "0 B of 0 B · 10 s".to_owned()),
            "a batch of empty files has had no byte yet"
        );
        assert_eq!(
            told("Unpacking", 20, Some(57)),
            ("Unpacking", "20 B of 57 B · 10 s".to_owned())
        );
    }

    /// the line says it is finishing while files are written, as its bar does, not a full count.
    #[test]
    fn a_plain_line_says_an_unpack_that_read_every_byte_is_finishing() {
        let ten = Duration::from_secs(10);
        assert_eq!(
            report("Unpacking", "set/set.tar", (57, Some(57)), ten, false),
            ("Finishing", "set/set.tar · 10 s".to_owned())
        );
        assert_eq!(
            report("Unpacking", "2 archives", (57, Some(80)), ten, false),
            ("Unpacking", "57 B of 80 B · 10 s".to_owned())
        );
        assert_eq!(
            report("Unpacking", "set/set.tar", (57, Some(57)), ten, true),
            ("Unpacking", "set/set.tar · 57 B".to_owned()),
            "the first line says what the batch does"
        );
    }

    /// the bar says which try of how many and whose limit it waits for, never that no data came.
    #[test]
    fn a_bar_waiting_to_try_again_says_which_try_and_when() {
        let second = Duration::from_secs(1);
        assert_eq!(
            next_try((3, 5, false), second * 2 + second / 3, None),
            "try 3 of 5 · next in 3 s"
        );
        assert_eq!(
            next_try((6, 15, false), second * 3, Some("127.0.0.1")),
            "waiting for 127.0.0.1's rate limit · try 6 of 15 · next in 3 s"
        );
        assert_eq!(
            next_try((4, 5, false), Duration::ZERO, None),
            "try 4 of 5",
            "under way"
        );
        let bars = MultiProgress::with_draw_target(indicatif::ProgressDrawTarget::hidden());
        let bar = Bar::add(
            &bars,
            "Downloading",
            "y.csv".to_owned(),
            (false, false),
            Arc::default(),
        );
        bar.retrying((2, 5, false), second * 4, None);
        assert_eq!(
            bar.moved.retrying().as_deref(),
            Some("try 2 of 5 · next in 4 s")
        );
        bar.start(Some(10), 0);
        assert_eq!(
            bar.moved.retrying(),
            None,
            "a try that got an answer is no wait"
        );
    }

    /// however long a stop takes, the bar says stopping, not no data or a stale rate and time left.
    #[test]
    fn a_bar_says_it_is_stopping_once_a_stop_is_asked_for() {
        let second = Duration::from_secs(1);
        let mib = 1024 * 1024;
        assert_eq!(
            measured(
                74 * mib,
                Some(300 * mib),
                4.6 * 1_048_576.0,
                60 * second,
                48 * second,
                Some(12 * second),
                true
            ),
            "74.0/300.0 MiB · stopping"
        );
        assert_eq!(
            measured(
                0,
                Some(4096),
                0.0,
                9 * second,
                Duration::ZERO,
                Some(second),
                true
            ),
            "stopping"
        );
        assert_eq!(
            measured(
                57,
                Some(57),
                4.0,
                15 * second,
                Duration::ZERO,
                Some(12 * second),
                true
            ),
            "57/57 B · stopping"
        );
        assert_eq!(
            measured(
                1024,
                Some(4096),
                0.0,
                3 * second,
                Duration::ZERO,
                None,
                true
            ),
            "1.0/4.0 KiB · stopping",
            "unpacking stops too"
        );
    }

    #[test]
    fn the_time_left_waits_until_the_rate_can_carry_it() {
        let second = Duration::from_secs(1);
        let busy = Some(Duration::ZERO);
        let mib = 1024 * 1024;
        assert_eq!(
            measured(
                9 * 1024,
                Some(136 * mib),
                2970.0,
                3 * second,
                801 * 60 * second,
                busy,
                false
            ),
            "0.01/136.0 MiB · 2.9 KiB/s",
            "three seconds at the start of a slow link cannot tell thirteen hours"
        );
        assert_eq!(
            measured(
                9 * mib,
                Some(136 * mib),
                2.0 * 1_048_576.0,
                10 * second,
                64 * second,
                busy,
                false
            ),
            "9.0/136.0 MiB · 2.0 MiB/s · 1 min 04 s left"
        );
        assert_eq!(
            measured(
                1024,
                Some(136 * mib),
                2970.0,
                30 * second,
                801 * 60 * second,
                busy,
                false
            ),
            "0.00/136.0 MiB · 2.9 KiB/s · 801 min 00 s left",
            "half a minute of a steady slow rate is worth telling"
        );
        let rate = 4.0 * 1_048_576.0;
        let after =
            |done, left| measured(done, Some(20 * mib), rate, 5 * second, left, busy, false);
        assert_eq!(
            after(13 * mib, Duration::from_millis(1750)),
            "13.0/20.0 MiB · 4.0 MiB/s · 2 s left",
            "a time left rounds up, as a wait does"
        );
        assert_eq!(
            after(17 * mib, Duration::from_millis(750)),
            "17.0/20.0 MiB · 4.0 MiB/s · 1 s left",
            "a transfer under way never has 0 s left"
        );
    }

    /// too slow a rate once panicked the conversion, which a release build turns into an abort.
    #[test]
    fn a_time_left_beyond_counting_is_the_longest() {
        assert_eq!(time_left(u64::MAX, 1e-6), Duration::MAX);
        assert_eq!(
            time_left(3 * 1024 * 1024, 2.0 * 1_048_576.0),
            Duration::from_millis(1500)
        );
    }

    #[test]
    fn a_wait_without_bytes_says_what_it_waits_for() {
        let (heard, reported, say) = ear();
        let pace = (Duration::from_millis(30), Duration::from_millis(90));
        let ticker = Ticker::paced(
            "Resolving",
            "zenodo:1".to_owned(),
            (0, 1),
            pace,
            (true, None),
            say,
        );
        for _ in 0..2 {
            reported.recv_timeout(Duration::from_mins(1)).unwrap();
        }
        drop(ticker);
        let heard = heard.lock().unwrap();
        assert_eq!(heard[0], "Resolving zenodo:1", "{heard:?}");
        assert!(heard.len() >= 2, "{heard:?}");
        assert!(
            heard[1..]
                .iter()
                .all(|line| line.starts_with("Resolving zenodo:1 · ")),
            "{heard:?}"
        );
    }

    /// a list of bars waiting to join, the second at `start` plus `offsets`.
    fn waiting_at(start: Instant, offsets: &[Duration]) -> (Waiting, Vec<Arc<Later>>) {
        let bars = MultiProgress::with_draw_target(ProgressDrawTarget::hidden());
        let mut waiting = Waiting::default();
        let mut kept = Vec::new();
        for offset in offsets {
            let later = Arc::new(Later {
                bar: ProgressBar::hidden(),
                bars: bars.clone(),
                settled: Mutex::new(false),
            });
            waiting
                .due
                .push_back((start + *offset, Arc::downgrade(&later)));
            kept.push(later);
        }
        (waiting, kept)
    }

    /// bars join in the order they came, each once its time came, and the thread that adds them
    /// sleeps at least [`JOINS_EVERY`] between rounds, however close their times.
    #[test]
    fn a_bar_joins_once_its_moment_passed() {
        let start = Instant::now();
        let second = Duration::from_secs(1);
        let offsets = [second, second + Duration::from_millis(1), 2 * second];
        let (mut waiting, kept) = waiting_at(start, &offsets);
        assert_eq!(waiting.next(start), Some(second));
        assert!(waiting.take_due(start + second / 2).is_empty());
        let due = waiting.take_due(start + second);
        assert_eq!(due.len(), 1);
        assert!(due[0].ptr_eq(&Arc::downgrade(&kept[0])));
        assert_eq!(waiting.next(start + second), Some(JOINS_EVERY));
        let due = waiting.take_due(start + 3 * second);
        assert_eq!(due.len(), 2);
        assert!(due[1].ptr_eq(&Arc::downgrade(&kept[2])));
        assert_eq!(waiting.next(start + 3 * second), None);
    }

    /// an output on `screen` for a command that started at `started`, whose import bars wait until
    /// it has lasted `moment`.
    fn importing_on(screen: &Screen, moment: Duration, started: Instant) -> super::super::Output {
        let mut output = super::super::Output::on_screen(screen.clone());
        output.untold = moment;
        output.started = started;
        output
    }

    /// the times the import bars waiting to join are due.
    fn due_of(output: &super::super::Output) -> Vec<Instant> {
        let waiting = output.joining.as_ref().unwrap().waiting.lock().unwrap();
        waiting.due.iter().map(|(at, _)| *at).collect()
    }

    /// an import that copies and finishes within its command's moment draws no frame, and one
    /// whose moment has passed draws both its bars at once.
    #[test]
    fn an_import_that_ends_within_its_moment_draws_no_bar() {
        let hour = Duration::from_secs(3600);
        let screen = Screen::default();
        let output = importing_on(&screen, hour, Instant::now());
        let copying = output.short_bar("Copying", "10 files".to_owned());
        copying.start(Some(1000), 0);
        copying.advance(1000);
        drop(copying);
        drop(output.short_wait("Finishing", "10 files".to_owned()));
        assert_eq!(screen.records(), Vec::<String>::new());

        let screen = Screen::default();
        let output = importing_on(&screen, Duration::ZERO, Instant::now());
        let copying = output.short_bar("Copying", "10 files".to_owned());
        copying.start(Some(1000), 0);
        assert!(screen.shows("Copying"), "{:?}", screen.records());
        drop(copying);
        let finishing = output.short_bar("Finishing", "10 files".to_owned());
        finishing.start(None, 0);
        assert!(screen.shows("Finishing"), "{:?}", screen.records());
    }

    /// the moment counts from the command's start, not from the import's or a step's: every step
    /// that begins before it joins when the command has lasted it, at one time.
    #[test]
    fn the_moment_counts_from_the_command_s_start_across_steps() {
        let hour = Duration::from_secs(3600);
        let started = Instant::now();
        let output = importing_on(&Screen::default(), hour, started);
        let copying = output.short_bar("Copying", "10 files".to_owned());
        drop(copying);
        let finishing = output.short_bar("Finishing", "10 files".to_owned());
        assert_eq!(due_of(&output), [started + hour, started + hour]);
        drop(finishing);
    }

    /// an import's moment delays the import's bars alone: a download's bar made while the import's
    /// waits draws at once.
    #[test]
    fn an_import_s_moment_leaves_other_bars_as_they_were() {
        let hour = Duration::from_secs(3600);
        let screen = Screen::default();
        let output = importing_on(&screen, hour, Instant::now());
        let copying = output.short_bar("Copying", "10 files".to_owned());
        copying.start(Some(1000), 0);
        let bar = output.bar("Downloading", "mid.bin".to_owned());
        bar.start(Some(1000), 0);
        assert!(screen.shows("mid.bin"), "{:?}", screen.records());
        assert!(!screen.text().contains("Copying"), "{:?}", screen.records());
        drop((copying, bar));
    }

    /// a walk before copying begins does not push the first bar back: it is due a moment from the
    /// command's start, not the copying's, so a person sees progress about a moment after pressing
    /// enter, whatever came before the copying.
    #[test]
    fn a_long_walk_before_copying_still_shows_the_first_bar_a_moment_from_the_start() {
        let hour = Duration::from_secs(3600);
        // a walk short enough to subtract from any machine's clock, which windows counts from boot
        let started = Instant::now().checked_sub(Duration::from_secs(9)).unwrap();
        let output = importing_on(&Screen::default(), hour, started);
        let copying = output.short_bar("Copying", "10 files".to_owned());
        assert_eq!(due_of(&output), [started + hour]);
        drop(copying);
    }
}
