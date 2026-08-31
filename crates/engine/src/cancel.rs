//! Whether the run has been asked to stop, and how insistently.

use std::sync::atomic::{AtomicU32, Ordering};

use crate::outcome::ExitCode;

/// How many times this process has been asked to stop.
static INTERRUPTS: AtomicU32 = AtomicU32::new(0);

/// Records that the run was asked to stop.
///
/// Returns how many times it has now been asked, counted from one.
pub fn interrupt() -> u32 {
    INTERRUPTS.fetch_add(1, Ordering::SeqCst) + 1
}

/// Returns whether the run has been asked to stop.
#[must_use]
pub fn requested() -> bool {
    INTERRUPTS.load(Ordering::SeqCst) > 0
}

/// Ends the process because it was asked to stop.
///
/// Called only where the caller has already stopped taking new work, flushed
/// what was in flight, and written down what a later run can resume from.
pub fn stop() -> ! {
    std::process::exit(ExitCode::Cancelled.code());
}
