//! What is known about the bytes a run produced, and the witnesses that raise
//! it.

use serde::{Deserialize, Serialize};

use crate::digest::{ContentDigest, ManifestDigest, WITNESS_KEY_CONTEXT};
use crate::identity::MachineId;

/// The mechanical class of evidence behind an object's bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustClass {
    /// The content digest matched a digest supplied before this run.
    Verified,
    /// No prior digest, and the observed digest matched at least two
    /// independent recorded witnesses.
    Corroborated,
    /// No prior digest and no witnesses. The observed digest is recorded.
    Tofu,
    /// Content could not be digested, or the user disabled verification.
    Unverified,
}

/// The identity of one run, which is one process on one machine.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RunId(String);

impl RunId {
    /// Builds a run identity from a value that is unique to one process on one
    /// machine.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Returns the value.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The name the witnesses for one artifact are filed under.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ArtifactKey([u8; 32]);

impl ArtifactKey {
    /// Returns the key one artifact of one manifest is filed under.
    #[must_use]
    pub fn of(manifest: ManifestDigest, artifact: &str) -> Self {
        let mut hasher = blake3::Hasher::new_derive_key(WITNESS_KEY_CONTEXT);
        hasher.update(&(manifest.bytes().len() as u64).to_le_bytes());
        hasher.update(manifest.bytes());
        hasher.update(&(artifact.len() as u64).to_le_bytes());
        hasher.update(artifact.as_bytes());
        Self(*hasher.finalize().as_bytes())
    }

    /// Returns the raw bytes of the key.
    #[must_use]
    pub fn bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// One recorded observation that an artifact hashed to a digest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Witness {
    /// What the bytes hashed to.
    pub digest: ContentDigest,
    /// The machine that observed them.
    pub machine: MachineId,
    /// The origin that served them, as the host and path.
    pub origin: String,
    /// The run that recorded the observation.
    pub run: RunId,
    /// When the observation was recorded.
    pub observed_at: crate::timestamp::Timestamp,
}

/// The three facts a run records separately about an artifact.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustRecord {
    /// What is known about who published the artifact.
    pub publisher_identity: Option<String>,
    /// What is known about the authenticity of the manifest.
    pub manifest_authenticity: Option<String>,
    /// What is known about the integrity of the content.
    pub content_integrity: TrustClass,
}

/// Returns the class the evidence supports.
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

/// Reports whether any two of these witnesses are independent of each other.
fn has_an_independent_pair(witnesses: &[&Witness]) -> bool {
    witnesses.iter().enumerate().any(|(index, one)| {
        witnesses
            .iter()
            .skip(index + 1)
            .any(|other| independent(one, other))
    })
}

/// Reports whether two witnesses are independent.
fn independent(one: &Witness, other: &Witness) -> bool {
    one.machine != other.machine && one.origin != other.origin && one.run != other.run
}

impl TrustClass {
    /// Returns the name the contract lists this class under.
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
