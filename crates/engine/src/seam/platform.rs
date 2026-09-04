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

pub trait Platform: Send + Sync {
    type Lock: Send;

    fn volume_id(&self, path: &Path) -> Result<VolumeId, Error>;

    fn free_space(&self, path: &Path) -> Result<u64, Error>;

    fn file_id(&self, path: &Path) -> Result<FileId, Error>;

    fn file_id_of(&self, file: &File) -> Result<FileId, Error>;

    fn owns(&self, path: &Path) -> Result<bool, Error>;

    fn fingerprint(&self, path: &Path) -> Result<Fingerprint, Error>;

    fn volume_backing(&self, path: &Path) -> Result<Backing, Error>;

    fn volume_capabilities(&self, probe_directory: &Path) -> Result<VolumeCapabilities, Error>;

    fn processor_capabilities(&self, requested: Option<NonZeroUsize>) -> ProcessorCapabilities;

    fn create_file_exclusive(&self, path: &Path) -> Result<File, Error>;

    fn create_directory_exclusive(&self, path: &Path) -> Result<(), Error>;

    fn create_directories(&self, path: &Path) -> Result<(), Error>;

    fn preallocate(&self, file: &File, length: u64) -> Result<(), Error>;

    fn flush(&self, file: &File, tier: DurabilityTier) -> Result<(), Error>;

    fn release_written(&self, file: &File, from: u64, length: u64) -> Result<bool, Error>;

    fn publish_file(&self, from: &Path, to: &Path, tier: DurabilityTier) -> Result<(), Error>;

    fn publish_directory(
        &self,
        staging: &Path,
        destination: &Path,
        tier: DurabilityTier,
    ) -> Result<(), Error>;

    fn clone_or_copy(&self, from: &Path, to: &Path) -> Result<CopyMechanism, Error>;

    fn create_symlink(&self, target: &[u8], link: &Path) -> Result<(), Error>;

    fn owner_token(&self) -> Result<OwnerToken, Error>;

    fn liveness(&self, token: &OwnerToken) -> Liveness;

    fn try_lock(&self, path: &Path) -> Result<Option<Self::Lock>, Error>;

    fn lock(&self, path: &Path) -> Result<Self::Lock, Error>;

    fn try_lock_shared(&self, path: &Path) -> Result<Option<Self::Lock>, Error>;

    fn lock_shared(&self, path: &Path) -> Result<Self::Lock, Error>;
}
