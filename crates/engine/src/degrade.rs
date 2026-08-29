//! Where a seam records the fallbacks it performed.

use std::sync::{Mutex, PoisonError};

/// One fallback a seam performed instead of what was requested.
///
/// A seam with no observer records a fallback here and the composition root
/// turns each entry into the one `degrade` event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Degradation {
    /// What was requested.
    pub requested: String,
    /// What was used instead.
    pub used: String,
    /// Why the substitution happened.
    pub reason: String,
}

/// Where a seam records the fallbacks it performed.
#[derive(Debug, Default)]
pub struct DegradeQueue {
    entries: Mutex<Vec<Degradation>>,
}

impl DegradeQueue {
    /// Starts a queue that has recorded nothing.
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(Vec::new()),
        }
    }

    /// Records one fallback.
    pub fn record(
        &self,
        requested: impl Into<String>,
        used: impl Into<String>,
        reason: impl Into<String>,
    ) {
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Degradation {
                requested: requested.into(),
                used: used.into(),
                reason: reason.into(),
            });
    }

    /// Removes and returns everything recorded so far.
    #[must_use]
    pub fn take(&self) -> Vec<Degradation> {
        std::mem::take(&mut self.entries.lock().unwrap_or_else(PoisonError::into_inner))
    }
}
