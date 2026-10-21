//! Dataset sources of fetchloom: the source traits, adapters, the declarative engine and the plugin protocol that turn references into listings.

pub mod declarative;
pub mod http;
pub mod listing;
mod record;
pub mod registry;
pub mod source;

pub use declarative::Declarative;
pub use http::Http;
pub use listing::{Entry, Listing, Meta};
pub use registry::Registry;
pub use source::{Source, SourceError};
