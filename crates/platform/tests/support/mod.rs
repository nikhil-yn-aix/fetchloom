//! Shared helpers for the platform contract tests.

#![expect(
    clippy::unwrap_used,
    dead_code,
    reason = "test helpers, where a failure to build the input is the assertion and each test binary uses a different part"
)]

#[cfg(windows)]
use windows_sys as _;

use std::path::{Path, PathBuf};

use fetchloom_engine::seam::platform::Platform;
use fetchloom_platform::NativePlatform;

/// A directory this process owns, on the volume the temporary directory is on.
pub fn scratch() -> tempfile::TempDir {
    tempfile::TempDir::new().unwrap()
}

/// A directory on a volume other than the one the temporary directory is on.
///
/// Returns nothing when this machine has only one writable volume, which is
/// what a caller reports as a named skip rather than a pass.
pub fn other_volume_scratch() -> Option<tempfile::TempDir> {
    let here = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("target")
        .join("platform-tests");
    std::fs::create_dir_all(&here).ok()?;

    let platform = NativePlatform::new();
    let temporary = std::env::temp_dir();
    let one = platform.volume_id(&temporary).ok()?;
    let two = platform.volume_id(&here).ok()?;
    if one == two {
        return None;
    }
    tempfile::TempDir::new_in(&here).ok()
}

/// Reports whether this build can create a symbolic link in a directory.
///
/// Answers by attempting one and removing it, so the answer is the filesystem's
/// rather than a guess about privilege.
pub fn symlink_works(directory: &Path) -> bool {
    let platform = NativePlatform::new();
    let link = directory.join("fetchloom-symlink-check");
    let created = platform.create_symlink(b"target", &link).is_ok();
    let _ = std::fs::remove_file(&link);
    created
}

/// Creates a file holding the given bytes.
pub fn write_file(path: &Path, bytes: &[u8]) {
    std::fs::write(path, bytes).unwrap();
}

/// Reports whether a directory holds no entries at all.
pub fn is_empty(directory: &Path) -> bool {
    std::fs::read_dir(directory).is_ok_and(|mut entries| entries.next().is_none())
}

/// Lists the names a directory holds, sorted.
pub fn names(directory: &Path) -> Vec<String> {
    let mut found: Vec<String> = std::fs::read_dir(directory)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    found.sort();
    found
}
