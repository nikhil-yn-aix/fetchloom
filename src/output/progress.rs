//! progress while downloads and unpacking run: bars on a terminal a person watches, and plain
//! lines now and then anywhere else.

use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
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
}

/// a download's next try, as its bar tells it.
struct Retrying {
    next: u32,
    of: u32,
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
        Some(next_try(retry.next, retry.of, left, retry.limit.as_deref()))
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
        let millis = u64::try_from(self.born.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.at.store(millis, Ordering::Relaxed);
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

    /// a failed try, after which try `next` of at most `of` goes out after `wait`, once the rate
    /// limit of the host `limit` names resets when it names one.
    pub(crate) fn retrying(&self, next: u32, of: u32, wait: Duration, limit: Option<String>) {
        if self.drawn.is_some() {
            *self
                .moved
                .retry
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = Some(Retrying {
                next,
                of,
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
        if let Some(bar) = &self.drawn {
            bar.finish_and_clear();
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
                let bar = if matches!(verb, "Downloading" | "Unpacking" | "Checking") {
                    total.map_or_else(|| meter(0, 1, color), |total| meter(done, total, color))
                } else {
                    String::new()
                };
                // unpacking and checking read local files, whose pauses mean nothing
                let quiet = (verb == "Downloading").then(|| moved.quiet());
                let stopping = moved.stopping.load(Ordering::Relaxed);
                let behind = quiet.filter(|_| moved.behind.load(Ordering::Relaxed));
                let amount = waiting(done, total, moved.retrying(), behind)
                    .filter(|_| !stopping)
                    .unwrap_or_else(|| {
                        // this run's bytes over its own time, as the time left is
                        let rate = moved.rate(state.pos(), Instant::now());
                        let left = state
                            .len()
                            .filter(|_| rate > 0.0)
                            .map_or(Duration::ZERO, |length| {
                                time_left(length.saturating_sub(state.pos()), rate)
                            });
                        measure(done, total, rate, state.elapsed(), left, quiet, stopping)
                    });
                let width = usize::from(console::Term::stderr().size().1);
                let line = render(&name, &heading, &bar, &amount, width, color);
                let _ = out.write_str(&super::shown(&line, unicode));
            },
        )
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

/// the fewest columns a name keeps beside its counters before the counters give way.
const NAME_LEAST: usize = 16;

/// facts before decoration: as the terminal narrows, the bar goes first, then the verb, then the
/// name down to [`NAME_LEAST`], then the counters from the end; a name alone is cut to fit.
fn render(name: &str, verb: &str, bar: &str, amount: &str, width: usize, color: bool) -> String {
    let dim = super::paint(color, anstyle::Style::new().dimmed());
    let line = |parts: &[&str], amount: &str| {
        let amount = if amount.is_empty() {
            String::new()
        } else {
            format!("{dim}{amount}{dim:#}")
        };
        parts
            .iter()
            .copied()
            .chain([amount.as_str()])
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    };
    let fits = |shown: &str| console::measure_text_width(shown) <= width;
    for shown in [
        line(&[verb, name, bar], amount),
        line(&[verb, name], amount),
    ] {
        if fits(&shown) {
            return shown;
        }
    }
    let mut kept = std::borrow::Cow::Borrowed(amount);
    loop {
        let shown = line(&[name], &kept);
        if fits(&shown) {
            return shown;
        }
        if kept.is_empty() {
            break;
        }
        let room = width.saturating_sub(console::measure_text_width(&kept) + 1);
        if room >= NAME_LEAST {
            return line(&[&middle(name, room)], &kept);
        }
        let shorter = match kept.rsplit_once(" · ") {
            Some((head, _)) => head.to_owned(),
            // a fraction that shares one unit, as in 1.0/4.0 MiB, keeps that unit
            None => kept
                .split_once('/')
                .map_or_else(String::new, |(done, total)| match total.rsplit_once(' ') {
                    Some((_, unit)) if !done.contains(' ') => format!("{done} {unit}"),
                    _ => done.to_owned(),
                }),
        };
        kept = shorter.into();
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

/// the counters: elapsed time until a settled rate gives a time left, `no data` once `quiet`, which
/// local work leaves none, passes [`STALL`], the disk once every byte is in, and `stopping` alone.
fn measure(
    done: u64,
    total: Option<u64>,
    rate: f64,
    elapsed: Duration,
    left: Duration,
    quiet: Option<Duration>,
    stopping: bool,
) -> String {
    let mut parts = Vec::new();
    if done > 0 {
        parts.push(counted(done, total));
    }
    if stopping {
        parts.push("stopping".to_owned());
        return parts.join(" · ");
    }
    if let Some(quiet) = quiet {
        if every_byte_in(done, total) {
            parts.push("waiting for the disk".to_owned());
            if quiet >= MOMENT {
                parts.push(units::seconds(quiet));
            }
            return parts.join(" · ");
        }
        if quiet >= STALL {
            parts.push(format!("no data for {}", units::seconds(quiet)));
            return parts.join(" · ");
        }
    }
    let settled = done > 0 && rate > 0.0 && elapsed >= SETTLE;
    // a download that just went quiet still has a rate, but no time left it can keep to
    let still = quiet.is_some_and(|quiet| quiet >= STILL);
    let foreseen = settled
        && !still
        && total.is_some()
        && (elapsed >= STEADY || left <= elapsed.saturating_mul(REACH));
    if settled {
        parts.push(units::speed(rate));
        if foreseen {
            parts.push(format!("{} left", units::seconds(whole_up(left))));
        }
    }
    if elapsed >= MOMENT && !foreseen && !still {
        parts.push(units::seconds(elapsed));
    }
    parts.join(" · ")
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
) -> Option<String> {
    let what = match (retry, behind) {
        (Some(next), _) => next,
        (None, Some(quiet)) if quiet >= STALL && !every_byte_in(done, total) => {
            format!("waiting for the disk · {}", units::seconds(quiet))
        }
        _ => return None,
    };
    Some(if done > 0 {
        format!("{} · {what}", counted(done, total))
    } else {
        what
    })
}

/// how far a bar got: `done` of `total` when it is known.
fn counted(done: u64, total: Option<u64>) -> String {
    total.map_or_else(|| units::size(done), |total| fraction(done, total))
}

/// what a bar says of the try its download waits to make, `left` from now: which try of how
/// many, and whose rate limit it waits for when `limit` names a host.
fn next_try(next: u32, of: u32, left: Duration, limit: Option<&str>) -> String {
    let mut parts = Vec::new();
    if let Some(host) = limit {
        parts.push(format!("waiting for {host}'s rate limit"));
    }
    parts.push(format!("try {next} of {of}"));
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

    /// says what the batch does, unless it has. whether it said so now.
    fn begin(&self) -> bool {
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
        (self.say)(verb, &text);
        true
    }
}

/// how long a batch runs before its first plain line, or none for no lines. debug builds take
/// `FETCHLOOM_TEST_TICKER` from tests: `off` drops the lines, any other value waits 0.1 s.
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
    /// up to `total`, with `unknown` more files whose downloads have yet to tell theirs.
    pub(super) fn start(
        verb: &'static str,
        what: String,
        (total, unknown): (u64, usize),
        moment: Duration,
        say: impl Fn(&str, &str) + Send + Sync + 'static,
    ) -> Ticker {
        Ticker::paced(verb, what, (total, unknown), (moment, EVERY), say)
    }

    fn paced(
        verb: &'static str,
        what: String,
        (total, unknown): (u64, usize),
        pace: (Duration, Duration),
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
        // said before the work starts, so even a batch that ends at once has said it began
        if shared.first == First::Now || (shared.first == First::Sized && unknown == 0) {
            shared.begin();
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

    /// adds the size a download just told. a download that knows its whole batch's size now says
    /// what it does, unless it told none, as one refused before its body, which never began.
    pub(crate) fn sized(&self, bytes: u64) {
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
                shared.begin();
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
        if shared.begin() || next == moment {
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

    /// a terminal that keeps what bars draw on it, and says when that changes.
    #[derive(Debug, Default, Clone)]
    struct Drawn(Arc<Mutex<Vec<String>>>, Arc<Condvar>);

    impl Drawn {
        fn draw(&self, text: &str) {
            self.0.lock().unwrap().push(text.to_owned());
            self.1.notify_all();
        }

        /// whether what was drawn holds `text`, waiting for it up to 30 s, which only a broken
        /// test reaches.
        fn shows(&self, text: &str) -> bool {
            let drawn = self.0.lock().unwrap();
            let held = |drawn: &Vec<String>| drawn.join("").contains(text);
            let (drawn, _) = self
                .1
                .wait_timeout_while(drawn, Duration::from_secs(30), |drawn| !held(drawn))
                .unwrap();
            held(&drawn)
        }
    }

    impl TermLike for Drawn {
        fn width(&self) -> u16 {
            100
        }
        fn move_cursor_up(&self, _: usize) -> std::io::Result<()> {
            Ok(())
        }
        fn move_cursor_down(&self, _: usize) -> std::io::Result<()> {
            Ok(())
        }
        fn move_cursor_right(&self, _: usize) -> std::io::Result<()> {
            Ok(())
        }
        fn move_cursor_left(&self, _: usize) -> std::io::Result<()> {
            Ok(())
        }
        fn write_line(&self, line: &str) -> std::io::Result<()> {
            self.draw(line);
            Ok(())
        }
        fn write_str(&self, text: &str) -> std::io::Result<()> {
            self.draw(text);
            Ok(())
        }
        fn clear_line(&self) -> std::io::Result<()> {
            Ok(())
        }
        fn flush(&self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_long_setup_step_gets_a_line_and_a_short_one_none() {
        let screen = Drawn::default();
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
        assert!(screen.0.lock().unwrap().is_empty());
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
        let screen = Drawn::default();
        output.bars = Some(MultiProgress::with_draw_target(
            ProgressDrawTarget::term_like(Box::new(screen.clone())),
        ));
        let bar = output.bar("Downloading", "mid.bin".to_owned());
        if let Some(drawn) = &bar.drawn {
            drawn.disable_steady_tick();
        }
        bar.start(Some(1000), 0);
        bar.advance(452);
        screen.0.lock().unwrap().clear();
        output.alone().stopping();
        let drawn = screen.0.lock().unwrap().join("\n");
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

    #[test]
    fn bars_keep_the_name_and_fit_at_every_fraction() {
        for color in [false, true] {
            for done in [0, 1, 25, 50, 99, 100, 150] {
                let bar = meter(done, 100, color);
                for width in [0, 1, 7, 8, 12, 20, 40, 60, 80, 120] {
                    let line = render(
                        "iris.csv",
                        "Downloading",
                        &bar,
                        "12.3/39.4 MiB · 4.1 MiB/s · 7 s left",
                        width,
                        color,
                    );
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
        let amount = "1.0/4.0 MiB · 512 KiB/s · 6 s left · 12 s";
        let bar = "#####---------------";
        for width in [128, 100, 80, 60, 40, 20, 12, 8] {
            let line = render("iris.csv", "Downloading", bar, amount, width, false);
            assert!(line.contains("iris.csv"), "{width}: {line}");
            assert!(
                console::measure_text_width(&line) <= width,
                "{width}: {line}"
            );
        }
        assert_eq!(
            render("iris.csv", "Downloading", bar, amount, 8, false),
            "iris.csv"
        );
        let wide = render("iris.csv", "Downloading", bar, amount, 128, false);
        assert!(wide.contains("512 KiB/s"), "{wide}");
        assert!(wide.contains("6 s left"), "{wide}");
    }

    #[test]
    fn narrow_progress_never_shows_a_number_without_its_unit() {
        let amount = "1.0/4.0 MiB · 512 KiB/s · 6 s left";
        for width in 0..=80 {
            let line = render("iris.csv", "Downloading", "", amount, width, false);
            let counter = line.rsplit(' ').next().unwrap_or_default();
            assert!(
                !counter.starts_with(|c: char| c.is_ascii_digit()),
                "{width}: {line}"
            );
        }
        assert_eq!(
            render("iris.csv", "Downloading", "", amount, 16, false),
            "iris.csv 1.0 MiB"
        );
        assert_eq!(
            render("iris.csv", "Downloading", "", "512 KiB/4.0 MiB", 16, false),
            "iris.csv 512 KiB"
        );
    }

    /// a narrow line drops the bar, then the verb, then the name's middle; the counters keep room.
    #[test]
    fn narrow_progress_drops_decoration_before_facts() {
        let name = "train-00000-of-00004.parquet";
        let bar = "#####---------------";
        let amount = "113.9/190.7 MiB · 47.7 MiB/s · 2 s left";
        let at = |width| render(name, "Downloading", bar, amount, width, false);
        assert_eq!(at(101), format!("Downloading {name} {bar} {amount}"));
        assert_eq!(at(80), format!("Downloading {name} {amount}"));
        assert_eq!(at(68), format!("{name} {amount}"));
        let narrow = at(60);
        assert!(narrow.ends_with(&format!(" {amount}")), "{narrow}");
        assert!(
            narrow.starts_with("train-") && narrow.contains('~'),
            "{narrow}"
        );
        assert_eq!(console::measure_text_width(&narrow), 60, "{narrow}");
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
            let line = render(
                "specimen_replicate_01.fastq.gz",
                "Downloading",
                "#####---------------",
                "1.0/4.0 MiB · 512 KiB/s · 6 s left",
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
                ("\x1b[36m━━━━━━━━━━━━━━━━━━━━\x1b[0m", "1.0/4.0 MiB · 12 s"),
                ("", "1.0 MiB · 12 s"),
                ("", ""),
            ] {
                for width in 0..=100 {
                    let line = render(name, "\x1b[36mDownloading\x1b[0m", bar, amount, width, true);
                    assert!(
                        console::measure_text_width(&line) <= width,
                        "{width}: {line}"
                    );
                    assert!(!line.contains('\n'), "{width}: {line}");
                    let beside =
                        console::measure_text_width(name) + 1 + console::measure_text_width(amount);
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
    #[test]
    fn a_network_batch_tells_its_start_at_once() {
        let pace = (Duration::from_mins(1), Duration::from_mins(2));
        let (heard, _reported, say) = ear();
        drop(Ticker::paced(
            "Downloading",
            "3 files".to_owned(),
            (300, 0),
            pace,
            say,
        ));
        assert_eq!(*heard.lock().unwrap(), ["Downloading 3 files · 300 B"]);
        let (heard, _reported, say) = ear();
        let ticker = Ticker::paced("Downloading", "iris.csv".to_owned(), (0, 1), pace, say);
        assert!(heard.lock().unwrap().is_empty());
        ticker.sized(32);
        assert_eq!(*heard.lock().unwrap(), ["Downloading iris.csv · 32 B"]);
        drop(ticker);
        // a download refused before its body tells no size, and never began
        let (heard, _reported, say) = ear();
        let ticker = Ticker::paced("Downloading", "iris.csv".to_owned(), (0, 1), pace, say);
        ticker.sized(0);
        drop(ticker);
        assert!(heard.lock().unwrap().is_empty());
        for verb in ["Resolving", "Searching"] {
            let (heard, _reported, say) = ear();
            drop(Ticker::paced(verb, "iris".to_owned(), (0, 1), pace, say));
            assert_eq!(*heard.lock().unwrap(), [format!("{verb} iris")]);
        }
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
            say,
        ));
        assert!(heard.lock().unwrap().is_empty());
    }

    #[test]
    fn a_long_batch_says_what_it_does_then_how_far_it_got() {
        let (heard, reported, say) = ear();
        let pace = (Duration::from_millis(30), Duration::from_millis(90));
        let ticker = Ticker::paced("Downloading", "3 files".to_owned(), (200, 1), pace, say);
        ticker.sized(100);
        ticker.sized(7);
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

    #[test]
    fn a_bar_shows_how_long_it_has_waited_past_two_seconds() {
        let second = Duration::from_secs(1);
        let busy = Some(Duration::ZERO);
        assert_eq!(
            measure(0, None, 0.0, second, Duration::ZERO, busy, false),
            ""
        );
        assert_eq!(
            measure(1024, None, 512.0, 12 * second, Duration::ZERO, busy, false),
            "1.0 KiB · 512 B/s · 12 s"
        );
        assert_eq!(
            measure(0, Some(4096), 0.0, 12 * second, Duration::ZERO, busy, false),
            "12 s"
        );
        assert_eq!(
            measure(1024, Some(4096), 0.0, second, Duration::ZERO, busy, false),
            "1.0/4.0 KiB"
        );
        assert_eq!(
            measure(
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
            measure(
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
            measure(
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
            measure(
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
            measure(
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
            measure(
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

    /// the bar never says no data came, which would send a person to check the network.
    #[test]
    fn a_download_held_by_the_disk_says_so() {
        let second = Duration::from_secs(1);
        let mib = 1024 * 1024;
        assert_eq!(
            waiting(680 * mib, Some(800 * mib), None, Some(3 * second)).as_deref(),
            Some("680.0/800.0 MiB · waiting for the disk · 3 s")
        );
        assert_eq!(
            waiting(680 * mib, Some(800 * mib), None, Some(second)),
            None,
            "a moment behind is ordinary"
        );
        assert_eq!(waiting(680 * mib, Some(800 * mib), None, None), None);
        assert_eq!(
            waiting(0, None, Some("try 2 of 5".to_owned()), Some(3 * second)).as_deref(),
            Some("try 2 of 5")
        );
    }

    /// a full bar adds how long it has waited after a moment, never saying no data came.
    #[test]
    fn a_full_bar_says_it_waits_for_the_disk() {
        let second = Duration::from_secs(1);
        assert_eq!(
            measure(
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
            measure(
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
            next_try(3, 5, second * 2 + second / 3, None),
            "try 3 of 5 · next in 3 s"
        );
        assert_eq!(
            next_try(6, 15, second * 3, Some("127.0.0.1")),
            "waiting for 127.0.0.1's rate limit · try 6 of 15 · next in 3 s"
        );
        assert_eq!(
            next_try(4, 5, Duration::ZERO, None),
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
        bar.retrying(2, 5, second * 4, None);
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
            measure(
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
            measure(
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
            measure(
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
            measure(
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
            measure(
                9 * 1024,
                Some(136 * mib),
                2970.0,
                3 * second,
                801 * 60 * second,
                busy,
                false
            ),
            "0.01/136.0 MiB · 2.9 KiB/s · 3 s",
            "three seconds at the start of a slow link cannot tell thirteen hours"
        );
        assert_eq!(
            measure(
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
            measure(
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
        let after = |done, left| measure(done, Some(20 * mib), rate, 5 * second, left, busy, false);
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
        let ticker = Ticker::paced("Resolving", "zenodo:1".to_owned(), (0, 1), pace, say);
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
}
