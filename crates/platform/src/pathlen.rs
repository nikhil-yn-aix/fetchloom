//! How long a path a volume accepts, measured on the volume rather than
//! assumed from the platform.

use std::path::{Path, PathBuf};

/// Returns the longest of the candidate lengths this volume accepts, measured
/// by writing one.
///
/// The candidates are given longest first, and the first that a volume accepts
/// is the answer, so a volume with long paths costs one attempt and one without
/// costs one more. Nothing is left behind whichever way the answer goes.
pub(crate) fn measure(directory: &Path, candidates: &[u32], component: u32) -> u32 {
    let last = candidates.last().copied().unwrap_or(0);
    for wanted in candidates {
        if accepts(directory, *wanted, component) {
            return *wanted;
        }
    }
    last
}

/// Reports whether this volume accepts a path of the given length inside the
/// given directory.
fn accepts(directory: &Path, wanted: u32, component: u32) -> bool {
    let tag = crate::probe_tag();
    let root = directory.join(format!("fetchloom-probe-{tag}-length"));
    let built = build(&root, wanted, component);
    let answer = built.is_some_and(|path| {
        std::fs::write(&path, b"").is_ok() && {
            let _ = std::fs::remove_file(&path);
            true
        }
    });
    let _ = remove_deeply(&root);
    answer
}

/// Builds a path of the wanted length under a root, nesting directories whose
/// names are no longer than the volume permits, and creates every directory but
/// the last name.
fn build(root: &Path, wanted: u32, component: u32) -> Option<PathBuf> {
    let step = usize::try_from(component.clamp(8, 200)).ok()?;
    let wanted = usize::try_from(wanted).ok()?;
    let mut path = root.to_path_buf();
    if path.as_os_str().len() >= wanted {
        return None;
    }
    while path.as_os_str().len() + step + 1 < wanted {
        path.push("x".repeat(step));
    }
    std::fs::create_dir_all(&path).ok()?;
    let left = wanted.checked_sub(path.as_os_str().len() + 1)?;
    if left == 0 {
        return None;
    }
    Some(path.join("x".repeat(left)))
}

/// Removes a probe tree, however deep the probe made it.
fn remove_deeply(root: &Path) -> std::io::Result<()> {
    match std::fs::remove_dir_all(root) {
        Err(reason) if reason.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}
