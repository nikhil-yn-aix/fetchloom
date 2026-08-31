//! The calls that differ on Windows.

mod ffi;
mod probe;

use std::fs::File;
use std::num::NonZeroUsize;
use std::path::Path;

use fetchloom_engine::capability::{Backing, InteropAcceleration, VectorLevel, VolumeCapabilities};
use fetchloom_engine::durability::DurabilityTier;
use fetchloom_engine::error::{Error, ErrorKind, Surface, filesystem_failure};
use fetchloom_engine::identity::{BootId, FileId, Fingerprint, MachineId, VolumeId};

use fetchloom_engine::degrade::DegradeQueue;

use crate::ProcessState;

/// The interval the Windows epoch counts in, relative to the Unix epoch.
const WINDOWS_TO_UNIX_INTERVALS: i64 = 116_444_736_000_000_000;

/// The longest path Windows accepts through the extended prefix.
const MAX_PATH_LENGTH: u32 = 32_767;

/// The registry location of this machine's identity.
const MACHINE_KEY: &str = "SOFTWARE\\Microsoft\\Cryptography";

/// The registry location of the counter the session manager raises each boot.
const BOOT_KEY: &str =
    "SYSTEM\\CurrentControlSet\\Control\\Session Manager\\Memory Management\\PrefetchParameters";

fn nanos_from_windows(value: i64) -> i128 {
    i128::from(value - WINDOWS_TO_UNIX_INTERVALS) * 100
}

/// Returns the identifier of the volume a path is on.
///
/// # Errors
///
/// Fails when the path cannot be opened or the platform refuses the query.
pub(crate) fn volume_id(path: &Path) -> Result<VolumeId, Error> {
    let file = ffi::open_for_query(path)
        .map_err(|reason| filesystem_failure(Surface::Cache, path, &reason))?;
    let (serial, _) =
        ffi::id_info(&file).map_err(|reason| filesystem_failure(Surface::Cache, path, &reason))?;
    Ok(VolumeId::new(serial))
}

/// Returns the identifier of the file at a path within its volume.
///
/// # Errors
///
/// Fails when the path cannot be opened or the platform refuses the query.
pub(crate) fn file_id(path: &Path) -> Result<FileId, Error> {
    let file = ffi::open_for_query(path)
        .map_err(|reason| filesystem_failure(Surface::Cache, path, &reason))?;
    let (_, id) =
        ffi::id_info(&file).map_err(|reason| filesystem_failure(Surface::Cache, path, &reason))?;
    Ok(FileId::new(id))
}

/// Returns the tuple recording that a file is probably unchanged.
///
/// # Errors
///
/// Fails when the path cannot be opened or the platform refuses the query.
pub(crate) fn fingerprint(path: &Path) -> Result<Fingerprint, Error> {
    let file = ffi::open_for_query(path)
        .map_err(|reason| filesystem_failure(Surface::Cache, path, &reason))?;
    let (serial, id) =
        ffi::id_info(&file).map_err(|reason| filesystem_failure(Surface::Cache, path, &reason))?;
    let (written, changed) = ffi::basic_info(&file)
        .map_err(|reason| filesystem_failure(Surface::Cache, path, &reason))?;
    let size = file
        .metadata()
        .map_err(|reason| filesystem_failure(Surface::Cache, path, &reason))?
        .len();
    Ok(Fingerprint {
        volume: VolumeId::new(serial),
        file: FileId::new(id),
        size,
        modified_nanos: nanos_from_windows(written),
        changed_nanos: nanos_from_windows(changed),
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
pub(crate) fn detected_parallelism() -> NonZeroUsize {
    let standard = std::thread::available_parallelism().map_or(1, NonZeroUsize::get);
    let corrected = ffi::usable_processors().map_or(standard, |usable| usable.min(standard));
    NonZeroUsize::new(corrected.max(1)).unwrap_or(NonZeroUsize::MIN)
}

/// Returns the vector instruction level selected for the content digest.
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

/// Reports whether the interop digest can use a hardware instruction, or that
/// this target cannot detect one.
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
        degradations.record(
            "the processor's own instruction for the interop digest",
            "the portable implementation",
            "this target has no runtime detection for that instruction, so it is never claimed",
        );
        InteropAcceleration::Undetectable
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
    _degradations: &DegradeQueue,
) -> Result<(), Error> {
    ffi::preallocate(file, length).map_err(|reason| {
        Error::new(
            ErrorKind::ResourceDisk,
            format!("free {length} bytes on the volume: {reason}"),
        )
    })
}

/// Pushes a file's bytes as far as a durability tier requires.
///
/// # Errors
///
/// Fails when the platform reports the flush did not complete.
pub(crate) fn flush(
    file: &File,
    tier: DurabilityTier,
    _degradations: &DegradeQueue,
) -> Result<(), Error> {
    match tier {
        DurabilityTier::Strict | DurabilityTier::Normal => ffi::flush_file_buffers(file)
            .map_err(|reason| Error::new(ErrorKind::CacheCorrupt, format!("{reason}"))),
        DurabilityTier::Fast => Ok(()),
    }
}

/// Makes a directory's own entries durable.
///
/// # Errors
///
/// Never fails.
#[expect(
    clippy::unnecessary_wraps,
    reason = "the shape is the seam's, and the Unix side of it can fail"
)]
pub(crate) fn flush_directory(_directory: &Path, _tier: DurabilityTier) -> Result<(), Error> {
    Ok(())
}

/// Renames one path onto another on the same volume.
///
/// # Errors
///
/// Fails when the rename does not complete.
pub(crate) fn rename(from: &Path, to: &Path, tier: DurabilityTier) -> Result<(), Error> {
    let write_through = matches!(tier, DurabilityTier::Strict | DurabilityTier::Normal);
    ffi::rename(from, to, write_through)
        .map_err(|reason| filesystem_failure(Surface::Destination, to, &reason))
}

/// Shares the blocks of one file with another rather than writing them again.
///
/// # Errors
///
/// Fails on every volume that does not reference-count blocks, which the caller
/// turns into a copy and reports.
pub(crate) fn clone_file(from: &Path, to: &Path) -> Result<(), Error> {
    let source = File::open(from)
        .map_err(|reason| filesystem_failure(Surface::Destination, from, &reason))?;
    let length = source
        .metadata()
        .map_err(|reason| filesystem_failure(Surface::Destination, from, &reason))?
        .len();

    let information = ffi::open_for_query(from)
        .and_then(|handle| ffi::volume_information(&handle))
        .map_err(|reason| filesystem_failure(Surface::Destination, from, &reason))?;
    if information.flags
        & windows_sys::Win32::System::SystemServices::FILE_SUPPORTS_BLOCK_REFCOUNTING
        == 0
    {
        return Err(Error::new(
            ErrorKind::DestinationUnrepresentable,
            "this volume does not reference-count blocks, which only ReFS and a Dev Drive do"
                .to_owned(),
        ));
    }

    let target = File::options()
        .write(true)
        .create_new(true)
        .open(to)
        .map_err(|reason| filesystem_failure(Surface::Destination, to, &reason))?;
    ffi::preallocate(&target, length)
        .map_err(|reason| filesystem_failure(Surface::Destination, to, &reason))?;
    ffi::duplicate_extents(&source, &target, length)
        .map_err(|reason| filesystem_failure(Surface::Destination, to, &reason))
}

/// Creates a symbolic link with the given target bytes.
///
/// # Errors
///
/// Fails when the name exists, when the volume has no symbolic links, and when
/// this process is not permitted to create one.
pub(crate) fn create_symlink(target: &[u8], link: &Path) -> Result<(), Error> {
    let text = std::str::from_utf8(target).map_err(|_| {
        Error::new(
            ErrorKind::DestinationUnrepresentable,
            "a symbolic link target must be valid Unicode on this platform".to_owned(),
        )
    })?;
    ffi::create_symlink(text, link, false)
        .map_err(|reason| filesystem_failure(Surface::Destination, link, &reason))
}

/// Returns this machine's own identity.
pub(crate) fn machine_id() -> Option<MachineId> {
    ffi::registry_string(MACHINE_KEY, "MachineGuid").map(MachineId::new)
}

/// Returns the identity of this boot of this machine.
pub(crate) fn boot_id() -> Option<BootId> {
    ffi::registry_number(BOOT_KEY, "BootId").map(|value| BootId::new(value.to_string()))
}

/// Returns when a process started.
pub(crate) fn process_start(pid: u32) -> ProcessState {
    match ffi::process_start(pid) {
        ffi::ProcessQuery::Started(start) => ProcessState::Started(start),
        ffi::ProcessQuery::Gone => ProcessState::Gone,
        ffi::ProcessQuery::Unreadable => ProcessState::Unreadable,
    }
}

/// Returns the identifier of an open file within its volume.
///
/// # Errors
///
/// Fails when the platform refuses the query.
pub(crate) fn file_id_of(file: &File) -> Result<FileId, Error> {
    let (_, id) = ffi::id_info(file).map_err(|reason| {
        Error::new(
            ErrorKind::CacheCorrupt,
            format!("an open file does not report an identity: {reason}"),
        )
    })?;
    Ok(FileId::new(id))
}

/// Reports whether a file belongs to the user this process runs as.
///
/// # Errors
///
/// Fails when the path cannot be opened and when the platform reports no owner.
pub(crate) fn owns(path: &Path) -> Result<bool, Error> {
    let file = ffi::open_for_query(path)
        .map_err(|reason| filesystem_failure(Surface::Cache, path, &reason))?;
    let found = ffi::file_owner(&file)
        .map_err(|reason| filesystem_failure(Surface::Cache, path, &reason))?;
    let ours = ffi::process_owners().map_err(|reason| {
        Error::new(
            ErrorKind::CacheCorrupt,
            format!("this process does not report a user: {reason}"),
        )
    })?;
    Ok(ours.contains(&found))
}
/// Reports what the volume behind a path sits on.
pub(crate) fn volume_backing(path: &Path) -> Backing {
    if ffi::is_remote_drive(path) {
        Backing::Network
    } else {
        Backing::Local
    }
}
