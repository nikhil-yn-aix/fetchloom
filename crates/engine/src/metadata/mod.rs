//! Reading a manifest out of metadata somebody else already published.

mod digestline;

pub mod bagit;
pub mod croissant;
pub mod frictionless;
pub mod pooch;
pub mod sidecar;

use crate::error::{Error, ErrorKind};
use crate::limits::Limits;
use crate::manifest::Manifest;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MetadataFormat {
    ChecksumSidecar,
    Croissant,
    FrictionlessPackage,
    PoochRegistry,
    BagIt,
}

impl MetadataFormat {
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

#[derive(Clone, Debug)]
pub struct Context<'a> {
    pub base: &'a str,
    pub name: &'a str,
    pub limits: &'a Limits,
}

#[must_use]
pub fn unrepresentable(what: &str, why: &str) -> Error {
    Error::new(
        ErrorKind::ManifestInvalid,
        format!(
            "{why}, so {what} cannot be read as a manifest without inventing what it does not state"
        ),
    )
}

#[must_use]
pub fn malformed(format: MetadataFormat, why: &str) -> Error {
    Error::new(
        ErrorKind::ManifestInvalid,
        format!("correct {}, because {why}", format.label()),
    )
}

pub trait MetadataReader {
    fn format(&self) -> MetadataFormat;

    fn recognizes(&self, name: &str, bytes: &[u8]) -> bool;

    /// # Errors
    /// `manifest.invalid` when the document does not parse or names nothing
    /// this reader can turn into artifacts.
    fn read(&self, bytes: &[u8], context: &Context<'_>) -> Result<Manifest, Error>;
}
