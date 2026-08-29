//! Contract tests that need a particular kind of volume underneath them.
//!
//! Each one runs against the volumes the environment names, which continuous
//! integration builds before the suite runs. A runner that promised a volume
//! and did not build it fails rather than passing quietly.

#![expect(
    clippy::unwrap_used,
    reason = "test setup, where a failure to build the input is the assertion"
)]

use blake3 as _;
use serde as _;
use serde_json as _;

mod support;

use std::io::Write;

use fetchloom_engine::error::ErrorKind;
use fetchloom_engine::hashing::hash_bytes;
use fetchloom_engine::seam::store::Store;

use support::{Property, bytes_of, scratch_on, volumes};

#[test]
fn a_cache_on_a_network_volume_is_refused_as_unable_to_lock() {
    for scratch in scratch_on(Property::Network) {
        let refused = support::open_cache(scratch.path()).unwrap_err();
        assert_eq!(
            refused.kind(),
            ErrorKind::CacheLockingUnsupported,
            "{} is reached over a network and was accepted as a cache",
            scratch.path().display()
        );
    }
}

#[test]
fn a_cache_that_cannot_be_written_is_refused_rather_than_half_opened() {
    for volume in volumes(Property::ReadOnly) {
        let refused = support::open_cache(&volume).unwrap_err();
        assert_eq!(
            refused.kind(),
            ErrorKind::CacheCorrupt,
            "{} cannot be written and opening a cache there did not say so",
            volume.display()
        );
    }
}

#[test]
fn a_volume_with_no_room_left_fails_the_transfer_rather_than_the_cache() {
    for scratch in scratch_on(Property::Small) {
        let held = support::cache_in(scratch.path());
        let bytes = bytes_of(1 << 20, 17);
        let mut wrote = 0u32;
        let outcome = loop {
            let digest = hash_bytes(&bytes_of(1 << 20, u8::try_from(wrote % 251).unwrap_or(1)));
            let lease = match held.lease(digest) {
                Ok(lease) => lease,
                Err(refused) => break refused,
            };
            let began = held.begin(&lease, bytes.len() as u64);
            match began {
                Err(refused) => break refused,
                Ok(mut writer) => {
                    let filled = writer
                        .write_all(&bytes_of(1 << 20, u8::try_from(wrote % 251).unwrap_or(1)));
                    if filled.is_err() {
                        break held.commit(lease, writer).unwrap_err();
                    }
                    let _ = held.commit(lease, writer);
                }
            }
            wrote += 1;
            assert!(
                wrote < 4096,
                "{} never ran out of room, so it is not a small volume",
                scratch.path().display()
            );
        };
        assert_eq!(
            outcome.kind(),
            ErrorKind::ResourceDisk,
            "running out of room reported {} rather than a disk failure",
            outcome.kind().label()
        );
        assert!(
            support::digests_are_their_bytes(held.layout()),
            "running out of room left an object that does not hash to its name"
        );
    }
}

#[test]
fn a_cache_split_across_volumes_is_refused_when_it_is_opened() {
    let Some(second) = volumes(Property::Second).into_iter().next() else {
        return;
    };
    let scratch = tempfile::TempDir::new().unwrap();
    let root = scratch.path().join("cache");
    drop(support::cache_in(scratch.path()));

    let elsewhere = tempfile::TempDir::new_in(&second).unwrap();
    std::fs::remove_dir_all(root.join("staging")).unwrap();
    if !support::link_directory(elsewhere.path(), &root.join("staging")) {
        return;
    }

    let refused = support::open_cache(scratch.path()).unwrap_err();
    assert_eq!(
        refused.kind(),
        ErrorKind::CacheCrossVolume,
        "a cache spanning two volumes was opened"
    );
}

#[test]
fn prune_skips_an_object_another_user_created_and_reports_it() {
    let Some(other) = support::another_owner() else {
        return;
    };
    let (_scratch, held) = support::cache();
    let mine = support::publish(&held, &bytes_of(1024, 3));
    let theirs = support::publish(&held, &bytes_of(1024, 4));
    support::give_away(&held.layout().object(theirs), &other);

    held.prune(std::time::Duration::ZERO).unwrap();
    let report = held.prune(std::time::Duration::ZERO).unwrap();

    assert!(
        held.contains(theirs).unwrap(),
        "prune removed an object another user created"
    );
    assert!(
        !held.contains(mine).unwrap(),
        "prune kept an object it owns"
    );
    assert_eq!(
        report.skipped_other_owner, 1,
        "prune did not report what it skipped"
    );
}
