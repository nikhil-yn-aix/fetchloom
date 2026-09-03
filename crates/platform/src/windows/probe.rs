//! Detecting what a volume can do, by query where Windows answers and by probe
//! where it does not.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock, PoisonError};

use fetchloom_engine::capability::{
    Backing, CaseFolding, Normalization, Scanner, VolumeCapabilities,
};
use fetchloom_engine::error::{Error, Surface, filesystem_failure};
use fetchloom_engine::identity::VolumeId;
use windows_sys::Win32::System::SystemServices::{
    FILE_SUPPORTS_BLOCK_REFCOUNTING, FILE_SUPPORTS_HARD_LINKS, FILE_SUPPORTS_SPARSE_FILES,
};

use super::{PATH_LENGTHS, ffi};
use fetchloom_engine::degrade::DegradeQueue;

use crate::probe_tag;

/// The ratio above which writing many small files is called expensive.
const SMALL_WRITE_COST_RATIO: f64 = 2.0;

/// How many small files the scanner measurement writes.
const SCANNER_FILES: usize = 64;

/// How many bytes each of those files holds.
const SCANNER_FILE_BYTES: usize = 4096;

fn cache() -> &'static Mutex<HashMap<u64, VolumeCapabilities>> {
    static CACHE: OnceLock<Mutex<HashMap<u64, VolumeCapabilities>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Detects everything about the volume behind a directory.
///
/// # Errors
///
/// Fails when the directory cannot be written to.
pub(crate) fn capabilities(
    directory: &Path,
    degradations: &DegradeQueue,
) -> Result<VolumeCapabilities, Error> {
    let volume = super::volume_id(directory)?;
    let folding = fold_probe(directory, degradations)?;
    let held = cache()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&volume.value())
        .cloned();
    let answer = if let Some(found) = held {
        VolumeCapabilities {
            case_folding: folding.0,
            normalization: folding.1,
            ..found
        }
    } else {
        let measured = measure(directory, volume, folding)?;
        cache()
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(volume.value(), measured.clone());
        measured
    };
    if let Scanner::Unknown { cost_ratio } = answer.scanner {
        degradations.record(
            "whether an on-access scanner inspects writes on this volume",
            format!("a measured cost ratio of {cost_ratio:.2} with the answer left unknown"),
            "the filter manager refused to say which filters are registered, which it does for a process that is not elevated, so the cause is unknown",
        );
    }
    Ok(answer)
}

fn measure(
    directory: &Path,
    volume: VolumeId,
    folding: (CaseFolding, Normalization),
) -> Result<VolumeCapabilities, Error> {
    let handle = ffi::open_for_query(directory)
        .map_err(|reason| filesystem_failure(Surface::Destination, directory, &reason))?;
    let information = ffi::volume_information(&handle)
        .map_err(|reason| filesystem_failure(Surface::Destination, directory, &reason))?;
    drop(handle);

    let symlink = symlink_probe(directory);
    let scanner = scanner_probe(directory)?;
    let _ = volume;

    Ok(VolumeCapabilities {
        case_folding: folding.0,
        normalization: folding.1,
        clone: information.flags & FILE_SUPPORTS_BLOCK_REFCOUNTING != 0,
        sparse: information.flags & FILE_SUPPORTS_SPARSE_FILES != 0,
        symlink,
        hard_link: information.flags & FILE_SUPPORTS_HARD_LINKS != 0,
        max_component_length: information.max_component_length,
        max_path_length: crate::pathlen::measure(
            directory,
            &PATH_LENGTHS,
            information.max_component_length,
        ),
        backing: if ffi::is_remote_drive(directory) {
            Backing::Network
        } else {
            Backing::Local
        },
        scanner,
    })
}

fn fold_probe(
    directory: &Path,
    degradations: &DegradeQueue,
) -> Result<(CaseFolding, Normalization), Error> {
    let tag = probe_tag();
    let upper = directory.join(format!("fetchloom-probe-{tag}-A"));
    let lower = directory.join(format!("fetchloom-probe-{tag}-a"));
    std::fs::File::create_new(&upper)
        .map_err(|reason| filesystem_failure(Surface::Destination, &upper, &reason))?;
    let folds_case = std::fs::File::create_new(&lower).is_err();
    let _ = std::fs::remove_file(&lower);
    let _ = std::fs::remove_file(&upper);

    let stem = format!("fetchloom-probe-{tag}-");
    let composed = String::from_utf8([stem.as_bytes(), &[0xc3, 0xa9]].concat())
        .unwrap_or_else(|_| format!("{stem}composed"));
    let decomposed = String::from_utf8([stem.as_bytes(), &[0x65, 0xcc, 0x81]].concat())
        .unwrap_or_else(|_| format!("{stem}decomposed"));
    let first = directory.join(&composed);
    let second = directory.join(&decomposed);
    let case_folding = if folds_case {
        CaseFolding::Folding
    } else {
        CaseFolding::Sensitive
    };
    if let Err(reason) = std::fs::File::create_new(&first) {
        degradations.record(
            "how this volume treats two spellings of one name",
            "nothing, because the answer is unknown",
            format!("the volume refused the name the probe measures with: {reason}"),
        );
        return Ok((case_folding, Normalization::Unknown));
    }
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
    Ok((case_folding, normalization))
}

fn symlink_probe(directory: &Path) -> bool {
    let tag = probe_tag();
    let link = directory.join(format!("fetchloom-probe-{tag}-link"));
    let created = ffi::create_symlink("fetchloom-probe-target", &link, false).is_ok();
    let _ = std::fs::remove_file(&link);
    created
}

fn scanner_probe(directory: &Path) -> Result<Scanner, Error> {
    let tag = probe_tag();
    let bytes = vec![0u8; SCANNER_FILE_BYTES];

    let many = directory.join(format!("fetchloom-probe-{tag}-many"));
    std::fs::create_dir(&many)
        .map_err(|reason| filesystem_failure(Surface::Destination, &many, &reason))?;
    let started = std::time::Instant::now();
    for index in 0..SCANNER_FILES {
        let path = many.join(format!("{index}"));
        std::fs::write(&path, &bytes)
            .map_err(|reason| filesystem_failure(Surface::Destination, &path, &reason))?;
    }
    let small = started.elapsed();
    let _ = std::fs::remove_dir_all(&many);

    let one = directory.join(format!("fetchloom-probe-{tag}-one"));
    let whole = vec![0u8; SCANNER_FILE_BYTES * SCANNER_FILES];
    let started = std::time::Instant::now();
    std::fs::write(&one, &whole)
        .map_err(|reason| filesystem_failure(Surface::Destination, &one, &reason))?;
    let large = started.elapsed();
    let _ = std::fs::remove_file(&one);

    let ratio = if large.as_secs_f64() > 0.0 {
        small.as_secs_f64() / large.as_secs_f64()
    } else {
        1.0
    };

    let Some(loaded) = ffi::loaded_minifilters() else {
        if ratio > SMALL_WRITE_COST_RATIO {
            return Ok(Scanner::Unknown { cost_ratio: ratio });
        }
        return Ok(Scanner::Absent);
    };
    let named = loaded
        .into_iter()
        .find(|filter| is_scanner_altitude(filter.altitude))
        .map(|filter| filter.name);
    match named {
        Some(name) => Ok(Scanner::Present {
            name,
            cost_ratio: ratio,
        }),
        None => Ok(Scanner::Absent),
    }
}

/// Reports whether an altitude is one the platform allocates to a scanner.
fn is_scanner_altitude(altitude: u32) -> bool {
    (320_000..=329_998).contains(&altitude) || (360_000..=389_999).contains(&altitude)
}
