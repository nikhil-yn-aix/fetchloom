//! What is known about the bytes a run produced.

use serde::{Deserialize, Serialize};

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

/// A recorded observation of a digest for an artifact, from one origin.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Witness {
    /// The origin that reported the digest.
    pub origin: String,
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
