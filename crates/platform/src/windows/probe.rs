//! Detecting what a volume can do, by query where Windows answers and by
//! probe where it does not.
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
use fetchloom_engine::identity::VolumeId;
use windows_sys::Win32::System::SystemServices::{
    FILE_SUPPORTS_BLOCK_REFCOUNTING, FILE_SUPPORTS_HARD_LINKS, FILE_SUPPORTS_SPARSE_FILES,
};

use super::{MAX_PATH_LENGTH, ffi};
use crate::DegradeQueue;

/// The name the case probe creates first.
const UPPER_NAME: &str = "fetchloom-probe-A";

/// The name the case probe attempts second.
const LOWER_NAME: &str = "fetchloom-probe-a";

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
/// Takes a directory Fetchloom owns and somewhere to record a fallback.
/// Returns the same answer every time it is asked about one volume, because the
/// answer is kept for the life of the process.
///
/// # Errors
///
/// Fails when the directory cannot be written to.
pub(crate) fn capabilities(
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

    let measured = measure(directory, volume, degradations)?;
    cache()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(volume.value(), measured.clone());
    Ok(measured)
}

fn measure(
    directory: &Path,
    volume: VolumeId,
    degradations: &DegradeQueue,
) -> Result<VolumeCapabilities, Error> {
    let handle = ffi::open_for_query(directory).map_err(|reason| failure(directory, &reason))?;
    let information =
        ffi::volume_information(&handle).map_err(|reason| failure(directory, &reason))?;
    drop(handle);

    let (case_folding, normalization) = fold_probe(directory)?;
    let symlink = symlink_probe(directory);
    let scanner = scanner_probe(directory, degradations)?;
    let _ = volume;

    Ok(VolumeCapabilities {
        case_folding,
        normalization,
        clone: information.flags & FILE_SUPPORTS_BLOCK_REFCOUNTING != 0,
        sparse: information.flags & FILE_SUPPORTS_SPARSE_FILES != 0,
        symlink,
        hard_link: information.flags & FILE_SUPPORTS_HARD_LINKS != 0,
        max_component_length: information.max_component_length,
        max_path_length: MAX_PATH_LENGTH,
        backing: if ffi::is_remote_drive(directory) {
            Backing::Network
        } else {
            Backing::Local
        },
        scanner,
    })
}

fn fold_probe(directory: &Path) -> Result<(CaseFolding, Normalization), Error> {
    let upper = directory.join(UPPER_NAME);
    let lower = directory.join(LOWER_NAME);
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
    let created = ffi::create_symlink("fetchloom-probe-target", &link, false).is_ok();
    let _ = std::fs::remove_file(&link);
    created
}

fn scanner_probe(directory: &Path, degradations: &DegradeQueue) -> Result<Scanner, Error> {
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

    if let Some(name) = minifilter_name() {
        return Ok(Scanner::Present {
            name: Some(name),
            cost_ratio: ratio,
        });
    }
    degradations.record(
        "the name of any on-access scanner inspecting writes",
        "no scanner was named",
        "no minifilter sits in the range the platform allocates to scanners",
    );
    Ok(Scanner::Absent)
}

/// Names the on-access scanner inspecting writes, when one is loaded.
///
/// Returns the product's name when a minifilter sits in the range the platform
/// allocates to anti-virus filters or to activity monitors, and nothing when
/// none does.
fn minifilter_name() -> Option<String> {
    ffi::loaded_minifilters()
        .into_iter()
        .find(|filter| is_scanner_altitude(filter.altitude))
        .map(|filter| filter.name)
}

/// Reports whether an altitude is one the platform allocates to a scanner.
///
/// Anti-virus minifilters are allocated 320000 through 329998 and activity
/// monitors, where endpoint detection products sit, 360000 through 389999.
fn is_scanner_altitude(altitude: u32) -> bool {
    (320_000..=329_998).contains(&altitude) || (360_000..=389_999).contains(&altitude)
}
