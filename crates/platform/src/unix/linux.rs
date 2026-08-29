//! The calls that are Linux's own.

use std::fs::File;
use std::path::Path;

use fetchloom_engine::durability::DurabilityTier;
use fetchloom_engine::error::{Error, ErrorKind};
use fetchloom_engine::identity::{BootId, MachineId};

use super::Identity;
use crate::{DegradeQueue, ProcessState};

/// Where the system records this machine's own identity.
const MACHINE_FILE: &str = "/etc/machine-id";

/// Where the kernel records the identity of this boot.
const BOOT_FILE: &str = "/proc/sys/kernel/random/boot_id";

/// The field of a process status line holding its start time.
const START_TIME_FIELD: usize = 22;

fn failure(kind: ErrorKind, path: &Path, reason: rustix::io::Errno) -> Error {
    Error::new(kind, format!("{}: {reason}", path.display()))
}

/// Reads a path's identity, length, and times.
///
/// # Errors
///
/// Fails when the path cannot be read.
pub(super) fn identity(path: &Path) -> Result<Identity, Error> {
    let found = rustix::fs::statx(
        rustix::fs::CWD,
        path,
        rustix::fs::AtFlags::empty(),
        rustix::fs::StatxFlags::INO
            | rustix::fs::StatxFlags::SIZE
            | rustix::fs::StatxFlags::MTIME
            | rustix::fs::StatxFlags::CTIME,
    )
    .map_err(|reason| failure(ErrorKind::CacheCorrupt, path, reason))?;

    let device = (u64::from(found.stx_dev_major) << 32) | u64::from(found.stx_dev_minor);
    Ok(Identity {
        volume: device,
        file: (u128::from(device) << 64) | u128::from(found.stx_ino),
        size: found.stx_size,
        modified_nanos: i128::from(found.stx_mtime.tv_sec) * 1_000_000_000
            + i128::from(found.stx_mtime.tv_nsec),
        changed_nanos: i128::from(found.stx_ctime.tv_sec) * 1_000_000_000
            + i128::from(found.stx_ctime.tv_nsec),
    })
}

/// Reserves the full length of a file.
///
/// Reserves blocks where the filesystem can, and sets the length alone where it
/// cannot, reporting that as a degradation rather than performing it silently.
///
/// # Errors
///
/// Fails when the volume has no room.
pub(super) fn preallocate(
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
pub(super) fn flush(
    file: &File,
    tier: DurabilityTier,
    _degradations: &DegradeQueue,
) -> Result<(), Error> {
    let outcome = match tier {
        DurabilityTier::Strict => rustix::fs::fsync(file),
        DurabilityTier::Normal => rustix::fs::fdatasync(file),
        DurabilityTier::Fast => return Ok(()),
    };
    outcome.map_err(|reason| Error::new(ErrorKind::CacheCorrupt, format!("{reason}")))
}

/// Shares the blocks of one file with another.
///
/// # Errors
///
/// Fails on every filesystem that does not reference-count blocks.
pub(super) fn clone_file(from: &Path, to: &Path) -> Result<(), Error> {
    let source = File::open(from).map_err(|reason| {
        Error::new(
            ErrorKind::DestinationUnrepresentable,
            format!("{}: {reason}", from.display()),
        )
    })?;
    let target = File::options()
        .write(true)
        .create_new(true)
        .open(to)
        .map_err(|reason| {
            Error::new(
                ErrorKind::DestinationUnrepresentable,
                format!("{}: {reason}", to.display()),
            )
        })?;
    rustix::fs::ioctl_ficlone(&target, &source).map_err(|reason| {
        Error::new(
            ErrorKind::DestinationUnrepresentable,
            format!("this filesystem does not share blocks between files: {reason}"),
        )
    })
}

/// Returns this machine's own identity.
pub(super) fn machine_id() -> Option<MachineId> {
    std::fs::read_to_string(MACHINE_FILE)
        .ok()
        .map(|text| MachineId::new(text.trim()))
}

/// Returns the identity of this boot of this machine.
pub(super) fn boot_id() -> Option<BootId> {
    std::fs::read_to_string(BOOT_FILE)
        .ok()
        .map(|text| BootId::new(text.trim()))
}

/// Returns when a process started.
///
/// Reads the start time from the process's own status line. The name field of
/// that line can itself hold spaces and brackets, so the fields are counted
/// from the last closing bracket rather than from the start of the line.
pub(super) fn process_start(pid: u32) -> ProcessState {
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

/// Reads the identity of an open file.
///
/// Takes a handle rather than a name, so the answer is about the file that was
/// opened. Fails when the platform refuses the query.
pub(super) fn identity_of(file: &File) -> Result<Identity, Error> {
    let found = rustix::fs::statx(
        file,
        c"",
        rustix::fs::AtFlags::EMPTY_PATH,
        rustix::fs::StatxFlags::INO
            | rustix::fs::StatxFlags::SIZE
            | rustix::fs::StatxFlags::MTIME
            | rustix::fs::StatxFlags::CTIME,
    )
    .map_err(|reason| {
        Error::new(
            ErrorKind::CacheCorrupt,
            format!("an open file does not report an identity: {reason}"),
        )
    })?;

    let device = (u64::from(found.stx_dev_major) << 32) | u64::from(found.stx_dev_minor);
    Ok(Identity {
        volume: device,
        file: (u128::from(device) << 64) | u128::from(found.stx_ino),
        size: found.stx_size,
        modified_nanos: i128::from(found.stx_mtime.tv_sec) * 1_000_000_000
            + i128::from(found.stx_mtime.tv_nsec),
        changed_nanos: i128::from(found.stx_ctime.tv_sec) * 1_000_000_000
            + i128::from(found.stx_ctime.tv_nsec),
    })
}
