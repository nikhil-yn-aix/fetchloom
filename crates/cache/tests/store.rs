//! Contract tests over the invariants in contracts.md Cache.
//!
//! Every assertion here is a sentence from that section: an entry in `objects/`
//! has been verified and there is no other way for a file to appear there,
//! publication is a rename, a format mismatch fails every operation, and prune
//! never removes what is pinned or leased.

#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test setup, where a failure to build the input is the assertion"
)]

use blake3 as _;
use serde as _;
use serde_json as _;

mod support;

use std::io::{Read, Write};
use std::time::Duration;

use fetchloom_engine::error::ErrorKind;
use fetchloom_engine::hashing::hash_bytes;
use fetchloom_engine::seam::platform::Platform;
use fetchloom_engine::seam::store::Store;

use support::{bytes_of, cache, cache_in};

#[test]
fn a_cache_writes_every_directory_the_contract_names() {
    let scratch = tempfile::TempDir::new().unwrap();
    let held = cache_in(scratch.path());
    for name in [
        "objects", "outboard", "partial", "staging", "meta", "locks", "pins",
    ] {
        assert!(
            held.layout().root().join(name).is_dir(),
            "the cache holds no {name} directory"
        );
    }
    assert!(
        held.layout().format().is_file(),
        "no format file was written"
    );
}

#[test]
fn an_object_appears_only_when_a_commit_completes() {
    let (_scratch, held) = cache();
    let bytes = bytes_of(4096, 7);
    let digest = hash_bytes(&bytes);

    let lease = held.lease(digest).unwrap();
    let mut writer = held.begin(&lease, bytes.len() as u64).unwrap();
    writer.write_all(&bytes).unwrap();

    assert!(
        !held.contains(digest).unwrap(),
        "an object was readable before it was committed"
    );
    assert!(
        held.layout().partial_of(digest).is_file(),
        "the bytes were written somewhere other than the partial directory"
    );

    held.commit(lease, writer).unwrap();

    assert!(
        held.contains(digest).unwrap(),
        "a committed object is absent"
    );
    assert!(
        !held.layout().partial_of(digest).exists(),
        "the partial survived the publication"
    );
}

#[test]
fn a_committed_object_reads_back_the_bytes_that_were_written() {
    let (_scratch, held) = cache();
    let bytes = bytes_of(1 << 16, 31);
    let digest = hash_bytes(&bytes);

    let lease = held.lease(digest).unwrap();
    let mut writer = held.begin(&lease, bytes.len() as u64).unwrap();
    writer.write_all(&bytes).unwrap();
    held.commit(lease, writer).unwrap();

    let mut found = Vec::new();
    held.open(digest).unwrap().read_to_end(&mut found).unwrap();
    assert_eq!(found, bytes, "the object read back different bytes");
}

#[test]
fn bytes_that_do_not_hash_to_the_digest_are_refused_and_publish_nothing() {
    let (_scratch, held) = cache();
    let claimed = hash_bytes(b"what was asked for");
    let bytes = b"what arrived instead";

    let lease = held.lease(claimed).unwrap();
    let mut writer = held.begin(&lease, bytes.len() as u64).unwrap();
    writer.write_all(bytes).unwrap();
    let refused = held.commit(lease, writer).unwrap_err();

    assert_eq!(refused.kind(), ErrorKind::IntegrityMismatch);
    assert!(
        !held.contains(claimed).unwrap(),
        "an object that failed verification appeared in the object directory"
    );
    assert!(
        !held.layout().partial_of(claimed).exists(),
        "the partial of a refused object was left behind"
    );
}

#[test]
fn an_abandoned_write_leaves_the_object_directory_empty() {
    let (_scratch, held) = cache();
    let bytes = bytes_of(2048, 3);
    let digest = hash_bytes(&bytes);

    let lease = held.lease(digest).unwrap();
    let mut writer = held.begin(&lease, bytes.len() as u64).unwrap();
    writer.write_all(&bytes).unwrap();
    drop(writer);
    drop(lease);

    assert!(
        held.list().unwrap().is_empty(),
        "an object appeared without a commit"
    );
}

#[test]
fn a_format_written_by_another_build_fails_every_operation() {
    let scratch = tempfile::TempDir::new().unwrap();
    drop(cache_in(scratch.path()));
    std::fs::write(
        scratch.path().join("cache").join("format"),
        "blake3:0000000000000000000000000000000000000000000000000000000000000000\n",
    )
    .unwrap();

    let refused = support::open_cache(scratch.path()).unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::CacheFormatMismatch);
    assert!(
        refused.next_action().contains("cache clear"),
        "the message does not say to clear the cache: {}",
        refused.next_action()
    );
}

#[test]
fn a_second_lease_on_one_digest_is_refused_while_the_first_is_held() {
    let (_scratch, held) = cache();
    let digest = hash_bytes(b"one digest");

    let first = held.lease(digest).unwrap();
    let taken = held
        .platform()
        .try_lock(&held.layout().lock_of(digest))
        .unwrap();
    assert!(
        taken.is_none(),
        "a second writer took a digest another writer was holding"
    );
    drop(first);
}

#[test]
fn a_pinned_object_survives_a_prune_that_removes_everything_else() {
    let (_scratch, held) = cache();
    let pinned = support::publish(&held, &bytes_of(1024, 1));
    let loose = support::publish(&held, &bytes_of(1024, 2));
    held.pin(pinned).unwrap();

    held.prune(Duration::ZERO).unwrap();
    let report = held.prune(Duration::ZERO).unwrap();

    assert!(held.contains(pinned).unwrap(), "a pinned object was pruned");
    assert!(
        !held.contains(loose).unwrap(),
        "an unreferenced object survived"
    );
    assert_eq!(report.removed, 1);
}

#[test]
fn a_leased_object_survives_a_prune() {
    let (_scratch, held) = cache();
    let digest = support::publish(&held, &bytes_of(1024, 9));

    let reader = held.open(digest).unwrap();
    held.prune(Duration::ZERO).unwrap();
    held.prune(Duration::ZERO).unwrap();
    assert!(
        held.contains(digest).unwrap(),
        "an object a reader was holding was pruned"
    );

    drop(reader);
    held.prune(Duration::ZERO).unwrap();
    held.prune(Duration::ZERO).unwrap();
    assert!(
        !held.contains(digest).unwrap(),
        "an object nothing holds survived a sweep"
    );
}

#[test]
fn a_prune_marks_before_it_sweeps() {
    let (_scratch, held) = cache();
    let digest = support::publish(&held, &bytes_of(512, 5));

    let first = held.prune(Duration::from_secs(60)).unwrap();
    assert_eq!(
        first.removed, 0,
        "an object was swept in the run that marked it"
    );
    assert!(held.contains(digest).unwrap());
    assert!(
        held.layout().mark_of(digest).is_file(),
        "the first run left no mark"
    );

    let second = held.prune(Duration::from_secs(60)).unwrap();
    assert_eq!(
        second.removed, 0,
        "an object was swept before its grace period ended"
    );
    assert!(held.contains(digest).unwrap());
}

#[test]
fn an_unpinned_object_is_pruned_again() {
    let (_scratch, held) = cache();
    let digest = support::publish(&held, &bytes_of(256, 11));
    held.pin(digest).unwrap();
    held.prune(Duration::ZERO).unwrap();
    held.prune(Duration::ZERO).unwrap();
    assert!(held.contains(digest).unwrap());

    held.unpin(digest).unwrap();
    held.prune(Duration::ZERO).unwrap();
    held.prune(Duration::ZERO).unwrap();
    assert!(
        !held.contains(digest).unwrap(),
        "an unpinned object survived"
    );
}

#[test]
fn pinning_an_object_the_cache_does_not_hold_is_refused() {
    let (_scratch, held) = cache();
    let refused = held.pin(hash_bytes(b"never fetched")).unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::CacheCorrupt);
}

#[test]
fn status_counts_what_the_cache_holds() {
    let (_scratch, held) = cache();
    let first = support::publish(&held, &bytes_of(1000, 1));
    support::publish(&held, &bytes_of(2000, 2));
    held.pin(first).unwrap();

    let found = held.status().unwrap();
    assert_eq!(found.objects, 2);
    assert_eq!(found.bytes, 3000);
    assert_eq!(found.pins, 1);
    assert_eq!(found.partials, 0);
}

#[test]
fn an_object_changed_on_disk_is_refused_by_the_default_policy() {
    let (scratch, held) = cache();
    let digest = support::publish(&held, &bytes_of(4096, 13));
    let object = held.layout().object(digest);

    support::make_writable(&object);
    std::fs::write(&object, bytes_of(4096, 14)).unwrap();

    let reopened = support::open_cache(scratch.path()).unwrap();
    let refused = reopened.open(digest).unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::CacheCorrupt);
}

#[test]
fn verify_moves_a_damaged_object_to_quarantine_and_never_serves_it_again() {
    let (scratch, held) = cache();
    let digest = support::publish(&held, &bytes_of(4096, 19));
    let object = held.layout().object(digest);

    support::make_writable(&object);
    std::fs::write(&object, bytes_of(4096, 20)).unwrap();

    let reopened = support::open_cache(scratch.path()).unwrap();
    let report = fetchloom_cache::verify::run(&reopened).unwrap();

    assert_eq!(report.quarantined, vec![digest.to_string()]);
    assert_eq!(report.verified, 0);
    assert!(
        !reopened.contains(digest).unwrap(),
        "a damaged object was left where a hit would serve it"
    );
    assert!(
        reopened.layout().quarantined(digest).is_file(),
        "a damaged object was not kept for diagnosis"
    );
    assert_eq!(reopened.status().unwrap().quarantined, 1);
}

#[test]
fn verify_reports_an_object_that_is_still_its_digest() {
    let (_scratch, held) = cache();
    support::publish(&held, &bytes_of(2048, 21));
    let report = fetchloom_cache::verify::run(&held).unwrap();
    assert_eq!(report.verified, 1);
    assert!(report.quarantined.is_empty());
}

#[test]
fn prune_removes_a_quarantined_object() {
    let (scratch, held) = cache();
    let digest = support::publish(&held, &bytes_of(4096, 23));
    let object = held.layout().object(digest);
    support::make_writable(&object);
    std::fs::write(&object, bytes_of(4096, 24)).unwrap();

    let reopened = support::open_cache(scratch.path()).unwrap();
    fetchloom_cache::verify::run(&reopened).unwrap();
    let report = reopened.prune(Duration::ZERO).unwrap();

    assert_eq!(report.quarantined_removed, 1);
    assert!(
        !reopened.layout().quarantined(digest).exists(),
        "prune left a quarantined object behind"
    );
}
