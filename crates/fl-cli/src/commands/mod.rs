//! The commands, and the two pieces most of them share: bringing the lock in line with
//! `data.toml`, and making the data directory match the lock.

pub(crate) mod add;
pub(crate) mod get;
pub(crate) mod init;
pub(crate) mod path;
pub(crate) mod status;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::Instant;

use fl_model::digest::Digest;
use fl_model::error::ErrorKind;
use fl_model::event::Event;
use fl_model::filter::Filters;
use fl_model::lock::{Lock, LockedDataset, LockedFile};
use fl_model::manifest::{DatasetSpec, Manifest};
use fl_model::name::Name;
use fl_model::path::DataPath;
use fl_model::tree::{Tree, TreeEntry};
use fl_sources::Entry;
use fl_store::fingerprint::{self, Fingerprint};
use fl_transfer::url::SafeUrl;
use serde_json::json;

use crate::clock;
use crate::engine::{Engine, Got, Want};
use crate::failure::Failure;
use crate::project::{Placed, Project, State};
use crate::report::{Outcome, Reporter};

/// The parts a command works with.
pub(crate) struct Context<'a> {
    /// Resolving, fetching and linking.
    pub(crate) engine: &'a Engine,
    /// Where events go.
    pub(crate) reporter: &'a dyn Reporter,
    /// Where the command runs.
    pub(crate) cwd: &'a Path,
}

impl Context<'_> {
    fn emit(&self, event: Event) {
        self.reporter.emit(event);
    }

    /// Takes the project lock, saying so when another `fl` holds it.
    pub(crate) fn hold(&self, project: &Project) -> Result<crate::project::Held, Failure> {
        project.hold(|| {
            self.emit(Event::Warning(
                "another fl is changing this project, waiting for it".to_owned(),
            ));
        })
    }
}

/// `fl lock`.
pub(crate) async fn lock(context: &Context<'_>) -> Result<Outcome, Failure> {
    let project = Project::find(context.cwd)?;
    let _held = context.hold(&project)?;
    let manifest = project.manifest()?;
    let (lock, written) = relock(context, &project, &manifest).await?;
    let files = lock_files(&lock);
    Ok(Outcome {
        status: fl_model::document::Status::Ok,
        result: json!({ "written": written, "datasets": lock.datasets().len(), "files": files }),
        answer: None,
        sentence: if written {
            format!("Locked {} in data.lock.", plural(files, "file", "files"))
        } else {
            "data.lock already matches data.toml, nothing to do.".to_owned()
        },
    })
}

/// `fl sync`.
pub(crate) async fn sync(context: &Context<'_>) -> Result<Outcome, Failure> {
    let project = Project::find(context.cwd)?;
    let _held = context.hold(&project)?;
    let synced = sync_project(context, &project, None).await?;
    Ok(synced.outcome())
}

/// Brings `data.lock` in line with `manifest`, resolving only the entries whose spec changed.
/// Returns the lock and whether it was written.
pub(crate) async fn relock(
    context: &Context<'_>,
    project: &Project,
    manifest: &Manifest,
) -> Result<(Lock, bool), Failure> {
    let old = project.lock()?;
    let mut datasets = Vec::with_capacity(manifest.datasets.len());
    let mut failures = Vec::new();
    for (name, spec) in &manifest.datasets {
        let before = old
            .as_ref()
            .and_then(|lock| lock.datasets().iter().find(|locked| locked.name == *name));
        if let Some(kept) = before.filter(|locked| locked.spec == spec.spec()) {
            datasets.push(kept.clone());
            continue;
        }
        match resolve_dataset(context, name, spec, before).await {
            Ok(locked) => datasets.push(locked),
            Err(failure) => failures.push(failure.subject(name.as_str())),
        }
    }
    if let Some(first) = report_all_but_first(context, failures) {
        return Err(first);
    }
    let steps = old.map(|lock| lock.steps().to_vec()).unwrap_or_default();
    let lock = Lock::new(datasets, steps)?;
    let written = project.write_lock(&lock)?;
    if written {
        context.emit(Event::Locked {
            files: lock_files(&lock),
        });
    }
    Ok((lock, written))
}

/// Whether every dataset of `manifest` is locked with its current spec, and nothing else is.
pub(crate) fn is_current(manifest: &Manifest, lock: &Lock) -> bool {
    lock.datasets().len() == manifest.datasets.len()
        && lock.datasets().iter().all(|locked| {
            manifest
                .datasets
                .get(&locked.name)
                .is_some_and(|spec| spec.spec() == locked.spec)
        })
}

async fn resolve_dataset(
    context: &Context<'_>,
    name: &Name,
    spec: &DatasetSpec,
    before: Option<&LockedDataset>,
) -> Result<LockedDataset, Failure> {
    let filters = spec
        .filters()
        .map_err(|error| Failure::new(ErrorKind::ProjectInvalidManifest, error.to_string()))?;
    let listing = context.engine.resolve(&spec.reference, &filters).await?;
    let bytes = listing.entries.iter().filter_map(|entry| entry.size).sum();
    context.emit(Event::Resolved {
        dataset: name.clone(),
        resolved: listing.resolved.clone(),
        title: listing.meta.title.clone(),
        files: listing.entries.len() as u64,
        bytes,
        license: listing.meta.license.clone(),
    });
    let known: HashMap<&str, [u8; 32]> = before
        .and_then(|locked| locked.files.as_ref())
        .into_iter()
        .flatten()
        .filter_map(|file| Some((file.at.first()?.as_str(), file.blake3)))
        .collect();
    let wants = listing
        .entries
        .iter()
        .map(|entry| want(entry, &known))
        .collect::<Result<Vec<_>, _>>()?;
    let gots = fetch_reported(context, name, wants).await?;
    let files: Vec<LockedFile> = listing
        .entries
        .iter()
        .zip(&gots)
        .map(|(entry, got)| locked_file(entry, got))
        .collect();
    let tree = Tree::new(
        files
            .iter()
            .map(|file| TreeEntry {
                path: file.path.clone(),
                size: file.size,
                blake3: file.blake3,
            })
            .collect(),
    )
    .map_err(|error| Failure::new(ErrorKind::NetworkProtocol, error.to_string()))?
    .hash();
    Ok(LockedDataset {
        name: name.clone(),
        reference: spec.reference.clone(),
        spec: spec.spec(),
        resolved: listing.resolved,
        title: listing.meta.title,
        license: listing.meta.license,
        doi: listing.meta.doi,
        retrieved: clock::now()?,
        tree,
        files: Some(files),
    })
}

fn want(entry: &Entry, known: &HashMap<&str, [u8; 32]>) -> Result<Want, Failure> {
    let url = entry.urls.first().ok_or_else(|| {
        Failure::new(
            ErrorKind::ReferenceUnresolved,
            format!("the source names no location for `{}`", entry.path),
        )
    })?;
    Ok(Want {
        url: url.clone(),
        size: entry.size,
        stated: entry.stated.clone(),
        blake3: known.get(url.to_string().as_str()).copied(),
    })
}

fn locked_file(entry: &Entry, got: &Got) -> LockedFile {
    let mut file = LockedFile {
        path: entry.path.clone(),
        size: got.size,
        blake3: got.blake3,
        sha256: None,
        sha1: None,
        md5: None,
        at: entry.urls.iter().map(SafeUrl::to_string).collect(),
        from: None,
        role: None,
    };
    for digest in &entry.stated {
        match *digest {
            Digest::Sha256(bytes) => file.sha256 = Some(bytes),
            Digest::Sha1(bytes) => file.sha1 = Some(bytes),
            Digest::Md5(bytes) => file.md5 = Some(bytes),
            Digest::Blake3(_) => {}
        }
    }
    file
}

/// Fetches `wants` for `dataset`, says how much came over the network, and stops at the first
/// failure after reporting the others.
async fn fetch_reported(
    context: &Context<'_>,
    dataset: &Name,
    wants: Vec<Want>,
) -> Result<Vec<Got>, Failure> {
    let started = Instant::now();
    let results = context.engine.fetch(dataset.as_str(), wants).await;
    let mut gots = Vec::with_capacity(results.len());
    let mut failures = Vec::new();
    for result in results {
        match result {
            Ok(got) => gots.push(got),
            Err(failure) => failures.push(failure),
        }
    }
    if let Some(first) = report_all_but_first(context, failures) {
        return Err(first);
    }
    let fetched: Vec<&Got> = gots.iter().filter(|got| got.fetched > 0).collect();
    if !fetched.is_empty() {
        context.emit(Event::Fetched {
            dataset: dataset.clone(),
            files: fetched.len() as u64,
            bytes: fetched.iter().map(|got| got.fetched).sum(),
            elapsed: started.elapsed(),
        });
    }
    Ok(gots)
}

fn report_all_but_first(context: &Context<'_>, failures: Vec<Failure>) -> Option<Failure> {
    let mut failures = failures.into_iter();
    let first = failures.next()?;
    for failure in failures {
        context.emit(Event::Failed(failure.0));
    }
    Some(first)
}

/// What a sync did.
pub(crate) struct Synced {
    datasets: usize,
    linked: u64,
    fetched: u64,
    root: PathBuf,
}

impl Synced {
    fn outcome(&self) -> Outcome {
        let sentence = if self.linked == 0 {
            "Everything already matches data.lock, nothing to do.".to_owned()
        } else {
            format!(
                "Synced {} into {}, {} fetched over the network.",
                plural(self.linked, "file", "files"),
                self.root.display(),
                fl_model::units::size(self.fetched)
            )
        };
        Outcome {
            status: fl_model::document::Status::Ok,
            result: json!({
                "datasets": self.datasets,
                "linked": self.linked,
                "bytes_fetched": self.fetched,
                "data_dir": self.root.display().to_string(),
            }),
            answer: None,
            sentence,
        }
    }
}

/// The state of one locked file in the data directory.
pub(crate) enum Found {
    /// Placed and unchanged since.
    Clean,
    /// Not there.
    Missing,
    /// There, with other bytes than the lock records.
    Modified,
}

/// One locked file and where it belongs.
pub(crate) struct Slot<'l> {
    /// Its path relative to the data directory, as the state records it.
    pub(crate) key: String,
    /// Its path relative to the project root, for messages.
    pub(crate) shown: String,
    /// The file as locked.
    pub(crate) file: &'l LockedFile,
    /// What is there now.
    pub(crate) found: Found,
    /// Its fingerprint now, when it is there.
    pub(crate) print: Option<Fingerprint>,
}

/// Compares every file of `locked` with the data directory at `root`, rehashing a file only
/// when its fingerprint moved. Returns the dataset's directory and its files.
pub(crate) fn survey<'l>(
    context: &Context<'_>,
    root: &Path,
    shown_root: &str,
    dir: &DataPath,
    locked: &'l LockedDataset,
    state: &State,
) -> Result<Vec<Slot<'l>>, Failure> {
    let on_disk = root.join(dir.as_str());
    let present: BTreeMap<String, Fingerprint> = if on_disk.is_dir() {
        fingerprint::scan(&on_disk)?.into_iter().collect()
    } else {
        BTreeMap::new()
    };
    let mut slots = Vec::new();
    for file in locked.files.iter().flatten() {
        let key = format!("{dir}/{}", file.path);
        let print = present.get(file.path.as_str()).copied();
        let found = match print {
            None => Found::Missing,
            Some(print)
                if state
                    .files
                    .get(&key)
                    .is_some_and(|placed| placed.matches(&file.blake3, &print)) =>
            {
                Found::Clean
            }
            Some(_) => {
                context.engine.rehashed();
                let path = on_disk.join(file.path.as_str());
                if fingerprint::hash_file(&path)? == Digest::Blake3(file.blake3) {
                    Found::Clean
                } else {
                    Found::Modified
                }
            }
        };
        slots.push(Slot {
            shown: format!("{shown_root}/{key}"),
            key,
            file,
            found,
            print,
        });
    }
    Ok(slots)
}

/// The directory a dataset lands in under the data directory.
pub(crate) fn dataset_dir(manifest: &Manifest, name: &Name) -> Result<DataPath, Failure> {
    match manifest
        .datasets
        .get(name)
        .and_then(|spec| spec.path.clone())
    {
        Some(path) => Ok(path),
        None => DataPath::new(name.as_str())
            .map_err(|error| Failure::new(ErrorKind::ProjectInvalidManifest, error.to_string())),
    }
}

/// Makes the data directory match the lock, locking first when the lock is out of date. With
/// `only`, just that dataset is placed.
pub(crate) async fn sync_project(
    context: &Context<'_>,
    project: &Project,
    only: Option<&Name>,
) -> Result<Synced, Failure> {
    let manifest = project.manifest()?;
    let lock = match project.lock()? {
        Some(lock) if is_current(&manifest, &lock) => lock,
        _ => relock(context, project, &manifest).await?.0,
    };
    let root = project.data_dir(&manifest);
    let shown_root = manifest.project.data_dir.clone();
    let mut state = project.state()?;
    let chosen: Vec<&LockedDataset> = lock
        .datasets()
        .iter()
        .filter(|locked| only.is_none_or(|name| locked.name == *name))
        .collect();
    let mut surveys = Vec::with_capacity(chosen.len());
    for locked in &chosen {
        let dir = dataset_dir(&manifest, &locked.name)?;
        let slots = survey(context, &root, &shown_root, &dir, locked, &state)?;
        surveys.push((*locked, dir, slots));
    }
    let modified: Vec<&str> = surveys
        .iter()
        .flat_map(|(_, _, slots)| slots.iter())
        .filter(|slot| matches!(slot.found, Found::Modified))
        .map(|slot| slot.shown.as_str())
        .collect();
    if !modified.is_empty() {
        return Err(Failure::new(
            ErrorKind::LocalModified,
            format!(
                "{} changed by hand and would be overwritten: {}",
                plural(
                    modified.len() as u64,
                    "managed file was",
                    "managed files were"
                ),
                modified.join(", ")
            ),
        )
        .help(
            "move your edits out of the data directory, delete the files, then run `fl sync` again",
        ));
    }
    let mut synced = Synced {
        datasets: chosen.len(),
        linked: 0,
        fetched: 0,
        root: root.clone(),
    };
    for (locked, dir, slots) in surveys {
        for slot in &slots {
            if let (Found::Clean, Some(print)) = (&slot.found, &slot.print) {
                state
                    .files
                    .insert(slot.key.clone(), Placed::new(&slot.file.blake3, print));
            }
        }
        let needed: Vec<&Slot<'_>> = slots
            .iter()
            .filter(|slot| matches!(slot.found, Found::Missing))
            .collect();
        if needed.is_empty() {
            context.emit(Event::Unchanged {
                dataset: locked.name.clone(),
            });
            continue;
        }
        let started = Instant::now();
        let wants = needed
            .iter()
            .map(|slot| locked_want(slot.file))
            .collect::<Result<Vec<_>, _>>()?;
        let gots = fetch_reported(context, &locked.name, wants).await?;
        synced.fetched += gots.iter().map(|got| got.fetched).sum::<u64>();
        let files: Vec<(DataPath, [u8; 32])> = needed
            .iter()
            .map(|slot| (slot.file.path.clone(), slot.file.blake3))
            .collect();
        let placed = context.engine.link(&root, &dir, &files)?;
        let mut copied = 0;
        for ((destination, linked), slot) in placed.iter().zip(&needed) {
            copied += linked.bytes_copied;
            let print = Fingerprint::of(destination.path())?;
            state
                .files
                .insert(slot.key.clone(), Placed::new(&slot.file.blake3, &print));
        }
        synced.linked += placed.len() as u64;
        context.emit(Event::Linked {
            dataset: locked.name.clone(),
            path: format!("{shown_root}/{dir}"),
            elapsed: started.elapsed(),
            extra_bytes: copied,
        });
    }
    project.write_state(&state)?;
    context.engine.register(&project.lock_path())?;
    Ok(synced)
}

fn locked_want(file: &LockedFile) -> Result<Want, Failure> {
    let at = file.at.first().ok_or_else(|| {
        Failure::new(
            ErrorKind::ProjectInvalidLock,
            format!("data.lock names no location for `{}`", file.path),
        )
    })?;
    let url = SafeUrl::parse(at).map_err(|error| {
        Failure::new(
            ErrorKind::ProjectInvalidLock,
            format!("data.lock holds `{at}` for `{}`: {error}", file.path),
        )
    })?;
    let mut stated = Vec::new();
    if let Some(md5) = file.md5 {
        stated.push(Digest::Md5(md5));
    }
    if let Some(sha1) = file.sha1 {
        stated.push(Digest::Sha1(sha1));
    }
    if let Some(sha256) = file.sha256 {
        stated.push(Digest::Sha256(sha256));
    }
    Ok(Want {
        url,
        size: Some(file.size),
        stated,
        blake3: Some(file.blake3),
    })
}

/// No filters at all: every file of a listing is kept.
pub(crate) fn everything() -> Result<Filters, Failure> {
    Filters::new(Vec::new(), Vec::new(), None, None)
        .map_err(|error| Failure::new(ErrorKind::Usage, error.to_string()))
}

fn lock_files(lock: &Lock) -> u64 {
    lock.datasets()
        .iter()
        .map(|locked| locked.files.as_ref().map_or(0, Vec::len) as u64)
        .sum()
}

/// `n` and the noun that fits it.
pub(crate) fn plural(n: u64, one: &str, many: &str) -> String {
    fl_model::units::count(n, one, many).to_string()
}
