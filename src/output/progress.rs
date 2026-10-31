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
const TICK: Duration = Duration::from_millis(100);
/// how long a batch runs before its first plain line, so a quick one adds no line at all.
const MOMENT: Duration = Duration::from_secs(2);
/// how often a batch that is still running says how far it got.
const EVERY: Duration = Duration::from_secs(10);

/// one download's bar, or nothing when bars are off. it clears itself when dropped.
pub(crate) struct Bar {
    bar: Option<ProgressBar>,
    color: bool,
}

impl Bar {
    pub(super) fn hidden() -> Bar {
        Bar {
            bar: None,
            color: false,
        }
    }

    pub(super) fn add(
        bars: &MultiProgress,
        verb: &'static str,
        message: String,
        color: bool,
    ) -> Bar {
        let bar = bars.add(ProgressBar::no_length().with_message(message));
        bar.set_style(style(verb, color, &bar.message()));
        bar.enable_steady_tick(TICK);
        Bar {
            bar: Some(bar),
            color,
        }
    }

    pub(crate) fn message(&self, verb: &'static str, text: String) {
        if let Some(bar) = &self.bar {
            bar.set_style(style(verb, self.color, &text));
            bar.set_message(text);
        }
    }

    pub(crate) fn start(&self, total: Option<u64>, resumed: u64) {
        if let Some(bar) = &self.bar {
            if let Some(total) = total {
                bar.set_length(total);
            }
            bar.set_position(resumed);
            bar.reset_eta();
        }
    }

    pub(crate) fn advance(&self, bytes: u64) {
        if let Some(bar) = &self.bar {
            bar.inc(bytes);
        }
    }
}

impl Drop for Bar {
    fn drop(&mut self) {
        if let Some(bar) = &self.bar {
            bar.finish_and_clear();
        }
    }
}

fn style(verb: &'static str, color: bool, message: &str) -> ProgressStyle {
    let name = super::plain(message).into_owned();
    let cyan = super::paint(color, super::ACCENT.bold());
    let heading = format!("{cyan}{verb:>11}{cyan:#}");
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
                let amount = measure(
                    state.pos(),
                    state.len(),
                    state.per_sec(),
                    state.elapsed(),
                    state.eta(),
                );
                let width = usize::from(console::Term::stderr().size().1);
                let _ = out.write_str(&render(&name, &heading, &bar, &amount, width, color));
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
    let cyan = super::paint(color, super::ACCENT);
    let dim = super::paint(color, anstyle::Style::new().dimmed());
    format!(
        "{cyan}{}{cyan:#}{dim}{}{dim:#}",
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
    let mut kept = amount;
    loop {
        let shown = line(bar, kept);
        if console::measure_text_width(&shown) <= width {
            return shown;
        }
        if kept.is_empty() {
            break;
        }
        kept = kept
            .rsplit_once(" · ")
            .or_else(|| kept.split_once('/'))
            .map_or("", |(head, _)| head);
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

/// elapsed time stays visible until a settled rate can estimate the time left.
fn measure(done: u64, total: Option<u64>, rate: f64, elapsed: Duration, left: Duration) -> String {
    let mut parts = Vec::new();
    if done > 0 {
        parts.push(total.map_or_else(|| units::size(done), |total| fraction(done, total)));
        if rate > 0.0 && elapsed >= SETTLE {
            parts.push(units::speed(rate));
            if total.is_some() {
                parts.push(format!("{} left", units::seconds(left)));
            }
        }
    }
    if elapsed >= MOMENT && !(done > 0 && rate > 0.0 && elapsed >= SETTLE && total.is_some()) {
        parts.push(units::seconds(elapsed));
    }
    parts.join(" · ")
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
            shared.total.fetch_add(bytes, Ordering::Relaxed);
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
    verb: &str,
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
        if told {
            let done = shared.done.load(Ordering::Relaxed);
            let elapsed = units::seconds(started.elapsed());
            say(
                verb,
                &match total {
                    Some(total) => format!(
                        "{} of {} · {elapsed}",
                        units::size(done.min(total)),
                        units::size(total)
                    ),
                    // a wait with no bytes to count, such as a source taking its time to answer
                    None if done == 0 => format!("{what} · {elapsed}"),
                    None => format!("{} · {elapsed}", units::size(done)),
                },
            );
            next += every;
        } else {
            say(
                verb,
                &match total {
                    Some(total) => format!("{what} · {}", units::size(total)),
                    None => what.to_owned(),
                },
            );
            told = true;
            next = every;
        }
    }
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
                    assert!(console::measure_text_width(&line) <= width, "{width}: {line}");
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
                (
                    "\x1b[36m━━━━━━━━━━━━━━━━━━━━\x1b[0m",
                    "1.0/4.0 MiB · 12 s",
                ),
                ("", "1.0 MiB · 12 s"),
                ("", ""),
            ] {
                for width in 0..=100 {
                    let line = render(
                        name,
                        "\x1b[36mDownloading\x1b[0m",
                        bar,
                        amount,
                        width,
                        true,
                    );
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
        assert_eq!(measure(0, None, 0.0, second, Duration::ZERO), "");
        assert_eq!(
            measure(1024, None, 512.0, 12 * second, Duration::ZERO),
            "1.0 KiB · 512 B/s · 12 s"
        );
        assert_eq!(
            measure(0, Some(4096), 0.0, 12 * second, Duration::ZERO),
            "12 s"
        );
        assert_eq!(
            measure(1024, Some(4096), 0.0, second, Duration::ZERO),
            "1.0/4.0 KiB"
        );
        assert_eq!(
            measure(1024, Some(4096), 512.0, 12 * second, 6 * second),
            "1.0/4.0 KiB · 512 B/s · 6 s left"
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
