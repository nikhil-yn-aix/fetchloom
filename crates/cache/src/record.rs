//! The records the cache writes beside its entries.

use std::path::Path;

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

    fetchloom_engine::atomic::replace(path, &rendered).map_err(|(site, reason)| match site {
        fetchloom_engine::atomic::Site::Scratch(beside) => {
            filesystem_failure(Surface::Cache, &beside, &reason)
        }
        fetchloom_engine::atomic::Site::Final => filesystem_failure(Surface::Cache, path, &reason),
    })?;
    for _ in 0..fetchloom_engine::atomic::OPERATIONS {
        work.touched_file();
    }
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
