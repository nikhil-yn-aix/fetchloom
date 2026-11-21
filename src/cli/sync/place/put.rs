//! placing many files at once and recording how each one looked right after it was placed.

use std::collections::BTreeMap;
use std::io;
use std::path::Path;

use super::placed;
use crate::cache::Cache;
use crate::error::{Error, Kind};
use crate::place::{Method, Order, Placer};
use crate::plan::Target;
use crate::project::state::{Look, State};
use crate::project::{Project, native};
use crate::work::Work;

/// what one dataset's placing cost, for its summary line.
#[derive(Default)]
pub(super) struct Tally {
    pub(super) files: u64,
    pub(super) clones: u64,
    /// hardlinks, never clones.
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
    let mut tallies: BTreeMap<&str, Tally> = BTreeMap::new();
    let mut failure = None;
    // windows gives each new file a short 8.3 name, which takes the name of a file still to place
    // when that one's name has the form, as `abcdefghij.csv` takes `abcdef~1.csv`, so those go first
    let (first, rest): (Vec<&Target>, Vec<&Target>) = todo
        .iter()
        .partition(|target| cfg!(windows) && short_name_shaped(&target.path));
    for batch in [first, rest].into_iter().filter(|batch| !batch.is_empty()) {
        let (counted, first_failure) =
            put_batch(project, placer, cache, &batch, state, checked, work);
        for (dataset, tally) in counted {
            let sum = tallies.entry(dataset).or_default();
            sum.files += tally.files;
            sum.clones += tally.clones;
            sum.links += tally.links;
            sum.copied += tally.copied;
            sum.unlinked += tally.unlinked;
        }
        failure = failure.or(first_failure);
    }
    (tallies, failure)
}

/// whether the last name of `path` has the form of a short 8.3 name windows makes up, a base of
/// at most eight characters ending in `~` and digits, then at most three after a dot.
fn short_name_shaped(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    let (base, extension) = name.rsplit_once('.').unwrap_or((name, ""));
    let Some((_, number)) = base.rsplit_once('~') else {
        return false;
    };
    base.len() <= 8
        && extension.len() <= 3
        && !number.is_empty()
        && number.bytes().all(|byte| byte.is_ascii_digit())
}

/// [`put_all`] of one batch, placed at once.
fn put_batch<'t>(
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
            destination: native(&project.root, &target.path),
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
        match method {
            Method::Copy => {
                tally.copied += target.size;
                tally.unlinked += u64::from(!target.copy);
            }
            Method::Reflink => tally.clones += 1,
            Method::Hardlink => tally.links += 1,
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
    let error = Error::io(
        format_args!(
            "could not place {} at {}",
            object.display(),
            destination.display()
        ),
        err,
    );
    if cfg!(windows) && err.kind() == io::ErrorKind::AlreadyExists && is_link(destination) {
        return error.help(
            "a link stands at that name: remove the link itself, not what it points to, then run the same command again",
        );
    }
    match short_name_taker(destination, err) {
        Some(taker) => error.help(format!(
            "windows gives {taker}, beside it, that name as its short name: rename {taker}, or delete it if fetchloom placed it and sync again, which places a file with a name of that form first"
        )),
        None => error,
    }
}

/// whether a symbolic link or a junction stands at `path`, looked at without following it.
fn is_link(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|found| found.file_type().is_symlink())
}

/// the name of the file beside `destination` that windows answers its name for, as the short 8.3
/// name it gave that file, when that is why `err` says the name is taken. a link at the name is
/// no such file: following it would name what it points to.
fn short_name_taker(destination: &Path, err: &io::Error) -> Option<String> {
    if !cfg!(windows) || err.kind() != io::ErrorKind::AlreadyExists || is_link(destination) {
        return None;
    }
    let wanted = destination.file_name()?;
    let found = std::fs::canonicalize(destination).ok()?;
    let found = found.file_name()?;
    (!found.eq_ignore_ascii_case(wanted)).then(|| found.to_string_lossy().into_owned())
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
    use crate::project::lock::LockedFile;

    /// names of the form windows gives as short names are told apart from others.
    #[test]
    fn names_shaped_as_short_names_are_found() {
        for path in [
            "data/d/abcdef~1.csv",
            "data/d/ABCDE~12.CSV",
            "PROGRA~1",
            "a~1.c",
        ] {
            assert!(short_name_shaped(path), "{path}");
        }
        for path in [
            "data/d/abcdefghij.csv",
            "data/d/abcdefg~1.csv",
            "data/d/abcdef~1.csvx",
            "data/d/abcdef~.csv",
            "data/d/abc~x1.csv",
            "data/d/ABCDEF~12.CSV",
        ] {
            assert!(!short_name_shaped(path), "{path}");
        }
    }

    #[test]
    fn a_full_disk_the_cache_shares_names_what_unfinished_downloads_hold() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path().join("cache")).unwrap();
        let project = Project::at(dir.path().join("project"));
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
        fs::write(shard.join(format!("{key}.lock")), b"").unwrap();
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
                file: locked,
            })
            .collect();
        let todo: Vec<&Target> = targets.iter().collect();
        let project = Project::at(dir.path().join("project"));
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
                file: &locked,
            })
            .collect();
        let todo: Vec<&Target> = targets.iter().collect();
        let project = Project::at(dir.path().join("project"));
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

    /// a link made at a file's name after sync looked, here a junction to a folder outside the
    /// project, takes the name as a file would, and the refusal says a link stands there: following
    /// it would call the folder it points to a short-name owner, to be renamed.
    #[cfg(windows)]
    #[test]
    fn a_link_at_the_name_is_told_as_a_link_not_a_short_name() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path().join("cache")).unwrap();
        let stored = crate::cache::tests::store(&cache, b"station,temperature\n");
        let locked = LockedFile {
            path: "weather.csv".to_owned(),
            size: stored.size,
            sha256: stored.digest,
            md5: None,
            url: "https://example.org/weather.csv".to_owned(),
            unpacks_to: None,
        };
        let target = Target {
            dataset: "weather",
            path: "data/weather/weather.csv".to_owned(),
            sha256: stored.digest,
            size: stored.size,
            copy: false,
            file: &locked,
        };
        let project = Project::at(dir.path().join("project"));
        let data = project.root.join("data");
        fs::create_dir_all(data.join("weather")).unwrap();
        let outside = dir.path().join("outside");
        fs::create_dir(&outside).unwrap();
        let destination = data.join("weather").join("weather.csv");
        if crate::test_links::link_folder(&outside, &destination).is_err() {
            return crate::test_skips::skip("symlinks", "no link can be made here");
        }
        let mut placer = Placer::new(&data);
        let mut state = State::default();
        let (_, failure) = put_all(
            &project,
            &mut placer,
            &cache,
            &[&target],
            &mut state,
            0,
            &Work::default(),
        );
        let failure = failure.unwrap();
        assert_eq!(
            failure.help.as_deref(),
            Some(
                "a link stands at that name: remove the link itself, not what it points to, then run the same command again"
            ),
            "{}",
            failure.message
        );
        assert!(
            fs::read_dir(&outside).unwrap().next().is_none(),
            "nothing went through the link"
        );
    }

    /// a file that cannot be placed is named with this system's separators, as a person's own
    /// tools name it, never as data.lock writes it.
    #[cfg(windows)]
    #[test]
    fn a_file_that_cannot_be_placed_is_named_with_native_separators() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path().join("cache")).unwrap();
        let stored = crate::cache::tests::store(&cache, b"station,temperature\n");
        let locked = LockedFile {
            path: "weather.csv".to_owned(),
            size: stored.size,
            sha256: stored.digest,
            md5: None,
            url: "https://example.org/weather.csv".to_owned(),
            unpacks_to: None,
        };
        let target = Target {
            dataset: "weather",
            path: "data/weather/weather.csv".to_owned(),
            sha256: stored.digest,
            size: stored.size,
            copy: false,
            file: &locked,
        };
        let project = Project::at(dir.path().join("project"));
        let data = project.root.join("data");
        fs::create_dir_all(&data).unwrap();
        // a file where the dataset's folder belongs
        fs::write(data.join("weather"), "in the way\n").unwrap();
        let mut placer = Placer::new(&data);
        let mut state = State::default();
        let (_, failure) = put_all(
            &project,
            &mut placer,
            &cache,
            &[&target],
            &mut state,
            0,
            &Work::default(),
        );
        let message = failure.unwrap().message;
        let destination = data.join("weather").join("weather.csv");
        assert!(
            message.starts_with(&format!(
                "could not place {} at {}: ",
                cache.object_path(&stored.digest).display(),
                destination.display()
            )),
            "{message}"
        );
    }
}
