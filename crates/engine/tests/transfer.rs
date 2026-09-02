//! Contract tests over where a transfer starts and how long it waits.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test setup, where a failure to build the input is the assertion"
)]

use blake3 as _;
use rayon as _;
use serde as _;
use serde_json as _;
use sha2 as _;
use toml as _;

use std::time::Duration;

use fetchloom_engine::error::ErrorKind;
use fetchloom_engine::limits::Limits;
use fetchloom_engine::outcome::ExitCode;
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
        last_modified: None,
        supports_ranges,
        time_to_first_byte: Duration::from_millis(1),
        retry_after: None,
        cost: fetchloom_engine::seam::source::Cost::default(),
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
fn bytes_verified_against_the_tree_put_a_transfer_on_the_first_rung() {
    let (rung, keep) = rung_for(
        Some(&recorded(SourceIdentity::None)),
        &metadata(SourceIdentity::None, true),
        1024,
        1024,
    )
    .unwrap();
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
        0,
    )
    .unwrap();
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
        0,
    )
    .unwrap();
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
        0,
    )
    .unwrap();
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
        0,
    )
    .unwrap();
    assert_eq!(rung, ResumeRung::NoValidator);
    assert_eq!(keep, 0, "bytes were kept across a changed validator");
}

#[test]
fn a_source_with_no_identity_never_resumes_even_when_it_matches() {
    let (rung, keep) = rung_for(
        Some(&recorded(SourceIdentity::None)),
        &metadata(SourceIdentity::None, true),
        1024,
        0,
    )
    .unwrap();
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
        0,
    )
    .unwrap();
    assert_eq!(rung, ResumeRung::NoValidator);
    assert_eq!(keep, 0);
}

#[test]
fn a_partial_with_no_record_beside_it_restarts() {
    let (rung, keep) = rung_for(
        None,
        &metadata(SourceIdentity::StrongValidator("\"one\"".to_owned()), true),
        1024,
        0,
    )
    .unwrap();
    assert_eq!(rung, ResumeRung::NoValidator);
    assert_eq!(keep, 0);
}

#[test]
fn nothing_on_disk_reports_the_rung_the_source_would_earn() {
    let (rung, keep) = rung_for(
        None,
        &metadata(SourceIdentity::StrongValidator("\"one\"".to_owned()), true),
        0,
        0,
    )
    .unwrap();
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

#[test]
fn an_immutable_identity_that_moved_is_terminal_rather_than_a_restart() {
    let failed = rung_for(
        Some(&recorded(SourceIdentity::ImmutableVersion(
            "one".to_owned(),
        ))),
        &metadata(SourceIdentity::ImmutableVersion("two".to_owned()), true),
        1024,
        0,
    )
    .expect_err("a source that moved an immutable identity was allowed to restart");

    assert_eq!(failed.kind(), ErrorKind::SourceIdentityChanged);
    assert!(
        !failed.retryable(),
        "asking again cannot make a source immutable"
    );
    assert_eq!(ExitCode::from(failed.kind().layer()), ExitCode::Network);
}

#[test]
fn a_content_address_that_moved_is_terminal_rather_than_a_restart() {
    let one = SourceIdentity::ContentAddress(fetchloom_engine::hashing::hash_bytes(b"one"));
    let two = SourceIdentity::ContentAddress(fetchloom_engine::hashing::hash_bytes(b"two"));
    let failed = rung_for(Some(&recorded(one)), &metadata(two, true), 1024, 0)
        .expect_err("a source that moved a content address was allowed to restart");

    assert_eq!(failed.kind(), ErrorKind::SourceIdentityChanged);
}

#[test]
fn a_source_that_stopped_stating_an_immutable_identity_restarts_rather_than_failing() {
    let (rung, keep) = rung_for(
        Some(&recorded(SourceIdentity::ImmutableVersion(
            "one".to_owned(),
        ))),
        &metadata(SourceIdentity::StrongValidator("\"tag\"".to_owned()), true),
        1024,
        0,
    )
    .expect("a source that stopped making the promise did not break it");

    assert_eq!(rung, ResumeRung::NoValidator);
    assert_eq!(keep, 0);
}

/// A pause that never waits and records every wait it was asked for.
#[derive(Debug, Default)]
struct RecordingPause {
    jitter: f64,
    waits: std::sync::Mutex<Vec<Duration>>,
}

impl fetchloom_engine::transfer::Pause for RecordingPause {
    fn fraction(&self) -> f64 {
        self.jitter
    }

    fn sleep(&self, duration: Duration) {
        self.waits
            .lock()
            .expect("the recorder was poisoned")
            .push(duration);
    }
}

fn waits_when_a_source_asks_for(asked: Option<Duration>) -> Vec<Duration> {
    let limits = Limits::default();
    let pause = RecordingPause {
        jitter: 1.0,
        waits: std::sync::Mutex::new(Vec::new()),
    };
    let observer = SilentObserver;
    let sequence = fetchloom_engine::event::Sequence::new();
    let controller = std::sync::Mutex::new(fetchloom_engine::tuning::Controller::fixed(
        std::num::NonZeroU32::new(4).unwrap(),
    ));
    let retry = fetchloom_engine::transfer::Retry {
        limits: &limits,
        pause: &pause,
        observer: &observer,
        sequence: &sequence,
        controller: &controller,
    };
    let outcome: Result<(), _> = retry.until_spent(|_| {
        let mut failure = fetchloom_engine::error::Error::new(
            ErrorKind::NetworkStatus,
            "try the source again, because it answered 429",
        )
        .with_retryable(true);
        if let Some(wait) = asked {
            failure = failure.with_retry_after(wait);
        }
        Err(failure)
    });
    assert!(outcome.is_err(), "the attempt was supposed to run out");
    let waits = pause.waits.lock().expect("the recorder was poisoned");
    waits.clone()
}

#[test]
fn a_retry_after_shorter_than_the_backoff_never_shortens_the_wait() {
    let limits = Limits::default();
    let asked = Duration::from_millis(1);
    let waited = waits_when_a_source_asks_for(Some(asked));
    let alone = waits_when_a_source_asks_for(None);

    assert_eq!(
        waited.len(),
        alone.len(),
        "a different number of attempts ran"
    );
    for (attempt, (with, without)) in waited.iter().zip(&alone).enumerate() {
        assert_eq!(
            with,
            without,
            "attempt {} waited {with:?} with a one millisecond Retry-After and {without:?} without one",
            attempt + 1
        );
        assert!(
            *with >= asked,
            "attempt {} waited {with:?}, less than the source asked for",
            attempt + 1
        );
    }
    assert!(
        waited.iter().any(|wait| *wait > asked),
        "no attempt waited longer than the {asked:?} the source asked for, so the backoff was discarded"
    );
    assert!(
        waited.iter().all(|wait| *wait <= limits.retry_ceiling),
        "a wait passed the ceiling"
    );
}

#[test]
fn a_retry_after_longer_than_the_backoff_raises_the_wait_to_it() {
    let asked = Duration::from_secs(30);
    let waited = waits_when_a_source_asks_for(Some(asked));

    assert!(!waited.is_empty(), "nothing waited at all");
    for (attempt, wait) in waited.iter().enumerate() {
        assert!(
            *wait >= asked,
            "attempt {} waited {wait:?}, sooner than the {asked:?} the source asked for",
            attempt + 1
        );
    }
}

/// An observer a test hands the retry loop when the events are not what it is
/// asserting on.
#[derive(Debug)]
struct SilentObserver;

impl fetchloom_engine::seam::observer::Observer for SilentObserver {
    fn emit(&self, _event: &fetchloom_engine::event::Event) {}
}
