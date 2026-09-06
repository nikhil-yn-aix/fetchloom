//! One place decides which kind a filesystem failure carries.

#![expect(
    clippy::unwrap_used,
    reason = "test assertions, where the run that failed is the message"
)]

use blake3 as _;
use rayon as _;
use serde as _;
use serde_json as _;
use sha2 as _;
use toml as _;

use std::io::{Error as IoError, ErrorKind as IoErrorKind};
use std::path::Path;

use fetchloom_engine::error::{ErrorKind, Surface, filesystem_failure};

fn kind_of(surface: Surface, io: IoErrorKind) -> ErrorKind {
    filesystem_failure(surface, Path::new("somewhere"), &IoError::from(io)).kind()
}

#[test]
fn a_volume_with_no_room_is_a_resource_failure_on_either_surface() {
    for surface in [Surface::Cache, Surface::Destination] {
        assert_eq!(
            kind_of(surface, IoErrorKind::StorageFull),
            ErrorKind::ResourceDisk,
            "{surface:?} called a full volume something else"
        );
        assert_eq!(
            kind_of(surface, IoErrorKind::QuotaExceeded),
            ErrorKind::ResourceDisk,
            "{surface:?} called an exceeded quota something else"
        );
    }
}

#[test]
fn a_volume_boundary_names_the_surface_it_was_crossed_on() {
    assert_eq!(
        kind_of(Surface::Cache, IoErrorKind::CrossesDevices),
        ErrorKind::CacheCrossVolume
    );
    assert_eq!(
        kind_of(Surface::Destination, IoErrorKind::CrossesDevices),
        ErrorKind::DestinationCrossVolume
    );
}

#[test]
fn a_failure_the_table_does_not_name_falls_to_the_surface_it_happened_on() {
    for io in [
        IoErrorKind::PermissionDenied,
        IoErrorKind::NotFound,
        IoErrorKind::InvalidData,
    ] {
        assert_eq!(kind_of(Surface::Cache, io), ErrorKind::CacheCorrupt);
        assert_eq!(
            kind_of(Surface::Destination, io),
            ErrorKind::DestinationUnrepresentable
        );
    }
}

#[test]
fn the_failure_says_which_path_it_happened_on_and_what_the_filesystem_said() {
    let error = filesystem_failure(
        Surface::Cache,
        Path::new("objects/ab/cd"),
        &IoError::new(IoErrorKind::PermissionDenied, "access is denied"),
    );
    let action = error.next_action();
    assert!(action.contains("objects"), "{action}");
    assert!(action.contains("access is denied"), "{action}");
}

#[test]
fn no_other_place_turns_a_filesystem_failure_into_an_error() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let mut offenders = Vec::new();
    let crates = std::fs::read_dir(root.join("crates"))
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    for crate_name in crates {
        let directory = root.join("crates").join(crate_name).join("src");
        let mut files = Vec::new();
        collect(&directory, &mut files);
        for file in files {
            let text = std::fs::read_to_string(&file).unwrap();
            let lines: Vec<&str> = text.lines().collect();
            for (number, line) in lines.iter().enumerate() {
                let signature = line.trim_start();
                let statement = format!(
                    "{} {signature}",
                    lines.get(number.wrapping_sub(1)).unwrap_or(&"")
                );
                if decides_for_itself(signature)
                    && !PERMITTED.iter().any(|(_, held)| statement.contains(held))
                {
                    offenders.push(format!("{}:{}: {}", file.display(), number + 1, signature));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "these turn a filesystem failure into an error themselves instead of asking the one place that decides: {offenders:#?}"
    );
}

fn collect(directory: &Path, into: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, into);
        } else if path.extension().and_then(std::ffi::OsStr::to_str) == Some("rs") {
            into.push(path);
        }
    }
}

#[test]
fn a_lock_a_volume_cannot_express_is_refused_as_unable_to_lock() {
    for raw in [37, 95, 38, 22, 1] {
        let refused = fetchloom_engine::error::lock_failure(
            Path::new("locks/probe"),
            &IoError::from_raw_os_error(raw),
        );
        assert_eq!(
            refused.kind(),
            ErrorKind::CacheLockingUnsupported,
            "a lock refused with {raw} was called something else"
        );
    }
}

#[test]
fn a_lock_that_failed_for_want_of_room_is_a_resource_failure_and_not_a_volume_verdict() {
    let refused = fetchloom_engine::error::lock_failure(
        Path::new("locks/probe"),
        &IoError::from(IoErrorKind::StorageFull),
    );
    assert_eq!(refused.kind(), ErrorKind::ResourceDisk);
}

fn decides_for_itself(line: &str) -> bool {
    let reports_a_failure = ["io::Error", "Errno", "std::io::Error"]
        .iter()
        .any(|named| line.contains(named));
    let declares = line.starts_with("fn ")
        || line.starts_with("pub fn ")
        || line.starts_with("pub(crate) fn ");
    if declares && reports_a_failure && line.contains("-> Error") {
        return true;
    }
    line.contains("map_err") && line.contains("ErrorKind::")
}

const PERMITTED: &[(&str, &str)] = &[
    (
        "the rule itself, which every other site reaches",
        "fn filesystem_kind",
    ),
    (
        "the lock rule, which asks the filesystem rule for the failures it shares",
        "fn lock_failure",
    ),
    (
        "the platform's own path into the rule, which delegates rather than deciding",
        "fn from_errno",
    ),
    (
        "an archive member, which fails as a member and never as a path on a volume",
        "fn io_error",
    ),
    ("the same, for the member a plan names", "fn io_failure"),
    (
        "a response body, which fails as a transfer and never as a path on a volume",
        "fn body_failure",
    ),
    (
        "an entry path a tree cannot represent, which is a name and not a failure the filesystem reported",
        "EntryPath::new",
    ),
    (
        "a socket, which fails as a connection and never as a path on a volume",
        "fn socket_failure",
    ),
    (
        "the compressor refusing the bytes it was handed, which is the codec answering and never a path on a volume",
        "fn codec_failure",
    ),
];

#[test]
fn every_permitted_conversion_states_why_it_is_not_a_filesystem_decision() {
    for (reason, symbol) in PERMITTED {
        assert!(
            !reason.is_empty(),
            "{symbol} is permitted without a reason, which is how the walk stopped catching \
             anything the first time"
        );
    }
}
