//! Contract tests that no secret survives into anything a source writes.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test setup, where a failure to build the input is the assertion"
)]

use serde_json as _;
use sha2 as _;

use rustls as _;
use rustls_graviola as _;
use ureq as _;

use fetchloom_engine::limits::Limits;
use fetchloom_engine::seam::source::Source;
use fetchloom_engine::work::WorkCounter;
use fetchloom_faults::{IndexFormat, Reply, Script, TestServer};
use fetchloom_sources::{HttpSource, ObjectStoreSource};

use std::sync::Arc;

/// The value that must never appear anywhere a source writes.
const SECRET: &str = "s3cr3t-value-do-not-print";

fn http() -> HttpSource {
    HttpSource::new(Limits::default(), Arc::new(WorkCounter::new()))
}

fn everything(error: &fetchloom_engine::error::Error) -> String {
    format!(
        "{} {} {:?}",
        error.next_action(),
        error
            .source_location()
            .map_or_else(String::new, |held| held.as_str().to_owned()),
        error
    )
}

fn carries_no_secret(written: &str, what: &str) {
    assert!(
        !written.contains(SECRET),
        "{what} carried the secret: {written}"
    );
}

#[test]
fn a_userinfo_component_never_reaches_an_error_a_source_produces() {
    let location = format!("http://user:{SECRET}@nothing.invalid/object");
    let refused = http()
        .probe(&location, None)
        .expect_err("an unreachable host answered");

    carries_no_secret(&everything(&refused), "a probe failure");
}

#[test]
fn a_query_parameter_value_never_reaches_an_error_a_source_produces() {
    let location =
        format!("http://nothing.invalid/object?X-Amz-Signature={SECRET}&anything-at-all={SECRET}");
    let refused = http()
        .probe(&location, None)
        .expect_err("an unreachable host answered");

    carries_no_secret(&everything(&refused), "a probe failure");
}

#[test]
fn a_scheme_a_source_cannot_reach_never_repeats_the_secret_in_it() {
    let location = format!("ftp://user:{SECRET}@nothing.invalid/object?token={SECRET}");
    let refused = http()
        .probe(&location, None)
        .expect_err("a scheme no source reaches was accepted");

    carries_no_secret(&everything(&refused), "an unreachable scheme failure");
}

#[test]
fn an_index_in_no_recognized_format_never_repeats_the_secret_in_its_location() {
    let server = TestServer::start(Script::serving(Vec::new()).replying(vec![Reply::Listing {
        format: IndexFormat::Unrecognized,
    }]))
    .unwrap();
    let location = format!("{}/set/?signature={SECRET}", server.origin());

    let refused = http()
        .list(&location, None)
        .expect_err("an index in no recognized format was accepted");

    carries_no_secret(&everything(&refused), "an unrecognized index failure");
}

#[test]
fn an_object_store_refusing_a_body_never_repeats_the_secret_in_its_location() {
    let server = TestServer::start(Script::serving(Vec::new()).replying(vec![Reply::Listing {
        format: IndexFormat::Unrecognized,
    }]))
    .unwrap();
    let location = format!("{}/set/?signature={SECRET}", server.origin());

    let source = ObjectStoreSource::new(Limits::default(), Arc::new(WorkCounter::new()));
    let refused = source
        .list(&location, None)
        .expect_err("an index in no recognized format was accepted");

    carries_no_secret(&everything(&refused), "an object store listing failure");
}

#[test]
fn a_redirect_to_something_unfollowable_never_repeats_the_secret_it_points_at() {
    let server = TestServer::start(Script::serving(Vec::new()).replying(vec![Reply::Redirect {
        code: 307,
        location: format!("gopher://elsewhere.invalid/object?token={SECRET}"),
    }]))
    .unwrap();
    let location = format!("{}/object", server.origin());

    let refused = http()
        .fetch(&location, None, None)
        .expect_err("a redirect to a scheme no source reaches was followed");

    carries_no_secret(&everything(&refused), "an unfollowable redirect failure");
}
