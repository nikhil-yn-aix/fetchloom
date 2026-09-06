//! What names a volume, a file, and the belief that a file is unchanged.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct VolumeId(u64);

impl VolumeId {
    #[must_use]
    pub fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub fn value(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct FileId(u128);

impl FileId {
    #[must_use]
    pub fn new(value: u128) -> Self {
        Self(value)
    }

    #[must_use]
    pub fn value(self) -> u128 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Fingerprint {
    pub volume: VolumeId,
    pub file: FileId,
    pub size: u64,
    pub modified_nanos: i128,
    pub changed_nanos: i128,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MachineId(String);

impl MachineId {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BootId(String);

impl BootId {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct CacheFormatFingerprint(crate::digest::ContentDigest);

impl CacheFormatFingerprint {
    #[must_use]
    pub fn new(digest: crate::digest::ContentDigest) -> Self {
        Self(digest)
    }

    #[must_use]
    pub fn digest(self) -> crate::digest::ContentDigest {
        self.0
    }
}

impl Fingerprint {
    #[must_use]
    pub fn settled_before(self, instant: i128) -> bool {
        self.modified_nanos < instant && self.changed_nanos < instant
    }
}

#[must_use]
pub fn now_nanos() -> i128 {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(elapsed) => i128::try_from(elapsed.as_nanos()).unwrap_or(i128::MAX),
        Err(before) => i128::try_from(before.duration().as_nanos()).map_or(i128::MIN, |n| -n),
    }
}
