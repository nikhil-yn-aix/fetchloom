//! placing many files at once and recording how each one looked right after it was placed.

use std::collections::BTreeMap;
use std::io;
use std::path::Path;

use super::placed;
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
    /// copies of files that could have been links, unlike those `copy = true` asks for.
    pub(super) unlinked: u64,
}

/// places every file in `todo` and records each in `state`, counting in `work` each method and the
/// files from objects the run did not write; the first failure is returned, the rest still placed.
pub(super) fn put_all<'t>(
    project: &Project,
    placer: &mut Placer,
    cache: &Cache,
    todo: &[&'t Target],
    state: &mut State,
    checked: u64,
    work: &Work,
) -> (BTreeMap<&'t str, Tally>, Option<Error>) {
    let order = |index: usize| {
        let target = todo[index];
        Order {
            object: cache.object_path(&target.sha256),
            destination: project.root.join(&target.path),
            writable: target.copy,
            size: target.size,
        }
    };
    let results = placer.place_all(todo.len(), order, Look::of);
    let mut tallies: BTreeMap<&str, Tally> = BTreeMap::new();
    let mut failure = None;
    for (index, result) in results.into_iter().enumerate() {
        let target = todo[index];
        let (method, look) = match result {
            Ok(done) => done,
            Err(err) => {
                failure = failure.or_else(|| {
                    let order = order(index);
                    let error = failed(target, &order.object, &order.destination, &err);
                    Some(full_with_unfinished(error, &err, project, cache))
                });
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
        if work.cached_before(&target.sha256) {
            Work::add(&work.placed_from_cache, 1);
        }
        let tally = tallies.entry(target.dataset).or_default();
        tally.files += 1;
        if method == Method::Copy {
            tally.copied += target.size;
            tally.unlinked += u64::from(!target.copy);
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
            format!(
                "the cache lost {} while placing {} at {}",
                object.display(),
                target.path,
                destination.display()
            ),
        )
        .help("run the same command again to download it");
    }
    Error::io(
        format_args!(
            "could not place {} at {}",
            object.display(),
            destination.display()
        ),
        err,
    )
}

/// `error`, with what unfinished downloads hold when `err` says the disk is full and the cache
/// shares that disk with the project, since freeing them is one way to make room.
fn full_with_unfinished(error: Error, err: &io::Error, project: &Project, cache: &Cache) -> Error {
    if !matches!(
        err.kind(),
        io::ErrorKind::StorageFull | io::ErrorKind::QuotaExceeded
    ) || super::super::disk::apart(&project.root, cache.root())
    {
        return error;
    }
    match cache.unfinished_held() {
        Some(held) => error.detail(held),
        None => error,
    }
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
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    };

    use super::*;
    use crate::plan::Holder;
    use crate::project::lock::LockedFile;

    #[test]
    fn a_full_disk_the_cache_shares_names_what_unfinished_downloads_hold() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path().join("cache")).unwrap();
        let project = Project {
            root: dir.path().join("project"),
        };
        fs::create_dir(&project.root).unwrap();
        let full = || io::Error::from_raw_os_error(if cfg!(windows) { 112 } else { 28 });
        let error = || Error::io("could not place it", &full());
        assert_eq!(
            full_with_unfinished(error(), &full(), &project, &cache).details,
            Vec::<String>::new()
        );
        let key = "ab".repeat(32);
        let shard = cache.partial_folder(&key);
        fs::create_dir_all(&shard).unwrap();
        fs::write(shard.join(format!("{key}.part")), [0; 2048]).unwrap();
        let told = full_with_unfinished(error(), &full(), &project, &cache);
        assert_eq!(
            told.details,
            [format!(
                "unfinished downloads in {} hold {} of it, which `fetchloom cache gc --now` frees",
                cache.root().join("partial").display(),
                crate::units::size(cache.unfinished().bytes)
            )]
        );
        let denied = io::Error::from_raw_os_error(if cfg!(windows) { 5 } else { 13 });
        let other = full_with_unfinished(Error::io("x", &denied), &denied, &project, &cache);
        assert_eq!(other.details, Vec::<String>::new());
    }

    /// an object's time decides nothing: a disk keeping whole seconds, or a file server with its
    /// own clock, can date an object the run wrote long before the run began.
    #[test]
    fn placed_from_cache_counts_objects_the_run_did_not_write() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path().join("cache")).unwrap();
        let held = crate::cache::tests::store(&cache, b"held before the run\n");
        let written = crate::cache::tests::store(&cache, b"written by the run\n");
        // the start of 2000: FAT and exFAT keep no time before 1980
        let long_ago = std::time::UNIX_EPOCH + std::time::Duration::from_hours(262_968);
        for digest in [held.digest, written.digest] {
            let mut options = fs::OpenOptions::new();
            options.read(true);
            // windows sets a read-only file's times through a handle that may write attributes
            #[cfg(windows)]
            std::os::windows::fs::OpenOptionsExt::access_mode(&mut options, 0x100);
            options
                .open(cache.object_path(&digest))
                .unwrap()
                .set_times(fs::FileTimes::new().set_modified(long_ago))
                .unwrap();
        }
        let work = Work::default();
        work.wrote(&written.digest);
        let files: Vec<LockedFile> = [(&held, "held.csv"), (&written, "written.csv")]
            .into_iter()
            .map(|(stored, name)| LockedFile {
                path: name.to_owned(),
                size: stored.size,
                sha256: stored.digest,
                md5: None,
                url: format!("https://example.org/{name}"),
                unpacks_to: None,
            })
            .collect();
        let targets: Vec<Target> = files
            .iter()
            .map(|locked| Target {
                dataset: "both",
                path: format!("data/both/{}", locked.path),
                sha256: locked.sha256,
                size: locked.size,
                copy: false,
                holder: Holder::File(locked),
            })
            .collect();
        let todo: Vec<&Target> = targets.iter().collect();
        let project = Project {
            root: dir.path().join("project"),
        };
        let mut placer = Placer::new(&project.root.join("data"));
        let mut state = State::default();
        let (_, failure) = put_all(&project, &mut placer, &cache, &todo, &mut state, 0, &work);
        assert!(failure.is_none());
        let counts = work.counts();
        assert_eq!(counts.objects_written, 1);
        assert_eq!(counts.placed_from_cache, 1);
    }

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
        let targets: Vec<Target> = (0..2)
            .map(|index| Target {
                dataset: "weather",
                path: format!("data/weather/{index:04}.csv"),
                sha256: stored.digest,
                size: bytes.len() as u64,
                copy: true,
                holder: Holder::File(&locked),
            })
            .collect();
        let todo: Vec<&Target> = targets.iter().collect();
        let project = Project {
            root: dir.path().join("project"),
        };
        let mut state = State::default();
        let work = Work::default();
        let first = project.root.join(&targets[0].path);
        let second = project.root.join(&targets[1].path);

        let (notify, ready) = mpsc::channel();
        let (resume, edited) = mpsc::channel();
        let edited = Mutex::new(edited);
        let first_seen = AtomicBool::new(false);
        let placer = Placer::new(&project.root.join("data")).observing(move |_| {
            if !first_seen.swap(true, Ordering::SeqCst) {
                notify.send(second.exists()).unwrap();
                edited.lock().unwrap().recv().unwrap();
            }
        });
        std::thread::scope(|scope| {
            let placing = scope.spawn(|| {
                let mut placer = placer;

                put_all(&project, &mut placer, &cache, &todo, &mut state, 0, &work)
            });
            let observed = ready.recv();
            let changed = if observed.is_ok() {
                fs::write(&first, "edited while the rest were placed\n")
            } else {
                Ok(())
            };
            let _ = resume.send(());
            let (_, failure) = placing.join().unwrap();
            assert!(
                !observed.unwrap(),
                "metadata was deferred until later placements"
            );
            changed.unwrap();
            assert!(failure.is_none());
        });
        assert_eq!(
            fs::read_to_string(&first).unwrap(),
            "edited while the rest were placed\n"
        );

        assert_eq!(
            state.files[&targets[0].path].look.size,
            bytes.len() as u64,
            "the edit was recorded as the placed file"
        );
    }
}
