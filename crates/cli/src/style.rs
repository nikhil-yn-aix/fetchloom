//! Whether output carries color, and the one accent it carries.

use std::sync::atomic::{AtomicBool, Ordering};

static COLORED: AtomicBool = AtomicBool::new(false);

const ACCENT: &str = "\u{1b}[36m";

const FAILURE: &str = "\u{1b}[31m";

const WARNING: &str = "\u{1b}[33m";

const DIMMED: &str = "\u{1b}[2m";

const PLAIN: &str = "\u{1b}[0m";

pub fn colored(yes: bool) {
    COLORED.store(yes, Ordering::SeqCst);
}

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

#[must_use]
pub fn accent(text: &str) -> String {
    drawn(ACCENT, text)
}

#[must_use]
pub fn failure(text: &str) -> String {
    drawn(FAILURE, text)
}

#[must_use]
pub fn warning(text: &str) -> String {
    drawn(WARNING, text)
}

#[must_use]
pub fn dimmed(text: &str) -> String {
    drawn(DIMMED, text)
}
