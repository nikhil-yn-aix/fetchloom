//! What the store holds after a platform operation on the write path fails.

#![expect(
    clippy::unwrap_used,
    reason = "a failure to build the input is the assertion"
)]

use blake3 as _;
use serde as _;
use serde_json as _;
#[cfg(windows)]
use windows_sys as _;
use zstd as _;

mod support;

use std::io::Write;
use std::path::Path;

use fetchloom_cache::Cache;
use fetchloom_engine::compression::CompressionChoice;
use fetchloom_engine::durability::DurabilityTier;
use fetchloom_engine::error::{Error, Surface, filesystem_failure};
use fetchloom_engine::hashing::hash_bytes;
use fetchloom_engine::partial_key::PartialKey;
use fetchloom_engine::seam::policy::IoMode;
use fetchloom_engine::seam::store::Store;
use fetchloom_engine::verification::VerificationPolicy;
use fetchloom_faults::{FaultyPlatform, Operation};
use fetchloom_platform::NativePlatform;

const WRITE_PATH: [Operation; 7] = [
    Operation::CreateFileExclusive,
    Operation::CreateDirectoryExclusive,
    Operation::Preallocate,
    Operation::Flush,
    Operation::ReleaseWritten,
    Operation::PublishFile,
    Operation::FreeSpace,
];

const INJECTION_POINTS: u64 = 4;

fn refusal(operation: Operation) -> Error {
    filesystem_failure(
        Surface::Cache,
        Path::new("the write path"),
        &std::io::Error::other(format!("{operation:?} was failed on purpose")),
    )
}

fn faulty_cache(root: &Path) -> Cache<FaultyPlatform<NativePlatform>> {
    let work = std::sync::Arc::new(fetchloom_engine::work::WorkCounter::new());
    Cache::open(
        root.join("cache"),
        FaultyPlatform::new(NativePlatform::new(std::sync::Arc::clone(&work))),
        fetchloom_cache::CacheSettings {
            tier: DurabilityTier::Normal,
            policy: VerificationPolicy::Fingerprint,
            io: IoMode::Buffered,
            compression: CompressionChoice::None,
        },
        work,
        support::processor(),
    )
    .unwrap()
}

fn publish(held: &Cache<FaultyPlatform<NativePlatform>>, bytes: &[u8]) -> Result<(), Error> {
    let digest = hash_bytes(bytes);
    let lease = held.lease(PartialKey::of_content(digest))?;
    let mut writer = held.begin(&lease, bytes.len() as u64)?;
    writer
        .write_all(bytes)
        .map_err(|reason| filesystem_failure(Surface::Cache, Path::new("the partial"), &reason))?;
    held.commit(lease, writer)?;
    held.flush_packs()?;
    Ok(())
}

fn store_is_coherent(root: &Path, complaint: &str) {
    assert!(
        support::digests_are_their_bytes(&root.join("cache")),
        "{complaint}"
    );
}

fn sweep(bytes: &[u8], shape: &str) -> Vec<(Operation, u64)> {
    let mut refused = Vec::new();
    for operation in WRITE_PATH {
        for after in 0..INJECTION_POINTS {
            let scratch = tempfile::TempDir::new().unwrap();
            {
                let held = faulty_cache(scratch.path());
                held.platform()
                    .faults()
                    .fail(operation, after, 1, refusal(operation));
                if publish(&held, bytes).is_err() {
                    refused.push((operation, after));
                }
            }
            store_is_coherent(
                scratch.path(),
                &format!(
                    "a {shape} object entered the store unverified when {operation:?} failed on call {after}"
                ),
            );
        }
    }
    refused
}

#[test]
fn a_failed_write_path_operation_never_leaves_an_object_that_is_not_its_own_bytes() {
    let refused = sweep(&support::incompressible(4 << 20, 11), "whole");
    assert!(
        !refused.is_empty(),
        "no injected failure reached the write path, so this proves nothing"
    );
    let reached: std::collections::BTreeSet<Operation> =
        refused.iter().map(|(operation, _)| *operation).collect();
    for operation in [
        Operation::CreateFileExclusive,
        Operation::Preallocate,
        Operation::Flush,
        Operation::PublishFile,
    ] {
        assert!(
            reached.contains(&operation),
            "publishing a whole object never called {operation:?}, so the store no longer publishes the way this test believes"
        );
    }
}

#[test]
fn a_failed_write_path_operation_never_leaves_a_packed_object_that_is_not_its_own_bytes() {
    let refused = sweep(&support::bytes_of(4096, 7), "packed");
    let reached: std::collections::BTreeSet<Operation> =
        refused.iter().map(|(operation, _)| *operation).collect();
    for operation in [
        Operation::CreateFileExclusive,
        Operation::Preallocate,
        Operation::Flush,
    ] {
        assert!(
            reached.contains(&operation),
            "publishing a packed object never called {operation:?}, so the pack no longer writes the way this test believes"
        );
    }
    assert!(
        !reached.contains(&Operation::PublishFile),
        "a packed object was published by a rename of its own, which is what packing exists to avoid"
    );
}

#[test]
fn an_object_a_failed_publish_left_behind_is_not_served_as_that_digest() {
    let bytes = support::incompressible(4 << 20, 23);
    let digest = hash_bytes(&bytes);
    for after in 0..INJECTION_POINTS {
        let scratch = tempfile::TempDir::new().unwrap();
        {
            let held = faulty_cache(scratch.path());
            held.platform().faults().fail(
                Operation::PublishFile,
                after,
                1,
                refusal(Operation::PublishFile),
            );
            let _ = publish(&held, &bytes);
        }
        let reopened = support::open_cache_at(&scratch.path().join("cache")).unwrap();
        if reopened.contains(digest).unwrap() {
            let mut read = Vec::new();
            std::io::copy(&mut reopened.open(digest).unwrap(), &mut read).unwrap();
            assert_eq!(
                hash_bytes(&read),
                digest,
                "a publish that failed at call {after} left bytes served under a digest they do not have"
            );
        }
    }
}

#[test]
fn a_failed_compaction_never_loses_an_object_the_pack_held() {
    let mut refused = 0usize;
    for operation in [
        Operation::CreateFileExclusive,
        Operation::Flush,
        Operation::PublishFile,
    ] {
        for after in 0..INJECTION_POINTS {
            let scratch = tempfile::TempDir::new().unwrap();
            let mut stored = Vec::new();
            {
                let held = faulty_cache(scratch.path());
                for seed in 0..16u64 {
                    let bytes = support::bytes_of(8192, u8::try_from(seed + 1).unwrap());
                    publish(&held, &bytes).unwrap();
                    stored.push(hash_bytes(&bytes));
                }
                held.platform()
                    .faults()
                    .fail(operation, after, 1, refusal(operation));
                if held.compact().is_err() {
                    refused += 1;
                }
            }
            store_is_coherent(
                scratch.path(),
                &format!(
                    "compaction left an object that is not its own bytes when {operation:?} failed on call {after}"
                ),
            );
            let reopened = support::open_cache_at(&scratch.path().join("cache")).unwrap();
            for digest in &stored {
                assert!(
                    reopened.contains(*digest).unwrap(),
                    "compaction interrupted at {operation:?} call {after} lost an object the pack held"
                );
            }
        }
    }
    assert!(
        refused > 0,
        "no injected failure reached compaction, so this proves nothing"
    );
}

const PACKED_ENTRIES: usize = 8;

fn cache_at(root: &Path, tier: DurabilityTier) -> Cache<FaultyPlatform<NativePlatform>> {
    let work = std::sync::Arc::new(fetchloom_engine::work::WorkCounter::new());
    Cache::open(
        root,
        FaultyPlatform::new(NativePlatform::new(std::sync::Arc::clone(&work))),
        fetchloom_cache::CacheSettings {
            tier,
            policy: VerificationPolicy::Fingerprint,
            io: IoMode::Buffered,
            compression: CompressionChoice::None,
        },
        work,
        support::processor(),
    )
    .unwrap()
}

fn flushes_appending(root: &Path, tier: DurabilityTier) -> u64 {
    let held = cache_at(&root.join("cache"), tier);
    let beside = root.join("beside");
    std::fs::create_dir_all(&beside).unwrap();
    let before = held.platform().faults().calls(Operation::Flush);
    for index in 0..PACKED_ENTRIES {
        let bytes = support::incompressible(4096, 1001 + 2 * index as u64);
        let path = beside.join(format!("entry-{index}.bin"));
        std::fs::write(&path, &bytes).unwrap();
        let mut pair = fetchloom_engine::hashing::Pair::new();
        pair.update(held.processor(), &bytes);
        let digests = pair.finish();
        held.adopt(&digests, bytes.len() as u64, &path).unwrap();
    }
    held.flush_packs().unwrap();
    held.platform().faults().calls(Operation::Flush) - before
}

#[test]
fn a_run_pushes_its_pack_once_where_strict_pushes_every_entry_it_appended() {
    let scratch = tempfile::TempDir::new().unwrap();
    let strict = flushes_appending(&scratch.path().join("strict"), DurabilityTier::Strict);
    let normal = flushes_appending(&scratch.path().join("normal"), DurabilityTier::Normal);
    let fast = flushes_appending(&scratch.path().join("fast"), DurabilityTier::Fast);

    assert_eq!(
        strict, PACKED_ENTRIES as u64,
        "strict stopped pushing every entry as it landed, which is the whole of what it costs more \
         for"
    );
    assert_eq!(
        normal, 1,
        "normal pushed the pack once per entry rather than once for the run, which is the cost \
         this batch exists to remove"
    );
    assert_eq!(fast, 0, "fast pushed a pack it promises never to push");
}
