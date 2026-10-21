//! The declarative adapter against the zenodo record.

use std::sync::Arc;

use fl_model::digest::{Algorithm, Digest};
use fl_model::error::ErrorKind;
use fl_model::reference::Reference;
use fl_sources::{Declarative, Source};
use wiremock::{MockServer, ResponseTemplate};

use crate::support::{self, Failure, RECORD};

#[tokio::test]
async fn the_zenodo_record_resolves_to_its_one_file() -> Result<(), Failure> {
    let server = MockServer::start().await;
    support::serve(&server, "/api/records/3608990", RECORD).await;
    let client = support::client()?;
    let adapter = support::declarative(&server, &client)?;
    let reference: Reference = "zenodo:3608990".parse()?;
    let listing = adapter.resolve(&reference).await?;
    assert_eq!(listing.resolved.as_str(), "zenodo:3608990");
    assert_eq!(listing.meta.title.as_deref(), Some("DatasetIris"));
    assert_eq!(listing.meta.license.as_deref(), Some("cc-by-4.0"));
    assert_eq!(listing.meta.doi.as_deref(), Some("10.5281/zenodo.3608990"));
    assert_eq!(listing.meta.version, None);
    assert_eq!(listing.entries.len(), 1);
    let entry = &listing.entries[0];
    assert_eq!(entry.path.as_str(), "iris.csv");
    assert_eq!(entry.size, Some(4177));
    assert_eq!(
        entry.stated,
        vec![Digest::from_hex(
            Algorithm::Md5,
            "395d6abd001abf3760a1a794ef4c453c"
        )?]
    );
    assert_eq!(
        entry.urls[0].to_string(),
        format!(
            "{}/api/records/3608990/files/iris.csv/content",
            server.uri()
        )
    );
    assert_eq!(client.counters().read().requests, 1);
    Ok(())
}

#[tokio::test]
async fn a_concept_id_follows_the_redirect_to_the_record() -> Result<(), Failure> {
    let server = MockServer::start().await;
    support::answer(
        &server,
        "/api/records/3608989",
        ResponseTemplate::new(302).insert_header("location", "/api/records/3608990"),
    )
    .await;
    support::serve(&server, "/api/records/3608990", RECORD).await;
    let client = support::client()?;
    let adapter = support::declarative(&server, &client)?;
    let reference: Reference = "zenodo:3608989".parse()?;
    let listing = adapter.resolve(&reference).await?;
    assert_eq!(listing.resolved.as_str(), "zenodo:3608990");
    assert_eq!(client.counters().read().requests, 2);
    Ok(())
}

#[tokio::test]
async fn a_record_server_nobody_trusted_is_never_contacted_at_a_private_address()
-> Result<(), Failure> {
    let server = MockServer::start().await;
    support::serve(&server, "/api/records/3608990", RECORD).await;
    let client = support::client()?;
    let adapter = Declarative::from_toml(&support::adapter_text(&server), Arc::clone(&client))?;
    assert_eq!(adapter.record_host(), Some("127.0.0.1"));
    let reference: Reference = "zenodo:3608990".parse()?;
    let error = adapter.resolve(&reference).await.err().ok_or("resolved")?;
    assert_eq!(error.kind(), ErrorKind::NetworkRefused);
    assert_eq!(client.counters().read().bytes_down, 0);
    assert!(
        server
            .received_requests()
            .await
            .unwrap_or_default()
            .is_empty()
    );
    Ok(())
}

#[tokio::test]
async fn a_missing_record_is_unresolved() -> Result<(), Failure> {
    let server = MockServer::start().await;
    support::answer(&server, "/api/records/1", ResponseTemplate::new(404)).await;
    let client = support::client()?;
    let adapter = support::declarative(&server, &client)?;
    let reference: Reference = "zenodo:1".parse()?;
    let error = adapter.resolve(&reference).await.err().ok_or("resolved")?;
    assert_eq!(error.kind(), ErrorKind::ReferenceUnresolved);
    Ok(())
}

#[tokio::test]
async fn a_file_without_a_key_is_a_record_error_naming_the_path() -> Result<(), Failure> {
    let server = MockServer::start().await;
    let broken = RECORD.replace("\"key\"", "\"name\"");
    support::serve(&server, "/api/records/3608990", &broken).await;
    let client = support::client()?;
    let adapter = support::declarative(&server, &client)?;
    let reference: Reference = "zenodo:3608990".parse()?;
    let error = adapter.resolve(&reference).await.err().ok_or("resolved")?;
    assert_eq!(error.kind(), ErrorKind::NetworkProtocol);
    assert!(error.to_string().contains("$.key"), "{error}");
    Ok(())
}

#[tokio::test]
async fn an_invalid_identifier_is_refused_without_a_request() -> Result<(), Failure> {
    let server = MockServer::start().await;
    let client = support::client()?;
    let adapter = support::declarative(&server, &client)?;
    let reference: Reference = "zenodo:a/b".parse()?;
    let error = adapter.resolve(&reference).await.err().ok_or("resolved")?;
    assert_eq!(error.kind(), ErrorKind::ReferenceInvalid);
    assert_eq!(client.counters().read().requests, 0);
    Ok(())
}

#[tokio::test]
async fn an_unknown_key_in_the_adapter_is_named() -> Result<(), Failure> {
    let server = MockServer::start().await;
    let text = support::adapter_text(&server).replacen(
        "name = \"zenodo\"",
        "name = \"zenodo\"\nbogus = \"x\"",
        1,
    );
    let error = Declarative::from_toml(&text, support::client()?)
        .err()
        .ok_or("loaded")?;
    assert_eq!(error.kind(), ErrorKind::LocalInvalidConfig);
    assert!(error.to_string().contains("bogus"), "{error}");
    Ok(())
}
