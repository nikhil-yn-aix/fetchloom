//! `fl sync`: making the data directory match the lock, locking first when it is out of date.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use fl_model::document::Status;
use fl_model::error::ErrorKind;
use fl_model::event::Event;
use fl_model::lock::{Lock, LockedDataset};
use fl_model::manifest::{Link, Manifest};
use fl_model::name::Name;
use fl_model::path::DataPath;
use fl_model::reference::Target;
use fl_store::fingerprint::Fingerprint;
use fl_store::link::Ladder;
use serde_json::json;

use crate::args;
use crate::commands::lock::{Relock, is_current, relock, stale};
use crate::commands::{
    Context, Found, Slot, Survey, dataset_dir, datasets_named, fetch_all, locked_want, plural,
    some_of, survey, worst,
};
use crate::engine::Got;
use crate::failure::Failure;
use crate::project::{Placed, Project, State};
use crate::report::Outcome;

/// `fl sync`.
pub(crate) async fn run(context: &Context<'_>, sync: &args::Sync) -> Result<Outcome, Failure> {
    let project = Project::find(context.cwd)?;
    let _held = context.hold(&project)?;
    let manifest = project.manifest()?;
    let only = datasets_named(&manifest, &sync.names)?;
    let options = Options {
        only: &only,
        locked: sync.locked,
        force: sync.force,
        clean: sync.clean,
    };
    let synced = sync_project(context, &project, &options).await?;
    Ok(synced.outcome(context))
}

/// What a sync may do.
#[derive(Default)]
pub(crate) struct Options<'a> {
    /// Only these datasets; all of them when empty.
    pub(crate) only: &'a [Name],
    /// Refuse instead of locking when the lock is out of date.
    pub(crate) locked: bool,
    /// Overwrite files changed by hand.
    pub(crate) force: bool,
    /// Remove files fetchloom did not place.
    pub(crate) clean: bool,
}

/// What a sync did.
pub(crate) struct Synced {
    datasets: usize,
    linked: u64,
    fetched: u64,
    removed: u64,
    root: PathBuf,
    failures: Vec<Failure>,
}

impl Synced {
    /// The bytes it fetched, locking included.
    pub(crate) const fn fetched(&self) -> u64 {
        self.fetched
    }

    /// The command's result, without the closing sentence.
    pub(crate) fn result(&self) -> serde_json::Value {
        json!({
            "datasets": self.datasets,
            "linked": self.linked,
            "removed": self.removed,
            "bytes_fetched": self.fetched,
            "failed": self.failures.len(),
            "data_dir": self.root.display().to_string(),
        })
    }

    /// The sync as a whole, failing when any file failed: the others are reported and the most
    /// severe one is returned.
    pub(crate) fn finished(mut self, context: &Context<'_>) -> Result<Self, Failure> {
        match worst(context, std::mem::take(&mut self.failures)) {
            Some(failure) => Err(failure),
            None => Ok(self),
        }
    }

    fn outcome(mut self, context: &Context<'_>) -> Outcome {
        let result = self.result();
        let fetched = fl_model::units::size(self.fetched);
        let sentence = match (self.linked, self.removed) {
            (0, _) if !self.failures.is_empty() => format!(
                "Nothing was placed: {}.",
                plural(
                    self.failures.len() as u64,
                    "problem stopped it",
                    "problems stopped it"
                )
            ),
            _ if !self.failures.is_empty() => format!(
                "Placed {} into {}; {} stopped the rest.",
                plural(self.linked, "file", "files"),
                self.root.display(),
                plural(self.failures.len() as u64, "problem", "problems")
            ),
            (0, 0) => "Everything already matches data.lock, nothing to do.".to_owned(),
            (0, removed) => format!(
                "Removed {} that data.lock no longer lists.",
                plural(removed, "file", "files")
            ),
            (linked, _) => format!(
                "Synced {} into {}, {fetched} downloaded.",
                plural(linked, "file", "files"),
                self.root.display(),
            ),
        };
        match worst(context, std::mem::take(&mut self.failures)) {
            Some(failure) => {
                let kind = failure.kind();
                context.emit(Event::Failed(failure.into_problem()));
                Outcome {
                    status: Status::Partial,
                    result,
                    answer: None,
                    sentence,
                    exit: Some(kind),
                }
            }
            None => Outcome::ok(result, sentence),
        }
    }
}

/// Makes the data directory match the lock, locking first when the lock is out of date.
pub(crate) async fn sync_project(
    context: &Context<'_>,
    project: &Project,
    options: &Options<'_>,
) -> Result<Synced, Failure> {
    let manifest = project.manifest()?;
    let root = project.data_dir(&manifest)?;
    let (lock, fetched) = current_lock(context, project, &manifest, options.locked).await?;
    let shown_root = manifest.project.data_dir.to_string();
    let mut state = project.state()?;
    let chosen: Vec<&LockedDataset> = lock
        .datasets()
        .iter()
        .filter(|locked| options.only.is_empty() || options.only.contains(&locked.name))
        .collect();
    let mut surveys = Vec::with_capacity(chosen.len());
    let mut touch = false;
    for locked in &chosen {
        let dir = dataset_dir(&manifest, &locked.name)?;
        let found = survey(context, &root, &shown_root, &dir, locked, &state)?;
        touch |= found.confirmed > 0;
        surveys.push((*locked, dir, found));
    }
    let modified: Vec<String> = surveys
        .iter()
        .flat_map(|(_, _, found)| &found.slots)
        .filter(|slot| slot.found == Found::Modified)
        .map(|slot| slot.shown.clone())
        .collect();
    if !modified.is_empty() && !options.force {
        return Err(Failure::new(
            ErrorKind::LocalModified,
            format!(
                "{} changed by hand, and sync never overwrites them: {}",
                plural(modified.len() as u64, "file was", "files were"),
                some_of(&modified, 5)
            ),
        )
        .help(
            "`fl restore` puts the downloaded files back, or `fl sync --force` overwrites them",
        ));
    }
    let mut synced = Synced {
        datasets: chosen.len(),
        linked: 0,
        fetched,
        removed: 0,
        root: root.clone(),
        failures: Vec::new(),
    };
    let extras: Vec<String> = surveys
        .iter()
        .flat_map(|(_, _, found)| found.extras.iter().cloned())
        .collect();
    tidy_extras(
        context,
        &root,
        &shown_root,
        &extras,
        options.clean,
        &mut synced,
    )?;
    for key in surveys.iter().flat_map(|(_, _, found)| &found.stale) {
        fl_store::data_dir::remove(&root.join(key))?;
        state.files.remove(key);
        synced.removed += 1;
        context.emit(Event::Note {
            label: "Removed".to_owned(),
            text: format!("{shown_root}/{key}, which data.lock no longer lists"),
        });
    }
    for (locked, dir, found) in &surveys {
        let ladder = ladder(&manifest, &locked.name, context.link);
        place(
            context,
            &Placing {
                root: &root,
                shown_root: &shown_root,
                dir,
                locked,
                ladder,
                force: options.force,
            },
            found,
            &mut state,
            &mut synced,
        )
        .await?;
    }
    if options.only.is_empty() {
        let kept: Vec<String> = surveys
            .iter()
            .map(|(_, dir, _)| format!("{dir}/"))
            .collect();
        synced.removed += drop_removed(context, &root, &shown_root, &kept, &mut state)?;
    }
    project.write_state(&state, touch)?;
    context.engine.register(&project.lock_path())?;
    Ok(synced)
}

/// The lock, locked first when it is out of date with `manifest`, and the bytes locking
/// fetched. With `locked`, an out of date lock is refused instead.
async fn current_lock(
    context: &Context<'_>,
    project: &Project,
    manifest: &Manifest,
    locked: bool,
) -> Result<(Lock, u64), Failure> {
    match project.lock()? {
        Some(lock) if is_current(manifest, &lock) => Ok((lock, 0)),
        found if locked => Err(stale(if found.is_some() {
            "data.lock is out of date with data.toml, and --locked keeps it as it is"
        } else {
            "there is no data.lock, and --locked does not write one"
        })),
        _ => {
            let how = Relock {
                typed: None,
                upgrade: None,
            };
            let relocked = relock(context, project, manifest, &how).await?;
            Ok((relocked.lock, relocked.fetched))
        }
    }
}

/// The ladder a dataset is placed with: copies when the dataset or the machine says so.
fn ladder(manifest: &Manifest, name: &Name, machine: Link) -> Ladder {
    let dataset = manifest.datasets.get(name).map(|spec| spec.link);
    if machine == Link::Copy || dataset == Some(Link::Copy) {
        Ladder::COPY
    } else {
        Ladder::AUTO
    }
}

fn tidy_extras(
    context: &Context<'_>,
    root: &Path,
    shown_root: &str,
    extras: &[String],
    clean: bool,
    synced: &mut Synced,
) -> Result<(), Failure> {
    if extras.is_empty() {
        return Ok(());
    }
    if !clean {
        let shown: Vec<String> = extras
            .iter()
            .map(|key| format!("{shown_root}/{key}"))
            .collect();
        context.emit(Event::Note {
            label: "Kept".to_owned(),
            text: format!(
                "{} you added: {}; `fl sync --clean` removes {}",
                plural(extras.len() as u64, "file", "files"),
                some_of(&shown, 3),
                if extras.len() == 1 { "it" } else { "them" }
            ),
        });
        return Ok(());
    }
    for key in extras {
        let path = root.join(key);
        fs::remove_file(&path).map_err(|error| Failure::io("remove", &path, &error))?;
        context.emit(Event::Note {
            label: "Removed".to_owned(),
            text: format!("{shown_root}/{key}"),
        });
        synced.removed += 1;
    }
    Ok(())
}

/// One dataset being placed.
struct Placing<'a> {
    root: &'a Path,
    shown_root: &'a str,
    dir: &'a DataPath,
    locked: &'a LockedDataset,
    ladder: Ladder,
    force: bool,
}

async fn place(
    context: &Context<'_>,
    placing: &Placing<'_>,
    found: &Survey<'_>,
    state: &mut State,
    synced: &mut Synced,
) -> Result<(), Failure> {
    let name = &placing.locked.name;
    for slot in &found.slots {
        if let (Found::Clean, Some(print)) = (&slot.found, &slot.print) {
            state
                .files
                .insert(slot.key.clone(), Placed::new(&slot.file.blake3, print));
        }
    }
    let needed: Vec<&Slot<'_>> = found
        .slots
        .iter()
        .filter(|slot| {
            slot.found == Found::Missing || (placing.force && slot.found == Found::Modified)
        })
        .collect();
    context.emit(Event::Detail {
        label: "Dataset".to_owned(),
        text: format!(
            "{name}: {} locked, {} in place, {} to place",
            plural(found.slots.len() as u64, "file", "files"),
            found.slots.len() - needed.len(),
            needed.len()
        ),
    });
    if needed.is_empty() {
        context.emit(Event::Unchanged {
            dataset: name.clone(),
        });
        return Ok(());
    }
    let started = Instant::now();
    let (held, wanted): (Vec<&Slot<'_>>, Vec<&Slot<'_>>) = needed
        .iter()
        .partition(|slot| context.engine.reuse(&slot.file.blake3));
    let mut ready: Vec<&Slot<'_>> = held;
    if context.offline {
        if !wanted.is_empty() {
            let shown: Vec<String> = wanted.iter().map(|slot| slot.shown.clone()).collect();
            synced.failures.push(
                Failure::new(
                    ErrorKind::OfflineMissing,
                    format!(
                        "offline, and {} of {name} {} not been downloaded on this machine: {}",
                        plural(shown.len() as u64, "file", "files"),
                        if shown.len() == 1 { "has" } else { "have" },
                        some_of(&shown, 3)
                    ),
                )
                .subject(name.as_str())
                .help(format!(
                    "run `fl sync {name}` with the network to download them"
                )),
            );
        }
    } else {
        context.agree_to_download(name, wanted.iter().map(|slot| slot.file.size).sum())?;
        let local = matches!(
            placing.locked.reference.target(),
            Target::File { .. } | Target::Path(_)
        );
        let (got, failures) = fetch_from_any(context, name, &wanted, local).await?;
        synced.fetched += got.iter().map(|(_, got)| got.fetched).sum::<u64>();
        ready.extend(got.into_iter().map(|(slot, _)| slot));
        synced.failures.extend(failures);
    }
    if ready.is_empty() {
        return Ok(());
    }
    let files: Vec<(DataPath, [u8; 32])> = ready
        .iter()
        .map(|slot| (slot.file.path.clone(), slot.file.blake3))
        .collect();
    let placed = context
        .engine
        .link(placing.root, placing.dir, &files, placing.ladder)?;
    let mut copied = 0;
    for ((destination, linked), slot) in placed.iter().zip(&ready) {
        copied += linked.bytes_copied;
        let print = Fingerprint::of(destination.path())?;
        state
            .files
            .insert(slot.key.clone(), Placed::new(&slot.file.blake3, &print));
    }
    synced.linked += placed.len() as u64;
    context.emit(Event::Linked {
        dataset: name.clone(),
        path: format!("{}/{}", placing.shown_root, placing.dir),
        elapsed: started.elapsed(),
        extra_bytes: copied,
    });
    Ok(())
}

/// Fetches `slots` from the first of their locations that serves the pinned bytes, trying the
/// next location of a file when one fails. Returns what arrived and why the rest did not.
async fn fetch_from_any<'s, 'l>(
    context: &Context<'_>,
    name: &Name,
    slots: &[&'s Slot<'l>],
    local: bool,
) -> Result<(Vec<(&'s Slot<'l>, Got)>, Vec<Failure>), Failure> {
    let mut got = Vec::new();
    let mut failures = Vec::new();
    let mut pending: Vec<(&Slot<'_>, Option<Failure>)> =
        slots.iter().map(|slot| (*slot, None)).collect();
    let mut at = 0;
    while !pending.is_empty() {
        let mut tried = Vec::new();
        let mut wants = Vec::new();
        for (slot, last) in pending {
            match locked_want(slot.file, at, local)? {
                Some(want) => {
                    wants.push(want);
                    tried.push((slot, last));
                }
                None => failures.push(given_up(name, slot, last)),
            }
        }
        let results = fetch_all(context, name, wants).await;
        pending = Vec::new();
        for ((slot, _), result) in tried.into_iter().zip(results) {
            match result {
                Ok(arrived) => {
                    if at > 0 {
                        context.emit(Event::Warning(format!(
                            "{} failed for {}, so it came from {}",
                            host(slot.file.at.first()),
                            slot.shown,
                            host(slot.file.at.get(at))
                        )));
                    }
                    got.push((slot, arrived));
                }
                Err(failure) => pending.push((slot, Some(failure))),
            }
        }
        at += 1;
    }
    Ok((got, failures))
}

fn host(location: Option<&String>) -> String {
    location
        .and_then(|at| fl_transfer::url::SafeUrl::parse(at).ok())
        .and_then(|url| url.host().map(str::to_owned))
        .unwrap_or_else(|| "its location".to_owned())
}

/// The failure of a file no location served: drift when the bytes were not the pinned ones.
fn given_up(name: &Name, slot: &Slot<'_>, last: Option<Failure>) -> Failure {
    let failure = last.unwrap_or_else(|| {
        Failure::new(
            ErrorKind::ProjectInvalidLock,
            format!("data.lock names no location for `{}`", slot.file.path),
        )
    });
    if failure.kind() == ErrorKind::IntegrityMismatch {
        return Failure::new(
            ErrorKind::IntegrityDrift,
            format!(
                "{} is now served with other bytes than data.lock pins, at every location it lists",
                slot.shown
            ),
        )
        .subject(name.as_str())
        .help(format!(
            "`fl lock --upgrade {name}` accepts the new version"
        ));
    }
    failure.subject(slot.shown.clone())
}

/// Removes the files fetchloom placed outside the folders `kept`, and their folders once
/// empty. Returns how many files were removed.
pub(crate) fn drop_removed(
    context: &Context<'_>,
    root: &Path,
    shown_root: &str,
    kept: &[String],
    state: &mut State,
) -> Result<u64, Failure> {
    let gone: Vec<String> = state
        .files
        .keys()
        .filter(|key| !kept.iter().any(|dir| key.starts_with(dir.as_str())))
        .cloned()
        .collect();
    let mut dirs = BTreeSet::new();
    let mut removed = 0;
    for key in &gone {
        let path = root.join(key);
        if path.exists() && !state.untouched(key, &path)? {
            context.emit(Event::Warning(format!(
                "kept {shown_root}/{key}, which data.lock no longer lists, because it was changed by hand"
            )));
        } else {
            fl_store::data_dir::remove(&path)?;
            removed += 1;
        }
        state.files.remove(key);
        if let Some((dir, _)) = key.split_once('/') {
            dirs.insert(dir.to_owned());
        }
        prune(root, path.parent());
    }
    for dir in dirs {
        let path = root.join(&dir);
        if path.exists() {
            context.emit(Event::Warning(format!(
                "kept {shown_root}/{dir}, which data.lock no longer lists, because it holds files you added"
            )));
        } else {
            context.emit(Event::Note {
                label: "Removed".to_owned(),
                text: format!("{shown_root}/{dir}, which data.lock no longer lists"),
            });
        }
    }
    Ok(removed)
}

fn prune(root: &Path, mut dir: Option<&Path>) {
    while let Some(path) = dir.filter(|path| path.starts_with(root) && *path != root) {
        if fs::remove_dir(path).is_err() {
            return;
        }
        dir = path.parent();
    }
}
