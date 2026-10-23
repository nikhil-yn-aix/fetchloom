//! `fl lock`, and bringing the lock in line with `data.toml`, which `add` and `sync` share.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use fl_model::digest::Digest;
use fl_model::error::ErrorKind;
use fl_model::event::Event;
use fl_model::license::restricts;
use fl_model::lock::{Lock, LockedDataset, LockedFile};
use fl_model::manifest::{DatasetSpec, Manifest};
use fl_model::name::Name;
use fl_model::path::DataPath;
use fl_model::reference::{Reference, Target};
use fl_model::tree::{Tree, TreeEntry};
use fl_model::units::size;
use fl_sources::Entry;
use fl_transfer::url::SafeUrl;
use serde_json::json;

use crate::clock;
use crate::commands::{
    Context, dataset_dir, datasets_named, fetch_every, is_typed_url, plural, want, worst,
};
use crate::engine::Got;
use crate::failure::Failure;
use crate::project::Project;
use crate::report::Outcome;

/// `fl lock`.
pub(crate) async fn run(
    context: &Context<'_>,
    upgrade: Option<&[String]>,
    check: bool,
) -> Result<Outcome, Failure> {
    let project = Project::find(context.cwd)?;
    let manifest = project.manifest()?;
    if check {
        return match project.lock()? {
            Some(lock) if is_current(&manifest, &lock) => Ok(Outcome::ok(
                json!({ "current": true }),
                "data.lock matches data.toml.".to_owned(),
            )),
            _ => Err(stale("data.lock is out of date with data.toml")),
        };
    }
    let _held = context.hold(&project)?;
    let upgrade = upgrade
        .map(|names| datasets_named(&manifest, names))
        .transpose()?
        .map(|names| names.into_iter().collect::<BTreeSet<Name>>());
    let how = Relock {
        typed: None,
        upgrade: upgrade.as_ref(),
    };
    let relocked = relock(context, &project, &manifest, &how).await?;
    if upgrade.is_some() {
        for change in &relocked.changes {
            show_diff(context, change);
        }
    }
    let files = lock_files(&relocked.lock);
    let sentence = match (relocked.written, upgrade.is_some()) {
        (false, true) => {
            "Every dataset is already at its newest version, nothing to do.".to_owned()
        }
        (false, false) => "data.lock already matches data.toml, nothing to do.".to_owned(),
        (true, _) => format!(
            "data.lock now holds {} of {}.",
            plural(files, "file", "files"),
            plural(relocked.lock.datasets().len() as u64, "dataset", "datasets")
        ),
    };
    Ok(Outcome::ok(
        json!({
            "written": relocked.written,
            "datasets": relocked.lock.datasets().len(),
            "files": files,
            "changes": relocked.changes.iter().map(Change::to_json).collect::<Vec<_>>(),
        }),
        sentence,
    ))
}

/// The failure of a lock that no longer matches `data.toml`, saying `why`.
pub(crate) fn stale(why: &str) -> Failure {
    Failure::new(ErrorKind::LockStale, why)
        .subject("data.lock")
        .help("run `fl lock` and commit data.lock")
}

/// How a relock treats the datasets whose entry did not change.
pub(crate) struct Relock<'a> {
    /// The reference the user wrote on this command line: only the URL it names may reach a
    /// private address.
    pub(crate) typed: Option<&'a Reference>,
    /// Datasets to resolve again even though their entry did not change; empty for all of
    /// them. Pinned ones stay.
    pub(crate) upgrade: Option<&'a BTreeSet<Name>>,
}

/// A lock brought in line with `data.toml`.
pub(crate) struct Relocked {
    /// The lock.
    pub(crate) lock: Lock,
    /// It differed from `data.lock`, which now holds it.
    pub(crate) written: bool,
    /// The bytes downloaded to learn the hashes of new files.
    pub(crate) fetched: u64,
    /// What changed in each dataset that was resolved.
    pub(crate) changes: Vec<Change>,
}

/// How many files a locked dataset has, and their size.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Totals {
    /// Files.
    pub(crate) files: u64,
    /// Bytes.
    pub(crate) bytes: u64,
}

impl Totals {
    fn of(locked: &LockedDataset) -> Self {
        let files = locked.files.as_deref().unwrap_or_default();
        Self {
            files: files.len() as u64,
            bytes: files.iter().map(|file| file.size).sum(),
        }
    }
}

/// What resolving a dataset again changed, file by file.
pub(crate) struct Change {
    /// The dataset.
    pub(crate) name: Name,
    /// Its files before, when it was locked before.
    pub(crate) before: Option<Totals>,
    /// Its files now.
    pub(crate) after: Totals,
    /// Files that are new, with their sizes.
    pub(crate) added: Vec<(DataPath, u64)>,
    /// Files that are gone, with their sizes.
    pub(crate) removed: Vec<(DataPath, u64)>,
    /// Files whose bytes changed, with their sizes before and now.
    pub(crate) changed: Vec<(DataPath, u64, u64)>,
}

impl Change {
    fn between(name: &Name, before: Option<&LockedDataset>, after: &LockedDataset) -> Self {
        let old: BTreeMap<&DataPath, &LockedFile> = before
            .and_then(|locked| locked.files.as_deref())
            .unwrap_or_default()
            .iter()
            .map(|file| (&file.path, file))
            .collect();
        let new: BTreeMap<&DataPath, &LockedFile> = after
            .files
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(|file| (&file.path, file))
            .collect();
        let mut change = Self {
            name: name.clone(),
            before: before.map(Totals::of),
            after: Totals::of(after),
            added: Vec::new(),
            removed: Vec::new(),
            changed: Vec::new(),
        };
        for (path, file) in &new {
            match old.get(path) {
                None => change.added.push(((*path).clone(), file.size)),
                Some(was) if was.blake3 != file.blake3 => {
                    change.changed.push(((*path).clone(), was.size, file.size));
                }
                Some(_) => {}
            }
        }
        for (path, file) in &old {
            if !new.contains_key(path) {
                change.removed.push(((*path).clone(), file.size));
            }
        }
        change
    }

    /// Whether any file was added, removed or changed.
    pub(crate) fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.changed.is_empty()
    }

    fn to_json(&self) -> serde_json::Value {
        json!({
            "dataset": self.name.as_str(),
            "added": self.added.iter().map(|(path, bytes)| json!({ "path": path.as_str(), "size": bytes })).collect::<Vec<_>>(),
            "removed": self.removed.iter().map(|(path, bytes)| json!({ "path": path.as_str(), "size": bytes })).collect::<Vec<_>>(),
            "changed": self.changed.iter().map(|(path, was, now)| json!({ "path": path.as_str(), "size_before": was, "size": now })).collect::<Vec<_>>(),
        })
    }
}

fn show_diff(context: &Context<'_>, change: &Change) {
    if change.is_empty() {
        context.emit(Event::Unchanged {
            dataset: change.name.clone(),
        });
        return;
    }
    let note = |label: &str, text: String| {
        context.emit(Event::Note {
            label: label.to_owned(),
            text,
        });
    };
    for (path, bytes) in &change.added {
        note("+", format!("{}/{path} ({})", change.name, size(*bytes)));
    }
    for (path, bytes) in &change.removed {
        note("-", format!("{}/{path} ({})", change.name, size(*bytes)));
    }
    for (path, was, now) in &change.changed {
        note(
            "~",
            format!("{}/{path} ({} to {})", change.name, size(*was), size(*now)),
        );
    }
}

/// Brings `data.lock` in line with `manifest`. Entries whose spec did not change are kept
/// without touching the network, unless `how` upgrades them; the rest are resolved. A failure
/// in one dataset does not stop the others, and the lock is written only when all succeeded.
pub(crate) async fn relock(
    context: &Context<'_>,
    project: &Project,
    manifest: &Manifest,
    how: &Relock<'_>,
) -> Result<Relocked, Failure> {
    let old = project.lock()?;
    let mut fetched = 0;
    let mut datasets = Vec::with_capacity(manifest.datasets.len());
    let mut changes = Vec::new();
    let mut failures = Vec::new();
    for (name, spec) in &manifest.datasets {
        let before = old
            .as_ref()
            .and_then(|lock| lock.datasets().iter().find(|locked| locked.name == *name));
        let upgrading = how
            .upgrade
            .is_some_and(|names| names.is_empty() || names.contains(name));
        if let Some(kept) = before.filter(|locked| locked.spec == spec.spec()) {
            if !upgrading {
                datasets.push(kept.clone());
                continue;
            }
            if let Some(pin) = pinned(&spec.reference) {
                context.emit(Event::Note {
                    label: "Pinned".to_owned(),
                    text: format!("{name} stays at {pin}, as data.toml pins it"),
                });
                datasets.push(kept.clone());
                continue;
            }
        }
        let typed = how
            .typed
            .is_some_and(|typed| typed.as_str() == spec.reference.as_str() && is_typed_url(typed));
        let known = if upgrading { None } else { before };
        match resolve_dataset(context, manifest, name, spec, known, typed).await {
            Ok((locked, bytes)) => {
                changes.push(Change::between(name, before, &locked));
                datasets.push(locked);
                fetched += bytes;
            }
            Err(failure) => failures.push(failure.subject(name.as_str())),
        }
    }
    if let Some(failure) = worst(context, failures) {
        return Err(failure);
    }
    let steps = old.map(|lock| lock.steps().to_vec()).unwrap_or_default();
    let lock = Lock::new(datasets, steps)?;
    let written = project.write_lock(&lock)?;
    let resolved: u64 = changes.iter().map(|change| change.after.files).sum();
    if written && resolved > 0 {
        context.emit(Event::Locked { files: resolved });
    }
    Ok(Relocked {
        lock,
        written,
        fetched,
        changes,
    })
}

/// The revision `reference` is pinned to, written after an `@`, if any.
fn pinned(reference: &Reference) -> Option<&str> {
    match reference.target() {
        Target::Scheme { body, .. } => body.rsplit_once('@').map(|(_, pin)| pin),
        _ => None,
    }
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

/// Resolves the dataset `name` and fetches what it needs to lock it. The hashes of `trusted`,
/// the dataset as locked before, are reused for files at the same location instead of being
/// downloaded again; an upgrade trusts none.
async fn resolve_dataset(
    context: &Context<'_>,
    manifest: &Manifest,
    name: &Name,
    spec: &DatasetSpec,
    trusted: Option<&LockedDataset>,
    typed: bool,
) -> Result<(LockedDataset, u64), Failure> {
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
    let dir = dataset_dir(manifest, name)?;
    for entry in &listing.entries {
        fl_store::data_dir::representable(&dir, &entry.path).map_err(|error| {
            Failure::from(error).help(format!(
                "leave it out with `--exclude \"{}\"` on `fl add`, or an `exclude` in data.toml",
                entry.path
            ))
        })?;
    }
    if let Some(license) = listing
        .meta
        .license
        .as_deref()
        .filter(|license| restricts(license))
    {
        context.emit(Event::Warning(format!(
            "{name} is licensed {license}, which limits how it may be used; read its terms first"
        )));
    }
    let known: HashMap<&str, [u8; 32]> = trusted
        .and_then(|locked| locked.files.as_ref())
        .into_iter()
        .flatten()
        .filter_map(|file| Some((file.at.first()?.as_str(), file.blake3)))
        .collect();
    let wants = listing
        .entries
        .iter()
        .map(|entry| want(entry, &known, typed))
        .collect::<Result<Vec<_>, _>>()?;
    context.agree_to_download(name, context.engine.to_download(&wants))?;
    let gots = fetch_every(context, name, wants).await?;
    let fetched = gots.iter().map(|got| got.fetched).sum();
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
    let locked = LockedDataset {
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
    };
    Ok((locked, fetched))
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

/// How many files `lock` lists.
pub(crate) fn lock_files(lock: &Lock) -> u64 {
    lock.datasets()
        .iter()
        .map(|locked| locked.files.as_ref().map_or(0, Vec::len) as u64)
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_revision_after_an_at_pins_a_reference() {
        let pin = |text: &str| pinned(&text.parse().unwrap()).map(str::to_owned);
        assert_eq!(pin("hf:datasets/org/set@v2").as_deref(), Some("v2"));
        assert_eq!(pin("openneuro:ds003061@1.1.0").as_deref(), Some("1.1.0"));
        assert_eq!(pin("zenodo:3608990"), None);
        assert_eq!(pin("https://host/a@b"), None);
    }
}
