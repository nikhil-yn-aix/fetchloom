//! The sources bytes are fetched from.

#[cfg(test)]
use fetchloom_faults as _;

mod http;
mod index;
mod origin;

pub use http::{HttpBody, HttpSource};
pub use origin::Origin;
