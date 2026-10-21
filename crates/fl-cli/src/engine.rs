//! The order of work every command shares: resolve through the sources, fetch through the
//! transfer engine into a store partial, ingest, and link into a data directory, counting all
//! of it.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use fl_model::digest::Digest;
use fl_model::document::Work;
use fl_model::error::ErrorKind;
use fl_model::filter::Filters;
use fl_model::partial::PartialRecord;
use fl_model::path::DataPath;
use fl_model::reference::Reference;
use fl_sources::{Declarative, Listing, Registry, Source};
use fl_store::data_dir::{DataDir, Destination};
use fl_store::link::{Ladder, Linked, Linker};
use fl_store::partial::Partial;
use fl_store::tier::{Ingested, Sharing, Tier};
use fl_transfer::client::Client;
use fl_transfer::credentials::{Chain, CredentialsFile, Environment};
use fl_transfer::download::Download;
use fl_transfer::schedule::{Queued, Scheduler};
use fl_transfer::sink::Sink;
use fl_transfer::url::SafeUrl;
use tracing::Instrument;

use crate::config::Config;
use crate::failure::Failure;

/// The tier name work counters report hits under.
const LOCAL: &str = "local";

/// One file wanted in the store.
#[derive(Clone, Debug)]
pub(crate) struct Want {
    /// Where its bytes are.
    pub(crate) url: SafeUrl,
    /// Its length, when known.
    pub(crate) size: Option<u64>,
    /// The digests the source states.
    pub(crate) stated: Vec<Digest>,
    /// Its BLAKE3 digest, when a lock already pins it.
    pub(crate) blake3: Option<[u8; 32]>,
    /// The user wrote its URL on this command line, so its host may be private.
    pub(crate) typed: bool,
}

/// One file the store now holds.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Got {
    /// Its BLAKE3 digest, which names its object.
    pub(crate) blake3: [u8; 32],
    /// Its length.
    pub(crate) size: u64,
    /// Bytes received over the network for it.
    pub(crate) fetched: u64,
}

/// The parts every command works through, and what they did.
pub(crate) struct Engine {
    client: Arc<Client>,
    registry: Registry,
    tier: Arc<Tier>,
    jobs: usize,
    work: Mutex<Work>,
    phases: Mutex<BTreeMap<&'static str, Duration>>,
}

impl Engine {
    /// The client, the sources including the user's own adapters, and the local tier.
    pub(crate) fn new(config: &Config) -> Result<Self, Failure> {
        let mut places: Vec<Box<dyn fl_transfer::credentials::Credentials>> =
            vec![Box::new(Environment::new(|name| std::env::var(name).ok()))];
        if let Some(dir) = &config.settings {
            places.push(Box::new(CredentialsFile::new(dir.join("credentials.toml"))));
        }
        let client = Arc::new(Client::new(
            config.transfer.clone(),
            Arc::new(Chain::new(places)),
        )?);
        let mut registry = Registry::builtin(Arc::clone(&client))?;
        if let Some(dir) = &config.settings {
            for source in user_adapters(&dir.join("sources"), &client)? {
                if let Some(host) = source.record_host() {
                    client.trust(host);
                }
                registry.add(Arc::new(source));
            }
        }
        let tier = Arc::new(Tier::open(&config.store, Sharing::Private)?);
        Ok(Self {
            client,
            registry,
            tier,
            jobs: config.transfer.jobs,
            work: Mutex::new(Work::default()),
            phases: Mutex::new(BTreeMap::new()),
        })
    }

    /// Resolves `reference` and keeps what `filters` select.
    pub(crate) async fn resolve(
        &self,
        reference: &Reference,
        filters: &Filters,
    ) -> Result<Listing, Failure> {
        let started = Instant::now();
        let listing = self.registry.resolve(reference, filters).await;
        self.spent("resolve", started);
        Ok(listing?)
    }

    /// Makes the store hold every wanted file: a file whose object is already there costs
    /// nothing, the rest are downloaded, verified and ingested. Results are in `wants` order.
    pub(crate) async fn fetch(&self, group: &str, wants: Vec<Want>) -> Vec<Result<Got, Failure>> {
        let started = Instant::now();
        let mut results: Vec<Option<Result<Got, Failure>>> = Vec::with_capacity(wants.len());
        let mut queue = Vec::new();
        for (index, want) in wants.into_iter().enumerate() {
            let held = want.blake3.and_then(|blake3| {
                let located = self.tier.locate(&Digest::Blake3(blake3))?;
                let size = fs::metadata(&located.path).ok()?.len();
                Some(Got {
                    blake3,
                    size,
                    fetched: 0,
                })
            });
            if let Some(got) = held {
                self.count(|work| *work.tier_hits.entry(LOCAL.to_owned()).or_default() += 1);
                results.push(Some(Ok(got)));
                continue;
            }
            results.push(None);
            queue.push(Queued {
                group: group.to_owned(),
                host: want.url.host().unwrap_or_default().to_owned(),
                job: (index, want),
            });
        }
        let client = Arc::clone(&self.client);
        let tier = Arc::clone(&self.tier);
        let span = tracing::info_span!("transfer", group, files = queue.len());
        let done = Scheduler::new(self.jobs, self.client.hosts())
            .run(queue, move |(index, want): (usize, Want)| {
                let client = Arc::clone(&client);
                let tier = Arc::clone(&tier);
                async move { (index, download(&client, &tier, &want).await) }
            })
            .instrument(span)
            .await;
        for (index, result) in done {
            if let Ok((_, ingested)) = &result {
                self.count(|work| ingested.add_to(work));
            }
            results[index] = Some(result.map(|(got, _)| got));
        }
        self.spent("transfer", started);
        results
            .into_iter()
            .map(|result| {
                result
                    .unwrap_or_else(|| Err(Failure::new(ErrorKind::LocalIo, "a download vanished")))
            })
            .collect()
    }

    /// Places `files` of the dataset at `dir` inside the data directory at `root`.
    pub(crate) fn link(
        &self,
        root: &Path,
        dir: &DataPath,
        files: &[(DataPath, [u8; 32])],
    ) -> Result<Vec<(Destination, Linked)>, Failure> {
        let started = Instant::now();
        let span = tracing::info_span!("link", dir = dir.as_str(), files = files.len());
        let _entered = span.enter();
        let data = DataDir::new(root.to_owned());
        let mut pairs = Vec::with_capacity(files.len());
        for (path, blake3) in files {
            pairs.push((Digest::Blake3(*blake3), data.destination(dir, path)?));
        }
        let results = Linker::new(&self.tier, Ladder::AUTO).link_all(&pairs);
        let mut placed = Vec::with_capacity(pairs.len());
        for ((_, destination), result) in pairs.iter().zip(results) {
            let mode = result?;
            self.count(|work| mode.add_to(work));
            placed.push((destination.clone(), mode));
        }
        self.spent("link", started);
        Ok(placed)
    }

    /// Registers the lock at `lock` as a root of the local tier, so gc keeps what it needs.
    pub(crate) fn register(&self, lock: &Path) -> Result<(), Failure> {
        Ok(self.tier.register_root(lock)?)
    }

    /// Counts a file read again to check its hash.
    pub(crate) fn rehashed(&self) {
        self.count(|work| work.files_rehashed += 1);
    }

    /// Everything counted so far, network included.
    pub(crate) fn work(&self) -> Work {
        let mut work = self
            .work
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        self.client.counters().read().add_to(&mut work);
        for (phase, spent) in self
            .phases
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
        {
            work.phase_ms.insert(
                (*phase).to_owned(),
                u64::try_from(spent.as_millis()).unwrap_or(u64::MAX),
            );
        }
        work
    }

    fn count(&self, add: impl FnOnce(&mut Work)) {
        add(&mut self.work.lock().unwrap_or_else(PoisonError::into_inner));
    }

    fn spent(&self, phase: &'static str, started: Instant) {
        *self
            .phases
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(phase)
            .or_default() += started.elapsed();
    }
}

async fn download(
    client: &Client,
    tier: &Arc<Tier>,
    want: &Want,
) -> Result<(Got, Ingested), Failure> {
    let key = Digest::Blake3(*blake3::hash(want.url.to_string().as_bytes()).as_bytes());
    let opened = Arc::clone(tier);
    let partial = blocking(move || {
        opened
            .partial(&key, SystemTime::now())
            .map_err(Failure::from)
    })
    .await?;
    let mut stated = want.stated.clone();
    if let Some(blake3) = want.blake3 {
        stated.push(Digest::Blake3(blake3));
    }
    let download = Download {
        url: want.url.clone(),
        length: want.size,
        stable: !stated.is_empty(),
        stated,
        typed: want.typed,
    };
    let done = client.download(&download, Landing(partial)).await?;
    let (blake3, size) = (done.hashes.blake3, done.bytes);
    let placing = Arc::clone(tier);
    let partial = done.sink.0;
    let ingested = blocking(move || {
        placing
            .ingest_partial(partial, blake3, size)
            .map_err(Failure::from)
    })
    .await?;
    Ok((
        Got {
            blake3,
            size,
            fetched: size,
        },
        ingested,
    ))
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, Failure> + Send + 'static,
) -> Result<T, Failure> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|error| Failure::new(ErrorKind::LocalIo, error.to_string()))?
}

fn user_adapters(dir: &Path, client: &Arc<Client>) -> Result<Vec<Declarative>, Failure> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(Failure::io("list", dir, &error)),
    };
    let mut paths: Vec<_> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "toml"))
        .collect();
    paths.sort();
    paths
        .iter()
        .map(|path| {
            let text =
                fs::read_to_string(path).map_err(|error| Failure::io("read", path, &error))?;
            let adapter = Declarative::from_toml(&text, Arc::clone(client)).map_err(|error| {
                Failure::new(error.kind(), format!("`{}`: {error}", path.display()))
                    .subject(path.display().to_string())
            })?;
            if adapter
                .schemes()
                .iter()
                .any(|scheme| matches!(scheme.as_str(), "http" | "https"))
            {
                return Err(Failure::new(
                    ErrorKind::LocalInvalidConfig,
                    format!(
                        "`{}` claims http or https, which only fetchloom serves",
                        path.display()
                    ),
                )
                .subject(path.display().to_string())
                .help("give the adapter a scheme of its own"));
            }
            Ok(adapter)
        })
        .collect()
}

/// A store partial as the transfer engine writes into it.
struct Landing(Partial);

impl Sink for Landing {
    fn record(&self) -> Option<PartialRecord> {
        self.0.record().cloned()
    }

    fn allocate(&mut self, length: u64) -> io::Result<()> {
        self.0.allocate(length)
    }

    fn write_at(&mut self, offset: u64, bytes: &[u8]) -> io::Result<()> {
        self.0.write_at(offset, bytes)
    }

    fn read_at(&mut self, offset: u64, buffer: &mut [u8]) -> io::Result<usize> {
        self.0.read_at(offset, buffer)
    }

    fn reset(&mut self) -> io::Result<()> {
        self.0.reset()
    }

    fn save(&mut self, record: &PartialRecord) -> io::Result<()> {
        self.0.save(record)
    }
}
