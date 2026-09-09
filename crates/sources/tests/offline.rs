//! The second enforcement contracts.md asks for: an adapter refuses at the
//! point it would open a connection, whatever resolved to the location.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test setup, where a failure to read this file is the assertion"
)]

use fetchloom_faults as _;
use rustls as _;
use rustls_graviola as _;
use rustls_platform_verifier as _;
use serde_json as _;
use sha2 as _;
use ureq as _;

use std::sync::Arc;

use fetchloom_engine::error::ErrorKind;
use fetchloom_engine::limits::Limits;
use fetchloom_engine::network;
use fetchloom_engine::seam::source::Source;
use fetchloom_engine::work::WorkCounter;
use fetchloom_sources::FtpSource;

#[test]
fn an_ftp_control_connection_is_refused_by_the_latch_rather_than_attempted() {
    let ftp = FtpSource::new(Limits::default(), Arc::new(WorkCounter::new()));
    let location = "ftp://ftp.example.invalid/pub/README";

    network::forbid();

    let refused = ftp
        .probe(location, None)
        .expect_err("an offline run opened a control connection");
    assert_eq!(
        refused.kind(),
        ErrorKind::PolicyOffline,
        "the run reached the host and reported what the socket said rather than refusing first: {}",
        refused.next_action()
    );
}

/// `network::forbid()` is a latch with no unset, so this target holds one test
/// beside this one and cannot grow: a second would inherit a forbidden network
/// and the order they ran in would decide the answer.
#[test]
fn this_target_holds_one_test_because_the_latch_it_sets_cannot_be_unset() {
    let here = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("offline.rs"),
    )
    .unwrap();
    let tests = here
        .lines()
        .filter(|line| line.trim_end() == "#[test]")
        .count();
    assert_eq!(
        tests, 2,
        "offline.rs holds {tests} tests beside this one, and the latch it sets makes their order decide the outcome"
    );
}
