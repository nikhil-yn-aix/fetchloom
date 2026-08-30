//! Contract tests over locating damage in an object and bounding its repair.
//!
//! The walk is what turns "this object is wrong" into "these byte ranges are
//! wrong", which is the difference between refetching an object and repairing
//! one. Every case here is a shape of damage a real disk or a real network
//! produces: one flipped bit, a whole group, the first group, the last group,
//! a region spanning two groups, a truncation, and damage to the tree itself
//! rather than to the object.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test setup, where a failure to build the input is the assertion"
)]
#![expect(
    clippy::cast_possible_truncation,
    reason = "every target this crate ships on is 64-bit, so a usize round trips a u64 test length"
)]
#![expect(
    clippy::single_range_in_vec_init,
    reason = "a list of damaged spans holding one span is what one damaged region is"
)]

use blake3 as _;
use rayon as _;
use serde as _;
use serde_json as _;
use sha2 as _;
use toml as _;

use std::io::Cursor;

use fetchloom_engine::error::ErrorKind;
use fetchloom_engine::hashing::Pair;
use fetchloom_engine::limits::{Limits, OUTBOARD_CHUNK_GROUP, OUTBOARD_THRESHOLD};
use fetchloom_engine::outboard::{Outboard, find_damage};
use fetchloom_engine::pool::Processor;
use fetchloom_engine::repair::{RepairPlan, WholeReason, plan_repair};
use fetchloom_engine::seam::source::ByteRange;
use fetchloom_engine::threads::ThreadBudget;

fn processor() -> Processor {
    Processor::new(ThreadBudget::resolve(
        std::num::NonZeroUsize::new(2).unwrap(),
        None,
    ))
    .unwrap()
}

/// Hashes bytes in one pass the way every write path does.
fn hash_stream(object: &[u8]) -> fetchloom_engine::hashing::Digests {
    let processor = processor();
    let mut pair = Pair::new();
    for chunk in object.chunks(1 << 20) {
        pair.update(&processor, chunk);
    }
    pair.finish()
}

/// Builds an object of the given length with bytes that differ everywhere, so a
/// group swapped for another group is caught rather than matching by accident.
fn pattern(len: usize) -> Vec<u8> {
    let mut bytes = vec![0u8; len];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::try_from((index * 31 + index / 977) % 251).unwrap_or(0);
    }
    bytes
}

/// An object of `groups` whole leaf groups plus a partial final one, with its
/// tree.
fn large(groups: u64) -> (Vec<u8>, Outboard, fetchloom_engine::digest::ContentDigest) {
    let len = (OUTBOARD_CHUNK_GROUP * groups + 4097) as usize;
    assert!(len as u64 > OUTBOARD_THRESHOLD, "the object needs a tree");
    let object = pattern(len);
    let output = hash_stream(&object);
    let outboard = output.outboard.expect("an object this large stores a tree");
    (object, outboard, output.content)
}

fn damage_of(
    object: &[u8],
    outboard: &Outboard,
    digest: fetchloom_engine::digest::ContentDigest,
) -> Result<Vec<std::ops::Range<u64>>, fetchloom_engine::error::Error> {
    let mut cursor = Cursor::new(outboard.as_bytes().to_vec());
    find_damage(
        &mut cursor,
        object.len() as u64,
        digest,
        &mut |group, into| {
            let start = usize::try_from(group * OUTBOARD_CHUNK_GROUP).unwrap();
            let end = (start + usize::try_from(OUTBOARD_CHUNK_GROUP).unwrap()).min(object.len());
            into.clear();
            into.extend_from_slice(&object[start..end]);
            Ok(())
        },
    )
}

fn span(group: u64, count: u64, object_len: u64) -> std::ops::Range<u64> {
    let start = group * OUTBOARD_CHUNK_GROUP;
    start..(start + count * OUTBOARD_CHUNK_GROUP).min(object_len)
}

#[test]
fn an_undamaged_object_reports_no_damaged_range() {
    let (object, outboard, digest) = large(70);
    assert_eq!(damage_of(&object, &outboard, digest).unwrap(), Vec::new());
}

#[test]
fn a_single_flipped_bit_names_the_one_group_that_holds_it() {
    let (mut object, outboard, digest) = large(70);
    let at = usize::try_from(OUTBOARD_CHUNK_GROUP * 33 + 511).unwrap();
    object[at] ^= 0b0000_1000;
    let len = object.len() as u64;

    assert_eq!(
        damage_of(&object, &outboard, digest).unwrap(),
        vec![span(33, 1, len)]
    );
}

#[test]
fn damage_in_the_first_group_names_the_first_group() {
    let (mut object, outboard, digest) = large(70);
    object[0] ^= 0xFF;
    let len = object.len() as u64;

    assert_eq!(
        damage_of(&object, &outboard, digest).unwrap(),
        vec![span(0, 1, len)]
    );
}

#[test]
fn damage_in_the_last_partial_group_names_the_last_group() {
    let (mut object, outboard, digest) = large(70);
    let last = object.len() - 1;
    object[last] ^= 0xFF;
    let len = object.len() as u64;

    assert_eq!(
        damage_of(&object, &outboard, digest).unwrap(),
        vec![span(70, 1, len)]
    );
}

#[test]
fn a_region_spanning_two_groups_is_reported_as_one_merged_span() {
    let (mut object, outboard, digest) = large(70);
    let start = usize::try_from(OUTBOARD_CHUNK_GROUP * 12 - 8).unwrap();
    for byte in &mut object[start..start + 16] {
        *byte ^= 0xFF;
    }
    let len = object.len() as u64;

    assert_eq!(
        damage_of(&object, &outboard, digest).unwrap(),
        vec![span(11, 2, len)],
        "two adjacent damaged groups are one span, so one request serves them"
    );
}

#[test]
fn damage_in_two_groups_that_are_not_adjacent_stays_two_spans() {
    let (mut object, outboard, digest) = large(70);
    object[usize::try_from(OUTBOARD_CHUNK_GROUP * 4 + 1).unwrap()] ^= 0xFF;
    object[usize::try_from(OUTBOARD_CHUNK_GROUP * 40 + 1).unwrap()] ^= 0xFF;
    let len = object.len() as u64;

    assert_eq!(
        damage_of(&object, &outboard, digest).unwrap(),
        vec![span(4, 1, len), span(40, 1, len)]
    );
}

#[test]
fn a_whole_group_replaced_by_another_group_is_still_damage() {
    let (mut object, outboard, digest) = large(70);
    let group = usize::try_from(OUTBOARD_CHUNK_GROUP).unwrap();
    let (head, tail) = object.split_at_mut(group * 7);
    tail[..group].copy_from_slice(&head[group * 2..group * 3]);
    let len = object.len() as u64;

    assert_eq!(
        damage_of(&object, &outboard, digest).unwrap(),
        vec![span(7, 1, len)]
    );
}

#[test]
fn a_truncated_object_reports_every_group_past_the_truncation_as_damaged() {
    let (object, outboard, digest) = large(70);
    let kept = usize::try_from(OUTBOARD_CHUNK_GROUP * 68).unwrap();
    let mut short = object.clone();
    short.truncate(kept);
    let len = object.len() as u64;

    let mut cursor = Cursor::new(outboard.as_bytes().to_vec());
    let found = find_damage(&mut cursor, len, digest, &mut |group, into| {
        let start = usize::try_from(group * OUTBOARD_CHUNK_GROUP).unwrap();
        let end = (start + usize::try_from(OUTBOARD_CHUNK_GROUP).unwrap()).min(len as usize);
        into.clear();
        if start < short.len() {
            into.extend_from_slice(&short[start..end.min(short.len())]);
        }
        Ok(())
    })
    .unwrap();

    assert_eq!(
        found,
        vec![span(68, 3, len)],
        "the groups past the truncation are damaged and are one span"
    );
}

#[test]
fn a_tree_whose_root_does_not_match_the_digest_is_cache_corrupt_rather_than_damage() {
    let (object, outboard, digest) = large(70);
    let mut forged = outboard.as_bytes().to_vec();
    forged[8] ^= 0xFF;
    let mut cursor = Cursor::new(forged);

    let error = find_damage(
        &mut cursor,
        object.len() as u64,
        digest,
        &mut |_group, into| {
            into.clear();
            Ok(())
        },
    )
    .unwrap_err();

    assert_eq!(
        error.kind(),
        ErrorKind::CacheCorrupt,
        "a tree that disagrees with the digest says nothing about the object"
    );
}

#[test]
fn a_tree_an_attacker_wrote_cannot_make_damaged_bytes_verify() {
    let (mut object, _, digest) = large(70);
    let at = usize::try_from(OUTBOARD_CHUNK_GROUP * 5 + 9).unwrap();
    object[at] ^= 0xFF;

    let forged = hash_stream(&object).outboard.unwrap();
    let mut cursor = Cursor::new(forged.as_bytes().to_vec());

    let error = find_damage(
        &mut cursor,
        object.len() as u64,
        digest,
        &mut |group, into| {
            let start = usize::try_from(group * OUTBOARD_CHUNK_GROUP).unwrap();
            let end = (start + usize::try_from(OUTBOARD_CHUNK_GROUP).unwrap()).min(object.len());
            into.clear();
            into.extend_from_slice(&object[start..end]);
            Ok(())
        },
    )
    .unwrap_err();

    assert_eq!(
        error.kind(),
        ErrorKind::CacheCorrupt,
        "a tree built over the damaged bytes still has to root to the digest, and does not"
    );
}

#[test]
fn a_truncated_tree_is_cache_corrupt() {
    let (object, outboard, digest) = large(70);
    let mut short = outboard.as_bytes().to_vec();
    short.truncate(72);
    let mut cursor = Cursor::new(short);

    let error = find_damage(
        &mut cursor,
        object.len() as u64,
        digest,
        &mut |_group, into| {
            into.clear();
            Ok(())
        },
    )
    .unwrap_err();

    assert_eq!(error.kind(), ErrorKind::CacheCorrupt);
}

#[test]
fn a_group_that_cannot_be_read_fails_rather_than_being_called_undamaged() {
    let (object, outboard, digest) = large(70);
    let mut cursor = Cursor::new(outboard.as_bytes().to_vec());

    let error = find_damage(
        &mut cursor,
        object.len() as u64,
        digest,
        &mut |_group, _into| {
            Err(fetchloom_engine::error::Error::new(
                ErrorKind::CacheCorrupt,
                "the object could not be read",
            ))
        },
    )
    .unwrap_err();

    assert_eq!(error.kind(), ErrorKind::CacheCorrupt);
}

#[test]
fn nothing_damaged_is_a_repair_that_moves_no_bytes() {
    assert_eq!(
        plan_repair(&[], 1 << 30, true, &Limits::default()),
        RepairPlan::Nothing
    );
}

#[test]
fn a_small_amount_of_damage_is_repaired_by_its_own_spans() {
    let limits = Limits::default();
    let spans = vec![
        0..OUTBOARD_CHUNK_GROUP,
        (1 << 26)..(1 << 26) + OUTBOARD_CHUNK_GROUP,
    ];
    assert_eq!(
        plan_repair(&spans, 1 << 30, true, &limits),
        RepairPlan::Spans(vec![
            ByteRange {
                start: 0,
                end: OUTBOARD_CHUNK_GROUP
            },
            ByteRange {
                start: 1 << 26,
                end: (1 << 26) + OUTBOARD_CHUNK_GROUP
            },
        ])
    );
}

#[test]
fn damage_past_the_whole_refetch_share_fetches_the_object_whole() {
    let limits = Limits::default();
    let object_len = 100 * OUTBOARD_CHUNK_GROUP;
    let spans = vec![0..51 * OUTBOARD_CHUNK_GROUP];
    assert_eq!(
        plan_repair(&spans, object_len, true, &limits),
        RepairPlan::Whole(WholeReason::PastTheShare {
            damaged: 51 * OUTBOARD_CHUNK_GROUP,
            allowed: 50 * OUTBOARD_CHUNK_GROUP,
        })
    );
}

#[test]
fn damage_exactly_at_the_whole_refetch_share_is_still_repaired_by_range() {
    let limits = Limits::default();
    let object_len = 100 * OUTBOARD_CHUNK_GROUP;
    let spans = vec![0..50 * OUTBOARD_CHUNK_GROUP];
    assert!(matches!(
        plan_repair(&spans, object_len, true, &limits),
        RepairPlan::Spans(_)
    ));
}

#[test]
fn more_spans_than_the_limit_fetches_the_object_whole() {
    let limits = Limits::default();
    let object_len = 4096 * OUTBOARD_CHUNK_GROUP;
    let spans: Vec<std::ops::Range<u64>> = (0..=limits.repair_spans)
        .map(|index| {
            let start = index * 2 * OUTBOARD_CHUNK_GROUP;
            start..start + OUTBOARD_CHUNK_GROUP
        })
        .collect();
    assert_eq!(
        plan_repair(&spans, object_len, true, &limits),
        RepairPlan::Whole(WholeReason::TooManySpans {
            spans: limits.repair_spans + 1,
            allowed: limits.repair_spans,
        })
    );
}

#[test]
fn exactly_the_span_limit_is_still_repaired_by_range() {
    let limits = Limits::default();
    let object_len = 4096 * OUTBOARD_CHUNK_GROUP;
    let spans: Vec<std::ops::Range<u64>> = (0..limits.repair_spans)
        .map(|index| {
            let start = index * 2 * OUTBOARD_CHUNK_GROUP;
            start..start + OUTBOARD_CHUNK_GROUP
        })
        .collect();
    assert!(matches!(
        plan_repair(&spans, object_len, true, &limits),
        RepairPlan::Spans(_)
    ));
}

#[test]
fn a_source_that_cannot_serve_a_range_fetches_the_object_whole() {
    let limits = Limits::default();
    assert_eq!(
        plan_repair(&[0..OUTBOARD_CHUNK_GROUP], 1 << 30, false, &limits),
        RepairPlan::Whole(WholeReason::NoRanges)
    );
}

#[test]
fn a_source_that_cannot_serve_a_range_still_moves_nothing_when_nothing_is_damaged() {
    assert_eq!(
        plan_repair(&[], 1 << 30, false, &Limits::default()),
        RepairPlan::Nothing
    );
}
