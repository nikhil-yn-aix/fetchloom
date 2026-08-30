//! Contract tests over where a transfer starts and how long it waits.
//!
//! These cover the decisions a transfer makes before it touches a socket: which
//! rung of the resume ladder the bytes on disk earn, and what the backoff is.

use blake3 as _;
use rayon as _;
use serde as _;
use serde_json as _;
use sha2 as _;

use std::time::Duration;

use fetchloom_engine::limits::Limits;
use fetchloom_engine::redact::SafeUrl;
use fetchloom_engine::reference::Host;
use fetchloom_engine::resume::ResumeRung;
use fetchloom_engine::seam::source::{SourceIdentity, SourceMetadata};
use fetchloom_engine::source_record::SourceRecord;
use fetchloom_engine::transfer::{backoff, honors, rung_for};

fn metadata(identity: SourceIdentity, supports_ranges: bool) -> SourceMetadata {
    SourceMetadata {
        location: SafeUrl::new("https://example.invalid/object"),
        host: Host::new("example.invalid"),
        size: Some(4096),
        content: None,
        interop: None,
        identity,
        supports_ranges,
        time_to_first_byte: Duration::from_millis(1),
        retry_after: None,
    }
}

fn recorded(identity: SourceIdentity) -> SourceRecord {
    SourceRecord {
        location: SafeUrl::new("https://example.invalid/object"),
        host: "example.invalid".to_owned(),
        size: Some(4096),
        etag: None,
        last_modified: None,
        accepts_ranges: true,
        written: 1024,
        rung: ResumeRung::StrongValidator,
        identity,
    }
}

#[test]
fn an_outboard_tree_puts_a_transfer_on_the_first_rung() {
    let (rung, keep) = rung_for(
        Some(&recorded(SourceIdentity::None)),
        &metadata(SourceIdentity::None, true),
        1024,
        true,
    );
    assert_eq!(rung, ResumeRung::Outboard);
    assert_eq!(keep, 1024);
}

#[test]
fn an_unchanged_immutable_identity_puts_a_transfer_on_the_second_rung() {
    let identity = SourceIdentity::ImmutableVersion("v7".to_owned());
    let (rung, keep) = rung_for(
        Some(&recorded(identity.clone())),
        &metadata(identity, true),
        1024,
        false,
    );
    assert_eq!(rung, ResumeRung::ImmutableIdentity);
    assert_eq!(keep, 1024);
}

#[test]
fn an_unchanged_strong_validator_puts_a_transfer_on_the_third_rung() {
    let identity = SourceIdentity::StrongValidator("\"one\"".to_owned());
    let (rung, keep) = rung_for(
        Some(&recorded(identity.clone())),
        &metadata(identity, true),
        1024,
        false,
    );
    assert_eq!(rung, ResumeRung::StrongValidator);
    assert_eq!(keep, 1024);
}

#[test]
fn an_unchanged_weak_validator_puts_a_transfer_on_the_fourth_rung() {
    let identity = SourceIdentity::WeakValidator("W/\"one\"".to_owned());
    let (rung, keep) = rung_for(
        Some(&recorded(identity.clone())),
        &metadata(identity, true),
        1024,
        false,
    );
    assert_eq!(rung, ResumeRung::WeakValidator);
    assert_eq!(keep, 1024);
}

#[test]
fn a_changed_validator_restarts_from_zero_on_the_fifth_rung() {
    let (rung, keep) = rung_for(
        Some(&recorded(SourceIdentity::StrongValidator(
            "\"one\"".to_owned(),
        ))),
        &metadata(SourceIdentity::StrongValidator("\"two\"".to_owned()), true),
        1024,
        false,
    );
    assert_eq!(rung, ResumeRung::NoValidator);
    assert_eq!(keep, 0, "bytes were kept across a changed validator");
}

#[test]
fn a_source_with_no_identity_never_resumes_even_when_it_matches() {
    let (rung, keep) = rung_for(
        Some(&recorded(SourceIdentity::None)),
        &metadata(SourceIdentity::None, true),
        1024,
        false,
    );
    assert_eq!(rung, ResumeRung::NoValidator);
    assert_eq!(keep, 0);
}

#[test]
fn a_source_that_cannot_serve_a_range_restarts_however_good_its_validator_is() {
    let identity = SourceIdentity::StrongValidator("\"one\"".to_owned());
    let (rung, keep) = rung_for(
        Some(&recorded(identity.clone())),
        &metadata(identity, false),
        1024,
        false,
    );
    assert_eq!(rung, ResumeRung::NoValidator);
    assert_eq!(keep, 0);
}

#[test]
fn a_partial_with_no_record_beside_it_restarts() {
    let (rung, keep) = rung_for(
        None,
        &metadata(SourceIdentity::StrongValidator("\"one\"".to_owned()), true),
        1024,
        false,
    );
    assert_eq!(rung, ResumeRung::NoValidator);
    assert_eq!(keep, 0);
}

#[test]
fn nothing_on_disk_reports_the_rung_the_source_would_earn() {
    let (rung, keep) = rung_for(
        None,
        &metadata(SourceIdentity::StrongValidator("\"one\"".to_owned()), true),
        0,
        false,
    );
    assert_eq!(rung, ResumeRung::StrongValidator);
    assert_eq!(keep, 0);
}

#[test]
fn backoff_grows_and_never_passes_the_ceiling() {
    let limits = Limits::default();
    for attempt in 1..=limits.retry_attempts {
        assert!(
            backoff(&limits, attempt, 1.0) <= limits.retry_ceiling,
            "attempt {attempt} waited past the ceiling"
        );
    }
    assert!(
        backoff(&limits, 1, 1.0) < backoff(&limits, 3, 1.0),
        "the wait did not grow"
    );
    assert_eq!(
        backoff(&limits, 20, 1.0),
        limits.retry_ceiling,
        "a far attempt did not settle at the ceiling"
    );
}

#[test]
fn backoff_is_jittered_across_the_whole_window() {
    let limits = Limits::default();
    assert_eq!(
        backoff(&limits, 4, 0.0),
        Duration::ZERO,
        "there was no lower end"
    );
    assert!(
        backoff(&limits, 4, 0.5) < backoff(&limits, 4, 1.0),
        "the jitter did not spread the wait"
    );
}

#[test]
fn a_wait_longer_than_the_ceiling_is_not_honored() {
    let limits = Limits::default();
    assert!(honors(&limits, Duration::from_secs(1)));
    assert!(honors(&limits, limits.retry_ceiling));
    assert!(
        !honors(&limits, limits.retry_ceiling + Duration::from_secs(1)),
        "a wait past the ceiling was accepted"
    );
    assert!(!honors(&limits, Duration::from_secs(3600)));
}
