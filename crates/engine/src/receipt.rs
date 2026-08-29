//! The local record of what a run actually did.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::digest::{ContentDigest, ManifestDigest, TreeDigest};
use crate::license::Acceptance;
use crate::redact::SafeUrl;
use crate::timestamp::Timestamp;
use crate::trust::TrustClass;

/// One artifact as a receipt records it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptArtifact {
    /// The content digest the bytes had.
    pub digest: ContentDigest,
    /// The source the bytes came from, redacted when it was recorded.
    pub source_used: SafeUrl,
    /// What was known about the bytes.
    pub trust: TrustClass,
}

/// What one run did, on this machine.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    /// The dataset the run materialized.
    pub dataset: String,
    /// The manifest the run resolved from.
    pub manifest: ManifestDigest,
    /// The artifacts, by the name the manifest gave each one.
    pub artifacts: BTreeMap<String, ReceiptArtifact>,
    /// The tree the run materialized, when it materialized one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tree: Option<TreeDigest>,
    /// Where the tree was materialized.
    pub destination: PathBuf,
    /// Whether the user asserted acceptance of the recorded terms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted_terms: Option<Acceptance>,
    /// The release version of the build that produced this receipt.
    pub fetchloom: String,
    /// When the run finished.
    pub completed_at: Timestamp,
}
