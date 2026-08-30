//! The key a partial, its lease, its source record, and its owner record are
//! named by.

use crate::digest::{ContentDigest, PARTIAL_KEY_CONTEXT};
use crate::seam::source::{SourceIdentity, SourceMetadata};

/// The key a run's in-progress transfer is claimed and stored under.
///
/// A run that states a content digest is named by that digest, and the
/// object it publishes must hash to it. A run that states none is named by
/// the digest of the source identity, and the object it publishes is named
/// by whatever digest the bytes hash to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PartialKey {
    name: ContentDigest,
    expected: Option<ContentDigest>,
}

impl PartialKey {
    /// Names a partial by the content digest a run already states.
    ///
    /// Takes that digest. Returns a key whose name is the digest and whose
    /// expectation is the same digest.
    #[must_use]
    pub fn of_content(digest: ContentDigest) -> Self {
        Self {
            name: digest,
            expected: Some(digest),
        }
    }

    /// Names a partial by the identity a source published, for a run that
    /// states no digest.
    ///
    /// Takes what a bounded metadata request learned. Returns a key whose
    /// name is the domain-separated BLAKE3 digest of the redacted location,
    /// the host, and the identity the source published, each
    /// length-prefixed in that order, and whose expectation is nothing, so
    /// the object it publishes is named by whatever digest the bytes hash
    /// to.
    #[must_use]
    pub fn of_source(metadata: &SourceMetadata) -> Self {
        let mut hasher = blake3::Hasher::new_derive_key(PARTIAL_KEY_CONTEXT);
        hash_field(&mut hasher, metadata.location.as_str().as_bytes());
        hash_field(&mut hasher, metadata.host.as_str().as_bytes());
        hash_field(&mut hasher, &encode_identity(&metadata.identity));
        Self {
            name: ContentDigest::from_bytes(*hasher.finalize().as_bytes()),
            expected: None,
        }
    }

    /// Returns the name a partial, its lease, its source record and its
    /// owner record are stored under.
    #[must_use]
    pub fn name(&self) -> ContentDigest {
        self.name
    }

    /// Returns the digest the object this key names must hash to, when the
    /// run that named this key stated one.
    #[must_use]
    pub fn expected(&self) -> Option<ContentDigest> {
        self.expected
    }
}

fn hash_field(hasher: &mut blake3::Hasher, field: &[u8]) {
    hasher.update(&(field.len() as u64).to_le_bytes());
    hasher.update(field);
}

fn encode_identity(identity: &SourceIdentity) -> Vec<u8> {
    let mut encoded = Vec::new();
    match identity {
        SourceIdentity::ContentAddress(digest) => {
            encoded.push(0);
            encoded.extend_from_slice(digest.bytes());
        }
        SourceIdentity::ImmutableVersion(version) => {
            encoded.push(1);
            encoded.extend_from_slice(version.as_bytes());
        }
        SourceIdentity::StrongValidator(tag) => {
            encoded.push(2);
            encoded.extend_from_slice(tag.as_bytes());
        }
        SourceIdentity::WeakValidator(tag) => {
            encoded.push(3);
            encoded.extend_from_slice(tag.as_bytes());
        }
        SourceIdentity::None => {
            encoded.push(4);
        }
    }
    encoded
}
