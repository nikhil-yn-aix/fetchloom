//! The records the cache writes beside its entries.

use std::path::Path;

use fetchloom_engine::error::{Error, ErrorKind, Surface, filesystem_failure};
use fetchloom_engine::identity::{Fingerprint, VolumeId};
use fetchloom_engine::seam::platform::OwnerToken;

use serde::{Deserialize, Serialize};

/// Everything the cache knows about a published object that is not in its
/// bytes, in one record.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObjectRecord {
    /// The interop digest of the object's bytes, taken as they were written.
    pub interop: fetchloom_engine::digest::InteropDigest,
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

impl ObjectRecord {
    /// Builds the record of an object from what was observed as it was
    /// published.
    #[must_use]
    pub fn new(fingerprint: Fingerprint, interop: fetchloom_engine::digest::InteropDigest) -> Self {
        Self {
            interop,
            volume: fingerprint.volume.value(),
            file: fingerprint.file.value(),
            size: fingerprint.size,
            modified_nanos: fingerprint.modified_nanos,
            changed_nanos: fingerprint.changed_nanos,
        }
    }

    /// Reports whether a fingerprint read now is the one that was recorded.
    #[must_use]
    pub fn matches(self, now: Fingerprint) -> bool {
        let observed = Self::new(now, self.interop);
        self == observed
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

/// Writes a record where a reader finds it whole or not at all.
///
/// Writes beside the record and renames onto it. Takes where the run counts
/// file operations.
///
/// # Errors
///
/// Fails when the record cannot be written.
pub fn write<T: Serialize>(
    path: &Path,
    record: &T,
    work: &fetchloom_engine::work::WorkCounter,
) -> Result<(), Error> {
    let rendered = serde_json::to_vec(record).map_err(|reason| {
        Error::new(
            ErrorKind::CacheCorrupt,
            format!("a cache record could not be written: {reason}"),
        )
    })?;

    let mut beside = path.as_os_str().to_owned();
    beside.push(format!(".{}.writing", std::process::id()));
    let beside = std::path::PathBuf::from(beside);

    std::fs::write(&beside, rendered)
        .map_err(|reason| filesystem_failure(Surface::Cache, &beside, &reason))?;
    work.touched_file();
    std::fs::rename(&beside, path).map_err(|reason| {
        let _ = std::fs::remove_file(&beside);
        filesystem_failure(Surface::Cache, path, &reason)
    })?;
    work.touched_file();
    Ok(())
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
            return Err(filesystem_failure(Surface::Cache, path, &reason));
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
