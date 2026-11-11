//! progress while downloads and unpacking run: bars on a terminal a person watches, and plain
//! lines now and then anywhere else.

use std::fmt;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use indicatif::{MultiProgress, ProgressBar, ProgressState, ProgressStyle};

use crate::units;

/// the rate needs a few seconds of samples before the time left means anything.
const SETTLE: Duration = Duration::from_secs(3);
/// a time left reaching further than this many times the time watched so far is a guess, until
/// [`STEADY`] has passed.
const REACH: u32 = 60;
const STEADY: Duration = Duration::from_secs(30);
/// how long a download receives nothing before its bar says so instead of showing a fading rate.
const STALL: Duration = Duration::from_secs(5);
const TICK: Duration = Duration::from_millis(100);
/// how long a batch runs before its first plain line, so a quick one adds no line at all.
const MOMENT: Duration = Duration::from_secs(2);
/// how often a batch that is still running says how far it got.
const EVERY: Duration = Duration::from_secs(10);

/// one download's bar, or nothing when bars are off. it clears itself when dropped.
pub(crate) struct Bar {
    drawn: Option<ProgressBar>,
    color: bool,
    unicode: bool,
    moved: Arc<Moved>,
}

/// when a bar last moved, so a download that stopped receiving can say so.
struct Moved {
    born: Instant,
    /// milliseconds from `born` to the last bytes or answer.
    at: AtomicU64,
}

impl Moved {
    fn new() -> Moved {
        Moved {
            born: Instant::now(),
            at: AtomicU64::new(0),
        }
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
            moved: Arc::new(Moved::new()),
        }
    }

    pub(super) fn add(
        bars: &MultiProgress,
        verb: &'static str,
        message: String,
        color: bool,
        unicode: bool,
    ) -> Bar {
        let moved = Arc::new(Moved::new());
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

    pub(crate) fn start(&self, total: Option<u64>, resumed: u64) {
        if let Some(bar) = &self.drawn {
            if let Some(total) = total {
                bar.set_length(total);
            }
            bar.set_position(resumed);
            bar.reset_eta();
            self.moved.now();
        }
    }

    pub(crate) fn advance(&self, bytes: u64) {
        if let Some(bar) = &self.drawn {
            bar.inc(bytes);
            self.moved.now();
        }
    }

    /// what the bar fills to, as a size learned during the downloads changes it.
    pub(crate) fn length(&self, total: u64) {
        if let Some(bar) = &self.drawn {
            bar.set_length(total);
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
                let bar = if matches!(verb, "Downloading" | "Unpacking" | "Checking") {
                    state.len().map_or_else(
                        || meter(0, 1, color),
                        |total| meter(state.pos(), total, color),
                    )
                } else {
                    String::new()
                };
                // unpacking and checking read local files, whose pauses mean nothing
                let quiet = (verb == "Downloading").then(|| moved.quiet());
                let amount = measure(
                    state.pos(),
                    state.len(),
                    state.per_sec(),
                    state.elapsed(),
                    state.eta(),
                    quiet,
                );
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

/// counters and bar space give way before the name, including the verb when only the name fits.
fn render(name: &str, verb: &str, bar: &str, amount: &str, width: usize, color: bool) -> String {
    let dim = super::paint(color, anstyle::Style::new().dimmed());
    let line = |bar: &str, amount: &str| {
        let amount = if amount.is_empty() {
            String::new()
        } else {
            format!("{dim}{amount}{dim:#}")
        };
        [verb, name, bar, &amount]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    };
    let mut kept = std::borrow::Cow::Borrowed(amount);
    loop {
        let shown = line(bar, &kept);
        if console::measure_text_width(&shown) <= width {
            return shown;
        }
        if kept.is_empty() {
            break;
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
    let shown = line("", "");
    if console::measure_text_width(&shown) <= width {
        return shown;
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

/// elapsed time stays visible until a settled rate can estimate the time left, and a download
/// that has received nothing for [`STALL`] says so instead of showing a fading rate. one with
/// every byte in says it waits for the disk. `quiet`, how long since a download last received
/// bytes, is none for local work.
fn measure(
    done: u64,
    total: Option<u64>,
    rate: f64,
    elapsed: Duration,
    left: Duration,
    quiet: Option<Duration>,
) -> String {
    let mut parts = Vec::new();
    if done > 0 {
        parts.push(total.map_or_else(|| units::size(done), |total| fraction(done, total)));
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
    let foreseen =
        settled && total.is_some() && (elapsed >= STEADY || left <= elapsed.saturating_mul(REACH));
    if settled {
        parts.push(units::speed(rate));
        if foreseen {
            parts.push(format!("{} left", units::seconds(left)));
        }
    }
    if elapsed >= MOMENT && !foreseen {
        parts.push(units::seconds(elapsed));
    }
    parts.join(" · ")
}

/// whether `done` reached a known, nonzero `total`: a download then has every byte in, and what
/// is left is the disk's.
fn every_byte_in(done: u64, total: Option<u64>) -> bool {
    total.is_some_and(|total| total > 0 && done >= total)
}

fn fraction(done: u64, total: u64) -> String {
    let done = units::size(done);
    let total = units::size(total);
    match (done.rsplit_once(' '), total.rsplit_once(' ')) {
        (Some((value, unit)), Some((_, total_unit))) if unit == total_unit => {
            format!("{value}/{total}")
        }
        _ => format!("{done}/{total}"),
    }
}

/// keeps a wait visible until its owner finishes.
pub(crate) struct Wait {
    pub(super) _bar: Bar,
    pub(super) _ticker: Ticker,
}

/// a batch of work told in plain lines where no bar shows it, as in a log or a pipe: what it
/// does once it has run for a moment, then how far it got every 10 s, never a line per file.
/// it stops when dropped.
pub(crate) struct Ticker {
    shared: Option<Arc<Shared>>,
    thread: Option<JoinHandle<()>>,
}

struct Shared {
    done: AtomicU64,
    total: AtomicU64,
    /// files whose size only their download tells, so the total is not known until they start.
    unknown: AtomicUsize,
    finished: Mutex<bool>,
    wake: Condvar,
}

impl Ticker {
    pub(super) fn quiet() -> Ticker {
        Ticker {
            shared: None,
            thread: None,
        }
    }

    /// tells `verb` and `what` through `say`, for a batch whose known sizes add up to `total`,
    /// with `unknown` more files whose downloads have yet to tell theirs.
    pub(super) fn start(
        verb: &'static str,
        what: String,
        (total, unknown): (u64, usize),
        say: impl Fn(&str, &str) + Send + 'static,
    ) -> Ticker {
        Ticker::paced(verb, what, (total, unknown), (MOMENT, EVERY), say)
    }

    fn paced(
        verb: &'static str,
        what: String,
        (total, unknown): (u64, usize),
        pace: (Duration, Duration),
        say: impl Fn(&str, &str) + Send + 'static,
    ) -> Ticker {
        let shared = Arc::new(Shared {
            done: AtomicU64::new(0),
            total: AtomicU64::new(total),
            unknown: AtomicUsize::new(unknown),
            finished: Mutex::new(false),
            wake: Condvar::new(),
        });
        let watched = shared.clone();
        // without a thread the batch runs on untold, which costs nothing but the lines
        let thread = std::thread::Builder::new()
            .name("ticker".to_owned())
            .spawn(move || tick(&watched, verb, &what, pace, &say))
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

    /// adds the size of a file whose download just told it.
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

/// waits out the batch, saying what it does after `moment` and how far it got every `every`.
fn tick(
    shared: &Shared,
    verb: &'static str,
    what: &str,
    (moment, every): (Duration, Duration),
    say: &dyn Fn(&str, &str),
) {
    let started = Instant::now();
    let mut next = moment;
    let mut told = false;
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
        let total = (shared.unknown.load(Ordering::Relaxed) == 0)
            .then(|| shared.total.load(Ordering::Relaxed));
        let done = shared.done.load(Ordering::Relaxed);
        let (verb, text) = report(verb, what, (done, total), started.elapsed(), !told);
        say(verb, &text);
        if told {
            next += every;
        } else {
            told = true;
            next = every;
        }
    }
}

/// the line a batch tells once it has run `elapsed`: the `first` time what it does, after that
/// how far it got of `total`, which is known once every file's size is. a download with every
/// byte in waits only for the disk to keep its files, so it says that.
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
            render("iris.csv", "Downloading", "", amount, 28, false),
            "Downloading iris.csv 1.0 MiB"
        );
        assert_eq!(
            render("iris.csv", "Downloading", "", "512 KiB/4.0 MiB", 28, false),
            "Downloading iris.csv 512 KiB"
        );
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
                    if width >= console::measure_text_width(name) {
                        assert!(line.contains(name), "{width}: {line}");
                    }
                }
            }
        }
    }

    #[test]
    fn a_quick_batch_adds_no_line() {
        let (heard, _reported, say) = ear();
        let pace = (Duration::from_secs(5), Duration::from_secs(10));
        drop(Ticker::paced(
            "Downloading",
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

    #[test]
    fn counters_share_units_only_when_the_units_match() {
        assert_eq!(fraction(1024, 4096), "1.0/4.0 KiB");
        assert_eq!(fraction(512, 4096), "512 B/4.0 KiB");
        assert_eq!(fraction(1, 4), "1/4 B");
        assert_eq!(fraction(0, 0), "0/0 B");
    }

    #[test]
    fn a_bar_shows_how_long_it_has_waited_past_two_seconds() {
        let second = Duration::from_secs(1);
        let busy = Some(Duration::ZERO);
        assert_eq!(measure(0, None, 0.0, second, Duration::ZERO, busy), "");
        assert_eq!(
            measure(1024, None, 512.0, 12 * second, Duration::ZERO, busy),
            "1.0 KiB · 512 B/s · 12 s"
        );
        assert_eq!(
            measure(0, Some(4096), 0.0, 12 * second, Duration::ZERO, busy),
            "12 s"
        );
        assert_eq!(
            measure(1024, Some(4096), 0.0, second, Duration::ZERO, busy),
            "1.0/4.0 KiB"
        );
        assert_eq!(
            measure(1024, Some(4096), 512.0, 12 * second, 6 * second, busy),
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
                Some(12 * second)
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
                Some(9 * second)
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
                Some(4 * second)
            ),
            "1.0/4.0 KiB · 512 B/s · 6 s left",
            "a gap of a few seconds is ordinary"
        );
    }

    /// a bar whose download has every byte in waits on the disk to keep the file, so it says
    /// that, and for how long once it has waited a moment, rather than that no data came.
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
                Some(12 * second)
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
                Some(Duration::ZERO)
            ),
            "57/57 B · waiting for the disk"
        );
    }

    /// the plain line says what a batch does the first time, then how far it got. once every
    /// byte of a download is in, what is left is the disk's, so the line says it waits for that.
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
            told("Unpacking", 57, Some(57)),
            ("Unpacking", "57 B of 57 B · 10 s".to_owned())
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
                busy
            ),
            "9.0 KiB/136.0 MiB · 2.9 KiB/s · 3 s",
            "three seconds at the start of a slow link cannot tell thirteen hours"
        );
        assert_eq!(
            measure(
                9 * mib,
                Some(136 * mib),
                2.0 * 1_048_576.0,
                10 * second,
                64 * second,
                busy
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
                busy
            ),
            "1.0 KiB/136.0 MiB · 2.9 KiB/s · 801 min 00 s left",
            "half a minute of a steady slow rate is worth telling"
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
