//! The registry: scheme lookup, DOI routing, the http adapter, filters and case collisions.

use std::sync::Arc;

use fl_model::error::ErrorKind;
use fl_model::filter::{Filters, Pattern};
use fl_model::reference::Reference;
use fl_sources::Registry;
use serde_json::json;
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::support::{self, Failure};

const APACHE: &str = include_str!("../../../fixtures/responses/http/apache.html");

#[tokio::test]
async fn a_pattern_matching_nothing_is_an_empty_selection() -> Result<(), Failure> {
    let registry = Registry::builtin(&support::client()?)?;
    let filters = Filters::new(
        vec!["*.parquet".parse::<Pattern>()?],
        Vec::new(),
        None,
        None,
    )?;
    let reference: Reference = "https://example.org/data/iris.csv".parse()?;
    let error = registry
        .resolve(&reference, &filters)
        .await
        .err()
        .ok_or("resolved")?;
    assert_eq!(error.kind(), ErrorKind::SelectionEmpty);
    Ok(())
}

#[tokio::test]
async fn an_unknown_scheme_is_unresolved_and_names_the_scheme() -> Result<(), Failure> {
    let registry = Registry::builtin(&support::client()?)?;
    let reference: Reference = "nowhere:12345".parse()?;
    let error = registry
        .resolve(&reference, &support::no_filters()?)
        .await
        .err()
        .ok_or("resolved")?;
    assert_eq!(error.kind(), ErrorKind::ReferenceUnresolved);
    assert!(error.to_string().contains("nowhere"), "{error}");
    Ok(())
}

#[tokio::test]
async fn an_https_url_is_one_file_named_by_its_last_segment_without_a_request()
-> Result<(), Failure> {
    let client = support::client()?;
    let registry = Registry::builtin(&client)?;
    let reference: Reference = "https://example.org/data/my%20iris.csv?token=1".parse()?;
    let listing = registry
        .resolve(&reference, &support::no_filters()?)
        .await?;
    assert_eq!(listing.resolved.as_str(), reference.as_str());
    assert_eq!(listing.entries.len(), 1);
    assert_eq!(listing.entries[0].path.as_str(), "my iris.csv");
    assert_eq!(client.counters().read().requests, 0);
    Ok(())
}

#[tokio::test]
async fn a_url_ending_in_a_slash_lists_its_index_one_request_per_directory() -> Result<(), Failure>
{
    let server = MockServer::start().await;
    let index = |links: &str| {
        ResponseTemplate::new(200).set_body_string(format!(
            "<pre><a href=\"../\">../</a> <a href=\"?C=N;O=D\">Name</a> {links}</pre>"
        ))
    };
    Mock::given(method("GET"))
        .and(path("/pub/"))
        .respond_with(index(
            "<a href=\"a.csv\">a</a> <a href=\"sub/\">sub/</a> <a href=\"https://elsewhere.org/x\">x</a>",
        ))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/pub/sub/"))
        .respond_with(index(
            "<a href=\"b.csv\">b</a> <a href=\"/pub/a.csv\">up</a>",
        ))
        .mount(&server)
        .await;
    let client = support::client()?;
    let registry = Registry::builtin(&client)?;
    let reference: Reference = format!("{}/pub/", server.uri()).parse()?;
    let listing = registry
        .resolve(&reference, &support::no_filters()?)
        .await?;
    let paths: Vec<&str> = listing
        .entries
        .iter()
        .map(|entry| entry.path.as_str())
        .collect();
    assert_eq!(paths, ["a.csv", "sub/b.csv"]);
    assert_eq!(client.counters().read().requests, 2);
    Ok(())
}

#[tokio::test]
async fn an_apache_index_lists_its_files_and_walks_its_directories() -> Result<(), Failure> {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/dist/ant/"))
        .respond_with(ResponseTemplate::new(200).set_body_string(APACHE))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<p>empty</p>"))
        .mount(&server)
        .await;
    let client = support::client()?;
    let registry = Registry::builtin(&client)?;
    let reference: Reference = format!("{}/dist/ant/", server.uri()).parse()?;
    let listing = registry
        .resolve(&reference, &support::no_filters()?)
        .await?;
    let requests = server.received_requests().await.unwrap_or_default();
    assert!(requests.len() > 1, "{}", requests.len());
    assert!(
        requests
            .iter()
            .all(|request| request.url.path().starts_with("/dist/ant/"))
    );
    assert!(
        listing
            .entries
            .iter()
            .all(|entry| entry.urls[0].to_string().contains("/dist/ant/"))
    );
    Ok(())
}

#[tokio::test]
async fn names_that_differ_only_by_case_are_refused() -> Result<(), Failure> {
    let server = MockServer::start().await;
    let index = "<a href=\"Data.csv\">x</a> <a href=\"data.CSV\">y</a>";
    Mock::given(method("GET"))
        .and(path("/d/"))
        .respond_with(ResponseTemplate::new(200).set_body_string(index))
        .mount(&server)
        .await;
    let registry = Registry::builtin(&support::client()?)?;
    let reference: Reference = format!("{}/d/", server.uri()).parse()?;
    let error = registry
        .resolve(&reference, &support::no_filters()?)
        .await
        .err()
        .ok_or("resolved")?;
    assert_eq!(error.kind(), ErrorKind::LocalUnrepresentable);
    assert!(error.to_string().contains("data.CSV"), "{error}");
    Ok(())
}

async fn doi(server: &MockServer, doi: &str, location: &str) {
    Mock::given(method("GET"))
        .and(path(format!("/{doi}")))
        .respond_with(ResponseTemplate::new(302).insert_header("location", location))
        .mount(server)
        .await;
}

#[tokio::test]
async fn a_doi_routes_to_the_source_of_its_landing_page() -> Result<(), Failure> {
    let resolver = MockServer::start().await;
    let records = MockServer::start().await;
    let landing = format!("{}/record/3608990", records.uri());
    doi(&resolver, "10.5281/zenodo.3608990", &landing).await;
    support::serve(&records, "/api/records/3608990", support::RECORD).await;
    let client = support::client()?;
    let mut registry = Registry::new(Arc::clone(&client))
        .with_resolver(Url::parse(&format!("{}/", resolver.uri()))?);
    registry.add(Arc::new(support::declarative(&records, &client)?));
    client.trust(
        &fl_transfer::url::SafeUrl::parse(&resolver.uri())?
            .origin()
            .ok_or("origin")?,
    );
    let reference: Reference = "doi:10.5281/zenodo.3608990".parse()?;
    let listing = registry
        .resolve(&reference, &support::no_filters()?)
        .await?;
    assert_eq!(listing.resolved.as_str(), "zenodo:3608990");
    assert_eq!(client.counters().read().requests, 2);
    Ok(())
}

#[tokio::test]
async fn a_doi_landing_on_an_unknown_host_names_host_and_page() -> Result<(), Failure> {
    let resolver = MockServer::start().await;
    let landing = MockServer::start().await;
    let page = format!("{}/articles/s41597", landing.uri());
    doi(&resolver, "10.1038/s41597", &page).await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&landing)
        .await;
    let client = support::client()?;
    let registry =
        Registry::builtin(&client)?.with_resolver(Url::parse(&format!("{}/", resolver.uri()))?);
    for server in [&resolver, &landing] {
        client.trust(
            &fl_transfer::url::SafeUrl::parse(&server.uri())?
                .origin()
                .ok_or("origin")?,
        );
    }
    let reference: Reference = "doi:10.1038/s41597".parse()?;
    let error = registry
        .resolve(&reference, &support::no_filters()?)
        .await
        .err()
        .ok_or("resolved")?;
    assert_eq!(error.kind(), ErrorKind::ReferenceUnresolved);
    assert!(error.to_string().contains(&page), "{error}");
    assert!(error.to_string().contains("127.0.0.1"), "{error}");
    Ok(())
}
