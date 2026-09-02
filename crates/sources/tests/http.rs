//! Contract tests over the HTTPS source, driven against the adversarial server.

#![expect(
    clippy::unwrap_used,
    reason = "test setup, where a failure to build the input is the assertion"
)]

use rustls as _;
use rustls_graviola as _;
use ureq as _;

use std::io::Read;

use sha2 as _;

use fetchloom_engine::credential::{Credential, CredentialOrigin};
use fetchloom_engine::error::ErrorKind;
use fetchloom_engine::limits::Limits;
use fetchloom_engine::redact::Secret;
use fetchloom_engine::reference::Host;
use fetchloom_engine::seam::source::{ByteRange, Source, SourceIdentity};
use fetchloom_faults::{IndexFormat, Reply, Script, TestServer};
use fetchloom_sources::HttpSource;

fn object() -> Vec<u8> {
    (0..1024u32)
        .map(|value| u8::try_from(value % 256).unwrap_or(0))
        .collect()
}

fn read(body: impl Read) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut body = body;
    body.read_to_end(&mut bytes).unwrap();
    bytes
}

#[test]
fn a_probe_reports_the_length_the_validator_and_range_support() {
    let server =
        TestServer::start(Script::serving(object()).tagged(vec!["\"one\"".to_owned()])).unwrap();
    let source = HttpSource::new(
        Limits::default(),
        std::sync::Arc::new(fetchloom_engine::work::WorkCounter::new()),
    );
    let found = source
        .probe(&format!("{}/object", server.origin()), None)
        .unwrap();

    assert_eq!(found.size, Some(1024));
    assert!(found.supports_ranges);
    assert_eq!(
        found.identity,
        SourceIdentity::StrongValidator("\"one\"".to_owned())
    );
    assert_eq!(found.host.as_str(), "127.0.0.1");
}

#[test]
fn a_weak_validator_is_reported_as_weak() {
    let server =
        TestServer::start(Script::serving(object()).tagged(vec!["W/\"one\"".to_owned()])).unwrap();
    let source = HttpSource::new(
        Limits::default(),
        std::sync::Arc::new(fetchloom_engine::work::WorkCounter::new()),
    );
    let found = source
        .probe(&format!("{}/object", server.origin()), None)
        .unwrap();

    assert_eq!(
        found.identity,
        SourceIdentity::WeakValidator("W/\"one\"".to_owned())
    );
}

#[test]
fn a_source_that_withholds_range_support_is_reported_as_withholding_it() {
    let server = TestServer::start(Script::serving(object()).ranges(false)).unwrap();
    let source = HttpSource::new(
        Limits::default(),
        std::sync::Arc::new(fetchloom_engine::work::WorkCounter::new()),
    );
    let found = source
        .probe(&format!("{}/object", server.origin()), None)
        .unwrap();

    assert!(!found.supports_ranges);
    assert_eq!(found.identity, SourceIdentity::None);
}

#[test]
fn fetching_the_whole_object_returns_every_byte() {
    let server = TestServer::start(Script::serving(object())).unwrap();
    let source = HttpSource::new(
        Limits::default(),
        std::sync::Arc::new(fetchloom_engine::work::WorkCounter::new()),
    );
    let body = source
        .fetch(&format!("{}/object", server.origin()), None, None)
        .unwrap();

    assert_eq!(read(body.body), object());
}

#[test]
fn fetching_a_range_asks_for_it_and_returns_only_that_span() {
    let server = TestServer::start(Script::serving(object())).unwrap();
    let source = HttpSource::new(
        Limits::default(),
        std::sync::Arc::new(fetchloom_engine::work::WorkCounter::new()),
    );
    let body = source
        .fetch(
            &format!("{}/object", server.origin()),
            Some(ByteRange {
                start: 512,
                end: 1024,
            }),
            None,
        )
        .unwrap();

    assert_eq!(read(body.body), object()[512..]);
    let asked = server.received();
    assert_eq!(
        asked.last().unwrap().header("range"),
        Some("bytes=512-1023")
    );
}

#[test]
fn a_range_answered_with_the_whole_object_is_refused_as_unsupported() {
    let server =
        TestServer::start(Script::serving(object()).replying(vec![Reply::RangeIgnored])).unwrap();
    let source = HttpSource::new(
        Limits::default(),
        std::sync::Arc::new(fetchloom_engine::work::WorkCounter::new()),
    );
    let failure = source
        .fetch(
            &format!("{}/object", server.origin()),
            Some(ByteRange {
                start: 512,
                end: 1024,
            }),
            None,
        )
        .unwrap_err();

    assert_eq!(failure.kind(), ErrorKind::SourceUnsupportedRange);
}

#[test]
fn a_range_the_server_cannot_satisfy_is_refused_as_unsupported() {
    let server = TestServer::start(Script::serving(object()).replying(vec![Reply::Status {
        code: 416,
        retry_after: None,
    }]))
    .unwrap();
    let source = HttpSource::new(
        Limits::default(),
        std::sync::Arc::new(fetchloom_engine::work::WorkCounter::new()),
    );
    let failure = source
        .fetch(
            &format!("{}/object", server.origin()),
            Some(ByteRange {
                start: 4096,
                end: 8192,
            }),
            None,
        )
        .unwrap_err();

    assert_eq!(failure.kind(), ErrorKind::SourceUnsupportedRange);
    assert!(!failure.retryable());
}

#[test]
fn a_terminal_status_is_not_retryable_and_a_transient_one_is() {
    for (code, retryable) in [(404, false), (500, true), (503, true), (429, true)] {
        let server = TestServer::start(Script::serving(object()).replying(vec![Reply::Status {
            code,
            retry_after: None,
        }]))
        .unwrap();
        let source = HttpSource::new(
            Limits::default(),
            std::sync::Arc::new(fetchloom_engine::work::WorkCounter::new()),
        );
        let failure = source
            .fetch(&format!("{}/object", server.origin()), None, None)
            .unwrap_err();

        assert_eq!(failure.kind(), ErrorKind::NetworkStatus, "on {code}");
        assert_eq!(failure.retryable(), retryable, "on {code}");
    }
}

#[test]
fn a_rate_limited_source_reports_the_wait_it_asked_for() {
    let server = TestServer::start(Script::serving(object()).replying(vec![Reply::Status {
        code: 429,
        retry_after: Some("120".to_owned()),
    }]))
    .unwrap();
    let source = HttpSource::new(
        Limits::default(),
        std::sync::Arc::new(fetchloom_engine::work::WorkCounter::new()),
    );
    let failure = source
        .fetch(&format!("{}/object", server.origin()), None, None)
        .unwrap_err();

    assert!(failure.retryable());
    assert!(
        failure.next_action().contains("120"),
        "the wait was not reported: {}",
        failure.next_action()
    );
}

#[test]
fn every_recognized_index_is_listed_and_an_unrecognized_one_is_unresolved() {
    for format in [
        IndexFormat::ObjectStore,
        IndexFormat::WebDav,
        IndexFormat::GeneratedHtml,
    ] {
        let server =
            TestServer::start(Script::serving(object()).replying(vec![Reply::Listing { format }]))
                .unwrap();
        let source = HttpSource::new(
            Limits::default(),
            std::sync::Arc::new(fetchloom_engine::work::WorkCounter::new()),
        );
        let listed = source
            .list(&format!("{}/set/", server.origin()), None)
            .unwrap();
        let names: Vec<&str> = listed
            .entries
            .iter()
            .map(|entry| entry.path.as_str())
            .collect();
        assert_eq!(names, vec!["one", "two"], "{format:?} listed {names:?}");
    }

    let server = TestServer::start(Script::serving(object()).replying(vec![Reply::Listing {
        format: IndexFormat::Unrecognized,
    }]))
    .unwrap();
    let source = HttpSource::new(
        Limits::default(),
        std::sync::Arc::new(fetchloom_engine::work::WorkCounter::new()),
    );
    let failure = source
        .list(&format!("{}/set/", server.origin()), None)
        .unwrap_err();
    assert_eq!(failure.kind(), ErrorKind::ReferenceUnresolved);
}

#[test]
fn a_redirect_is_followed_and_the_bytes_arrive() {
    let target = TestServer::start(Script::serving(object())).unwrap();
    let server = TestServer::start(Script::serving(Vec::new()).replying(vec![Reply::Redirect {
        code: 307,
        location: format!("{}/object", target.origin()),
    }]))
    .unwrap();

    let source = HttpSource::new(
        Limits::default(),
        std::sync::Arc::new(fetchloom_engine::work::WorkCounter::new()),
    );
    let body = source
        .fetch(&format!("{}/object", server.origin()), None, None)
        .unwrap();
    assert_eq!(read(body.body), object());
}

#[test]
fn a_redirect_loop_ends_rather_than_running_forever() {
    let mut script = Script::serving(Vec::new());
    script.then = Reply::Redirect {
        code: 302,
        location: "/loop".to_owned(),
    };
    let looping = TestServer::start(script).unwrap();

    let source = HttpSource::new(
        Limits::default(),
        std::sync::Arc::new(fetchloom_engine::work::WorkCounter::new()),
    );
    let failure = source
        .fetch(&format!("{}/loop", looping.origin()), None, None)
        .unwrap_err();

    assert_eq!(failure.kind(), ErrorKind::ResourceLimit);
    assert!(
        failure.next_action().contains("10"),
        "the failure did not say which limit was reached: {}",
        failure.next_action()
    );
    assert!(
        looping.received().len() <= 11,
        "more than ten redirects were followed: {}",
        looping.received().len()
    );
}

#[test]
fn a_span_that_is_not_the_one_asked_for_is_refused() {
    let server = TestServer::start(
        Script::serving(object()).replying(vec![Reply::WrongRange { shift: 64 }]),
    )
    .unwrap();
    let source = HttpSource::new(
        Limits::default(),
        std::sync::Arc::new(fetchloom_engine::work::WorkCounter::new()),
    );

    let failure = source
        .fetch(
            &format!("{}/object", server.origin()),
            Some(ByteRange {
                start: 256,
                end: 1024,
            }),
            None,
        )
        .unwrap_err();

    assert_eq!(failure.kind(), ErrorKind::IntegrityRangeMismatch);
    assert!(
        failure.next_action().contains("256"),
        "the span that was asked for was not named: {}",
        failure.next_action()
    );
}

#[test]
fn the_span_that_was_asked_for_is_accepted() {
    let server = TestServer::start(Script::serving(object())).unwrap();
    let source = HttpSource::new(
        Limits::default(),
        std::sync::Arc::new(fetchloom_engine::work::WorkCounter::new()),
    );

    let body = source
        .fetch(
            &format!("{}/object", server.origin()),
            Some(ByteRange {
                start: 256,
                end: 1024,
            }),
            None,
        )
        .unwrap();

    assert_eq!(read(body.body), object()[256..]);
}

#[test]
fn a_name_that_does_not_resolve_is_refused_once_and_never_retried() {
    let source = HttpSource::new(
        Limits::default(),
        std::sync::Arc::new(fetchloom_engine::work::WorkCounter::new()),
    );
    let failure = source
        .fetch("http://fetchloom.invalid/object", None, None)
        .unwrap_err();

    assert_eq!(failure.kind(), ErrorKind::NetworkRefused);
    assert!(
        !failure.retryable(),
        "a name the resolver does not know will not be known on the next attempt: {failure}"
    );
    assert!(
        failure.next_action().contains("resolve"),
        "the failure did not say the name is what failed: {}",
        failure.next_action()
    );
}

#[test]
fn a_source_that_goes_quiet_mid_body_runs_out_of_time() {
    let server =
        TestServer::start(Script::serving(object()).replying(vec![Reply::Stalled { after: 16 }]))
            .unwrap();
    let limits = Limits {
        idle_timeout: std::time::Duration::from_millis(200),
        response_timeout: std::time::Duration::from_millis(200),
        ..Limits::default()
    };
    let source = HttpSource::new(
        limits,
        std::sync::Arc::new(fetchloom_engine::work::WorkCounter::new()),
    );
    let body = source
        .fetch(&format!("{}/object", server.origin()), None, None)
        .unwrap();
    let mut sink = Vec::new();
    let mut body = body.body;
    let failure = body.read_to_end(&mut sink).unwrap_err();

    assert!(
        matches!(
            failure.kind(),
            std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
        ) || failure.to_string().to_lowercase().contains("timeout"),
        "a stalled body was not reported as running out of time: {failure}"
    );
}

#[test]
fn a_server_that_refuses_the_handshake_fails_on_the_connection() {
    let server = TestServer::start(Script::serving(object()).refusing_tls()).unwrap();
    let secured = server.secured_origin();
    let source = HttpSource::new(
        Limits::default(),
        std::sync::Arc::new(fetchloom_engine::work::WorkCounter::new()),
    );
    let failure = source
        .fetch(&format!("{secured}/object"), None, None)
        .unwrap_err();

    assert_eq!(
        failure.kind(),
        ErrorKind::NetworkTls,
        "a failed handshake was reported as something else: {failure}"
    );
    assert!(
        !failure.retryable(),
        "a connection that could not be secured will not secure itself on a retry"
    );
}

#[test]
fn a_credential_the_source_rejects_is_reported_as_the_credential_and_not_the_status() {
    for code in [401, 403] {
        let server = TestServer::start(Script::serving(object()).replying(vec![Reply::Status {
            code,
            retry_after: None,
        }]))
        .unwrap();
        let source = HttpSource::new(
            Limits::default(),
            std::sync::Arc::new(fetchloom_engine::work::WorkCounter::new()),
        );
        let credential = Credential {
            host: Host::new("127.0.0.1".to_owned()),
            origin: CredentialOrigin::Environment,
            secrets: fetchloom_engine::credential::Secrets::Bearer { value: Secret::new("token".to_owned()) },
        };
        let failure = source
            .fetch(
                &format!("{}/object", server.origin()),
                None,
                Some(&credential),
            )
            .unwrap_err();

        assert_eq!(
            failure.kind(),
            ErrorKind::PolicyCredentialInvalid,
            "a {code} answered to a credential was surfaced as a raw status"
        );
        assert!(
            failure.next_action().contains("127.0.0.1"),
            "the refusal did not name the provider: {}",
            failure.next_action()
        );
    }
}

#[test]
fn a_status_a_run_carried_no_credential_for_is_a_missing_credential() {
    let server = TestServer::start(Script::serving(object()).replying(vec![Reply::Status {
        code: 401,
        retry_after: None,
    }]))
    .unwrap();
    let source = HttpSource::new(
        Limits::default(),
        std::sync::Arc::new(fetchloom_engine::work::WorkCounter::new()),
    );
    let failure = source
        .fetch(&format!("{}/object", server.origin()), None, None)
        .unwrap_err();

    assert_eq!(failure.kind(), ErrorKind::PolicyCredentialMissing);
    assert!(
        !failure.retryable(),
        "a refusal for want of authorization was retryable"
    );
}

#[test]
fn a_listing_with_more_entries_than_the_limit_allows_is_refused_as_a_limit() {
    let server = TestServer::start(Script::serving(Vec::new()).replying(vec![Reply::Listing {
        format: IndexFormat::ObjectStore,
    }]))
    .unwrap();
    let limits = Limits {
        listing_entries: 1,
        ..Limits::default()
    };
    let source = HttpSource::new(
        limits,
        std::sync::Arc::new(fetchloom_engine::work::WorkCounter::new()),
    );
    let failure = source
        .list(&format!("{}/prefix/", server.origin()), None)
        .unwrap_err();

    assert_eq!(failure.kind(), ErrorKind::ResourceLimit);
    assert!(
        failure.next_action().contains('1'),
        "the refusal did not say which limit was reached: {}",
        failure.next_action()
    );
}
