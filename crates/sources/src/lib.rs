//! The sources bytes are fetched from.

#[cfg(test)]
use fetchloom_faults as _;

mod file;
mod http;
mod index;
mod origin;

pub use file::{FileBody, FileSource};
pub use http::{HttpBody, HttpSource};
pub use origin::Origin;
