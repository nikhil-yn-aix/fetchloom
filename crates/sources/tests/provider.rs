//! What the two provider adapters serve, driven against a local server.

#![expect(
    clippy::unwrap_used,
    reason = "test setup, where a failure to build the input is the assertion"
)]

use rustls as _;
use rustls_graviola as _;
use serde_json as _;
use sha2 as _;
use ureq as _;

use std::sync::Arc;

use fetchloom_engine::limits::Limits;
use fetchloom_engine::seam::source::{Serves, Source};
use fetchloom_engine::work::WorkCounter;
use fetchloom_faults::{IndexFormat, Reply, Script, TestServer};
use fetchloom_sources::{HuggingFaceSource, ZenodoSource};

fn counter() -> Arc<WorkCounter> {
    Arc::new(WorkCounter::new())
}

#[test]
fn each_provider_serves_only_its_own_scheme() {
    let hugging_face = HuggingFaceSource::new(Limits::default(), counter());
    let zenodo = ZenodoSource::new(Limits::default(), counter());

    assert_eq!(
        hugging_face.serves("hf:datasets/org/name"),
        Some(Serves::Container)
    );
    assert_eq!(hugging_face.serves("zenodo:10.5281/zenodo.1"), None);
    assert_eq!(hugging_face.serves("https://host/object"), None);
    assert_eq!(hugging_face.serves("./local/path"), None);

    assert_eq!(
        zenodo.serves("zenodo:10.5281/zenodo.1234567"),
        Some(Serves::Container)
    );
    assert_eq!(zenodo.serves("hf:datasets/org/name"), None);
    assert_eq!(zenodo.serves("https://host/object"), None);
}

#[test]
fn a_repository_reference_is_refused_by_name_when_it_names_no_repository() {
    let source = HuggingFaceSource::new(Limits::default(), counter());
    for refused in ["hf:", "hf:onlyone", "hf:datasets/org"] {
        let error = source.list(refused, None).unwrap_err();
        assert_eq!(
            error.kind(),
            fetchloom_engine::error::ErrorKind::ReferenceUnresolved,
            "{refused} failed as something other than an unresolved reference"
        );
        assert!(
            error.next_action().contains("hf:datasets/org/name"),
            "{refused} was refused without naming the form to write: {}",
            error.next_action()
        );
    }
}

#[test]
fn a_record_reference_is_refused_by_name_when_it_names_no_record() {
    let source = ZenodoSource::new(Limits::default(), counter());
    let error = source.list("zenodo:not-a-record", None).unwrap_err();
    assert_eq!(
        error.kind(),
        fetchloom_engine::error::ErrorKind::ReferenceUnresolved
    );
    assert!(error.next_action().contains("zenodo:10.5281/zenodo."));
}

#[test]
fn a_record_lists_the_files_it_states_and_nothing_else() {
    let body = r#"{"files":[
        {"key":"second.csv","size":20,"links":{"self":"https://zenodo.org/api/files/x/second.csv"}},
        {"key":"first.csv","size":10,"links":{"self":"https://zenodo.org/api/files/x/first.csv"}}
    ]}"#;
    let server = TestServer::start(Script::serving(body.as_bytes().to_vec())).unwrap();
    let host = server.origin();
    let source = ZenodoSource::reaching(host, Limits::default(), counter());

    let listing = source.list("zenodo:10.5281/zenodo.1234567", None).unwrap();
    assert_eq!(listing.entries.len(), 2);
    assert_eq!(listing.entries[0].path, "first.csv");
    assert_eq!(listing.entries[0].size, Some(10));
    assert_eq!(listing.entries[1].path, "second.csv");
    assert_eq!(listing.skipped, 0);
}

#[test]
fn a_repository_lists_its_files_and_skips_what_is_not_one() {
    let body = r#"[
        {"type":"file","path":"train.csv","size":100},
        {"type":"directory","path":"nested"},
        {"type":"file","path":"README.md","size":10}
    ]"#;
    let server = TestServer::start(Script::serving(body.as_bytes().to_vec())).unwrap();
    let host = server.origin();
    let source = HuggingFaceSource::reaching(host, Limits::default(), counter());

    let listing = source.list("hf:datasets/org/name", None).unwrap();
    assert_eq!(
        listing.entries.len(),
        2,
        "a directory entry was listed as a file"
    );
    assert_eq!(listing.entries[0].path, "README.md");
    assert_eq!(listing.entries[1].path, "train.csv");
}

#[test]
fn a_pinned_revision_reaches_a_different_location_than_an_unpinned_one() {
    let body = r#"[{"type":"file","path":"one.csv","size":1}]"#;
    let server = TestServer::start(Script::serving(body.as_bytes().to_vec())).unwrap();
    let host = server.origin();
    let source = HuggingFaceSource::reaching(host, Limits::default(), counter());

    source.list("hf:datasets/org/name", None).unwrap();
    source.list("hf:datasets/org/name@abc123", None).unwrap();

    let asked: Vec<String> = server
        .received()
        .into_iter()
        .map(|request| request.target)
        .collect();
    assert!(
        asked.iter().any(|target| target.ends_with("/tree/main")),
        "an unpinned reference did not ask for the default revision: {asked:?}"
    );
    assert!(
        asked.iter().any(|target| target.ends_with("/tree/abc123")),
        "a pinned revision was not asked for: {asked:?}"
    );
}

#[test]
fn a_provider_answering_something_else_is_refused_and_never_guessed_at() {
    let server = TestServer::start(
        Script::serving(b"not a record".to_vec()).replying(vec![Reply::Listing {
            format: IndexFormat::GeneratedHtml,
        }]),
    )
    .unwrap();
    let host = server.origin();
    let source = ZenodoSource::reaching(host, Limits::default(), counter());

    let error = source.list("zenodo:1234567", None).unwrap_err();
    assert_eq!(
        error.kind(),
        fetchloom_engine::error::ErrorKind::ReferenceUnresolved,
        "an index in another format was read as a record"
    );
}

#[test]
fn a_provider_refusal_never_carries_a_secret_from_the_reference() {
    let server = TestServer::start(Script::serving(b"{}".to_vec())).unwrap();
    let host = server.origin();
    let source = ZenodoSource::reaching(host, Limits::default(), counter());

    let error = source
        .list("zenodo:1234567?signature=super-secret-value", None)
        .unwrap_err();
    let everything = format!("{error:?}{}", error.next_action());
    assert!(
        !everything.contains("super-secret-value"),
        "a query value reached the refusal: {everything}"
    );
}
