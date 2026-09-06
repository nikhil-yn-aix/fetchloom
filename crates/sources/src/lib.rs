//! The sources bytes are fetched from.

#[cfg(test)]
use fetchloom_faults as _;

mod delegate;
mod described;
mod doi;
mod file;
mod ftp;
mod help;
mod http;
mod index;
mod listing;
mod object_store;
mod origin;
mod provider;
mod search;
mod signing;
mod tls;

pub use described::{Provider, described, described_reaching};
pub use doi::{DoiRouter, is_doi};
pub use file::{FileBody, FileSource};
pub use ftp::{FtpBody, FtpSource};
pub use help::{help_for, provider_variable, signs_requests};
pub use http::{HttpBody, HttpSource, trust_store_loads};
pub use listing::DescribedSource;
pub use object_store::ObjectStoreSource;
pub use origin::Origin;
pub use provider::{HuggingFaceSource, ZenodoSource};
pub use search::{Found, Registry, Searcher};
pub use signing::{Request as SigningRequest, Signed, SigningTime, sign};

#[must_use]
pub fn joined(container: &str, path: &str) -> String {
    let head = container.trim_end_matches('/');
    let tail = path.trim_start_matches('/');
    format!("{head}/{tail}")
}
