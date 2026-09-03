//! Whether output carries color, and the one accent it carries.

use std::sync::atomic::{AtomicBool, Ordering};

/// Whether this run's output carries color.
static COLORED: AtomicBool = AtomicBool::new(false);

/// The accent every colored fact is drawn in.
const ACCENT: &str = "\u{1b}[36m";

/// What a failure is drawn in.
const FAILURE: &str = "\u{1b}[31m";

/// What a warning is drawn in.
const WARNING: &str = "\u{1b}[33m";

/// What detail beside a fact is drawn in.
const DIMMED: &str = "\u{1b}[2m";

/// What returns the stream to what it was.
const PLAIN: &str = "\u{1b}[0m";

/// Records whether this run's output carries color, which the composition root
/// settles once from the flag, the configuration, the environment, and whether
/// the stream is a terminal.
pub fn colored(yes: bool) {
    COLORED.store(yes, Ordering::SeqCst);
}

/// Reports whether output carries color.
#[must_use]
pub fn is_colored() -> bool {
    COLORED.load(Ordering::SeqCst)
}

fn drawn(code: &str, text: &str) -> String {
    if is_colored() {
        format!("{code}{text}{PLAIN}")
    } else {
        text.to_owned()
    }
}

/// Draws the one fact a line is about.
#[must_use]
pub fn accent(text: &str) -> String {
    drawn(ACCENT, text)
}

/// Draws a failure.
#[must_use]
pub fn failure(text: &str) -> String {
    drawn(FAILURE, text)
}

/// Draws a warning.
#[must_use]
pub fn warning(text: &str) -> String {
    drawn(WARNING, text)
}

/// Draws detail beside a fact.
#[must_use]
pub fn dimmed(text: &str) -> String {
    drawn(DIMMED, text)
}
