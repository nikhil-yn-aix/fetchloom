//! What a registry search answers, driven against a local server.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test setup, where a failure to build the input is the assertion"
)]

use rustls as _;
use rustls_graviola as _;
use rustls_platform_verifier as _;
use serde_json as _;
use sha2 as _;
use ureq as _;

use std::sync::Arc;

use fetchloom_engine::limits::Limits;
use fetchloom_engine::work::WorkCounter;
use fetchloom_faults::{Reply, Script, TestServer};
use fetchloom_sources::{Registry, Searcher};

fn counter() -> Arc<WorkCounter> {
    Arc::new(WorkCounter::new())
}

fn answering(body: &str) -> TestServer {
    TestServer::start(Script::serving(body.as_bytes().to_vec())).unwrap()
}

fn searcher(registry: Registry, origin: &str) -> Searcher {
    Searcher::reaching(registry, origin, Limits::default(), counter())
}

#[test]
fn a_hugging_face_answer_becomes_the_reference_that_adapter_serves() {
    let server = answering(
        r#"[{"id":"Nagabu/HAM10000","author":"Nagabu"},{"id":"zs389/Ham10000","author":"zs389"}]"#,
    );
    let found = searcher(Registry::HuggingFace, &server.origin())
        .search("ham10000", None)
        .expect("a search that answered was not read");
    assert_eq!(
        found
            .iter()
            .map(|held| held.reference.as_str())
            .collect::<Vec<_>>(),
        vec!["hf:datasets/Nagabu/HAM10000", "hf:datasets/zs389/Ham10000"]
    );
    assert_eq!(
        found[0].name, "HAM10000",
        "the name matched against is the record's own, not the owner's"
    );
}

#[test]
fn a_kaggle_answer_carries_the_size_the_registry_states() {
    let server = answering(
        r#"[{"ref":"kmader/skin-cancer-mnist-ham10000","title":"Skin Cancer MNIST: HAM10000","totalBytes":5582914511}]"#,
    );
    let found = searcher(Registry::Kaggle, &server.origin())
        .search("ham10000", None)
        .expect("a search that answered was not read");
    assert_eq!(
        found[0].reference,
        "kaggle:kmader/skin-cancer-mnist-ham10000"
    );
    assert_eq!(found[0].size, Some(5_582_914_511));
    assert_eq!(found[0].title, "Skin Cancer MNIST: HAM10000");
}

#[test]
fn an_openml_answer_naming_no_results_is_no_match_and_not_a_failure() {
    let server = TestServer::start(
        Script::serving(br#"{"error":{"code":"372","message":"No results"}}"#.to_vec()).replying(
            vec![Reply::Status {
                code: 412,
                retry_after: None,
            }],
        ),
    )
    .unwrap();
    let found = searcher(Registry::OpenMl, &server.origin())
        .search("nothing", None)
        .expect(
            "OpenML answers 412 rather than an empty list when a name matches nothing, and that \
             was reported as a failure",
        );
    assert!(found.is_empty());
}

#[test]
fn an_openml_answer_names_the_dataset_by_the_identifier_the_adapter_takes() {
    let server = answering(
        r#"{"data":{"dataset":[{"did":61,"name":"iris","version":1},{"did":969,"name":"iris","version":3}]}}"#,
    );
    let found = searcher(Registry::OpenMl, &server.origin())
        .search("iris", None)
        .expect("a search that answered was not read");
    assert_eq!(
        found
            .iter()
            .map(|held| held.reference.as_str())
            .collect::<Vec<_>>(),
        vec!["openml:61", "openml:969"]
    );
    assert_eq!(found[0].name, "iris");
}

#[test]
fn a_zenodo_answer_names_the_record_by_its_doi() {
    let server = answering(
        r#"{"hits":{"hits":[{"id":17498946,"doi":"10.5281/zenodo.17498946","title":"HAM10000"}]}}"#,
    );
    let found = searcher(Registry::Zenodo, &server.origin())
        .search("ham10000", None)
        .expect("a search that answered was not read");
    assert_eq!(found[0].reference, "zenodo:10.5281/zenodo.17498946");
    assert_eq!(found[0].name, "HAM10000");
}

#[test]
fn a_figshare_search_is_asked_for_with_a_post_because_its_get_answers_something_else() {
    let server = answering(r#"[{"id":33392224,"title":"HAM10000 segmented"}]"#);
    let found = searcher(Registry::Figshare, &server.origin())
        .search("ham10000", None)
        .expect("a search that answered was not read");
    assert_eq!(found[0].reference, "figshare:33392224");
    let asked = server.received();
    assert_eq!(
        asked[0].method, "POST",
        "Figshare answers a search only to a POST, and its GET returns the newest articles \
         instead of refusing an unknown parameter"
    );
    assert!(
        asked[0].body.contains("ham10000"),
        "the term was not in the body: {:?}",
        asked[0].body
    );
}

#[test]
fn a_ckan_and_a_dataverse_answer_carry_the_host_that_was_searched() {
    let ckan = answering(r#"{"result":{"results":[{"name":"a-dataset","title":"A dataset"}]}}"#);
    let found = searcher(Registry::Ckan, &ckan.origin())
        .search("water", None)
        .expect("a search that answered was not read");
    let host = ckan.origin().replace("http://", "");
    assert_eq!(found[0].reference, format!("ckan:{host}/a-dataset"));

    let dataverse = answering(
        r#"{"status":"OK","data":{"items":[{"name":"Segmented HAM10000","global_id":"doi:10.7910/DVN/LZJTKO","type":"dataset"}]}}"#,
    );
    let found = searcher(Registry::Dataverse, &dataverse.origin())
        .search("ham10000", None)
        .expect("a search that answered was not read");
    let host = dataverse.origin().replace("http://", "");
    assert_eq!(
        found[0].reference,
        format!("dataverse:{host}/doi:10.7910/DVN/LZJTKO")
    );
}

#[test]
fn a_datacite_answer_is_routed_by_its_landing_page_and_one_it_cannot_route_is_dropped() {
    let server = answering(
        r#"{"data":[
            {"attributes":{"doi":"10.5281/zenodo.20633541","url":"https://zenodo.org/records/20633541","titles":[{"title":"Melanoma detection"}]}},
            {"attributes":{"doi":"10.1000/unknown","url":"https://example.invalid/thing","titles":[{"title":"Somewhere else"}]}}
        ]}"#,
    );
    let found = searcher(Registry::DataCite, &server.origin())
        .search("ham10000", None)
        .expect("a search that answered was not read");
    assert_eq!(
        found
            .iter()
            .map(|held| held.reference.as_str())
            .collect::<Vec<_>>(),
        vec!["zenodo:10.5281/zenodo.20633541"],
        "a DOI landing where no adapter reaches was guessed at instead of dropped"
    );
}

#[test]
fn an_answer_that_is_not_the_shape_the_registry_states_is_a_failure_and_not_an_empty_result() {
    let server = answering("{ not json at all");
    let refused = searcher(Registry::HuggingFace, &server.origin())
        .search("ham10000", None)
        .expect_err("a malformed answer was read as no results, which hides a broken registry");
    assert_eq!(
        refused.kind(),
        fetchloom_engine::error::ErrorKind::ReferenceUnresolved
    );
}

#[test]
fn a_registry_that_is_rate_limiting_says_so_rather_than_answering_nothing() {
    let server = TestServer::start(
        Script::serving(b"{}".to_vec()).replying(vec![Reply::Status {
            code: 429,
            retry_after: Some("30".to_owned()),
        }]),
    )
    .unwrap();
    let refused = searcher(Registry::HuggingFace, &server.origin())
        .search("ham10000", None)
        .expect_err("a rate limited registry was read as no results");
    assert_eq!(
        refused.kind(),
        fetchloom_engine::error::ErrorKind::NetworkStatus
    );
}

#[test]
fn a_term_is_escaped_so_it_cannot_add_a_parameter_of_its_own() {
    let server = answering("[]");
    let _ = searcher(Registry::HuggingFace, &server.origin())
        .search("a b&limit=9999#", None)
        .expect("a search that answered was not read");
    let asked = server.received();
    assert!(
        asked[0].target.contains("a%20b%26limit%3D9999%23"),
        "a term went into the query unescaped, so a name could add a parameter: {}",
        asked[0].target
    );
}
