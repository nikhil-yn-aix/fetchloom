//! The one model every accepted manifest syntax parses into.

use serde::{Deserialize, Serialize};

use crate::digest::{ContentDigest, InteropDigest};
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
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ArchiveFormat(String);

impl ArchiveFormat {
    /// Keeps a format name exactly as the manifest wrote it.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    /// Returns the format name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
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
