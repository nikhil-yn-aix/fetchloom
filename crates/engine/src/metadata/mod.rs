//! Reading a manifest out of metadata somebody else already published.

pub mod bagit;
pub mod croissant;
pub mod frictionless;
pub mod pooch;
pub mod sidecar;

use crate::error::{Error, ErrorKind};
use crate::limits::Limits;
use crate::manifest::Manifest;

/// A published metadata format a manifest can be read out of.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MetadataFormat {
    /// A `sha256sum` style sidecar listing a digest and a path per line.
    ChecksumSidecar,
    /// A Croissant dataset description.
    Croissant,
    /// A Frictionless data package descriptor.
    FrictionlessPackage,
    /// A pooch registry listing a path and a hash per line.
    PoochRegistry,
    /// A `BagIt` payload manifest, with the fetch file beside it.
    BagIt,
}

impl MetadataFormat {
    /// Returns the name this format is reported under.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::ChecksumSidecar => "a checksum sidecar",
            Self::Croissant => "a Croissant description",
            Self::FrictionlessPackage => "a Frictionless data package",
            Self::PoochRegistry => "a pooch registry",
            Self::BagIt => "a BagIt manifest",
        }
    }
}

/// Everything one reader needs that is not in the document itself.
#[derive(Clone, Debug)]
pub struct Context<'a> {
    /// Where the document was read from, which every relative source in it
    /// resolves against.
    pub base: &'a str,
    /// The name the dataset takes when the document states none.
    pub name: &'a str,
    /// What bounds the document and what it may name.
    pub limits: &'a Limits,
}

/// Returns the failure a format states honestly rather than approximating.
///
/// A refusal names the format, what it stated, and why that cannot be a
/// manifest field, because an approximation would record a number under a name
/// that does not mean it.
#[must_use]
pub fn unrepresentable(what: &str, why: &str) -> Error {
    Error::new(
        ErrorKind::ManifestInvalid,
        format!("{why}, so {what} cannot be read as a manifest without inventing what it does not state"),
    )
}

/// Returns the failure a document that does not parse states.
#[must_use]
pub fn malformed(format: MetadataFormat, why: &str) -> Error {
    Error::new(
        ErrorKind::ManifestInvalid,
        format!("correct {}, because {why}", format.label()),
    )
}

/// Reads a manifest out of a published metadata document.
pub trait MetadataReader {
    /// Returns the format this reader reads.
    fn format(&self) -> MetadataFormat;

    /// Reports whether this reader recognizes a document, from the name it was
    /// read under and its leading bytes.
    fn recognizes(&self, name: &str, bytes: &[u8]) -> bool;

    /// Reads the document into a manifest.
    ///
    /// # Errors
    ///
    /// Fails with `manifest.invalid` when the document does not parse, and when
    /// it states something that cannot be represented honestly.
    fn read(&self, bytes: &[u8], context: &Context<'_>) -> Result<Manifest, Error>;
}
