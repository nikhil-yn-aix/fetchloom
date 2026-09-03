//! What a run does with one large object a source will serve in parts.

#![expect(
    clippy::unwrap_used,
    reason = "test assertions, where the run that failed is the message"
)]

use clap as _;
use clap_complete as _;
use ctrlc as _;
use fetchloom_archive as _;
use fetchloom_cache::Cache;
use fetchloom_cli::cache as cache_cli;
use fetchloom_engine::degrade::DegradeQueue;
use fetchloom_engine::event::Sequence;
use fetchloom_engine::flights::Flights;
use fetchloom_engine::hashing::hash_bytes;
use fetchloom_engine::limits::Limits;
use fetchloom_engine::pool::Processor;
use fetchloom_engine::seam::observer::Observer;
use fetchloom_engine::threads::ThreadBudget;
use fetchloom_engine::transfer::{SleepingPause, Transfer};
use fetchloom_engine::tuning::{Ceilings, Controller, HostMeasurement};
use fetchloom_engine::work::WorkCounter;
use fetchloom_platform as _;
use fetchloom_platform::NativePlatform;
use fetchloom_sources::ObjectStoreSource;
use fetchloom_view as _;
use flate2 as _;
#[cfg(unix)]
use rustix as _;
use serde as _;
use serde_json as _;
use toml as _;
#[cfg(windows)]
use windows_sys as _;

use std::num::NonZeroU32;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;

use fetchloom_faults::{Script, TestServer};
use tempfile::TempDir;

/// How wide a run has to have measured a host before it splits one object
/// across that host.
const MEASURED_WIDTH: u32 = 4;

/// The header an object store answers with when the version it served cannot
/// change under the same name.
const VERSION_HEADER: &str = "x-amz-version-id";

fn large() -> Vec<u8> {
    let length = usize::try_from(Limits::default().split_threshold).unwrap() + 4096;
    (0..length)
        .map(|index| u8::try_from(index % 251).unwrap_or(0))
        .collect()
}

/// An observer that keeps nothing, because these tests read the store and the
/// requests the server saw rather than the stream.
struct Quiet;

impl Observer for Quiet {
    fn emit(&self, _event: &fetchloom_engine::event::Event) {}
}

fn cache_at(root: &Path) -> Cache<NativePlatform> {
    let work = Arc::new(WorkCounter::new());
    let processor = Arc::new(
        Processor::new(ThreadBudget::resolve(
            std::thread::available_parallelism().unwrap_or(std::num::NonZeroUsize::MIN),
            None,
        ))
        .unwrap(),
    );
    cache_cli::require(root, work, processor).unwrap()
}

fn measurement(concurrency: u32) -> HostMeasurement {
    HostMeasurement {
        concurrency,
        throughput: 1_000_000,
        time_to_first_byte_ms: 1,
        observed_at: fetchloom_engine::timestamp::Timestamp::now(),
    }
}

/// Transfers one object through the object store adapter, which is the adapter
/// that reports an identity a split may rely on, and returns the degradations
/// the transfer recorded.
fn transfer_with(
    server: &TestServer,
    width: u32,
) -> (fetchloom_engine::digest::ContentDigest, Vec<String>) {
    let scratch = TempDir::new().unwrap();
    let cache = cache_at(&scratch.path().join("cache"));
    let work = Arc::new(WorkCounter::new());
    let source = ObjectStoreSource::new(Limits::default(), work);
    let pause = SleepingPause;
    let limits = Limits::default();
    let degradations = DegradeQueue::new();
    let observer = Quiet;
    let sequence = Sequence::new();
    let flights = Flights::new(
        Ceilings {
            global: NonZeroU32::new(8).unwrap(),
            per_host: NonZeroU32::new(8).unwrap(),
        },
        move |_: &str| Controller::fixed(NonZeroU32::new(width.max(1)).unwrap()),
    );
    let measured = measurement(width);
    let done = Transfer {
        store: &cache,
        source: &source,
        pause: &pause,
        limits: &limits,
        degradations: &degradations,
        measurement: &move |_: &str| Some(measured),
        observer: &observer,
        sequence: &sequence,
        flights: &flights,
        meter: None,
        credential: &|_: &str| Ok(None),
        offer: &|_: &str, _: std::time::Duration| {},
    }
    .run(
        None,
        &|_: &str| None,
        &[format!("{}/object.bin", server.origin())],
    )
    .unwrap();
    (
        done.digest,
        degradations
            .take()
            .into_iter()
            .map(|entry| entry.reason)
            .collect(),
    )
}

#[test]
fn a_large_immutable_object_on_a_measured_host_is_fetched_as_several_ranges_at_once() {
    let bytes = large();
    let server = TestServer::start(Script::serving(bytes.clone()).headers(vec![(
        VERSION_HEADER.to_owned(),
        "an-immutable-version".to_owned(),
    )]))
    .unwrap();

    let (digest, said) = transfer_with(&server, MEASURED_WIDTH);

    let ranged = server
        .received()
        .into_iter()
        .filter(|request| request.method == "GET" && request.header("range").is_some())
        .count();
    assert_eq!(
        ranged, MEASURED_WIDTH as usize,
        "the object was not asked for as {MEASURED_WIDTH} ranges at once"
    );
    assert_eq!(
        digest,
        hash_bytes(&bytes),
        "a split object did not rejoin into the bytes the source served"
    );
    assert!(
        said.is_empty(),
        "a split that happened reported a degradation: {said:?}"
    );
}

#[test]
fn the_same_object_fetched_whole_and_in_parts_produces_one_digest() {
    let bytes = large();
    let server = TestServer::start(Script::serving(bytes.clone()).headers(vec![(
        VERSION_HEADER.to_owned(),
        "an-immutable-version".to_owned(),
    )]))
    .unwrap();

    let (split, _) = transfer_with(&server, MEASURED_WIDTH);
    let (whole, said) = transfer_with(&server, 1);

    assert_eq!(
        split, whole,
        "splitting one object changed the digest, which concurrency may never do"
    );
    assert_eq!(
        split,
        hash_bytes(&bytes),
        "neither run produced the bytes the source served"
    );
    assert!(
        said.contains(
            &"this run has not measured the host as serving more with more streams".to_owned()
        ),
        "the unsplit run did not say why it was not split: {said:?}"
    );
}

#[test]
fn a_large_object_a_source_will_not_serve_in_parts_says_why_it_was_not_split() {
    let bytes = large();
    let server = TestServer::start(Script::serving(bytes).ranges(false).headers(vec![(
        VERSION_HEADER.to_owned(),
        "an-immutable-version".to_owned(),
    )]))
    .unwrap();

    let (_, said) = transfer_with(&server, MEASURED_WIDTH);
    assert!(
        said.contains(&"the source does not serve ranges".to_owned()),
        "a split that was wanted and refused said nothing: {said:?}"
    );
}

#[test]
fn a_large_object_from_a_real_run_over_plain_http_says_why_it_was_not_split() {
    let bytes = large();
    let server = TestServer::start(Script::serving(bytes)).unwrap();
    let scratch = TempDir::new().unwrap();
    let events = scratch.path().join("events.ndjson");

    let run = Command::new(env!("CARGO_BIN_EXE_fetchloom"))
        .current_dir(scratch.path())
        .args([
            "get",
            &format!("{}/object.bin", server.origin()),
            "--output",
        ])
        .arg(scratch.path().join("out"))
        .arg("--events")
        .arg(&events)
        .env("FETCHLOOM_CACHE_DIR", scratch.path().join("cache"))
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(
        run.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );

    let reasons: Vec<String> = std::fs::read_to_string(&events)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|event| event["event"] == "degrade")
        .map(|event| event["reason"].as_str().unwrap_or_default().to_owned())
        .collect();
    assert!(
        reasons.contains(
            &"the source states no identity that cannot change under the same name".to_owned()
        ),
        "a real run that wanted a split and could not have one said nothing: {reasons:?}"
    );
}
