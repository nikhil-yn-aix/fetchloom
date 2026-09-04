//! The key a partial, its lease, its source record, and its owner record are
//! named by.

use crate::digest::{ContentDigest, PARTIAL_KEY_CONTEXT};
use crate::seam::source::{SourceIdentity, SourceMetadata};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PartialKey {
    name: ContentDigest,
    expected: Option<ContentDigest>,
}

impl PartialKey {
    #[must_use]
    pub fn of_content(digest: ContentDigest) -> Self {
        Self {
            name: digest,
            expected: Some(digest),
        }
    }

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

    #[must_use]
    pub fn name(&self) -> ContentDigest {
        self.name
    }

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
