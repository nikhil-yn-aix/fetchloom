//! The records the cache writes beside its entries.

use std::path::Path;

use fetchloom_engine::error::{Error, ErrorKind};
use fetchloom_engine::identity::{Fingerprint, VolumeId};
use fetchloom_engine::seam::platform::OwnerToken;
use serde::{Deserialize, Serialize};

/// The fingerprint of an object as it was when the object was published.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordedFingerprint {
    /// The volume the object is on.
    pub volume: u64,
    /// The object within that volume.
    pub file: u128,
    /// The length of the object in bytes.
    pub size: u64,
    /// The modification time in nanoseconds since the epoch.
    pub modified_nanos: i128,
    /// The change time in nanoseconds since the epoch.
    pub changed_nanos: i128,
}

impl From<Fingerprint> for RecordedFingerprint {
    fn from(value: Fingerprint) -> Self {
        Self {
            volume: value.volume.value(),
            file: value.file.value(),
            size: value.size,
            modified_nanos: value.modified_nanos,
            changed_nanos: value.changed_nanos,
        }
    }
}

impl RecordedFingerprint {
    /// Reports whether a fingerprint read now is the one that was recorded.
    #[must_use]
    pub fn matches(self, now: Fingerprint) -> bool {
        self == Self::from(now)
    }

    /// Returns the volume the object was on when it was recorded.
    #[must_use]
    pub fn volume(self) -> VolumeId {
        VolumeId::new(self.volume)
    }
}

/// When a prune run marked an object unreferenced.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mark {
    /// The instant the object was marked, in nanoseconds since the epoch.
    pub marked_nanos: i128,
}

/// Writes a record where a reader can find it whole or not at all.
///
/// # Errors
///
/// Fails when the record cannot be written.
pub fn write<T: Serialize>(path: &Path, record: &T) -> Result<(), Error> {
    let rendered = serde_json::to_vec(record).map_err(|reason| {
        Error::new(
            ErrorKind::CacheCorrupt,
            format!("a cache record could not be written: {reason}"),
        )
    })?;
    std::fs::write(path, rendered).map_err(|reason| {
        Error::new(
            ErrorKind::CacheCorrupt,
            format!("{}: {reason}", path.display()),
        )
    })
}

/// Reads a record.
///
/// Returns nothing when the record is absent.
///
/// # Errors
///
/// Fails when the record is present and cannot be read or does not parse.
pub fn read<T: for<'a> Deserialize<'a>>(path: &Path) -> Result<Option<T>, Error> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(reason) if reason.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(reason) => {
            return Err(Error::new(
                ErrorKind::CacheCorrupt,
                format!("{}: {reason}", path.display()),
            ));
        }
    };
    serde_json::from_slice(&bytes).map(Some).map_err(|reason| {
        Error::new(
            ErrorKind::CacheCorrupt,
            format!(
                "run cache clear, because {} does not parse: {reason}",
                path.display()
            ),
        )
    })
}

/// Reads an owner record.
///
/// # Errors
///
/// Fails when the record is present and does not parse.
pub fn read_owner(path: &Path) -> Result<Option<OwnerToken>, Error> {
    read(path)
}
