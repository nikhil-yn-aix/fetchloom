//! Dataset sources of fetchloom: the source traits, adapters, the declarative engine and the plugin protocol that turn references into listings.

pub mod croissant;
pub mod declarative;
mod doi;
pub mod file;
pub mod hf;
pub mod http;
mod index;
pub mod license;
pub mod listing;
mod paging;
mod record;
pub mod registry;
pub mod source;
mod template;

pub use croissant::Croissant;
pub use declarative::Declarative;
pub use file::Files;
pub use hf::HuggingFace;
pub use http::Http;
pub use listing::{Entry, Listing, Meta};
pub use registry::Registry;
pub use source::{Hit, Searchable, Source, SourceError};
