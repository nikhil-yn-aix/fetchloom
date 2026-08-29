//! The calls that are macOS's own.
//!
//! These are the gaps the Unix syscall wrapper does not cover, so each one goes
//! through the C library directly and each unsafe block states its invariant.

#![expect(
    unsafe_code,
    reason = "the platform seam is where the Apple calls the syscall wrapper does not cover are made, and each block states its invariant"
)]

use std::ffi::{CString, c_void};
use std::fs::File;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use fetchloom_engine::durability::DurabilityTier;
use fetchloom_engine::error::{Error, ErrorKind};
use fetchloom_engine::identity::{BootId, MachineId};

use super::Identity;
use crate::{DegradeQueue, ProcessState};

/// The flag telling a clone not to follow a symbolic link.
///
/// The C library does not name it, so it is named here from the platform's own
/// header, where it is the first flag.
const CLONE_NOFOLLOW: u32 = 0x0001;

/// The attribute group holding what a volume can do.
const ATTR_VOL_INFO: u32 = 0x8000_0000;

/// The attribute naming a volume's capabilities.
const ATTR_VOL_CAPABILITIES: u32 = 0x0002_0000;

/// The index of the group holding the format capabilities.
const VOL_CAPABILITIES_FORMAT: usize = 0;

/// The index of the group holding the interface capabilities.
const VOL_CAPABILITIES_INTERFACES: usize = 1;

/// The bit saying a volume can share blocks between files.
pub(super) const VOL_CAP_INT_CLONE: u32 = 0x0001_0000;

/// The bit saying a volume can reserve blocks without writing them.
pub(super) const VOL_CAP_INT_ALLOCATE: u32 = 0x0000_0040;

/// The bit saying a volume can order writes without flushing to the device.
pub(super) const VOL_CAP_INT_BARRIERFSYNC: u32 = 0x0100_0000;

/// The bit saying a volume stores files with holes.
pub(super) const VOL_CAP_FMT_SPARSE_FILES: u32 = 0x0000_0040;

/// The bit saying a volume links one file under two names.
pub(super) const VOL_CAP_FMT_HARDLINKS: u32 = 0x0000_0004;

#[repr(C)]
#[derive(Clone, Copy)]
struct AttributeList {
    bitmapcount: u16,
    reserved: u16,
    commonattr: u32,
    volattr: u32,
    dirattr: u32,
    fileattr: u32,
    forkattr: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct VolumeCapabilitiesAttribute {
    capabilities: [u32; 4],
    valid: [u32; 4],
}

fn c_path(path: &Path) -> Result<CString, Error> {
    CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        Error::new(
            ErrorKind::DestinationUnrepresentable,
            format!("{} holds a zero byte", path.display()),
        )
    })
}

fn last_error(kind: ErrorKind, path: &Path) -> Error {
    Error::new(
        kind,
        format!("{}: {}", path.display(), std::io::Error::last_os_error()),
    )
}

/// Reads a path's identity, length, and times.
///
/// # Errors
///
/// Fails when the path cannot be read.
pub(super) fn identity(path: &Path) -> Result<Identity, Error> {
    let found = rustix::fs::stat(path).map_err(|reason| {
        Error::new(
            ErrorKind::CacheCorrupt,
            format!("{}: {reason}", path.display()),
        )
    })?;
    let device = found.st_dev as u64;
    Ok(Identity {
        volume: device,
        file: (u128::from(device) << 64) | u128::from(found.st_ino),
        size: found.st_size as u64,
        modified_nanos: i128::from(found.st_mtime) * 1_000_000_000
            + i128::from(found.st_mtime_nsec),
        changed_nanos: i128::from(found.st_ctime) * 1_000_000_000 + i128::from(found.st_ctime_nsec),
    })
}

/// Reads what a volume says it can do.
///
/// Returns the capability bits and the bits that are meaningful, so a
/// capability is believed only where the volume says it answered for it.
///
/// # Errors
///
/// Fails when the platform refuses the query.
pub(super) fn volume_capability_bits(path: &Path) -> Result<([u32; 4], [u32; 4]), Error> {
    let target = c_path(path)?;
    let mut request = AttributeList {
        bitmapcount: 5,
        reserved: 0,
        commonattr: 0,
        volattr: ATTR_VOL_CAPABILITIES | ATTR_VOL_INFO,
        dirattr: 0,
        fileattr: 0,
        forkattr: 0,
    };
    let mut buffer = [0u8; 64];

    // SAFETY: the attribute list is fully initialized, the buffer is at least the size passed, and the path outlives the call.
    let outcome = unsafe {
        libc::getattrlist(
            target.as_ptr(),
            std::ptr::from_mut(&mut request).cast::<c_void>(),
            buffer.as_mut_ptr().cast::<c_void>(),
            buffer.len(),
            0,
        )
    };
    if outcome != 0 {
        return Err(last_error(ErrorKind::DestinationUnrepresentable, path));
    }

    let header = size_of::<u32>();
    if buffer.len() < header + size_of::<VolumeCapabilitiesAttribute>() {
        return Err(Error::new(
            ErrorKind::DestinationUnrepresentable,
            "this volume answered with fewer capability bits than it was asked for".to_owned(),
        ));
    }
    // SAFETY: the reply covers this offset plus the size of the record, and the read is unaligned because the kernel chose the offset.
    let found = unsafe {
        buffer
            .as_ptr()
            .add(header)
            .cast::<VolumeCapabilitiesAttribute>()
            .read_unaligned()
    };
    Ok((found.capabilities, found.valid))
}

/// Reports whether a volume advertises one interface capability.
pub(super) fn has_interface(bits: &([u32; 4], [u32; 4]), which: u32) -> bool {
    bits.1[VOL_CAPABILITIES_INTERFACES] & which != 0
        && bits.0[VOL_CAPABILITIES_INTERFACES] & which != 0
}

/// Reports whether a volume advertises one format capability.
pub(super) fn has_format(bits: &([u32; 4], [u32; 4]), which: u32) -> bool {
    bits.1[VOL_CAPABILITIES_FORMAT] & which != 0 && bits.0[VOL_CAPABILITIES_FORMAT] & which != 0
}

/// Reserves the full length of a file.
///
/// Grows the allocation and then the length, because the allocation call does
/// not itself move the end of the file.
///
/// # Errors
///
/// Fails when the volume has no room.
pub(super) fn preallocate(
    file: &File,
    length: u64,
    degradations: &DegradeQueue,
) -> Result<(), Error> {
    let mut request = libc::fstore_t {
        fst_flags: libc::F_ALLOCATEALL,
        fst_posmode: libc::F_PEOFPOSMODE,
        fst_offset: 0,
        fst_length: i64::try_from(length).unwrap_or(i64::MAX),
        fst_bytesalloc: 0,
    };
    // SAFETY: the descriptor is borrowed from an owned file for the call, and the request behind the pointer is fully initialized and outlives it.
    let outcome = unsafe {
        libc::fcntl(
            file.as_raw_fd(),
            libc::F_PREALLOCATE,
            std::ptr::from_mut(&mut request),
        )
    };
    if outcome == -1 {
        degradations.record(
            "reserving the whole length in blocks before writing",
            "the length was set without reserving blocks",
            "this volume does not reserve blocks without writing them",
        );
    }
    rustix::fs::ftruncate(file, length).map_err(|reason| {
        Error::new(
            ErrorKind::ResourceDisk,
            format!("free {length} bytes on the volume: {reason}"),
        )
    })
}

/// Pushes a file's bytes as far as a durability tier requires.
///
/// The strict tier goes through the standard library, which on this platform is
/// already the call that waits for the device. The normal tier asks only for
/// write ordering, and falls back to the strict call where the volume cannot
/// order writes, reporting that rather than performing it silently.
///
/// # Errors
///
/// Fails when the platform reports the flush did not complete.
pub(super) fn flush(
    file: &File,
    tier: DurabilityTier,
    degradations: &DegradeQueue,
) -> Result<(), Error> {
    match tier {
        DurabilityTier::Strict => file
            .sync_all()
            .map_err(|reason| Error::new(ErrorKind::CacheCorrupt, format!("{reason}"))),
        DurabilityTier::Normal => {
            // SAFETY: the descriptor is borrowed from an owned file for the call, and this command takes no further argument.
            let outcome = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_BARRIERFSYNC) };
            if outcome == -1 {
                degradations.record(
                    "ordering this write against later ones without waiting for the device",
                    "the write was flushed all the way to the device",
                    "this volume does not order writes without flushing them",
                );
                return file
                    .sync_all()
                    .map_err(|reason| Error::new(ErrorKind::CacheCorrupt, format!("{reason}")));
            }
            Ok(())
        }
        DurabilityTier::Fast => Ok(()),
    }
}

/// Shares the blocks of one file with another.
///
/// # Errors
///
/// Fails on every volume that does not share blocks.
pub(super) fn clone_file(from: &Path, to: &Path) -> Result<(), Error> {
    let source = c_path(from)?;
    let target = c_path(to)?;
    // SAFETY: both path pointers are NUL-terminated and outlive the call, and the current directory is named by the constant the C library provides for it.
    let outcome = unsafe {
        libc::clonefileat(
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            target.as_ptr(),
            CLONE_NOFOLLOW,
        )
    };
    if outcome != 0 {
        return Err(Error::new(
            ErrorKind::DestinationUnrepresentable,
            format!(
                "this volume does not share blocks between files: {}",
                std::io::Error::last_os_error()
            ),
        ));
    }
    Ok(())
}

fn sysctl_string(name: &str) -> Option<String> {
    let key = CString::new(name).ok()?;
    let mut length = 0usize;
    // SAFETY: the name is NUL-terminated and outlives the call, and a null value pointer asks only for the length.
    let outcome = unsafe {
        libc::sysctlbyname(
            key.as_ptr(),
            std::ptr::null_mut(),
            &raw mut length,
            std::ptr::null_mut(),
            0,
        )
    };
    if outcome != 0 || length == 0 {
        return None;
    }
    let mut buffer = vec![0u8; length];
    // SAFETY: the name is NUL-terminated and outlives the call, and the buffer is exactly the length the call just reported.
    let outcome = unsafe {
        libc::sysctlbyname(
            key.as_ptr(),
            buffer.as_mut_ptr().cast::<c_void>(),
            &raw mut length,
            std::ptr::null_mut(),
            0,
        )
    };
    if outcome != 0 {
        return None;
    }
    let text = String::from_utf8_lossy(&buffer);
    Some(text.trim_end_matches('\0').to_owned())
}

/// Returns this machine's own identity.
pub(super) fn machine_id() -> Option<MachineId> {
    sysctl_string("kern.uuid").map(MachineId::new)
}

/// Returns the identity of this boot of this machine.
///
/// This is the instant the kernel recorded when it started, which every reader
/// on the machine reads identically, rather than a value derived from uptime.
pub(super) fn boot_id() -> Option<BootId> {
    let mut boot = libc::timeval {
        tv_sec: 0,
        tv_usec: 0,
    };
    let mut length = size_of::<libc::timeval>();
    let key = CString::new("kern.boottime").ok()?;
    // SAFETY: the name is NUL-terminated and outlives the call, and the buffer is exactly the size passed.
    let outcome = unsafe {
        libc::sysctlbyname(
            key.as_ptr(),
            std::ptr::from_mut(&mut boot).cast::<c_void>(),
            &raw mut length,
            std::ptr::null_mut(),
            0,
        )
    };
    if outcome != 0 {
        return None;
    }
    Some(BootId::new(format!("{}.{}", boot.tv_sec, boot.tv_usec)))
}

/// Returns when a process started.
///
/// Reports that no such process exists only when the platform says so, never
/// when one exists and cannot be inspected.
pub(super) fn process_start(pid: u32) -> ProcessState {
    let mut found = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let wanted = i32::try_from(size_of::<libc::proc_bsdinfo>()).unwrap_or(i32::MAX);
    // SAFETY: the buffer is a whole record and the size passed is its own size, so the call cannot write past it.
    let written = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTBSDINFO,
            0,
            found.as_mut_ptr().cast::<c_void>(),
            wanted,
        )
    };
    if written != wanted {
        return match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::ESRCH) => ProcessState::Gone,
            _ => ProcessState::Unreadable,
        };
    }
    // SAFETY: the call reported it filled the whole record, so it is initialized.
    let found = unsafe { found.assume_init() };
    ProcessState::Started(
        found
            .pbi_start_tvsec
            .wrapping_mul(1_000_000)
            .wrapping_add(found.pbi_start_tvusec),
    )
}
