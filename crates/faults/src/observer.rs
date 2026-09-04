//! An observer that keeps every event a run emitted.

use std::sync::{Mutex, PoisonError};

use fetchloom_engine::event::Event;
use fetchloom_engine::seam::observer::Observer;

#[derive(Debug, Default)]
pub struct RecordingObserver {
    events: Mutex<Vec<Event>>,
}

impl RecordingObserver {
    #[must_use]
    pub fn new() -> Self {
        Self {
            events: Mutex::new(Vec::new()),
        }
    }

    #[must_use]
    pub fn events(&self) -> Vec<Event> {
        self.events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

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
