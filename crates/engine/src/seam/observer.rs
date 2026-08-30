//! The Observer seam: where every event goes.

use crate::event::Event;

/// Somewhere the event stream is written to.
///
/// An observer cannot influence a run and can be removed without changing the
/// engine.
pub trait Observer: Send + Sync {
    /// Writes one event.
    ///
    /// An observer that cannot write drops the event rather than failing a run.
    fn emit(&self, event: &Event);
}
