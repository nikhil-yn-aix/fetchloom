//! The records the cache writes beside its entries.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use fetchloom_engine::error::{Error, ErrorKind, Surface, filesystem_failure};
use fetchloom_engine::identity::Fingerprint;
use fetchloom_engine::seam::platform::OwnerToken;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ObjectRecord {
    pub(crate) interop: fetchloom_engine::digest::InteropDigest,
    pub(crate) volume: u64,
    pub(crate) file: u128,
    pub(crate) size: u64,
    modified_nanos: i128,
    changed_nanos: i128,
}

impl ObjectRecord {
    #[must_use]
    pub(crate) fn new(
        fingerprint: Fingerprint,
        interop: fetchloom_engine::digest::InteropDigest,
    ) -> Self {
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
    pub(crate) fn matches(self, now: Fingerprint) -> bool {
        let observed = Self::new(now, self.interop);
        self == observed
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mark {
    pub(crate) marked_nanos: i128,
}

/// # Errors
/// `cache.corrupt` when the record cannot be rendered, written beside its
/// destination, or renamed onto it, and `resource.disk` when the volume is
/// full.
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

/// # Errors
/// `cache.corrupt` when a record exists and cannot be read or does not parse.
/// No record is `None` rather than an error.
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

pub(crate) fn read_owner(path: &Path) -> Result<Option<OwnerToken>, Error> {
    read(path)
}

static WRITES: AtomicU64 = AtomicU64::new(0);
