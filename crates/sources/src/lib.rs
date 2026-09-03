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
pub use provider::{HuggingFaceSource, ZenodoSource};
pub use signing::{EMPTY_PAYLOAD, Request as SigningRequest, Signed, SigningTime, payload_digest, sign};
pub use origin::Origin;
