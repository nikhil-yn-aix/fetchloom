//! Whether the run has been asked to stop, and how insistently.

use std::sync::atomic::{AtomicU32, Ordering};

use crate::outcome::ExitCode;

static INTERRUPTS: AtomicU32 = AtomicU32::new(0);

pub fn interrupt() -> u32 {
    INTERRUPTS.fetch_add(1, Ordering::SeqCst) + 1
}

#[must_use]
pub fn requested() -> bool {
    INTERRUPTS.load(Ordering::SeqCst) > 0
}

pub fn stop() -> ! {
    std::process::exit(ExitCode::Cancelled.code());
}
