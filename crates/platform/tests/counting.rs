//! Contract tests over the file operation counter.
//!
//! The contract says file operations counts every file or directory the run
//! created, every rename it performed, and every flush it issued, and that it
//! is counted where the platform performs the operation.

#![expect(
    clippy::unwrap_used,
    reason = "test setup, where a failure to build the input is the assertion"
)]

#[cfg(windows)]
use windows_sys as _;

#[cfg(unix)]
use libc as _;
#[cfg(unix)]
use rustix as _;

use std::sync::Arc;

use fetchloom_engine::durability::DurabilityTier;
use fetchloom_engine::seam::platform::Platform;
use fetchloom_engine::work::WorkCounter;
use fetchloom_platform::NativePlatform;

fn counting() -> (Arc<WorkCounter>, NativePlatform) {
    let work = Arc::new(WorkCounter::new());
    let platform = NativePlatform::new(Arc::clone(&work));
    (work, platform)
}

#[test]
fn creating_a_file_counts_one_operation() {
    let (work, platform) = counting();
    let root = tempfile::tempdir().unwrap();
    platform
        .create_file_exclusive(&root.path().join("one"))
        .unwrap();
    assert_eq!(work.taken().file_operations, 1);
}

#[test]
fn creating_a_directory_counts_one_operation() {
    let (work, platform) = counting();
    let root = tempfile::tempdir().unwrap();
    platform
        .create_directory_exclusive(&root.path().join("tree"))
        .unwrap();
    assert_eq!(work.taken().file_operations, 1);
}

#[test]
fn a_flush_counts_one_operation_and_a_publication_counts_its_rename() {
    let (work, platform) = counting();
    let root = tempfile::tempdir().unwrap();
    let file = platform
        .create_file_exclusive(&root.path().join("written"))
        .unwrap();
    let after_create = work.taken().file_operations;
    platform.flush(&file, DurabilityTier::Normal).unwrap();
    let after_flush = work.taken().file_operations;
    drop(file);
    platform
        .publish_file(
            &root.path().join("written"),
            &root.path().join("published"),
            DurabilityTier::Normal,
        )
        .unwrap();
    let after_publish = work.taken().file_operations;

    assert_eq!(after_create, 1, "the create was not counted once");
    assert_eq!(after_flush, 2, "the flush was not counted once");
    assert!(
        after_publish > after_flush,
        "the rename was not counted at all"
    );
}

#[test]
fn a_run_that_touches_no_file_counts_no_operation() {
    let (work, platform) = counting();
    let root = tempfile::tempdir().unwrap();
    platform.volume_id(root.path()).unwrap();
    platform.fingerprint(root.path()).unwrap();
    assert_eq!(work.taken().file_operations, 0);
}

#[test]
fn a_symlink_that_can_be_created_counts_one_operation() {
    let (work, platform) = counting();
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("target"), b"bytes").unwrap();
    if platform
        .create_symlink(b"target", &root.path().join("link"))
        .is_err()
    {
        return;
    }
    assert_eq!(work.taken().file_operations, 1);
}

#[test]
fn placing_bytes_at_another_path_counts_one_operation_whichever_mechanism_ran() {
    let (work, platform) = counting();
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("from"), b"bytes").unwrap();
    platform
        .clone_or_copy(&root.path().join("from"), &root.path().join("to"))
        .unwrap();
    assert_eq!(work.taken().file_operations, 1);
}
