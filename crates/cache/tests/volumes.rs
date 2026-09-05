//! Contract tests that need a particular kind of volume underneath them.

#![expect(
    clippy::unwrap_used,
    reason = "test setup, where a failure to build the input is the assertion"
)]

use blake3 as _;
use serde as _;
use serde_json as _;
#[cfg(windows)]
use windows_sys as _;
use zstd as _;

mod support;

use std::io::Write;

use fetchloom_cache::Cache;
use fetchloom_engine::durability::DurabilityTier;
use fetchloom_engine::error::{ErrorKind, lock_failure};
use fetchloom_engine::hashing::hash_bytes;
use fetchloom_engine::partial_key::PartialKey;
use fetchloom_engine::seam::policy::IoMode;
use fetchloom_engine::seam::store::Store;
use fetchloom_engine::verification::VerificationPolicy;
use fetchloom_faults::{FaultyPlatform, Operation};
use fetchloom_platform::NativePlatform;

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
            let lease = match held.lease(PartialKey::of_content(digest)) {
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
            support::digests_are_their_bytes(held.layout().root()),
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
fn prune_skips_a_pack_another_user_wrote_and_reports_it() {
    let Some(other) = support::another_owner() else {
        return;
    };
    let (_scratch, held) = support::cache();
    let large = usize::try_from(fetchloom_engine::limits::PACK_THRESHOLD).unwrap() + 1;
    let mine = support::publish(&held, &bytes_of(large, 3));
    let theirs = support::publish(&held, &bytes_of(1024, 4));
    let pack = held.placement(theirs).unwrap().container().to_path_buf();
    support::give_away(&pack, &other);

    held.prune(std::time::Duration::ZERO).unwrap();
    let report = held.prune(std::time::Duration::ZERO).unwrap();

    assert!(
        held.contains(theirs).unwrap(),
        "prune removed an object out of a pack another user wrote"
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

#[test]
fn a_volume_whose_locks_are_refused_is_refused_as_a_cache() {
    let scratch = tempfile::TempDir::new().unwrap();
    let work = std::sync::Arc::new(fetchloom_engine::work::WorkCounter::new());
    let platform = FaultyPlatform::new(NativePlatform::new(std::sync::Arc::clone(&work)));
    platform.faults().fail(
        Operation::TryLock,
        0,
        1,
        lock_failure(scratch.path(), &std::io::Error::from_raw_os_error(ENOLCK)),
    );
    let refused = Cache::open(
        scratch.path().join("cache"),
        platform,
        fetchloom_cache::CacheSettings {
            tier: DurabilityTier::Fast,
            policy: VerificationPolicy::Fingerprint,
            io: IoMode::Buffered,
            compression: fetchloom_engine::compression::CompressionChoice::Auto,
        },
        std::sync::Arc::clone(&work),
        std::sync::Arc::new(
            fetchloom_engine::pool::Processor::new(
                fetchloom_engine::threads::ThreadBudget::resolve(std::num::NonZeroUsize::MIN, None),
            )
            .unwrap(),
        ),
    )
    .unwrap_err();

    assert_eq!(
        refused.kind(),
        ErrorKind::CacheLockingUnsupported,
        "a volume that refused the probe lock was accepted as a cache: {refused}"
    );
}

#[cfg(unix)]
const ENOLCK: i32 = 37;

#[cfg(windows)]
const ENOLCK: i32 = 1;
