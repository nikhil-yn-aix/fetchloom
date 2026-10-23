//! A client that never sleeps, a sink in memory, and the zenodo record served by a mock.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use fl_model::filter::Filters;
use fl_model::partial::PartialRecord;
use fl_sources::Declarative;
use fl_transfer::client::{Client, Config, Timer};
use fl_transfer::credentials::Chain;
use fl_transfer::sink::Sink;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

pub type Failure = Box<dyn std::error::Error>;

pub const RECORD: &str = include_str!("../../../fixtures/responses/zenodo/3608990/record.json");
pub const ADAPTER: &str = include_str!("../../../sources/zenodo.toml");

/// A clock fixed at one moment whose waits return at once.
struct Instant;

impl Timer for Instant {
    fn now(&self) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_hours(500_000)
    }

    fn sleep(&self, _duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(std::future::ready(()))
    }
}

pub fn client() -> Result<Arc<Client>, Failure> {
    client_trusting(Vec::new())
}

pub fn client_trusting(ca: Vec<u8>) -> Result<Arc<Client>, Failure> {
    let config = Config {
        connect: Duration::from_secs(5),
        response: Duration::from_secs(5),
        idle: Duration::from_secs(5),
        trust_only: if ca.is_empty() { Vec::new() } else { vec![ca] },
        ..Config::default()
    };
    Ok(Arc::new(Client::with_timer(
        config,
        Arc::new(Chain::new(Vec::new())),
        Arc::new(Instant),
        1,
        Vec::new(),
    )?))
}

/// Bytes held in memory.
#[derive(Debug, Default)]
pub struct Memory(Vec<u8>);

impl Sink for Memory {
    fn record(&self) -> Option<PartialRecord> {
        None
    }

    fn allocate(&mut self, _length: u64) -> io::Result<()> {
        Ok(())
    }

    fn write_at(&mut self, offset: u64, bytes: &[u8]) -> io::Result<()> {
        let start = usize::try_from(offset).map_err(io::Error::other)?;
        let end = start + bytes.len();
        if self.0.len() < end {
            self.0.resize(end, 0);
        }
        self.0[start..end].copy_from_slice(bytes);
        Ok(())
    }

    fn read_at(&mut self, offset: u64, buffer: &mut [u8]) -> io::Result<usize> {
        let start = usize::try_from(offset).map_err(io::Error::other)?;
        let held = self.0.get(start..).unwrap_or_default();
        let count = held.len().min(buffer.len());
        buffer[..count].copy_from_slice(&held[..count]);
        Ok(count)
    }

    fn reset(&mut self) -> io::Result<()> {
        self.0.clear();
        Ok(())
    }

    fn save(&mut self, _record: &PartialRecord) -> io::Result<()> {
        Ok(())
    }
}

pub fn adapter_text(server: &MockServer) -> String {
    ADAPTER.replace("https://zenodo.org", &server.uri())
}

pub fn declarative(server: &MockServer, client: &Arc<Client>) -> Result<Declarative, Failure> {
    let adapter = Declarative::from_toml(&adapter_text(server), Arc::clone(client))?;
    client.trust(
        adapter
            .record_origin()
            .ok_or("the adapter names no record host")?,
    );
    Ok(adapter)
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
