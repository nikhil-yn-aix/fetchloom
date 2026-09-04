//! Contract tests over a transfer that moves real bytes into a real store.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test setup, where a failure to build the input is the assertion"
)]

use clap as _;
use clap_complete as _;
use ctrlc as _;
use fetchloom_archive as _;
use fetchloom_platform as _;
use fetchloom_view as _;
use flate2 as _;
#[cfg(unix)]
use rustix as _;
use serde as _;
use serde_json as _;
use toml as _;
#[cfg(windows)]
use windows_sys as _;

use std::io::Read as _;
use std::num::{NonZeroU32, NonZeroUsize};
use std::sync::Mutex;
use std::time::Duration;

use fetchloom_cache::Cache;
use fetchloom_cli::run::{Materialization, materialize_remote};
use fetchloom_engine::degrade::DegradeQueue;
use fetchloom_engine::digest::ContentDigest;
use fetchloom_engine::durability::DurabilityTier;
use fetchloom_engine::error::ErrorKind;
use fetchloom_engine::event::{EventPayload, Sequence};
use fetchloom_engine::flights::Flights;
use fetchloom_engine::hashing::hash_bytes;
use fetchloom_engine::limits::Limits;
use fetchloom_engine::pool::Processor;
use fetchloom_engine::redact::SafeUrl;
use fetchloom_engine::resume::ResumeRung;
use fetchloom_engine::seam::policy::IoMode;
use fetchloom_engine::seam::store::Store;
use fetchloom_engine::selection::Selection;
use fetchloom_engine::threads::ThreadBudget;
use fetchloom_engine::transfer::{Pause, Transfer};
use fetchloom_engine::tuning::{Ceilings, Controller};
use fetchloom_engine::verification::VerificationPolicy;
use fetchloom_faults::{RecordingObserver, Reply, Script, TestServer};
use fetchloom_platform::NativePlatform;
use fetchloom_sources::HttpSource;
use tempfile::TempDir;

mod support;

use support::NoCredentialPolicy;

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
    flights: Flights<'static>,
}

impl Harness {
    fn new() -> Self {
        let root = TempDir::new().unwrap();
        let work = std::sync::Arc::new(fetchloom_engine::work::WorkCounter::new());
        let cache = Cache::open(
            root.path().join("cache"),
            NativePlatform::new(std::sync::Arc::new(
                fetchloom_engine::work::WorkCounter::new(),
            )),
            DurabilityTier::Fast,
            VerificationPolicy::Fingerprint,
            IoMode::Buffered,
            std::sync::Arc::clone(&work),
            test_processor(),
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
            flights: Flights::new(one_at_a_time(), |_: &str| {
                Controller::fixed(NonZeroU32::MIN)
            }),
        }
    }

    fn transfer(&self) -> Transfer<'_, HttpSource, Cache<NativePlatform>, CountedPause> {
        Transfer {
            store: &self.cache,
            source: &self.source,
            pause: &self.pause,
            limits: &self.limits,
            degradations: &self.degradations,
            measurement: &|_location: &str| None,
            observer: &self.observer,
            sequence: &self.sequence,
            flights: &self.flights,
            meter: None,
            credential: &|_: &str| Ok(None),
            offer: &|_: &str, _: std::time::Duration| {},
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

fn nothing_prior(_location: &str) -> Option<fetchloom_engine::transfer::Prior> {
    None
}

#[test]
fn a_whole_object_arrives_and_hashes_to_what_was_expected() {
    let bytes = object(64 * 1024);
    let server = TestServer::start(Script::serving(bytes.clone())).unwrap();
    let harness = Harness::new();

    let done = harness
        .transfer()
        .run(Some(digest_of(&bytes)), &nothing_prior, &at(&server))
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
        .run(Some(digest_of(&bytes)), &nothing_prior, &at(&server))
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
        .run(Some(digest_of(&bytes)), &nothing_prior, &at(&server))
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
        .run(Some(digest_of(b"anything")), &nothing_prior, &at(&server))
        .unwrap_err();

    assert_eq!(failure.kind(), ErrorKind::NetworkStatus);
    assert_eq!(failure.attempts(), 1, "a terminal status was retried");
    assert!(
        harness.pause.waits().is_empty(),
        "it waited before giving up"
    );
}
#[test]
fn a_transfer_leaves_a_source_that_served_the_wrong_bytes_for_the_next_one() {
    let bytes = object(4096);
    let wrong = TestServer::start(
        Script::serving(bytes.clone()).replying(vec![Reply::Flipped { offset: 0 }]),
    )
    .unwrap();
    let alive = TestServer::start(Script::serving(bytes.clone())).unwrap();
    let harness = Harness::new();

    let locations = vec![
        format!("{}/object", wrong.origin()),
        format!("{}/object", alive.origin()),
    ];
    let done = harness
        .transfer()
        .run(Some(digest_of(&bytes)), &nothing_prior, &locations)
        .unwrap();

    assert_eq!(done.bytes_transferred, bytes.len() as u64);
    assert!(
        harness.events().contains(&"source.failover".to_owned()),
        "the failover was not reported: {:?}",
        harness.events()
    );
}

#[test]
fn a_failover_records_a_degradation_naming_both_sources_and_the_reason() {
    let bytes = object(4096);
    let wrong = TestServer::start(
        Script::serving(bytes.clone()).replying(vec![Reply::Flipped { offset: 0 }]),
    )
    .unwrap();
    let alive = TestServer::start(Script::serving(bytes.clone())).unwrap();
    let harness = Harness::new();

    let wrong_location = format!("{}/object", wrong.origin());
    let alive_location = format!("{}/object", alive.origin());
    let locations = vec![wrong_location.clone(), alive_location.clone()];
    let done = harness
        .transfer()
        .run(Some(digest_of(&bytes)), &nothing_prior, &locations)
        .unwrap();
    assert_eq!(done.bytes_transferred, bytes.len() as u64);

    let fell = harness.degradations.take();
    let wrong_safe = SafeUrl::new(&wrong_location).to_string();
    let alive_safe = SafeUrl::new(&alive_location).to_string();
    assert!(
        fell.iter()
            .any(|entry| entry.requested.contains(&wrong_safe) && entry.used.contains(&alive_safe)),
        "no degradation named the source left ({wrong_safe}) and the source taken ({alive_safe}): {fell:?}"
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
        .run(Some(digest_of(&bytes)), &nothing_prior, &at(&server))
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
        .run(Some(digest_of(&bytes)), &nothing_prior, &at(&server))
        .unwrap();

    assert_eq!(done.rung, ResumeRung::NoValidator);
    assert_eq!(done.bytes_kept, 0, "bytes were kept across a changed tag");
    assert_eq!(done.bytes_transferred, bytes.len() as u64);

    let fell = harness.degradations.take();
    let named = fell
        .iter()
        .find(|entry| entry.used.ends_with("a transfer from zero"))
        .expect("the restart was silent, which is the thing a degrade exists to prevent");
    assert_eq!(named.requested, "a resume on rung 3");
    assert_eq!(named.used, "rung 5, a transfer from zero");
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
        .run(Some(digest_of(&bytes)), &nothing_prior, &at(&server))
        .unwrap();

    assert_eq!(done.rung, ResumeRung::NoValidator);
    assert_eq!(done.bytes_kept, 0);
}

#[test]
fn a_source_without_range_support_names_that_as_the_reason_it_restarts() {
    let bytes = object(64 * 1024);
    let server = TestServer::start(
        Script::serving(bytes.clone())
            .ranges(false)
            .replying(vec![Reply::ClosedMidBody { after: 16 * 1024 }]),
    )
    .unwrap();
    let harness = Harness::new();

    let done = harness
        .transfer()
        .run(Some(digest_of(&bytes)), &nothing_prior, &at(&server))
        .unwrap();

    assert_eq!(done.rung, ResumeRung::NoValidator);
    assert_eq!(
        done.bytes_kept, 0,
        "bytes were kept from a source with no range support"
    );

    let fell = harness.degradations.take();
    let named = fell
        .iter()
        .find(|entry| entry.reason == "the source does not serve ranges")
        .expect("no degradation named the missing range support as the reason for the restart");
    assert_eq!(named.requested, "a resume by range");
    assert_eq!(named.used, "the whole object, transferred again from zero");
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
        measurement: &|_location: &str| None,
        observer: &harness.observer,
        sequence: &harness.sequence,
        flights: &harness.flights,
        meter: None,
        credential: &|_: &str| Ok(None),
        offer: &|_: &str, _: std::time::Duration| {},
    };

    let done = transfer
        .run(Some(digest_of(&bytes)), &nothing_prior, &at(&server))
        .unwrap();

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
            .filter(|entry| entry.used.ends_with("a transfer from zero"))
            .count(),
        0,
        "the transfer restarted from zero at least once"
    );
}

fn test_tuning() -> fetchloom_cli::run::Tuning {
    fetchloom_cli::run::Tuning {
        ceilings: Ceilings {
            global: NonZeroU32::MIN,
            per_host: NonZeroU32::MIN,
        },
        adapts: true,
        bandwidth: None,
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "each argument names one field of the materialization a test runs under"
)]
fn materialization<'a>(
    processor: &'a Processor,
    platform: &'a NativePlatform,
    cache: &'a Cache<NativePlatform>,
    work: &'a std::sync::Arc<fetchloom_engine::work::WorkCounter>,
    digester: &'a std::sync::Mutex<fetchloom_engine::hashing::Digester>,
    tuning: &'a fetchloom_cli::run::Tuning,
    policy: &'a NoCredentialPolicy,
    adapters: &'a fetchloom_engine::erased::Adapters,
) -> Materialization<'a> {
    Materialization {
        processor,
        platform,
        durability: DurabilityTier::Fast,
        cache: Some(cache),
        work,
        extract: true,
        verify: fetchloom_engine::verification::VerificationPolicy::Fingerprint,
        digester,
        tuning,
        policy,
        adapters,
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
        NativePlatform::new(std::sync::Arc::new(
            fetchloom_engine::work::WorkCounter::new(),
        )),
        DurabilityTier::Fast,
        VerificationPolicy::Fingerprint,
        IoMode::Buffered,
        std::sync::Arc::clone(&work),
        test_processor(),
    )
    .unwrap();
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    let processor =
        Processor::new(ThreadBudget::resolve(NonZeroUsize::new(2).unwrap(), None)).unwrap();
    let digester = std::sync::Mutex::new(fetchloom_engine::hashing::Digester::new());
    let tuning = test_tuning();
    let policy = NoCredentialPolicy::default();
    let adapters =
        fetchloom_cli::run::adapters_for(&work, &fetchloom_engine::limits::Limits::default());
    let with = materialization(
        &processor, &platform, &cache, &work, &digester, &tuning, &policy, &adapters,
    );
    let destination = root.path().join("dest").join("object");

    let first_observer = RecordingObserver::new();
    let first_sequence = Sequence::new();
    let first = materialize_remote(
        &with,
        &location,
        &destination,
        &Selection::default(),
        false,
        false,
        None,
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
        false,
        false,
        None,
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

#[test]
fn a_container_reference_lists_and_materializes_every_entry() {
    let bytes = object(11);
    let server = TestServer::start(
        Script::serving(bytes.clone()).replying(vec![Reply::Listing {
            format: fetchloom_faults::IndexFormat::ObjectStore,
        }]),
    )
    .unwrap();
    let location = format!("{}/set/", server.origin());

    let root = TempDir::new().unwrap();
    let work = std::sync::Arc::new(fetchloom_engine::work::WorkCounter::new());
    let cache = Cache::open(
        root.path().join("cache"),
        NativePlatform::new(std::sync::Arc::new(
            fetchloom_engine::work::WorkCounter::new(),
        )),
        DurabilityTier::Fast,
        VerificationPolicy::Fingerprint,
        IoMode::Buffered,
        std::sync::Arc::clone(&work),
        test_processor(),
    )
    .unwrap();
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    let processor =
        Processor::new(ThreadBudget::resolve(NonZeroUsize::new(2).unwrap(), None)).unwrap();
    let digester = std::sync::Mutex::new(fetchloom_engine::hashing::Digester::new());
    let tuning = test_tuning();
    let policy = NoCredentialPolicy::default();
    let adapters =
        fetchloom_cli::run::adapters_for(&work, &fetchloom_engine::limits::Limits::default());
    let with = materialization(
        &processor, &platform, &cache, &work, &digester, &tuning, &policy, &adapters,
    );
    let destination = root.path().join("dest");

    let observer = RecordingObserver::new();
    let sequence = Sequence::new();
    let result = fetchloom_cli::run::materialize_remote_container(
        &with,
        &location,
        &destination,
        &fetchloom_engine::selection::Selection::default(),
        false,
        false,
        &observer,
        &sequence,
    )
    .expect("a container reference did not materialize");

    assert_eq!(result.entries, 2, "not every listed entry was materialized");
    for name in ["one", "two"] {
        let mut found = Vec::new();
        std::fs::File::open(destination.join(name))
            .expect("the entry was not materialized")
            .read_to_end(&mut found)
            .unwrap();
        assert_eq!(found, bytes, "{name} did not carry the object's bytes");
    }

    let listed = observer
        .events()
        .into_iter()
        .any(|event| event.name() == "listing.end");
    assert!(listed, "the container was materialized without listing it");
}

fn test_processor() -> std::sync::Arc<fetchloom_engine::pool::Processor> {
    let budget = fetchloom_engine::threads::ThreadBudget::resolve(
        std::thread::available_parallelism().unwrap_or(std::num::NonZeroUsize::MIN),
        None,
    );
    std::sync::Arc::new(fetchloom_engine::pool::Processor::new(budget).unwrap())
}

struct CollapsingVolume<'a> {
    inner: &'a Cache<NativePlatform>,
    buffers: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

struct SlowWriter {
    inner: fetchloom_cache::store::PartialWriter,
    buffers: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

const BEFORE_THE_COLLAPSE: u64 = 1024 * 1024;

impl std::io::Write for SlowWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let seen = self
            .buffers
            .fetch_add(bytes.len() as u64, std::sync::atomic::Ordering::Relaxed);
        if seen > BEFORE_THE_COLLAPSE {
            std::thread::sleep(Duration::from_nanos(500 * bytes.len() as u64));
        }
        self.inner.write(bytes)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

impl Store for CollapsingVolume<'_> {
    type Reader = <Cache<NativePlatform> as Store>::Reader;
    type Writer = SlowWriter;
    type Lease = <Cache<NativePlatform> as Store>::Lease;

    fn format_fingerprint(
        &self,
    ) -> Result<fetchloom_engine::identity::CacheFormatFingerprint, fetchloom_engine::error::Error>
    {
        self.inner.format_fingerprint()
    }

    fn contains(&self, digest: ContentDigest) -> Result<bool, fetchloom_engine::error::Error> {
        self.inner.contains(digest)
    }

    fn open(&self, digest: ContentDigest) -> Result<Self::Reader, fetchloom_engine::error::Error> {
        self.inner.open(digest)
    }

    fn lease(
        &self,
        key: fetchloom_engine::partial_key::PartialKey,
    ) -> Result<Self::Lease, fetchloom_engine::error::Error> {
        self.inner.lease(key)
    }

    fn waited(&self, lease: &Self::Lease) -> bool {
        self.inner.waited(lease)
    }

    fn begin(
        &self,
        lease: &Self::Lease,
        length: u64,
    ) -> Result<Self::Writer, fetchloom_engine::error::Error> {
        Ok(SlowWriter {
            inner: self.inner.begin(lease, length)?,
            buffers: std::sync::Arc::clone(&self.buffers),
        })
    }

    fn resume(
        &self,
        lease: &Self::Lease,
        length: u64,
        valid: u64,
    ) -> Result<Self::Writer, fetchloom_engine::error::Error> {
        Ok(SlowWriter {
            inner: self.inner.resume(lease, length, valid)?,
            buffers: std::sync::Arc::clone(&self.buffers),
        })
    }

    fn record_source(
        &self,
        key: fetchloom_engine::partial_key::PartialKey,
        record: &fetchloom_engine::source_record::SourceRecord,
    ) -> Result<(), fetchloom_engine::error::Error> {
        self.inner.record_source(key, record)
    }

    fn recorded_source(
        &self,
        key: fetchloom_engine::partial_key::PartialKey,
    ) -> Result<Option<fetchloom_engine::source_record::SourceRecord>, fetchloom_engine::error::Error>
    {
        self.inner.recorded_source(key)
    }

    fn discard_partial(
        &self,
        key: fetchloom_engine::partial_key::PartialKey,
    ) -> Result<(), fetchloom_engine::error::Error> {
        self.inner.discard_partial(key)
    }

    fn commit(
        &self,
        lease: Self::Lease,
        writer: Self::Writer,
    ) -> Result<fetchloom_engine::hashing::Digests, fetchloom_engine::error::Error> {
        self.inner.commit(lease, writer.inner)
    }

    fn has_outboard(&self, digest: ContentDigest) -> Result<bool, fetchloom_engine::error::Error> {
        self.inner.has_outboard(digest)
    }

    fn open_outboard(
        &self,
        digest: ContentDigest,
    ) -> Result<Self::Reader, fetchloom_engine::error::Error> {
        self.inner.open_outboard(digest)
    }

    fn verified_prefix(
        &self,
        key: fetchloom_engine::partial_key::PartialKey,
        digest: ContentDigest,
        on_disk: u64,
    ) -> Result<u64, fetchloom_engine::error::Error> {
        self.inner.verified_prefix(key, digest, on_disk)
    }

    fn write_outboard(
        &self,
        digest: ContentDigest,
        tree: &[u8],
    ) -> Result<(), fetchloom_engine::error::Error> {
        self.inner.write_outboard(digest, tree)
    }

    fn stage(
        &self,
        destination_volume: &std::path::Path,
    ) -> Result<std::path::PathBuf, fetchloom_engine::error::Error> {
        self.inner.stage(destination_volume)
    }

    fn pin(&self, digest: ContentDigest) -> Result<(), fetchloom_engine::error::Error> {
        self.inner.pin(digest)
    }

    fn unpin(&self, digest: ContentDigest) -> Result<(), fetchloom_engine::error::Error> {
        self.inner.unpin(digest)
    }

    fn list(&self) -> Result<Vec<ContentDigest>, fetchloom_engine::error::Error> {
        self.inner.list()
    }

    fn prune(
        &self,
        grace: Duration,
    ) -> Result<fetchloom_engine::seam::store::PruneReport, fetchloom_engine::error::Error> {
        self.inner.prune(grace)
    }

    fn status(
        &self,
    ) -> Result<fetchloom_engine::seam::store::CacheStatus, fetchloom_engine::error::Error> {
        self.inner.status()
    }
}

#[test]
fn a_volume_that_collapses_mid_transfer_moves_the_same_bytes() {
    let bytes = object(4 * 1024 * 1024);
    let harness = Harness::new();

    let steady = TestServer::start(Script::serving(bytes.clone())).unwrap();
    let fast = harness
        .transfer()
        .run(Some(digest_of(&bytes)), &nothing_prior, &at(&steady))
        .unwrap();

    let collapsing = Harness::new();
    let volume = CollapsingVolume {
        inner: &collapsing.cache,
        buffers: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
    };
    let flights = Flights::new(
        fetchloom_engine::tuning::Ceilings {
            global: NonZeroU32::new(8).unwrap(),
            per_host: NonZeroU32::new(8).unwrap(),
        },
        |_: &str| Controller::start(Some(4), NonZeroU32::new(8).unwrap()),
    );
    let slow = TestServer::start(Script::serving(bytes.clone())).unwrap();
    let done = Transfer {
        store: &volume,
        source: &collapsing.source,
        pause: &collapsing.pause,
        limits: &collapsing.limits,
        degradations: &collapsing.degradations,
        measurement: &|_location| None,
        observer: &collapsing.observer,
        sequence: &collapsing.sequence,
        flights: &flights,
        meter: None,
        credential: &|_: &str| Ok(None),
        offer: &|_: &str, _: std::time::Duration| {},
    }
    .run(Some(digest_of(&bytes)), &nothing_prior, &at(&slow))
    .unwrap();

    assert_eq!(
        done.digest, fast.digest,
        "the collapsing volume changed the content digest"
    );
    assert_eq!(
        done.bytes_transferred, fast.bytes_transferred,
        "the collapsing volume changed how many bytes moved"
    );
}

#[test]
fn a_second_container_run_against_an_unchanged_destination_writes_nothing() {
    let bytes = object(11);
    let server = TestServer::start(Script::serving(bytes.clone()).replying(vec![
        Reply::Listing {
            format: fetchloom_faults::IndexFormat::ObjectStore,
        },
        Reply::Whole,
        Reply::Whole,
        Reply::Listing {
            format: fetchloom_faults::IndexFormat::ObjectStore,
        },
    ]))
    .unwrap();
    let location = format!("{}/set/", server.origin());

    let root = TempDir::new().unwrap();
    let work = std::sync::Arc::new(fetchloom_engine::work::WorkCounter::new());
    let cache = Cache::open(
        root.path().join("cache"),
        NativePlatform::new(std::sync::Arc::new(
            fetchloom_engine::work::WorkCounter::new(),
        )),
        DurabilityTier::Fast,
        VerificationPolicy::Fingerprint,
        IoMode::Buffered,
        std::sync::Arc::clone(&work),
        test_processor(),
    )
    .unwrap();
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    let processor =
        Processor::new(ThreadBudget::resolve(NonZeroUsize::new(2).unwrap(), None)).unwrap();
    let digester = std::sync::Mutex::new(fetchloom_engine::hashing::Digester::new());
    let tuning = test_tuning();
    let policy = NoCredentialPolicy::default();
    let adapters =
        fetchloom_cli::run::adapters_for(&work, &fetchloom_engine::limits::Limits::default());
    let with = materialization(
        &processor, &platform, &cache, &work, &digester, &tuning, &policy, &adapters,
    );
    let destination = root.path().join("dest");
    let observer = RecordingObserver::new();
    let sequence = Sequence::new();

    let first = fetchloom_cli::run::materialize_remote_container(
        &with,
        &location,
        &destination,
        &fetchloom_engine::selection::Selection::default(),
        false,
        false,
        &observer,
        &sequence,
    )
    .expect("a container reference did not materialize");
    assert_eq!(
        first.status,
        fetchloom_engine::outcome::RunStatus::Materialized
    );

    let second = fetchloom_cli::run::materialize_remote_container(
        &with,
        &location,
        &destination,
        &fetchloom_engine::selection::Selection::default(),
        false,
        false,
        &observer,
        &sequence,
    )
    .expect("a second container run against an unchanged destination failed");

    assert_eq!(
        second.status,
        fetchloom_engine::outcome::RunStatus::Unchanged,
        "a container run against a destination it already holds did not reconcile"
    );
    assert_eq!(
        second.tree, first.tree,
        "the second run reported a different tree"
    );
}

#[test]
fn a_selection_matching_no_listed_entry_is_an_error_rather_than_an_empty_destination() {
    let bytes = object(11);
    let server = TestServer::start(Script::serving(bytes).replying(vec![Reply::Listing {
        format: fetchloom_faults::IndexFormat::ObjectStore,
    }]))
    .unwrap();
    let location = format!("{}/set/", server.origin());

    let root = TempDir::new().unwrap();
    let work = std::sync::Arc::new(fetchloom_engine::work::WorkCounter::new());
    let cache = Cache::open(
        root.path().join("cache"),
        NativePlatform::new(std::sync::Arc::new(
            fetchloom_engine::work::WorkCounter::new(),
        )),
        DurabilityTier::Fast,
        VerificationPolicy::Fingerprint,
        IoMode::Buffered,
        std::sync::Arc::clone(&work),
        test_processor(),
    )
    .unwrap();
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    let processor =
        Processor::new(ThreadBudget::resolve(NonZeroUsize::new(2).unwrap(), None)).unwrap();
    let digester = std::sync::Mutex::new(fetchloom_engine::hashing::Digester::new());
    let tuning = test_tuning();
    let policy = NoCredentialPolicy::default();
    let adapters =
        fetchloom_cli::run::adapters_for(&work, &fetchloom_engine::limits::Limits::default());
    let with = materialization(
        &processor, &platform, &cache, &work, &digester, &tuning, &policy, &adapters,
    );
    let destination = root.path().join("dest");
    let observer = RecordingObserver::new();
    let sequence = Sequence::new();

    let selection = fetchloom_engine::selection::Selection {
        include: vec![fetchloom_engine::selection::Glob::new(
            "nothing-matches-this",
        )],
        exclude: Vec::new(),
        layout: fetchloom_engine::selection::Layout::Keep,
    };
    let refused = fetchloom_cli::run::materialize_remote_container(
        &with,
        &location,
        &destination,
        &selection,
        false,
        false,
        &observer,
        &sequence,
    )
    .expect_err("a selection matching no listed entry produced a destination");

    assert_eq!(refused.kind(), ErrorKind::ReferenceUnresolved);
    assert!(
        !destination.exists(),
        "a destination was published for a selection that matched nothing"
    );
}

fn one_at_a_time() -> fetchloom_engine::tuning::Ceilings {
    fetchloom_engine::tuning::Ceilings {
        global: std::num::NonZeroU32::MIN,
        per_host: std::num::NonZeroU32::MIN,
    }
}
