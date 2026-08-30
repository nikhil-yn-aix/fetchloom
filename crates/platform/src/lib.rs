//! The Platform seam implemented for Windows and Linux.

use std::fs::File;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};

use fetchloom_engine::capability::{
    Backing, CopyMechanism, ProcessorCapabilities, VolumeCapabilities,
};
use fetchloom_engine::degrade::{Degradation, DegradeQueue};
use fetchloom_engine::durability::DurabilityTier;
use fetchloom_engine::error::{Error, ErrorKind};
use fetchloom_engine::identity::{FileId, Fingerprint, VolumeId};
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

#[cfg(target_os = "linux")]
mod linux;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "linux")]
use crate::linux as imp;
#[cfg(windows)]
use crate::windows as imp;

/// A name fragment no other probe uses at the same moment.
///
/// Returns this process's identifier and a count that never repeats within it.
pub(crate) fn probe_tag() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!(
        "{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
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
#[derive(Debug)]
pub struct NativePlatform {
    degradations: DegradeQueue,
    refused_cloning: std::sync::Mutex<std::collections::BTreeSet<std::ffi::OsString>>,
    work: std::sync::Arc<fetchloom_engine::work::WorkCounter>,
}

impl NativePlatform {
    /// Builds the platform for this machine.
    ///
    /// Takes where the run counts the file operations it performs.
    #[must_use]
    pub fn new(work: std::sync::Arc<fetchloom_engine::work::WorkCounter>) -> Self {
        Self {
            degradations: DegradeQueue::new(),
            refused_cloning: std::sync::Mutex::new(std::collections::BTreeSet::new()),
            work,
        }
    }

    /// Returns where this platform counts the file operations it performs.
    #[must_use]
    pub fn work(&self) -> &std::sync::Arc<fetchloom_engine::work::WorkCounter> {
        &self.work
    }

    /// Returns the volume a path is on, as far as its own text says.
    ///
    /// The key the clone answer for a volume is remembered under.
    fn volume_key(path: &Path) -> std::ffi::OsString {
        path.components()
            .next()
            .map_or_else(std::ffi::OsString::new, |first| {
                first.as_os_str().to_owned()
            })
    }

    /// Reports whether this volume has already refused to clone.
    fn volume_refused_cloning(&self, path: &Path) -> bool {
        self.refused_cloning
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(&Self::volume_key(path))
    }

    /// Records that this volume cannot clone.
    fn remember_refusal(&self, path: &Path) {
        self.refused_cloning
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(Self::volume_key(path));
    }

    /// Copies the bytes of one file into a path that does not exist.
    fn copy_bytes(&self, from: &Path, to: &Path) -> Result<CopyMechanism, Error> {
        std::fs::copy(from, to)
            .map_err(|error| failure(ErrorKind::DestinationUnrepresentable, to, &error))?;
        self.work.touched_file();
        Ok(CopyMechanism::Copy)
    }

    /// Renames a path onto another, counting the operation.
    fn rename(&self, from: &Path, to: &Path, tier: DurabilityTier) -> Result<(), Error> {
        imp::rename(from, to, tier)?;
        self.work.touched_file();
        Ok(())
    }

    /// Flushes a directory as far as a durability tier requires, counting the
    /// operation when the tier issues one.
    fn flush_directory(&self, directory: &Path, tier: DurabilityTier) -> Result<(), Error> {
        imp::flush_directory(directory, tier)?;
        if tier == DurabilityTier::Strict {
            self.work.touched_file();
        }
        Ok(())
    }

    /// Removes and returns every fallback performed since the last call.
    #[must_use]
    pub fn take_degradations(&self) -> Vec<Degradation> {
        self.degradations.take()
    }
}

fn failure(kind: ErrorKind, path: &Path, reason: &std::io::Error) -> Error {
    let kind = if reason.kind() == std::io::ErrorKind::StorageFull {
        ErrorKind::ResourceDisk
    } else {
        kind
    };
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
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    }
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

    fn owns(&self, path: &Path) -> Result<bool, Error> {
        imp::owns(path)
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
        let made = File::create_new(path)
            .map_err(|reason| failure(ErrorKind::DestinationUnrepresentable, path, &reason))?;
        self.work.touched_file();
        Ok(made)
    }

    fn create_directory_exclusive(&self, path: &Path) -> Result<(), Error> {
        std::fs::create_dir(path)
            .map_err(|reason| failure(ErrorKind::DestinationUnrepresentable, path, &reason))?;
        self.work.touched_file();
        Ok(())
    }

    fn preallocate(&self, file: &File, length: u64) -> Result<(), Error> {
        if length == 0 {
            return Ok(());
        }
        imp::preallocate(file, length, &self.degradations)
    }

    fn flush(&self, file: &File, tier: DurabilityTier) -> Result<(), Error> {
        imp::flush(file, tier, &self.degradations)?;
        self.work.touched_file();
        Ok(())
    }

    fn publish_file(&self, from: &Path, to: &Path, tier: DurabilityTier) -> Result<(), Error> {
        let source_volume = imp::volume_id(&containing_directory(from))?;
        let target_volume = imp::volume_id(&containing_directory(to))?;
        if source_volume != target_volume {
            return Err(cross_volume(from, to));
        }
        self.rename(from, to, tier)?;
        self.flush_directory(&containing_directory(to), tier)
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
            self.rename(staging, destination, tier)?;
            return self.flush_directory(&containing_directory(destination), tier);
        }

        let aside = destination.with_file_name(format!(
            "{}.fetchloom-old-{}",
            destination.file_name().map_or_else(
                || "destination".to_owned(),
                |name| name.to_string_lossy().into_owned()
            ),
            random_suffix()
        ));
        self.rename(destination, &aside, tier)?;
        match self.rename(staging, destination, tier) {
            Ok(()) => {}
            Err(error) => {
                let _ = self.rename(&aside, destination, tier);
                return Err(error);
            }
        }
        std::fs::remove_dir_all(&aside)
            .map_err(|reason| failure(ErrorKind::DestinationForeign, &aside, &reason))?;
        self.flush_directory(&containing_directory(destination), tier)
    }

    fn clone_or_copy(&self, from: &Path, to: &Path) -> Result<CopyMechanism, Error> {
        if to.exists() {
            return Err(Error::new(
                ErrorKind::DestinationForeign,
                format!("remove {} before placing bytes there", to.display()),
            ));
        }
        if self.volume_refused_cloning(from) {
            return self.copy_bytes(from, to);
        }
        match imp::clone_file(from, to) {
            Ok(()) => {
                self.work.touched_file();
                Ok(CopyMechanism::Clone)
            }
            Err(reason) => {
                self.remember_refusal(from);
                self.degradations.record(
                    "a copy-on-write clone",
                    "the bytes were copied",
                    reason.next_action().to_owned(),
                );
                let _ = std::fs::remove_file(to);
                self.copy_bytes(from, to)
            }
        }
    }

    fn create_symlink(&self, target: &[u8], link: &Path) -> Result<(), Error> {
        imp::create_symlink(target, link)?;
        self.work.touched_file();
        Ok(())
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
        self.work.touched_file();
        acquire(path, Sharing::Exclusive, Waiting::No)
    }

    fn lock(&self, path: &Path) -> Result<Self::Lock, Error> {
        self.work.touched_file();
        acquire(path, Sharing::Exclusive, Waiting::Yes)?.ok_or_else(|| waited_without_it(path))
    }

    fn try_lock_shared(&self, path: &Path) -> Result<Option<Self::Lock>, Error> {
        self.work.touched_file();
        acquire(path, Sharing::Shared, Waiting::No)
    }

    fn lock_shared(&self, path: &Path) -> Result<Self::Lock, Error> {
        self.work.touched_file();
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
/// path afterwards, and the acquisition is retried when they differ.
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
