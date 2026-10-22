//! The conformance suite: every adapter answers the same scenarios against its recorded
//! responses. Each adapter's `fixtures/responses/<adapter>/suite.toml` names the responses of
//! one small record and, when the adapter pages, one paged record, with what each must list.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fl_model::digest::Digest;
use fl_model::error::ErrorKind;
use fl_model::reference::Reference;
use fl_sources::{Croissant, Declarative, HuggingFace, Listing, Source};
use fl_transfer::client::Client;
use fl_transfer::download::Download;
use fl_transfer::url::SafeUrl;
use serde::Deserialize;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::support::{self, Failure, Memory};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Suite {
    hosts: Vec<String>,
    small: Case,
    paged: Option<Case>,
    unhashed: Option<Case>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    reference: String,
    resolved: String,
    files: usize,
    requests: u64,
    first: First,
    serve: Vec<Serve>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct First {
    path: String,
    size: Option<u64>,
    #[serde(default)]
    digests: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Serve {
    path: String,
    #[serde(default)]
    query: BTreeMap<String, String>,
    body: String,
    #[serde(default)]
    headers: BTreeMap<String, String>,
}

struct Adapter {
    name: String,
    dir: PathBuf,
    suite: Suite,
    text: Option<String>,
}

impl Adapter {
    fn load(name: &str) -> Result<Self, Failure> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let dir = root.join("fixtures/responses").join(name);
        let suite = toml::from_str(&fs::read_to_string(dir.join("suite.toml"))?)?;
        let text = fs::read_to_string(root.join("sources").join(format!("{name}.toml"))).ok();
        Ok(Self {
            name: name.to_owned(),
            dir,
            suite,
            text,
        })
    }

    fn local(&self, text: &str, server: &MockServer) -> String {
        let uri = server.uri();
        let mut text = text.replace("https://{host}", "http://{host}");
        for host in &self.suite.hosts {
            text = text
                .replace(&format!("https://{host}"), &uri)
                .replace(host.as_str(), uri.trim_start_matches("http://"));
        }
        text
    }

    fn reference(&self, case: &Case, server: &MockServer) -> Result<Reference, Failure> {
        Ok(self.local(&case.reference, server).parse()?)
    }

    fn source(
        &self,
        server: &MockServer,
        client: &Arc<Client>,
    ) -> Result<Box<dyn Source>, Failure> {
        let base = SafeUrl::parse(&server.uri())?;
        client.trust(&base.origin().ok_or("the mock has no origin")?);
        let client = Arc::clone(client);
        Ok(match (&self.text, self.name.as_str()) {
            (Some(text), _) => Box::new(Declarative::from_toml(&self.local(text, server), client)?),
            (None, "hf") => Box::new(HuggingFace::at(base, client)),
            (None, "croissant") => Box::new(Croissant::new(client)),
            (None, other) => return Err(format!("no adapter named {other}").into()),
        })
    }

    async fn serve(&self, case: &Case, server: &MockServer) -> Result<(), Failure> {
        for serve in &case.serve {
            let body = fs::read(self.dir.join(&serve.body))?;
            let body = match String::from_utf8(body) {
                Ok(text) => self.local(&text, server).into_bytes(),
                Err(raw) => raw.into_bytes(),
            };
            let mut answer = ResponseTemplate::new(200).set_body_bytes(body);
            for (name, value) in &serve.headers {
                answer = answer.insert_header(name.as_str(), self.local(value, server).as_str());
            }
            let mut mock = Mock::given(method("GET")).and(path(serve.path.as_str()));
            for (name, value) in &serve.query {
                mock = mock.and(query_param(name.as_str(), value.as_str()));
            }
            let priority = u8::try_from(5 - serve.query.len().min(4)).unwrap_or(5);
            mock.respond_with(answer)
                .with_priority(priority)
                .mount(server)
                .await;
        }
        Ok(())
    }

    async fn resolve(&self, case: &Case) -> Result<(Listing, Arc<Client>, MockServer), Failure> {
        let server = MockServer::start().await;
        self.serve(case, &server).await?;
        let client = support::client()?;
        let source = self.source(&server, &client)?;
        let listing = source.resolve(&self.reference(case, &server)?).await?;
        Ok((listing, client, server))
    }

    async fn lists(&self, case: &Case) -> Result<(), Failure> {
        let (listing, client, server) = self.resolve(case).await?;
        let name = &self.name;
        let resolved = self.local(&case.resolved, &server);
        assert_eq!(listing.resolved.as_str(), resolved, "{name}: resolved");
        assert_eq!(listing.entries.len(), case.files, "{name}: files");
        assert_eq!(
            client.counters().read().requests,
            case.requests,
            "{name}: requests"
        );
        let first = &listing.entries[0];
        assert_eq!(first.path.as_str(), case.first.path, "{name}: first path");
        assert_eq!(first.size, case.first.size, "{name}: first size");
        let digests: Vec<String> = first.stated.iter().map(Digest::to_string).collect();
        assert_eq!(digests, case.first.digests, "{name}: first digests");
        Ok(())
    }
}

/// Every scenario for the adapter `name`.
async fn conforms(name: &str) -> Result<(), Failure> {
    let adapter = Adapter::load(name)?;
    let suite = &adapter.suite;
    adapter.lists(&suite.small).await?;
    if let Some(paged) = &suite.paged {
        adapter.lists(paged).await?;
    }
    missing_is_unresolved(&adapter).await?;
    rate_limit_is_waited_out(&adapter).await?;
    redirect_to_another_host_is_followed(&adapter).await?;
    unhashed_files_download(&adapter).await?;
    wrong_hashes_are_refused(&adapter).await?;
    Ok(())
}

async fn missing_is_unresolved(adapter: &Adapter) -> Result<(), Failure> {
    let server = MockServer::start().await;
    let client = support::client()?;
    let source = adapter.source(&server, &client)?;
    let reference = adapter.reference(&adapter.suite.small, &server)?;
    let error = source.resolve(&reference).await.err().ok_or("resolved")?;
    assert_eq!(
        error.kind(),
        ErrorKind::ReferenceUnresolved,
        "{}: {error}",
        adapter.name
    );
    assert_eq!(client.counters().read().requests, 1, "{}", adapter.name);
    Ok(())
}

async fn rate_limit_is_waited_out(adapter: &Adapter) -> Result<(), Failure> {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "1"))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    let case = &adapter.suite.small;
    adapter.serve(case, &server).await?;
    let client = support::client()?;
    let source = adapter.source(&server, &client)?;
    let listing = source.resolve(&adapter.reference(case, &server)?).await?;
    assert_eq!(listing.entries.len(), case.files, "{}", adapter.name);
    let counted = client.counters().read();
    assert_eq!(
        (counted.requests, counted.retries),
        (case.requests + 1, 1),
        "{}",
        adapter.name
    );
    Ok(())
}

async fn redirect_to_another_host_is_followed(adapter: &Adapter) -> Result<(), Failure> {
    let front = MockServer::start().await;
    let back = MockServer::start().await;
    let case = &adapter.suite.small;
    adapter.serve(case, &back).await?;
    let client = support::client()?;
    let source = adapter.source(&front, &client)?;
    for serve in &case.serve {
        let pairs: Vec<String> = serve
            .query
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect();
        let query = if pairs.is_empty() {
            String::new()
        } else {
            format!("?{}", pairs.join("&"))
        };
        let target = format!("{}{}{query}", back.uri(), serve.path);
        let mut mock = Mock::given(method("GET")).and(path(serve.path.as_str()));
        for (name, value) in &serve.query {
            mock = mock.and(query_param(name.as_str(), value.as_str()));
        }
        let priority = u8::try_from(5 - serve.query.len().min(4)).unwrap_or(5);
        mock.respond_with(ResponseTemplate::new(302).insert_header("location", target.as_str()))
            .with_priority(priority)
            .mount(&front)
            .await;
    }
    client.trust(&SafeUrl::parse(&back.uri())?.origin().ok_or("origin")?);
    let listing = source.resolve(&adapter.reference(case, &front)?).await?;
    assert_eq!(listing.entries.len(), case.files, "{}", adapter.name);
    Ok(())
}

async fn unhashed_files_download(adapter: &Adapter) -> Result<(), Failure> {
    let case = adapter
        .suite
        .unhashed
        .as_ref()
        .unwrap_or(&adapter.suite.small);
    let (listing, client, server) = adapter.resolve(case).await?;
    let entry = listing
        .entries
        .iter()
        .find(|entry| entry.stated.is_empty())
        .ok_or_else(|| format!("{}: no file without a stated hash", adapter.name))?;
    let url = &entry.urls[0];
    Mock::given(method("GET"))
        .and(path(route(url, &server)))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"bytes".to_vec()))
        .with_priority(1)
        .mount(&server)
        .await;
    let done = client
        .download(
            &download(entry.urls[0].clone(), Vec::new()),
            Memory::default(),
        )
        .await?;
    assert_eq!(done.bytes, 5, "{}", adapter.name);
    Ok(())
}

async fn wrong_hashes_are_refused(adapter: &Adapter) -> Result<(), Failure> {
    let (listing, client, server) = adapter.resolve(&adapter.suite.small).await?;
    let Some(entry) = listing
        .entries
        .iter()
        .find(|entry| !entry.stated.is_empty())
    else {
        return Ok(());
    };
    let url = &entry.urls[0];
    Mock::given(method("GET"))
        .and(path(route(url, &server)))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"other bytes".to_vec()))
        .with_priority(1)
        .mount(&server)
        .await;
    let error = client
        .download(
            &download(url.clone(), entry.stated.clone()),
            Memory::default(),
        )
        .await
        .err()
        .ok_or("downloaded")?;
    assert_eq!(
        error.kind(),
        ErrorKind::IntegrityMismatch,
        "{}",
        adapter.name
    );
    Ok(())
}

fn route(url: &SafeUrl, server: &MockServer) -> String {
    let text = url.to_string();
    let rest = text.trim_start_matches(&server.uri());
    rest.split_once('?')
        .map_or(rest, |(route, _)| route)
        .to_owned()
}

fn download(url: SafeUrl, stated: Vec<Digest>) -> Download {
    Download {
        url,
        length: None,
        stable: !stated.is_empty(),
        stated,
        typed: false,
    }
}

#[tokio::test]
async fn zenodo() -> Result<(), Failure> {
    conforms("zenodo").await
}

#[tokio::test]
async fn hugging_face() -> Result<(), Failure> {
    conforms("hf").await
}

#[tokio::test]
async fn croissant() -> Result<(), Failure> {
    conforms("croissant").await
}

#[tokio::test]
async fn figshare() -> Result<(), Failure> {
    conforms("figshare").await
}

#[tokio::test]
async fn dataverse() -> Result<(), Failure> {
    conforms("dataverse").await
}

#[tokio::test]
async fn ckan() -> Result<(), Failure> {
    conforms("ckan").await
}

#[tokio::test]
async fn inveniordm() -> Result<(), Failure> {
    conforms("inveniordm").await
}
