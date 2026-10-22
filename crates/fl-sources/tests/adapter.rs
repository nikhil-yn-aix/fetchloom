//! The declarative engine: records, forms, captures, the five kinds of paging, folders,
//! digests, landing pages and search, against toy APIs and the zenodo record.

use std::sync::Arc;

use fl_model::error::ErrorKind;
use fl_model::reference::Reference;
use fl_sources::{Declarative, Searchable, Source};
use serde_json::json;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::support::{self, Failure, RECORD};

const TOY: &str = r#"
[adapter]
name = "toy"
scheme = "toy"
pattern = "{id}"
record = "BASE/records/{id}"
resolved = "toy:{$.id}"
files = "$.files[*]"
path = "$.name"
size = "$.size"
url = "BASE/download/{id}/{%$.name}"
"#;

fn toy(server: &MockServer, extra: &str) -> Result<Declarative, Failure> {
    let text = format!("{TOY}{extra}").replace("BASE", &server.uri());
    let client = support::client()?;
    let adapter = Declarative::from_toml(&text, Arc::clone(&client))?;
    client.trust(adapter.record_origin().ok_or("no record host")?);
    Ok(adapter)
}

fn page(files: &[&str], extra: &serde_json::Value) -> ResponseTemplate {
    let files: Vec<_> = files
        .iter()
        .map(|name| json!({"name": name, "size": 1}))
        .collect();
    let mut body = json!({"id": 7, "files": files});
    if let (Some(body), Some(extra)) = (body.as_object_mut(), extra.as_object()) {
        body.extend(extra.clone());
    }
    ResponseTemplate::new(200).set_body_json(body)
}

async fn mount(server: &MockServer, query: Option<(&str, &str)>, answer: ResponseTemplate) {
    let mock = Mock::given(method("GET")).and(path("/records/7"));
    match query {
        Some((name, value)) => {
            mock.and(query_param(name, value))
                .respond_with(answer)
                .with_priority(1)
                .mount(server)
                .await;
        }
        None => mock.respond_with(answer).mount(server).await,
    }
}

async fn listed(adapter: &Declarative, reference: &str) -> Result<Vec<String>, Failure> {
    let reference: Reference = reference.parse()?;
    let listing = adapter.resolve(&reference).await?;
    Ok(listing
        .entries
        .iter()
        .map(|entry| entry.path.as_str().to_owned())
        .collect())
}

#[tokio::test]
async fn the_zenodo_record_states_its_metadata_with_the_license_in_spdx_case() -> Result<(), Failure>
{
    let server = MockServer::start().await;
    support::serve(&server, "/api/records/3608990", RECORD).await;
    let client = support::client()?;
    let adapter = support::declarative(&server, &client)?;
    let listing = adapter.resolve(&"zenodo:3608990".parse()?).await?;
    assert_eq!(listing.meta.title.as_deref(), Some("DatasetIris"));
    assert_eq!(listing.meta.license.as_deref(), Some("CC-BY-4.0"));
    assert_eq!(listing.meta.doi.as_deref(), Some("10.5281/zenodo.3608990"));
    assert_eq!(listing.meta.creators, ["Montero Loaiza, David"]);
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
    let listing = adapter.resolve(&"zenodo:3608989".parse()?).await?;
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
    let error = adapter
        .resolve(&"zenodo:3608990".parse()?)
        .await
        .err()
        .ok_or("resolved")?;
    assert_eq!(error.kind(), ErrorKind::NetworkRefused);
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
async fn a_file_without_a_key_is_a_record_error_naming_the_path() -> Result<(), Failure> {
    let server = MockServer::start().await;
    let broken = RECORD.replace("\"key\"", "\"name\"");
    support::serve(&server, "/api/records/3608990", &broken).await;
    let client = support::client()?;
    let adapter = support::declarative(&server, &client)?;
    let error = adapter
        .resolve(&"zenodo:3608990".parse()?)
        .await
        .err()
        .ok_or("resolved")?;
    assert_eq!(error.kind(), ErrorKind::NetworkProtocol);
    assert!(error.to_string().contains("$.key"), "{error}");
    Ok(())
}

#[tokio::test]
async fn an_identifier_the_pattern_refuses_is_invalid_without_a_request() -> Result<(), Failure> {
    let server = MockServer::start().await;
    let client = support::client()?;
    let adapter = support::declarative(&server, &client)?;
    let error = adapter
        .resolve(&"zenodo:a/b".parse()?)
        .await
        .err()
        .ok_or("resolved")?;
    assert_eq!(error.kind(), ErrorKind::ReferenceInvalid);
    assert!(error.to_string().contains("{id}"), "{error}");
    assert_eq!(client.counters().read().requests, 0);
    Ok(())
}

#[tokio::test]
async fn adapter_files_that_cannot_work_name_the_key_at_fault() -> Result<(), Failure> {
    let server = MockServer::start().await;
    let cases = [
        ("bogus = \"x\"\n", "bogus"),
        ("listing = \"BASE/l/{nope}\"\n", "nope"),
        ("folders = \"$.dirs[*]\"\n", "descend"),
        (
            "auth = { header = \"A\", format = \"plain\", env = \"T\" }\n",
            "{token}",
        ),
    ];
    for (extra, named) in cases {
        let text = TOY
            .replacen("name = \"toy\"", &format!("name = \"toy\"\n{extra}"), 1)
            .replace("BASE", &server.uri());
        let error = Declarative::from_toml(&text, support::client()?)
            .err()
            .ok_or(named)?;
        assert_eq!(error.kind(), ErrorKind::LocalInvalidConfig);
        assert!(error.to_string().contains(named), "{named}: {error}");
    }
    Ok(())
}

#[tokio::test]
async fn every_kind_of_paging_asks_for_each_page_once() -> Result<(), Failure> {
    let kinds = [
        ("{ kind = \"page\", param = \"page\", size = 2 }", 2),
        ("{ kind = \"page\", param = \"page\" }", 3),
        (
            "{ kind = \"cursor\", param = \"token\", field = \"$.more\" }",
            2,
        ),
        ("{ kind = \"link\" }", 2),
        ("{ kind = \"next\", field = \"$.next\" }", 2),
    ];
    for (kind, requests) in kinds {
        let server = MockServer::start().await;
        let second = format!("{}/records/7?page=2", server.uri());
        match kind {
            k if k.contains("\"page\"") => {
                mount(&server, Some(("page", "1")), page(&["a", "b"], &json!({}))).await;
                mount(&server, Some(("page", "2")), page(&["c"], &json!({}))).await;
                mount(&server, Some(("page", "3")), page(&[], &json!({}))).await;
            }
            k if k.contains("cursor") => {
                mount(
                    &server,
                    Some(("token", "t2")),
                    page(&["c"], &json!({"more": ""})),
                )
                .await;
                mount(&server, None, page(&["a", "b"], &json!({"more": "t2"}))).await;
            }
            k if k.contains("link") => {
                mount(&server, Some(("page", "2")), page(&["c"], &json!({}))).await;
                let first = page(&["a", "b"], &json!({}))
                    .insert_header("link", format!("<{second}>; rel=\"next\"").as_str());
                mount(&server, None, first).await;
            }
            _ => {
                mount(
                    &server,
                    Some(("page", "2")),
                    page(&["c"], &json!({"next": null})),
                )
                .await;
                mount(&server, None, page(&["a", "b"], &json!({"next": second}))).await;
            }
        }
        let adapter = toy(&server, &format!("paginate = {kind}\n"))?;
        assert_eq!(listed(&adapter, "toy:7").await?, ["a", "b", "c"], "{kind}");
        let requests_made = server.received_requests().await.unwrap_or_default().len();
        assert_eq!(requests_made, requests, "{kind}");
    }
    Ok(())
}

#[tokio::test]
async fn a_page_that_names_itself_again_is_refused() -> Result<(), Failure> {
    let server = MockServer::start().await;
    let again = format!("{}/records/7", server.uri());
    mount(&server, None, page(&["a"], &json!({"next": again}))).await;
    let adapter = toy(
        &server,
        "paginate = { kind = \"next\", field = \"$.next\" }\n",
    )?;
    let error = listed(&adapter, "toy:7").await.err().ok_or("listed")?;
    assert!(error.to_string().contains("already sent"), "{error}");
    Ok(())
}

#[tokio::test]
async fn a_form_with_a_version_pins_it_and_one_without_captures_it() -> Result<(), Failure> {
    let server = MockServer::start().await;
    for route in ["/records/7", "/records/7/v/3"] {
        Mock::given(method("GET"))
            .and(path(route))
            .respond_with(page(&["a b/c.csv"], &json!({"version": 3})))
            .mount(&server)
            .await;
    }
    let text = TOY
        .replace("record = \"BASE/records/{id}\"", "record = \"BASE/records/{id}\"\ncapture = { version = \"$.version\" }\nforms = [{ pattern = \"{id}@{version}\", record = \"BASE/records/{id}/v/{version}\" }]")
        .replace("resolved = \"toy:{$.id}\"", "resolved = \"toy:{id}@{version}\"")
        .replace("BASE/download/{id}/{%$.name}", "BASE/download/{id}/{%$.name}?v={version}")
        .replace("BASE", &server.uri());
    let client = support::client()?;
    let adapter = Declarative::from_toml(&text, Arc::clone(&client))?;
    client.trust(adapter.record_origin().ok_or("no record host")?);
    for written in ["toy:7", "toy:7@3"] {
        let listing = adapter.resolve(&written.parse()?).await?;
        assert_eq!(listing.resolved.as_str(), "toy:7@3", "{written}");
        assert_eq!(
            listing.entries[0].urls[0].to_string(),
            format!("{}/download/7/a%20b%2Fc.csv?v=***", server.uri())
        );
    }
    Ok(())
}

#[tokio::test]
async fn folders_are_walked_into_and_their_files_listed() -> Result<(), Failure> {
    let server = MockServer::start().await;
    let sub = format!("{}/records/7/sub", server.uri());
    Mock::given(method("GET"))
        .and(path("/records/7"))
        .respond_with(page(&["/top.csv"], &json!({"dirs": [{"href": sub}]})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/records/7/sub"))
        .respond_with(page(&["/d/inner.csv"], &json!({})))
        .mount(&server)
        .await;
    let adapter = toy(&server, "folders = \"$.dirs[*]\"\ndescend = \"$.href\"\n")?;
    assert_eq!(listed(&adapter, "toy:7").await?, ["top.csv", "d/inner.csv"]);
    Ok(())
}

#[tokio::test]
async fn digests_name_their_algorithm_and_urls_may_be_relative() -> Result<(), Failure> {
    let server = MockServer::start().await;
    let body = json!({"id": 7, "files": [
        {"name": "a", "size": "12", "kind": "SHA-256", "sum": "AB".repeat(32), "href": "../files/a"},
        {"name": "b", "kind": "crc", "sum": "1", "md5": "x", "href": "/files/b"},
        {"name": "c", "size": "", "kind": "md5", "sum": "", "href": "/files/c"},
    ]});
    Mock::given(method("GET"))
        .and(path("/records/7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;
    let text = TOY
        .replace(
            "url = \"BASE/download/{id}/{%$.name}\"",
            "url = \"$.href\"\ndigest = [\"{$.kind}:{$.sum}\", \"$.md5\"]",
        )
        .replace("BASE", &server.uri());
    let client = support::client()?;
    let adapter = Declarative::from_toml(&text, Arc::clone(&client))?;
    client.trust(adapter.record_origin().ok_or("no record host")?);
    let listing = adapter.resolve(&"toy:7".parse()?).await?;
    let [a, b, c] = &listing.entries[..] else {
        return Err("three files".into());
    };
    assert_eq!(a.size, Some(12));
    assert_eq!(
        a.stated[0].to_string(),
        format!("sha256:{}", "ab".repeat(32))
    );
    assert_eq!(a.urls[0].to_string(), format!("{}/files/a", server.uri()));
    assert!(b.stated.is_empty());
    assert_eq!(b.size, None);
    assert!(c.stated.is_empty());
    assert_eq!(c.size, None);
    Ok(())
}

#[tokio::test]
async fn a_landing_page_names_the_record_it_shows() -> Result<(), Failure> {
    let adapter = Declarative::from_toml(support::ADAPTER, support::client()?)?;
    assert_eq!(
        adapter
            .landing("https://zenodo.org/records/3608990")
            .map(|found| found.as_str().to_owned())
            .as_deref(),
        Some("zenodo:3608990")
    );
    assert!(
        adapter
            .landing("https://zenodo.org/communities/x")
            .is_none()
    );
    Ok(())
}

#[tokio::test]
async fn a_search_returns_references_and_titles() -> Result<(), Failure> {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/records"))
        .and(query_param("q", "iris data"))
        .and(query_param("size", "2"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"hits": {"hits": [
                {"id": 1, "metadata": {"title": "One"}},
                {"id": 2, "metadata": {}},
            ]}})),
        )
        .mount(&server)
        .await;
    let client = support::client()?;
    let adapter = support::declarative(&server, &client)?;
    let hits = adapter.search("iris data", 2).await?;
    let found: Vec<(&str, Option<&str>)> = hits
        .iter()
        .map(|hit| (hit.reference.as_str(), hit.title.as_deref()))
        .collect();
    assert_eq!(found, [("zenodo:1", Some("One")), ("zenodo:2", None)]);
    Ok(())
}
