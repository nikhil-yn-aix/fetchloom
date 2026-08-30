//! Contract tests over publication, durability, and what a killed run leaves.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test setup, where a failure to build the input is the assertion"
)]

#[cfg(windows)]
use windows_sys as _;

#[cfg(unix)]
use libc as _;
#[cfg(unix)]
use rustix as _;

mod support;

use std::path::Path;
use std::process::Command;

use fetchloom_engine::durability::DurabilityTier;
use fetchloom_engine::error::ErrorKind;
use fetchloom_engine::seam::platform::Platform;
use fetchloom_platform::NativePlatform;

const TIERS: [DurabilityTier; 3] = [
    DurabilityTier::Strict,
    DurabilityTier::Normal,
    DurabilityTier::Fast,
];

/// The variable the killed child reads to know where to publish.
const KILL_DIRECTORY: &str = "FETCHLOOM_TEST_KILL_DIRECTORY";

/// The variable naming how far the killed child gets before it dies.
const KILL_STAGE: &str = "FETCHLOOM_TEST_KILL_STAGE";

/// The bytes a complete object holds.
const WHOLE_OBJECT: &[u8] = b"the whole object";

#[test]
fn a_file_is_published_by_rename_under_every_tier() {
    for tier in TIERS {
        let scratch = support::scratch();
        let platform = NativePlatform::new(std::sync::Arc::new(
            fetchloom_engine::work::WorkCounter::new(),
        ));
        let partial = scratch.path().join("partial");
        let object = scratch.path().join("object");
        support::write_file(&partial, b"published bytes");

        platform.publish_file(&partial, &object, tier).unwrap();

        assert!(object.is_file(), "{tier:?} did not publish the object");
        assert!(!partial.exists(), "{tier:?} left the partial file behind");
        assert_eq!(std::fs::read(&object).unwrap(), b"published bytes");
    }
}

#[test]
fn publishing_replaces_an_existing_object_under_every_tier() {
    for tier in TIERS {
        let scratch = support::scratch();
        let platform = NativePlatform::new(std::sync::Arc::new(
            fetchloom_engine::work::WorkCounter::new(),
        ));
        let partial = scratch.path().join("partial");
        let object = scratch.path().join("object");
        support::write_file(&object, b"old");
        support::write_file(&partial, b"new");

        platform.publish_file(&partial, &object, tier).unwrap();
        assert_eq!(std::fs::read(&object).unwrap(), b"new", "{tier:?}");
    }
}

#[test]
fn a_flush_completes_under_every_tier_and_the_bytes_survive_reopen() {
    for tier in TIERS {
        let scratch = support::scratch();
        let platform = NativePlatform::new(std::sync::Arc::new(
            fetchloom_engine::work::WorkCounter::new(),
        ));
        let path = scratch.path().join("flushed");
        let file = platform.create_file_exclusive(&path).unwrap();
        {
            use std::io::Write;
            let mut handle = &file;
            handle.write_all(b"durable bytes").unwrap();
        }
        platform.flush(&file, tier).unwrap();
        drop(file);
        assert_eq!(std::fs::read(&path).unwrap(), b"durable bytes", "{tier:?}");
    }
}

#[test]
fn preallocation_reserves_the_whole_length_before_anything_is_written() {
    let scratch = support::scratch();
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    let path = scratch.path().join("reserved");
    let file = platform.create_file_exclusive(&path).unwrap();

    platform.preallocate(&file, 1_048_576).unwrap();
    drop(file);

    assert_eq!(
        std::fs::metadata(&path).unwrap().len(),
        1_048_576,
        "the reserved length is not the file's length"
    );
}

#[test]
fn a_cross_volume_publish_is_refused_and_never_becomes_a_copy() {
    let Some(other) = support::other_volume_scratch() else {
        eprintln!(
            "skipped: a cross-volume publish needs two writable volumes and this machine has one"
        );
        return;
    };
    let scratch = support::scratch();
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));

    let partial = scratch.path().join("partial");
    let object = other.path().join("object");
    support::write_file(&partial, b"bytes that must not cross");

    let error = platform
        .publish_file(&partial, &object, DurabilityTier::Normal)
        .expect_err("a cross-volume publish must fail");

    assert_eq!(error.kind(), ErrorKind::DestinationCrossVolume);
    assert!(
        !object.exists(),
        "a cross-volume publish was completed as a copy"
    );
    assert!(
        partial.is_file(),
        "the source was consumed by a publish that failed"
    );
}

#[test]
fn two_volumes_report_different_identifiers() {
    let Some(other) = support::other_volume_scratch() else {
        eprintln!("skipped: comparing volumes needs two writable volumes and this machine has one");
        return;
    };
    let scratch = support::scratch();
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));

    let one = platform.volume_id(scratch.path()).unwrap();
    let two = platform.volume_id(other.path()).unwrap();
    assert_ne!(
        one, two,
        "two volumes reported the same identifier, so no cross-volume check can work"
    );
}

#[test]
fn a_directory_is_published_onto_a_destination_that_does_not_exist() {
    let scratch = support::scratch();
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    let staging = scratch.path().join("staging");
    let destination = scratch.path().join("destination");
    std::fs::create_dir_all(staging.join("nested")).unwrap();
    support::write_file(&staging.join("nested").join("file"), b"content");

    platform
        .publish_directory(&staging, &destination, DurabilityTier::Normal)
        .unwrap();

    assert!(destination.join("nested").join("file").is_file());
    assert!(!staging.exists(), "staging survived publication");
}

#[test]
fn publishing_a_directory_over_an_existing_one_leaves_no_sibling_behind() {
    let scratch = support::scratch();
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    let staging = scratch.path().join("staging");
    let destination = scratch.path().join("destination");
    std::fs::create_dir_all(&staging).unwrap();
    std::fs::create_dir_all(&destination).unwrap();
    support::write_file(&staging.join("new"), b"new");
    support::write_file(&destination.join("old"), b"old");

    platform
        .publish_directory(&staging, &destination, DurabilityTier::Normal)
        .unwrap();

    assert!(
        destination.join("new").is_file(),
        "the new tree is not there"
    );
    assert!(!destination.join("old").exists(), "the old tree survived");
    assert_eq!(
        support::names(scratch.path()),
        vec!["destination".to_owned()],
        "publication left a sibling behind"
    );
}

#[test]
fn killing_a_process_at_any_stage_of_publication_leaves_no_partial_object() {
    let stages = ["created", "written", "flushed", "renamed"];
    let mut published = 0;

    for stage in stages {
        let scratch = support::scratch();
        let objects = scratch.path().join("objects");
        std::fs::create_dir_all(&objects).unwrap();

        let status = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "publish_then_abort", "--ignored", "--nocapture"])
            .env(KILL_DIRECTORY, scratch.path())
            .env(KILL_STAGE, stage)
            .status()
            .unwrap();
        assert!(
            !status.success(),
            "the child was supposed to die at the {stage} stage"
        );

        let names = support::names(&objects);
        for name in &names {
            let bytes = std::fs::read(objects.join(name)).unwrap();
            assert_eq!(
                bytes, WHOLE_OBJECT,
                "a torn object appeared in objects as {name} after dying at the {stage} stage"
            );
        }
        assert!(
            names.len() <= 1,
            "dying at the {stage} stage left more than one object behind: {names:?}"
        );
        published += names.len();
    }

    assert!(
        published > 0,
        "no stage ever reached publication, so this test proved nothing"
    );
}

#[test]
#[ignore = "run only as the killed child of the publication test"]
fn publish_then_abort() {
    let Ok(directory) = std::env::var(KILL_DIRECTORY) else {
        return;
    };
    let stage = std::env::var(KILL_STAGE).unwrap_or_default();
    let root = Path::new(&directory);
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    let partial = root.join("partial-object");
    let objects = root.join("objects");

    let file = platform.create_file_exclusive(&partial).unwrap();
    if stage == "created" {
        std::process::abort();
    }
    {
        use std::io::Write;
        let mut handle = &file;
        handle.write_all(WHOLE_OBJECT).unwrap();
    }
    if stage == "written" {
        std::process::abort();
    }
    platform.flush(&file, DurabilityTier::Strict).unwrap();
    drop(file);
    if stage == "flushed" {
        std::process::abort();
    }

    platform
        .publish_file(&partial, &objects.join("object"), DurabilityTier::Strict)
        .unwrap();
    std::process::abort();
}
