//! The seam every source sits behind, and the errors a source reports.

use fl_model::error::ErrorKind;
use fl_model::filter::Unmatched;
use fl_model::reference::Reference;
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
            Self::Unresolved { .. } | Self::NoSource { .. } => ErrorKind::ReferenceUnresolved,
            Self::Invalid { .. } => ErrorKind::ReferenceInvalid,
            Self::Transfer(error) => error.kind(),
            Self::Selection(error) => error.kind(),
            Self::Record { .. } | Self::Body { .. } => ErrorKind::NetworkProtocol,
            Self::Adapter { .. } => ErrorKind::LocalInvalidConfig,
        }
    }
}
