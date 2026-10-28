//! placing many files at once and recording each one the way later scans will see it.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::{local, placed};
use crate::cache::Cache;
use crate::error::{Error, Kind};
use crate::place::{Method, Order, Placer};
use crate::plan::Target;
use crate::project::Project;
use crate::project::state::{Look, State};
use crate::work::Work;

/// what one dataset's placing cost, for its summary line.
#[derive(Default)]
pub(super) struct Tally {
    pub(super) files: u64,
    pub(super) links: u64,
    pub(super) copied: u64,
}

/// places every file in `todo` several at once and records each one placed in `state`. a failure
/// leaves the others placed and recorded, and the first one is returned.
pub(super) fn put_all<'t>(
    project: &Project,
    placer: &mut Placer,
    cache: &Cache,
    todo: &[&'t Target],
    state: &mut State,
    work: &Work,
) -> (BTreeMap<&'t str, Tally>, Option<Error>) {
    let objects: Vec<PathBuf> = todo
        .iter()
        .map(|target| cache.object_path(&target.sha256))
        .collect();
    let destinations: Vec<PathBuf> = todo
        .iter()
        .map(|target| project.root.join(&target.path))
        .collect();
    let orders: Vec<Order> = todo
        .iter()
        .zip(objects.iter().zip(&destinations))
        .map(|(target, (object, destination))| Order {
            object,
            destination,
            writable: target.copy,
        })
        .collect();
    let results = placer.place_all(&orders);
    let done = todo
        .iter()
        .zip(&results)
        .filter(|(_, result)| result.is_ok())
        .map(|(target, _)| target.path.as_str());
    let looks = looks(&project.root, done);
    let mut tallies: BTreeMap<&str, Tally> = BTreeMap::new();
    let mut failure = None;
    for (index, result) in results.into_iter().enumerate() {
        let target = todo[index];
        let method = match result {
            Ok(method) => method,
            Err(err) => {
                failure = failure
                    .or_else(|| Some(failed(target, &objects[index], &destinations[index], &err)));
                continue;
            }
        };
        if let Some(look) = looks.get(target.path.as_str()) {
            state
                .files
                .insert(target.path.clone(), placed(target, method, *look));
        }
        count(work, method);
        let tally = tallies.entry(target.dataset.as_str()).or_default();
        tally.files += 1;
        if method == Method::Copy {
            tally.copied += target.size;
        } else {
            tally.links += 1;
        }
    }
    (tallies, failure)
}

/// what went wrong placing `target`: the cache losing its object, or the disk refusing.
fn failed(target: &Target, object: &Path, destination: &Path, err: &io::Error) -> Error {
    if err.kind() == io::ErrorKind::NotFound && !object.exists() {
        return Error::new(
            Kind::LocalIo,
            format!("the cache lost {} while placing it", target.path),
        )
        .help("run the same command again to download it");
    }
    local(destination, err)
}

/// how each newly placed file looks, read the way every later scan reads it. on windows a
/// folder's listing carries every file's size and time, where reading one file costs an open
/// that antivirus scanners inspect; elsewhere a listing has names only, so each file is read.
fn looks<'p>(root: &Path, paths: impl Iterator<Item = &'p str>) -> HashMap<&'p str, Look> {
    if !cfg!(windows) {
        return paths
            .filter_map(|path| {
                let metadata = fs::symlink_metadata(root.join(path)).ok()?;
                Some((path, Look::of(&metadata)))
            })
            .collect();
    }
    let mut folders: HashMap<&str, HashMap<&str, &str>> = HashMap::new();
    for path in paths {
        let (folder, name) = path.rsplit_once('/').unwrap_or(("", path));
        folders.entry(folder).or_default().insert(name, path);
    }
    let mut found = HashMap::new();
    for (folder, names) in folders {
        for entry in fs::read_dir(root.join(folder))
            .into_iter()
            .flatten()
            .flatten()
        {
            let name = entry.file_name();
            if let Some(path) = name.to_str().and_then(|name| names.get(name))
                && let Ok(metadata) = entry.metadata()
            {
                found.insert(*path, Look::of(&metadata));
            }
        }
    }
    found
}

fn count(work: &Work, method: Method) {
    let counter = match method {
        Method::Reflink => &work.reflinks,
        Method::Hardlink => &work.hardlinks,
        Method::Copy => &work.copies,
    };
    Work::add(counter, 1);
}
