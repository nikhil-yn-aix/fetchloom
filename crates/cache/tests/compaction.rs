//! What `cache compact` may and may not change, and what a dictionary costs
//! when it is damaged.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test inputs, where a failure to build one is the assertion"
)]

use std::io::{Read, Seek, SeekFrom, Write};

use fetchloom_engine::compression::CompressionChoice;
use fetchloom_engine::digest::ContentDigest;
use fetchloom_engine::error::ErrorKind;
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

use fetchloom_engine::seam::store::Store;
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
fn a_damaged_dictionary_is_refused_by_its_own_digest_and_names_its_pack() {
    let scratch = scratch();
    let held = packed_cache(scratch.path());
    let written = fill(&held, 64);
    held.compact().unwrap();

    let packs = held.packs().unwrap();
    let pack = packs[0].clone();
    let dictionary = fetchloom_cache::pack::dictionary_in(&pack).unwrap();
    assert!(!dictionary.is_empty(), "nothing to damage");

    let mut read = Vec::new();
    held.read(written[0].0)
        .unwrap()
        .read_to_end(&mut read)
        .unwrap();
    assert_eq!(
        read, written[0].1,
        "the pack does not read back before it is damaged, so damaging it proves nothing"
    );
    drop(held);

    // One byte, three quarters of the way into the dictionary's own content,
    // so the zstd magic and the dictionary identifier it is chosen by both
    // survive. What refuses this is the recorded digest and nothing else.
    let at = 40 + (dictionary.len() as u64) * 3 / 4;
    let mut file = std::fs::OpenOptions::new().write(true).open(&pack).unwrap();
    file.seek(SeekFrom::Start(at)).unwrap();
    file.write_all(&[dictionary[dictionary.len() * 3 / 4] ^ 0x01])
        .unwrap();
    drop(file);

    assert_eq!(
        fetchloom_cache::pack::dictionary_in(&pack)
            .expect_err("a dictionary that does not hash to what was recorded was handed back")
            .kind(),
        ErrorKind::CacheCorrupt
    );

    let reopened = packed_cache(scratch.path());
    let refused = reopened
        .read(written[0].0)
        .expect_err("a damaged dictionary served an object rather than refusing");

    assert_eq!(refused.kind(), ErrorKind::CacheCorrupt);
    let action = refused.next_action().to_owned();
    assert!(
        action.contains("does not hash to what the pack recorded"),
        "the refusal does not say the dictionary failed its own digest, so it is indistinguishable from the decompressor failing: {action}"
    );
    assert!(
        action.contains(&pack.display().to_string()),
        "the refusal does not name the pack every object was lost with: {action}"
    );
    assert!(
        action.contains("cache") || action.contains("fetch"),
        "the refusal does not state what to run: {action}"
    );
}

/// contracts.md:295 — an entry whose length runs past the end of the pack was
/// cut short by a crash and is not one the cache holds. Every truncation offset
/// inside the last entry must read back as a prefix of what was written, and
/// never as a short object under a digest it does not have.
#[test]
fn a_pack_cut_short_holds_the_entries_before_the_cut_and_never_the_cut_one() {
    let scratch = scratch();
    let held = packed_cache(scratch.path());
    let written = fill(&held, 8);
    let packs = held.packs().unwrap();
    assert_eq!(packs.len(), 1, "the objects did not land in one pack");
    let whole = std::fs::read(&packs[0]).unwrap();
    let entries = held.entries_in(&packs[0]).unwrap();
    assert_eq!(
        entries.len(),
        written.len(),
        "the pack holds a different set"
    );
    drop(held);

    let last = entries
        .iter()
        .map(|(_, entry)| entry.offset + entry.length)
        .max()
        .unwrap();
    let first_of_last = entries.iter().map(|(_, entry)| entry.offset).max().unwrap();
    assert!(
        usize::try_from(last).unwrap() <= whole.len() && first_of_last < last,
        "the last entry spans {first_of_last}..{last} of a {} byte pack",
        whole.len()
    );

    let (cut_digest, _) = entries
        .iter()
        .max_by_key(|(_, entry)| entry.offset)
        .unwrap();
    let cut_digest = *cut_digest;

    let whole_again = support::scratch();
    let control = packed_cache(whole_again.path());
    std::fs::create_dir_all(control.layout().packs()).unwrap();
    let control_pack = control.layout().packs().join(packs[0].file_name().unwrap());
    std::fs::write(&control_pack, &whole).unwrap();
    assert_eq!(
        control.entries_in(&control_pack).unwrap().len(),
        entries.len(),
        "the pack does not read back whole when nothing was cut, so a cut proves nothing"
    );

    let mut checked = 0u32;
    for cut in [
        0,
        1,
        8,
        first_of_last.saturating_sub(1),
        first_of_last,
        first_of_last + 1,
        u64::midpoint(first_of_last, last),
        last.saturating_sub(1),
    ] {
        let cut = usize::try_from(cut).unwrap().min(whole.len());
        let elsewhere = support::scratch();
        let cache = packed_cache(elsewhere.path());
        let into = cache.layout().packs().join(packs[0].file_name().unwrap());
        std::fs::create_dir_all(cache.layout().packs()).unwrap();
        std::fs::write(&into, &whole[..cut]).unwrap();

        let readable = cache.entries_in(&into).unwrap_or_default();
        assert!(
            readable.len() < entries.len(),
            "a pack cut at {cut} still reported every entry the whole one held"
        );
        assert!(
            !readable.iter().any(|(digest, _)| *digest == cut_digest),
            "a pack cut at {cut}, inside its last entry, still held that entry"
        );
        for (digest, _) in &readable {
            let mut bytes = Vec::new();
            if let Ok(mut reader) = cache.open(*digest) {
                std::io::copy(&mut reader, &mut bytes).unwrap();
                assert_eq!(
                    hash_bytes(&bytes),
                    *digest,
                    "a pack cut at {cut} served an object that is not its own bytes"
                );
            }
        }
        checked += 1;
    }
    assert!(checked >= 8, "only {checked} truncation offsets were tried");
}
