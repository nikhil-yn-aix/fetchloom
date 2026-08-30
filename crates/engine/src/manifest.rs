//! The one model every accepted manifest syntax parses into.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::digest::{ContentDigest, InteropDigest};
use crate::error::{Error, ErrorKind};
use crate::license::License;
use crate::selection::{Glob, Layout};

/// The digests a manifest claims for an artifact.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DigestClaims {
    /// The content digest the publisher claims.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blake3: Option<ContentDigest>,
    /// The interop digest the publisher claims.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<InteropDigest>,
}

/// The name of an archive format a manifest declares.
///
/// These are the only values `archive.format` takes and the only containers
/// extraction recognizes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ArchiveFormat {
    /// A POSIX ustar stream.
    #[serde(rename = "tar")]
    Tar,
    /// A tar wrapped in a gzip member.
    #[serde(rename = "tar+gzip")]
    TarGzip,
    /// A tar wrapped in a zstd frame.
    #[serde(rename = "tar+zstd")]
    TarZstd,
    /// A tar wrapped in an xz stream.
    #[serde(rename = "tar+xz")]
    TarXz,
    /// A tar wrapped in a bzip2 stream.
    #[serde(rename = "tar+bzip2")]
    TarBzip2,
    /// A zip container, store and deflate methods only.
    #[serde(rename = "zip")]
    Zip,
    /// One gzip-compressed object, materialized as one file.
    #[serde(rename = "gzip")]
    Gzip,
    /// One zstd-compressed object, materialized as one file.
    #[serde(rename = "zstd")]
    Zstd,
    /// One xz-compressed object, materialized as one file.
    #[serde(rename = "xz")]
    Xz,
    /// One bzip2-compressed object, materialized as one file.
    #[serde(rename = "bzip2")]
    Bzip2,
}

impl ArchiveFormat {
    /// Returns the name this format is written with.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Tar => "tar",
            Self::TarGzip => "tar+gzip",
            Self::TarZstd => "tar+zstd",
            Self::TarXz => "tar+xz",
            Self::TarBzip2 => "tar+bzip2",
            Self::Zip => "zip",
            Self::Gzip => "gzip",
            Self::Zstd => "zstd",
            Self::Xz => "xz",
            Self::Bzip2 => "bzip2",
        }
    }
}

impl fmt::Display for ArchiveFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

impl FromStr for ArchiveFormat {
    type Err = Error;

    /// Parses a format name into the one value it names.
    ///
    /// Takes the name exactly as a manifest or a location extension wrote it.
    /// Fails with `archive.unsupported` naming the format that was asked for
    /// when the name is not one of the ten this build carries.
    fn from_str(name: &str) -> Result<Self, Self::Err> {
        match name {
            "tar" => Ok(Self::Tar),
            "tar+gzip" => Ok(Self::TarGzip),
            "tar+zstd" => Ok(Self::TarZstd),
            "tar+xz" => Ok(Self::TarXz),
            "tar+bzip2" => Ok(Self::TarBzip2),
            "zip" => Ok(Self::Zip),
            "gzip" => Ok(Self::Gzip),
            "zstd" => Ok(Self::Zstd),
            "xz" => Ok(Self::Xz),
            "bzip2" => Ok(Self::Bzip2),
            other => Err(Error::new(
                ErrorKind::ArchiveUnsupported,
                format!("archive format \"{other}\" is not one this build carries"),
            )),
        }
    }
}

/// What a manifest says about the archive an artifact is packed in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveSpec {
    /// The format the artifact is packed in.
    pub format: ArchiveFormat,
}

/// One addressable thing a manifest names.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    /// The name this artifact is referred to by.
    pub id: String,
    /// Where the artifact can be fetched from, in order of preference.
    pub sources: Vec<String>,
    /// The length of the artifact in bytes, when the publisher states it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// The digests the publisher claims, which may be absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<DigestClaims>,
    /// The media type the publisher states.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    /// What the artifact is packed in, when it is packed at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive: Option<ArchiveSpec>,
    /// Member paths to include. An empty list means every member.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub select: Vec<Glob>,
    /// How member paths are rewritten.
    #[serde(default)]
    pub layout: Layout,
}

/// A dataset and the artifacts it is made of.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// The name of the dataset.
    pub name: String,
    /// The release of the dataset, when the publisher names one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release: Option<String>,
    /// The artifacts the dataset is made of. At least one is required.
    pub artifacts: Vec<Artifact>,
    /// What the manifest records about terms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<License>,
}

impl Manifest {
    /// Reads a manifest written in any of the three accepted syntaxes.
    ///
    /// Takes the bytes exactly as they were read, the syntax to read them in,
    /// and the bounds a document may not exceed. Returns the one model all
    /// three syntaxes parse into.
    ///
    /// # Errors
    ///
    /// Fails with `manifest.invalid` when the document does not parse, when it
    /// holds a key this build does not read, and when it names no artifact.
    pub fn parse(
        bytes: &[u8],
        syntax: crate::document::Syntax,
        limits: &crate::limits::Limits,
    ) -> Result<Self, Error> {
        let manifest: Self = crate::document::read_model_in(bytes, syntax, "manifest", limits)?;
        if manifest.name.is_empty() {
            return Err(Error::new(
                ErrorKind::ManifestInvalid,
                "give the manifest a name, because a dataset is named",
            ));
        }
        if manifest.artifacts.is_empty() {
            return Err(Error::new(
                ErrorKind::ManifestInvalid,
                "name at least one artifact, because a manifest with none resolves to nothing",
            ));
        }
        Ok(manifest)
    }

    /// Returns the digest of this manifest's canonical form.
    ///
    /// The digest covers the canonical JSON of the model, never the text a
    /// manifest was written in, so reformatting a manifest or writing it in
    /// another accepted syntax does not change what it identifies.
    ///
    /// # Errors
    ///
    /// Fails when the canonical form cannot be written.
    pub fn digest(&self) -> Result<crate::digest::ManifestDigest, Error> {
        Ok(crate::canonical::manifest_digest(
            &crate::document::canonical_json_of(self)?,
        ))
    }
}
