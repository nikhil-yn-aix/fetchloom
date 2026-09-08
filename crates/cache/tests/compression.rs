//! What compressing a cached object may and may not change.

#![expect(
    clippy::unwrap_used,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::panic,
    reason = "test inputs, where the narrowing is the point and a failure to build one is the assertion"
)]

use std::io::{Read, Seek, SeekFrom};

use fetchloom_engine::compression::{CompressionChoice, PROBE_RATIO, Stored, shuffle, unshuffle};
use fetchloom_engine::digest::ContentDigest;
use fetchloom_engine::hashing::hash_bytes;
use fetchloom_engine::limits::{COMPRESSION_FRAME_BYTES, OUTBOARD_CHUNK_GROUP, PACK_THRESHOLD};
use fetchloom_engine::seam::policy::IoMode;
use fetchloom_engine::seam::store::Store;
use fetchloom_engine::verification::VerificationPolicy;

use fetchloom_cache::layout::COMPRESSED_SUFFIX;
use fetchloom_cache::storage::Placement;

use blake3 as _;
use fetchloom_faults as _;
use serde as _;
use serde_json as _;
#[cfg(windows)]
use windows_sys as _;
use zstd as _;

mod support;

use support::{cache_in, open_cache_compressed, publish, scratch};

/// Text that compresses well, at a length that spans several frames.
fn compressible(length: usize) -> Vec<u8> {
    let line = b"the quick brown fox jumps over the lazy dog, and does it again\n";
    line.iter().copied().cycle().take(length).collect()
}

/// Bytes that do not compress, drawn from a counter mixed enough to be dense.
fn incompressible(length: usize, seed: u64) -> Vec<u8> {
    let mut state = seed | 1;
    (0..length)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 24) as u8
        })
        .collect()
}

/// A float array whose exponent bytes repeat and whose mantissa bytes do not,
/// which is the shape byte shuffling exists for.
fn float_array(values: usize) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(values * 4);
    for index in 0..values {
        let value = 300.0_f32 + (index % 512) as f32 / 512.0;
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

fn compressed_cache(
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

#[test]
fn a_compression_frame_covers_exactly_one_outboard_chunk_group() {
    assert_eq!(
        COMPRESSION_FRAME_BYTES, OUTBOARD_CHUNK_GROUP,
        "a frame that is not one chunk group makes a ranged verify decompress bytes it did not ask for"
    );
    assert_eq!(
        COMPRESSION_FRAME_BYTES, PACK_THRESHOLD,
        "a packed object has to be exactly one frame"
    );
}

#[test]
fn every_range_of_a_compressed_object_reads_back_the_bytes_that_went_in() {
    let scratch = scratch();
    let held = compressed_cache(scratch.path());
    let bytes = compressible(usize::try_from(COMPRESSION_FRAME_BYTES).unwrap() * 4 + 7919);
    let digest = publish(&held, &bytes);

    assert!(
        held.placement(digest).unwrap().is_compressed(),
        "compressible bytes were stored raw"
    );

    let length = bytes.len() as u64;
    let mut reader = held.read(digest).unwrap();
    for start in [
        0,
        1,
        COMPRESSION_FRAME_BYTES - 1,
        COMPRESSION_FRAME_BYTES,
        COMPRESSION_FRAME_BYTES + 1,
        COMPRESSION_FRAME_BYTES * 2 + 33,
        COMPRESSION_FRAME_BYTES * 4,
        length - 1,
    ] {
        for span in [
            1_u64,
            17,
            COMPRESSION_FRAME_BYTES / 3,
            COMPRESSION_FRAME_BYTES + 5,
        ] {
            let end = (start + span).min(length);
            if start >= length {
                continue;
            }
            reader.seek(SeekFrom::Start(start)).unwrap();
            let mut read = vec![0u8; usize::try_from(end - start).unwrap()];
            reader.read_exact(&mut read).unwrap();
            assert_eq!(
                read,
                bytes[usize::try_from(start).unwrap()..usize::try_from(end).unwrap()],
                "the range from {start} to {end} did not read back what was written"
            );
        }
    }
}

#[test]
fn a_compressed_object_reads_back_whole_and_hashes_to_its_own_name() {
    let scratch = scratch();
    let held = compressed_cache(scratch.path());
    let bytes = compressible(usize::try_from(COMPRESSION_FRAME_BYTES).unwrap() * 3 + 11);
    let digest = publish(&held, &bytes);

    let mut read = Vec::new();
    held.read(digest).unwrap().read_to_end(&mut read).unwrap();
    assert_eq!(read, bytes, "a compressed object did not read back whole");
    assert_eq!(
        hash_bytes(&read),
        digest,
        "the bytes read back do not hash to the name they are filed under"
    );
    assert!(
        held.object_is_its_digest(digest).unwrap(),
        "the cache does not agree the object is its own digest"
    );
}

#[test]
fn a_compressed_object_is_smaller_on_disk_than_the_bytes_it_holds() {
    let scratch = scratch();
    let held = compressed_cache(scratch.path());
    let bytes = compressible(usize::try_from(COMPRESSION_FRAME_BYTES).unwrap() * 2);
    let digest = publish(&held, &bytes);

    let path = held.layout().compressed_object(digest);
    let on_disk = std::fs::metadata(&path).unwrap().len();
    assert!(
        on_disk * 2 < bytes.len() as u64,
        "{} bytes of repeated text were stored in {on_disk} bytes, which is no saving at all",
        bytes.len()
    );
    assert_eq!(
        held.size_of(digest),
        Some(bytes.len() as u64),
        "the cache reports the stored length where it should report the object's own"
    );
}

#[test]
fn the_probe_compresses_what_compresses_and_stores_the_rest_raw() {
    let scratch = scratch();
    let held = compressed_cache(scratch.path());
    let large = usize::try_from(PACK_THRESHOLD).unwrap() * 2;

    let text = publish(&held, &compressible(large));
    let noise = publish(&held, &incompressible(large, 0x5eed));

    assert!(
        held.placement(text).unwrap().is_compressed(),
        "an entry that compresses was stored raw"
    );
    assert!(
        !held.placement(noise).unwrap().is_compressed(),
        "an entry that does not compress was compressed anyway, which spends processor time to store more bytes"
    );
    assert!(
        held.layout().compressed_object(text).is_file(),
        "a compressed object is not filed under the name that says so"
    );
    assert!(
        held.layout().object(noise).is_file(),
        "an object stored raw is not filed under its plain name"
    );
}

#[test]
fn the_probe_decides_from_the_bytes_and_not_from_a_ratio_below_the_threshold() {
    let sample = incompressible(1 << 20, 0xabcd);
    let decided = fetchloom_cache::compress::decide(&sample, CompressionChoice::Auto).unwrap();
    assert_eq!(
        decided.stored,
        Stored::Raw,
        "dense bytes measured at {} were compressed anyway",
        decided.measurements()
    );
    assert!(
        decided.ratios.iter().all(|ratio| *ratio < PROBE_RATIO),
        "the corpus assumption that dense bytes measure below {PROBE_RATIO} on every stride does not hold here: {}",
        decided.measurements()
    );
    assert!(
        decided.reason().contains("by stride"),
        "the decision does not say what it measured: {}",
        decided.reason()
    );
}

#[test]
fn byte_shuffling_is_chosen_when_it_measures_better_and_never_changes_the_bytes() {
    let array = float_array(1 << 18);
    let decided = fetchloom_cache::compress::decide(&array, CompressionChoice::Auto).unwrap();
    assert!(
        decided.ratios[2] > decided.ratios[0],
        "shuffling a float array measured no better than not shuffling it: {:.3} against {:.3}",
        decided.ratios[2],
        decided.ratios[0]
    );
    assert!(
        matches!(decided.stored, Stored::Zstd { stride, .. } if stride >= 2),
        "the better stride was measured and then not taken"
    );
    assert_eq!(
        unshuffle(&shuffle(&array, 4), 4),
        array,
        "shuffling a float array did not survive the round trip"
    );

    let scratch = scratch();
    let held = compressed_cache(scratch.path());
    let bytes = float_array(1 << 20);
    let digest = publish(&held, &bytes);
    let mut read = Vec::new();
    held.read(digest).unwrap().read_to_end(&mut read).unwrap();
    assert_eq!(read, bytes, "a shuffled object did not read back its bytes");
}

#[test]
fn compress_none_stores_everything_raw() {
    let scratch = scratch();
    let held = open_cache_compressed(
        scratch.path(),
        VerificationPolicy::Fingerprint,
        IoMode::Buffered,
        CompressionChoice::None,
    )
    .unwrap();
    let bytes = compressible(usize::try_from(PACK_THRESHOLD).unwrap() * 2);
    let digest = publish(&held, &bytes);

    assert!(
        !held.placement(digest).unwrap().is_compressed(),
        "compress none compressed an object anyway"
    );
    let mut read = Vec::new();
    held.read(digest).unwrap().read_to_end(&mut read).unwrap();
    assert_eq!(read, bytes);
}

#[test]
fn a_forced_level_compresses_what_the_probe_would_have_stored_raw() {
    let scratch = scratch();
    let held = open_cache_compressed(
        scratch.path(),
        VerificationPolicy::Fingerprint,
        IoMode::Buffered,
        CompressionChoice::Zstd(1),
    )
    .unwrap();
    let bytes = incompressible(usize::try_from(PACK_THRESHOLD).unwrap() * 2, 0x1234);
    let digest = publish(&held, &bytes);

    assert!(
        held.placement(digest).unwrap().is_compressed(),
        "a level the user named was not used"
    );
    let mut read = Vec::new();
    held.read(digest).unwrap().read_to_end(&mut read).unwrap();
    assert_eq!(read, bytes, "forcing a level changed the bytes");
}

#[test]
fn a_packed_object_survives_being_compressed() {
    let scratch = scratch();
    let held = compressed_cache(scratch.path());
    let bytes = compressible(4096);
    let digest = publish(&held, &bytes);

    assert!(
        matches!(held.placement(digest), Some(Placement::Packed { .. })),
        "a small object was not packed"
    );
    assert!(
        held.placement(digest).unwrap().is_compressed(),
        "a compressible packed object was stored raw"
    );
    assert_eq!(held.size_of(digest), Some(bytes.len() as u64));

    let mut read = Vec::new();
    held.read(digest).unwrap().read_to_end(&mut read).unwrap();
    assert_eq!(read, bytes, "a packed compressed object changed its bytes");
}

#[test]
fn a_pack_holding_both_forms_reads_each_one_back() {
    let scratch = scratch();
    let held = compressed_cache(scratch.path());
    let text = compressible(4096);
    let noise = incompressible(4096, 0x99);
    let more = compressible(8192);

    let first = publish(&held, &text);
    let second = publish(&held, &noise);
    let third = publish(&held, &more);

    for (digest, expected) in [(first, &text), (second, &noise), (third, &more)] {
        let mut read = Vec::new();
        held.read(digest).unwrap().read_to_end(&mut read).unwrap();
        assert_eq!(
            &read, expected,
            "{digest} did not read back out of its pack"
        );
    }
    assert!(!held.placement(second).unwrap().is_compressed());
    assert!(held.placement(first).unwrap().is_compressed());
}

#[test]
fn a_compressed_object_is_listed_pruned_and_removed_by_its_digest() {
    let scratch = scratch();
    let held = compressed_cache(scratch.path());
    let bytes = compressible(usize::try_from(PACK_THRESHOLD).unwrap() * 2);
    let digest = publish(&held, &bytes);

    assert!(
        held.list().unwrap().contains(&digest),
        "a compressed object is not listed among what the cache holds"
    );
    assert!(held.holds(digest), "a compressed object is not held");

    held.remove_object(digest).unwrap();
    assert!(!held.holds(digest), "a compressed object survived removal");
}

#[test]
fn a_name_carrying_the_compressed_suffix_is_not_a_digest() {
    let name = fetchloom_cache::layout::name_of(hash_bytes(b"anything"));
    assert!(
        fetchloom_cache::layout::digest_of(&name).is_some(),
        "a plain digest name stopped parsing"
    );
    assert!(
        fetchloom_cache::layout::digest_of(&format!("{name}{COMPRESSED_SUFFIX}")).is_none(),
        "a bundle member could claim to be a digest by carrying the suffix a compressed object uses"
    );
    assert_eq!(
        fetchloom_cache::layout::stored_digest_of(&format!("{name}{COMPRESSED_SUFFIX}")),
        Some(hash_bytes(b"anything")),
        "the objects directory cannot find a compressed object by its digest"
    );
}

#[test]
fn a_compressed_object_that_lost_its_frame_table_is_refused_rather_than_served() {
    let scratch = scratch();
    let held = compressed_cache(scratch.path());
    let bytes = compressible(usize::try_from(PACK_THRESHOLD).unwrap() * 2);
    let digest = publish(&held, &bytes);
    let path = held.layout().compressed_object(digest);

    let on_disk = std::fs::metadata(&path).unwrap().len();
    support::make_writable(&path);
    let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    file.set_len(on_disk - 8).unwrap();
    drop(file);

    let refused = held.read(digest).map(|_| ()).unwrap_err();
    assert_eq!(
        refused.kind(),
        fetchloom_engine::error::ErrorKind::CacheCorrupt,
        "a truncated frame table was served rather than refused"
    );
}

#[test]
fn the_digest_of_an_object_does_not_depend_on_how_it_is_stored() {
    let bytes = compressible(usize::try_from(PACK_THRESHOLD).unwrap() * 3 + 101);
    let expected: ContentDigest = hash_bytes(&bytes);

    let raw_scratch = scratch();
    let raw = open_cache_compressed(
        raw_scratch.path(),
        VerificationPolicy::Fingerprint,
        IoMode::Buffered,
        CompressionChoice::None,
    )
    .unwrap();
    let stored_raw = publish(&raw, &bytes);

    let zip_scratch = scratch();
    let compressed = compressed_cache(zip_scratch.path());
    let stored_compressed = publish(&compressed, &bytes);

    assert_eq!(stored_raw, expected);
    assert_eq!(stored_compressed, expected);
    assert_eq!(
        stored_raw, stored_compressed,
        "the same bytes took two names depending on how they were stored"
    );
    assert_eq!(
        raw.recorded_interop(stored_raw).unwrap(),
        compressed.recorded_interop(stored_compressed).unwrap(),
        "the interop digest moved with the storage decision"
    );
}

#[test]
fn a_bundle_round_trips_through_a_cache_that_compresses() {
    let source_scratch = scratch();
    let source = compressed_cache(source_scratch.path());
    let text = compressible(usize::try_from(PACK_THRESHOLD).unwrap() * 2);
    let noise = incompressible(4096, 0x77);
    let first = publish(&source, &text);
    let second = publish(&source, &noise);

    let bundle = source_scratch.path().join("carried.bundle");
    let report = source.export(&bundle).unwrap();
    assert_eq!(report.objects, 2);

    let into_scratch = scratch();
    let into = cache_in(into_scratch.path());
    let read = fetchloom_cache::bundle::BundleReader::open(&bundle).unwrap();
    let imported = into.import(&bundle, read).unwrap();
    assert_eq!(imported.objects, 2, "a compressed bundle lost a member");

    for (digest, expected) in [(first, &text), (second, &noise)] {
        let mut got = Vec::new();
        into.read(digest).unwrap().read_to_end(&mut got).unwrap();
        assert_eq!(&got, expected, "{digest} did not survive the bundle");
        assert!(into.object_is_its_digest(digest).unwrap());
    }
}

#[test]
fn an_uncompressed_bundle_is_still_read_by_a_build_that_writes_compressed_ones() {
    let source_scratch = scratch();
    let source = open_cache_compressed(
        source_scratch.path(),
        VerificationPolicy::Fingerprint,
        IoMode::Buffered,
        CompressionChoice::None,
    )
    .unwrap();
    let bytes = compressible(4096);
    let digest = publish(&source, &bytes);
    let bundle = source_scratch.path().join("plain.bundle");
    source.export(&bundle).unwrap();

    let opening = std::fs::read(&bundle).unwrap();
    assert_ne!(
        &opening[..4],
        &[0x28, 0xb5, 0x2f, 0xfd],
        "compress none wrote a compressed bundle"
    );

    let into_scratch = scratch();
    let into = cache_in(into_scratch.path());
    let read = fetchloom_cache::bundle::BundleReader::open(&bundle).unwrap();
    into.import(&bundle, read).unwrap();
    let mut got = Vec::new();
    into.read(digest).unwrap().read_to_end(&mut got).unwrap();
    assert_eq!(got, bytes);
}

#[test]
fn a_compressed_bundle_member_whose_bytes_were_changed_is_refused() {
    let source_scratch = scratch();
    let source = compressed_cache(source_scratch.path());
    let bytes = compressible(4096);
    publish(&source, &bytes);
    let bundle = source_scratch.path().join("tampered.bundle");
    source.export(&bundle).unwrap();

    let carried = std::fs::read(&bundle).unwrap();
    let mut decoded = Vec::new();
    zstd::stream::read::Decoder::new(std::io::Cursor::new(carried))
        .unwrap()
        .read_to_end(&mut decoded)
        .unwrap();
    let body = 512;
    decoded[body] ^= 0xff;
    let recoded = zstd::stream::encode_all(std::io::Cursor::new(decoded), 1).unwrap();
    std::fs::write(&bundle, recoded).unwrap();

    let into_scratch = scratch();
    let into = cache_in(into_scratch.path());
    let read = fetchloom_cache::bundle::BundleReader::open(&bundle).unwrap();
    let refused = into.import(&bundle, read).unwrap_err();
    assert_eq!(
        refused.kind(),
        fetchloom_engine::error::ErrorKind::IntegrityMismatch,
        "a member whose bytes no longer hash to its name was imported"
    );
    assert!(
        into.list().unwrap().is_empty(),
        "a bundle that failed published something"
    );
}

#[test]
fn the_probe_measures_every_stride_and_records_the_one_that_won() {
    let mut array = Vec::new();
    for step in 0..(1 << 17) {
        let value = 240.0_f64 + f64::from(step % 97) / 7.0;
        array.extend_from_slice(&value.to_le_bytes());
    }
    let decided = fetchloom_cache::compress::decide(&array, CompressionChoice::Auto).unwrap();
    assert_eq!(
        decided.ratios.len(),
        4,
        "the probe did not measure one ratio per stride"
    );
    let Stored::Zstd { stride, .. } = decided.stored else {
        panic!(
            "an eight byte array measured {:?} and was stored raw",
            decided.ratios
        );
    };
    assert!(
        matches!(stride, 0 | 2 | 4 | 8),
        "the probe chose a stride of {stride}, which is not one this format can state"
    );
    let best = decided.ratios.iter().copied().fold(f64::MIN, f64::max);
    let chosen = match stride {
        0 => decided.ratios[0],
        2 => decided.ratios[1],
        4 => decided.ratios[2],
        _ => decided.ratios[3],
    };
    assert!(
        (chosen - best).abs() < f64::EPSILON,
        "the probe measured {best:.3} at its best and then stored the object at {chosen:.3}"
    );
    assert!(
        decided.reason().contains("by stride"),
        "the decision does not say what each stride measured: {}",
        decided.reason()
    );
}

#[test]
fn an_eight_byte_array_reads_back_whatever_stride_the_probe_chose() {
    let scratch = scratch();
    let held = open_cache_compressed(
        scratch.path(),
        VerificationPolicy::Fingerprint,
        IoMode::Buffered,
        CompressionChoice::Auto,
    )
    .unwrap();
    let mut array = Vec::new();
    for step in 0..(1 << 18) {
        let value = 101.5_f64 + f64::from(step % 313) / 11.0;
        array.extend_from_slice(&value.to_le_bytes());
    }
    let digest = publish(&held, &array);
    let mut read = Vec::new();
    held.read(digest).unwrap().read_to_end(&mut read).unwrap();
    assert_eq!(
        read, array,
        "an eight byte array did not read back the bytes that went in"
    );
    assert_eq!(
        hash_bytes(&read),
        digest,
        "an eight byte array no longer hashes to the name it is stored under"
    );
}

#[test]
fn every_pack_opens_with_a_preamble_that_states_its_dictionary() {
    let scratch = scratch();
    let held = cache_in(scratch.path());
    publish(&held, &compressible(4096));
    let packs = held.packs().unwrap();
    assert_eq!(packs.len(), 1, "publishing one small object made no pack");
    let mut file = std::fs::File::open(&packs[0]).unwrap();
    let mut preamble = [0_u8; 40];
    file.read_exact(&mut preamble).unwrap();
    assert_eq!(
        &preamble[..4],
        b"FLP1",
        "a pack does not open with the four bytes that say it is one"
    );
    assert_eq!(
        u32::from_le_bytes(preamble[4..8].try_into().unwrap()),
        0,
        "a pack that was only appended to states a dictionary it never trained"
    );
    assert!(
        fetchloom_cache::pack::dictionary_in(&packs[0])
            .unwrap()
            .is_empty(),
        "an appended pack handed back a dictionary"
    );
}

#[test]
fn a_frame_table_stating_a_stride_this_format_cannot_hold_is_refused() {
    let scratch = scratch();
    let held = open_cache_compressed(
        scratch.path(),
        VerificationPolicy::Fingerprint,
        IoMode::Buffered,
        CompressionChoice::Zstd(1),
    )
    .unwrap();
    let bytes = compressible(PACK_THRESHOLD as usize + (1 << 16));
    let digest = publish(&held, &bytes);
    let path = held.layout().compressed_object(digest);
    let span = std::fs::metadata(&path).unwrap().len();

    support::make_writable(&path);
    let mut file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    file.seek(SeekFrom::Start(span - 10)).unwrap();
    std::io::Write::write_all(&mut file, &[3_u8]).unwrap();
    drop(file);

    let refused = held.read(digest).unwrap_err();
    assert_eq!(
        refused.kind(),
        fetchloom_engine::error::ErrorKind::CacheCorrupt,
        "a stride of three was read as if this format stated it"
    );
    assert!(
        refused.next_action().contains("stride"),
        "the refusal does not name what it could not read: {}",
        refused.next_action()
    );
}
