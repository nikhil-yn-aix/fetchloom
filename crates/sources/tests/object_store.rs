//! Contract tests over the object store source, driven against the
//! adversarial server.

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

use std::io::Read;

use fetchloom_engine::error::ErrorKind;
use fetchloom_engine::limits::Limits;
use fetchloom_engine::seam::source::{ByteRange, Cost, Source, SourceIdentity};
use fetchloom_faults::{IndexFormat, Reply, Script, TestServer};
use fetchloom_sources::ObjectStoreSource;

fn object() -> Vec<u8> {
    (0..1024u32)
        .map(|value| u8::try_from(value % 256).unwrap_or(0))
        .collect()
}

fn source() -> ObjectStoreSource {
    ObjectStoreSource::new(
        Limits::default(),
        std::sync::Arc::new(fetchloom_engine::work::WorkCounter::new()),
    )
}

fn read(body: impl Read) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut body = body;
    body.read_to_end(&mut bytes).unwrap();
    bytes
}

#[test]
fn a_version_identifier_reports_an_immutable_version_and_wins_over_an_etag() {
    let server = TestServer::start(
        Script::serving(object())
            .tagged(vec!["\"one\"".to_owned()])
            .headers(vec![("x-amz-version-id".to_owned(), "v7".to_owned())]),
    )
    .unwrap();
    let found = source()
        .probe(&format!("{}/object", server.origin()), None)
        .unwrap();

    assert_eq!(
        found.identity,
        SourceIdentity::ImmutableVersion("v7".to_owned())
    );
}

#[test]
fn a_strong_etag_alone_is_reported_as_a_strong_validator() {
    let server =
        TestServer::start(Script::serving(object()).tagged(vec!["\"one\"".to_owned()])).unwrap();
    let found = source()
        .probe(&format!("{}/object", server.origin()), None)
        .unwrap();

    assert_eq!(
        found.identity,
        SourceIdentity::StrongValidator("\"one\"".to_owned())
    );
}

#[test]
fn a_weak_etag_alone_is_reported_as_a_weak_validator() {
    let server =
        TestServer::start(Script::serving(object()).tagged(vec!["W/\"one\"".to_owned()])).unwrap();
    let found = source()
        .probe(&format!("{}/object", server.origin()), None)
        .unwrap();

    assert_eq!(
        found.identity,
        SourceIdentity::WeakValidator("W/\"one\"".to_owned())
    );
}

#[test]
fn neither_a_version_nor_an_etag_reports_no_identity() {
    let server = TestServer::start(Script::serving(object())).unwrap();
    let found = source()
        .probe(&format!("{}/object", server.origin()), None)
        .unwrap();

    assert_eq!(found.identity, SourceIdentity::None);
}

#[test]
fn a_store_advertising_ranges_reports_support_and_serves_a_span() {
    let server = TestServer::start(Script::serving(object())).unwrap();
    let source = source();
    let found = source
        .probe(&format!("{}/object", server.origin()), None)
        .unwrap();
    assert!(found.supports_ranges);

    let fetched = source
        .fetch(
            &format!("{}/object", server.origin()),
            Some(ByteRange {
                start: 512,
                end: 1024,
            }),
            None,
        )
        .unwrap();
    assert_eq!(read(fetched.body), object()[512..]);
}

#[test]
fn a_store_that_withholds_ranges_reports_no_support() {
    let server = TestServer::start(Script::serving(object()).ranges(false)).unwrap();
    let found = source()
        .probe(&format!("{}/object", server.origin()), None)
        .unwrap();

    assert!(!found.supports_ranges);
}

#[test]
fn a_requester_charged_response_reports_both_cost_facts_true() {
    let server = TestServer::start(Script::serving(object()).headers(vec![(
        "x-amz-request-charged".to_owned(),
        "requester".to_owned(),
    )]))
    .unwrap();
    let found = source()
        .probe(&format!("{}/object", server.origin()), None)
        .unwrap();

    assert_eq!(
        found.cost,
        Cost {
            egress_charged: Some(true),
            requester_pays: Some(true),
        }
    );
}

#[test]
fn no_request_charged_header_leaves_cost_unknown() {
    let server = TestServer::start(Script::serving(object())).unwrap();
    let found = source()
        .probe(&format!("{}/object", server.origin()), None)
        .unwrap();

    assert!(found.cost.is_unknown());
}

#[test]
fn an_object_store_listing_is_parsed_into_entries() {
    let server = TestServer::start(Script::serving(object()).replying(vec![Reply::Listing {
        format: IndexFormat::ObjectStore,
    }]))
    .unwrap();
    let listed = source()
        .list(&format!("{}/set/", server.origin()), None)
        .unwrap();
    let names: Vec<&str> = listed
        .entries
        .iter()
        .map(|entry| entry.path.as_str())
        .collect();
    assert_eq!(names, vec!["one", "two"]);
}

#[test]
fn an_unrecognized_body_is_refused_as_not_an_object_store_response() {
    let server = TestServer::start(Script::serving(object()).replying(vec![Reply::Listing {
        format: IndexFormat::Unrecognized,
    }]))
    .unwrap();
    let failure = source()
        .list(&format!("{}/set/", server.origin()), None)
        .unwrap_err();

    assert_eq!(failure.kind(), ErrorKind::ReferenceUnresolved);
}

#[test]
fn a_listing_past_the_entry_bound_fails_as_a_resource_limit() {
    let server = TestServer::start(Script::serving(Vec::new()).replying(vec![Reply::Listing {
        format: IndexFormat::ObjectStore,
    }]))
    .unwrap();
    let limits = Limits {
        listing_entries: 1,
        ..Limits::default()
    };
    let source = ObjectStoreSource::new(
        limits,
        std::sync::Arc::new(fetchloom_engine::work::WorkCounter::new()),
    );
    let failure = source
        .list(&format!("{}/prefix/", server.origin()), None)
        .unwrap_err();

    assert_eq!(failure.kind(), ErrorKind::ResourceLimit);
}

#[test]
fn every_index_format_the_contract_recognizes_is_listed_and_only_an_unrecognized_one_is_refused() {
    for format in [
        IndexFormat::ObjectStore,
        IndexFormat::WebDav,
        IndexFormat::GeneratedHtml,
    ] {
        let server = TestServer::start(
            Script::serving(Vec::new()).replying(vec![Reply::Listing { format }]),
        )
        .unwrap();
        let source = source();
        let listed = source.list(&format!("{}/set/", server.origin()), None);
        assert!(
            listed.is_ok(),
            "{format:?} is a format the contract recognizes and it was refused: {listed:?}"
        );
        let listed = listed.unwrap_or_default();
        assert_eq!(
            listed.entries.len(),
            2,
            "{format:?} did not list both entries"
        );
    }

    let server = TestServer::start(Script::serving(Vec::new()).replying(vec![Reply::Listing {
        format: IndexFormat::Unrecognized,
    }]))
    .unwrap();
    let source = source();
    let refused = source.list(&format!("{}/set/", server.origin()), None);

    assert!(
        refused.is_err(),
        "an index in no recognized format was listed: {refused:?}"
    );
    assert_eq!(
        refused.err().map(|error| error.kind()),
        Some(ErrorKind::ReferenceUnresolved)
    );
}

#[test]
fn a_source_refusing_for_want_of_authorization_is_a_missing_credential_rather_than_a_status() {
    for code in [401, 403] {
        let server = TestServer::start(Script::serving(object()).replying(vec![Reply::Status {
            code,
            retry_after: None,
        }]))
        .unwrap();
        let source = source();
        let refused = source
            .probe(&format!("{}/object", server.origin()), None)
            .expect_err("a source refusing for want of authorization answered");

        assert_eq!(
            refused.kind(),
            ErrorKind::PolicyCredentialMissing,
            "a {code} with no credential presented was reported as {:?}",
            refused.kind()
        );
        assert!(
            !refused.retryable(),
            "a {code} with no credential presented was reported as retryable"
        );
        assert!(
            refused.next_action().contains("FETCHLOOM_TOKEN_"),
            "the failure does not say where to put a token: {}",
            refused.next_action()
        );
    }
}
