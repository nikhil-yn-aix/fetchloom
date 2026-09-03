//! The calls that are Linux's own.

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

/// Where the system records this machine's own identity.
const MACHINE_FILE: &str = "/etc/machine-id";

/// Where the kernel records the identity of this boot.
const BOOT_FILE: &str = "/proc/sys/kernel/random/boot_id";

/// The field of a process status line holding its start time.
const START_TIME_FIELD: usize = 22;

/// Turns a kernel failure into the one the seam decides, so that a path this
/// platform touches is judged by the same rule as one every other path is.
fn from_errno(surface: Surface, path: &Path, reason: rustix::io::Errno) -> Error {
    filesystem_failure(surface, path, &std::io::Error::from(reason))
}

/// What one path's identity and times are.
struct Identity {
    /// The volume the file is on.
    volume: u64,
    /// The file within that volume.
    file: u128,
    /// The length of the file in bytes.
    size: u64,
    /// The modification time in nanoseconds since the epoch.
    modified_nanos: i128,
    /// The change time in nanoseconds since the epoch.
    changed_nanos: i128,
}

fn identity_from(found: &rustix::fs::Statx) -> Identity {
    let device = (u64::from(found.stx_dev_major) << 32) | u64::from(found.stx_dev_minor);
    Identity {
        volume: device,
        file: (u128::from(device) << 64) | u128::from(found.stx_ino),
        size: found.stx_size,
        modified_nanos: i128::from(found.stx_mtime.tv_sec) * 1_000_000_000
            + i128::from(found.stx_mtime.tv_nsec),
        changed_nanos: i128::from(found.stx_ctime.tv_sec) * 1_000_000_000
            + i128::from(found.stx_ctime.tv_nsec),
    }
}

/// The fields every identity query asks for.
const IDENTITY_FIELDS: rustix::fs::StatxFlags = rustix::fs::StatxFlags::INO
    .union(rustix::fs::StatxFlags::SIZE)
    .union(rustix::fs::StatxFlags::MTIME)
    .union(rustix::fs::StatxFlags::CTIME);

/// Reads a path's identity, length, and times.
///
/// # Errors
///
/// Fails when the path cannot be read.
fn identity(path: &Path) -> Result<Identity, Error> {
    let found = rustix::fs::statx(
        rustix::fs::CWD,
        path,
        rustix::fs::AtFlags::empty(),
        IDENTITY_FIELDS,
    )
    .map_err(|reason| from_errno(Surface::Cache, path, reason))?;
    Ok(identity_from(&found))
}

/// Reads the identity of an open file.
///
/// # Errors
///
/// Fails when the platform refuses the query.
fn identity_of(file: &File) -> Result<Identity, Error> {
    let found = rustix::fs::statx(file, c"", rustix::fs::AtFlags::EMPTY_PATH, IDENTITY_FIELDS)
        .map_err(|reason| {
            Error::new(
                ErrorKind::CacheCorrupt,
                format!("an open file does not report an identity: {reason}"),
            )
        })?;
    Ok(identity_from(&found))
}

/// Returns the identifier of the volume a path is on.
///
/// # Errors
///
/// Fails when the path cannot be opened or the platform refuses the query.
pub(crate) fn volume_id(path: &Path) -> Result<VolumeId, Error> {
    identity(path).map(|found| VolumeId::new(found.volume))
}

/// Returns the identifier of the file at a path within its volume.
///
/// # Errors
///
/// Fails when the path cannot be opened or the platform refuses the query.
pub(crate) fn file_id(path: &Path) -> Result<FileId, Error> {
    identity(path).map(|found| FileId::new(found.file))
}

/// Returns the identifier of an open file within its volume.
///
/// # Errors
///
/// Fails when the platform refuses the query.
pub(crate) fn file_id_of(file: &File) -> Result<FileId, Error> {
    identity_of(file).map(|found| FileId::new(found.file))
}

/// Returns the tuple recording that a file is probably unchanged.
///
/// # Errors
///
/// Fails when the path cannot be opened or the platform refuses the query.
pub(crate) fn fingerprint(path: &Path) -> Result<Fingerprint, Error> {
    let found = identity(path)?;
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
pub(crate) fn detected_parallelism() -> NonZeroUsize {
    std::thread::available_parallelism().unwrap_or(NonZeroUsize::MIN)
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
    match rustix::fs::fallocate(file, rustix::fs::FallocateFlags::empty(), 0, length) {
        Ok(()) => Ok(()),
        Err(rustix::io::Errno::OPNOTSUPP) => {
            degradations.record(
                "reserving the whole length in blocks before writing",
                "the length was set without reserving blocks",
                "this filesystem does not support reserving blocks",
            );
            rustix::fs::ftruncate(file, length).map_err(|reason| {
                Error::new(
                    ErrorKind::ResourceDisk,
                    format!("free {length} bytes on the volume: {reason}"),
                )
            })
        }
        Err(reason) => Err(Error::new(
            ErrorKind::ResourceDisk,
            format!("free {length} bytes on the volume: {reason}"),
        )),
    }
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
    let outcome = match tier {
        DurabilityTier::Strict => rustix::fs::fsync(file),
        DurabilityTier::Normal => rustix::fs::fdatasync(file),
        DurabilityTier::Fast => return Ok(()),
    };
    outcome.map_err(|reason| {
        Error::new(
            fetchloom_engine::error::filesystem_kind(Surface::Cache, &std::io::Error::from(reason)),
            format!("{reason}"),
        )
    })
}

/// Releases a file's written range from the page cache.
///
/// # Errors
///
/// Fails when the platform reports the request did not complete.
pub(crate) fn release_written(file: &File, from: u64, length: u64) -> Result<bool, Error> {
    if length == 0 {
        return Ok(true);
    }
    rustix::fs::fadvise(
        file,
        from,
        std::num::NonZeroU64::new(length),
        rustix::fs::Advice::DontNeed,
    )
    .map(|()| true)
    .map_err(|reason| {
        Error::new(
            fetchloom_engine::error::filesystem_kind(Surface::Cache, &std::io::Error::from(reason)),
            format!("{reason}"),
        )
    })
}

/// Makes a directory's own entries durable.
///
/// # Errors
///
/// Fails when the directory cannot be opened or the flush does not complete.
pub(crate) fn flush_directory(directory: &Path, tier: DurabilityTier) -> Result<(), Error> {
    if matches!(tier, DurabilityTier::Fast) {
        return Ok(());
    }
    let handle = File::open(directory)
        .map_err(|reason| filesystem_failure(Surface::Cache, directory, &reason))?;
    rustix::fs::fsync(&handle).map_err(|reason| from_errno(Surface::Cache, directory, reason))
}

/// Renames one path onto another on the same volume.
///
/// # Errors
///
/// Fails when the rename does not complete.
pub(crate) fn rename(from: &Path, to: &Path, _tier: DurabilityTier) -> Result<(), Error> {
    rustix::fs::renameat(rustix::fs::CWD, from, rustix::fs::CWD, to)
        .map_err(|reason| from_errno(Surface::Destination, to, reason))
}

/// Shares the blocks of one file with another rather than writing them again.
///
/// # Errors
///
/// Fails on every filesystem that does not reference-count blocks.
pub(crate) fn clone_file(from: &Path, to: &Path) -> Result<(), Error> {
    let source = File::open(from)
        .map_err(|reason| filesystem_failure(Surface::Destination, from, &reason))?;
    let target = File::options()
        .write(true)
        .create_new(true)
        .open(to)
        .map_err(|reason| filesystem_failure(Surface::Destination, to, &reason))?;
    rustix::fs::ioctl_ficlone(&target, &source).map_err(|reason| {
        Error::new(
            ErrorKind::DestinationUnrepresentable,
            format!("this filesystem does not share blocks between files: {reason}"),
        )
    })
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
        .map_err(|reason| from_errno(Surface::Destination, link, reason))
}

/// Returns this machine's own identity.
pub(crate) fn machine_id() -> Option<MachineId> {
    std::fs::read_to_string(MACHINE_FILE)
        .ok()
        .map(|text| MachineId::new(text.trim()))
}

/// Returns the identity of this boot of this machine.
pub(crate) fn boot_id() -> Option<BootId> {
    std::fs::read_to_string(BOOT_FILE)
        .ok()
        .map(|text| BootId::new(text.trim()))
}

/// Returns when a process started.
pub(crate) fn process_start(pid: u32) -> ProcessState {
    let text = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(text) => text,
        Err(reason) if reason.kind() == std::io::ErrorKind::NotFound => return ProcessState::Gone,
        Err(_) => return ProcessState::Unreadable,
    };
    let Some(after_name) = text.rfind(')') else {
        return ProcessState::Unreadable;
    };
    let fields: Vec<&str> = text[after_name + 1..].split_whitespace().collect();
    match fields
        .get(START_TIME_FIELD - 3)
        .and_then(|f| f.parse().ok())
    {
        Some(start) => ProcessState::Started(start),
        None => ProcessState::Unreadable,
    }
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

/// The mode bits that let a user other than the owner read a file.
const READABLE_BY_OTHERS: u32 = 0o077;

/// Reads the token the credential file holds for a host.
///
/// # Errors
///
/// Fails with `policy.credential_invalid` when the file is present and cannot
/// be read, and when any user but its owner can read it.
pub fn stored_token(host: &str, configuration: &Path) -> Result<Option<String>, Error> {
    use std::os::unix::fs::PermissionsExt;

    let file = configuration.join("credentials");
    let Ok(found) = std::fs::metadata(&file) else {
        return Ok(None);
    };
    let mode = found.permissions().mode();
    if mode & READABLE_BY_OTHERS != 0 {
        return Err(Error::new(
            ErrorKind::PolicyCredentialInvalid,
            format!(
                "run chmod 600 on {}, because it is mode {:03o} and a credential another user can read is not one Fetchloom will send",
                file.display(),
                mode & 0o777
            ),
        ));
    }
    let text = std::fs::read_to_string(&file).map_err(|reason| {
        Error::new(
            ErrorKind::PolicyCredentialInvalid,
            format!("make {} readable: {reason}", file.display()),
        )
    })?;
    Ok(token_for(host, &text))
}

/// Returns the token an entry names for a host.
fn token_for(host: &str, text: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((named, value)) = line.split_once('=') else {
            continue;
        };
        if named.trim() != host {
            continue;
        }
        return Some(value.trim().trim_matches('"').to_owned());
    }
    None
}

/// Returns how many bytes the volume a path is on has free for this user.
///
/// # Errors
///
/// Fails when the path is not there or the platform refuses the query.
pub(crate) fn free_space(path: &Path) -> Result<u64, Error> {
    let found = rustix::fs::statvfs(path).map_err(|reason| {
        filesystem_failure(Surface::Cache, path, &std::io::Error::from(reason))
    })?;
    Ok(found.f_bavail.saturating_mul(found.f_frsize))
}
