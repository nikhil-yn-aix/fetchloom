//! keeping the cache tidy: which projects use it, how big it is, freeing what no project needs,
//! and hashing every object again.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use super::{Cache, Failure};
use crate::digest::{self, Sha256};
use crate::error::Error;
use crate::stop::Stop;
use crate::threads::Threads;

/// how long an object no project needs stays, so a download a project has not locked yet, or a
/// `get` from this morning, survives a gc.
pub(crate) const GRACE: Duration = Duration::from_hours(24);

pub(crate) struct Object {
    pub(crate) digest: Sha256,
    pub(crate) size: u64,
    pub(crate) path: PathBuf,
    /// when the object last changed, which gc ages it from: its modification time, or on unix its
    /// ctime when later, which a seal, publishing or a link moves, as a seal dates the modification
    /// time back to when the download began.
    pub(crate) changed: SystemTime,
    /// how many names its file has, more than one while a hardlink, such as a project's, shares
    /// it. none where a folder's listing does not tell, as on windows.
    pub(crate) names: Option<u64>,
}

impl Object {
    /// whether a name outside the cache, as a project's hardlink, keeps the object's bytes, so
    /// removing it frees nothing. windows listings count no names, so there the object is opened.
    pub(crate) fn has_other_names(&self) -> bool {
        self.names.map_or_else(
            || crate::place::has_other_names(&self.path),
            |names| names > 1,
        )
    }
}

#[derive(Default)]
pub(crate) struct Scan {
    pub(crate) objects: Vec<Object>,
    pub(crate) unreadable: Vec<Failure>,
}

/// what the projects using a cache need from it.
#[derive(Debug, Default)]
pub(crate) struct Needed {
    /// objects a project places, and archives a project has not unpacked on this machine yet.
    pub(crate) objects: HashSet<Sha256>,
    /// archives whose record tells what a project places from them.
    pub(crate) unpacked: HashSet<Sha256>,
    /// the results projects pin, by key and tree, whose records under `builds/` gc keeps.
    pub(crate) results: HashSet<(Sha256, Sha256)>,
    /// whether `uses` tells how the projects use each of `objects`.
    pub(crate) told: bool,
    /// how the projects use each of `objects`, as [`Needed::add`] merges it, when `told`.
    pub(crate) uses: HashMap<Sha256, Use>,
}

impl Needed {
    /// notes that a project needs the object `digest` as `used`. an object used in several ways
    /// is an input only while every use is one.
    pub(crate) fn add(&mut self, digest: Sha256, used: Use) {
        self.objects.insert(digest);
        if self.told {
            self.uses
                .entry(digest)
                .and_modify(|was| *was = was.and(used))
                .or_insert(used);
        }
    }
}

/// how a project uses an object it needs. gc keeps every one; `cache gc --inputs` may free one
/// whose every use is an input, where it can come back from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Use {
    /// a file of a fetched dataset in the data folder, or of one with `place = false` no recipe
    /// reads, which no build makes again.
    Placed,
    /// a file of a recipe's result, other than an input.
    Output,
    /// a file of a dataset with `place = false` that only recipes read, each of them built.
    Input(Supply),
    /// a file of a fetched dataset a recipe still to build reads: one data.lock pins no result
    /// of, one that changed since, or one whose result the cache lacks files of.
    Pending,
}

/// where the bytes of an input can come back from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Supply {
    /// the link data.lock pins for the file.
    Source,
    /// a build of the recipe that made it.
    Derived,
    /// nowhere but a mirror: a file imported from a folder on some machine.
    Imported,
}

impl Use {
    /// the use of an object used both as `self` and as `other`: an input only when both are,
    /// then from the supply that brings it back most readily.
    fn and(self, other: Use) -> Use {
        match (self, other) {
            (Use::Input(mine), Use::Input(theirs)) => Use::Input(mine.min(theirs)),
            (Use::Input(_), other) => other,
            (kept, _) => kept,
        }
    }
}

/// the downloads a run stopped before they finished, whose bytes the cache keeps so a later run
/// can continue them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Unfinished {
    pub(crate) downloads: u64,
    /// what their files take on the disk.
    pub(crate) bytes: u64,
}

/// what a sweep removed, or a dry run would remove, and what it kept.
#[derive(Debug, Default, Serialize)]
pub(crate) struct Freed {
    pub(crate) removed_objects: u64,
    /// the payload bytes of the objects removed.
    pub(crate) removed_bytes: u64,
    /// unfinished downloads and temporary files nobody came back for, removed.
    pub(crate) removed_leftovers: u64,
    /// what those leftovers took on the disk.
    pub(crate) removed_leftover_bytes: u64,
    pub(crate) not_removed: Vec<Failure>,
    /// objects no project needs that stay because they are younger than a day.
    pub(crate) kept_objects: u64,
    pub(crate) kept_bytes: u64,
    /// projects whose registration went because their data.toml is gone, as from a folder that
    /// moved, so what only they needed was freed too.
    pub(crate) forgotten: Vec<PathBuf>,
}

#[derive(Debug, Default, Serialize)]
pub(crate) struct Checked {
    pub(crate) objects: u64,
    pub(crate) bytes: u64,
    /// objects whose bytes no longer matched their name.
    pub(crate) changed: Vec<Sha256>,
    /// changed objects whose repair marks could not be written.
    pub(crate) stuck: Vec<Sha256>,
    /// objects or folders that could not be read, left untouched without a new change mark.
    pub(crate) unreadable: Vec<Failure>,
}

#[derive(Serialize, Deserialize)]
struct Registration {
    root: PathBuf,
}

impl Cache {
    /// remembers that the project at `root` uses this cache, so gc keeps what it needs. a
    /// registration turns read-only once its folder is flushed, so an unchanged read-only one costs
    /// nothing, and a writable one, whose flush may have failed, has its folder flushed again.
    pub(crate) fn register(&self, root: &Path) -> io::Result<()> {
        let path = self.registration(root);
        let text = serde_json::to_vec(&Registration {
            root: root.to_path_buf(),
        })
        .map_err(io::Error::other)?;
        match fs::symlink_metadata(&path) {
            Ok(found)
                if crate::volume::is_plain(&found)
                    && fs::read(&path).is_ok_and(|bytes| bytes == text) =>
            {
                if found.permissions().readonly() {
                    return Ok(());
                }
                super::sync_publication(&path, &path, crate::project::sync_directory)?;
            }
            Ok(found) => {
                // windows renames nothing over a read-only file
                if crate::volume::is_plain(&found) {
                    crate::volume::set_read_only(&path, false)?;
                }
                self.write_atomically(&path, &text)?;
            }
            Err(_) => self.write_atomically(&path, &text)?,
        }
        crate::volume::set_read_only(&path, true)
    }

    /// how many unfinished downloads the partial folder keeps, and what they take on the disk. a
    /// folder that cannot be read holds none.
    pub(crate) fn unfinished(&self) -> Unfinished {
        let mut unfinished = Unfinished::default();
        for folder in self.partial_folders() {
            let Ok(entries) = fs::read_dir(folder) else {
                continue;
            };
            for entry in entries.flatten() {
                if entry.file_name().to_string_lossy().ends_with(".part")
                    && let Ok(metadata) = entry.metadata()
                    && metadata.is_file()
                {
                    unfinished.downloads += 1;
                    unfinished.bytes += on_disk(&metadata);
                }
            }
        }
        unfinished
    }

    /// reads every registration before gc can discard anything it might protect.
    pub(crate) fn registered_projects(&self, stop: &Stop) -> Result<Vec<PathBuf>, Error> {
        let folder = self.root.join("projects");
        stop.check()?;
        let entries = match fs::read_dir(&folder) {
            Ok(entries) => entries,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(unreadable(&folder, &err)),
        };
        let mut roots = Vec::new();
        for entry in entries {
            stop.check()?;
            let path = entry.map_err(|err| unreadable(&folder, &err))?.path();
            let bytes = fs::read(&path).map_err(|err| unreadable(&path, &err))?;
            let registration: Registration = serde_json::from_slice(&bytes).map_err(|err| {
                unreadable(&path, &io::Error::new(io::ErrorKind::InvalidData, err))
            })?;
            roots.push(registration.root);
        }
        roots.sort();
        Ok(roots)
    }

    /// removes `path`, refusing when a folder between it and the cache root was swapped for a
    /// link or a file since the cache opened, so a sweep never deletes outside the cache.
    pub(super) fn remove_inside(&self, path: &Path) -> io::Result<()> {
        self.check_inside(path)?;
        self.volume.remove(path)
    }

    /// refuses `path` when a folder between it and the cache root is not a real folder.
    pub(super) fn check_inside(&self, path: &Path) -> io::Result<()> {
        let mut folder = path.parent();
        while let Some(current) =
            folder.filter(|folder| *folder != self.root && folder.starts_with(&self.root))
        {
            let metadata = fs::symlink_metadata(current)?;
            crate::place::found_folder(current, &metadata, crate::place::FolderUse::Cache)?;
            folder = current.parent();
        }
        Ok(())
    }

    /// removes the object `digest`, an input `cache gc --inputs` found elsewhere, and says whether
    /// it went, recording a refusal, as of a folder swapped for a link, in `not_removed`.
    pub(crate) fn free_input(&self, digest: &Sha256, not_removed: &mut Vec<Failure>) -> bool {
        let path = self.object_path(digest);
        removed(&path, self.remove_inside(&path), not_removed)
    }

    /// checks an archive member without letting a large writable object delay cancellation.
    pub(crate) fn contains_for_gc(
        &self,
        digest: &Sha256,
        size: u64,
        stop: &Stop,
    ) -> Result<bool, Error> {
        stop.check()?;
        let path = self.object_path(digest);
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            return Ok(false);
        };
        if !crate::volume::is_plain(&metadata) || metadata.len() != size {
            return Ok(false);
        }
        let intact = !super::changed::doubted(&path, &metadata, self.changed_at(digest)?)
            || hash(
                &path,
                digest::Block::sized(size.saturating_add(1)),
                stop,
                &|_| {},
            )
            .is_ok_and(|actual| actual == *digest);
        stop.check()?;
        Ok(intact)
    }

    /// every object in the cache, its fan folders read on up to `threads` workers, in the order
    /// a serial walk would give.
    pub(crate) fn scan_objects(&self, threads: Threads, stop: &Stop) -> Result<Scan, Error> {
        let mut scan = Scan::default();
        let folder = self.root.join("objects");
        // a file the system leaves behind, such as finder's .DS_Store, is no fan folder. a fan's
        // own name that is not a folder is damage, and is reported as unreadable
        let fans: Vec<(String, PathBuf)> = scan_entries(&folder, &mut scan.unreadable, stop)?
            .into_iter()
            .filter_map(|fan| {
                let prefix = fan.file_name().to_string_lossy().into_owned();
                (prefix.len() == 2 && prefix.bytes().all(|byte| byte.is_ascii_hexdigit()))
                    .then(|| (prefix, fan.path()))
            })
            .collect();
        let read = crate::place::run_coarse(&fans, threads, FANS_PER_WORKER, |(prefix, fan)| {
            scan_fan(prefix, fan, stop)
        });
        let read = read.into_iter().collect::<Result<Vec<_>, Error>>()?;
        scan.objects
            .reserve_exact(read.iter().map(|fan| fan.found.len()).sum());
        // each path is made here rather than on the worker, whose memory would stay spread over
        // threads that are gone
        for ((_, path), fan) in fans.iter().zip(read) {
            let mut paths = fan.paths.into_iter().peekable();
            for (at, found) in fan.found.into_iter().enumerate() {
                let path = if let Some((_, path)) = paths.next_if(|(place, _)| *place == at) {
                    path
                } else {
                    let hex = found.digest.hex();
                    let mut object = PathBuf::with_capacity(path.as_os_str().len() + 64);
                    object.push(path);
                    object.push(&hex[2..]);
                    object
                };
                scan.objects.push(Object {
                    digest: found.digest,
                    size: found.size,
                    path,
                    changed: found.changed,
                    names: found.names,
                });
            }
            scan.unreadable.extend(fan.unreadable);
        }
        super::failure::sort(&mut scan.unreadable);
        Ok(scan)
    }

    /// removes stale registrations, unneeded objects and unpinned result records older than
    /// `grace`, and downloads no run holds, which keep a day unless `grace` is zero, recording each
    /// refused removal. a download in progress keeps its bytes, as its run holds its lock.
    pub(crate) fn sweep_with_stop(
        &self,
        needed: &Needed,
        stale_projects: &[PathBuf],
        now: SystemTime,
        grace: Duration,
        threads: Threads,
        stop: &Stop,
    ) -> Result<Freed, Error> {
        let old = |modified: SystemTime| now.duration_since(modified).is_ok_and(|age| age > GRACE);
        let unused = |modified: SystemTime| {
            // FAT rounds a new file's time up, into the future, so no age is asked of `--now`
            grace.is_zero() || now.duration_since(modified).is_ok_and(|age| age >= grace)
        };
        let scan = self.scan_objects(threads, stop)?;
        if let Some(failure) = scan.unreadable.first() {
            return Err(failure.error("cannot inspect; the cache stays unchanged:"));
        }
        let mut freed = Freed::default();
        for root in stale_projects {
            stop.check()?;
            let path = self.registration(root);
            if removed(&path, self.remove_inside(&path), &mut freed.not_removed) {
                freed.forgotten.push(root.clone());
            }
        }
        for object in scan.objects {
            stop.check()?;
            if needed.objects.contains(&object.digest) {
                continue;
            }
            if !unused(object.changed) {
                freed.kept_objects += 1;
                freed.kept_bytes += object.size;
            } else if removed(
                &object.path,
                self.remove_inside(&object.path),
                &mut freed.not_removed,
            ) {
                freed.removed_objects += 1;
                freed.removed_bytes += object.size;
            }
        }
        for entry in scan_entries(&self.root.join("unpacked"), &mut freed.not_removed, stop)? {
            stop.check()?;
            let Some(archive) = Sha256::from_hex(&entry.file_name().to_string_lossy()) else {
                continue;
            };
            let path = entry.path();
            let stale = fs::metadata(&path)
                .and_then(|metadata| metadata.modified())
                .is_ok_and(unused);
            if !needed.unpacked.contains(&archive) && stale {
                removed(&path, self.remove_inside(&path), &mut freed.not_removed);
            }
        }
        for folder in scan_entries(&self.root.join("builds"), &mut freed.not_removed, stop)? {
            stop.check()?;
            if let Some(key) = Sha256::from_hex(&folder.file_name().to_string_lossy())
                && folder.file_type().is_ok_and(|kind| kind.is_dir())
            {
                self.sweep_results(&folder.path(), key, needed, &unused, stop, &mut freed)?;
            }
        }
        let abandoned = |modified: SystemTime| grace.is_zero() || old(modified);
        self.sweep_partials(&abandoned, stop, &mut freed)?;
        freed.removed_leftovers += self.sweep_staging(now, stop, &mut freed.not_removed);
        stop.check()?;
        self.forget_changed_reporting(&needed.objects, stop, &mut freed.not_removed)?;
        self.forget_url_records(stop, &mut freed.not_removed)?;
        for entry in scan_entries(&self.root.join("tmp"), &mut freed.not_removed, stop)? {
            stop.check()?;
            let stale = entry
                .metadata()
                .is_ok_and(|metadata| metadata.is_file() && metadata.modified().is_ok_and(old));
            let size = entry.metadata().map_or(0, |metadata| on_disk(&metadata));
            if stale
                && removed(
                    &entry.path(),
                    self.remove_inside(&entry.path()),
                    &mut freed.not_removed,
                )
            {
                freed.removed_leftovers += 1;
                freed.removed_leftover_bytes += size;
            }
        }
        stop.check()?;
        super::failure::sort(&mut freed.not_removed);
        Ok(freed)
    }

    /// removes the records in `folder` of `key`'s results that no project pins and that `unused`
    /// finds old enough, then the key's lock and its folder once no record is left. a build holds
    /// the cache while it runs, so a sweep, which holds it alone, never takes a lock one waits on.
    fn sweep_results(
        &self,
        folder: &Path,
        key: Sha256,
        needed: &Needed,
        unused: &dyn Fn(SystemTime) -> bool,
        stop: &Stop,
        freed: &mut Freed,
    ) -> Result<(), Error> {
        let mut left = false;
        let mut lock = None;
        for entry in scan_entries(folder, &mut freed.not_removed, stop)? {
            stop.check()?;
            let path = entry.path();
            let Some(tree) = Sha256::from_hex(&entry.file_name().to_string_lossy()) else {
                if entry.file_name() == "lock" {
                    lock = Some(path);
                } else {
                    left = true;
                }
                continue;
            };
            let stale = entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .is_ok_and(unused);
            left |= needed.results.contains(&(key, tree))
                || !stale
                || !removed(&path, self.remove_inside(&path), &mut freed.not_removed);
        }
        if left
            || lock.is_some_and(|lock| {
                !removed(&lock, self.remove_inside(&lock), &mut freed.not_removed)
            })
        {
            return Ok(());
        }
        let emptied = self
            .check_inside(folder)
            .and_then(|()| self.volume.remove_folder(folder));
        removed(folder, emptied, &mut freed.not_removed);
        Ok(())
    }

    /// drops the record of each link whose object and unpack record are both gone, which would
    /// otherwise only grow; the next download of that link writes a new one.
    fn forget_url_records(&self, stop: &Stop, not_removed: &mut Vec<Failure>) -> Result<(), Error> {
        for entry in scan_entries(&self.root.join("urls"), not_removed, stop)? {
            stop.check()?;
            let path = entry.path();
            let Some(record) = fs::read(&path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<super::UrlRecord>(&bytes).ok())
            else {
                continue;
            };
            // a pretend sweep keeps what it would remove, so their records go as well
            let missing = |path: &Path| {
                self.volume.pretends_removed(path) || path.try_exists().is_ok_and(|exists| !exists)
            };
            if missing(&self.object_path(&record.sha256))
                && missing(&self.unpacked_path(&record.sha256))
            {
                removed(&path, self.remove_inside(&path), not_removed);
            }
        }
        Ok(())
    }

    /// removes abandoned downloads and the lock files nobody holds, holding each lock; a run that
    /// opened one before it went locks a file with no name, which `claim` notices.
    fn sweep_partials(
        &self,
        abandoned: &dyn Fn(SystemTime) -> bool,
        stop: &Stop,
        freed: &mut Freed,
    ) -> Result<(), Error> {
        for folder in self.partial_folders() {
            self.sweep_partial_folder(&folder, abandoned, stop, freed)?;
        }
        Ok(())
    }

    fn sweep_partial_folder(
        &self,
        folder: &Path,
        abandoned: &dyn Fn(SystemTime) -> bool,
        stop: &Stop,
        freed: &mut Freed,
    ) -> Result<(), Error> {
        for entry in scan_entries(folder, &mut freed.not_removed, stop)? {
            stop.check()?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(key) = name.strip_suffix(".lock") else {
                continue;
            };
            let Some(_held) = super::partial::open_unfollowed(&entry.path(), false)
                .ok()
                .and_then(|lock| super::partial::unheld(&entry.path(), lock))
            else {
                continue;
            };
            let bytes = folder.join(format!("{key}.part"));
            let left = match fs::metadata(&bytes) {
                Ok(metadata) => metadata.modified().is_ok_and(abandoned),
                Err(err) => err.kind() == io::ErrorKind::NotFound,
            };
            if !left {
                continue;
            }
            for leftover in [bytes, folder.join(format!("{key}.json"))] {
                stop.check()?;
                let size = fs::symlink_metadata(&leftover).map_or(0, |metadata| on_disk(&metadata));
                if removed(
                    &leftover,
                    self.remove_inside(&leftover),
                    &mut freed.not_removed,
                ) {
                    freed.removed_leftovers += 1;
                    freed.removed_leftover_bytes += size;
                }
            }
            // an empty lock file frees nothing worth counting, and one left behind costs nothing
            let _ = self.remove_inside(&entry.path());
        }
        Ok(())
    }

    /// hashes every object on the requested threads, stopping between reads when asked.
    #[cfg(test)]
    pub(crate) fn verify_with_stop(
        &self,
        threads: usize,
        progress: &(dyn Fn(u64) + Sync),
        stop: &Stop,
    ) -> Result<Checked, Error> {
        self.verify_scanned(
            self.scan_objects(Threads::default(), stop)?,
            threads,
            progress,
            stop,
        )
    }

    /// hashes every object `scan` found, so a caller that sized a progress bar from the scan
    /// does not walk the cache twice.
    pub(crate) fn verify_scanned(
        &self,
        scan: Scan,
        threads: usize,
        progress: &(dyn Fn(u64) + Sync),
        stop: &Stop,
    ) -> Result<Checked, Error> {
        let objects = scan.objects;
        let next = AtomicUsize::new(0);
        let checked = Mutex::new(Checked {
            objects: objects.len() as u64,
            bytes: objects.iter().map(|object| object.size).sum(),
            unreadable: scan.unreadable,
            ..Checked::default()
        });
        let verify = || {
            while let Some(object) = objects.get(next.fetch_add(1, Ordering::Relaxed)) {
                if stop.is_asked() {
                    break;
                }
                let left = Cell::new(object.size);
                let block = digest::Block::sized(object.size.saturating_add(1));
                let actual = hash(&object.path, block, stop, &|read| {
                    let counted = read.min(left.get());
                    left.set(left.get() - counted);
                    progress(counted);
                });
                if stop.is_asked() {
                    break;
                }
                // an object that ended early or could not be read still fills its share of the bar
                if left.get() > 0 {
                    progress(left.get());
                }
                if stop.is_asked() {
                    break;
                }
                match actual {
                    Ok(actual) if actual == object.digest => continue,
                    Err(err) => {
                        checked
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .unreadable
                            .push(Failure::new(&object.path, &err));
                        continue;
                    }
                    Ok(_) => {}
                }
                let marked = self.mark_changed(&object.digest).is_ok();
                let mut found = checked
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                found.changed.push(object.digest);
                if !marked {
                    found.stuck.push(object.digest);
                }
            }
        };
        std::thread::scope(|scope| {
            let workers: Vec<_> = (0..threads.max(1))
                .map(|_| crate::threads::scoped(scope, &verify))
                .collect();
            for worker in workers {
                worker.join();
            }
        });
        stop.check()?;
        let mut checked = checked
            .into_inner()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        checked.changed.sort();
        checked.stuck.sort();
        super::failure::sort(&mut checked.unreadable);
        Ok(checked)
    }

    fn registration(&self, root: &Path) -> PathBuf {
        let key = digest::sha256(root.as_os_str().as_encoded_bytes()).hex();
        self.root.join("projects").join(format!("{key}.json"))
    }
}

/// the entries of `folder`, none when it does not exist, as in a cache a dry run left unmade.
/// a folder that cannot be read is recorded in `unreadable`.
pub(super) fn scan_entries(
    folder: &Path,
    unreadable: &mut Vec<Failure>,
    stop: &Stop,
) -> Result<Vec<fs::DirEntry>, Error> {
    stop.check()?;
    let mut found = Vec::new();
    match fs::read_dir(folder) {
        Ok(entries) => {
            for entry in entries {
                stop.check()?;
                match entry {
                    Ok(entry) => found.push(entry),
                    Err(err) => unreadable.push(Failure::new(folder, &err)),
                }
            }
        }
        Err(err)
            if err.kind() == io::ErrorKind::NotFound
                && fs::symlink_metadata(folder)
                    .is_err_and(|err| err.kind() == io::ErrorKind::NotFound) => {}
        Err(err) => unreadable.push(Failure::new(folder, &err)),
    }
    Ok(found)
}

/// fan folders a scan worker is worth, each holding many objects in a large cache.
const FANS_PER_WORKER: usize = 32;

/// one fan folder's objects as a scan worker reads them, without their paths.
struct Fan {
    found: Vec<Found>,
    /// the paths of the objects at these places in `found` whose names are not in lowercase, so
    /// not the ones their digests give.
    paths: Vec<(usize, PathBuf)>,
    unreadable: Vec<Failure>,
}

struct Found {
    digest: Sha256,
    size: u64,
    changed: SystemTime,
    names: Option<u64>,
}

/// the objects in the fan folder `fan`, whose name is `prefix`.
fn scan_fan(prefix: &str, fan: &Path, stop: &Stop) -> Result<Fan, Error> {
    let mut unreadable = Vec::new();
    let entries = scan_entries(fan, &mut unreadable, stop)?;
    let mut found = Vec::with_capacity(entries.len());
    let mut paths = Vec::new();
    for entry in &entries {
        stop.check()?;
        let name = entry.file_name();
        let name = name.as_encoded_bytes();
        if prefix.len() + name.len() != 64 {
            continue;
        }
        let mut hex = [0; 64];
        hex[..2].copy_from_slice(prefix.as_bytes());
        hex[2..].copy_from_slice(name);
        let Some(digest) = std::str::from_utf8(&hex).ok().and_then(Sha256::from_hex) else {
            continue;
        };
        match entry.metadata().and_then(|metadata| {
            let modified = metadata.modified()?;
            let changed =
                super::object::changed(&metadata).map_or(modified, |changed| changed.max(modified));
            // the metadata unix gives counts names, so this takes no further call; what a windows
            // listing gives does not count them
            #[cfg(unix)]
            let names = Some(std::os::unix::fs::MetadataExt::nlink(&metadata));
            #[cfg(not(unix))]
            let names = None;
            Ok((metadata.len(), changed, names))
        }) {
            Ok((size, changed, names)) => {
                if !hex
                    .iter()
                    .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
                {
                    paths.push((found.len(), entry.path()));
                }
                found.push(Found {
                    digest,
                    size,
                    changed,
                    names,
                });
            }
            Err(err) => unreadable.push(Failure::new(&entry.path(), &err)),
        }
    }
    Ok(Fan {
        found,
        paths,
        unreadable,
    })
}

pub(super) fn removed(path: &Path, result: io::Result<()>, not_removed: &mut Vec<Failure>) -> bool {
    match result {
        Ok(()) => true,
        Err(err) if err.kind() == io::ErrorKind::NotFound => false,
        Err(err) => {
            not_removed.push(Failure::new(path, &err));
            false
        }
    }
}

fn unreadable(path: &Path, error: &io::Error) -> Error {
    Error::io(format!("cannot read {}", path.display()), error)
}

/// what the file `metadata` describes takes on its disk, which for a split download with holes is
/// less than its length; windows makes no holes in these files, so there it is the length.
pub(super) fn on_disk(metadata: &fs::Metadata) -> u64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        metadata.blocks().saturating_mul(512)
    }
    #[cfg(not(unix))]
    {
        metadata.len()
    }
}

/// hashes the object at `path` through `block`, telling `progress` the bytes of each block as it
/// reads them.
fn hash(
    path: &Path,
    mut block: digest::Block,
    stop: &Stop,
    progress: &dyn Fn(u64),
) -> io::Result<Sha256> {
    let mut file = super::read_unfollowed(path)?;
    let mut hasher = digest::Hasher::default();
    let buffer = block.get();
    loop {
        if stop.is_asked() {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let read = file.read(buffer)?;
        if read == 0 {
            return Ok(hasher.finish().0);
        }
        hasher.update(&buffer[..read]);
        progress(read as u64);
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use crate::error::Kind;

    use super::super::tests::{cache, fat, hold, seal, store};
    use super::*;

    impl Cache {
        pub(crate) fn projects(&self) -> Vec<PathBuf> {
            self.registered_projects(&Stop::default()).unwrap()
        }

        pub(crate) fn objects(&self) -> Vec<Object> {
            self.scan_objects(Threads::default(), &Stop::default())
                .unwrap()
                .objects
        }

        pub(crate) fn sweep(&self, needed: &Needed, now: SystemTime) -> Freed {
            self.sweep_with_stop(
                needed,
                &[],
                now,
                GRACE,
                Threads::default(),
                &Stop::default(),
            )
            .unwrap()
        }

        pub(crate) fn verify_all(
            &self,
            threads: usize,
            progress: &(dyn Fn(u64) + Sync),
        ) -> Checked {
            self.verify_with_stop(threads, progress, &Stop::default())
                .unwrap()
        }
    }

    #[test]
    fn maintenance_failures_keep_their_io_causes() {
        for (remove, kind, message) in [
            (
                (|_: &Path| Err(io::Error::other("injected storage error")))
                    as fn(&Path) -> io::Result<()>,
                "Other",
                "injected storage error",
            ),
            (
                (|_: &Path| {
                    Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "injected access error",
                    ))
                }) as fn(&Path) -> io::Result<()>,
                "PermissionDenied",
                "injected access error",
            ),
        ] {
            let (_dir, mut cache) = cache();
            let digest = store(&cache, b"good").digest;
            let path = cache.object_path(&digest);
            cache.volume = std::sync::Arc::new(
                crate::volume::Volume::new(cache.root.join("tmp")).with_removal(remove),
            );
            let freed = cache
                .sweep_with_stop(
                    &Needed::default(),
                    &[],
                    SystemTime::now(),
                    Duration::ZERO,
                    Threads::default(),
                    &Stop::default(),
                )
                .unwrap();
            let report = serde_json::to_value(&freed).unwrap();
            assert_eq!(report["not_removed"].as_array().unwrap().len(), 1);
            assert_eq!(report["not_removed"][0]["path"], serde_json::json!(path));
            assert_eq!(report["not_removed"][0]["kind"], kind, "{report}");
            assert_eq!(report["not_removed"][0]["message"], message, "{report}");
            assert_eq!(fs::read(path).unwrap(), b"good");
        }
    }

    #[test]
    fn failed_registration_removals_keep_the_original_cause() {
        let (directory, mut cache) = cache();
        let root = directory.path().join("missing-project");
        cache.register(&root).unwrap();
        let path = cache.registration(&root);
        cache.volume = std::sync::Arc::new(
            crate::volume::Volume::new(cache.root.join("tmp"))
                .with_removal(|_| Err(io::Error::other("registration storage failure"))),
        );
        let freed = cache
            .sweep_with_stop(
                &Needed::default(),
                &[root],
                SystemTime::now(),
                GRACE,
                Threads::default(),
                &Stop::default(),
            )
            .unwrap();
        let report = serde_json::to_value(&freed).unwrap();
        assert_eq!(report["not_removed"].as_array().unwrap().len(), 1);
        assert_eq!(report["not_removed"][0]["path"], serde_json::json!(path));
        assert_eq!(report["not_removed"][0]["kind"], "Other");
        assert_eq!(
            report["not_removed"][0]["message"],
            "registration storage failure"
        );
        assert!(path.exists());
    }

    #[test]
    fn verification_runs_on_the_caller_when_workers_are_refused() {
        let (_dir, cache) = cache();
        store(&cache, b"good");
        let caller = std::thread::current().id();
        let _refusing = crate::threads::refuse_after(0);
        let checked = cache.verify_all(2, &|_| assert_eq!(std::thread::current().id(), caller));
        assert_eq!(checked.objects, 1);
        assert_eq!(checked.bytes, 4);
        assert_eq!(checked.changed, []);
        assert_eq!(checked.unreadable, []);
    }

    #[test]
    fn stopped_upkeep_does_not_read_or_remove_objects() {
        let (_dir, cache) = cache();
        let stored = store(&cache, b"keep");
        let root = cache.root.join("missing-project");
        cache.register(&root).unwrap();
        let registration = cache.registration(&root);
        let stop = Stop::default();
        stop.ask();
        let checked = cache.verify_with_stop(1, &|_| {}, &stop).unwrap_err();
        assert_eq!(checked.kind, Kind::Interrupted);
        let freed = cache
            .sweep_with_stop(
                &Needed::default(),
                &[root],
                SystemTime::now() + GRACE + Duration::from_secs(60),
                GRACE,
                Threads::default(),
                &stop,
            )
            .unwrap_err();
        assert_eq!(freed.kind, Kind::Interrupted);
        assert!(cache.object_path(&stored.digest).exists());
        assert!(registration.exists());
        assert!(cache.changed().is_empty());
    }

    #[test]
    fn stopping_verification_does_not_mark_the_current_object() {
        let (_dir, cache) = cache();
        let stored = store(&cache, b"good");
        damage(&cache.object_path(&stored.digest));
        let stop = Stop::default();
        let error = cache
            .verify_with_stop(1, &|_| stop.ask(), &stop)
            .unwrap_err();
        assert_eq!(error.kind, Kind::Interrupted);
        assert!(cache.changed().is_empty());
    }

    /// the fan folders are read on several workers, yet the scan is what a walk on one thread
    /// gives: the same objects in the same order, each at the path its folder gives it.
    #[test]
    fn a_scan_on_workers_matches_one_on_a_single_thread() {
        let (_dir, cache) = cache();
        let digests: Vec<Sha256> = (0..300)
            .map(|n| store(&cache, format!("object {n}").as_bytes()).digest)
            .collect();
        let fans: HashSet<String> = digests
            .iter()
            .map(|digest| digest.hex()[..2].to_owned())
            .collect();
        assert!(fans.len() >= 2 * FANS_PER_WORKER, "{} fans", fans.len());
        // a name in capitals still names its object, at its own path. the rename goes through
        // another name, since fat and exfat volumes ignore one that changes only the case
        let shouted = cache.object_path(&digests[0]);
        let loud = shouted.with_file_name(digests[0].hex()[2..].to_uppercase());
        let aside = shouted.with_file_name("renaming");
        fs::rename(&shouted, &aside).unwrap();
        fs::rename(&aside, &loud).unwrap();
        let fan = shouted.parent().unwrap();
        fs::write(fan.join("notes.txt"), b"not an object").unwrap();
        let free = (0..=255u8)
            .map(|byte| format!("{byte:02x}"))
            .find(|prefix| !fans.contains(prefix))
            .unwrap();
        fs::write(
            cache.root.join("objects").join(&free),
            b"a fan that is a file",
        )
        .unwrap();
        let listed = |threads: Threads| {
            let scan = cache.scan_objects(threads, &Stop::default()).unwrap();
            let objects: Vec<(Sha256, PathBuf, u64)> = scan
                .objects
                .into_iter()
                .map(|object| (object.digest, object.path, object.size))
                .collect();
            let unreadable: Vec<String> = scan
                .unreadable
                .iter()
                .map(|failure| failure.path.display().to_string())
                .collect();
            (objects, unreadable)
        };
        let (many, unreadable) = listed(Threads::default());
        assert_eq!(
            (many.clone(), unreadable.clone()),
            listed(Threads::default().at_most(1))
        );
        assert_eq!(many.len(), digests.len());
        for (digest, path, size) in &many {
            let expected = if *digest == digests[0] {
                loud.clone()
            } else {
                cache.object_path(digest)
            };
            assert_eq!(path, &expected);
            assert_eq!(fs::symlink_metadata(path).unwrap().len(), *size);
        }
        assert_eq!(
            unreadable,
            [cache.root.join("objects").join(&free).display().to_string()]
        );
    }

    #[cfg(unix)]
    #[test]
    fn verification_reports_directory_and_metadata_permission_errors() {
        use std::os::unix::fs::PermissionsExt;
        if rustix::process::geteuid().is_root() {
            return crate::test_skips::skip("unprivileged", "root ignores file permissions");
        }
        for (root, mode) in [(true, 0), (false, 0), (false, 0o400)] {
            let (_dir, cache) = cache();
            let stored = store(&cache, b"abc");
            let object = cache.object_path(&stored.digest);
            let folder = if root {
                cache.root.join("objects")
            } else {
                object.parent().unwrap().to_path_buf()
            };
            let before = fs::metadata(&folder).unwrap().permissions();
            fs::set_permissions(&folder, fs::Permissions::from_mode(mode)).unwrap();
            let checked = cache.verify_with_stop(1, &|_| {}, &Stop::default());
            fs::set_permissions(&folder, before).unwrap();
            let checked = checked.unwrap();
            let unreadable = if mode == 0o400 {
                object.clone()
            } else {
                folder
            };
            assert_eq!(checked.unreadable.len(), 1);
            assert_eq!(checked.unreadable[0].path, unreadable);
            assert_eq!(
                serde_json::to_value(&checked.unreadable[0]).unwrap()["kind"],
                "PermissionDenied"
            );
            assert_eq!(checked.objects, 0);
            assert_eq!(checked.changed, []);
            assert!(object.exists());
        }
    }

    #[test]
    fn gc_now_frees_an_unneeded_object_whose_time_is_ahead_of_the_clock() {
        let (_dir, cache) = cache();
        let stored = store(&cache, b"from the future");
        let object = cache.object_path(&stored.digest);
        // FAT rounds a new file's time up by as much as two seconds
        let ahead = SystemTime::now() + Duration::from_secs(2);
        let mut permissions = fs::metadata(&object).unwrap().permissions();
        #[expect(
            clippy::permissions_set_readonly_false,
            reason = "the test moves a sealed object's time"
        )]
        permissions.set_readonly(false);
        fs::set_permissions(&object, permissions).unwrap();
        fs::File::options()
            .write(true)
            .open(&object)
            .unwrap()
            .set_modified(ahead)
            .unwrap();
        let young = cache
            .sweep_with_stop(
                &Needed::default(),
                &[],
                SystemTime::now(),
                GRACE,
                Threads::default(),
                &Stop::default(),
            )
            .unwrap();
        assert_eq!(young.kept_objects, 1);
        let now = cache
            .sweep_with_stop(
                &Needed::default(),
                &[],
                SystemTime::now(),
                Duration::ZERO,
                Threads::default(),
                &Stop::default(),
            )
            .unwrap();
        assert_eq!(now.removed_objects, 1);
        assert!(!object.exists());
    }

    #[test]
    fn projects_register_once_and_can_be_forgotten() {
        let (dir, cache) = cache();
        let root = dir.path().join("thesis");
        cache.register(&root).unwrap();
        cache.register(&root).unwrap();
        assert_eq!(cache.projects(), vec![root.clone()]);
        let freed = cache
            .sweep_with_stop(
                &Needed::default(),
                std::slice::from_ref(&root),
                SystemTime::now(),
                GRACE,
                Threads::default(),
                &Stop::default(),
            )
            .unwrap();
        assert_eq!(freed.not_removed, []);
        assert_eq!(freed.forgotten, std::slice::from_ref(&root));
        assert_eq!(cache.projects(), Vec::<PathBuf>::new());
        let absent = cache
            .sweep_with_stop(
                &Needed::default(),
                &[root],
                SystemTime::now(),
                GRACE,
                Threads::default(),
                &Stop::default(),
            )
            .unwrap();
        assert_eq!(absent.not_removed, []);
        assert_eq!(absent.forgotten, Vec::<PathBuf>::new());
    }

    #[test]
    fn a_registration_is_sealed_once_flushed_and_written_again_when_it_changed() {
        let (dir, cache) = cache();
        let root = dir.path().join("thesis");
        cache.register(&root).unwrap();
        let path = cache.registration(&root);
        assert!(fs::metadata(&path).unwrap().permissions().readonly());
        crate::volume::set_read_only(&path, false).unwrap();
        fs::write(&path, b"{}").unwrap();
        cache.register(&root).unwrap();
        assert_eq!(cache.projects(), vec![root]);
        assert!(fs::metadata(&path).unwrap().permissions().readonly());
    }

    #[test]
    fn a_sweep_reports_refused_removals() {
        let (_dir, mut cache) = cache();
        if !super::super::tests::hardlinks(cache.root()) {
            return crate::test_skips::skip("hardlinks", "this disk makes no hardlinks");
        }
        let stored = store(&cache, b"cannot remove");
        let object = cache.object_path(&stored.digest);
        let leftover = cache.root.join("tmp").join("left.new");
        fs::write(&leftover, b"leftover").unwrap();
        seal(&leftover);
        cache.volume = std::sync::Arc::new(crate::volume::Volume::refusing(
            cache.root.join("tmp"),
            true,
        ));
        let freed = cache.sweep(
            &Needed::default(),
            SystemTime::now() + GRACE + Duration::from_secs(60),
        );
        assert_eq!(
            (
                freed.removed_objects,
                freed.removed_bytes,
                freed.removed_leftovers
            ),
            (0, 0, 0)
        );
        let report = serde_json::to_value(&freed).unwrap();
        let paths = report["not_removed"].as_array().unwrap();
        assert_eq!(paths.len(), 2);
        assert!(
            paths
                .iter()
                .any(|failure| failure["path"] == serde_json::json!(object))
        );
        assert!(
            paths
                .iter()
                .any(|failure| failure["path"] == serde_json::json!(leftover))
        );
        assert!(object.exists());
        assert!(leftover.exists());
    }

    /// a download a later run finished is dated back to its first run's start, so a sweep ages an
    /// object by its ctime, which its seal, publishing or a link moves.
    #[cfg(unix)]
    #[test]
    fn an_object_dated_back_to_its_birth_still_gets_a_day() {
        let (_dir, cache) = cache();
        let stored = store(&cache, b"resumed");
        let object = cache.object_path(&stored.digest);
        let began = SystemTime::now() - GRACE - Duration::from_hours(24);
        fs::File::open(&object)
            .unwrap()
            .set_modified(began)
            .unwrap();
        let freed = cache.sweep(&Needed::default(), SystemTime::now());
        assert_eq!(freed.removed_objects, 0);
        assert!(object.exists());
    }

    #[test]
    fn a_sweep_keeps_what_is_needed_and_what_is_new() {
        for (_dir, cache) in [cache(), fat()] {
            let needed = store(&cache, b"needed");
            let unneeded = store(&cache, b"unneeded");
            let later = SystemTime::now() + GRACE + Duration::from_secs(60);
            let keep = Needed {
                objects: HashSet::from([needed.digest]),
                ..Needed::default()
            };
            assert_eq!(
                cache.sweep(&keep, SystemTime::now()).removed_objects,
                0,
                "nothing is old yet"
            );
            let freed = cache.sweep(&keep, later);
            assert_eq!((freed.removed_objects, freed.removed_bytes), (1, 8));
            assert!(cache.find(&needed.digest, Some(6)).is_some());
            assert!(cache.find(&unneeded.digest, Some(8)).is_none());
        }
    }

    #[test]
    fn a_sweep_removes_what_stopped_runs_left_sealed() {
        for (_dir, cache) in [cache(), fat()] {
            // a download sealed by a run that died while publishing it, and a member sealed by
            // one that died while unpacking
            let key = "ab".repeat(32);
            let partial = cache.partial_folder(&key);
            fs::create_dir_all(&partial).unwrap();
            fs::write(partial.join(format!("{key}.lock")), b"").unwrap();
            fs::write(partial.join(format!("{key}.json")), b"{}").unwrap();
            let sealed = [
                partial.join(format!("{key}.part")),
                cache.root.join("tmp").join("0123456789abcdef.new"),
            ];
            for path in &sealed {
                fs::write(path, b"abc").unwrap();
                seal(path);
            }
            let later = SystemTime::now() + GRACE + Duration::from_secs(60);
            assert_eq!(cache.sweep(&Needed::default(), later).removed_leftovers, 3);
            assert!(sealed.iter().all(|path| !path.exists()));
            let left: Vec<String> = fs::read_dir(partial)
                .unwrap()
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect();
            assert_eq!(left, Vec::<String>::new(), "nothing holds the lock");
        }
    }

    #[test]
    fn a_sweep_keeps_the_lock_of_a_download_in_progress_and_drops_finished_ones() {
        let (_dir, cache) = cache();
        let running = cache.claim("https://example.org/running").unwrap();
        let lock = |url: &str| {
            let key = digest::sha256(url.as_bytes()).hex();
            cache.partial_folder(&key).join(format!("{key}.lock"))
        };
        let finished = lock("https://example.org/finished");
        fs::create_dir_all(finished.parent().unwrap()).unwrap();
        fs::write(&finished, b"").unwrap();
        let freed = cache.sweep(&Needed::default(), SystemTime::now());
        assert_eq!(freed.removed_leftovers, 0, "a lock file frees nothing");
        assert!(lock("https://example.org/running").exists());
        assert!(!lock("https://example.org/finished").exists());
        drop(running);
    }

    #[test]
    fn unfinished_downloads_in_shards_are_counted_and_swept() {
        let (_dir, cache) = cache();
        let url = "https://example.org/sharded";
        let crate::cache::Claim::Owned(partial) = cache.claim(url).unwrap() else {
            panic!("nothing else holds the download")
        };
        let key = digest::sha256(url.as_bytes()).hex();
        let shard = cache.root.join("partial").join(&key[..1]);
        assert!(shard.join(format!("{key}.lock")).is_file());
        let mut writer = partial
            .writer(
                0,
                Some(6),
                crate::cache::Validators::default(),
                false,
                false,
            )
            .unwrap();
        writer.write_all(b"abc").unwrap();
        writer.save().unwrap();
        drop(writer);
        let part = shard.join(format!("{key}.part"));
        assert!(part.is_file());
        let unfinished = cache.unfinished();
        assert_eq!(
            (unfinished.downloads, unfinished.bytes),
            (1, on_disk(&fs::metadata(&part).unwrap()))
        );
        drop(partial);
        let later = SystemTime::now() + GRACE + Duration::from_secs(60);
        assert_eq!(
            cache.sweep(&Needed::default(), later).removed_leftovers,
            2,
            "the bytes and the record that lets them resume"
        );
        assert_eq!(fs::read_dir(&shard).unwrap().count(), 0);
        assert_eq!(cache.unfinished().downloads, 0);
    }

    /// a split download's file has holes, so what unfinished downloads hold, and what gc frees of
    /// them, is what the disk holds, not their files' length.
    #[test]
    fn unfinished_downloads_count_what_the_disk_holds() {
        let (_dir, cache) = cache();
        let key = "ab".repeat(32);
        let shard = cache.partial_folder(&key);
        fs::create_dir_all(&shard).unwrap();
        let part = shard.join(format!("{key}.part"));
        let record = shard.join(format!("{key}.json"));
        let mut file = fs::File::create(&part).unwrap();
        file.write_all(b"abc").unwrap();
        file.set_len(64 << 20).unwrap();
        drop(file);
        fs::write(&record, b"{}").unwrap();
        fs::write(shard.join(format!("{key}.lock")), b"").unwrap();
        #[cfg(unix)]
        let held = |path: &Path| {
            use std::os::unix::fs::MetadataExt;
            fs::metadata(path).unwrap().blocks() * 512
        };
        #[cfg(unix)]
        let (part_held, record_held) = (held(&part), held(&record));
        let unfinished = cache.unfinished();
        let later = SystemTime::now() + GRACE + Duration::from_secs(60);
        let freed = cache.sweep(&Needed::default(), later);
        assert_eq!((unfinished.downloads, freed.removed_leftovers), (1, 2));
        // windows makes no holes in these files, so there their length is what the disk holds
        #[cfg(unix)]
        assert_eq!(
            (unfinished.bytes, freed.removed_leftover_bytes),
            (part_held, part_held + record_held)
        );
    }

    /// only the 16 shards under `partial/` count, so a file at its top, in a folder that is no
    /// shard or behind a link in a shard's place, is neither counted nor freed.
    #[test]
    fn only_the_shards_under_partial_hold_unfinished_downloads() {
        let (_dir, cache) = cache();
        let partial = cache.root.join("partial");
        let key = "ab".repeat(32);
        let mut kept = Vec::new();
        for folder in [partial.clone(), partial.join("ab")] {
            fs::create_dir_all(&folder).unwrap();
            for extension in ["part", "json", "lock"] {
                let path = folder.join(format!("{key}.{extension}"));
                fs::write(&path, b"abc").unwrap();
                kept.push(path);
            }
        }
        #[cfg(unix)]
        {
            let outside = cache.root.with_file_name("outside");
            fs::create_dir(&outside).unwrap();
            for extension in ["part", "json", "lock"] {
                let path = outside.join(format!("{key}.{extension}"));
                fs::write(&path, b"abc").unwrap();
                kept.push(path);
            }
            std::os::unix::fs::symlink(&outside, partial.join("a")).unwrap();
        }
        assert_eq!(cache.partial_folders(), Vec::<PathBuf>::new());
        assert_eq!(cache.unfinished(), Unfinished::default());
        let later = SystemTime::now() + GRACE + Duration::from_secs(60);
        assert_eq!(cache.sweep(&Needed::default(), later).removed_leftovers, 0);
        assert!(kept.iter().all(|path| path.exists()));
    }

    #[test]
    fn a_sweep_removes_unlocked_staging_without_waiting() {
        for (_dir, cache) in [cache(), fat()] {
            let folder = cache.root().join("tmp").join("0123456789abcdef.stage");
            fs::create_dir_all(folder.join("03")).unwrap();
            fs::write(folder.join("lock"), b"").unwrap();
            let member = folder.join("03/member.new");
            fs::write(&member, b"unfinished").unwrap();
            seal(&member);
            let freed = cache.sweep(&Needed::default(), SystemTime::now());
            assert_eq!(freed.removed_leftovers, 1);
            assert!(!folder.exists());
        }
    }

    #[test]
    fn staging_without_locking_keeps_the_age_rule() {
        for (_dir, cache) in [cache(), fat()] {
            let folder = cache.root().join("tmp").join("0123456789abcdef.stage");
            fs::create_dir_all(folder.join("03")).unwrap();
            fs::write(folder.join("unlocked"), b"").unwrap();
            let member = folder.join("03/member.new");
            fs::write(&member, b"unfinished").unwrap();
            let now = SystemTime::now();
            let written = now + GRACE;
            fs::OpenOptions::new()
                .write(true)
                .open(&member)
                .unwrap()
                .set_modified(written)
                .unwrap();
            seal(&member);
            assert_eq!(cache.sweep(&Needed::default(), now).removed_leftovers, 0);
            assert_eq!(
                cache
                    .sweep(&Needed::default(), written + Duration::from_secs(60))
                    .removed_leftovers,
                0,
                "a recent write keeps a member in an older folder"
            );
            assert!(member.exists());
            let later = written + GRACE + Duration::from_secs(60);
            assert_eq!(cache.sweep(&Needed::default(), later).removed_leftovers, 1);
            assert!(!folder.exists());
        }
    }

    #[test]
    fn a_run_that_opened_a_lock_before_a_sweep_removed_it_never_owns_the_download_twice() {
        let (_dir, cache) = cache();
        let url = "https://example.org/raced";
        let key = digest::sha256(url.as_bytes()).hex();
        let lock = cache.partial_folder(&key).join(format!("{key}.lock"));
        fs::create_dir_all(cache.partial_folder(&key)).unwrap();
        // a run waiting for the download opens the lock, gc sweeps, then the run takes the lock
        let opened = super::super::partial::open_unfollowed(&lock, true).unwrap();
        cache.sweep(
            &Needed::default(),
            SystemTime::now() + GRACE + Duration::from_secs(60),
        );
        // fat and exfat keep a removed file's name until its last handle closes, so there the
        // new claim is refused instead; either way no two runs own the download
        let claimed = cache.claim(url);
        let old = super::super::partial::take_lock(&lock, opened);
        match claimed {
            Ok(super::super::Claim::Owned(_owner)) => assert!(
                matches!(
                    old,
                    Err(std::fs::TryLockError::WouldBlock | std::fs::TryLockError::Error(_))
                ),
                "a second run owns the same download"
            ),
            Ok(super::super::Claim::Busy) => {}
            Err(err) => assert!(
                cfg!(windows) && err.kind() == io::ErrorKind::PermissionDenied,
                "{err}"
            ),
        }
    }

    #[test]
    fn a_sweep_keeps_the_records_of_archives_in_use() {
        let (_dir, cache) = cache();
        let used = digest::sha256(b"used archive");
        let unused = digest::sha256(b"unused archive");
        for archive in [used, unused] {
            cache.record_unpacked(&archive, &[]).unwrap();
        }
        let keep = Needed {
            unpacked: HashSet::from([used]),
            ..Needed::default()
        };
        cache.sweep(&keep, SystemTime::now() + GRACE + Duration::from_secs(60));
        assert_eq!(cache.unpacked_archives(), [used]);
    }

    /// a result's record stays while pinned or for a day, as a failed build's does; others go, and
    /// with a key's last record its lock and its folder.
    #[test]
    fn a_sweep_removes_old_records_no_project_pins() {
        let (_dir, cache) = cache();
        let member = |path: &str| crate::cache::Member {
            path: path.to_owned(),
            size: 1,
            sha256: digest::sha256(path.as_bytes()),
        };
        let (kept, other) = (digest::sha256(b"kept key"), digest::sha256(b"other key"));
        let pinned = cache.record_result(&kept, &[member("a.csv")]).unwrap();
        let unpinned = cache.record_result(&kept, &[member("b.csv")]).unwrap();
        cache.record_result(&other, &[member("c.csv")]).unwrap();
        for key in [kept, other] {
            drop(cache.hold_key(&key, &Stop::default(), || {}).unwrap());
        }
        let keep = Needed {
            results: HashSet::from([(kept, pinned)]),
            ..Needed::default()
        };
        let builds = cache.root().join("builds");
        cache.sweep(&keep, SystemTime::now());
        assert!(builds.join(kept.hex()).join(unpinned.hex()).is_file());
        assert!(builds.join(other.hex()).is_dir());
        cache.sweep(&keep, SystemTime::now() + GRACE + Duration::from_secs(60));
        assert!(builds.join(kept.hex()).join(pinned.hex()).is_file());
        assert!(!builds.join(kept.hex()).join(unpinned.hex()).exists());
        assert!(builds.join(kept.hex()).join("lock").is_file());
        assert!(!builds.join(other.hex()).exists());
    }

    #[test]
    fn a_sweep_forgets_links_whose_bytes_are_gone() {
        let (_dir, cache) = cache();
        let held = store(&cache, b"held");
        let record = |url: &str, sha256| super::super::UrlRecord {
            url: url.to_owned(),
            sha256,
            size: 4,
            name: None,
            validators: super::super::Validators::default(),
        };
        cache
            .save_url_record(&record("https://example.org/held", held.digest))
            .unwrap();
        cache
            .save_url_record(&record("https://example.org/gone", digest::sha256(b"gone")))
            .unwrap();
        let keep = Needed {
            objects: HashSet::from([held.digest]),
            ..Needed::default()
        };
        cache.sweep(&keep, SystemTime::now());
        assert!(cache.url_record("https://example.org/held").is_some());
        assert!(cache.url_record("https://example.org/gone").is_none());
    }

    #[test]
    fn a_sweep_keeps_the_link_of_an_archive_whose_files_are_unpacked() {
        let (_dir, cache) = cache();
        let member = store(&cache, b"member");
        let archive = digest::sha256(b"archive dropped after unpacking");
        cache
            .record_unpacked(
                &archive,
                &[super::super::Member {
                    path: "a.csv".to_owned(),
                    sha256: member.digest,
                    size: member.size,
                }],
            )
            .unwrap();
        cache
            .save_url_record(&super::super::UrlRecord {
                url: "https://example.org/data.zip".to_owned(),
                sha256: archive,
                size: 99,
                name: None,
                validators: super::super::Validators::default(),
            })
            .unwrap();
        let keep = Needed {
            objects: HashSet::from([member.digest]),
            unpacked: HashSet::from([archive]),
            ..Needed::default()
        };
        cache.sweep(&keep, SystemTime::now() + GRACE + Duration::from_secs(60));
        assert!(
            cache.url_record("https://example.org/data.zip").is_some(),
            "the link's record serves its unpacked files"
        );
    }

    #[test]
    fn verifying_marks_objects_whose_bytes_changed() {
        let (_dir, cache) = cache();
        let good = store(&cache, b"good");
        let bad = store(&cache, b"bad!");
        let mut permissions = fs::metadata(cache.object_path(&bad.digest))
            .unwrap()
            .permissions();
        #[expect(
            clippy::permissions_set_readonly_false,
            reason = "the test damages an object on purpose"
        )]
        permissions.set_readonly(false);
        fs::set_permissions(cache.object_path(&bad.digest), permissions).unwrap();
        fs::OpenOptions::new()
            .write(true)
            .open(cache.object_path(&bad.digest))
            .unwrap()
            .write_all(b"BAD!")
            .unwrap();
        let seen = AtomicUsize::new(0);
        let checked = cache.verify_all(3, &|bytes| {
            seen.fetch_add(usize::try_from(bytes).unwrap(), Ordering::Relaxed);
        });
        assert_eq!(checked.objects, 2);
        assert_eq!(checked.changed, vec![bad.digest]);
        assert_eq!(seen.load(Ordering::Relaxed), 8);
        assert!(cache.find(&good.digest, Some(4)).is_some());
        assert!(cache.find(&bad.digest, Some(4)).is_none());
        assert_eq!(
            cache.changed().into_keys().collect::<Vec<_>>(),
            [bad.digest],
            "projects linking to it can learn it changed"
        );
    }

    /// a bar sized from the scan moves as each block of a large object is read, never only once
    /// the whole object is hashed.
    #[test]
    fn verifying_tells_the_bytes_of_each_block_it_reads() {
        let (_dir, cache) = cache();
        let block = 1024 * 1024;
        store(&cache, &vec![7; 2 * block + 3]);
        let told = Mutex::new(Vec::new());
        let checked = cache.verify_all(1, &|bytes| told.lock().unwrap().push(bytes));
        assert_eq!(checked.changed, []);
        let block = block as u64;
        assert_eq!(told.into_inner().unwrap(), [block, block, 3]);
    }

    #[test]
    fn unreadable_objects_are_kept_without_a_change_mark() {
        let (_dir, cache) = cache();
        let stored = store(&cache, b"intact");
        let object = cache.object_path(&stored.digest);
        #[cfg(windows)]
        let held = {
            use std::os::windows::fs::OpenOptionsExt;
            const FILE_SHARE_DELETE: u32 = 4;
            fs::OpenOptions::new()
                .read(true)
                .share_mode(FILE_SHARE_DELETE)
                .open(&object)
                .unwrap()
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if rustix::process::geteuid().is_root() {
                return crate::test_skips::skip("unprivileged", "root ignores file permissions");
            }
            fs::set_permissions(&object, fs::Permissions::from_mode(0o000)).unwrap();
        }
        assert!(cache.verify(&stored.digest).is_err());
        assert!(object.exists());
        assert!(cache.changed().is_empty());
        let checked = cache.verify_all(2, &|_| {});
        #[cfg(windows)]
        drop(held);
        assert!(
            checked.changed.is_empty(),
            "a read error is not changed bytes"
        );
        assert_eq!(checked.stuck, vec![]);
        assert!(object.exists(), "unreadable objects must stay in the cache");
        assert!(cache.changed().is_empty());
        assert_eq!(checked.objects, 1);
        assert_eq!(checked.bytes, 6);
        assert_eq!(
            serde_json::to_value(&checked).unwrap()["unreadable"][0]["path"],
            serde_json::json!(object)
        );
    }

    #[test]
    fn a_changed_object_held_open_is_still_marked() {
        let (_dir, cache) = cache();
        let bad = store(&cache, b"bad!");
        let object = cache.object_path(&bad.digest);
        damage(&object);
        let held = hold(&object);
        let checked = cache.verify_all(2, &|_| {});
        drop(held);
        assert_eq!(checked.changed, vec![bad.digest]);
        assert_eq!(checked.stuck, []);
        assert_eq!(fs::read(&object).unwrap(), b"BAD!");
    }

    #[test]
    fn objects_whose_bytes_rotted_wait_for_replacement() {
        for (_dir, cache) in [cache(), fat()] {
            let rotten = [store(&cache, b"rot1"), store(&cache, b"rot2")];
            for object in &rotten {
                damage(&cache.object_path(&object.digest));
                // bytes that rot on disk leave the read-only flag as it was
                seal(&cache.object_path(&object.digest));
            }
            assert!(!cache.verify(&rotten[0].digest).unwrap());
            let checked = cache.verify_all(2, &|_| {});
            assert_eq!(checked.changed.len(), 2);
            assert!(
                rotten
                    .iter()
                    .all(|object| checked.changed.contains(&object.digest))
            );
            assert_eq!(checked.stuck, vec![]);
            assert!(
                rotten
                    .iter()
                    .all(|object| cache.object_path(&object.digest).exists())
            );
        }
    }

    fn damage(path: &Path) {
        let mut permissions = fs::metadata(path).unwrap().permissions();
        #[expect(
            clippy::permissions_set_readonly_false,
            reason = "the test damages an object on purpose"
        )]
        permissions.set_readonly(false);
        fs::set_permissions(path, permissions).unwrap();
        fs::OpenOptions::new()
            .write(true)
            .open(path)
            .unwrap()
            .write_all(b"BAD!")
            .unwrap();
    }
}
