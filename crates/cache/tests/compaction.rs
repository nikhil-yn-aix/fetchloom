//! What `cache compact` may and may not change, and what a dictionary costs
//! when it is damaged.

#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test inputs, where a failure to build one is the assertion"
)]

use std::io::{Read, Seek, SeekFrom, Write};

use fetchloom_engine::compression::CompressionChoice;
use fetchloom_engine::digest::ContentDigest;
use fetchloom_engine::hashing::hash_bytes;
use fetchloom_engine::seam::policy::IoMode;
use fetchloom_engine::verification::VerificationPolicy;

use blake3 as _;
use fetchloom_faults as _;
use serde as _;
use serde_json as _;
#[cfg(windows)]
use windows_sys as _;
use zstd as _;

mod support;

use support::{open_cache_compressed, publish, scratch};

/// Records of the shape a pack holds: small, alike, and individually too short
/// for a frame to find much in.
fn record(seed: u64, length: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(length);
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let words: [&[u8]; 4] = [
        b"{\"dataset\":\"ncep\",\"variable\":\"air\",\"level\":",
        b"},{\"time\":\"2020-01-",
        b"\",\"units\":\"K\",\"value\":",
        b"},{\"station\":\"KDEN\",\"quality\":\"ok\",\"flag\":",
    ];
    while out.len() < length {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.extend_from_slice(words[(state % 4) as usize]);
        out.extend_from_slice((state % 1000).to_string().as_bytes());
    }
    out.truncate(length);
    out
}

fn packed_cache(
    under: &std::path::Path,
) -> fetchloom_cache::Cache<fetchloom_platform::NativePlatform> {
    open_cache_compressed(
        under,
        VerificationPolicy::Fingerprint,
        IoMode::Buffered,
        CompressionChoice::Auto,
    )
    .unwrap()
}

fn fill(
    held: &fetchloom_cache::Cache<fetchloom_platform::NativePlatform>,
    count: u64,
) -> Vec<(ContentDigest, Vec<u8>)> {
    (0..count)
        .map(|seed| {
            let bytes = record(seed, 6000);
            (publish(held, &bytes), bytes)
        })
        .collect()
}

#[test]
fn compaction_trains_a_dictionary_and_every_object_still_reads_back_its_own_bytes() {
    let scratch = scratch();
    let held = packed_cache(scratch.path());
    let written = fill(&held, 64);

    let before = held.compact().unwrap();
    assert_eq!(before.packs, 1, "the objects did not land in one pack");
    assert_eq!(
        before.dictionaries, 1,
        "64 alike objects trained no dictionary"
    );
    assert!(
        before.bytes_after < before.bytes_before,
        "compaction grew the pack, {} to {}",
        before.bytes_before,
        before.bytes_after
    );

    for (digest, bytes) in &written {
        let mut read = Vec::new();
        held.read(*digest).unwrap().read_to_end(&mut read).unwrap();
        assert_eq!(
            &read, bytes,
            "a compacted object did not read back the bytes that went in"
        );
        assert_eq!(
            hash_bytes(&read),
            *digest,
            "a compacted object no longer hashes to the name it is stored under"
        );
    }
}

#[test]
fn a_compacted_pack_states_the_dictionary_its_entries_name() {
    let scratch = scratch();
    let held = packed_cache(scratch.path());
    fill(&held, 64);
    held.compact().unwrap();

    let packs = held.packs().unwrap();
    let dictionary = fetchloom_cache::pack::dictionary_in(&packs[0]).unwrap();
    assert!(
        !dictionary.is_empty(),
        "a compacted pack states no dictionary"
    );
    assert_ne!(
        fetchloom_cache::compress::identifier_of(Some(&dictionary)),
        0,
        "the dictionary a compacted pack states carries no identifier"
    );
}

#[test]
fn a_pack_with_too_few_objects_trains_nothing_and_says_so() {
    let scratch = scratch();
    let held = packed_cache(scratch.path());
    let written = fill(&held, 3);
    let _ = held.take_degradations();

    let report = held.compact().unwrap();
    assert_eq!(
        report.dictionaries, 0,
        "three objects trained a dictionary zstd cannot train"
    );
    let degraded = held.take_degradations();
    assert!(
        degraded.iter().any(|held| held.reason.contains("objects")),
        "compaction trained nothing and never said why: {degraded:?}"
    );

    for (digest, bytes) in &written {
        let mut read = Vec::new();
        held.read(*digest).unwrap().read_to_end(&mut read).unwrap();
        assert_eq!(&read, bytes, "an uncompacted object stopped reading back");
    }
}

#[test]
fn compaction_under_compress_none_trains_nothing_and_says_so() {
    let scratch = scratch();
    let held = open_cache_compressed(
        scratch.path(),
        VerificationPolicy::Fingerprint,
        IoMode::Buffered,
        CompressionChoice::None,
    )
    .unwrap();
    let written = fill(&held, 64);
    let _ = held.take_degradations();

    let report = held.compact().unwrap();
    assert_eq!(
        report.dictionaries, 0,
        "a run told never to compress got a compressed pack from a maintenance command"
    );
    let degraded = held.take_degradations();
    assert!(
        degraded
            .iter()
            .any(|held| held.reason.contains("compress none")),
        "compaction ignored compress none without saying so: {degraded:?}"
    );

    for (digest, bytes) in &written {
        let mut read = Vec::new();
        held.read(*digest).unwrap().read_to_end(&mut read).unwrap();
        assert_eq!(&read, bytes, "an object stopped reading back under none");
    }
}

#[test]
fn a_damaged_dictionary_names_its_pack_and_states_what_to_run() {
    let scratch = scratch();
    let held = packed_cache(scratch.path());
    let written = fill(&held, 64);
    held.compact().unwrap();

    let packs = held.packs().unwrap();
    let pack = packs[0].clone();
    let dictionary = fetchloom_cache::pack::dictionary_in(&pack).unwrap();
    assert!(!dictionary.is_empty(), "nothing to damage");

    let mut file = std::fs::OpenOptions::new().write(true).open(&pack).unwrap();
    file.seek(SeekFrom::Start(40 + (dictionary.len() as u64) / 2))
        .unwrap();
    file.write_all(&[0xff; 512]).unwrap();
    drop(file);

    let reopened = packed_cache(scratch.path());
    let mut read = Vec::new();
    let refused = match reopened.read(written[0].0) {
        Ok(mut held) => match held.read_to_end(&mut read) {
            Ok(_) => {
                assert_ne!(
                    read, written[0].1,
                    "a damaged dictionary handed back the bytes as if nothing had happened"
                );
                assert_ne!(
                    hash_bytes(&read),
                    written[0].0,
                    "a damaged dictionary produced bytes that still hash to the name"
                );
                panic!(
                    "a damaged dictionary produced {} bytes instead of refusing",
                    read.len()
                );
            }
            Err(reason) => reason.to_string(),
        },
        Err(refused) => refused.next_action().to_owned(),
    };
    let action = refused;
    assert!(
        action.contains(&pack.display().to_string()) || action.contains("pack"),
        "the refusal does not name the pack every object was lost with: {action}"
    );
    assert!(
        action.contains("cache") || action.contains("fetch"),
        "the refusal does not state what to run: {action}"
    );
}
