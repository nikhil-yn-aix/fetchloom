//! The sources bytes are fetched from.

#[cfg(test)]
use fetchloom_faults as _;

mod file;
mod help;
mod http;
mod index;
mod object_store;
mod origin;
mod provider;
mod signing;

pub use file::{FileBody, FileSource};
pub use help::{help_for, signs_requests};
pub use http::{HttpBody, HttpSource, trust_store_loads};
pub use object_store::ObjectStoreSource;
pub use origin::Origin;
pub use provider::{HuggingFaceSource, ZenodoSource};
pub use signing::{
    EMPTY_PAYLOAD, Request as SigningRequest, Signed, SigningTime, payload_digest, sign,
};

/// Returns the reference one entry of a container is named by, with exactly one
/// separator between them however the container was written.
#[must_use]
pub fn joined(container: &str, path: &str) -> String {
    let head = container.trim_end_matches('/');
    let tail = path.trim_start_matches('/');
    format!("{head}/{tail}")
}
