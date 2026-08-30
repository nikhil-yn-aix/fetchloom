//! Contract tests over a transfer that moves real bytes into a real store.
//!
//! Every one drives the whole path: the HTTPS source, the retry ladder, the
//! resume ladder, and the cache the bytes land in. Nothing here sleeps, because
//! the wait is supplied by the test.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test setup, where a failure to build the input is the assertion"
)]

use clap as _;
use clap_complete as _;
use fetchloom_archive as _;
use fetchloom_platform as _;
use flate2 as _;
use serde as _;
use serde_json as _;
use toml as _;

use std::io::Read as _;
use std::num::NonZeroUsize;
use std::sync::Mutex;
use std::time::Duration;

use fetchloom_cache::Cache;
use fetchloom_cli::run::{Materialization, materialize_remote};
use fetchloom_engine::degrade::DegradeQueue;
use fetchloom_engine::digest::ContentDigest;
use fetchloom_engine::durability::DurabilityTier;
use fetchloom_engine::error::ErrorKind;
use fetchloom_engine::event::{EventPayload, Sequence};
use fetchloom_engine::hashing::hash_bytes;
use fetchloom_engine::limits::Limits;
use fetchloom_engine::pool::Processor;
use fetchloom_engine::resume::ResumeRung;
use fetchloom_engine::seam::store::Store;
use fetchloom_engine::selection::Selection;
use fetchloom_engine::threads::ThreadBudget;
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
        0.0
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
    fn new() -> Self {
        let root = TempDir::new().unwrap();
        let work = std::sync::Arc::new(fetchloom_engine::work::WorkCounter::new());
        let cache = Cache::open(
            root.path().join("cache"),
            NativePlatform::new(),
            DurabilityTier::Fast,
            VerificationPolicy::Fingerprint,
            std::sync::Arc::clone(&work),
        )
        .unwrap();
        Self {
            _root: root,
            cache,
            source: HttpSource::new(Limits::default(), work),
            pause: CountedPause::default(),
            limits: Limits::default(),
            degradations: DegradeQueue::new(),
            observer: RecordingObserver::new(),
            sequence: Sequence::new(),
        }
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

    fn events(&self) -> Vec<String> {
        self.observer
            .events()
            .iter()
            .map(|event| event.name().to_owned())
            .collect()
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

#[test]
fn a_whole_object_arrives_and_hashes_to_what_was_expected() {
    let bytes = object(64 * 1024);
    let server = TestServer::start(Script::serving(bytes.clone())).unwrap();
    let harness = Harness::new();

    let done = harness
        .transfer()
        .run(Some(digest_of(&bytes)), &at(&server))
        .unwrap();

    assert_eq!(done.bytes_transferred, bytes.len() as u64);
    assert_eq!(done.bytes_kept, 0);
    assert_eq!(done.attempts, 1);
    assert!(harness.cache.contains(digest_of(&bytes)).unwrap());
}

#[test]
fn bytes_that_do_not_hash_to_the_expected_digest_are_refused() {
    let bytes = object(4096);
    let server = TestServer::start(
        Script::serving(bytes.clone()).replying(vec![Reply::Flipped { offset: 100 }]),
    )
    .unwrap();
    let harness = Harness::new();

    let failure = harness
        .transfer()
        .run(Some(digest_of(&bytes)), &at(&server))
        .unwrap_err();

    assert_eq!(failure.kind(), ErrorKind::IntegrityMismatch);
    assert!(!harness.cache.contains(digest_of(&bytes)).unwrap());
}

#[test]
fn a_transient_status_is_retried_and_the_wait_grows() {
    let bytes = object(4096);
    let server = TestServer::start(Script::serving(bytes.clone()).replying(vec![
        Reply::Status {
            code: 503,
            retry_after: None,
        },
        Reply::Status {
            code: 503,
            retry_after: None,
        },
    ]))
    .unwrap();
    let harness = Harness::new();

    let done = harness
        .transfer()
        .run(Some(digest_of(&bytes)), &at(&server))
        .unwrap();

    assert_eq!(done.attempts, 3, "the transfer did not retry twice");
    assert_eq!(harness.pause.waits().len(), 2, "it did not wait twice");
    assert!(
        harness
            .events()
            .iter()
            .filter(|name| *name == "transfer.retry")
            .count()
            == 2,
        "the retries were not reported: {:?}",
        harness.events()
    );
}

#[test]
fn a_terminal_status_is_not_retried() {
    let server = TestServer::start(Script::serving(Vec::new()).replying(vec![Reply::Status {
        code: 404,
        retry_after: None,
    }]))
    .unwrap();
    let harness = Harness::new();

    let failure = harness
        .transfer()
        .run(Some(digest_of(b"anything")), &at(&server))
        .unwrap_err();

    assert_eq!(failure.kind(), ErrorKind::NetworkStatus);
    assert_eq!(failure.attempts(), 1, "a terminal status was retried");
    assert!(
        harness.pause.waits().is_empty(),
        "it waited before giving up"
    );
}

#[test]
fn a_transfer_leaves_a_dead_source_for_the_next_one() {
    let bytes = object(4096);
    let dead = TestServer::start(Script::serving(Vec::new()).replying(vec![Reply::Status {
        code: 403,
        retry_after: None,
    }]))
    .unwrap();
    let alive = TestServer::start(Script::serving(bytes.clone())).unwrap();
    let harness = Harness::new();

    let locations = vec![
        format!("{}/object", dead.origin()),
        format!("{}/object", alive.origin()),
    ];
    let done = harness
        .transfer()
        .run(Some(digest_of(&bytes)), &locations)
        .unwrap();

    assert_eq!(done.bytes_transferred, bytes.len() as u64);
    assert!(
        harness.events().contains(&"source.failover".to_owned()),
        "the failover was not reported: {:?}",
        harness.events()
    );
}

#[test]
fn an_interrupted_transfer_resumes_from_what_is_already_on_disk() {
    let bytes = object(64 * 1024);
    let server = TestServer::start(
        Script::serving(bytes.clone())
            .tagged(vec!["\"one\"".to_owned()])
            .replying(vec![Reply::ClosedMidBody { after: 16 * 1024 }]),
    )
    .unwrap();
    let harness = Harness::new();

    let done = harness
        .transfer()
        .run(Some(digest_of(&bytes)), &at(&server))
        .unwrap();

    assert_eq!(done.rung, ResumeRung::StrongValidator);
    assert_eq!(done.bytes_kept, 16 * 1024, "the transfer restarted");
    assert_eq!(done.bytes_transferred, (bytes.len() - 16 * 1024) as u64);
    assert!(harness.cache.contains(digest_of(&bytes)).unwrap());
}

#[test]
fn a_validator_that_changes_mid_resume_discards_what_was_kept() {
    let bytes = object(64 * 1024);
    let server = TestServer::start(
        Script::serving(bytes.clone())
            .tagged(vec!["\"one\"".to_owned(), "\"two\"".to_owned()])
            .replying(vec![Reply::ClosedMidBody { after: 16 * 1024 }]),
    )
    .unwrap();
    let harness = Harness::new();

    let done = harness
        .transfer()
        .run(Some(digest_of(&bytes)), &at(&server))
        .unwrap();

    assert_eq!(done.rung, ResumeRung::NoValidator);
    assert_eq!(done.bytes_kept, 0, "bytes were kept across a changed tag");
    assert_eq!(done.bytes_transferred, bytes.len() as u64);
}

#[test]
fn a_source_with_no_validator_restarts_rather_than_appending() {
    let bytes = object(64 * 1024);
    let server = TestServer::start(
        Script::serving(bytes.clone()).replying(vec![Reply::ClosedMidBody { after: 16 * 1024 }]),
    )
    .unwrap();
    let harness = Harness::new();

    let done = harness
        .transfer()
        .run(Some(digest_of(&bytes)), &at(&server))
        .unwrap();

    assert_eq!(done.rung, ResumeRung::NoValidator);
    assert_eq!(done.bytes_kept, 0);
}

#[test]
fn a_large_transfer_interrupted_twenty_times_completes_and_never_restarts_from_zero() {
    let bytes = object(2 * 1024 * 1024);
    let interruptions = 20;
    let step = bytes.len() / (interruptions + 1);
    let replies: Vec<Reply> = (1..=interruptions)
        .map(|which| Reply::ClosedMidBody {
            after: step * which,
        })
        .collect();
    let server = TestServer::start(
        Script::serving(bytes.clone())
            .tagged(vec!["\"stable\"".to_owned()])
            .replying(replies),
    )
    .unwrap();

    let harness = Harness::new();
    let limits = Limits {
        retry_attempts: u32::try_from(interruptions).unwrap() + 2,
        ..Limits::default()
    };
    let transfer = Transfer {
        store: &harness.cache,
        source: &harness.source,
        pause: &harness.pause,
        limits: &limits,
        degradations: &harness.degradations,
        observer: &harness.observer,
        sequence: &harness.sequence,
    };

    let done = transfer.run(Some(digest_of(&bytes)), &at(&server)).unwrap();

    assert!(harness.cache.contains(digest_of(&bytes)).unwrap());
    assert_eq!(
        done.rung,
        ResumeRung::StrongValidator,
        "the transfer fell below the fourth rung"
    );
    assert!(
        done.bytes_kept > 0,
        "the last attempt restarted from zero after {interruptions} interruptions"
    );
    assert_eq!(
        harness
            .degradations
            .take()
            .iter()
            .filter(|entry| entry.used == "a transfer from zero")
            .count(),
        0,
        "the transfer restarted from zero at least once"
    );
}

fn materialization<'a>(
    processor: &'a Processor,
    platform: &'a NativePlatform,
    cache: &'a Cache<NativePlatform>,
    work: &'a std::sync::Arc<fetchloom_engine::work::WorkCounter>,
) -> Materialization<'a> {
    Materialization {
        processor,
        platform,
        durability: DurabilityTier::Fast,
        cache: Some(cache),
        work,
        extract: true,
    }
}

#[test]
fn a_bare_url_with_no_known_digest_resumes_its_second_run_from_its_first() {
    let bytes = object(256 * 1024);
    let script = Script::serving(bytes.clone())
        .tagged(vec!["\"one\"".to_owned()])
        .replying(vec![Reply::ClosedMidBody { after: 8 * 1024 }; 5]);
    let server = TestServer::start(script).unwrap();
    let location = format!("{}/object", server.origin());

    let root = TempDir::new().unwrap();
    let work = std::sync::Arc::new(fetchloom_engine::work::WorkCounter::new());
    let cache = Cache::open(
        root.path().join("cache"),
        NativePlatform::new(),
        DurabilityTier::Fast,
        VerificationPolicy::Fingerprint,
        std::sync::Arc::clone(&work),
    )
    .unwrap();
    let platform = NativePlatform::new();
    let processor =
        Processor::new(ThreadBudget::resolve(NonZeroUsize::new(2).unwrap(), None)).unwrap();
    let with = materialization(&processor, &platform, &cache, &work);
    let destination = root.path().join("dest").join("object");

    let first_observer = RecordingObserver::new();
    let first_sequence = Sequence::new();
    let first = materialize_remote(
        &with,
        &location,
        &destination,
        &Selection::default(),
        &first_observer,
        &first_sequence,
    );
    assert!(
        first.is_err(),
        "the first run did not exit non-zero after its retries were spent"
    );
    assert!(
        !destination.exists(),
        "a destination appeared after a run that never finished a transfer"
    );

    let second_observer = RecordingObserver::new();
    let second_sequence = Sequence::new();
    let second = materialize_remote(
        &with,
        &location,
        &destination,
        &Selection::default(),
        &second_observer,
        &second_sequence,
    )
    .expect("the second run did not complete against a source serving the object whole");

    let resumed = second_observer.events().into_iter().find_map(|event| {
        if let EventPayload::TransferResume { rung, bytes_kept } = *event.payload() {
            Some((rung, bytes_kept))
        } else {
            None
        }
    });
    let (rung, bytes_kept) = resumed.expect("the second run reported no transfer.resume event");
    assert_eq!(rung, ResumeRung::StrongValidator);
    assert_eq!(rung.number(), 3);
    assert!(bytes_kept > 0, "the second run kept nothing from the first");

    let mut found = Vec::new();
    std::fs::File::open(destination.join("object"))
        .unwrap()
        .read_to_end(&mut found)
        .unwrap();
    assert_eq!(
        found, bytes,
        "the destination did not materialize the whole object"
    );
    assert_eq!(second.bytes, bytes.len() as u64);
}
