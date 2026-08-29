//! Detecting what a volume can do on Unix.
//!
//! Every probe runs inside a directory Fetchloom owns and removes what it
//! creates, so nothing is ever written into a user's destination.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock, PoisonError};

use fetchloom_engine::capability::{
    Backing, CaseFolding, Normalization, Scanner, VolumeCapabilities,
};
use fetchloom_engine::error::{Error, ErrorKind};

use crate::DegradeQueue;

/// The ratio above which an on-access scanner is called present.
///
/// This value is provisional. Neither Linux nor macOS can enumerate a scanner,
/// so presence is decided by measurement alone, and no measurement stands
/// behind this number yet. The slice that measures a runner with a scanner
/// enabled and disabled replaces it here, in this one place.
const PROVISIONAL_SCANNER_RATIO: f64 = 2.0;

/// How many small files the scanner measurement writes.
const SCANNER_FILES: usize = 64;

/// How many bytes each of those files holds.
const SCANNER_FILE_BYTES: usize = 4096;

fn cache() -> &'static Mutex<HashMap<u64, VolumeCapabilities>> {
    static CACHE: OnceLock<Mutex<HashMap<u64, VolumeCapabilities>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn failure(path: &Path, reason: &std::io::Error) -> Error {
    Error::new(
        ErrorKind::DestinationUnrepresentable,
        format!("{}: {reason}", path.display()),
    )
}

/// Detects everything about the volume behind a directory.
///
/// Returns the same answer every time it is asked about one volume, because the
/// answer is kept for the life of the process.
///
/// # Errors
///
/// Fails when the directory cannot be written to.
pub(super) fn capabilities(
    directory: &Path,
    degradations: &DegradeQueue,
) -> Result<VolumeCapabilities, Error> {
    let volume = super::volume_id(directory)?;
    if let Some(found) = cache()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&volume.value())
    {
        return Ok(found.clone());
    }
    let measured = measure(directory, degradations)?;
    cache()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(volume.value(), measured.clone());
    Ok(measured)
}

fn measure(directory: &Path, degradations: &DegradeQueue) -> Result<VolumeCapabilities, Error> {
    let (case_folding, normalization) = fold_probe(directory)?;
    let symlink = symlink_probe(directory);
    let hard_link = hard_link_probe(directory);
    let clone = clone_probe(directory);
    let scanner = scanner_probe(directory)?;
    let _ = degradations;

    Ok(VolumeCapabilities {
        case_folding,
        normalization,
        clone,
        sparse: sparse_probe(directory),
        symlink,
        hard_link,
        max_component_length: max_component_length(directory),
        max_path_length: MAX_PATH_LENGTH,
        backing: backing(directory),
        scanner,
    })
}

#[cfg(target_os = "linux")]
const MAX_PATH_LENGTH: u32 = 4096;

#[cfg(target_vendor = "apple")]
const MAX_PATH_LENGTH: u32 = 1024;

fn fold_probe(directory: &Path) -> Result<(CaseFolding, Normalization), Error> {
    let upper = directory.join("fetchloom-probe-A");
    let lower = directory.join("fetchloom-probe-a");
    std::fs::File::create_new(&upper).map_err(|reason| failure(&upper, &reason))?;
    let folds_case = std::fs::File::create_new(&lower).is_err();
    let _ = std::fs::remove_file(&lower);
    let _ = std::fs::remove_file(&upper);

    let composed = String::from_utf8(vec![b'f', b'l', 0xc3, 0xa9])
        .unwrap_or_else(|_| "flprobe-composed".to_owned());
    let decomposed = String::from_utf8(vec![b'f', b'l', 0x65, 0xcc, 0x81])
        .unwrap_or_else(|_| "flprobe-decomposed".to_owned());
    let first = directory.join(&composed);
    let second = directory.join(&decomposed);
    std::fs::File::create_new(&first).map_err(|reason| failure(&first, &reason))?;
    let folds_normalization = std::fs::File::create_new(&second).is_err();

    let stored_as_written = std::fs::read_dir(directory).is_ok_and(|entries| {
        entries
            .flatten()
            .any(|entry| entry.file_name().to_string_lossy() == composed)
    });

    let _ = std::fs::remove_file(&second);
    let _ = std::fs::remove_file(&first);

    let normalization = if !folds_normalization {
        Normalization::Sensitive
    } else if stored_as_written {
        Normalization::InsensitivePreserving
    } else {
        Normalization::Normalizing
    };
    let case_folding = if folds_case {
        CaseFolding::Folding
    } else {
        CaseFolding::Sensitive
    };
    Ok((case_folding, normalization))
}

fn symlink_probe(directory: &Path) -> bool {
    let link = directory.join("fetchloom-probe-link");
    let created = rustix::fs::symlinkat("fetchloom-probe-target", rustix::fs::CWD, &link).is_ok();
    let _ = std::fs::remove_file(&link);
    created
}

fn hard_link_probe(directory: &Path) -> bool {
    let original = directory.join("fetchloom-probe-original");
    let linked = directory.join("fetchloom-probe-linked");
    if std::fs::write(&original, b"probe").is_err() {
        return false;
    }
    let created = rustix::fs::linkat(
        rustix::fs::CWD,
        &original,
        rustix::fs::CWD,
        &linked,
        rustix::fs::AtFlags::empty(),
    )
    .is_ok();
    let _ = std::fs::remove_file(&linked);
    let _ = std::fs::remove_file(&original);
    created
}

#[cfg(target_os = "linux")]
fn clone_probe(directory: &Path) -> bool {
    let from = directory.join("fetchloom-probe-clone-source");
    let to = directory.join("fetchloom-probe-clone-target");
    if std::fs::write(&from, vec![0u8; 4096]).is_err() {
        return false;
    }
    let cloned = super::clone_file(&from, &to).is_ok();
    let _ = std::fs::remove_file(&to);
    let _ = std::fs::remove_file(&from);
    cloned
}

#[cfg(target_vendor = "apple")]
fn clone_probe(directory: &Path) -> bool {
    super::macos::volume_capability_bits(directory)
        .map(|bits| super::macos::has_interface(&bits, super::macos::VOL_CAP_INT_CLONE))
        .unwrap_or(false)
}

#[cfg(target_os = "linux")]
fn sparse_probe(directory: &Path) -> bool {
    let path = directory.join("fetchloom-probe-sparse");
    let Ok(file) = std::fs::File::create(&path) else {
        return false;
    };
    if rustix::fs::ftruncate(&file, 65_536).is_err() {
        let _ = std::fs::remove_file(&path);
        return false;
    }
    let punched = rustix::fs::fallocate(
        &file,
        rustix::fs::FallocateFlags::PUNCH_HOLE | rustix::fs::FallocateFlags::KEEP_SIZE,
        0,
        4096,
    )
    .is_ok();
    drop(file);
    let _ = std::fs::remove_file(&path);
    punched
}

#[cfg(target_vendor = "apple")]
fn sparse_probe(directory: &Path) -> bool {
    super::macos::volume_capability_bits(directory)
        .map(|bits| super::macos::has_format(&bits, super::macos::VOL_CAP_FMT_SPARSE_FILES))
        .unwrap_or(false)
}

#[cfg(target_os = "linux")]
fn max_component_length(directory: &Path) -> u32 {
    rustix::fs::statfs(directory).map_or(255, |found| u32::try_from(found.f_namelen).unwrap_or(255))
}

#[cfg(target_vendor = "apple")]
#[expect(
    unsafe_code,
    reason = "the longest component a volume accepts has no wrapper, and the block states its invariant"
)]
fn max_component_length(directory: &Path) -> u32 {
    use std::os::unix::ffi::OsStrExt;
    let Ok(path) = std::ffi::CString::new(directory.as_os_str().as_bytes()) else {
        return 255;
    };
    // SAFETY: the path is NUL-terminated and outlives the call.
    let found = unsafe { libc::pathconf(path.as_ptr(), libc::_PC_NAME_MAX) };
    u32::try_from(found).unwrap_or(255)
}

#[cfg(target_os = "linux")]
pub(crate) fn backing(directory: &Path) -> Backing {
    const NETWORK_KINDS: [i64; 7] = [
        0x6969,
        0x517b,
        0xfe53_4d42,
        0xff53_4d42,
        0x5346_414f,
        0x0102_1997,
        0x7461_636f,
    ];
    const FUSE: i64 = 0x6573_5546;

    let Ok(found) = rustix::fs::statfs(directory) else {
        return Backing::Unknown;
    };
    let kind = found.f_type;
    if NETWORK_KINDS.contains(&kind) {
        Backing::Network
    } else if kind == FUSE {
        Backing::Unknown
    } else {
        Backing::Local
    }
}

#[cfg(target_vendor = "apple")]
#[expect(
    unsafe_code,
    reason = "the mount flags have no wrapper on this platform, and each block states its invariant"
)]
pub(crate) fn backing(directory: &Path) -> Backing {
    use std::os::unix::ffi::OsStrExt;
    let Ok(path) = std::ffi::CString::new(directory.as_os_str().as_bytes()) else {
        return Backing::Unknown;
    };
    let mut found = std::mem::MaybeUninit::<libc::statfs>::zeroed();
    // SAFETY: the path is NUL-terminated and outlives the call, and the buffer is a whole record the call fills.
    let outcome = unsafe { libc::statfs(path.as_ptr(), found.as_mut_ptr()) };
    if outcome != 0 {
        return Backing::Unknown;
    }
    // SAFETY: the call reported success, so the record is initialized.
    let found = unsafe { found.assume_init() };
    if found.f_flags & libc::MNT_LOCAL as u32 == 0 {
        Backing::Network
    } else {
        Backing::Local
    }
}

fn scanner_probe(directory: &Path) -> Result<Scanner, Error> {
    let bytes = vec![0u8; SCANNER_FILE_BYTES];

    let many = directory.join("fetchloom-probe-many");
    std::fs::create_dir(&many).map_err(|reason| failure(&many, &reason))?;
    let started = std::time::Instant::now();
    for index in 0..SCANNER_FILES {
        let path = many.join(format!("{index}"));
        std::fs::write(&path, &bytes).map_err(|reason| failure(&path, &reason))?;
    }
    let small = started.elapsed();
    let _ = std::fs::remove_dir_all(&many);

    let one = directory.join("fetchloom-probe-one");
    let whole = vec![0u8; SCANNER_FILE_BYTES * SCANNER_FILES];
    let started = std::time::Instant::now();
    std::fs::write(&one, &whole).map_err(|reason| failure(&one, &reason))?;
    let large = started.elapsed();
    let _ = std::fs::remove_file(&one);

    let ratio = if large.as_secs_f64() > 0.0 {
        small.as_secs_f64() / large.as_secs_f64()
    } else {
        1.0
    };

    if ratio > PROVISIONAL_SCANNER_RATIO {
        Ok(Scanner::Present {
            name: None,
            cost_ratio: ratio,
        })
    } else {
        Ok(Scanner::Absent)
    }
}
