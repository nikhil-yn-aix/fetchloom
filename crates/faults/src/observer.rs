//! An observer that keeps every event a run emitted.

use std::sync::{Mutex, PoisonError};

use fetchloom_engine::event::Event;
use fetchloom_engine::seam::observer::Observer;

/// An observer that keeps every event, so a test can assert the sequence a run
/// produced.
#[derive(Debug, Default)]
pub struct RecordingObserver {
    events: Mutex<Vec<Event>>,
}

impl RecordingObserver {
    /// Starts an observer that has recorded nothing.
    #[must_use]
    pub fn new() -> Self {
        Self {
            events: Mutex::new(Vec::new()),
        }
    }

    /// Returns every event recorded so far, in the order they were emitted.
    #[must_use]
    pub fn events(&self) -> Vec<Event> {
        self.events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Returns the name of every event recorded so far, in order.
    #[must_use]
    pub fn names(&self) -> Vec<&'static str> {
        self.events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(Event::name)
            .collect()
    }
}

impl Observer for RecordingObserver {
    fn emit(&self, event: &Event) {
        self.events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(event.clone());
    }
}
