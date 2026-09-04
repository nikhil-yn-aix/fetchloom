//! The Observer seam: where every event goes.

use crate::event::Event;

pub trait Observer: Send + Sync {
    fn emit(&self, event: &Event);
}
