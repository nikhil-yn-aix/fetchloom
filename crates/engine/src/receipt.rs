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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptArtifact {
    pub digest: ContentDigest,
    pub source_used: SafeUrl,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_reason: Option<String>,
    pub trust: TrustClass,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub dataset: String,
    pub manifest: ManifestDigest,
    pub artifacts: BTreeMap<String, ReceiptArtifact>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tree: Option<TreeDigest>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub executable: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fingerprints: BTreeMap<String, RecordedFingerprint>,
    pub destination: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted_terms: Option<Acceptance>,
    pub fetchloom: String,
    pub completed_at: Timestamp,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordedFingerprint {
    pub volume: String,
    pub file: String,
    pub size: u64,
    pub modified_nanos: String,
    pub changed_nanos: String,
}

impl RecordedFingerprint {
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

    #[must_use]
    pub fn matches(&self, now: Fingerprint) -> bool {
        *self == Self::new(now)
    }
}

impl Receipt {
    #[must_use]
    pub fn key(destination: &Path) -> ContentDigest {
        let mut hasher = blake3::Hasher::new_derive_key(RECEIPT_KEY_CONTEXT);
        hasher.update(destination.as_os_str().as_encoded_bytes());
        ContentDigest::from_bytes(*hasher.finalize().as_bytes())
    }

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

    pub fn render(&self) -> Result<String, Error> {
        crate::document::render_model(self)
    }

    pub fn parse(bytes: &[u8], limits: &crate::limits::Limits) -> Result<Self, Error> {
        crate::document::read_model(bytes, "receipt", limits)
    }
}
