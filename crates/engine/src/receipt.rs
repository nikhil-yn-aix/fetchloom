//! The local record of what a run actually did.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::digest::{ContentDigest, ManifestDigest, RECEIPT_KEY_CONTEXT, TreeDigest};
use crate::error::Error;
use crate::identity::Fingerprint;
use crate::license::Acceptance;
use crate::redact::SafeUrl;
use crate::timestamp::Timestamp;
use crate::tree::Mode;
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
    /// The entry paths the run materialized with the executable mode, in
    /// ascending order. Every other file entry carries the read and write mode.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub executable: Vec<String>,
    /// The fingerprint each file entry carried when the run published it.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fingerprints: BTreeMap<String, RecordedFingerprint>,
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

/// The fingerprint tuple of one destination file, as a receipt records it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordedFingerprint {
    /// The volume the file was on.
    pub volume: String,
    /// The file within that volume.
    pub file: String,
    /// The length of the file in bytes.
    pub size: u64,
    /// The modification time in nanoseconds since the epoch.
    pub modified_nanos: String,
    /// The change time in nanoseconds since the epoch.
    pub changed_nanos: String,
}

impl RecordedFingerprint {
    /// Records what a file carried when it was published.
    #[must_use]
    pub fn new(found: Fingerprint) -> Self {
        Self {
            volume: found.volume.value().to_string(),
            file: found.file.value().to_string(),
            size: found.size,
            modified_nanos: found.modified_nanos.to_string(),
            changed_nanos: found.changed_nanos.to_string(),
        }
    }

    /// Reports whether a fingerprint read now is the one that was recorded.
    #[must_use]
    pub fn matches(&self, now: Fingerprint) -> bool {
        *self == Self::new(now)
    }
}

impl Receipt {
    /// Returns the name a receipt for a destination is stored under.
    #[must_use]
    pub fn key(destination: &Path) -> ContentDigest {
        let mut hasher = blake3::Hasher::new_derive_key(RECEIPT_KEY_CONTEXT);
        hasher.update(destination.as_os_str().as_encoded_bytes());
        ContentDigest::from_bytes(*hasher.finalize().as_bytes())
    }

    /// Returns the mode this receipt states for one entry path.
    #[must_use]
    pub fn mode_of(&self, path: &str) -> Mode {
        if self
            .executable
            .binary_search_by(|listed| listed.as_str().cmp(path))
            .is_ok()
        {
            Mode::Executable
        } else {
            Mode::ReadWrite
        }
    }

    /// Renders this receipt as the canonical text it is written in.
    ///
    /// # Errors
    ///
    /// Fails when the model cannot be written.
    pub fn render(&self) -> Result<String, Error> {
        crate::document::render_model(self)
    }

    /// Reads a receipt from the text it was written in.
    ///
    /// # Errors
    ///
    /// Fails with `manifest.invalid` when the text does not parse or holds a
    /// key this build does not read.
    pub fn parse(bytes: &[u8], limits: &crate::limits::Limits) -> Result<Self, Error> {
        crate::document::read_model(bytes, "receipt", limits)
    }
}
