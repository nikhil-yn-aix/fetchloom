//! Hugging Face: gated repositories, model locations and trees spread over pages.

use std::sync::Arc;

use fl_model::error::ErrorKind;
use fl_sources::{HuggingFace, Source};
use fl_transfer::credentials::{Credential, Credentials};
use fl_transfer::url::SafeUrl;
use serde_json::json;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::support::{self, Failure};

const GPQA: &str = include_str!("../../../fixtures/responses/hf/gpqa/revision.json");
const GPT2: &str = include_str!("../../../fixtures/responses/hf/gpt2/revision.json");
const GPT2_TREE: &str = include_str!("../../../fixtures/responses/hf/gpt2/tree.json");
const GPT2_SHA: &str = "607a30d783dfa663caf39e06633721c8d4cfcd7e";

fn hub(server: &MockServer) -> Result<HuggingFace, Failure> {
    hub_at(server, "")
}

fn hub_at(server: &MockServer, prefix: &str) -> Result<HuggingFace, Failure> {
    let base = SafeUrl::parse(&format!("{}{prefix}", server.uri()))?;
    let client = support::client()?;
    client.trust(&base.origin().ok_or("no origin")?);
    Ok(HuggingFace::at(&base, client))
}

async fn serve(server: &MockServer, route: &str, body: &str) {
    Mock::given(method("GET"))
        .and(path(route))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .mount(server)
        .await;
}

#[tokio::test]
async fn a_gated_repository_on_a_mirror_names_the_mirror_page_to_accept() -> Result<(), Failure> {
    let server = MockServer::start().await;
    serve(
        &server,
        "/hub/api/datasets/Idavidrein/gpqa/revision/main",
        GPQA,
    )
    .await;
    let error = hub_at(&server, "/hub")?
        .resolve(&"hf:datasets/Idavidrein/gpqa".parse()?)
        .await
        .err()
        .ok_or("resolved")?;
    assert_eq!(error.kind(), ErrorKind::TermsRequired);
    assert!(
        error
            .to_string()
            .contains(&format!("{}/hub/datasets/Idavidrein/gpqa", server.uri())),
        "{error}"
    );
    assert_eq!(
        server.received_requests().await.unwrap_or_default().len(),
        1
    );
    Ok(())
}

#[tokio::test]
async fn a_model_lists_files_resolved_at_its_commit() -> Result<(), Failure> {
    let server = MockServer::start().await;
    serve(
        &server,
        "/api/models/openai-community/gpt2/revision/main",
        GPT2,
    )
    .await;
    serve(
        &server,
        &format!("/api/models/openai-community/gpt2/tree/{GPT2_SHA}"),
        GPT2_TREE,
    )
    .await;
    let listing = hub(&server)?
        .resolve(&"hf:openai-community/gpt2".parse()?)
        .await?;
    assert_eq!(
        listing.resolved.as_str(),
        format!("hf:models/openai-community/gpt2@{GPT2_SHA}")
    );
    assert_eq!(listing.meta.license.as_deref(), Some("MIT"));
    assert_eq!(listing.entries.len(), 26);
    let readme = listing
        .entries
        .iter()
        .find(|entry| entry.path.as_str() == "README.md")
        .ok_or("no README.md")?;
    assert_eq!(
        readme.urls[0].to_string(),
        format!(
            "{}/openai-community/gpt2/resolve/{GPT2_SHA}/README.md",
            server.uri()
        )
    );
    Ok(())
}

#[tokio::test]
async fn a_tree_over_several_pages_follows_its_links_once_each() -> Result<(), Failure> {
    let server = MockServer::start().await;
    let sha = "a".repeat(40);
    let revision = json!({"sha": sha, "gated": false});
    serve(
        &server,
        "/api/datasets/o/n/revision/v1",
        &revision.to_string(),
    )
    .await;
    let tree = format!("/api/datasets/o/n/tree/{sha}");
    let next = format!("{}{tree}?recursive=true&cursor=2", server.uri());
    let file = |name: &str| json!([{"type": "file", "oid": "0", "size": 1, "path": name}]);
    Mock::given(method("GET"))
        .and(path(tree.as_str()))
        .and(query_param("cursor", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(file("b")))
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(tree.as_str()))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(file("a"))
                .insert_header("link", format!("<{next}>; rel=\"next\"").as_str()),
        )
        .mount(&server)
        .await;
    let source = Arc::new(hub(&server)?);
    let listing = source.resolve(&"hf:datasets/o/n@v1".parse()?).await?;
    let paths: Vec<&str> = listing
        .entries
        .iter()
        .map(|entry| entry.path.as_str())
        .collect();
    assert_eq!(paths, ["a", "b"]);
    assert_eq!(
        server.received_requests().await.unwrap_or_default().len(),
        3
    );
    Ok(())
}

#[tokio::test]
async fn a_revision_whose_sha_is_not_a_commit_is_refused() -> Result<(), Failure> {
    let server = MockServer::start().await;
    for (revision, sha) in [("short", "a".repeat(39)), ("letters", "g".repeat(40))] {
        serve(
            &server,
            &format!("/api/models/o/n/revision/{revision}"),
            &json!({"sha": sha}).to_string(),
        )
        .await;
        let error = hub(&server)?
            .resolve(&format!("hf:o/n@{revision}").parse()?)
            .await
            .err()
            .ok_or("resolved")?;
        assert_eq!(error.kind(), ErrorKind::NetworkProtocol, "{revision}");
    }
    Ok(())
}

async fn license_of(card: serde_json::Value, tags: &[&str]) -> Result<Option<String>, Failure> {
    let server = MockServer::start().await;
    let sha = "b".repeat(40);
    serve(
        &server,
        "/api/models/o/n/revision/main",
        &json!({"sha": sha, "cardData": card, "tags": tags}).to_string(),
    )
    .await;
    serve(&server, &format!("/api/models/o/n/tree/{sha}"), "[]").await;
    let listing = hub(&server)?.resolve(&"hf:o/n".parse()?).await?;
    Ok(listing.meta.license)
}

#[tokio::test]
async fn the_license_is_the_cards_string_or_list_before_the_tags() -> Result<(), Failure> {
    let tags = ["license:mit"];
    assert_eq!(
        license_of(json!({"license": "apache-2.0"}), &tags)
            .await?
            .as_deref(),
        Some("Apache-2.0")
    );
    assert_eq!(
        license_of(json!({"license": ["bsd-3-clause", "isc"]}), &tags)
            .await?
            .as_deref(),
        Some("BSD-3-Clause")
    );
    assert_eq!(
        license_of(json!({"license": 5}), &tags).await?.as_deref(),
        Some("MIT")
    );
    assert_eq!(license_of(json!({}), &[]).await?, None);
    Ok(())
}

#[tokio::test]
async fn the_hub_serves_hf_and_names_its_host_and_landing_pages() -> Result<(), Failure> {
    let source = HuggingFace::new(support::client()?);
    assert_eq!(source.schemes(), ["hf"]);
    assert_eq!(source.host().as_deref(), Some("huggingface.co"));
    let landing = |url: &str| source.landing(url).map(|found| found.as_str().to_owned());
    assert_eq!(
        landing("https://huggingface.co/datasets/org/name").as_deref(),
        Some("hf:datasets/org/name")
    );
    assert_eq!(
        landing("https://huggingface.co/org/name/?tab=card#x").as_deref(),
        Some("hf:models/org/name")
    );
    assert_eq!(landing("https://example.org/datasets/org/name"), None);
    assert_eq!(landing("https://huggingface.co/a/b/c"), None);
    Ok(())
}

#[tokio::test]
async fn the_hub_declares_a_token_for_its_own_host() -> Result<(), Failure> {
    let source = HuggingFace::new(support::client()?);
    let variable = source
        .credential(Arc::new(|_: &str| Some("secret".to_owned())))
        .ok_or("no credential")?;
    let origin = SafeUrl::parse("https://huggingface.co/")?
        .origin()
        .ok_or("no origin")?;
    assert_eq!(variable.find(&origin)?, Some(Credential::token("secret")));
    Ok(())
}
