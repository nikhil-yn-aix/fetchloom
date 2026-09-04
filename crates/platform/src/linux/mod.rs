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

const MACHINE_FILE: &str = "/etc/machine-id";

const BOOT_FILE: &str = "/proc/sys/kernel/random/boot_id";

const START_TIME_FIELD: usize = 22;

fn from_errno(surface: Surface, path: &Path, reason: rustix::io::Errno) -> Error {
    filesystem_failure(surface, path, &std::io::Error::from(reason))
}

struct Identity {
    volume: u64,
    file: u128,
    size: u64,
    modified_nanos: i128,
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

const IDENTITY_FIELDS: rustix::fs::StatxFlags = rustix::fs::StatxFlags::INO
    .union(rustix::fs::StatxFlags::SIZE)
    .union(rustix::fs::StatxFlags::MTIME)
    .union(rustix::fs::StatxFlags::CTIME);

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

pub(crate) fn volume_id(path: &Path) -> Result<VolumeId, Error> {
    identity(path).map(|found| VolumeId::new(found.volume))
}

pub(crate) fn file_id(path: &Path) -> Result<FileId, Error> {
    identity(path).map(|found| FileId::new(found.file))
}

pub(crate) fn file_id_of(file: &File) -> Result<FileId, Error> {
    identity_of(file).map(|found| FileId::new(found.file))
}

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

pub(crate) fn volume_capabilities(
    probe_directory: &Path,
    degradations: &DegradeQueue,
) -> Result<VolumeCapabilities, Error> {
    probe::capabilities(probe_directory, degradations)
}

pub(crate) fn detected_parallelism() -> NonZeroUsize {
    std::thread::available_parallelism().unwrap_or(NonZeroUsize::MIN)
}

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

pub(crate) fn flush_directory(directory: &Path, tier: DurabilityTier) -> Result<(), Error> {
    if matches!(tier, DurabilityTier::Fast) {
        return Ok(());
    }
    let handle = File::open(directory)
        .map_err(|reason| filesystem_failure(Surface::Cache, directory, &reason))?;
    rustix::fs::fsync(&handle).map_err(|reason| from_errno(Surface::Cache, directory, reason))
}

pub(crate) fn rename(from: &Path, to: &Path, _tier: DurabilityTier) -> Result<(), Error> {
    rustix::fs::renameat(rustix::fs::CWD, from, rustix::fs::CWD, to)
        .map_err(|reason| from_errno(Surface::Destination, to, reason))
}

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

pub(crate) fn machine_id() -> Option<MachineId> {
    std::fs::read_to_string(MACHINE_FILE)
        .ok()
        .map(|text| MachineId::new(text.trim()))
}

pub(crate) fn boot_id() -> Option<BootId> {
    std::fs::read_to_string(BOOT_FILE)
        .ok()
        .map(|text| BootId::new(text.trim()))
}

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

pub(crate) fn owns(path: &Path) -> Result<bool, Error> {
    let found = rustix::fs::stat(path).map_err(|reason| {
        Error::new(
            ErrorKind::CacheCorrupt,
            format!("{}: {reason}", path.display()),
        )
    })?;
    Ok(found.st_uid == rustix::process::geteuid().as_raw())
}

pub(crate) fn volume_backing(path: &Path) -> Backing {
    probe::backing(path)
}

const READABLE_BY_OTHERS: u32 = 0o077;

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

pub(crate) fn free_space(path: &Path) -> Result<u64, Error> {
    let found = rustix::fs::statvfs(path).map_err(|reason| {
        filesystem_failure(Surface::Cache, path, &std::io::Error::from(reason))
    })?;
    Ok(found.f_bavail.saturating_mul(found.f_frsize))
}
