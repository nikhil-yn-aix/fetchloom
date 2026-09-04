//! Contract tests for the content digest, interop digest, outboard tree,
//! canonical entry stream, tree digest, and conformance corpus.

#![expect(
    clippy::unwrap_used,
    reason = "test setup, where a failure to build the input is the assertion"
)]
#![expect(
    clippy::cast_possible_truncation,
    reason = "every target this crate ships on is 64-bit, so a usize round trips a u64 test length"
)]

use rayon as _;
use serde as _;
use serde_json as _;
use sha2 as _;
use toml as _;

use std::io::Cursor;
use std::num::NonZeroUsize;

use fetchloom_engine::canonical::{encode_entries, tree_digest};
use fetchloom_engine::conformance::{declared_failures, portable_core};
use fetchloom_engine::digest::ContentDigest;
use fetchloom_engine::error::ErrorKind;
use fetchloom_engine::hashing::Digester;
use fetchloom_engine::hashing::{Digests, Pair};
use fetchloom_engine::limits::{OUTBOARD_CHUNK_GROUP, OUTBOARD_THRESHOLD};
use fetchloom_engine::outboard::{tree_of, verify_range};
use fetchloom_engine::pool::Processor;
use fetchloom_engine::threads::ThreadBudget;
use fetchloom_engine::tree::{EntryPath, Mode, TreeEntry};

fn processor() -> Processor {
    let budget = ThreadBudget::resolve(NonZeroUsize::new(4).unwrap(), None);
    Processor::new(budget).unwrap()
}

fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|index| (index % 251) as u8).collect()
}

fn known_sha256_of_abc() -> [u8; 32] {
    let hex = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    assert_eq!(hex.len(), 64);
    let mut bytes = [0u8; 32];
    for (index, slot) in bytes.iter_mut().enumerate() {
        let byte = &hex[index * 2..index * 2 + 2];
        *slot = u8::from_str_radix(byte, 16).unwrap();
    }
    bytes
}

struct Trickle<'a> {
    data: &'a [u8],
    offset: usize,
}

impl std::io::Read for Trickle<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let take = buf.len().min(3).min(self.data.len() - self.offset);
        buf[..take].copy_from_slice(&self.data[self.offset..self.offset + take]);
        self.offset += take;
        Ok(take)
    }
}

#[test]
fn content_digest_of_one_call_matches_content_digest_of_many_small_chunks() {
    let processor = processor();
    let data = pattern(5_000_003);

    let whole = Digester::new()
        .hash(&processor, Cursor::new(data.clone()))
        .unwrap();

    let trickled = Digester::new()
        .hash(
            &processor,
            Trickle {
                data: &data,
                offset: 0,
            },
        )
        .unwrap();

    assert_eq!(whole.content, trickled.content);
    assert_eq!(whole.interop, trickled.interop);
    assert_eq!(whole.content.bytes(), blake3::hash(&data).as_bytes());
}

#[test]
fn interop_digest_matches_a_known_sha256_value() {
    let processor = processor();
    let digests = Digester::new()
        .hash(&processor, Cursor::new(b"abc".to_vec()))
        .unwrap();
    assert_eq!(*digests.interop.bytes(), known_sha256_of_abc());
}

fn walked_root(object: &[u8]) -> [u8; 32] {
    let processor = processor();
    let output = hash_stream(&processor, object);
    let content = output.content;
    if let Some(outboard) = &output.outboard {
        let mut cursor = Cursor::new(outboard.as_bytes().to_vec());
        let range = 0..object.len() as u64;
        verify_range(
            &mut cursor,
            object.len() as u64,
            content,
            range,
            &mut |group, into| {
                let start = (group * OUTBOARD_CHUNK_GROUP) as usize;
                let end = (start + OUTBOARD_CHUNK_GROUP as usize).min(object.len());
                into.clear();
                into.extend_from_slice(&object[start..end]);
                Ok(())
            },
        )
        .unwrap();
    }
    *content.bytes()
}

fn assert_root_matches_blake3(len: usize) {
    let object = pattern(len);
    let root = walked_root(&object);
    assert_eq!(
        root,
        *blake3::hash(&object).as_bytes(),
        "length {len} outboard root disagrees with blake3::hash"
    );
}

#[test]
fn one_chunk_shape_matches_blake3_hash() {
    assert_root_matches_blake3(0);
    assert_root_matches_blake3(1);
    assert_root_matches_blake3(1024);
}

#[test]
fn one_group_shape_matches_blake3_hash() {
    assert_root_matches_blake3(1025);
    assert_root_matches_blake3(OUTBOARD_CHUNK_GROUP as usize);
}

#[test]
fn exact_multiple_of_group_len_matches_blake3_hash() {
    assert_root_matches_blake3((OUTBOARD_CHUNK_GROUP as usize) * 65);
}

#[test]
fn power_of_two_at_or_above_2_26_matches_blake3_hash() {
    assert_root_matches_blake3(1 << 26);
    assert_root_matches_blake3(1 << 27);
}

#[test]
fn final_partial_chunk_cases_match_blake3_hash() {
    assert_root_matches_blake3(OUTBOARD_THRESHOLD as usize + 1);
    assert_root_matches_blake3(OUTBOARD_THRESHOLD as usize + 1023);
    assert_root_matches_blake3(100 * 1_048_576 + 7);
}

#[test]
fn outboard_file_size_is_exactly_the_header_plus_the_parent_nodes() {
    let processor = processor();
    let len = (OUTBOARD_CHUNK_GROUP as usize) * 65 + 3;
    let object = pattern(len);
    let output = hash_stream(&processor, &object);
    let outboard = output.outboard.unwrap();
    let leaf_count = len.div_ceil(OUTBOARD_CHUNK_GROUP as usize) as u64;
    assert_eq!(outboard.as_bytes().len() as u64, 8 + 64 * (leaf_count - 1));
    assert_eq!(outboard.leaf_count(), leaf_count);
}

#[test]
fn no_outboard_is_stored_at_or_below_the_threshold() {
    let processor = processor();
    let output = hash_stream(&processor, &pattern(OUTBOARD_THRESHOLD as usize));
    assert!(output.outboard.is_none());
}

fn build_large_object(extra_groups: u64) -> (Vec<u8>, Digests) {
    let processor = processor();
    let len = (OUTBOARD_THRESHOLD + OUTBOARD_CHUNK_GROUP * extra_groups + 12345) as usize;
    let object = pattern(len);
    let output = hash_stream(&processor, &object);
    (object, output)
}

#[test]
fn flipping_a_byte_inside_a_group_is_reported_as_a_range_mismatch_naming_that_group() {
    let (mut object, output) = build_large_object(4);
    let outboard = output.outboard.unwrap();
    let flip_index = (OUTBOARD_CHUNK_GROUP as usize) * 2 + 17;
    object[flip_index] ^= 0xFF;

    let mut cursor = Cursor::new(outboard.as_bytes().to_vec());
    let object_len = object.len() as u64;
    let error = verify_range(
        &mut cursor,
        object_len,
        output.content,
        0..object_len,
        &mut |group, into| {
            let start = (group * OUTBOARD_CHUNK_GROUP) as usize;
            let end = (start + OUTBOARD_CHUNK_GROUP as usize).min(object.len());
            into.clear();
            into.extend_from_slice(&object[start..end]);
            Ok(())
        },
    )
    .unwrap_err();

    assert_eq!(error.kind(), ErrorKind::IntegrityRangeMismatch);
    let expected_start = 2 * OUTBOARD_CHUNK_GROUP;
    assert!(
        error.next_action().contains(&expected_start.to_string()),
        "{}",
        error.next_action()
    );
}

#[test]
fn flipping_a_byte_inside_a_parent_node_is_reported_at_that_node_not_at_a_leaf() {
    let (object, output) = build_large_object(4);
    let outboard = output.outboard.unwrap();
    let mut corrupted = outboard.as_bytes().to_vec();
    let root_node_left_cv_offset = 8;
    corrupted[root_node_left_cv_offset] ^= 0xFF;

    let mut cursor = Cursor::new(corrupted);
    let object_len = object.len() as u64;
    let error = verify_range(
        &mut cursor,
        object_len,
        output.content,
        0..object_len,
        &mut |group, into| {
            let start = (group * OUTBOARD_CHUNK_GROUP) as usize;
            let end = (start + OUTBOARD_CHUNK_GROUP as usize).min(object.len());
            into.clear();
            into.extend_from_slice(&object[start..end]);
            Ok(())
        },
    )
    .unwrap_err();

    assert_eq!(error.kind(), ErrorKind::IntegrityRangeMismatch);
    assert!(
        error.next_action().contains(&format!("0..{object_len}")),
        "expected the whole-object range to be named at the root node: {}",
        error.next_action()
    );
}

#[test]
fn truncating_the_outboard_is_cache_corrupt() {
    let (object, output) = build_large_object(4);
    let outboard = output.outboard.unwrap();
    let mut truncated = outboard.as_bytes().to_vec();
    truncated.truncate(20);

    let mut cursor = Cursor::new(truncated);
    let object_len = object.len() as u64;
    let error = verify_range(
        &mut cursor,
        object_len,
        output.content,
        0..object_len,
        &mut |_group, into| {
            into.clear();
            Ok(())
        },
    )
    .unwrap_err();

    assert_eq!(error.kind(), ErrorKind::CacheCorrupt);
}

#[test]
fn a_length_that_disagrees_with_the_object_is_cache_corrupt_not_an_integrity_error() {
    let (object, output) = build_large_object(4);
    let outboard = output.outboard.unwrap();
    let mut cursor = Cursor::new(outboard.as_bytes().to_vec());
    let wrong_len = object.len() as u64 + 1;
    let error = verify_range(
        &mut cursor,
        wrong_len,
        output.content,
        0..wrong_len,
        &mut |_group, into| {
            into.clear();
            Ok(())
        },
    )
    .unwrap_err();

    assert_eq!(error.kind(), ErrorKind::CacheCorrupt);
}

#[test]
fn a_tree_is_stored_above_the_threshold_and_not_at_it() {
    let leaves: Vec<[u8; 32]> = (0..64).map(|_| [0u8; 32]).collect();
    assert!(tree_of(OUTBOARD_THRESHOLD, &leaves).1.is_none());
    let leaves: Vec<[u8; 32]> = (0..65).map(|_| [0u8; 32]).collect();
    assert!(tree_of(OUTBOARD_THRESHOLD + 1, &leaves).1.is_some());
}

fn hash_stream(processor: &Processor, object: &[u8]) -> Digests {
    let mut pair = Pair::new();
    for chunk in object.chunks(1 << 20) {
        pair.update(processor, chunk);
    }
    if object.is_empty() {
        pair.update(processor, &[]);
    }
    pair.finish()
}

fn file_entry(path: &str, mode: Mode, bytes: &[u8]) -> TreeEntry {
    TreeEntry::File {
        path: EntryPath::new(path).unwrap(),
        mode,
        size: bytes.len() as u64,
        content: ContentDigest::from_bytes(*blake3::hash(bytes).as_bytes()),
    }
}

fn dir_entry(path: &str) -> TreeEntry {
    TreeEntry::Directory {
        path: EntryPath::new(path).unwrap(),
    }
}

#[test]
fn tree_digest_ignores_the_order_entries_are_given_in() {
    let a = file_entry("a.txt", Mode::ReadWrite, b"one");
    let b = file_entry("b.txt", Mode::ReadWrite, b"two");
    let forward = tree_digest(&[a.clone(), b.clone()]);
    let backward = tree_digest(&[b, a]);
    assert_eq!(forward, backward);
}

#[test]
fn tree_digest_changes_when_a_path_a_mode_a_size_or_a_content_byte_changes() {
    let base = tree_digest(&[file_entry("a.txt", Mode::ReadWrite, b"one")]);
    let different_path = tree_digest(&[file_entry("a2.txt", Mode::ReadWrite, b"one")]);
    let different_mode = tree_digest(&[file_entry("a.txt", Mode::Executable, b"one")]);
    let different_content = tree_digest(&[file_entry("a.txt", Mode::ReadWrite, b"two")]);
    assert_ne!(base, different_path);
    assert_ne!(base, different_mode);
    assert_ne!(base, different_content);
}

#[test]
fn an_empty_directory_and_a_zero_byte_file_at_the_same_path_differ() {
    let as_file = tree_digest(&[file_entry("thing", Mode::ReadWrite, b"")]);
    let as_dir = tree_digest(&[dir_entry("thing")]);
    assert_ne!(as_file, as_dir);
}

#[test]
fn tree_digest_is_not_a_plain_blake3_hash_of_the_same_stream() {
    let entries = [file_entry("a.txt", Mode::ReadWrite, b"one")];
    let stream = encode_entries(&entries);
    let plain = blake3::hash(&stream);
    let derived = tree_digest(&entries);
    assert_ne!(*derived.bytes(), *plain.as_bytes());
}

#[test]
fn length_framing_distinguishes_sets_that_would_collide_under_a_terminator() {
    let split = tree_digest(&[dir_entry("ab"), file_entry("ab/c", Mode::ReadWrite, b"")]);
    let joined = tree_digest(&[dir_entry("abc")]);
    assert_ne!(split, joined);
}

#[test]
fn the_portable_core_produces_a_stable_committed_tree_digest() {
    let digest = tree_digest(&portable_core());
    assert_eq!(
        digest.to_string(),
        "blake3:082f4824def0e6707a1d2fec185a0dec152c41e1a86c01ba1d8d9005af2f7ce8"
    );
}

#[test]
fn declared_failures_are_never_present_in_the_portable_core() {
    let core_paths: std::collections::BTreeSet<String> = portable_core()
        .iter()
        .map(|entry| entry.path().as_str().to_owned())
        .collect();
    for failure in declared_failures() {
        for entry in &failure.entries {
            assert!(
                !core_paths.contains(entry.path().as_str()),
                "{} appears in both the portable core and a declared failure",
                entry.path()
            );
        }
    }
}

#[test]
fn declared_failures_cover_the_required_reasons() {
    let kinds: Vec<ErrorKind> = declared_failures()
        .iter()
        .map(|failure| failure.kind)
        .collect();
    assert!(kinds.contains(&ErrorKind::ArchiveCollision));
    assert!(kinds.contains(&ErrorKind::DestinationUnrepresentable));
    assert_eq!(
        declared_failures()
            .iter()
            .filter(|failure| failure.kind == ErrorKind::ArchiveCollision)
            .count(),
        2
    );
}
