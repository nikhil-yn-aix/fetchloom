//! What names a volume, a file, and the belief that a file is unchanged.

use serde::{Deserialize, Serialize};

/// The identifier of one volume on one machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct VolumeId(u64);

impl VolumeId {
    /// Builds a volume identifier from the platform's own value.
    #[must_use]
    pub fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the platform's own value.
    #[must_use]
    pub fn value(self) -> u64 {
        self.0
    }
}

/// The identifier of one file within one volume.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct FileId(u128);

impl FileId {
    /// Builds a file identifier from the platform's own value.
    #[must_use]
    pub fn new(value: u128) -> Self {
        Self(value)
    }

    /// Returns the platform's own value.
    #[must_use]
    pub fn value(self) -> u128 {
        self.0
    }
}

/// The tuple recording that a file is probably unchanged.
///
/// It is never evidence of content and never appears in a lock.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Fingerprint {
    /// The volume the file is on.
    pub volume: VolumeId,
    /// The file within that volume.
    pub file: FileId,
    /// The length of the file in bytes.
    pub size: u64,
    /// The modification time in nanoseconds since the epoch.
    pub modified_nanos: i128,
    /// The change time in nanoseconds since the epoch.
    pub changed_nanos: i128,
}

/// The identity of the machine a cache record was written on.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MachineId(String);

impl MachineId {
    /// Builds a machine identity from the platform's own value.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Returns the platform's own value.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The identity of one boot of one machine.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BootId(String);

impl BootId {
    /// Builds a boot identity from the platform's own value.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Returns the platform's own value.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The fingerprint of the cache format the running build writes.
///
/// It is compared for equality and nothing is ever branched on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct CacheFormatFingerprint(crate::digest::ContentDigest);

impl CacheFormatFingerprint {
    /// Builds a cache format fingerprint from the hash of the format
    /// definition.
    #[must_use]
    pub fn new(digest: crate::digest::ContentDigest) -> Self {
        Self(digest)
    }

    /// Returns the hash of the format definition.
    #[must_use]
    pub fn digest(self) -> crate::digest::ContentDigest {
        self.0
    }
}
