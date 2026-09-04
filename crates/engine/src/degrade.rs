//! Where a seam records the fallbacks it performed.

use std::sync::{Mutex, PoisonError};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Degradation {
    pub requested: String,
    pub used: String,
    pub reason: String,
}

#[derive(Debug, Default)]
pub struct DegradeQueue {
    entries: Mutex<Vec<Degradation>>,
}

impl DegradeQueue {
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(Vec::new()),
        }
    }

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

    #[must_use]
    pub fn take(&self) -> Vec<Degradation> {
        std::mem::take(&mut self.entries.lock().unwrap_or_else(PoisonError::into_inner))
    }
}
