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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Property {
    Clone,
    CaseSensitive,
    CaseInsensitive,
    Normalizing,
    Network,
    Memory,
    NoOwnership,
    NoSparse,
    Small,
    ReadOnly,
    Second,
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
        let windows = cfg!(windows);
        match self {
            Self::Clone | Self::Small | Self::Second => true,
            Self::CaseSensitive => linux,
            Self::CaseInsensitive => windows,
            Self::Memory | Self::NoOwnership | Self::NoSparse | Self::ReadOnly | Self::Fuse => {
                linux
            }
            Self::Normalizing | Self::Network => false,
        }
    }
}

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

pub fn scratch_on(property: Property) -> Vec<tempfile::TempDir> {
    volumes(property)
        .iter()
        .map(|volume| {
            tempfile::TempDir::new_in(volume)
                .unwrap_or_else(|reason| panic!("{} is not writable: {reason}", volume.display()))
        })
        .collect()
}

pub fn another_user() -> Option<String> {
    let named = std::env::var("FETCHLOOM_TEST_OTHER_OWNER").ok();
    assert!(
        !(named.is_none() && volumes_were_built() && cfg!(target_os = "linux")),
        "FETCHLOOM_TEST_OTHER_OWNER is unset in a verification run that creates the user"
    );
    named
}

#[cfg(unix)]
pub fn as_another_user() -> std::process::Command {
    let root = std::process::Command::new("id")
        .arg("-u")
        .output()
        .is_ok_and(|answer| answer.stdout.starts_with(b"0"));
    if root {
        return std::process::Command::new("env");
    }
    let mut elevated = std::process::Command::new("sudo");
    elevated.args(["-n", "env"]);
    elevated
}

pub fn volumes_were_built() -> bool {
    std::env::var_os("FETCHLOOM_VERIFY_VOLUMES").is_some()
}

pub fn scratch() -> tempfile::TempDir {
    tempfile::TempDir::new().unwrap()
}

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

    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    let temporary = std::env::temp_dir();
    let one = platform.volume_id(&temporary).ok()?;
    let two = platform.volume_id(&here).ok()?;
    if one == two {
        return None;
    }
    tempfile::TempDir::new_in(&here).ok()
}

pub fn symlink_works(directory: &Path) -> bool {
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    let link = directory.join("fetchloom-symlink-check");
    let created = platform.create_symlink(b"target", &link).is_ok();
    let _ = std::fs::remove_file(&link);
    created
}

pub fn write_file(path: &Path, bytes: &[u8]) {
    std::fs::write(path, bytes).unwrap();
}

pub fn is_empty(directory: &Path) -> bool {
    std::fs::read_dir(directory).is_ok_and(|mut entries| entries.next().is_none())
}

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

pub fn volume_directories(property: Property) -> Vec<PathBuf> {
    volumes(property)
}

pub fn remove_all(names: &[PathBuf]) {
    for name in names {
        let _ = std::fs::remove_file(name);
    }
}
