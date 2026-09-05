//! The Platform seam: filesystem, publication, cloning, locking, detection.

use std::fs::File;
use std::num::NonZeroUsize;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::capability::{Backing, CopyMechanism, ProcessorCapabilities, VolumeCapabilities};
use crate::durability::DurabilityTier;
use crate::error::Error;
use crate::identity::{BootId, FileId, Fingerprint, MachineId, VolumeId};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerToken {
    pub machine: MachineId,
    pub boot: BootId,
    pub pid: u32,
    pub start: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Liveness {
    Live,
    Stale,
    OtherMachine,
    Undecidable { missing: String },
}

/// A filesystem failure is reported as the kind for the surface the path is on:
/// `cache.corrupt` under a cache, `destination.unrepresentable` under a
/// destination, and `resource.disk` on either when the volume is full. The
/// sections below name only what a method adds to that.
pub trait Platform: Send + Sync {
    type Lock: Send;

    /// # Errors
    /// When the volume cannot be identified.
    fn volume_id(&self, path: &Path) -> Result<VolumeId, Error>;

    /// # Errors
    /// When the volume cannot be asked how much room is left.
    fn free_space(&self, path: &Path) -> Result<u64, Error>;

    /// # Errors
    /// When the path cannot be stat'd.
    fn file_id(&self, path: &Path) -> Result<FileId, Error>;

    /// # Errors
    /// When the open file cannot be stat'd.
    fn file_id_of(&self, file: &File) -> Result<FileId, Error>;

    /// # Errors
    /// When the path's owner cannot be read.
    fn owns(&self, path: &Path) -> Result<bool, Error>;

    /// # Errors
    /// When size, identity or timestamps cannot be read.
    fn fingerprint(&self, path: &Path) -> Result<Fingerprint, Error>;

    /// # Errors
    /// When the volume cannot be asked what backs it. A volume that answers
    /// nothing is `Backing::Unknown` rather than an error.
    fn volume_backing(&self, path: &Path) -> Result<Backing, Error>;

    /// # Errors
    /// When the probe directory cannot be written. A capability the probe
    /// cannot decide is reported as unknown rather than as an error.
    fn volume_capabilities(&self, probe_directory: &Path) -> Result<VolumeCapabilities, Error>;

    fn processor_capabilities(&self, requested: Option<NonZeroUsize>) -> ProcessorCapabilities;

    /// # Errors
    /// When the file exists already or cannot be created.
    fn create_file_exclusive(&self, path: &Path) -> Result<File, Error>;

    /// # Errors
    /// When the directory exists already or cannot be created.
    fn create_directory_exclusive(&self, path: &Path) -> Result<(), Error>;

    /// # Errors
    /// When a directory on the path cannot be created. One that already exists
    /// is not an error.
    fn create_directories(&self, path: &Path) -> Result<(), Error>;

    /// # Errors
    /// When the length asked for does not fit or the volume refuses to reserve
    /// it.
    fn preallocate(&self, file: &File, length: u64) -> Result<(), Error>;

    /// # Errors
    /// When the flush is refused or finds the volume full.
    fn flush(&self, file: &File, tier: DurabilityTier) -> Result<(), Error>;

    /// # Errors
    /// When the range cannot be released. A platform with no way to release it
    /// answers `false` rather than failing.
    fn release_written(&self, file: &File, from: u64, length: u64) -> Result<bool, Error>;

    /// # Errors
    /// `destination.cross_volume` when the two paths are on different volumes,
    /// and a filesystem failure when the rename is refused.
    fn publish_file(&self, from: &Path, to: &Path, tier: DurabilityTier) -> Result<(), Error>;

    /// # Errors
    /// `destination.cross_volume` when staging and destination are on
    /// different volumes, and a filesystem failure when the publication cannot
    /// be completed.
    fn publish_directory(
        &self,
        staging: &Path,
        destination: &Path,
        tier: DurabilityTier,
    ) -> Result<(), Error>;

    /// # Errors
    /// `destination.foreign` when something is already at the destination, and
    /// a filesystem failure when the clone or the copy is refused. A volume
    /// that refuses cloning is copied from instead, which is a `CopyMechanism`
    /// answer rather than an error.
    fn clone_or_copy(&self, from: &Path, to: &Path) -> Result<CopyMechanism, Error>;

    /// # Errors
    /// `destination.unrepresentable` when the platform will not create the
    /// link, which includes a Windows without the privilege for one.
    fn create_symlink(&self, target: &[u8], link: &Path) -> Result<(), Error>;

    /// # Errors
    /// `cache.locked` when the machine, boot or process identity this token is
    /// built from cannot be read, because an owner that cannot be named cannot
    /// be compared against a stale one.
    fn owner_token(&self) -> Result<OwnerToken, Error>;

    fn liveness(&self, token: &OwnerToken) -> Liveness;

    /// # Errors
    /// `cache.locking_unsupported` when the volume cannot express an advisory
    /// lock, and `cache.locked` when the lock file is replaced under every
    /// attempt. A lock another process holds is `Ok(None)` rather than an
    /// error.
    fn try_lock(&self, path: &Path) -> Result<Option<Self::Lock>, Error>;

    /// # Errors
    /// The kinds `try_lock` gives, for a call that waits rather than answering
    /// `None`.
    fn lock(&self, path: &Path) -> Result<Self::Lock, Error>;

    /// # Errors
    /// The kinds `try_lock` gives, for a shared lock.
    fn try_lock_shared(&self, path: &Path) -> Result<Option<Self::Lock>, Error>;

    /// # Errors
    /// The kinds `lock` gives, for a shared lock.
    fn lock_shared(&self, path: &Path) -> Result<Self::Lock, Error>;
}
