//! The Platform seam implemented for Windows and Linux.

#[cfg(not(any(target_os = "linux", windows)))]
compile_error!(
    "Fetchloom builds for Linux and Windows. This target has no platform seam, so nothing below would compile with a message worth reading."
);

use std::fs::File;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};

use fetchloom_engine::capability::{
    Backing, CopyMechanism, ProcessorCapabilities, VolumeCapabilities,
};
use fetchloom_engine::degrade::{Degradation, DegradeQueue};
use fetchloom_engine::durability::DurabilityTier;
use fetchloom_engine::error::{Error, ErrorKind, Surface, filesystem_failure, lock_failure};
use fetchloom_engine::identity::{FileId, Fingerprint, VolumeId};
use fetchloom_engine::seam::platform::{Liveness, OwnerToken, Platform};
use fetchloom_engine::threads::ThreadBudget;

#[cfg(test)]
use fetchloom_faults as _;
#[cfg(test)]
use tempfile as _;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProcessState {
    Started(u64),
    Gone,
    Unreadable,
}

mod pathlen;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "linux")]
use crate::linux as imp;
#[cfg(windows)]
use crate::windows as imp;

pub(crate) fn probe_tag() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!(
        "{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

#[derive(Debug)]
pub struct PlatformLock {
    file: File,
}

impl Drop for PlatformLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

#[derive(Debug)]
pub struct NativePlatform {
    degradations: DegradeQueue,
    refused_cloning: std::sync::Mutex<std::collections::BTreeSet<std::ffi::OsString>>,
    work: std::sync::Arc<fetchloom_engine::work::WorkCounter>,
    probes_in: std::sync::Mutex<Option<PathBuf>>,
}

impl NativePlatform {
    #[must_use]
    pub fn new(work: std::sync::Arc<fetchloom_engine::work::WorkCounter>) -> Self {
        Self {
            degradations: DegradeQueue::new(),
            refused_cloning: std::sync::Mutex::new(std::collections::BTreeSet::new()),
            work,
            probes_in: std::sync::Mutex::new(None),
        }
    }

    fn probe_memo(&self, probe_directory: &Path) -> Option<PathBuf> {
        let directory = self
            .probes_in
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()?;
        let volume = imp::volume_id(probe_directory).ok()?;
        Some(directory.join(format!("volume-{:016x}", volume.value())))
    }

    fn recalled_path_length(&self, probe_directory: &Path) -> Option<u32> {
        let path = self.probe_memo(probe_directory)?;
        let boot = imp::boot_id()?;
        let held = std::fs::read_to_string(&path).ok()?;
        let (recorded, length) = held.trim_end().split_once(' ')?;
        if recorded != boot.as_str() {
            return None;
        }
        length.parse().ok()
    }

    fn remember_path_length(&self, probe_directory: &Path, length: u32) {
        let Some(path) = self.probe_memo(probe_directory) else {
            return;
        };
        let Some(boot) = imp::boot_id() else {
            return;
        };
        let rendered = format!("{} {length}", boot.as_str());
        if fetchloom_engine::atomic::replace(&path, rendered.as_bytes()).is_err() {
            return;
        }
        self.work.touched_file();
    }

    #[must_use]
    pub fn work(&self) -> &std::sync::Arc<fetchloom_engine::work::WorkCounter> {
        &self.work
    }

    fn volume_key(path: &Path) -> std::ffi::OsString {
        path.components()
            .next()
            .map_or_else(std::ffi::OsString::new, |first| {
                first.as_os_str().to_owned()
            })
    }

    fn volume_refused_cloning(&self, path: &Path) -> bool {
        self.refused_cloning
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(&Self::volume_key(path))
    }

    fn remember_refusal(&self, path: &Path) {
        self.refused_cloning
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(Self::volume_key(path));
    }

    fn copy_bytes(&self, from: &Path, to: &Path) -> Result<CopyMechanism, Error> {
        let copied = std::fs::copy(from, to)
            .map_err(|error| filesystem_failure(Surface::Destination, to, &error))?;
        self.work.read_bytes(copied);
        self.work.wrote_bytes(copied);
        self.work.touched_file();
        Ok(CopyMechanism::Copy)
    }

    fn rename(&self, from: &Path, to: &Path, tier: DurabilityTier) -> Result<(), Error> {
        imp::rename(from, to, tier)?;
        self.work.touched_file();
        Ok(())
    }

    fn flush_directory(&self, directory: &Path, tier: DurabilityTier) -> Result<(), Error> {
        imp::flush_directory(directory, tier)?;
        if tier == DurabilityTier::Strict {
            self.work.touched_file();
        }
        Ok(())
    }
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

    fn take_degradations(&self) -> Vec<Degradation> {
        self.degradations.take()
    }

    fn volume_id(&self, path: &Path) -> Result<VolumeId, Error> {
        imp::volume_id(path)
    }

    fn free_space(&self, path: &Path) -> Result<u64, Error> {
        imp::free_space(path)
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

    fn remember_probes_in(&self, directory: &Path) {
        *self
            .probes_in
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(directory.to_path_buf());
    }

    fn volume_capabilities(&self, probe_directory: &Path) -> Result<VolumeCapabilities, Error> {
        let remembered = self.recalled_path_length(probe_directory);
        let mut measured =
            imp::volume_capabilities(probe_directory, &self.degradations, remembered)?;
        match remembered {
            Some(length) => measured.max_path_length = length,
            None => self.remember_path_length(probe_directory, measured.max_path_length),
        }
        Ok(measured)
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
            .map_err(|reason| filesystem_failure(Surface::Destination, path, &reason))?;
        self.work.touched_file();
        Ok(made)
    }

    fn create_directory_exclusive(&self, path: &Path) -> Result<(), Error> {
        std::fs::create_dir(path)
            .map_err(|reason| filesystem_failure(Surface::Destination, path, &reason))?;
        self.work.touched_file();
        Ok(())
    }

    fn create_directories(&self, path: &Path) -> Result<(), Error> {
        if path.is_dir() {
            return Ok(());
        }
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            self.create_directories(parent)?;
        }
        match std::fs::create_dir(path) {
            Ok(()) => {
                self.work.touched_file();
                Ok(())
            }
            Err(reason) if reason.kind() == std::io::ErrorKind::AlreadyExists && path.is_dir() => {
                Ok(())
            }
            Err(reason) => Err(filesystem_failure(Surface::Destination, path, &reason)),
        }
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

    fn release_written(&self, file: &File, from: u64, length: u64) -> Result<bool, Error> {
        imp::release_written(file, from, length)
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
            .map_err(|reason| filesystem_failure(Surface::Destination, &aside, &reason))?;
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
        attempted(path, Sharing::Exclusive)
    }

    fn lock(&self, path: &Path) -> Result<Self::Lock, Error> {
        self.work.touched_file();
        waited_for(path, Sharing::Exclusive)
    }

    fn try_lock_shared(&self, path: &Path) -> Result<Option<Self::Lock>, Error> {
        self.work.touched_file();
        attempted(path, Sharing::Shared)
    }

    fn lock_shared(&self, path: &Path) -> Result<Self::Lock, Error> {
        self.work.touched_file();
        waited_for(path, Sharing::Shared)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Sharing {
    Exclusive,
    Shared,
}

const LOCK_ATTEMPTS: u32 = 16;

fn waited_for(path: &Path, sharing: Sharing) -> Result<PlatformLock, Error> {
    for _ in 0..LOCK_ATTEMPTS {
        let file = open_lock_file(path)?;
        match sharing {
            Sharing::Exclusive => file.lock().map_err(|why| lock_failure(path, &why))?,
            Sharing::Shared => file.lock_shared().map_err(|why| lock_failure(path, &why))?,
        }
        let held = PlatformLock { file };
        if imp::file_id_of(&held.file)? == imp::file_id(path)? {
            return Ok(held);
        }
    }
    Err(replaced_under_every_attempt(path))
}

fn attempted(path: &Path, sharing: Sharing) -> Result<Option<PlatformLock>, Error> {
    for _ in 0..LOCK_ATTEMPTS {
        let file = open_lock_file(path)?;
        let outcome = match sharing {
            Sharing::Exclusive => file.try_lock(),
            Sharing::Shared => file.try_lock_shared(),
        };
        match outcome {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => return Ok(None),
            Err(std::fs::TryLockError::Error(reason)) => {
                return Err(lock_failure(path, &reason));
            }
        }
        let held = PlatformLock { file };
        if imp::file_id_of(&held.file)? == imp::file_id(path)? {
            return Ok(Some(held));
        }
    }
    Err(replaced_under_every_attempt(path))
}

fn replaced_under_every_attempt(path: &Path) -> Error {
    Error::new(
        ErrorKind::CacheLocked,
        format!(
            "try again, because {} was replaced under every attempt to lock it",
            path.display()
        ),
    )
}

#[cfg(unix)]
const SHARED_LOCK_MODE: u32 = 0o666;

#[cfg(unix)]
fn open_lock_file(path: &Path) -> Result<File, Error> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let created = File::options()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(SHARED_LOCK_MODE)
        .open(path);
    match created {
        Ok(file) => {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(SHARED_LOCK_MODE))
                .map_err(|reason| filesystem_failure(Surface::Cache, path, &reason))?;
            Ok(file)
        }
        Err(reason) if reason.kind() == std::io::ErrorKind::AlreadyExists => File::options()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|why| filesystem_failure(Surface::Cache, path, &why)),
        Err(reason) => Err(filesystem_failure(Surface::Cache, path, &reason)),
    }
}

#[cfg(windows)]
fn open_lock_file(path: &Path) -> Result<File, Error> {
    File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|reason| filesystem_failure(Surface::Cache, path, &reason))
}

/// # Errors
/// `policy.credential_invalid` when a credential is stored for the host and
/// cannot be read, which on Linux includes a credentials file other users can
/// read. No stored credential is `None` rather than an error.
pub fn stored_token(host: &str, configuration: &Path) -> Result<Option<String>, Error> {
    #[cfg(windows)]
    {
        let _ = configuration;
        windows::stored_token(host)
    }
    #[cfg(unix)]
    {
        linux::stored_token(host, configuration)
    }
}
