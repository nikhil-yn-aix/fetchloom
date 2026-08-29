//! Shared helpers for the platform contract tests.

#![expect(
    clippy::unwrap_used,
    clippy::panic,
    dead_code,
    reason = "test helpers, where a failure to build the input is the assertion and each test binary uses a different part"
)]

#[cfg(windows)]
use windows_sys as _;

use std::path::{Path, PathBuf};

use fetchloom_engine::seam::platform::Platform;
use fetchloom_platform::NativePlatform;

/// A property a test needs from a volume, rather than a filesystem name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Property {
    /// Blocks can be shared instead of copied.
    Clone,
    /// Two names differing only in case are two names.
    CaseSensitive,
    /// Two names differing only in case are one name.
    CaseInsensitive,
    /// Two spellings of one string are stored as one normalized form.
    Normalizing,
    /// The volume is reached over a network protocol.
    Network,
    /// The volume is memory rather than a device.
    Memory,
    /// The volume records no owner for a file.
    NoOwnership,
    /// The volume stores no holes.
    NoSparse,
    /// The volume is small enough to fill.
    Small,
    /// The volume cannot be written to.
    ReadOnly,
    /// The volume is not the one the temporary directory is on.
    Second,
    /// The volume is served by a filesystem in user space.
    Fuse,
}

impl Property {
    fn variable(self) -> &'static str {
        match self {
            Self::Clone => "FETCHLOOM_TEST_CLONE_VOLUMES",
            Self::CaseSensitive => "FETCHLOOM_TEST_CASE_SENSITIVE_VOLUMES",
            Self::CaseInsensitive => "FETCHLOOM_TEST_CASE_INSENSITIVE_VOLUMES",
            Self::Normalizing => "FETCHLOOM_TEST_NORMALIZING_VOLUMES",
            Self::Network => "FETCHLOOM_TEST_NETWORK_VOLUMES",
            Self::Memory => "FETCHLOOM_TEST_MEMORY_VOLUMES",
            Self::NoOwnership => "FETCHLOOM_TEST_NO_OWNERSHIP_VOLUMES",
            Self::NoSparse => "FETCHLOOM_TEST_NO_SPARSE_VOLUMES",
            Self::Small => "FETCHLOOM_TEST_SMALL_VOLUMES",
            Self::ReadOnly => "FETCHLOOM_TEST_READ_ONLY_VOLUMES",
            Self::Second => "FETCHLOOM_TEST_SECOND_VOLUMES",
            Self::Fuse => "FETCHLOOM_TEST_FUSE_VOLUMES",
        }
    }

    fn promised_here(self) -> bool {
        let linux = cfg!(target_os = "linux");
        let macos = cfg!(target_os = "macos");
        let windows = cfg!(windows);
        match self {
            Self::Clone | Self::Small | Self::Second => true,
            Self::CaseSensitive => linux || macos,
            Self::CaseInsensitive => macos || windows,
            Self::Normalizing => macos,
            Self::Memory | Self::NoOwnership | Self::NoSparse | Self::ReadOnly | Self::Fuse => {
                linux
            }
            Self::Network => false,
        }
    }
}

/// Every volume the environment offers with the given property.
///
/// Returns the paths named by the property's variable, which the volume script
/// writes after it builds the filesystems. An empty answer in a verification run
/// that promised the property fails rather than skipping, so a filesystem that
/// did not get built is a failure and not a silent pass.
pub fn volumes(property: Property) -> Vec<PathBuf> {
    let named = std::env::var_os(property.variable()).unwrap_or_default();
    let found: Vec<PathBuf> = std::env::split_paths(&named)
        .filter(|path| !path.as_os_str().is_empty())
        .collect();
    assert!(
        !(found.is_empty() && volumes_were_built() && property.promised_here()),
        "{} is unset in a verification run that builds this filesystem",
        property.variable()
    );
    found
}

/// A directory inside each volume offering the given property.
///
/// Returns one owned directory per volume, removed when it is dropped.
pub fn scratch_on(property: Property) -> Vec<tempfile::TempDir> {
    volumes(property)
        .iter()
        .map(|volume| {
            tempfile::TempDir::new_in(volume)
                .unwrap_or_else(|reason| panic!("{} is not writable: {reason}", volume.display()))
        })
        .collect()
}

/// The user a test hands work to, when the environment names one.
///
/// A verification run that built the volumes promised this user on Linux, so an
/// absent name there is a provisioning failure rather than a reason to skip.
pub fn another_user() -> Option<String> {
    let named = std::env::var("FETCHLOOM_TEST_OTHER_OWNER").ok();
    assert!(
        !(named.is_none() && volumes_were_built() && cfg!(target_os = "linux")),
        "FETCHLOOM_TEST_OTHER_OWNER is unset in a verification run that creates the user"
    );
    named
}

/// Reports whether the verification matrix built the volumes for this run.
pub fn volumes_were_built() -> bool {
    std::env::var_os("FETCHLOOM_VERIFY_VOLUMES").is_some()
}

/// A directory this process owns, on the volume the temporary directory is on.
pub fn scratch() -> tempfile::TempDir {
    tempfile::TempDir::new().unwrap()
}

/// A directory on a volume other than the one the temporary directory is on.
///
/// Returns nothing when this machine has only one writable volume, which is
/// what a caller reports as a named skip rather than a pass.
pub fn other_volume_scratch() -> Option<tempfile::TempDir> {
    if let Some(built) = scratch_on(Property::Second).into_iter().next() {
        return Some(built);
    }
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

/// A directory this test owns inside each volume offering a property, where
/// the directory is the volume's own rather than one created inside it.
///
/// Windows carries case sensitivity on the directory itself and does not pass
/// it to a directory created inside one, so a test about case has to work in
/// the directory the environment named.
pub fn volume_directories(property: Property) -> Vec<PathBuf> {
    volumes(property)
}

/// Removes the names a test made in a directory it does not own.
pub fn remove_all(names: &[PathBuf]) {
    for name in names {
        let _ = std::fs::remove_file(name);
    }
}
