//! A platform that fails the operations a schedule names.

use std::fs::File;
use std::num::NonZeroUsize;
use std::path::Path;

use fetchloom_engine::capability::{
    Backing, CopyMechanism, ProcessorCapabilities, VolumeCapabilities,
};
use fetchloom_engine::durability::DurabilityTier;
use fetchloom_engine::error::Error;
use fetchloom_engine::identity::{FileId, Fingerprint, VolumeId};
use fetchloom_engine::seam::platform::{Liveness, OwnerToken, Platform};

use crate::schedule::{Faults, Operation};

#[derive(Debug)]
pub struct FaultyPlatform<P> {
    inner: P,
    faults: Faults,
}

impl<P: Platform> FaultyPlatform<P> {
    #[must_use]
    pub fn new(inner: P) -> Self {
        Self {
            inner,
            faults: Faults::new(),
        }
    }

    #[must_use]
    pub fn faults(&self) -> &Faults {
        &self.faults
    }

    fn gate(&self, operation: Operation) -> Result<(), Error> {
        match self.faults.check(operation) {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

impl<P: Platform> Platform for FaultyPlatform<P> {
    type Lock = P::Lock;

    fn volume_id(&self, path: &Path) -> Result<VolumeId, Error> {
        self.gate(Operation::VolumeId)?;
        self.inner.volume_id(path)
    }

    fn free_space(&self, path: &Path) -> Result<u64, Error> {
        self.gate(Operation::FreeSpace)?;
        self.inner.free_space(path)
    }

    fn file_id(&self, path: &Path) -> Result<FileId, Error> {
        self.gate(Operation::FileId)?;
        self.inner.file_id(path)
    }

    fn fingerprint(&self, path: &Path) -> Result<Fingerprint, Error> {
        self.gate(Operation::Fingerprint)?;
        self.inner.fingerprint(path)
    }

    fn volume_backing(&self, path: &Path) -> Result<Backing, Error> {
        self.gate(Operation::VolumeBacking)?;
        self.inner.volume_backing(path)
    }

    fn volume_capabilities(&self, probe_directory: &Path) -> Result<VolumeCapabilities, Error> {
        self.gate(Operation::VolumeCapabilities)?;
        self.inner.volume_capabilities(probe_directory)
    }

    fn processor_capabilities(&self, requested: Option<NonZeroUsize>) -> ProcessorCapabilities {
        self.inner.processor_capabilities(requested)
    }

    fn create_file_exclusive(&self, path: &Path) -> Result<File, Error> {
        self.gate(Operation::CreateFileExclusive)?;
        self.inner.create_file_exclusive(path)
    }

    fn create_directory_exclusive(&self, path: &Path) -> Result<(), Error> {
        self.gate(Operation::CreateDirectoryExclusive)?;
        self.inner.create_directory_exclusive(path)
    }

    fn create_directories(&self, path: &Path) -> Result<(), Error> {
        self.gate(Operation::CreateDirectoryExclusive)?;
        self.inner.create_directories(path)
    }

    fn preallocate(&self, file: &File, length: u64) -> Result<(), Error> {
        self.gate(Operation::Preallocate)?;
        self.inner.preallocate(file, length)
    }

    fn flush(&self, file: &File, tier: DurabilityTier) -> Result<(), Error> {
        self.gate(Operation::Flush)?;
        self.inner.flush(file, tier)
    }

    fn release_written(&self, file: &File, from: u64, length: u64) -> Result<bool, Error> {
        self.gate(Operation::ReleaseWritten)?;
        self.inner.release_written(file, from, length)
    }

    fn publish_file(&self, from: &Path, to: &Path, tier: DurabilityTier) -> Result<(), Error> {
        self.gate(Operation::PublishFile)?;
        self.inner.publish_file(from, to, tier)
    }

    fn publish_directory(
        &self,
        staging: &Path,
        destination: &Path,
        tier: DurabilityTier,
    ) -> Result<(), Error> {
        self.gate(Operation::PublishDirectory)?;
        self.inner.publish_directory(staging, destination, tier)
    }

    fn clone_or_copy(&self, from: &Path, to: &Path) -> Result<CopyMechanism, Error> {
        self.gate(Operation::CloneOrCopy)?;
        self.inner.clone_or_copy(from, to)
    }

    fn create_symlink(&self, target: &[u8], link: &Path) -> Result<(), Error> {
        self.gate(Operation::CreateSymlink)?;
        self.inner.create_symlink(target, link)
    }

    fn owner_token(&self) -> Result<OwnerToken, Error> {
        self.gate(Operation::OwnerToken)?;
        self.inner.owner_token()
    }

    fn liveness(&self, token: &OwnerToken) -> Liveness {
        self.inner.liveness(token)
    }

    fn try_lock(&self, path: &Path) -> Result<Option<Self::Lock>, Error> {
        self.gate(Operation::TryLock)?;
        self.inner.try_lock(path)
    }

    fn lock(&self, path: &Path) -> Result<Self::Lock, Error> {
        self.gate(Operation::Lock)?;
        self.inner.lock(path)
    }

    fn file_id_of(&self, file: &File) -> Result<FileId, Error> {
        self.gate(Operation::FileIdOf)?;
        self.inner.file_id_of(file)
    }

    fn owns(&self, path: &Path) -> Result<bool, Error> {
        self.gate(Operation::Owns)?;
        self.inner.owns(path)
    }

    fn try_lock_shared(&self, path: &Path) -> Result<Option<Self::Lock>, Error> {
        self.gate(Operation::TryLockShared)?;
        self.inner.try_lock_shared(path)
    }

    fn lock_shared(&self, path: &Path) -> Result<Self::Lock, Error> {
        self.gate(Operation::LockShared)?;
        self.inner.lock_shared(path)
    }
}
