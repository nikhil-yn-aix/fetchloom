//! The Platform seam implemented for Windows, macOS, and Linux.
//!
//! Everything that decides behavior lives here and is written once: the volume
//! comparison that refuses a cross-volume publish, the rename-aside sequence
//! that publishes a tree, the fallback from cloning to copying, the advisory
//! lock, and the liveness ladder. Each platform module supplies only the calls
//! that differ between platforms, so there is one behavior and three sets of
//! syscalls behind it.

use std::fs::File;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use fetchloom_engine::capability::{
    Backing, CopyMechanism, ProcessorCapabilities, VolumeCapabilities,
};
use fetchloom_engine::durability::DurabilityTier;
use fetchloom_engine::error::{Error, ErrorKind};
use fetchloom_engine::identity::{FileId, Fingerprint, OwnerId, VolumeId};
use fetchloom_engine::seam::platform::{Liveness, OwnerToken, Platform};
use fetchloom_engine::threads::ThreadBudget;

/// What a query about a recorded lock holder found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProcessState {
    /// The process exists and started at this instant.
    Started(u64),
    /// No process with that identifier exists.
    Gone,
    /// A process with that identifier exists and could not be inspected, which
    /// is never treated as the process being gone.
    Unreadable,
}

#[cfg(test)]
use tempfile as _;

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
use crate::unix as imp;
#[cfg(windows)]
use crate::windows as imp;

/// One fallback the platform performed instead of what was requested.
///
/// The Platform seam has no observer, so a fallback is recorded here and the
/// composition root turns each entry into the one `degrade` event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Degradation {
    /// What was requested.
    pub requested: String,
    /// What was used instead.
    pub used: String,
    /// Why the substitution happened.
    pub reason: String,
}

/// Where a platform records the fallbacks it performed.
#[derive(Debug, Default)]
pub struct DegradeQueue {
    entries: Mutex<Vec<Degradation>>,
}

impl DegradeQueue {
    /// Starts a queue that has recorded nothing.
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(Vec::new()),
        }
    }

    /// Records one fallback.
    pub fn record(
        &self,
        requested: impl Into<String>,
        used: impl Into<String>,
        reason: impl Into<String>,
    ) {
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Degradation {
                requested: requested.into(),
                used: used.into(),
                reason: reason.into(),
            });
    }

    /// Removes and returns everything recorded so far.
    #[must_use]
    pub fn take(&self) -> Vec<Degradation> {
        std::mem::take(&mut self.entries.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

/// A held advisory lock. Releasing it is dropping it.
#[derive(Debug)]
pub struct PlatformLock {
    file: File,
}

impl Drop for PlatformLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

/// The Platform seam as this machine implements it.
#[derive(Debug, Default)]
pub struct NativePlatform {
    degradations: DegradeQueue,
}

impl NativePlatform {
    /// Builds the platform for this machine.
    #[must_use]
    pub fn new() -> Self {
        Self {
            degradations: DegradeQueue::new(),
        }
    }

    /// Removes and returns every fallback performed since the last call.
    #[must_use]
    pub fn take_degradations(&self) -> Vec<Degradation> {
        self.degradations.take()
    }
}

fn failure(kind: ErrorKind, path: &Path, reason: &std::io::Error) -> Error {
    Error::new(kind, format!("{}: {reason}", path.display()))
}

fn cross_volume(from: &Path, to: &Path) -> Error {
    Error::new(
        ErrorKind::DestinationCrossVolume,
        format!(
            "put the staging directory on the same volume as {}, because {} is on another volume and a publish is a rename and never a copy",
            to.display(),
            from.display()
        ),
    )
}

fn containing_directory(path: &Path) -> PathBuf {
    path.parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

fn random_suffix() -> String {
    let token = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.subsec_nanos());
    format!("{:08x}{:08x}", std::process::id(), token)
}

impl Platform for NativePlatform {
    type Lock = PlatformLock;

    fn volume_id(&self, path: &Path) -> Result<VolumeId, Error> {
        imp::volume_id(path)
    }

    fn file_id(&self, path: &Path) -> Result<FileId, Error> {
        imp::file_id(path)
    }

    fn file_id_of(&self, file: &File) -> Result<FileId, Error> {
        imp::file_id_of(file)
    }

    fn owner(&self, path: &Path) -> Result<OwnerId, Error> {
        imp::owner(path)
    }

    fn current_owner(&self) -> Result<OwnerId, Error> {
        imp::current_owner()
    }

    fn fingerprint(&self, path: &Path) -> Result<Fingerprint, Error> {
        imp::fingerprint(path)
    }

    fn volume_backing(&self, path: &Path) -> Result<Backing, Error> {
        Ok(imp::volume_backing(path))
    }

    fn volume_capabilities(&self, probe_directory: &Path) -> Result<VolumeCapabilities, Error> {
        imp::volume_capabilities(probe_directory, &self.degradations)
    }

    fn processor_capabilities(&self, requested: Option<NonZeroUsize>) -> ProcessorCapabilities {
        ProcessorCapabilities {
            budget: ThreadBudget::resolve(imp::detected_parallelism(), requested),
            vector_level: imp::vector_level(),
            interop_acceleration: imp::interop_acceleration(&self.degradations),
        }
    }

    fn create_file_exclusive(&self, path: &Path) -> Result<File, Error> {
        File::create_new(path)
            .map_err(|reason| failure(ErrorKind::DestinationUnrepresentable, path, &reason))
    }

    fn create_directory_exclusive(&self, path: &Path) -> Result<(), Error> {
        std::fs::create_dir(path)
            .map_err(|reason| failure(ErrorKind::DestinationUnrepresentable, path, &reason))
    }

    fn preallocate(&self, file: &File, length: u64) -> Result<(), Error> {
        imp::preallocate(file, length, &self.degradations)
    }

    fn flush(&self, file: &File, tier: DurabilityTier) -> Result<(), Error> {
        imp::flush(file, tier, &self.degradations)
    }

    fn publish_file(&self, from: &Path, to: &Path, tier: DurabilityTier) -> Result<(), Error> {
        let source_volume = imp::volume_id(&containing_directory(from))?;
        let target_volume = imp::volume_id(&containing_directory(to))?;
        if source_volume != target_volume {
            return Err(cross_volume(from, to));
        }
        imp::rename(from, to, tier)?;
        imp::flush_directory(&containing_directory(to), tier)
    }

    fn publish_directory(
        &self,
        staging: &Path,
        destination: &Path,
        tier: DurabilityTier,
    ) -> Result<(), Error> {
        let source_volume = imp::volume_id(staging)?;
        let target_volume = imp::volume_id(&containing_directory(destination))?;
        if source_volume != target_volume {
            return Err(cross_volume(staging, destination));
        }

        if !destination.exists() {
            imp::rename(staging, destination, tier)?;
            return imp::flush_directory(&containing_directory(destination), tier);
        }

        let aside = destination.with_file_name(format!(
            "{}.fetchloom-old-{}",
            destination.file_name().map_or_else(
                || "destination".to_owned(),
                |name| name.to_string_lossy().into_owned()
            ),
            random_suffix()
        ));
        imp::rename(destination, &aside, tier)?;
        match imp::rename(staging, destination, tier) {
            Ok(()) => {}
            Err(error) => {
                let _ = imp::rename(&aside, destination, tier);
                return Err(error);
            }
        }
        std::fs::remove_dir_all(&aside)
            .map_err(|reason| failure(ErrorKind::DestinationForeign, &aside, &reason))?;
        imp::flush_directory(&containing_directory(destination), tier)
    }

    fn clone_or_copy(&self, from: &Path, to: &Path) -> Result<CopyMechanism, Error> {
        if to.exists() {
            return Err(Error::new(
                ErrorKind::DestinationForeign,
                format!("remove {} before placing bytes there", to.display()),
            ));
        }
        match imp::clone_file(from, to) {
            Ok(()) => Ok(CopyMechanism::Clone),
            Err(reason) => {
                self.degradations.record(
                    "a copy-on-write clone",
                    "the bytes were copied",
                    reason.next_action().to_owned(),
                );
                let _ = std::fs::remove_file(to);
                std::fs::copy(from, to)
                    .map_err(|error| failure(ErrorKind::DestinationUnrepresentable, to, &error))?;
                Ok(CopyMechanism::Copy)
            }
        }
    }

    fn create_symlink(&self, target: &[u8], link: &Path) -> Result<(), Error> {
        imp::create_symlink(target, link)
    }

    fn owner_token(&self) -> Result<OwnerToken, Error> {
        let machine = imp::machine_id().ok_or_else(|| {
            Error::new(
                ErrorKind::CacheLocked,
                "this machine does not report an identity, so a lock cannot record one",
            )
        })?;
        let boot = imp::boot_id().ok_or_else(|| {
            Error::new(
                ErrorKind::CacheLocked,
                "this machine does not report a boot identity, so a lock cannot record one",
            )
        })?;
        let pid = std::process::id();
        let ProcessState::Started(start) = imp::process_start(pid) else {
            return Err(Error::new(
                ErrorKind::CacheLocked,
                "this process does not report a start time, so a lock cannot record one",
            ));
        };
        Ok(OwnerToken {
            machine,
            boot,
            pid,
            start,
        })
    }

    fn liveness(&self, token: &OwnerToken) -> Liveness {
        if token.machine.as_str().is_empty() {
            return Liveness::Undecidable {
                missing: "machine".to_owned(),
            };
        }
        let Some(machine) = imp::machine_id() else {
            return Liveness::Undecidable {
                missing: "machine".to_owned(),
            };
        };
        if token.machine != machine {
            return Liveness::OtherMachine;
        }

        if token.boot.as_str().is_empty() {
            return Liveness::Undecidable {
                missing: "boot".to_owned(),
            };
        }
        let Some(boot) = imp::boot_id() else {
            return Liveness::Undecidable {
                missing: "boot".to_owned(),
            };
        };
        if token.boot != boot {
            return Liveness::Stale;
        }

        #[expect(
            clippy::match_same_arms,
            reason = "a holder that is gone and one whose identifier was recycled are different findings that happen to share an outcome"
        )]
        match imp::process_start(token.pid) {
            ProcessState::Gone => Liveness::Stale,
            ProcessState::Started(start) if start == token.start => Liveness::Live,
            ProcessState::Started(_) => Liveness::Stale,
            ProcessState::Unreadable => Liveness::Undecidable {
                missing: "process start time".to_owned(),
            },
        }
    }

    fn try_lock(&self, path: &Path) -> Result<Option<Self::Lock>, Error> {
        acquire(path, Sharing::Exclusive, Waiting::No)
    }

    fn lock(&self, path: &Path) -> Result<Self::Lock, Error> {
        acquire(path, Sharing::Exclusive, Waiting::Yes)?.ok_or_else(|| waited_without_it(path))
    }

    fn try_lock_shared(&self, path: &Path) -> Result<Option<Self::Lock>, Error> {
        acquire(path, Sharing::Shared, Waiting::No)
    }

    fn lock_shared(&self, path: &Path) -> Result<Self::Lock, Error> {
        acquire(path, Sharing::Shared, Waiting::Yes)?.ok_or_else(|| waited_without_it(path))
    }
}

/// Whether a lock admits other holders.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Sharing {
    /// One holder at a time.
    Exclusive,
    /// Any number of readers, and no writer.
    Shared,
}

/// Whether an acquisition waits for a holder to release.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Waiting {
    /// Return without the lock rather than wait.
    No,
    /// Wait for the holder to release it.
    Yes,
}

/// How many times an acquisition is retried when the name is replaced under it.
const LOCK_ATTEMPTS: u32 = 16;

/// Takes an advisory lock over the file a path currently names.
///
/// The identity of the locked handle is compared against the identity of the
/// path afterwards, because a lock file can be removed and recreated between
/// the open and the lock, and a lock over a name nothing refers to any more
/// would let a second holder take the same digest.
fn acquire(path: &Path, sharing: Sharing, waiting: Waiting) -> Result<Option<PlatformLock>, Error> {
    for _ in 0..LOCK_ATTEMPTS {
        let file = open_lock_file(path)?;
        let taken = match (sharing, waiting) {
            (Sharing::Exclusive, Waiting::No) => immediate(file.try_lock(), path)?,
            (Sharing::Shared, Waiting::No) => immediate(file.try_lock_shared(), path)?,
            (Sharing::Exclusive, Waiting::Yes) => {
                file.lock().map_err(|why| unsupported(path, &why))?;
                true
            }
            (Sharing::Shared, Waiting::Yes) => {
                file.lock_shared().map_err(|why| unsupported(path, &why))?;
                true
            }
        };
        if !taken {
            return Ok(None);
        }

        let held = PlatformLock { file };
        if imp::file_id_of(&held.file)? == imp::file_id(path)? {
            return Ok(Some(held));
        }
    }
    Err(Error::new(
        ErrorKind::CacheLocked,
        format!(
            "try again, because {} was replaced under every attempt to lock it",
            path.display()
        ),
    ))
}

/// Reports whether an immediate attempt took the lock.
fn immediate(outcome: Result<(), std::fs::TryLockError>, path: &Path) -> Result<bool, Error> {
    match outcome {
        Ok(()) => Ok(true),
        Err(std::fs::TryLockError::WouldBlock) => Ok(false),
        Err(std::fs::TryLockError::Error(reason)) => Err(unsupported(path, &reason)),
    }
}

fn waited_without_it(path: &Path) -> Error {
    Error::new(
        ErrorKind::CacheLocked,
        format!(
            "try again, because the wait for {} ended without the lock",
            path.display()
        ),
    )
}
fn open_lock_file(path: &Path) -> Result<File, Error> {
    File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|reason| failure(ErrorKind::CacheLocked, path, &reason))
}

fn unsupported(path: &Path, reason: &std::io::Error) -> Error {
    Error::new(
        ErrorKind::CacheLockingUnsupported,
        format!(
            "put the cache on a volume that supports advisory locking, because {} cannot express one: {reason}",
            path.display()
        ),
    )
}
