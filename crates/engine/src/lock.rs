//! What a lock pins, containing nothing local to one machine.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::digest::{ContentDigest, InteropDigest, ManifestDigest, TreeDigest};
use crate::selection::{Glob, Layout};

/// One artifact as a lock pins it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LockedArtifact {
    /// The content digest the bytes must have.
    pub digest: ContentDigest,
    /// The interop digest recorded alongside it.
    pub interop: InteropDigest,
    /// The length of the artifact in bytes.
    pub size: u64,
    /// The member paths the lock entry covers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub select: Vec<Glob>,
    /// How member paths are rewritten.
    #[serde(default)]
    pub layout: Layout,
}

/// One dataset as a lock pins it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LockedDataset {
    /// The manifest the entry was resolved from.
    pub manifest: ManifestDigest,
    /// The release the entry was resolved at, when the manifest names one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release: Option<String>,
    /// The artifacts, by the name the manifest gave each one.
    pub artifacts: BTreeMap<String, LockedArtifact>,
    /// The tree the artifacts materialized to, present only after a successful
    /// materialization.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tree: Option<TreeDigest>,
}

/// The portable record of what a run resolved to.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lock {
    /// The datasets pinned, by name.
    pub datasets: BTreeMap<String, LockedDataset>,
}
