//! What is drawn on a terminal while a command waits: one spinner naming what it waits for,
//! one bar per running transfer and one bar for all of them. Lines printed meanwhile go above
//! the bars, so nothing tears.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use fl_model::units::{rate, size};
use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget, ProgressState, ProgressStyle};

use crate::term::Style;

const SPINNER: &str =
    "\u{280b}\u{2819}\u{2839}\u{2838}\u{283c}\u{2834}\u{2826}\u{2827}\u{2807}\u{280f} ";
const SPINNER_ASCII: &str = "-\\|/ ";
const BAR: &str = "\u{2501}\u{2578}\u{2500}";
const BAR_ASCII: &str = "=> ";

/// The spinner and bars of one run.
pub(crate) struct Bars {
    multi: MultiProgress,
    style: Style,
    room: usize,
    drawn: Mutex<Drawn>,
}

#[derive(Default)]
struct Drawn {
    spinner: Option<ProgressBar>,
    total: Option<ProgressBar>,
    transfers: HashMap<u64, ProgressBar>,
    reached: HashMap<u64, u64>,
}

impl Bars {
    /// Bars drawn on stderr, with at most `rows` lines of them at once.
    pub(crate) fn stderr(style: Style, rows: usize) -> Self {
        Self::new(ProgressDrawTarget::stderr(), style, rows)
    }

    /// Bars drawn to `target`, with at most `rows` lines of them at once.
    pub(crate) fn new(target: ProgressDrawTarget, style: Style, rows: usize) -> Self {
        Self {
            multi: MultiProgress::with_draw_target(target),
            style,
            room: rows.saturating_sub(2).max(1),
            drawn: Mutex::new(Drawn::default()),
        }
    }

    fn drawn(&self) -> MutexGuard<'_, Drawn> {
        self.drawn.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Writes `text` above the bars.
    pub(crate) fn println(&self, text: &str) {
        let _ignored = self.multi.println(text.strip_suffix('\n').unwrap_or(text));
    }

    /// Shows the spinner with `text`, saying what the run waits for.
    pub(crate) fn wait(&self, text: &str) {
        let mut drawn = self.drawn();
        let spinner = drawn.spinner.get_or_insert_with(|| {
            let spinner = self.multi.add(ProgressBar::new_spinner());
            spinner.set_style(self.spinner_style());
            spinner.enable_steady_tick(Duration::from_millis(100));
            spinner
        });
        spinner.set_message(text.to_owned());
    }

    /// Removes the spinner.
    pub(crate) fn settle(&self) {
        if let Some(spinner) = self.drawn().spinner.take() {
            spinner.finish_and_clear();
            self.multi.remove(&spinner);
        }
    }

    /// Starts transfer `id` from `host` of `bytes`, when known. It gets a bar of its own while
    /// there is room.
    pub(crate) fn start(&self, id: u64, host: &str, bytes: Option<u64>) {
        let mut drawn = self.drawn();
        let total = drawn.total.get_or_insert_with(|| {
            let total = self.multi.add(ProgressBar::new(0));
            total.set_style(self.bar_style("Downloading"));
            total
        });
        total.inc_length(bytes.unwrap_or(0));
        if drawn.transfers.len() < self.room {
            let bar = self.multi.add(ProgressBar::new(bytes.unwrap_or(0)));
            bar.set_style(self.bar_style(""));
            bar.set_message(host.to_owned());
            drawn.transfers.insert(id, bar);
        }
    }

    /// Moves transfer `id` to `bytes` received, and the total by the difference.
    pub(crate) fn advance(&self, id: u64, bytes: u64) {
        let mut drawn = self.drawn();
        if let Some(bar) = drawn.transfers.get(&id) {
            bar.set_position(bytes);
        }
        let before = drawn.reached.insert(id, bytes).unwrap_or(0);
        if let Some(total) = &drawn.total {
            total.inc(bytes.saturating_sub(before));
        }
    }

    /// Ends transfer `id`, collapsing its bar.
    pub(crate) fn finish(&self, id: u64) {
        let mut drawn = self.drawn();
        drawn.reached.remove(&id);
        if let Some(bar) = drawn.transfers.remove(&id) {
            bar.finish_and_clear();
            self.multi.remove(&bar);
        }
    }

    /// Removes everything drawn.
    pub(crate) fn clear(&self) {
        let mut drawn = self.drawn();
        let spinner = drawn.spinner.take();
        let total = drawn.total.take();
        let bars: Vec<ProgressBar> = drawn.transfers.drain().map(|(_, bar)| bar).collect();
        drawn.reached.clear();
        for bar in spinner.iter().chain(&total).chain(&bars) {
            bar.finish_and_clear();
            self.multi.remove(bar);
        }
    }

    fn spinner_style(&self) -> ProgressStyle {
        let template = if self.style.color() {
            "{spinner:.cyan} {msg}"
        } else {
            "{spinner} {msg}"
        };
        let ticks = if self.style.unicode() {
            SPINNER
        } else {
            SPINNER_ASCII
        };
        ProgressStyle::with_template(template)
            .unwrap_or_else(|_| ProgressStyle::default_spinner())
            .tick_chars(ticks)
    }

    fn bar_style(&self, verb: &str) -> ProgressStyle {
        let verb = format!("{verb:>11}");
        let template = if self.style.color() {
            "{verb:.cyan.bold} {bar:25.cyan/dim} {amount} {speed} {msg:.dim}"
        } else {
            "{verb} {bar:25} {amount} {speed} {msg}"
        };
        let chars = if self.style.unicode() { BAR } else { BAR_ASCII };
        ProgressStyle::with_template(template)
            .unwrap_or_else(|_| ProgressStyle::default_bar())
            .progress_chars(chars)
            .with_key(
                "verb",
                move |_: &ProgressState, out: &mut dyn std::fmt::Write| {
                    let _ = out.write_str(&verb);
                },
            )
            .with_key(
                "amount",
                |state: &ProgressState, out: &mut dyn std::fmt::Write| {
                    let _ = out.write_str(&amount(state.pos(), state.len()));
                },
            )
            .with_key(
                "speed",
                |state: &ProgressState, out: &mut dyn std::fmt::Write| {
                    if state.elapsed() >= Duration::from_secs(2) {
                        let _ = write!(out, " {}", rate(per_second(state.per_sec())));
                    }
                },
            )
    }
}

fn amount(done: u64, length: Option<u64>) -> String {
    match length {
        Some(length) if length > 0 => format!("{}/{}", size(done), size(length)),
        _ => size(done).to_string(),
    }
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a rate is shown rounded down, and a negative or huge one cannot occur"
)]
fn per_second(rate: f64) -> u64 {
    rate.max(0.0) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bars(rows: usize) -> Bars {
        Bars::new(ProgressDrawTarget::hidden(), Style::PLAIN, rows)
    }

    #[test]
    fn transfers_get_bars_while_there_is_room_and_all_count_in_the_total() {
        let bars = bars(3);
        bars.start(1, "zenodo.org", Some(100));
        bars.start(2, "zenodo.org", None);
        assert_eq!(bars.drawn().transfers.len(), 1);
        bars.advance(1, 40);
        bars.advance(2, 10);
        bars.advance(1, 60);
        {
            let drawn = bars.drawn();
            assert_eq!(drawn.transfers[&1].position(), 60);
            let total = drawn.total.as_ref().unwrap();
            assert_eq!(total.position(), 70);
            assert_eq!(total.length(), Some(100));
        }
        bars.finish(1);
        bars.finish(2);
        assert!(bars.drawn().transfers.is_empty());
        assert!(bars.drawn().reached.is_empty());
    }

    #[test]
    fn the_spinner_comes_and_goes() {
        let bars = bars(10);
        bars.wait("Resolving zenodo:1 at zenodo.org");
        bars.wait("Resolving zenodo:2 at zenodo.org");
        assert_eq!(
            bars.drawn().spinner.as_ref().unwrap().message(),
            "Resolving zenodo:2 at zenodo.org"
        );
        bars.settle();
        assert!(bars.drawn().spinner.is_none());
        bars.wait("x");
        bars.start(1, "h", Some(1));
        bars.clear();
        let drawn = bars.drawn();
        assert!(drawn.spinner.is_none() && drawn.total.is_none() && drawn.transfers.is_empty());
    }

    #[test]
    fn amounts_show_the_total_when_known() {
        assert_eq!(amount(1024, Some(4096)), "1.0 KiB/4.0 KiB");
        assert_eq!(amount(1024, Some(0)), "1.0 KiB");
        assert_eq!(amount(5, None), "5 B");
        assert_eq!(per_second(-3.0), 0);
        assert_eq!(per_second(2.9), 2);
    }
}
