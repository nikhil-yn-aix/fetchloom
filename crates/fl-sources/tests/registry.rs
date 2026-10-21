//! The registry: scheme lookup, the http adapter and filters.

use fl_model::error::ErrorKind;
use fl_model::filter::{Filters, Pattern};
use fl_model::reference::Reference;
use fl_sources::Registry;
use wiremock::MockServer;

use crate::support::{self, Failure};

#[tokio::test]
async fn a_pattern_matching_nothing_is_an_empty_selection() -> Result<(), Failure> {
    let registry = Registry::builtin(support::client()?)?;
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
    let registry = Registry::builtin(support::client()?)?;
    let reference: Reference = "figshare:12345".parse()?;
    let error = registry
        .resolve(&reference, &support::no_filters()?)
        .await
        .err()
        .ok_or("resolved")?;
    assert_eq!(error.kind(), ErrorKind::ReferenceUnresolved);
    assert!(error.to_string().contains("figshare"), "{error}");
    Ok(())
}

#[tokio::test]
async fn an_https_url_is_one_file_named_by_its_last_segment_without_a_request()
-> Result<(), Failure> {
    let client = support::client()?;
    let registry = Registry::builtin(client.clone())?;
    let reference: Reference = "https://example.org/data/my%20iris.csv?token=1".parse()?;
    let listing = registry
        .resolve(&reference, &support::no_filters()?)
        .await?;
    assert_eq!(listing.resolved.as_str(), reference.as_str());
    assert_eq!(listing.entries.len(), 1);
    assert_eq!(listing.entries[0].path.as_str(), "my iris.csv");
    assert_eq!(listing.entries[0].size, None);
    assert!(listing.entries[0].stated.is_empty());
    assert_eq!(client.counters().read().requests, 0);
    Ok(())
}

#[tokio::test]
async fn a_url_without_a_file_name_is_invalid() -> Result<(), Failure> {
    let registry = Registry::builtin(support::client()?)?;
    let reference: Reference = "https://example.org/".parse()?;
    let error = registry
        .resolve(&reference, &support::no_filters()?)
        .await
        .err()
        .ok_or("resolved")?;
    assert_eq!(error.kind(), ErrorKind::ReferenceInvalid);
    Ok(())
}

#[tokio::test]
async fn the_registry_resolves_a_zenodo_reference_through_the_embedded_adapter()
-> Result<(), Failure> {
    let server = MockServer::start().await;
    let client = support::client()?;
    let mut registry = Registry::default();
    registry.add(std::sync::Arc::new(support::declarative(&server, &client)?));
    support::serve(&server, "/api/records/3608990", support::RECORD).await;
    let reference: Reference = "zenodo:3608990".parse()?;
    let filters = Filters::new(vec!["*.csv".parse::<Pattern>()?], Vec::new(), None, None)?;
    let listing = registry.resolve(&reference, &filters).await?;
    assert_eq!(listing.entries.len(), 1);
    Ok(())
}
