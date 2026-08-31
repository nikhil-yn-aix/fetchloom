//! Contract tests proving a credential is dropped on a cross-origin redirect
//! and never appears in any output.

#![expect(
    clippy::unwrap_used,
    reason = "test setup, where a failure to build the input is the assertion"
)]

use rustls as _;
use ureq as _;

use std::io::Read;

use fetchloom_engine::credential::{Credential, CredentialOrigin};
use fetchloom_engine::limits::Limits;
use fetchloom_engine::redact::{SafeUrl, Secret};
use fetchloom_engine::reference::Host;
use fetchloom_engine::seam::source::Source;
use fetchloom_faults::{Reply, Script, TestServer};
use fetchloom_sources::HttpSource;

const SECRET: &str = "super-secret-token-value";

fn credential() -> Credential {
    Credential {
        host: Host::new("credential-test-host"),
        origin: CredentialOrigin::Environment,
        value: Secret::new(SECRET.to_owned()),
    }
}

fn object() -> Vec<u8> {
    b"the object body".to_vec()
}

fn read(body: impl Read) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut body = body;
    body.read_to_end(&mut bytes).unwrap();
    bytes
}

#[test]
fn a_credential_is_sent_on_the_first_request() {
    let server = TestServer::start(Script::serving(object())).unwrap();
    let source = HttpSource::new(
        Limits::default(),
        std::sync::Arc::new(fetchloom_engine::work::WorkCounter::new()),
    );
    let credential = credential();

    let body = source
        .fetch(
            &format!("{}/object", server.origin()),
            None,
            Some(&credential),
        )
        .unwrap();
    assert_eq!(read(body.body), object());

    let asked = server.received();
    assert_eq!(asked[0].header("authorization"), Some(SECRET));
}

#[test]
fn a_credential_is_dropped_across_a_redirect_to_a_different_port() {
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
    let credential = credential();

    let body = source
        .fetch(
            &format!("{}/object", server.origin()),
            None,
            Some(&credential),
        )
        .unwrap();
    assert_eq!(read(body.body), object());

    let first_request = server.received();
    assert_eq!(first_request[0].header("authorization"), Some(SECRET));

    let second_request = target.received();
    assert_eq!(second_request[0].header("authorization"), None);
}

#[test]
fn a_credential_survives_a_same_origin_redirect() {
    let server = TestServer::start(Script::serving(object()).replying(vec![Reply::Redirect {
        code: 302,
        location: "/elsewhere".to_owned(),
    }]))
    .unwrap();

    let source = HttpSource::new(
        Limits::default(),
        std::sync::Arc::new(fetchloom_engine::work::WorkCounter::new()),
    );
    let credential = credential();

    let body = source
        .fetch(
            &format!("{}/object", server.origin()),
            None,
            Some(&credential),
        )
        .unwrap();
    assert_eq!(read(body.body), object());

    let asked = server.received();
    assert_eq!(asked.len(), 2);
    assert_eq!(asked[1].target, "/elsewhere");
    assert_eq!(asked[1].header("authorization"), Some(SECRET));
}

#[test]
fn the_drop_across_a_cross_origin_redirect_is_reported_as_a_degradation() {
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
    let credential = credential();
    let requested_location = format!("{}/object", server.origin());

    source
        .fetch(&requested_location, None, Some(&credential))
        .unwrap();

    let degradations = source.take_degradations();
    assert_eq!(degradations.len(), 1);
    let degradation = &degradations[0];
    assert!(
        degradation.requested.contains("credential"),
        "the requested field did not mention the credential: {}",
        degradation.requested
    );
    assert!(
        degradation.reason.contains(&server.origin()),
        "the reason did not name the origin the request started at: {}",
        degradation.reason
    );
    assert!(
        degradation.reason.contains(&target.origin()),
        "the reason did not name the origin the redirect led to: {}",
        degradation.reason
    );
}

#[test]
fn the_secret_never_appears_in_an_error_produced_after_a_cross_origin_redirect() {
    let target = TestServer::start(Script::serving(Vec::new()).replying(vec![Reply::Status {
        code: 404,
        retry_after: None,
    }]))
    .unwrap();
    let server = TestServer::start(Script::serving(Vec::new()).replying(vec![Reply::Redirect {
        code: 307,
        location: format!("{}/object", target.origin()),
    }]))
    .unwrap();

    let source = HttpSource::new(
        Limits::default(),
        std::sync::Arc::new(fetchloom_engine::work::WorkCounter::new()),
    );
    let credential = credential();

    let failure = source
        .fetch(
            &format!("{}/object", server.origin()),
            None,
            Some(&credential),
        )
        .unwrap_err();

    let displayed = failure.to_string();
    let next_action = failure.next_action();
    assert!(
        !displayed.contains(SECRET),
        "the error display carried the secret: {displayed}"
    );
    assert!(
        !next_action.contains(SECRET),
        "the next action carried the secret: {next_action}"
    );
}

#[test]
fn the_secret_never_appears_in_a_redacted_location() {
    let location = format!("https://user:{SECRET}@host/path?token={SECRET}");
    let safe = SafeUrl::new(&location);

    let displayed = safe.to_string();
    let exposed = safe.as_str();
    assert!(
        !displayed.contains(SECRET),
        "the redacted location's display carried the secret: {displayed}"
    );
    assert!(
        !exposed.contains(SECRET),
        "the redacted location's text carried the secret: {exposed}"
    );
}
