//! Fault injection, and the observer that records what a run emitted.
//!
//! Faults are scheduled against named operations and are part of the product,
//! not a mock written inside a test.

mod observer;
mod platform;
mod schedule;

pub use observer::RecordingObserver;
pub use platform::FaultyPlatform;
pub use schedule::{Faults, Operation};
