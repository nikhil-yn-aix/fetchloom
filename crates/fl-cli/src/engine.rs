//! The order of work every command shares: resolve through the sources, fetch through the
//! transfer engine into a store partial, ingest, and link into a data directory, counting all
//! of it.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use fl_model::digest::Digest;
use fl_model::document::Work;
use fl_model::error::ErrorKind;
use fl_model::event::Event;
use fl_model::filter::Filters;
use fl_model::partial::PartialRecord;
use fl_model::path::DataPath;
use fl_model::reference::{Reference, Target};
use fl_sources::{Declarative, Files, HuggingFace, Listing, Registry, Source};
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
use crate::report::Reporter;

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
    events: Arc<dyn Reporter>,
    transfers: Arc<AtomicU64>,
}

impl Engine {
    /// The client, the sources including the user's own adapters, and the local tier.
    /// Relative local paths in references are read against `base`, the project root.
    pub(crate) fn new(
        config: &Config,
        base: &Path,
        events: Arc<dyn Reporter>,
    ) -> Result<Self, Failure> {
        let mut places: Vec<Box<dyn fl_transfer::credentials::Credentials>> =
            vec![Box::new(Environment::new(|name| std::env::var(name).ok()))];
        if let Some(dir) = &config.settings {
            places.push(Box::new(CredentialsFile::new(dir.join("credentials.toml"))));
        }
        let chain = Arc::new(Chain::new(places));
        let client = Arc::new(Client::new(config.transfer(), chain.clone())?);
        let mut registry = Registry::builtin(&client)?;
        registry.add(Arc::new(Files::new(base.to_path_buf())));
        if let Some(endpoint) = &config.hub {
            if let Some(origin) = endpoint.origin() {
                client.trust(&origin);
            }
            registry.add(Arc::new(HuggingFace::at(endpoint, Arc::clone(&client))));
        }
        if let Some(dir) = &config.settings {
            for source in user_adapters(&dir.join("sources"), &client)? {
                if let Some(origin) = source.record_origin() {
                    client.trust(origin);
                }
                registry.add(Arc::new(source));
            }
        }
        for credential in registry.credentials(|name| std::env::var(name).ok()) {
            chain.add(Box::new(credential));
        }
        let tier = Arc::new(Tier::open(&config.store.value, Sharing::Private)?);
        Ok(Self {
            client,
            registry,
            tier,
            jobs: config.jobs.value,
            work: Mutex::new(Work::default()),
            phases: Mutex::new(BTreeMap::new()),
            events,
            transfers: Arc::new(AtomicU64::new(0)),
        })
    }

    /// Resolves `reference` and keeps what `filters` select.
    pub(crate) async fn resolve(
        &self,
        reference: &Reference,
        filters: &Filters,
    ) -> Result<Listing, Failure> {
        let started = Instant::now();
        self.events.emit(Event::Waiting(waiting_for(reference)));
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
            let known = want.blake3.or_else(|| {
                want.size
                    .and_then(|size| self.tier.recall(&want.stated, Some(size)))
            });
            let held = known.and_then(|blake3| {
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
        let events = Arc::clone(&self.events);
        let ids = Arc::clone(&self.transfers);
        let span = tracing::info_span!("transfer", group, files = queue.len());
        let done = Scheduler::new(self.jobs, self.client.hosts())
            .run(queue, move |(index, want): (usize, Want)| {
                let client = Arc::clone(&client);
                let tier = Arc::clone(&tier);
                let shown = Shown {
                    id: ids.fetch_add(1, Ordering::Relaxed),
                    events: Arc::clone(&events),
                };
                async move { (index, download(&client, &tier, &want, shown).await) }
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

    fn holds(&self, blake3: &[u8; 32]) -> bool {
        self.tier.locate(&Digest::Blake3(*blake3)).is_some()
    }

    /// Whether the store holds the object `blake3`, counted as a hit in its tier when it does.
    pub(crate) fn reuse(&self, blake3: &[u8; 32]) -> bool {
        let held = self.holds(blake3);
        if held {
            self.count(|work| *work.tier_hits.entry(LOCAL.to_owned()).or_default() += 1);
        }
        held
    }

    /// The bytes fetching `wants` would download: the known sizes of those the store does not
    /// hold yet.
    pub(crate) fn to_download(&self, wants: &[Want]) -> u64 {
        wants
            .iter()
            .filter(|want| {
                let known = want.blake3.or_else(|| {
                    want.size
                        .and_then(|size| self.tier.recall(&want.stated, Some(size)))
                });
                !known.is_some_and(|blake3| self.holds(&blake3))
            })
            .filter_map(|want| want.size)
            .sum()
    }

    /// Places `files` of the dataset at `dir` inside the data directory at `root`, trying the
    /// modes of `ladder`.
    pub(crate) fn link(
        &self,
        root: &Path,
        dir: &DataPath,
        files: &[(DataPath, [u8; 32])],
        ladder: Ladder,
    ) -> Result<Vec<(Destination, Linked)>, Failure> {
        let started = Instant::now();
        let span = tracing::info_span!("link", dir = dir.as_str(), files = files.len());
        let _entered = span.enter();
        let data = DataDir::new(root.to_owned());
        let mut pairs = Vec::with_capacity(files.len());
        for (path, blake3) in files {
            pairs.push((Digest::Blake3(*blake3), data.destination(dir, path)?));
        }
        let results = Linker::new(&self.tier, ladder).link_all(&pairs);
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
    shown: Shown,
) -> Result<(Got, Ingested), Failure> {
    if let Some(path) = want.url.to_file_path() {
        let copying = Arc::clone(tier);
        let ingested = blocking(move || copying.ingest_file(&path).map_err(Failure::from)).await?;
        let Digest::Blake3(blake3) = ingested.digest else {
            return Err(Failure::new(
                ErrorKind::LocalIo,
                "an object is named by BLAKE3",
            ));
        };
        if want.blake3.is_some_and(|pinned| pinned != blake3) {
            return Err(Failure::new(
                ErrorKind::IntegrityDrift,
                format!("{} now holds other bytes than data.lock records", want.url),
            ));
        }
        let got = Got {
            blake3,
            size: ingested.bytes,
            fetched: 0,
        };
        return Ok((got, ingested));
    }
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
    shown.events.emit(Event::TransferStarted {
        id: shown.id,
        host: want.url.host().unwrap_or_default().to_owned(),
        bytes: want.size,
    });
    let began = Instant::now();
    let landing = Landing {
        partial,
        shown,
        received: 0,
    };
    let done = client.download(&download, landing).await;
    let done = match done {
        Ok(done) => done,
        Err(error) => return Err(error.into()),
    };
    done.sink.shown.events.emit(Event::TransferFinished {
        id: done.sink.shown.id,
        bytes: done.bytes,
        elapsed: began.elapsed(),
    });
    let (blake3, size) = (done.hashes.blake3, done.bytes);
    let placing = Arc::clone(tier);
    let partial = done.sink.partial;
    let stated = want.stated.clone();
    let ingested = blocking(move || {
        let ingested = placing
            .ingest_partial(partial, blake3, size)
            .map_err(Failure::from)?;
        if let Err(error) = placing.remember(&stated, blake3) {
            tracing::warn!("the stated digests of an object were not remembered: {error}");
        }
        Ok(ingested)
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

/// What a resolve waits on, naming the host when the reference names one.
fn waiting_for(reference: &Reference) -> String {
    match reference.target() {
        Target::Url { .. } => match SafeUrl::parse(reference.as_str())
            .ok()
            .and_then(|url| url.host().map(str::to_owned))
        {
            Some(host) => format!("Resolving {reference} at {host}"),
            None => format!("Resolving {reference}"),
        },
        Target::Doi { .. } => format!("Resolving {reference} at doi.org"),
        _ => format!("Resolving {reference}"),
    }
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

/// Which transfer this is and where its progress goes.
struct Shown {
    id: u64,
    events: Arc<dyn Reporter>,
}

/// A store partial as the transfer engine writes into it, reporting what it received.
struct Landing {
    partial: Partial,
    shown: Shown,
    received: u64,
}

impl Sink for Landing {
    fn record(&self) -> Option<PartialRecord> {
        self.partial.record().cloned()
    }

    fn allocate(&mut self, length: u64) -> io::Result<()> {
        self.partial.allocate(length)
    }

    fn write_at(&mut self, offset: u64, bytes: &[u8]) -> io::Result<()> {
        self.partial.write_at(offset, bytes)?;
        self.received += bytes.len() as u64;
        self.shown.events.emit(Event::TransferAdvanced {
            id: self.shown.id,
            bytes: self.received,
        });
        Ok(())
    }

    fn read_at(&mut self, offset: u64, buffer: &mut [u8]) -> io::Result<usize> {
        self.partial.read_at(offset, buffer)
    }

    fn reset(&mut self) -> io::Result<()> {
        self.received = 0;
        self.partial.reset()
    }

    fn save(&mut self, record: &PartialRecord) -> io::Result<()> {
        self.partial.save(record)
    }
}
