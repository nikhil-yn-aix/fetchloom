//! When a run splits one object into ranges, and what it says when it does not.

#![expect(
    clippy::unwrap_used,
    reason = "test assertions, where the run that failed is the message"
)]

use blake3 as _;
use rayon as _;
use serde as _;
use serde_json as _;
use sha2 as _;
use toml as _;

use std::time::Duration;

use fetchloom_engine::digest::ContentDigest;
use fetchloom_engine::limits::Limits;
use fetchloom_engine::redact::SafeUrl;
use fetchloom_engine::reference::Host;
use fetchloom_engine::seam::source::{ByteRange, Cost, SourceIdentity, SourceMetadata};
use fetchloom_engine::split::{Refused, parts_for, spans};

fn metadata(size: u64, ranges: bool, identity: SourceIdentity) -> SourceMetadata {
    SourceMetadata {
        location: SafeUrl::new("https://one.example/object"),
        host: Host::new("one.example".to_owned()),
        size: Some(size),
        content: None,
        interop: None,
        identity,
        last_modified: None,
        supports_ranges: ranges,
        time_to_first_byte: Duration::from_millis(1),
        retry_after: None,
        cost: Cost::default(),
    }
}

fn immutable() -> SourceIdentity {
    SourceIdentity::ImmutableVersion("v1".to_owned())
}

fn large() -> u64 {
    Limits::default().split_threshold + 1
}

#[test]
fn a_large_immutable_ranged_object_on_a_host_measured_wider_is_split() {
    let parts = parts_for(&metadata(large(), true, immutable()), &Limits::default(), 4).unwrap();
    assert_eq!(parts.get(), 4, "the split did not take the measured width");
}

#[test]
fn an_object_at_or_below_the_threshold_is_not_split() {
    let refused = parts_for(
        &metadata(Limits::default().split_threshold, true, immutable()),
        &Limits::default(),
        4,
    )
    .unwrap_err();
    assert_eq!(refused, Refused::NotLarge);
    assert_eq!(refused.because(), "the object is not large enough to split");
}

#[test]
fn an_object_whose_identity_can_change_is_not_split() {
    let refused = parts_for(
        &metadata(
            large(),
            true,
            SourceIdentity::StrongValidator("tag".to_owned()),
        ),
        &Limits::default(),
        4,
    )
    .unwrap_err();
    assert_eq!(refused, Refused::NotImmutable);
    assert_eq!(
        refused.because(),
        "the source states no identity that cannot change under the same name"
    );
}

#[test]
fn a_source_that_serves_no_range_is_not_split() {
    let refused = parts_for(
        &metadata(large(), false, immutable()),
        &Limits::default(),
        4,
    )
    .unwrap_err();
    assert_eq!(refused, Refused::NoRanges);
    assert_eq!(refused.because(), "the source does not serve ranges");
}

#[test]
fn a_host_never_measured_to_serve_more_with_more_streams_is_not_split() {
    let refused =
        parts_for(&metadata(large(), true, immutable()), &Limits::default(), 1).unwrap_err();
    assert_eq!(refused, Refused::NoMeasuredGain);
    assert_eq!(
        refused.because(),
        "this run has not measured the host as serving more with more streams"
    );
}

#[test]
fn an_object_of_unstated_length_is_not_split() {
    let mut unstated = metadata(large(), true, immutable());
    unstated.size = None;
    let refused = parts_for(&unstated, &Limits::default(), 4).unwrap_err();
    assert_eq!(refused, Refused::NotLarge);
}

#[test]
fn the_spans_a_split_asks_for_cover_the_object_once_and_leave_no_gap() {
    let covered = spans(0, 1000, std::num::NonZeroU32::new(3).unwrap());
    assert_eq!(
        covered,
        vec![
            ByteRange { start: 0, end: 334 },
            ByteRange {
                start: 334,
                end: 668
            },
            ByteRange {
                start: 668,
                end: 1000
            },
        ]
    );
}

#[test]
fn a_split_that_resumes_covers_only_what_is_missing() {
    let covered = spans(600, 1000, std::num::NonZeroU32::new(2).unwrap());
    assert_eq!(covered.first().unwrap().start, 600);
    assert_eq!(covered.last().unwrap().end, 1000);
    assert_eq!(
        covered.iter().map(|span| span.length()).sum::<u64>(),
        400,
        "the spans covered a length other than what was missing"
    );
}

#[test]
fn a_split_never_asks_for_more_spans_than_the_object_has_bytes() {
    let covered = spans(0, 2, std::num::NonZeroU32::new(8).unwrap());
    assert_eq!(covered.len(), 2);
    assert_eq!(
        covered.iter().map(|span| span.length()).sum::<u64>(),
        2,
        "the spans covered a length other than the object"
    );
}

#[test]
fn the_digest_of_a_split_object_is_the_digest_of_the_same_bytes_in_order() {
    let bytes: Vec<u8> = (0..4096_usize)
        .map(|index| u8::try_from(index % 251).unwrap_or(0))
        .collect();
    let whole = fetchloom_engine::hashing::hash_bytes(&bytes);
    let covered = spans(0, bytes.len() as u64, std::num::NonZeroU32::new(4).unwrap());
    let rejoined: Vec<u8> = covered
        .iter()
        .flat_map(|span| {
            bytes[usize::try_from(span.start).unwrap()..usize::try_from(span.end).unwrap()].to_vec()
        })
        .collect();
    let joined: ContentDigest = fetchloom_engine::hashing::hash_bytes(&rejoined);
    assert_eq!(
        joined, whole,
        "the spans a split asks for do not rejoin into the object"
    );
}
