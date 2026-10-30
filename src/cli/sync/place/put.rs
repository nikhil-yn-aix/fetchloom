//! placing many files at once and recording how each one looked right after it was placed.

use std::collections::BTreeMap;
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
    checked: u64,
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
    let results = placer.place_all(&orders, Look::of);
    let mut tallies: BTreeMap<&str, Tally> = BTreeMap::new();
    let mut failure = None;
    for (index, result) in results.into_iter().enumerate() {
        let target = todo[index];
        let (method, look) = match result {
            Ok(done) => done,
            Err(err) => {
                failure = failure
                    .or_else(|| Some(failed(target, &objects[index], &destinations[index], &err)));
                continue;
            }
        };
        // the placer seals a reflink or copy writable exactly when the dataset asks for copies
        let writable = target.copy && method != Method::Hardlink;
        state.files.insert(
            target.path.clone(),
            placed(target, method, look, writable, checked),
        );
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

fn count(work: &Work, method: Method) {
    let counter = match method {
        Method::Reflink => &work.reflinks,
        Method::Hardlink => &work.hardlinks,
        Method::Copy => &work.copies,
    };
    Work::add(counter, 1);
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{Duration, Instant};

    use super::*;
    use crate::project::lock::LockedFile;

    #[test]
    fn an_edit_made_while_later_files_are_placed_is_not_recorded_as_placed() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path().join("cache")).unwrap();
        let bytes = b"station,temperature\nlerwick,9.1\n";
        let stored = crate::cache::tests::store(&cache, bytes);
        let locked = LockedFile {
            path: "weather.csv".to_owned(),
            size: bytes.len() as u64,
            sha256: stored.digest,
            md5: None,
            url: "https://example.org/weather.csv".to_owned(),
            unpacks_to: None,
        };
        // enough writable copies that the rest take a while once the first is in place
        let targets: Vec<Target> = (0..1000)
            .map(|index| Target {
                dataset: "weather".to_owned(),
                path: format!("data/weather/{index:04}.csv"),
                sha256: stored.digest,
                size: bytes.len() as u64,
                copy: true,
                locked: &locked,
            })
            .collect();
        let todo: Vec<&Target> = targets.iter().collect();
        let project = Project {
            root: dir.path().join("project"),
        };
        let mut placer = Placer::new(&project.root.join("data"), &project.own_folder());
        let mut state = State::default();
        let work = Work::default();
        let first = project.root.join(&targets[0].path);
        let second = project.root.join(&targets[1].path);

        std::thread::scope(|scope| {
            let placing =
                scope.spawn(|| put_all(&project, &mut placer, &cache, &todo, &mut state, 0, &work));
            // the first file is placed alone, so it is in place once the second appears
            let started = Instant::now();
            while !second.exists() {
                assert!(started.elapsed() < Duration::from_mins(1), "nothing placed");
                std::thread::sleep(Duration::from_millis(1));
            }
            fs::write(&first, "edited while the rest were placed\n").unwrap();
            let (_, failure) = placing.join().unwrap();
            assert!(failure.is_none());
        });

        assert_eq!(
            state.files[&targets[0].path].look.size,
            bytes.len() as u64,
            "the edit was recorded as the placed file"
        );
    }
}
