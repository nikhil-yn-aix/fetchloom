//! The Platform seam: filesystem, publication, cloning, locking, detection.

use std::fs::File;
use std::num::NonZeroUsize;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::capability::{Backing, CopyMechanism, ProcessorCapabilities, VolumeCapabilities};
use crate::durability::DurabilityTier;
use crate::error::Error;
use crate::identity::{BootId, FileId, Fingerprint, MachineId, VolumeId};

/// What a lock holder recorded about itself.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerToken {
    /// The machine the holder runs on.
    pub machine: MachineId,
    /// The boot of that machine the holder started in.
    pub boot: BootId,
    /// The holder's process identifier.
    pub pid: u32,
    /// When the holder's process started, in the platform's own units.
    pub start: u64,
}

/// What is known about whether a lock holder is still running.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Liveness {
    /// The holder is running.
    Live,
    /// The holder is a process that no longer exists.
    Stale,
    /// The holder is on another machine and cannot be inspected.
    OtherMachine,
    /// A field could not be read.
    Undecidable {
        /// The field that could not be read.
        missing: String,
    },
}

/// Filesystem behavior that differs by platform and by volume.
pub trait Platform: Send + Sync {
    /// What a held advisory lock is represented by. Releasing it is dropping
    /// it.
    type Lock: Send;

    /// Returns the identifier of the volume a path is on.
    ///
    /// # Errors
    ///
    /// Fails when the path cannot be opened or the platform refuses the query.
    fn volume_id(&self, path: &Path) -> Result<VolumeId, Error>;

    /// Returns the identifier of the file at a path within its volume.
    ///
    /// # Errors
    ///
    /// Fails when the path cannot be opened or the platform refuses the query.
    fn file_id(&self, path: &Path) -> Result<FileId, Error>;

    /// Returns the identifier of an open file within its volume.
    ///
    /// # Errors
    ///
    /// Fails when the platform refuses the query.
    fn file_id_of(&self, file: &File) -> Result<FileId, Error>;

    /// Reports whether a file belongs to the user this process runs as.
    ///
    /// # Errors
    ///
    /// Fails when the path cannot be opened, and when the platform reports no
    /// owner for it.
    fn owns(&self, path: &Path) -> Result<bool, Error>;

    /// Returns the tuple recording that the file at a path is probably
    /// unchanged.
    ///
    /// # Errors
    ///
    /// Fails when the path cannot be opened or the platform refuses the query.
    fn fingerprint(&self, path: &Path) -> Result<Fingerprint, Error>;

    /// Reports what the volume behind a path sits on.
    ///
    /// # Errors
    ///
    /// Fails when the path cannot be read or the platform refuses the query.
    fn volume_backing(&self, path: &Path) -> Result<Backing, Error>;

    /// Detects what the volume behind a directory can do.
    ///
    /// # Errors
    ///
    /// Fails when the directory cannot be written to.
    fn volume_capabilities(&self, probe_directory: &Path) -> Result<VolumeCapabilities, Error>;

    /// Detects what the processor can do and how many threads may be used.
    fn processor_capabilities(&self, requested: Option<NonZeroUsize>) -> ProcessorCapabilities;

    /// Creates a file, failing when the name already exists.
    ///
    /// # Errors
    ///
    /// Fails when the name exists, which is how a folding collision is found,
    /// and when the directory cannot be written to.
    fn create_file_exclusive(&self, path: &Path) -> Result<File, Error>;

    /// Creates a directory, failing when the name already exists.
    ///
    /// # Errors
    ///
    /// Fails when the name exists and when the parent cannot be written to.
    fn create_directory_exclusive(&self, path: &Path) -> Result<(), Error>;

    /// Creates a directory and every missing ancestor of it.
    ///
    /// # Errors
    ///
    /// Fails when a directory cannot be created and when a name on the path
    /// exists as something other than a directory.
    fn create_directories(&self, path: &Path) -> Result<(), Error>;

    /// Reserves the full length of a file before anything is written to it.
    ///
    /// # Errors
    ///
    /// Fails when the volume has no room. A volume that cannot reserve blocks
    /// sets the length instead and emits a degrade event.
    fn preallocate(&self, file: &File, length: u64) -> Result<(), Error>;

    /// Pushes a file's bytes as far as a durability tier requires.
    ///
    /// # Errors
    ///
    /// Fails when the platform reports the flush did not complete.
    fn flush(&self, file: &File, tier: DurabilityTier) -> Result<(), Error>;

    /// Asks the platform to release a file's written range from the page
    /// cache, reporting whether it did.
    ///
    /// # Errors
    ///
    /// Fails when the platform reports the request did not complete.
    fn release_written(&self, file: &File, from: u64, length: u64) -> Result<bool, Error>;

    /// Publishes one file by renaming it onto its final name.
    ///
    /// # Errors
    ///
    /// Fails when the two paths are on different volumes and when the rename or
    /// the directory flush does not complete.
    fn publish_file(&self, from: &Path, to: &Path, tier: DurabilityTier) -> Result<(), Error>;

    /// Publishes a staging tree onto a destination.
    ///
    /// # Errors
    ///
    /// Fails when staging and the destination are on different volumes and when
    /// either rename does not complete.
    fn publish_directory(
        &self,
        staging: &Path,
        destination: &Path,
        tier: DurabilityTier,
    ) -> Result<(), Error>;

    /// Places a file's bytes at another path, sharing blocks when the volume
    /// can and writing them again when it cannot.
    ///
    /// # Errors
    ///
    /// Fails when the target exists or cannot be written.
    fn clone_or_copy(&self, from: &Path, to: &Path) -> Result<CopyMechanism, Error>;

    /// Creates a symbolic link with the given target bytes.
    ///
    /// # Errors
    ///
    /// Fails when the name exists, when the volume has no symbolic links, and
    /// when this process is not permitted to create one.
    fn create_symlink(&self, target: &[u8], link: &Path) -> Result<(), Error>;

    /// Returns what this process would record about itself as a lock holder.
    ///
    /// # Errors
    ///
    /// Fails when the platform cannot report machine identity, boot identity,
    /// or this process's start time.
    fn owner_token(&self) -> Result<OwnerToken, Error>;

    /// Decides whether a recorded lock holder is still running.
    fn liveness(&self, token: &OwnerToken) -> Liveness;

    /// Takes an advisory lock without waiting.
    ///
    /// # Errors
    ///
    /// Fails when the volume cannot express advisory locking.
    fn try_lock(&self, path: &Path) -> Result<Option<Self::Lock>, Error>;

    /// Takes an advisory lock, waiting for another holder to release it.
    ///
    /// # Errors
    ///
    /// Fails when the volume cannot express advisory locking and when the wait
    /// ends without the lock.
    fn lock(&self, path: &Path) -> Result<Self::Lock, Error>;

    /// Takes an advisory lock that other readers may hold at the same time,
    /// without waiting.
    ///
    /// # Errors
    ///
    /// Fails when the volume cannot express advisory locking.
    fn try_lock_shared(&self, path: &Path) -> Result<Option<Self::Lock>, Error>;

    /// Takes an advisory lock that other readers may hold at the same time,
    /// waiting for a writer to release it.
    ///
    /// # Errors
    ///
    /// Fails when the volume cannot express advisory locking and when the wait
    /// ends without the lock.
    fn lock_shared(&self, path: &Path) -> Result<Self::Lock, Error>;
}
