//! The registry: scheme lookup, DOI routing, the http adapter, filters and case collisions.

use std::collections::BTreeMap;
use std::sync::Arc;

use fl_model::error::ErrorKind;
use fl_model::filter::{Filters, Pattern};
use fl_model::reference::Reference;
use fl_sources::{Listing, Meta, Registry, Source, SourceError};
use fl_transfer::url::SafeUrl;
use futures_util::future::BoxFuture;
use serde_json::json;
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::support::{self, Failure};

const APACHE: &str = include_str!("../../../fixtures/responses/http/apache.html");
const LOCATIONS: &str = include_str!("../../../fixtures/responses/doi/locations.toml");

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
    client.trust(&SafeUrl::parse(&resolver.uri())?.origin().ok_or("origin")?);
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
        .and(path("/articles/s41597"))
        .respond_with(
            ResponseTemplate::new(303).insert_header("location", "/articles/s41597?error=cookies"),
        )
        .with_priority(1)
        .mount(&landing)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&landing)
        .await;
    let client = support::client()?;
    let registry =
        Registry::builtin(&client)?.with_resolver(Url::parse(&format!("{}/", resolver.uri()))?);
    for server in [&resolver, &landing] {
        client.trust(&SafeUrl::parse(&server.uri())?.origin().ok_or("origin")?);
    }
    let reference: Reference = "doi:10.1038/s41597".parse()?;
    let error = registry
        .resolve(&reference, &support::no_filters()?)
        .await
        .err()
        .ok_or("resolved")?;
    assert_eq!(error.kind(), ErrorKind::ReferenceUnresolved);
    assert!(error.to_string().contains(&format!("{page},")), "{error}");
    assert!(error.to_string().contains("127.0.0.1"), "{error}");
    Ok(())
}

/// A source that lists nothing and answers with the reference it was asked for.
struct Echo(Vec<String>);

impl Source for Echo {
    fn schemes(&self) -> &[String] {
        &self.0
    }

    fn resolve<'a>(
        &'a self,
        reference: &'a Reference,
    ) -> BoxFuture<'a, Result<Listing, SourceError>> {
        Box::pin(std::future::ready(Ok(Listing {
            resolved: reference.clone(),
            meta: Meta::default(),
            entries: Vec::new(),
        })))
    }
}

#[tokio::test]
async fn dois_of_zenodo_figshare_and_dataverse_route_by_the_host_they_land_on()
-> Result<(), Failure> {
    let recorded: BTreeMap<String, String> = toml::from_str(LOCATIONS)?;
    let resolver = MockServer::start().await;
    for (doi, location) in &recorded {
        Mock::given(method("GET"))
            .and(path(format!("/{doi}")))
            .respond_with(ResponseTemplate::new(302).insert_header("location", location.as_str()))
            .mount(&resolver)
            .await;
    }
    let client = support::client()?;
    client.trust(&SafeUrl::parse(&resolver.uri())?.origin().ok_or("origin")?);
    let mut registry =
        Registry::builtin(&client)?.with_resolver(Url::parse(&format!("{}/", resolver.uri()))?);
    let schemes = ["zenodo", "figshare", "dataverse"].map(str::to_owned);
    registry.add(Arc::new(Echo(schemes.to_vec())));
    let routes = [
        ("10.5281/zenodo.3608990", "zenodo:3608990"),
        ("10.6084/m9.figshare.34744689.v1", "figshare:34744689@1"),
        (
            "10.7910/DVN/TJCLKP",
            "dataverse:dataverse.harvard.edu/doi:10.7910/DVN/TJCLKP",
        ),
    ];
    for (doi, routed) in routes {
        let listing = registry
            .resolve(&format!("doi:{doi}").parse()?, &support::no_filters()?)
            .await?;
        assert_eq!(listing.resolved.as_str(), routed, "{doi}");
    }
    assert_eq!(client.counters().read().requests, 3);
    Ok(())
}

#[tokio::test]
async fn an_index_reached_through_a_redirect_lists_links_under_where_it_landed()
-> Result<(), Failure> {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/pub/"))
        .respond_with(ResponseTemplate::new(301).insert_header("location", "/pub/v2/"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/pub/v2/"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<a href=\"a.csv\">a</a>"))
        .mount(&server)
        .await;
    let registry = Registry::builtin(&support::client()?)?;
    let reference: Reference = format!("{}/pub/", server.uri()).parse()?;
    let listing = registry
        .resolve(&reference, &support::no_filters()?)
        .await?;
    assert_eq!(listing.entries.len(), 1);
    assert_eq!(
        listing.entries[0].urls[0].to_string(),
        format!("{}/pub/v2/a.csv", server.uri())
    );
    Ok(())
}
