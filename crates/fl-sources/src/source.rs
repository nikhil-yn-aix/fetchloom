//! The seam every source sits behind, and the errors a source reports.

use fl_model::error::ErrorKind;
use fl_model::filter::Unmatched;
use fl_model::reference::Reference;
use std::sync::Arc;

use fl_transfer::credentials::Variable;
use fl_transfer::error::TransferError;
use fl_transfer::url::SafeUrl;
use futures_util::future::BoxFuture;

use crate::listing::Listing;

/// Something that turns references of its schemes into listings.
pub trait Source: Send + Sync {
    /// The schemes this source serves, lowercase and without the colon.
    fn schemes(&self) -> &[String];

    /// Resolves `reference` to the files it names.
    fn resolve<'a>(
        &'a self,
        reference: &'a Reference,
    ) -> BoxFuture<'a, Result<Listing, SourceError>>;

    /// Resolves `reference` when the user did not write it, such as one a DOI led to: no host
    /// it names is trusted as one the user typed.
    fn resolve_routed<'a>(
        &'a self,
        reference: &'a Reference,
    ) -> BoxFuture<'a, Result<Listing, SourceError>> {
        self.resolve(reference)
    }

    /// The reference of this source that a landing page at `url` shows, such as the record a
    /// DOI resolves to. None when the page is not one of this source's.
    fn landing(&self, url: &str) -> Option<Reference> {
        let _ = url;
        None
    }

    /// The search this source offers over its records, if it offers one.
    fn searchable(&self) -> Option<&dyn Searchable> {
        None
    }

    /// The credential this source declares, its token read through `read`, if it declares one.
    fn credential(&self, read: Read) -> Option<Variable> {
        let _ = read;
        None
    }
}

/// Reads an environment variable by name.
pub type Read = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// One record a search found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    /// The reference that resolves it.
    pub reference: Reference,
    /// Its title, when the source states one.
    pub title: Option<String>,
}

/// A source that can search its records by text.
pub trait Searchable: Send + Sync {
    /// At most `limit` records matching `query`, best first.
    fn search<'a>(
        &'a self,
        query: &'a str,
        limit: usize,
    ) -> BoxFuture<'a, Result<Vec<Hit>, SourceError>>;
}

/// A failure to resolve a reference.
#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    /// The reference names nothing the source knows.
    #[error("{reference} names nothing the source knows")]
    Unresolved {
        /// The reference as written.
        reference: String,
    },
    /// No source serves the reference's scheme.
    #[error("no source serves {scheme}")]
    NoSource {
        /// The scheme, or a description of the kind of reference when it has none.
        scheme: String,
    },
    /// The reference is not valid for the source it names.
    #[error("{reference} is not a valid reference: {reason}")]
    Invalid {
        /// The reference as written.
        reference: String,
        /// What is wrong with it.
        reason: String,
    },
    /// Fetching the record failed.
    #[error(transparent)]
    Transfer(#[from] TransferError),
    /// A filter pattern matched no file of the listing.
    #[error(transparent)]
    Selection(#[from] Unmatched),
    /// The record does not have the shape the adapter describes.
    #[error("the record at {url} has {problem} at {path}")]
    Record {
        /// The `JSONPath`, or the name of the template, that failed.
        path: String,
        /// The location of the record.
        url: SafeUrl,
        /// What is wrong with the value found.
        problem: &'static str,
    },
    /// The record is not JSON.
    #[error("the record at {url} is not JSON: {reason}")]
    Body {
        /// The location of the record.
        url: SafeUrl,
        /// What the parser said.
        reason: String,
    },
    /// A DOI landed on a page no source serves.
    #[error("{reference} lands on {landing}, and no source serves {host}")]
    NoLanding {
        /// The DOI reference as written.
        reference: String,
        /// The landing page's host.
        host: String,
        /// The landing page.
        landing: String,
    },
    /// Two files of a listing have names that differ only by case, which one directory cannot
    /// hold on every filesystem.
    #[error(
        "`{first}` and `{second}` differ only by case, which windows and macos cannot hold apart"
    )]
    Collision {
        /// The first of the two paths.
        first: String,
        /// The second.
        second: String,
    },
    /// The record is gated: its terms must be accepted, signed in, before its files are served.
    #[error("{reference} is gated: accept its terms at {page} and give fetchloom a token")]
    Terms {
        /// The reference as written.
        reference: String,
        /// The page where the terms are accepted.
        page: String,
    },
    /// A local file or directory cannot be read.
    #[error("`{}` cannot be read: {reason}", path.display())]
    Local {
        /// The file or directory.
        path: std::path::PathBuf,
        /// What went wrong.
        reason: String,
    },
    /// An adapter file cannot be used.
    #[error("the adapter is not valid: {reason}")]
    Adapter {
        /// What is wrong with it, naming the key or path at fault.
        reason: String,
    },
}

impl SourceError {
    /// The kind a command reports for this error.
    #[must_use]
    pub const fn kind(&self) -> ErrorKind {
        match self {
            Self::Unresolved { .. } | Self::NoSource { .. } | Self::NoLanding { .. } => {
                ErrorKind::ReferenceUnresolved
            }
            Self::Collision { .. } => ErrorKind::LocalUnrepresentable,
            Self::Terms { .. } => ErrorKind::TermsRequired,
            Self::Local { .. } => ErrorKind::LocalIo,
            Self::Invalid { .. } => ErrorKind::ReferenceInvalid,
            Self::Transfer(error) => error.kind(),
            Self::Selection(error) => error.kind(),
            Self::Record { .. } | Self::Body { .. } => ErrorKind::NetworkProtocol,
            Self::Adapter { .. } => ErrorKind::LocalInvalidConfig,
        }
    }
}
