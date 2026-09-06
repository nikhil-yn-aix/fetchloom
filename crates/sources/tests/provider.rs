//! What each provider adapter serves, driven against a local server.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
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
    let server = TestServer::start(Script::serving(b"not a record".to_vec()).replying(vec![
        Reply::Listing {
            format: IndexFormat::GeneratedHtml,
        },
    ]))
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

#[test]
fn every_entry_a_record_lists_is_named_by_a_reference_that_resolves() {
    let body = r#"{"files":[
        {"key":"article.pdf","size":10,"links":{"self":"https://zenodo.org/api/files/x/article.pdf"}}
    ]}"#;
    let server = TestServer::start(Script::serving(body.as_bytes().to_vec())).unwrap();
    let host = server.origin();
    let source = ZenodoSource::reaching(host, Limits::default(), counter());

    let listing = source.list("zenodo:10.5281/zenodo.1234567", None).unwrap();
    let named =
        fetchloom_sources::joined("zenodo:10.5281/zenodo.1234567", &listing.entries[0].path);

    assert_eq!(
        named, "zenodo:10.5281/zenodo.1234567/article.pdf",
        "a listed entry was named by pasting its path onto the container"
    );
    assert!(
        source.serves(&named).is_some(),
        "the reference a listing produces is not one the provider serves"
    );
}

#[test]
fn a_container_reference_and_an_entry_path_are_joined_by_one_separator() {
    for (container, path, expected) in [
        (
            "zenodo:10.5281/zenodo.1234567",
            "article.pdf",
            "zenodo:10.5281/zenodo.1234567/article.pdf",
        ),
        (
            "hf:datasets/org/name/",
            "train.csv",
            "hf:datasets/org/name/train.csv",
        ),
        ("https://host/set/", "one.bin", "https://host/set/one.bin"),
        ("https://host/set", "one.bin", "https://host/set/one.bin"),
    ] {
        assert_eq!(fetchloom_sources::joined(container, path), expected);
    }
}

#[test]
fn a_file_inside_a_record_is_fetched_from_the_link_the_record_states() {
    let files = TestServer::start(Script::serving(b"the article bytes".to_vec())).unwrap();
    let body = format!(
        r#"{{"files":[{{"key":"article.pdf","size":17,"links":{{"self":"{}/files/article.pdf"}}}}]}}"#,
        files.origin()
    );
    let record = TestServer::start(Script::serving(body.into_bytes())).unwrap();
    let source = ZenodoSource::reaching(record.origin(), Limits::default(), counter());

    let described = source
        .probe("zenodo:10.5281/zenodo.1234567/article.pdf", None)
        .expect("a file inside a record did not resolve");
    assert!(
        described.location.as_str().contains("/files/article.pdf"),
        "a file reference resolved to {} rather than to the link the record states",
        described.location
    );

    let missing = source
        .probe("zenodo:10.5281/zenodo.1234567/absent.pdf", None)
        .expect_err("a file the record does not state resolved anyway");
    assert_eq!(
        missing.kind(),
        fetchloom_engine::error::ErrorKind::ReferenceUnresolved
    );
}

fn described_body(body: &str) -> TestServer {
    TestServer::start(Script::serving(body.as_bytes().to_vec())).unwrap()
}

#[test]
fn each_described_provider_serves_only_its_own_scheme() {
    let held: Vec<(&str, Box<dyn Source<Body = fetchloom_sources::HttpBody>>)> = vec![
        (
            "kaggle:owner/slug",
            Box::new(fetchloom_sources::described(
                fetchloom_sources::Provider::Kaggle,
                Limits::default(),
                counter(),
            )),
        ),
        (
            "openml:61",
            Box::new(fetchloom_sources::described(
                fetchloom_sources::Provider::OpenMl,
                Limits::default(),
                counter(),
            )),
        ),
        (
            "github:owner/repo",
            Box::new(fetchloom_sources::described(
                fetchloom_sources::Provider::GitHubReleases,
                Limits::default(),
                counter(),
            )),
        ),
        (
            "figshare:1234567",
            Box::new(fetchloom_sources::described(
                fetchloom_sources::Provider::Figshare,
                Limits::default(),
                counter(),
            )),
        ),
        (
            "ckan:demo.ckan.org/a-dataset",
            Box::new(fetchloom_sources::described(
                fetchloom_sources::Provider::Ckan,
                Limits::default(),
                counter(),
            )),
        ),
        (
            "dataverse:dataverse.harvard.edu/doi:10.7910/DVN/OMV93V",
            Box::new(fetchloom_sources::described(
                fetchloom_sources::Provider::Dataverse,
                Limits::default(),
                counter(),
            )),
        ),
    ];
    for (own, source) in &held {
        assert_eq!(
            source.serves(own),
            Some(Serves::Container),
            "{own} is not served by the provider it names"
        );
        for (other, _) in &held {
            if other == own {
                continue;
            }
            assert_eq!(
                source.serves(other),
                None,
                "{other} was served by the adapter for {own}"
            );
        }
        assert_eq!(source.serves("https://host/object"), None);
    }
}

#[test]
fn kaggle_lists_the_files_it_states_and_builds_the_location_it_does_not() {
    let body = r#"{"datasetFiles":[
        {"name":"Iris.csv","totalBytes":5107,"url":"","columns":[]},
        {"name":"database.sqlite","totalBytes":10240,"url":""}
    ],"nextPageToken":""}"#;
    let server = described_body(body);
    let source = fetchloom_sources::described_reaching(
        fetchloom_sources::Provider::Kaggle,
        server.origin(),
        Limits::default(),
        counter(),
    );

    let listing = source.list("kaggle:uciml/iris", None).unwrap();
    assert_eq!(listing.entries.len(), 2);
    assert_eq!(listing.entries[0].path, "Iris.csv");
    assert_eq!(listing.entries[0].size, Some(5107));
    assert_eq!(
        listing.entries[0].interop, None,
        "Kaggle states no checksum and one was carried anyway"
    );
    assert!(
        listing.entries[0]
            .location
            .as_str()
            .ends_with("/api/v1/datasets/download/uciml/iris/Iris.csv"),
        "the location was taken from the empty url the listing states: {}",
        listing.entries[0].location
    );
}

#[test]
fn openml_lists_the_one_file_a_description_is() {
    let body = r#"{"data_set_description":{"id":"61","name":"iris","format":"ARFF",
        "file_id":"61","url":"https://openml.org/data/v1/download/61/iris.arff",
        "md5_checksum":"ad484452702105cbf3d30f8deaba39a9"}}"#;
    let server = described_body(body);
    let source = fetchloom_sources::described_reaching(
        fetchloom_sources::Provider::OpenMl,
        server.origin(),
        Limits::default(),
        counter(),
    );

    let listing = source.list("openml:61", None).unwrap();
    assert_eq!(listing.entries.len(), 1);
    assert_eq!(
        listing.entries[0].path, "iris.arff",
        "a record naming no file name did not take one from the location"
    );
    assert_eq!(
        listing.entries[0].interop, None,
        "an MD5 was carried as a digest claim"
    );
}

#[test]
fn a_release_asset_carries_the_sha256_github_states() {
    let body = r#"{"assets":[{"name":"rg.tar.gz","size":1764284,
        "digest":"sha256:3750b2e93f37e0c692657da574d7019a101c0084da05a790c83fd335bad973e4",
        "browser_download_url":"https://github.com/o/r/releases/download/1/rg.tar.gz"}]}"#;
    let server = described_body(body);
    let source = fetchloom_sources::described_reaching(
        fetchloom_sources::Provider::GitHubReleases,
        server.origin(),
        Limits::default(),
        counter(),
    );

    let listing = source.list("github:o/r@1", None).unwrap();
    assert_eq!(listing.entries.len(), 1);
    assert_eq!(
        listing.entries[0].interop.map(|held| held.to_string()),
        Some("sha256:3750b2e93f37e0c692657da574d7019a101c0084da05a790c83fd335bad973e4".to_owned()),
        "a stated SHA-256 did not become a digest claim, so the run would trust it on first use"
    );
    let asked: Vec<String> = server
        .received()
        .into_iter()
        .map(|request| request.target)
        .collect();
    assert!(
        asked
            .iter()
            .any(|target| target.ends_with("/releases/tags/1")),
        "a pinned tag asked for something other than that tag: {asked:?}"
    );
}

#[test]
fn a_release_with_no_tag_asks_for_the_latest_one() {
    let body = r#"{"assets":[{"name":"a.bin","size":1,"digest":null,
        "browser_download_url":"https://github.com/o/r/releases/download/9/a.bin"}]}"#;
    let server = described_body(body);
    let source = fetchloom_sources::described_reaching(
        fetchloom_sources::Provider::GitHubReleases,
        server.origin(),
        Limits::default(),
        counter(),
    );

    source.list("github:o/r", None).unwrap();
    let asked: Vec<String> = server
        .received()
        .into_iter()
        .map(|request| request.target)
        .collect();
    assert!(
        asked
            .iter()
            .any(|target| target.ends_with("/releases/latest")),
        "an unpinned release asked for something other than the latest: {asked:?}"
    );
}

#[test]
fn figshare_states_only_an_md5_so_nothing_is_claimed() {
    let body = r#"{"files":[{"id":1786118,"name":"Checklist_S1.docx","size":201739,
        "is_link_only":false,"download_url":"https://ndownloader.figshare.com/files/1786118",
        "supplied_md5":"277dc43b2b2ee467ffafe9e4903ab15d",
        "computed_md5":"277dc43b2b2ee467ffafe9e4903ab15d"}]}"#;
    let server = described_body(body);
    let source = fetchloom_sources::described_reaching(
        fetchloom_sources::Provider::Figshare,
        server.origin(),
        Limits::default(),
        counter(),
    );

    let listing = source.list("figshare:1234567", None).unwrap();
    assert_eq!(listing.entries[0].path, "Checklist_S1.docx");
    assert_eq!(listing.entries[0].size, Some(201_739));
    assert_eq!(
        listing.entries[0].interop, None,
        "an MD5 was carried as evidence the bytes can be verified against"
    );
}

#[test]
fn a_ckan_resource_is_named_by_its_location_rather_than_by_its_label() {
    let body = r#"{"help":"x","success":true,"result":{"resources":[
        {"name":"Virtual Tour","format":"MP4","hash":"","size":null,
         "url":"https://host/dataset/a/resource/b/download/virtual-tour.mp4"}
    ]}}"#;
    let server = described_body(body);
    let source = fetchloom_sources::described_reaching(
        fetchloom_sources::Provider::Ckan,
        server.origin(),
        Limits::default(),
        counter(),
    );

    let listing = source.list("ckan:a-dataset", None).unwrap();
    assert_eq!(
        listing.entries[0].path, "virtual-tour.mp4",
        "a display label was written to disk as a file name"
    );
    assert_eq!(listing.entries[0].size, None);
    assert_eq!(
        listing.entries[0].interop, None,
        "an empty hash was carried as a digest claim"
    );
}

#[test]
fn a_ckan_hash_is_carried_only_when_it_names_its_algorithm() {
    let hex = "3750b2e93f37e0c692657da574d7019a101c0084da05a790c83fd335bad973e4";
    for (hash, carried) in [
        (format!("sha256:{hex}"), true),
        (hex.to_owned(), false),
        ("md5:277dc43b2b2ee467ffafe9e4903ab15d".to_owned(), false),
    ] {
        let body = format!(
            r#"{{"result":{{"resources":[{{"hash":"{hash}","url":"https://host/d/download/a.csv"}}]}}}}"#
        );
        let server = described_body(&body);
        let source = fetchloom_sources::described_reaching(
            fetchloom_sources::Provider::Ckan,
            server.origin(),
            Limits::default(),
            counter(),
        );
        let listing = source.list("ckan:a-dataset", None).unwrap();
        assert_eq!(
            listing.entries[0].interop.is_some(),
            carried,
            "a hash written as {hash} was read as evidence it is not"
        );
    }
}

#[test]
fn a_dataverse_file_is_reached_by_its_identifier_and_carries_only_a_named_sha256() {
    for (algorithm, carried) in [("MD5", false), ("SHA-256", true)] {
        let body = format!(
            r#"{{"status":"OK","totalCount":1,"data":[{{"label":"a.tab","dataFile":{{
                "id":2914111,"filename":"dmoz.tab","filesize":296615991,
                "checksum":{{"type":"{algorithm}",
                "value":"3750b2e93f37e0c692657da574d7019a101c0084da05a790c83fd335bad973e4"}}}}}}]}}"#
        );
        let server = described_body(&body);
        let source = fetchloom_sources::described_reaching(
            fetchloom_sources::Provider::Dataverse,
            server.origin(),
            Limits::default(),
            counter(),
        );

        let listing = source
            .list("dataverse:doi:10.7910/DVN/OMV93V", None)
            .unwrap();
        assert_eq!(listing.entries[0].path, "dmoz.tab");
        assert_eq!(listing.entries[0].size, Some(296_615_991));
        assert!(
            listing.entries[0]
                .location
                .as_str()
                .ends_with("/api/access/datafile/2914111"),
            "a file was not reached by the identifier the record states: {}",
            listing.entries[0].location
        );
        assert_eq!(
            listing.entries[0].interop.is_some(),
            carried,
            "a checksum labelled {algorithm} was treated as though it were not"
        );
    }
}

#[test]
fn a_folder_a_record_states_is_part_of_the_path_it_writes() {
    let body = r#"{"data":[{"directoryLabel":"raw/2024","dataFile":{
        "id":7,"filename":"a.csv","filesize":1,"checksum":{"type":"MD5","value":"x"}}}]}"#;
    let server = described_body(body);
    let source = fetchloom_sources::described_reaching(
        fetchloom_sources::Provider::Dataverse,
        server.origin(),
        Limits::default(),
        counter(),
    );

    let listing = source.list("dataverse:doi:10.1/X", None).unwrap();
    assert_eq!(
        listing.entries[0].path, "raw/2024/a.csv",
        "two files in different folders would have collided"
    );
}

#[test]
fn a_described_provider_answering_something_else_is_refused_rather_than_guessed_at() {
    for body in ["not json at all", "{}", r#"{"files":"a string"}"#, "[]"] {
        let server = described_body(body);
        let source = fetchloom_sources::described_reaching(
            fetchloom_sources::Provider::Figshare,
            server.origin(),
            Limits::default(),
            counter(),
        );
        let error = source
            .list("figshare:1234567", None)
            .expect_err("a record this build cannot read was read anyway");
        assert_eq!(
            error.kind(),
            fetchloom_engine::error::ErrorKind::ReferenceUnresolved,
            "{body} failed as something other than an unresolved reference"
        );
    }
}

#[test]
fn a_listing_naming_a_path_outside_the_record_is_refused() {
    for name in ["../escape.csv", "/etc/passwd", "a/../../b.csv"] {
        let body = format!(
            r#"{{"files":[{{"name":"{name}","size":1,"download_url":"https://host/x"}}]}}"#
        );
        let server = described_body(&body);
        let source = fetchloom_sources::described_reaching(
            fetchloom_sources::Provider::Figshare,
            server.origin(),
            Limits::default(),
            counter(),
        );
        let error = source
            .list("figshare:1234567", None)
            .expect_err("a member naming a path outside the record was listed");
        assert_eq!(
            error.kind(),
            fetchloom_engine::error::ErrorKind::ReferenceUnresolved,
            "{name} was accepted as a member path"
        );
    }
}

#[test]
fn a_described_provider_that_rate_limits_is_reported_as_one_to_wait_for() {
    let server = TestServer::start(
        Script::serving(b"{}".to_vec()).replying(vec![Reply::Status {
            code: 429,
            retry_after: Some("120".to_owned()),
        }]),
    )
    .unwrap();
    let source = fetchloom_sources::described_reaching(
        fetchloom_sources::Provider::Kaggle,
        server.origin(),
        Limits::default(),
        counter(),
    );

    let error = source.list("kaggle:owner/slug", None).unwrap_err();
    assert_eq!(
        error.kind(),
        fetchloom_engine::error::ErrorKind::NetworkStatus
    );
    assert!(error.retryable(), "a rate limit was reported as terminal");
    assert_eq!(
        error.retry_after(),
        Some(std::time::Duration::from_secs(120)),
        "the wait the source asked for was dropped"
    );
}

#[test]
fn a_described_reference_that_names_no_record_is_refused_by_name() {
    let source = fetchloom_sources::described(
        fetchloom_sources::Provider::Kaggle,
        Limits::default(),
        counter(),
    );
    for refused in ["kaggle:", "kaggle:onlyone"] {
        let error = source.list(refused, None).unwrap_err();
        assert_eq!(
            error.kind(),
            fetchloom_engine::error::ErrorKind::ReferenceUnresolved,
            "{refused} failed as something other than an unresolved reference"
        );
        assert!(
            error.next_action().contains("kaggle:part/part"),
            "{refused} was refused without naming the form to write: {}",
            error.next_action()
        );
    }
}

#[test]
fn a_multiplier_is_refused_when_its_reference_names_no_host() {
    let source = fetchloom_sources::described(
        fetchloom_sources::Provider::Ckan,
        Limits::default(),
        counter(),
    );
    for refused in ["ckan:", "ckan:no-host", "ckan:/a-dataset"] {
        let error = source.list(refused, None).unwrap_err();
        assert_eq!(
            error.kind(),
            fetchloom_engine::error::ErrorKind::ReferenceUnresolved,
            "{refused} named no host and was not refused for it"
        );
    }
}

#[test]
fn a_file_a_listing_named_is_reached_without_asking_the_record_again() {
    let files = TestServer::start(Script::serving(b"the asset bytes".to_vec())).unwrap();
    let body = format!(
        r#"{{"assets":[{{"name":"a.bin","size":15,"digest":null,"browser_download_url":"{}/a.bin"}}]}}"#,
        files.origin()
    );
    let record = described_body(&body);
    let source = fetchloom_sources::described_reaching(
        fetchloom_sources::Provider::GitHubReleases,
        record.origin(),
        Limits::default(),
        counter(),
    );

    source.list("github:o/r", None).unwrap();
    let asked_after_listing = record.received().len();

    let described = source
        .probe("github:o/r/a.bin", None)
        .expect("a file the listing named did not resolve");
    assert!(
        described.location.as_str().ends_with("/a.bin"),
        "a file reference resolved to {}",
        described.location
    );
    assert_eq!(
        record.received().len(),
        asked_after_listing,
        "the record was asked again for a file its own listing had already named"
    );
}

#[test]
fn a_described_refusal_never_carries_a_secret_from_the_reference() {
    let server = described_body("{}");
    let source = fetchloom_sources::described_reaching(
        fetchloom_sources::Provider::Ckan,
        server.origin(),
        Limits::default(),
        counter(),
    );

    let error = source
        .list("ckan:a-dataset?token=super-secret-value", None)
        .unwrap_err();
    let everything = format!("{error:?}{}", error.next_action());
    assert!(
        !everything.contains("super-secret-value"),
        "a query value reached the refusal: {everything}"
    );
}

fn registration(landing: &str, registrant: &str) -> String {
    format!(
        r#"{{"data":{{"id":"10.1/x","type":"dois","attributes":{{"url":"{landing}"}},
        "relationships":{{"client":{{"data":{{"id":"{registrant}","type":"clients"}}}}}}}}}}"#
    )
}

#[test]
fn a_doi_resolves_to_the_reference_of_the_provider_that_holds_it() {
    let server = described_body(&registration(
        "https://dataverse.harvard.edu/citation?persistentId=doi:10.7910/DVN/OMV93V",
        "gdcc.harvard-dv",
    ));
    let router =
        fetchloom_sources::DoiRouter::reaching(server.origin(), Limits::default(), counter());

    let provider = router.route("doi:10.7910/DVN/OMV93V", None).unwrap();
    assert_eq!(
        provider,
        "dataverse:dataverse.harvard.edu/doi:10.7910/DVN/OMV93V"
    );
    assert!(
        fetchloom_sources::described(
            fetchloom_sources::Provider::Dataverse,
            Limits::default(),
            counter()
        )
        .serves(&provider)
        .is_some(),
        "the router answered a reference no adapter of this build serves"
    );
    let asked: Vec<String> = server
        .received()
        .into_iter()
        .map(|request| request.target)
        .collect();
    assert!(
        asked
            .iter()
            .any(|target| target.ends_with("/dois/10.7910/DVN/OMV93V")),
        "the registry was asked for something other than the DOI: {asked:?}"
    );
}

#[test]
fn a_doi_whose_provider_has_no_adapter_names_the_provider_and_what_is_missing() {
    let server = described_body(&registration(
        "https://example.org/records/12345",
        "some.registrant",
    ));
    let router =
        fetchloom_sources::DoiRouter::reaching(server.origin(), Limits::default(), counter());

    let error = router.route("doi:10.1000/xyz", None).unwrap_err();
    assert_eq!(
        error.kind(),
        fetchloom_engine::error::ErrorKind::ReferenceUnresolved
    );
    let said = error.next_action();
    assert!(
        said.contains("some.registrant"),
        "the refusal did not name the registrant: {said}"
    );
    assert!(
        said.contains("example.org"),
        "the refusal did not name where the DOI resolves: {said}"
    );
    assert!(
        said.contains("adapter"),
        "the refusal did not say what serving it would need: {said}"
    );
}

#[test]
fn a_registration_this_build_cannot_read_is_refused_rather_than_guessed_at() {
    for body in ["not json", "{}", r#"{"data":{"attributes":{}}}"#] {
        let server = described_body(body);
        let router =
            fetchloom_sources::DoiRouter::reaching(server.origin(), Limits::default(), counter());
        let error = router
            .route("doi:10.1000/xyz", None)
            .expect_err("a registration this build cannot read was read anyway");
        assert_eq!(
            error.kind(),
            fetchloom_engine::error::ErrorKind::ReferenceUnresolved,
            "{body} failed as something other than an unresolved reference"
        );
    }
}

#[test]
fn a_name_that_is_not_a_doi_is_refused_before_anything_is_asked() {
    let server = described_body("{}");
    let router =
        fetchloom_sources::DoiRouter::reaching(server.origin(), Limits::default(), counter());

    for refused in ["doi:", "doi:10.1000", "zenodo:1", "doi:notadoi/x"] {
        let error = router.route(refused, None).unwrap_err();
        assert_eq!(
            error.kind(),
            fetchloom_engine::error::ErrorKind::ReferenceUnresolved,
            "{refused} was treated as a DOI"
        );
    }
    assert!(
        server.received().is_empty(),
        "a name that is not a DOI was sent to the registry anyway"
    );
}

#[test]
fn a_doi_the_registry_does_not_hold_says_to_check_the_name() {
    let server = TestServer::start(
        Script::serving(b"{}".to_vec()).replying(vec![Reply::Status {
            code: 404,
            retry_after: None,
        }]),
    )
    .unwrap();
    let router =
        fetchloom_sources::DoiRouter::reaching(server.origin(), Limits::default(), counter());

    let error = router.route("doi:10.1000/absent", None).unwrap_err();
    assert_eq!(
        error.kind(),
        fetchloom_engine::error::ErrorKind::ReferenceUnresolved
    );
    assert!(
        !error.retryable(),
        "a DOI the registry does not hold was reported as worth another attempt"
    );
}

#[test]
fn a_dataverse_file_named_cold_is_reached_by_peeling_the_record_off_the_reference() {
    let files = TestServer::start(Script::serving(b"the tabular bytes".to_vec())).unwrap();
    let record = TestServer::start(Script::serving(
        br#"{"status":"OK","data":[{"dataFile":{"id":7,"filename":"dmoz.tab","filesize":17,
            "checksum":{"type":"MD5","value":"x"}}}]}"#
            .to_vec(),
    ))
    .unwrap();
    let source = fetchloom_sources::described_reaching(
        fetchloom_sources::Provider::Dataverse,
        record.origin(),
        Limits::default(),
        counter(),
    );

    let described = source
        .probe("dataverse:doi:10.7910/DVN/OMV93V/dmoz.tab", None)
        .expect("a file under a record whose identifier holds slashes did not resolve");
    assert!(
        described
            .location
            .as_str()
            .ends_with("/api/access/datafile/7"),
        "a peeled reference resolved to {} rather than to the file the record states",
        described.location
    );
    let _ = files;
}

#[test]
fn a_record_reference_a_described_provider_serves_is_a_container_and_a_file_is_not() {
    let kaggle = fetchloom_sources::described(
        fetchloom_sources::Provider::Kaggle,
        Limits::default(),
        counter(),
    );
    assert_eq!(
        kaggle.serves("kaggle:uciml/iris"),
        Some(Serves::Container),
        "a record was not served as a container"
    );
    assert_eq!(
        kaggle.serves("kaggle:uciml/iris/Iris.csv"),
        Some(Serves::Object),
        "a file inside a record was served as a container, so a run would try to list it"
    );
}
