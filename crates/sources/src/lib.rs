//! The sources bytes are fetched from.

#[cfg(test)]
use fetchloom_faults as _;

mod file;
mod http;
mod index;
mod object_store;
mod origin;

pub use file::{FileBody, FileSource};
pub use http::{HttpBody, HttpSource};
pub use object_store::ObjectStoreSource;
pub use origin::Origin;
