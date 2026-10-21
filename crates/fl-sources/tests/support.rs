//! A mock record server and a client for it.

use std::sync::Arc;

use fl_model::filter::Filters;
use fl_sources::Declarative;
use fl_transfer::client::{Client, Config};
use fl_transfer::credentials::Chain;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

pub type Failure = Box<dyn std::error::Error>;

pub const RECORD: &str = include_str!("../../../fixtures/responses/zenodo/3608990/record.json");
pub const ADAPTER: &str = include_str!("../../../sources/zenodo.toml");

pub fn client() -> Result<Arc<Client>, Failure> {
    Ok(Arc::new(Client::new(
        Config::default(),
        Arc::new(Chain::new(Vec::new())),
    )?))
}

pub fn adapter_text(server: &MockServer) -> String {
    ADAPTER.replace("https://zenodo.org", &server.uri())
}

pub fn declarative(server: &MockServer, client: &Arc<Client>) -> Result<Declarative, Failure> {
    Ok(Declarative::from_toml(
        &adapter_text(server),
        Arc::clone(client),
    )?)
}

pub fn no_filters() -> Result<Filters, Failure> {
    Ok(Filters::new(Vec::new(), Vec::new(), None, None)?)
}

pub async fn serve(server: &MockServer, route: &str, record: &str) {
    let body = record.replace("https://zenodo.org", &server.uri());
    Mock::given(method("GET"))
        .and(path(route))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .mount(server)
        .await;
}

pub async fn answer(server: &MockServer, route: &str, response: ResponseTemplate) {
    Mock::given(method("GET"))
        .and(path(route))
        .respond_with(response)
        .mount(server)
        .await;
}
