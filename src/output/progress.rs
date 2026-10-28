//! progress bars while downloads run, drawn only on a terminal a person watches.

use std::fmt;
use std::time::Duration;

use indicatif::{MultiProgress, ProgressBar, ProgressState, ProgressStyle};

use crate::units;

/// the rate needs a few seconds of samples before the time left means anything.
const SETTLE: Duration = Duration::from_secs(3);
const TICK: Duration = Duration::from_millis(100);

/// one download's bar, or nothing when bars are off. it clears itself when dropped.
pub(crate) struct Bar {
    bar: Option<ProgressBar>,
    unicode: bool,
}

impl Bar {
    pub(super) fn hidden() -> Bar {
        Bar {
            bar: None,
            unicode: false,
        }
    }

    pub(super) fn add(bars: &MultiProgress, message: String, unicode: bool) -> Bar {
        let bar = bars.add(ProgressBar::no_length().with_message(message));
        bar.set_style(style(unicode, false));
        bar.enable_steady_tick(TICK);
        Bar {
            bar: Some(bar),
            unicode,
        }
    }

    pub(crate) fn message(&self, text: String) {
        if let Some(bar) = &self.bar {
            bar.set_message(text);
        }
    }

    pub(crate) fn start(&self, total: Option<u64>, resumed: u64) {
        if let Some(bar) = &self.bar {
            bar.set_style(style(self.unicode, total.is_some()));
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

fn style(unicode: bool, sized: bool) -> ProgressStyle {
    let template = if sized {
        "{spinner:.cyan} {wide_msg} {bar:20.cyan/dim} {amount}"
    } else {
        "{spinner:.cyan} {wide_msg} {amount}"
    };
    let (ticks, fill) = if unicode {
        ("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏ ", "━╸─")
    } else {
        ("-\\|/ ", "=> ")
    };
    ProgressStyle::with_template(template)
        .unwrap_or_else(|_| ProgressStyle::default_spinner())
        .tick_chars(ticks)
        .progress_chars(fill)
        .with_key("amount", amount)
}

/// `1.2 MiB / 4.1 MiB · 3.4 MiB/s · 1 s left`, in the units the rest of fetchloom prints.
fn amount(state: &ProgressState, out: &mut dyn fmt::Write) {
    if state.pos() == 0 {
        return;
    }
    let done = units::size(state.pos());
    let _ = match state.len() {
        Some(total) => write!(out, "{done} / {}", units::size(total)),
        None => write!(out, "{done}"),
    };
    let rate = state.per_sec();
    if rate > 0.0 && state.elapsed() >= SETTLE {
        let _ = write!(out, " · {}", units::speed(rate));
        if state.len().is_some() {
            let _ = write!(out, " · {} left", units::seconds(state.eta()));
        }
    }
}
