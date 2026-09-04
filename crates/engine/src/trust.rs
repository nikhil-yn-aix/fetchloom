//! What is known about the bytes a run produced, and the witnesses that raise
//! it.

use serde::{Deserialize, Serialize};

use crate::digest::{ContentDigest, ManifestDigest, WITNESS_KEY_CONTEXT};
use crate::identity::MachineId;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustClass {
    Verified,
    Corroborated,
    Tofu,
    Unverified,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RunId(String);

impl RunId {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ArtifactKey([u8; 32]);

impl ArtifactKey {
    #[must_use]
    pub fn of(manifest: ManifestDigest, artifact: &str) -> Self {
        let mut hasher = blake3::Hasher::new_derive_key(WITNESS_KEY_CONTEXT);
        hasher.update(&(manifest.bytes().len() as u64).to_le_bytes());
        hasher.update(manifest.bytes());
        hasher.update(&(artifact.len() as u64).to_le_bytes());
        hasher.update(artifact.as_bytes());
        Self(*hasher.finalize().as_bytes())
    }

    #[must_use]
    pub fn bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Witness {
    pub digest: ContentDigest,
    pub machine: MachineId,
    pub origin: String,
    pub run: RunId,
    pub observed_at: crate::timestamp::Timestamp,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustRecord {
    pub publisher_identity: Option<String>,
    pub manifest_authenticity: Option<String>,
    pub content_integrity: TrustClass,
}

#[must_use]
pub fn classify(
    prior: Option<ContentDigest>,
    observed: ContentDigest,
    witnesses: &[Witness],
) -> TrustClass {
    if prior == Some(observed) {
        return TrustClass::Verified;
    }
    let agreeing: Vec<&Witness> = witnesses
        .iter()
        .filter(|witness| witness.digest == observed)
        .collect();
    if has_an_independent_pair(&agreeing) {
        TrustClass::Corroborated
    } else {
        TrustClass::Tofu
    }
}

fn has_an_independent_pair(witnesses: &[&Witness]) -> bool {
    witnesses.iter().enumerate().any(|(index, one)| {
        witnesses
            .iter()
            .skip(index + 1)
            .any(|other| independent(one, other))
    })
}

fn independent(one: &Witness, other: &Witness) -> bool {
    one.machine != other.machine && one.origin != other.origin && one.run != other.run
}

impl TrustClass {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::Corroborated => "corroborated",
            Self::Tofu => "tofu",
            Self::Unverified => "unverified",
        }
    }
}
