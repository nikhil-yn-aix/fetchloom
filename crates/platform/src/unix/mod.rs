//! The calls that differ on Unix, split again where Linux and macOS differ.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_vendor = "apple")]
mod macos;

#[cfg(target_os = "linux")]
use crate::unix::linux as host;
#[cfg(target_vendor = "apple")]
use crate::unix::macos as host;

mod probe;

#[cfg(target_os = "linux")]
use libc as _;

use std::fs::File;
use std::num::NonZeroUsize;
use std::path::Path;

use fetchloom_engine::capability::{Backing, InteropAcceleration, VectorLevel, VolumeCapabilities};
use fetchloom_engine::durability::DurabilityTier;
use fetchloom_engine::error::{Error, ErrorKind};
use fetchloom_engine::identity::{BootId, FileId, Fingerprint, MachineId, VolumeId};

use fetchloom_engine::degrade::DegradeQueue;

use crate::ProcessState;

fn failure(kind: ErrorKind, path: &Path, reason: &std::io::Error) -> Error {
    Error::new(kind, format!("{}: {reason}", path.display()))
}

fn from_errno(kind: ErrorKind, path: &Path, reason: rustix::io::Errno) -> Error {
    Error::new(kind, format!("{}: {reason}", path.display()))
}

/// Returns the identifier of the volume a path is on.
///
/// # Errors
///
/// Fails when the path cannot be opened or the platform refuses the query.
pub(crate) fn volume_id(path: &Path) -> Result<VolumeId, Error> {
    host::identity(path).map(|found| VolumeId::new(found.volume))
}

/// Returns the identifier of the file at a path within its volume.
///
/// # Errors
///
/// Fails when the path cannot be opened or the platform refuses the query.
pub(crate) fn file_id(path: &Path) -> Result<FileId, Error> {
    host::identity(path).map(|found| FileId::new(found.file))
}

/// Returns the tuple recording that a file is probably unchanged.
///
/// # Errors
///
/// Fails when the path cannot be opened or the platform refuses the query.
pub(crate) fn fingerprint(path: &Path) -> Result<Fingerprint, Error> {
    let found = host::identity(path)?;
    Ok(Fingerprint {
        volume: VolumeId::new(found.volume),
        file: FileId::new(found.file),
        size: found.size,
        modified_nanos: found.modified_nanos,
        changed_nanos: found.changed_nanos,
    })
}

/// Detects what the volume behind a directory can do.
///
/// # Errors
///
/// Fails when the directory cannot be written to.
pub(crate) fn volume_capabilities(
    probe_directory: &Path,
    degradations: &DegradeQueue,
) -> Result<VolumeCapabilities, Error> {
    probe::capabilities(probe_directory, degradations)
}

/// Returns how many threads this process may actually run on.
///
/// The standard library already reads control group limits and the affinity
/// mask on Linux and the online processor count on macOS, so nothing is added.
pub(crate) fn detected_parallelism() -> NonZeroUsize {
    std::thread::available_parallelism().unwrap_or(NonZeroUsize::MIN)
}

/// Returns the vector instruction level selected for the content digest.
///
/// This is a vector width and never a dedicated instruction, because the
/// content digest algorithm has none.
pub(crate) fn vector_level() -> VectorLevel {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        if std::arch::is_x86_feature_detected!("avx512f") {
            VectorLevel::Avx512
        } else if std::arch::is_x86_feature_detected!("avx2") {
            VectorLevel::Avx2
        } else if std::arch::is_x86_feature_detected!("sse4.1") {
            VectorLevel::Sse41
        } else {
            VectorLevel::Portable
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        VectorLevel::Neon
    }
    #[cfg(not(any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64")))]
    {
        VectorLevel::Portable
    }
}

/// Reports whether the interop digest can use a hardware instruction.
pub(crate) fn interop_acceleration(degradations: &DegradeQueue) -> InteropAcceleration {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        let _ = degradations;
        if std::arch::is_x86_feature_detected!("sha") {
            InteropAcceleration::Usable
        } else {
            InteropAcceleration::Absent
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        let _ = degradations;
        if std::arch::is_aarch64_feature_detected!("sha2") {
            InteropAcceleration::Usable
        } else {
            InteropAcceleration::Absent
        }
    }
    #[cfg(not(any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64")))]
    {
        let _ = degradations;
        InteropAcceleration::Absent
    }
}

/// Reserves the full length of a file before anything is written to it.
///
/// # Errors
///
/// Fails when the volume has no room.
pub(crate) fn preallocate(
    file: &File,
    length: u64,
    degradations: &DegradeQueue,
) -> Result<(), Error> {
    host::preallocate(file, length, degradations)
}

/// Pushes a file's bytes as far as a durability tier requires.
///
/// # Errors
///
/// Fails when the platform reports the flush did not complete.
pub(crate) fn flush(
    file: &File,
    tier: DurabilityTier,
    degradations: &DegradeQueue,
) -> Result<(), Error> {
    host::flush(file, tier, degradations)
}

/// Makes a directory's own entries durable.
///
/// Opens the directory and flushes it, which is what makes a rename survive a
/// power failure on Unix.
///
/// # Errors
///
/// Fails when the directory cannot be opened or the flush does not complete.
pub(crate) fn flush_directory(directory: &Path, tier: DurabilityTier) -> Result<(), Error> {
    if matches!(tier, DurabilityTier::Fast) {
        return Ok(());
    }
    let handle = File::open(directory)
        .map_err(|reason| failure(ErrorKind::CacheCorrupt, directory, &reason))?;
    rustix::fs::fsync(&handle)
        .map_err(|reason| from_errno(ErrorKind::CacheCorrupt, directory, reason))
}

/// Renames one path onto another on the same volume.
///
/// # Errors
///
/// Fails when the rename does not complete.
pub(crate) fn rename(from: &Path, to: &Path, _tier: DurabilityTier) -> Result<(), Error> {
    rustix::fs::renameat(rustix::fs::CWD, from, rustix::fs::CWD, to)
        .map_err(|reason| from_errno(ErrorKind::DestinationUnrepresentable, to, reason))
}

/// Shares the blocks of one file with another rather than writing them again.
///
/// # Errors
///
/// Fails on every volume that does not share blocks, which the caller turns
/// into a copy and reports.
pub(crate) fn clone_file(from: &Path, to: &Path) -> Result<(), Error> {
    host::clone_file(from, to)
}

/// Creates a symbolic link with the given target bytes.
///
/// # Errors
///
/// Fails when the name exists and when the volume has no symbolic links.
pub(crate) fn create_symlink(target: &[u8], link: &Path) -> Result<(), Error> {
    let text = std::str::from_utf8(target).map_err(|_| {
        Error::new(
            ErrorKind::DestinationUnrepresentable,
            "a symbolic link target must be valid Unicode".to_owned(),
        )
    })?;
    rustix::fs::symlinkat(text, rustix::fs::CWD, link)
        .map_err(|reason| from_errno(ErrorKind::DestinationUnrepresentable, link, reason))
}

/// Returns this machine's own identity.
pub(crate) fn machine_id() -> Option<MachineId> {
    host::machine_id()
}

/// Returns the identity of this boot of this machine.
pub(crate) fn boot_id() -> Option<BootId> {
    host::boot_id()
}

/// Returns when a process started.
///
/// Reports that no such process exists only when the platform says so, never
/// when one exists and cannot be inspected.
pub(crate) fn process_start(pid: u32) -> ProcessState {
    host::process_start(pid)
}

/// What one path's identity and times are.
pub(crate) struct Identity {
    /// The volume the file is on.
    pub(crate) volume: u64,
    /// The file within that volume.
    pub(crate) file: u128,
    /// The length of the file in bytes.
    pub(crate) size: u64,
    /// The modification time in nanoseconds since the epoch.
    pub(crate) modified_nanos: i128,
    /// The change time in nanoseconds since the epoch.
    pub(crate) changed_nanos: i128,
}

/// Returns the identifier of an open file within its volume.
///
/// # Errors
///
/// Fails when the platform refuses the query.
pub(crate) fn file_id_of(file: &File) -> Result<FileId, Error> {
    host::identity_of(file).map(|found| FileId::new(found.file))
}

/// Reports whether a file belongs to the user this process runs as.
///
/// # Errors
///
/// Fails when the path cannot be read.
pub(crate) fn owns(path: &Path) -> Result<bool, Error> {
    let found = rustix::fs::stat(path).map_err(|reason| {
        Error::new(
            ErrorKind::CacheCorrupt,
            format!("{}: {reason}", path.display()),
        )
    })?;
    Ok(found.st_uid == rustix::process::geteuid().as_raw())
}

/// Reports what the volume behind a path sits on.
pub(crate) fn volume_backing(path: &Path) -> Backing {
    probe::backing(path)
}
