//! The one model every accepted manifest syntax parses into.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::digest::{ContentDigest, InteropDigest};
use crate::error::{Error, ErrorKind};
use crate::license::License;
use crate::selection::{Glob, Layout};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DigestClaims {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blake3: Option<ContentDigest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<InteropDigest>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ArchiveFormat {
    #[serde(rename = "tar")]
    Tar,
    #[serde(rename = "tar+gzip")]
    TarGzip,
    #[serde(rename = "tar+zstd")]
    TarZstd,
    #[serde(rename = "tar+xz")]
    TarXz,
    #[serde(rename = "tar+bzip2")]
    TarBzip2,
    #[serde(rename = "zip")]
    Zip,
    #[serde(rename = "gzip")]
    Gzip,
    #[serde(rename = "zstd")]
    Zstd,
    #[serde(rename = "xz")]
    Xz,
    #[serde(rename = "bzip2")]
    Bzip2,
}

impl ArchiveFormat {
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveSpec {
    pub format: ArchiveFormat,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub id: String,
    pub sources: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<DigestClaims>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive: Option<ArchiveSpec>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub select: Vec<Glob>,
    #[serde(default)]
    pub layout: Layout,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release: Option<String>,
    pub artifacts: Vec<Artifact>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<License>,
}

impl Manifest {
    /// # Errors
    /// `manifest.invalid` when the document does not parse, names no dataset,
    /// or holds no artifact.
    pub fn parse(
        bytes: &[u8],
        syntax: crate::document::Syntax,
        limits: &crate::limits::Limits,
    ) -> Result<Self, Error> {
        let manifest: Self = crate::document::read_model_in(
            bytes,
            syntax,
            "manifest",
            limits,
            crate::document::Bound::Foreign,
        )?;
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

    /// # Errors
    /// `manifest.invalid` when the manifest cannot be written in its canonical
    /// form, which is what the digest is taken over.
    pub fn digest(&self) -> Result<crate::digest::ManifestDigest, Error> {
        Ok(crate::canonical::manifest_digest(
            &crate::document::canonical_json_of(self)?,
        ))
    }
}
