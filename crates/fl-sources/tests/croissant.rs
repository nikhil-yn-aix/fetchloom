//! Croissant metadata: file objects, file sets and the record's own description.

use fl_model::error::ErrorKind;
use fl_sources::{Croissant, Listing, Source};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::support::{self, Failure};

const SQUAD: &str = include_str!("../../../fixtures/responses/croissant/squad.json");

#[tokio::test]
async fn file_sets_inside_a_repository_point_to_the_repository_reference() -> Result<(), Failure> {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/datasets/rajpurkar/squad/croissant"))
        .respond_with(ResponseTemplate::new(200).set_body_string(SQUAD))
        .mount(&server)
        .await;
    let reference = format!(
        "croissant:{}/api/datasets/rajpurkar/squad/croissant",
        server.uri()
    );
    let error = Croissant::new(support::client()?)
        .resolve(&reference.parse()?)
        .await
        .err()
        .ok_or("resolved")?;
    assert_eq!(error.kind(), ErrorKind::ReferenceInvalid);
    assert!(error.to_string().contains("file sets"), "{error}");
    Ok(())
}

async fn listing_of(metadata: &Value) -> Result<Listing, Failure> {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/meta.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(metadata))
        .mount(&server)
        .await;
    let reference = format!("croissant:{}/meta.json", server.uri());
    Ok(Croissant::new(support::client()?)
        .resolve(&reference.parse()?)
        .await?)
}

fn paths(listing: &Listing) -> Vec<&str> {
    listing
        .entries
        .iter()
        .map(|entry| entry.path.as_str())
        .collect()
}

#[tokio::test]
async fn croissant_serves_its_own_scheme() -> Result<(), Failure> {
    let source = Croissant::new(support::client()?);
    assert_eq!(source.schemes(), ["croissant"]);
    Ok(())
}

#[tokio::test]
async fn a_distribution_may_be_one_object() -> Result<(), Failure> {
    let listing = listing_of(&json!({
        "distribution": {"@type": "cr:FileObject", "name": "a.csv", "contentUrl": "https://example.org/a.csv"}
    }))
    .await?;
    assert_eq!(paths(&listing), ["a.csv"]);
    Ok(())
}

#[tokio::test]
async fn only_objects_typed_as_files_are_listed() -> Result<(), Failure> {
    let url = "https://example.org/x.csv";
    let listing = listing_of(&json!({
        "distribution": [
            {"@type": ["sc:Thing", "FileObject"], "name": "typed.csv", "contentUrl": url},
            {"@type": "sc:FileObject", "name": "short.csv", "contentUrl": url},
            {"@type": "cr:Other", "name": "other.csv", "contentUrl": url},
            {"@type": ["cr:Other", "sc:Thing"], "name": "others.csv", "contentUrl": url},
            {"name": "untyped.csv", "contentUrl": url},
            {"@type": 5, "name": "numbered.csv", "contentUrl": url}
        ]
    }))
    .await?;
    assert_eq!(paths(&listing), ["typed.csv", "short.csv"]);
    Ok(())
}

#[tokio::test]
async fn only_http_locations_that_are_not_repositories_are_listed() -> Result<(), Failure> {
    let listing = listing_of(&json!({
        "distribution": [
            {"@type": "cr:FileObject", "name": "ftp.csv", "contentUrl": "ftp://example.org/a.csv"},
            {"@type": "cr:FileObject", "name": "repo", "contentUrl": "https://example.org/r.git", "encodingFormat": "git+https"},
            {"@type": "cr:FileObject", "name": "plain.csv", "contentUrl": "https://example.org/p.csv", "encodingFormat": "text/csv"},
            {"@type": "cr:FileObject", "name": "none.csv"}
        ]
    }))
    .await?;
    assert_eq!(paths(&listing), ["plain.csv"]);
    Ok(())
}

#[tokio::test]
async fn a_plain_id_names_the_file_before_its_name_and_a_url_or_fragment_id_does_not()
-> Result<(), Failure> {
    let object = |id: &str, name: &str| json!({"@type": "cr:FileObject", "@id": id, "name": name, "contentUrl": "https://example.org/x"});
    let listing = listing_of(&json!({
        "distribution": [
            object("data/id.csv", "ignored.csv"),
            object("urn:thing", "colon.csv"),
            object("#fragment", "hash.csv")
        ]
    }))
    .await?;
    assert_eq!(paths(&listing), ["data/id.csv", "colon.csv", "hash.csv"]);
    Ok(())
}

#[tokio::test]
async fn sizes_are_numbers_or_text_with_a_unit() -> Result<(), Failure> {
    let object = |name: &str, size: Value| json!({"@type": "cr:FileObject", "name": name, "contentUrl": "https://example.org/x", "contentSize": size});
    let listing = listing_of(&json!({
        "distribution": [
            object("number", json!(1234)),
            object("text", json!(" 56 B")),
            object("words", json!("big")),
            object("flag", json!(true))
        ]
    }))
    .await?;
    let sizes: Vec<Option<u64>> = listing.entries.iter().map(|entry| entry.size).collect();
    assert_eq!(sizes, [Some(1234), Some(56), None, None]);
    Ok(())
}

#[tokio::test]
async fn file_sets_beside_files_are_not_an_error() -> Result<(), Failure> {
    let listing = listing_of(&json!({
        "distribution": [
            {"@type": "cr:FileSet", "name": "set"},
            {"@type": "cr:FileObject", "name": "a.csv", "contentUrl": "https://example.org/a.csv"}
        ]
    }))
    .await?;
    assert_eq!(paths(&listing), ["a.csv"]);
    let empty = listing_of(&json!({"name": "nothing"})).await?;
    assert!(empty.entries.is_empty());
    Ok(())
}

#[tokio::test]
async fn the_metadata_names_title_version_license_and_creators() -> Result<(), Failure> {
    let listing = listing_of(&json!({
        "name": "Iris",
        "version": "1.2",
        "license": "MIT",
        "creator": [
            {"name": "Ada"},
            {"givenName": "Grace", "familyName": "Hopper"},
            {"givenName": "Solo"},
            {"familyName": "Only"},
            {"url": "https://example.org"}
        ]
    }))
    .await?;
    assert_eq!(listing.meta.title.as_deref(), Some("Iris"));
    assert_eq!(listing.meta.version.as_deref(), Some("1.2"));
    assert_eq!(listing.meta.license.as_deref(), Some("MIT"));
    assert_eq!(
        listing.meta.creators,
        ["Ada", "Hopper, Grace", "Solo", "Only"]
    );
    Ok(())
}

#[tokio::test]
async fn a_license_is_a_name_the_first_of_a_list_or_an_object() -> Result<(), Failure> {
    let license = |value: Value| async move {
        listing_of(&json!({"license": value}))
            .await
            .map(|listing| listing.meta.license)
    };
    assert_eq!(
        license(json!(["Apache-2.0", "MIT"])).await?.as_deref(),
        Some("Apache-2.0")
    );
    assert_eq!(
        license(json!({"name": "BSD-3-Clause"})).await?.as_deref(),
        Some("BSD-3-Clause")
    );
    assert_eq!(
        license(json!({"@id": "https://spdx.org/licenses/ISC", "name": "other"}))
            .await?
            .as_deref(),
        Some("ISC")
    );
    assert_eq!(license(json!(7)).await?, None);
    Ok(())
}

#[tokio::test]
async fn one_creator_may_be_an_object() -> Result<(), Failure> {
    let listing = listing_of(&json!({"creator": {"name": "Ada"}})).await?;
    assert_eq!(listing.meta.creators, ["Ada"]);
    Ok(())
}
