//! A sink in memory, and a client whose waits are recorded instead of slept.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime};

use fl_model::partial::PartialRecord;
use fl_transfer::client::{Client, Config, Timer};
use fl_transfer::credentials::{Chain, Credentials, Environment};
use fl_transfer::sink::Sink;

use super::Server;

/// Bytes and a record held in memory, with a count of every reset.
#[derive(Debug, Default)]
pub struct Memory {
    /// The bytes written.
    pub bytes: Vec<u8>,
    /// The record saved last.
    pub record: Option<PartialRecord>,
    /// How many times the download reset it.
    pub resets: usize,
    /// How many times it was read back.
    pub reads: usize,
    /// The length of every write, in order.
    pub writes: Vec<usize>,
}

impl Memory {
    /// A sink that already holds `bytes` under `record`, as an earlier attempt left it.
    pub fn kept(bytes: &[u8], record: PartialRecord) -> Self {
        Self {
            bytes: bytes.to_vec(),
            record: Some(record),
            resets: 0,
            reads: 0,
            writes: Vec::new(),
        }
    }
}

impl Sink for Memory {
    fn record(&self) -> Option<PartialRecord> {
        self.record.clone()
    }

    fn allocate(&mut self, _length: u64) -> io::Result<()> {
        Ok(())
    }

    fn write_at(&mut self, offset: u64, bytes: &[u8]) -> io::Result<()> {
        self.writes.push(bytes.len());
        let start = usize::try_from(offset).map_err(io::Error::other)?;
        let end = start + bytes.len();
        if self.bytes.len() < end {
            self.bytes.resize(end, 0);
        }
        self.bytes[start..end].copy_from_slice(bytes);
        Ok(())
    }

    fn read_at(&mut self, offset: u64, buffer: &mut [u8]) -> io::Result<usize> {
        self.reads += 1;
        let start = usize::try_from(offset).map_err(io::Error::other)?;
        let held = self.bytes.get(start..).unwrap_or_default();
        let count = held.len().min(buffer.len());
        buffer[..count].copy_from_slice(&held[..count]);
        Ok(count)
    }

    fn reset(&mut self) -> io::Result<()> {
        self.bytes.clear();
        self.record = None;
        self.resets += 1;
        Ok(())
    }

    fn save(&mut self, record: &PartialRecord) -> io::Result<()> {
        self.record = Some(record.clone());
        Ok(())
    }
}

/// A sink whose every write fails with a full disk.
#[derive(Debug)]
pub struct Broken;

impl Sink for Broken {
    fn record(&self) -> Option<PartialRecord> {
        None
    }

    fn allocate(&mut self, _length: u64) -> io::Result<()> {
        Ok(())
    }

    fn write_at(&mut self, _offset: u64, _bytes: &[u8]) -> io::Result<()> {
        Err(io::Error::other("the disk is full"))
    }

    fn read_at(&mut self, _offset: u64, _buffer: &mut [u8]) -> io::Result<usize> {
        Ok(0)
    }

    fn reset(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn save(&mut self, _record: &PartialRecord) -> io::Result<()> {
        Ok(())
    }
}

/// A clock fixed at one moment that records every wait and returns at once.
#[derive(Debug, Clone, Default)]
pub struct Waits(Arc<Mutex<Vec<Duration>>>);

impl Waits {
    /// Every wait asked for, in order.
    pub fn taken(&self) -> Vec<Duration> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl Timer for Waits {
    fn now(&self) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_hours(500_000)
    }

    fn sleep(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(duration);
        Box::pin(async {})
    }
}

/// Short timeouts, so a test of silence takes a fraction of a second.
pub fn quick() -> Config {
    Config {
        connect: Duration::from_millis(250),
        response: Duration::from_millis(250),
        idle: Duration::from_millis(250),
        ..Config::default()
    }
}

/// Timeouts long enough for a loaded machine, with `idle` as the silence a body may keep.
pub fn patient(idle: Duration) -> Config {
    Config {
        connect: Duration::from_secs(5),
        response: Duration::from_secs(5),
        idle,
        ..Config::default()
    }
}

/// Short timeouts that trust only the certificates of `servers`.
pub fn trusting(servers: &[&Server]) -> Config {
    Config {
        trust_only: servers
            .iter()
            .filter_map(|server| server.ca_der())
            .collect(),
        ..quick()
    }
}

/// Credentials that give every one of `servers` the token `s3cret`.
pub fn tokens(servers: &[&Server]) -> Environment {
    let names: Vec<String> = servers
        .iter()
        .map(|server| {
            let authority = server.authority().replace(['.', ':'], "_");
            format!("FETCHLOOM_TOKEN_{authority}")
        })
        .collect();
    Environment::new(move |name| {
        names
            .iter()
            .any(|known| known == name)
            .then(|| "s3cret".to_owned())
    })
}

/// A client with `config` and `credentials`, whose waits go to `waits`.
pub fn client_with(
    config: Config,
    credentials: impl Credentials + 'static,
    waits: &Waits,
) -> Result<Client, Box<dyn std::error::Error>> {
    Ok(Client::with_timer(
        config,
        Arc::new(credentials),
        Arc::new(waits.clone()),
        1,
        Vec::new(),
    )?)
}

/// A client with short timeouts and no credentials.
pub fn client(waits: &Waits) -> Result<Client, Box<dyn std::error::Error>> {
    client_with(quick(), Chain::new(Vec::new()), waits)
}
