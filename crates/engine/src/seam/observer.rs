//! The Observer seam: where every event goes.

use crate::event::Event;

/// Somewhere the event stream is written to.
pub trait Observer: Send + Sync {
    /// Writes one event.
    fn emit(&self, event: &Event);
}
