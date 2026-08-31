//! Detecting what a volume can do on Linux.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock, PoisonError};

use fetchloom_engine::capability::{
    Backing, CaseFolding, Normalization, Scanner, VolumeCapabilities,
};
use fetchloom_engine::error::{Error, ErrorKind, Surface, filesystem_failure};

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
pub(super) fn capabilities(
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
        let measured = measure(directory, folding)?;
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
            "this platform cannot enumerate what inspects a write, and the measurement cannot tell a scanner from a filesystem that is slow at small writes, so the cause is unknown",
        );
    }
    Ok(answer)
}

fn measure(
    directory: &Path,
    folding: (CaseFolding, Normalization),
) -> Result<VolumeCapabilities, Error> {
    let symlink = symlink_probe(directory);
    let hard_link = hard_link_probe(directory);
    let clone = clone_probe(directory);
    let scanner = scanner_probe(directory)?;

    Ok(VolumeCapabilities {
        case_folding: folding.0,
        normalization: folding.1,
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

const MAX_PATH_LENGTH: u32 = 4096;

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
    let created = rustix::fs::symlinkat("fetchloom-probe-target", rustix::fs::CWD, &link).is_ok();
    let _ = std::fs::remove_file(&link);
    created
}

fn hard_link_probe(directory: &Path) -> bool {
    let tag = probe_tag();
    let original = directory.join(format!("fetchloom-probe-{tag}-original"));
    let linked = directory.join(format!("fetchloom-probe-{tag}-linked"));
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

fn clone_probe(directory: &Path) -> bool {
    let tag = probe_tag();
    let from = directory.join(format!("fetchloom-probe-{tag}-clone-source"));
    let to = directory.join(format!("fetchloom-probe-{tag}-clone-target"));
    if std::fs::write(&from, vec![0u8; 4096]).is_err() {
        return false;
    }
    let cloned = super::clone_file(&from, &to).is_ok();
    let _ = std::fs::remove_file(&to);
    let _ = std::fs::remove_file(&from);
    cloned
}

fn sparse_probe(directory: &Path) -> bool {
    let tag = probe_tag();
    let path = directory.join(format!("fetchloom-probe-{tag}-sparse"));
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

fn max_component_length(directory: &Path) -> u32 {
    rustix::fs::statfs(directory).map_or(255, |found| u32::try_from(found.f_namelen).unwrap_or(255))
}

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

    if ratio > SMALL_WRITE_COST_RATIO {
        return Ok(Scanner::Unknown { cost_ratio: ratio });
    }
    Ok(Scanner::Absent)
}
