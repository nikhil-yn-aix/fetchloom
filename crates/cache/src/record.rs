//! The records the cache writes beside its entries.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use fetchloom_engine::error::{Error, ErrorKind, Surface, filesystem_failure};
use fetchloom_engine::identity::{Fingerprint, VolumeId};
use fetchloom_engine::seam::platform::OwnerToken;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObjectRecord {
    pub interop: fetchloom_engine::digest::InteropDigest,
    pub volume: u64,
    pub file: u128,
    pub size: u64,
    pub modified_nanos: i128,
    pub changed_nanos: i128,
}

impl ObjectRecord {
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

    #[must_use]
    pub fn matches(self, now: Fingerprint) -> bool {
        let observed = Self::new(now, self.interop);
        self == observed
    }

    #[must_use]
    pub fn volume(self) -> VolumeId {
        VolumeId::new(self.volume)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mark {
    pub marked_nanos: i128,
}

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
    beside.push(format!(
        ".{}.{}.writing",
        std::process::id(),
        WRITES.fetch_add(1, Ordering::Relaxed)
    ));
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

pub fn read_owner(path: &Path) -> Result<Option<OwnerToken>, Error> {
    read(path)
}

static WRITES: AtomicU64 = AtomicU64::new(0);
