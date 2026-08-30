//! The adversarial proofs phase two is judged on.
//!
//! Every failure the roadmap names is produced against a real server or a real
//! unresolvable name, and each is asserted to leave the transfer in a state a
//! later run can continue from.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test setup, where a failure to build the input is the assertion"
)]

use clap as _;
use clap_complete as _;
use fetchloom_archive as _;
use fetchloom_cli as _;
use flate2 as _;
use serde as _;
use serde_json as _;
use toml as _;

use std::sync::Mutex;
use std::time::Duration;

use fetchloom_cache::Cache;
use fetchloom_engine::degrade::DegradeQueue;
use fetchloom_engine::digest::ContentDigest;
use fetchloom_engine::durability::DurabilityTier;
use fetchloom_engine::error::{ErrorKind, Layer};
use fetchloom_engine::event::Sequence;
use fetchloom_engine::hashing::hash_bytes;
use fetchloom_engine::limits::Limits;
use fetchloom_engine::outcome::ExitCode;
use fetchloom_engine::partial_key::PartialKey;
use fetchloom_engine::resume::ResumeRung;
use fetchloom_engine::seam::store::Store;
use fetchloom_engine::transfer::{Pause, Transfer};
use fetchloom_engine::verification::VerificationPolicy;
use fetchloom_faults::{RecordingObserver, Reply, Script, TestServer};
use fetchloom_platform::NativePlatform;
use fetchloom_sources::HttpSource;
use tempfile::TempDir;

/// A pause that records what it was asked to wait and never waits.
#[derive(Debug, Default)]
struct CountedPause {
    waits: Mutex<Vec<Duration>>,
}

impl CountedPause {
    fn waits(&self) -> Vec<Duration> {
        self.waits.lock().unwrap().clone()
    }
}

impl Pause for CountedPause {
    fn fraction(&self) -> f64 {
        1.0
    }

    fn sleep(&self, duration: Duration) {
        self.waits.lock().unwrap().push(duration);
    }
}

struct Harness {
    _root: TempDir,
    cache: Cache<NativePlatform>,
    source: HttpSource,
    pause: CountedPause,
    limits: Limits,
    degradations: DegradeQueue,
    observer: RecordingObserver,
    sequence: Sequence,
}

impl Harness {
    fn with(limits: Limits) -> Self {
        let root = TempDir::new().unwrap();
        let work = std::sync::Arc::new(fetchloom_engine::work::WorkCounter::new());
        let cache = Cache::open(
            root.path().join("cache"),
            NativePlatform::new(),
            DurabilityTier::Fast,
            VerificationPolicy::Fingerprint,
            std::sync::Arc::clone(&work),
            test_processor(),
        )
        .unwrap();
        Self {
            _root: root,
            cache,
            source: HttpSource::new(limits, work),
            pause: CountedPause::default(),
            limits,
            degradations: DegradeQueue::new(),
            observer: RecordingObserver::new(),
            sequence: Sequence::new(),
        }
    }

    fn new() -> Self {
        Self::with(Limits::default())
    }

    fn transfer(&self) -> Transfer<'_, HttpSource, Cache<NativePlatform>, CountedPause> {
        Transfer {
            store: &self.cache,
            source: &self.source,
            pause: &self.pause,
            limits: &self.limits,
            degradations: &self.degradations,
            observer: &self.observer,
            sequence: &self.sequence,
        }
    }
}

fn object(length: usize) -> Vec<u8> {
    (0..length)
        .map(|index| u8::try_from(index % 251).unwrap_or(0))
        .collect()
}

fn digest_of(bytes: &[u8]) -> ContentDigest {
    hash_bytes(bytes)
}

fn at(server: &TestServer) -> Vec<String> {
    vec![format!("{}/object", server.origin())]
}

fn impatient() -> Limits {
    Limits {
        connect_timeout: Duration::from_millis(500),
        response_timeout: Duration::from_millis(500),
        idle_timeout: Duration::from_millis(500),
        ..Limits::default()
    }
}

#[test]
fn a_truncated_response_exits_thirty_and_leaves_bytes_to_resume_from() {
    let bytes = object(64 * 1024);
    let server = TestServer::start(
        Script::serving(bytes.clone())
            .tagged(vec!["\"one\"".to_owned()])
            .replying(vec![Reply::Truncated { after: 8 * 1024 }]),
    )
    .unwrap();
    let harness = Harness::with(Limits {
        retry_attempts: 1,
        ..Limits::default()
    });

    let failure = harness
        .transfer()
        .run(Some(digest_of(&bytes)), &at(&server))
        .unwrap_err();

    assert_eq!(failure.kind(), ErrorKind::IntegrityTruncated);
    assert_eq!(failure.layer(), Layer::Verify);
    assert_eq!(ExitCode::from(failure.layer()), ExitCode::Integrity);
    assert!(failure.retryable(), "a truncated body is not terminal");

    let recorded = harness
        .cache
        .recorded_source(PartialKey::of_content(digest_of(&bytes)))
        .unwrap()
        .expect("the partial recorded where its bytes came from");
    assert_eq!(
        recorded.written,
        8 * 1024,
        "the partial did not record what arrived, so nothing could resume from it"
    );
}

#[test]
fn a_flipped_byte_exits_thirty_and_publishes_nothing() {
    let bytes = object(4096);
    let server = TestServer::start(Script::serving(bytes.clone()).replying(vec![
        Reply::Flipped { offset: 11 },
        Reply::Flipped { offset: 11 },
        Reply::Flipped { offset: 11 },
        Reply::Flipped { offset: 11 },
        Reply::Flipped { offset: 11 },
    ]))
    .unwrap();
    let harness = Harness::new();

    let failure = harness
        .transfer()
        .run(Some(digest_of(&bytes)), &at(&server))
        .unwrap_err();

    assert_eq!(failure.kind(), ErrorKind::IntegrityMismatch);
    assert_eq!(ExitCode::from(failure.layer()), ExitCode::Integrity);
    assert!(
        !harness.cache.contains(digest_of(&bytes)).unwrap(),
        "a mismatched object was published"
    );
}

#[test]
fn a_stalled_connection_exits_twenty_rather_than_hanging() {
    let bytes = object(64 * 1024);
    let server = TestServer::start(
        Script::serving(bytes.clone())
            .tagged(vec!["\"one\"".to_owned()])
            .replying(vec![Reply::Stalled { after: 4096 }]),
    )
    .unwrap();
    let harness = Harness::with(Limits {
        retry_attempts: 1,
        ..impatient()
    });

    let failure = harness
        .transfer()
        .run(Some(digest_of(&bytes)), &at(&server))
        .unwrap_err();

    assert_eq!(ExitCode::from(failure.layer()), ExitCode::Network);
    assert!(failure.retryable(), "a stall is not terminal");
    assert!(
        harness
            .cache
            .recorded_source(PartialKey::of_content(digest_of(&bytes)))
            .unwrap()
            .is_some_and(|recorded| recorded.written > 0),
        "a stalled transfer kept nothing to resume from"
    );
}

#[test]
fn a_name_that_does_not_resolve_exits_twenty() {
    let harness = Harness::with(impatient());
    let locations = vec!["http://fetchloom-no-such-host.invalid/object".to_owned()];

    let failure = harness
        .transfer()
        .run(Some(digest_of(b"anything")), &locations)
        .unwrap_err();

    assert_eq!(failure.layer(), Layer::Transfer);
    assert_eq!(ExitCode::from(failure.layer()), ExitCode::Network);
    assert!(
        failure.retryable(),
        "a name that does not resolve today may resolve later"
    );
}

#[test]
fn a_rate_limit_storm_stops_at_the_attempt_limit_and_waits_within_the_ceiling() {
    let server = TestServer::start(Script::serving(object(4096)).replying(vec![
        Reply::Status {
            code: 429,
            retry_after: Some("1".to_owned()),
        },
        Reply::Status {
            code: 429,
            retry_after: Some("1".to_owned()),
        },
        Reply::Status {
            code: 429,
            retry_after: Some("1".to_owned()),
        },
        Reply::Status {
            code: 429,
            retry_after: Some("1".to_owned()),
        },
        Reply::Status {
            code: 429,
            retry_after: Some("1".to_owned()),
        },
        Reply::Status {
            code: 429,
            retry_after: Some("1".to_owned()),
        },
    ]))
    .unwrap();
    let harness = Harness::new();

    let failure = harness
        .transfer()
        .run(Some(digest_of(b"anything")), &at(&server))
        .unwrap_err();

    assert_eq!(failure.kind(), ErrorKind::NetworkStatus);
    assert_eq!(ExitCode::from(failure.layer()), ExitCode::Network);
    assert_eq!(
        failure.attempts(),
        harness.limits.retry_attempts,
        "the storm did not stop at the attempt limit"
    );

    let waits = harness.pause.waits();
    assert_eq!(waits.len(), (harness.limits.retry_attempts - 1) as usize);
    for wait in waits {
        assert!(
            wait <= harness.limits.retry_ceiling,
            "a wait of {wait:?} passed the ceiling"
        );
    }
}

#[test]
fn a_wait_the_source_asks_for_beyond_the_ceiling_is_not_waited_out() {
    let limits = Limits::default();
    assert!(fetchloom_engine::transfer::honors(
        &limits,
        Duration::from_secs(30)
    ));
    assert!(
        !fetchloom_engine::transfer::honors(&limits, Duration::from_secs(3600)),
        "an hour was accepted as a wait"
    );
}

#[test]
fn an_outboard_tree_puts_a_resumed_transfer_on_the_first_rung() {
    let bytes = object(64 * 1024);
    let digest = digest_of(&bytes);
    let server = TestServer::start(
        Script::serving(bytes.clone())
            .tagged(vec!["\"one\"".to_owned()])
            .replying(vec![Reply::ClosedMidBody { after: 16 * 1024 }]),
    )
    .unwrap();
    let harness = Harness::new();
    harness
        .cache
        .write_outboard(digest, b"a stored tree")
        .unwrap();

    let done = harness.transfer().run(Some(digest), &at(&server)).unwrap();

    assert_eq!(done.rung, ResumeRung::Outboard);
    assert_eq!(done.rung.number(), 1);
    assert!(harness.cache.contains(digest).unwrap());
}

#[test]
fn every_rung_the_ladder_names_is_reported_by_its_number() {
    assert_eq!(ResumeRung::Outboard.number(), 1);
    assert_eq!(ResumeRung::ImmutableIdentity.number(), 2);
    assert_eq!(ResumeRung::StrongValidator.number(), 3);
    assert_eq!(ResumeRung::WeakValidator.number(), 4);
    assert_eq!(ResumeRung::NoValidator.number(), 5);
}

#[test]
fn a_weak_validator_resumes_on_the_fourth_rung() {
    let bytes = object(64 * 1024);
    let server = TestServer::start(
        Script::serving(bytes.clone())
            .tagged(vec!["W/\"one\"".to_owned()])
            .replying(vec![Reply::ClosedMidBody { after: 16 * 1024 }]),
    )
    .unwrap();
    let harness = Harness::new();

    let done = harness
        .transfer()
        .run(Some(digest_of(&bytes)), &at(&server))
        .unwrap();

    assert_eq!(done.rung, ResumeRung::WeakValidator);
    assert_eq!(done.rung.number(), 4);
    assert_eq!(done.bytes_kept, 16 * 1024);
}

/// The processor pool every cache in a test is opened with.
fn test_processor() -> std::sync::Arc<fetchloom_engine::pool::Processor> {
    let budget = fetchloom_engine::threads::ThreadBudget::resolve(
        std::thread::available_parallelism().unwrap_or(std::num::NonZeroUsize::MIN),
        None,
    );
    std::sync::Arc::new(fetchloom_engine::pool::Processor::new(budget).unwrap())
}
