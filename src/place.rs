//! putting cached objects where a project expects them, by reflink, else hardlink, else copy.

#[cfg(unix)]
mod parents;
#[cfg(windows)]
#[path = "place/parents_windows.rs"]
mod parents;
mod schedule;
mod synced;

use parents::{Location, Parents, Staged};

use std::collections::{HashMap, HashSet};
use std::fs::{self, File, Metadata, Permissions};
use std::io::{self, Read, Write};
use std::num::NonZero;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use serde::{Deserialize, Serialize};

use crate::cache::read_unfollowed;
use crate::exp::Experiments;
use crate::stop::Stop;
use crate::threads::Threads;
#[cfg(windows)]
use crate::volume::Volume;
use crate::volume::{crosses_volumes, is_plain, refuses_links};

/// the most threads that place files or look up objects at once. creating a link or opening a
/// file waits on the disk and, on windows, on the antivirus scanner, so a few threads hide each
/// other's waits. placing 10,000 files on windows took 7.5 s on four threads and 5.1 s on eight,
/// and sixteen gained little more.
const THREADS: usize = 8;

/// the fewest items worth a thread of their own, since starting a thread costs more than a few
/// calls.
const PER_THREAD: usize = 64;

/// how a file was placed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Method {
    /// a new file that shares the object's blocks until either one changes.
    Reflink,
    /// a second name for the object itself.
    Hardlink,
    /// a new file holding its own copy of the object's bytes.
    Copy,
}

/// puts objects into one folder tree, and remembers which methods its filesystem refuses.
pub(crate) struct Placer {
    exp: Experiments,
    threads: Threads,
    stop: Arc<Stop>,
    root: PathBuf,
    /// whether the root itself must be a real folder, as a project's data folder must. a folder
    /// someone named for `get` may be a link to one.
    checks_root: bool,
    /// folders under the root already seen to be real folders, not links.
    folders: HashSet<PathBuf>,
    synced: Option<&'static str>,
    can_reflink: AtomicBool,
    can_hardlink: AtomicBool,
    other_volume: AtomicBool,
    /// the volume under the root, which may refuse to remove a read-only file.
    #[cfg(windows)]
    volume: Volume,
    parents: Parents,
    links: OnceLock<bool>,
    serial: OnceLock<bool>,
    #[cfg(test)]
    observed: Option<Box<Observation>>,
    /// on linux 5.8 and later, the root's filesystem, opened before the first copy so one syncfs
    /// flushes every copy and reports a write-back that failed since. a flush per copy cost 33
    /// times the placing of 10,000 small copies.
    #[cfg(target_os = "linux")]
    batch: std::sync::OnceLock<Option<File>>,
    #[cfg(target_os = "linux")]
    unflushed: AtomicBool,
    #[cfg(target_os = "macos")]
    copy_flush: std::sync::Mutex<Option<File>>,
}

#[cfg(test)]
type Observation = dyn Fn(&Metadata) + Send + Sync;

/// one file to place: the object, where it goes, and whether it must be a writable copy.
pub(crate) struct Order {
    pub(crate) object: PathBuf,
    pub(crate) destination: PathBuf,
    pub(crate) writable: bool,
}

/// what putting one file in place did.
enum Put<T> {
    /// placed it by a method, with what was kept of its metadata.
    Placed(Method, T),
    /// copied it under a hidden name, which it trades for its own once the batch is flushed.
    Waiting(Staged),
}

impl<T> Put<T> {
    fn method(&self) -> Method {
        match self {
            Put::Placed(method, _) => *method,
            Put::Waiting(_) => Method::Copy,
        }
    }
}

/// why a placer copied files it would rather have linked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Unlinked {
    /// the root sits inside a folder this sync client owns, which breaks links: "onedrive",
    /// "dropbox" or "icloud drive".
    Synced(&'static str),
    /// hardlinks failed because the cache sits on another drive, which a cache on the same drive
    /// would fix.
    OtherDrive,
    /// the drive refused reflinks and hardlinks alike, as FAT and exFAT drives do.
    NoLinks,
}

impl Unlinked {
    /// explains why the named files were copied and how another placement may allow links.
    pub(crate) fn copied_because(self, place: impl std::fmt::Display, subject: &str) -> String {
        match self {
            Self::Synced(client) => format!(
                "{place} is inside {client}, which breaks links, so {subject} copied. a folder outside {client} lets fetchloom try links"
            ),
            Self::NoLinks => {
                format!("the drive holding {place} cannot link files, so {subject} copied")
            }
            Self::OtherDrive => format!(
                "the cache is on another drive, so {subject} copied. set FETCHLOOM_CACHE to a folder on this drive to let fetchloom try links"
            ),
        }
    }
}

impl Placer {
    /// prepares placement under `root`, which must be a real folder, and notes whether `root`
    /// sits inside a folder a sync client owns. capability probes use the opened root too.
    pub(crate) fn new(root: &Path) -> Placer {
        let root = std::path::absolute(root).unwrap_or_else(|_| root.to_path_buf());
        let synced = synced::client(&root, &|name| std::env::var_os(name));
        if let Some(client) = synced {
            tracing::debug!(root = %root.display(), client, "inside a synced folder, so no hardlinks");
        }
        Placer::starting_at(root, Method::Reflink, synced)
    }

    /// prepares placement under a folder someone named, which may be a link to a folder. the
    /// folders below it must still be real. it is the one folder fetchloom writes to on its
    /// volume, so a removal tests there, with a hidden file, whether the volume can hardlink.
    pub(crate) fn in_named_folder(root: &Path) -> Placer {
        Placer {
            checks_root: false,
            ..Placer::new(root)
        }
    }

    /// a placer that skips the methods above `first`, as if the filesystem had refused them.
    fn starting_at(root: PathBuf, first: Method, synced: Option<&'static str>) -> Placer {
        Placer {
            #[cfg(windows)]
            volume: Volume::new(root.clone()),
            parents: Parents::default(),
            links: OnceLock::new(),
            serial: OnceLock::new(),
            #[cfg(test)]
            observed: None,
            threads: Threads::default(),
            exp: Experiments::default(),
            stop: Arc::default(),
            root,
            checks_root: true,
            folders: HashSet::new(),
            synced,
            can_reflink: AtomicBool::new(first == Method::Reflink),
            can_hardlink: AtomicBool::new(first != Method::Copy),
            other_volume: AtomicBool::new(false),
            #[cfg(target_os = "linux")]
            batch: std::sync::OnceLock::new(),
            #[cfg(target_os = "linux")]
            unflushed: AtomicBool::new(false),
            #[cfg(target_os = "macos")]
            copy_flush: std::sync::Mutex::new(None),
        }
    }

    /// uses the cache's experiment settings for this placement.
    pub(crate) fn with_experiments(mut self, exp: Experiments) -> Placer {
        self.exp = exp;
        self
    }

    /// the experiment settings supplied for this placement.
    #[allow(
        dead_code,
        reason = "shared plumbing stays until all adoption changes are merged"
    )]
    pub(crate) fn experiments(&self) -> &Experiments {
        &self.exp
    }

    pub(crate) fn with_threads(mut self, threads: Threads) -> Placer {
        self.threads = threads;
        self
    }

    #[cfg(test)]
    pub(crate) fn observing(
        mut self,
        observed: impl Fn(&Metadata) + Send + Sync + 'static,
    ) -> Self {
        self.observed = Some(Box::new(observed));
        self
    }

    pub(crate) fn with_stop(mut self, stop: Arc<Stop>) -> Placer {
        self.stop = stop;
        self
    }

    pub(crate) fn stop(&self) -> &Stop {
        &self.stop
    }

    /// why files were copied rather than linked, as far as the filesystem told.
    pub(crate) fn unlinked(&self) -> Option<Unlinked> {
        if let Some(client) = self.synced {
            return Some(Unlinked::Synced(client));
        }
        if self.other_volume.load(Ordering::Relaxed) {
            return Some(Unlinked::OtherDrive);
        }
        let refused = |method: &AtomicBool| !method.load(Ordering::Relaxed);
        (refused(&self.can_reflink) && refused(&self.can_hardlink)).then_some(Unlinked::NoLinks)
    }

    /// puts `object` at `destination`, which must not exist yet. creates missing parent folders.
    /// `writable` asks for a writable file, for tools that write into their inputs.
    #[cfg(test)]
    pub(crate) fn place(
        &mut self,
        object: &Path,
        destination: &Path,
        writable: bool,
    ) -> io::Result<Method> {
        self.check_folders(destination, true)?;
        match self.put(object, destination, writable, false)? {
            Put::Placed(method, _) => Ok(method),
            Put::Waiting(staged) => staged.publish().map(|_| Method::Copy),
        }
    }

    /// places every order and says how each went, in order, with what `seen` keeps of the metadata
    /// of each placed file, read right after it was placed, so a later edit is never taken for the
    /// placed file. the first goes alone, so it learns which methods the filesystem allows before
    /// the rest go several at once. orders are rebuilt for checks and placement so only active
    /// workers keep their paths. copies flushed as a batch take their names only after the flush.
    pub(crate) fn place_all<T: Send>(
        &mut self,
        count: usize,
        order: impl Fn(usize) -> Order + Sync,
        seen: impl Fn(&Metadata) -> T + Sync,
    ) -> Vec<io::Result<(Method, T)>> {
        let mut results = Vec::with_capacity(count);
        let mut folders = schedule::Folders::new(count);
        let ready = spread((0..count).filter_map(|index| {
            if let Err(err) = check_stop(&self.stop) {
                results.push((index, Err(err)));
                return None;
            }
            let order = order(index);
            match self.check_folders(&order.destination, true) {
                Ok(()) => {
                    folders.record(index, &order.destination);
                    Some((index, order))
                }
                Err(err) => {
                    results.push((index, Err(err)));
                    None
                }
            }
        }));
        let this = &*self;
        let observe = |metadata: &Metadata| {
            #[cfg(test)]
            if let Some(observed) = &this.observed {
                observed(metadata);
            }
            seen(metadata)
        };
        let serial = !ready.is_empty()
            && *this.serial.get_or_init(|| {
                this.location(&this.root.join(".filesystem"), false)
                    .is_ok_and(|location| location.serializes())
            });
        let grouped = folders.finish(serial);
        let threads = grouped.threads(this.threads);
        let batch = this.batches_copies();
        // keeping a whole Metadata for each of 100,000 files cost tens of megabytes
        let put = |index: &usize| {
            let _permit = grouped.enter(*index);
            let order = order(*index);
            let placed = this
                .put(&order.object, &order.destination, order.writable, batch)
                .map(|put| match put {
                    Put::Placed(method, metadata) => Put::Placed(method, observe(&metadata)),
                    Put::Waiting(staged) => Put::Waiting(staged),
                });
            (*index, placed)
        };
        let mut done = Vec::with_capacity(ready.len());
        if let Some((first, rest)) = ready.split_first() {
            let placed = put(first);
            if let (_, Ok(placed)) = &placed {
                tracing::debug!(
                    method = ?placed.method(),
                    files = ready.len(),
                    "placed the first file, the rest try the same methods"
                );
            }
            done.push(placed);
            done.extend(run_all(rest, threads, put));
        }
        let mut waiting = Vec::new();
        for (index, placed) in done {
            match placed {
                Ok(Put::Placed(method, seen)) => results.push((index, Ok((method, seen)))),
                Ok(Put::Waiting(staged)) => waiting.push((index, staged)),
                Err(err) => results.push((index, Err(err))),
            }
        }
        results.extend(publish_flushed(
            waiting,
            || this.flush_copies(),
            threads,
            &observe,
            &this.stop,
            &grouped,
        ));
        results.sort_unstable_by_key(|(index, _)| *index);
        results.into_iter().map(|(_, result)| result).collect()
    }

    /// tries each method the filesystem may allow, best first. with `batch`, a copy is left
    /// under its hidden name until [`Placer::flush_copies`] has flushed it.
    fn put(
        &self,
        object: &Path,
        destination: &Path,
        writable: bool,
        batch: bool,
    ) -> io::Result<Put<Metadata>> {
        check_stop(&self.stop)?;
        let location = Arc::new(self.location(destination, false)?);
        let early_hardlink = cfg!(windows)
            && !writable
            && self.synced.is_none()
            && self.can_hardlink.load(Ordering::Relaxed);
        if early_hardlink {
            check_object(object)?;
            match location.hard_link(object) {
                Ok(metadata) => return Ok(Put::Placed(Method::Hardlink, metadata)),
                Err(error) => self.learn(Method::Hardlink, error)?,
            }
        }
        if self.can_reflink.load(Ordering::Relaxed) {
            check_object(object)?;
            match reflink(object, &location, writable) {
                Ok(sealed) => return Ok(Put::Placed(Method::Reflink, sealed?)),
                Err(error) => self.learn(Method::Reflink, error)?,
            }
        }
        // sync clients break hardlinks or upload every link as a full file.
        if !early_hardlink
            && self.can_hardlink.load(Ordering::Relaxed)
            && !writable
            && self.synced.is_none()
        {
            check_object(object)?;
            match location.hard_link(object) {
                Ok(metadata) => return Ok(Put::Placed(Method::Hardlink, metadata)),
                Err(error) => self.learn(Method::Hardlink, error)?,
            }
        }
        // a nested mount is not covered by the root filesystem's batch flush.
        let flush = !self.batches_copies();
        #[cfg(unix)]
        let flush = flush || location.other_device();
        #[cfg(target_os = "macos")]
        let flush = flush || !batch;
        let put = if batch && !flush {
            let (staged, file) =
                stage_copy_at(object, &location, writable, self.threads, &self.stop)?;
            #[cfg(target_os = "macos")]
            {
                rustix::fs::fsync(&file)?;
                let mut first = self
                    .copy_flush
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if first.is_none() {
                    *first = Some(file.try_clone()?);
                }
            }
            drop(file);
            Put::Waiting(staged)
        } else {
            let metadata = copy_at(object, &location, writable, self.threads, flush, &self.stop)?;
            Put::Placed(Method::Copy, metadata)
        };
        #[cfg(target_os = "linux")]
        if !flush {
            self.unflushed.store(true, Ordering::Relaxed);
        }
        Ok(put)
    }

    /// whether copies may skip their own flush, because [`Placer::flush_copies`] flushes them all.
    #[cfg(target_os = "linux")]
    fn batches_copies(&self) -> bool {
        self.batch
            .get_or_init(|| {
                let release = fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default();
                crate::cache::syncfs_reports_errors(&release)
                    .then(|| {
                        self.location(&self.root.join(".batch"), false)
                            .and_then(|location| location.flush_handle())
                            .ok()
                    })
                    .flatten()
            })
            .is_some()
    }

    #[cfg(target_os = "macos")]
    #[expect(
        clippy::unused_self,
        reason = "macos batches copies before publication"
    )]
    fn batches_copies(&self) -> bool {
        true
    }

    #[cfg(windows)]
    #[expect(
        clippy::unused_self,
        reason = "windows flushes each file before naming it"
    )]
    fn batches_copies(&self) -> bool {
        false
    }

    /// flushes the copies placed without their own flush, so what is recorded next holds only
    /// bytes on disk.
    #[cfg(target_os = "linux")]
    pub(crate) fn flush_copies(&self) -> io::Result<()> {
        if self.unflushed.load(Ordering::Relaxed)
            && let Some(Some(root)) = self.batch.get()
        {
            rustix::fs::syncfs(root)?;
            self.unflushed.store(false, Ordering::Relaxed);
        }
        Ok(())
    }

    #[cfg(target_os = "macos")]
    pub(crate) fn flush_copies(&self) -> io::Result<()> {
        let first = self
            .copy_flush
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(first) = first {
            match rustix::fs::fcntl_fullfsync(&first) {
                Err(rustix::io::Errno::NOTSUP | rustix::io::Errno::INVAL) => {}
                result => result?,
            }
        }
        Ok(())
    }

    #[cfg(windows)]
    #[expect(
        clippy::unused_self,
        clippy::unnecessary_wraps,
        reason = "windows flushes each copy as it is made"
    )]
    pub(crate) fn flush_copies(&self) -> io::Result<()> {
        Ok(())
    }

    /// checks every folder between the root and `path`, refusing a link among them, so a link
    /// inside the data folder can never send a file elsewhere. with `make`, missing folders are
    /// made one at a time. each folder is checked once.
    pub(crate) fn check_folders(&mut self, path: &Path, make: bool) -> io::Result<()> {
        check_stop(&self.stop)?;
        let (_, parent) = self.within(path)?;
        if !self.folders.contains(&parent) {
            if let Err(error) = self.location(path, make) {
                if let Ok(metadata) = fs::symlink_metadata(&parent) {
                    found_folder(&parent, &metadata, FolderUse::Place { make: false })?;
                }
                return Err(error);
            }
            self.folders.insert(parent);
        }
        Ok(())
    }

    /// removes a file fetchloom placed, by `placed` when fetchloom recorded how. every folder on
    /// its way is checked again first, since a deletion must not trust a check made before
    /// downloads ran. with `prune`, folders it leaves empty go too, up to the root. a file
    /// recorded as a hardlink is the cached object under another name, so it never has its
    /// read-only flag cleared; any other follows the volume's rule for read-only files.
    pub(crate) fn remove(
        &mut self,
        path: &Path,
        prune: bool,
        placed: Option<Method>,
    ) -> io::Result<()> {
        check_stop(&self.stop)?;
        let (path, parent) = self.within(path)?;
        let location = self.location(&path, false)?;
        #[cfg(unix)]
        {
            let _ = placed;
            location.remove_file()?;
        }
        #[cfg(windows)]
        self.volume.set_scratch(location.root_path());
        #[cfg(windows)]
        if placed == Some(Method::Hardlink) {
            self.volume.remove_hardlink(location.path())?;
        } else {
            self.volume.remove(location.path())?;
        }
        drop(location);
        let mut folder = Some(parent.as_path());
        while prune && let Some(current) = folder {
            if current == self.root {
                break;
            }
            check_stop(&self.stop)?;
            let removed = self
                .location(current, false)
                .and_then(|location| location.remove_dir());
            if removed.is_err() {
                break;
            }
            self.folders.remove(current);
            folder = current.parent();
        }
        Ok(())
    }

    /// checks a parent without caching the result or creating anything.
    pub(crate) fn check_parent(&self, path: &Path) -> io::Result<()> {
        self.location(path, false).map(drop)
    }

    /// creates one new folder below the pinned root; existing names are refused.
    pub(crate) fn create_folder(&self, path: &Path) -> io::Result<()> {
        check_stop(&self.stop)?;
        self.location(path, false)?.create_folder()
    }

    /// creates a new file through its parent, with a writable handle even when its mode is read-only.
    pub(crate) fn create_file(&self, path: &Path, readonly: bool) -> io::Result<File> {
        check_stop(&self.stop)?;
        self.location(path, false)?.create_file(readonly)
    }

    /// renames between two checked parents below the pinned root.
    pub(crate) fn rename_file(&self, from: &Path, to: &Path) -> io::Result<()> {
        check_stop(&self.stop)?;
        self.location(from, false)?
            .rename_to(&self.location(to, false)?)
    }

    /// deletes a temporary file through its parent; it is not a recorded placement.
    pub(crate) fn delete_file(&self, path: &Path) -> io::Result<()> {
        check_stop(&self.stop)?;
        let location = self.location(path, false)?;
        #[cfg(unix)]
        {
            location.remove_file()
        }
        #[cfg(windows)]
        {
            Volume::new(location.root_path().to_path_buf()).remove(location.path())
        }
    }

    /// removes an empty folder without following a replacement link.
    pub(crate) fn delete_folder(&self, path: &Path) -> io::Result<()> {
        check_stop(&self.stop)?;
        self.location(path, false)?.remove_dir()
    }

    fn location(&self, destination: &Path, make: bool) -> io::Result<Location> {
        let (destination, _) = self.within(destination)?;
        self.parents
            .locate(&self.root, &destination, self.checks_root, make)
    }

    fn can_hardlink(&self) -> Option<bool> {
        if let Some(links) = self.links.get() {
            return Some(*links);
        }
        let location = self.location(&self.root.join(".probe"), false).ok()?;
        let links = location.can_hardlink()?;
        Some(*self.links.get_or_init(|| links))
    }

    /// `path` made absolute, and its folder, which must be the root or inside it.
    fn within(&self, path: &Path) -> io::Result<(PathBuf, PathBuf)> {
        let path = std::path::absolute(path)?;
        match path.parent() {
            Some(parent) if parent.starts_with(&self.root) => {
                if path.strip_prefix(&self.root).is_ok_and(|relative| {
                    relative
                        .components()
                        .any(|part| matches!(part, std::path::Component::ParentDir))
                }) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "a placement path cannot contain .. below its root",
                    ));
                }
                let parent = parent.to_path_buf();
                Ok((path, parent))
            }
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} is outside {}", path.display(), self.root.display()),
            )),
        }
    }

    /// learns from `method` failing with `error`: a method the filesystem refuses is not tried
    /// again, and an error no other method can avoid, such as an existing destination, is returned.
    /// a hardlink refusal holds for every file only once the volume's own probe agrees, since one
    /// file can be refused alone, as linux's `protected_hardlinks` refuses another user's object.
    fn learn(&self, method: Method, error: io::Error) -> io::Result<()> {
        if error.kind() == io::ErrorKind::AlreadyExists {
            return Err(error);
        }
        let links_nothing = || self.can_hardlink() == Some(false);
        tracing::debug!(?method, %error, "placing failed, trying the next method");
        match method {
            Method::Reflink if refuses_links(&error) || crosses_volumes(&error) => {
                self.can_reflink.store(false, Ordering::Relaxed);
            }
            Method::Hardlink if crosses_volumes(&error) => {
                self.can_hardlink.store(false, Ordering::Relaxed);
                // a cache on this drive helps only where this drive makes links
                self.other_volume.store(!links_nothing(), Ordering::Relaxed);
            }
            Method::Hardlink if refuses_links(&error) && links_nothing() => {
                self.can_hardlink.store(false, Ordering::Relaxed);
                self.other_volume.store(false, Ordering::Relaxed);
            }
            _ => {}
        }
        Ok(())
    }
}

/// whether the file at `path` has another name, as a hardlink shared with the cache does. a
/// file saved anew by an editor has only its own.
pub(crate) fn has_other_names(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        fs::symlink_metadata(path).is_ok_and(|metadata| metadata.nlink() > 1)
    }
    #[cfg(windows)]
    {
        winapi_util::Handle::from_path_any(path)
            .and_then(|handle| winapi_util::file::information(&handle))
            .is_ok_and(|information| information.number_of_links() > 1)
    }
}

/// runs `job` on every item, on a few threads when there are many, and returns the results in
/// the order of `items`.
pub(crate) fn run_all<T: Sync, R: Send>(
    items: &[T],
    cap: Threads,
    job: impl Fn(&T) -> R + Sync,
) -> Vec<R> {
    let threads = workers(items.len(), cap);
    if threads < 2 {
        return items.iter().map(job).collect();
    }
    let next = AtomicUsize::new(0);
    let mut done: Vec<(usize, R)> = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                crate::threads::scoped(scope, || {
                    let mut done = Vec::new();
                    loop {
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        let Some(item) = items.get(index) else {
                            return done;
                        };
                        done.push((index, job(item)));
                    }
                })
            })
            .collect();
        workers
            .into_iter()
            .flat_map(crate::threads::Scoped::join)
            .collect()
    });
    done.sort_unstable_by_key(|(index, _)| *index);
    done.into_iter().map(|(_, result)| result).collect()
}

/// how many threads share `items` items of work.
fn workers(items: usize, cap: Threads) -> usize {
    let threads = std::thread::available_parallelism().map_or(1, NonZero::get);
    cap.limit(threads.min(THREADS).min(items / PER_THREAD))
}

/// runs `job` on every item as [`run_all`] does, for jobs that take their item. each thread
/// takes a run of neighbouring items.
fn run_owned<T: Send, R: Send>(items: Vec<T>, cap: Threads, job: impl Fn(T) -> R + Sync) -> Vec<R> {
    let threads = workers(items.len(), cap);
    if threads < 2 {
        return items.into_iter().map(job).collect();
    }
    let runs = owned_runs(items, threads);
    let job = &job;
    std::thread::scope(|scope| {
        let workers: Vec<_> = runs
            .into_iter()
            .map(|mut run| {
                crate::threads::scoped(scope, move || {
                    std::mem::take(&mut run)
                        .into_iter()
                        .map(job)
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        workers
            .into_iter()
            .flat_map(crate::threads::Scoped::join)
            .collect()
    })
}

fn owned_runs<T>(items: Vec<T>, threads: usize) -> Vec<Vec<T>> {
    let size = items.len().div_ceil(threads);
    let mut items = items.into_iter();
    std::iter::from_fn(|| {
        (!items.as_slice().is_empty()).then(|| items.by_ref().take(size).collect())
    })
    .collect()
}

/// the orders reordered so that neighbours go to different folders. links made into one folder
/// wait on each other, so threads taking the next order in turn would queue on one folder.
fn spread(orders: impl IntoIterator<Item = (usize, Order)>) -> Vec<usize> {
    let mut seen: HashMap<PathBuf, usize> = HashMap::new();
    let mut ranked: Vec<(usize, usize)> = orders
        .into_iter()
        .map(|(index, order)| {
            let destination = &order.destination;
            let rank = seen
                .entry(destination.parent().unwrap_or(destination).to_path_buf())
                .or_default();
            *rank += 1;
            (*rank, index)
        })
        .collect();
    ranked.sort_by_key(|(rank, _)| *rank);
    ranked.into_iter().map(|(_, index)| index).collect()
}

/// how a caller creates missing folders and reports invalid ones.
#[derive(Clone, Copy)]
pub(crate) enum FolderUse {
    Cache,
    Place { make: bool },
}

/// refuses links and files where a caller needs a real folder.
pub(crate) fn real_folder(folder: &Path, usage: FolderUse) -> io::Result<()> {
    let found = match usage {
        FolderUse::Cache => None,
        FolderUse::Place { make } => match fs::symlink_metadata(folder) {
            Err(err) if err.kind() == io::ErrorKind::NotFound && make => None,
            found => Some(found?),
        },
    };
    let metadata = if let Some(found) = found {
        found
    } else {
        if let Err(err) = fs::create_dir(folder)
            && err.kind() != io::ErrorKind::AlreadyExists
        {
            return Err(err);
        }
        fs::symlink_metadata(folder)?
    };
    found_folder(folder, &metadata, usage)
}

/// refuses what [`real_folder`] refuses, given what is at `folder`, creating nothing.
pub(crate) fn found_folder(
    folder: &Path,
    metadata: &fs::Metadata,
    usage: FolderUse,
) -> io::Result<()> {
    let is_link = metadata.file_type().is_symlink();
    if metadata.is_dir() && !is_link {
        return Ok(());
    }
    let (kind, message) = match usage {
        FolderUse::Cache => {
            let what = if is_link { "a link" } else { "a file" };
            (
                io::ErrorKind::InvalidInput,
                format!(
                    "{} is {what}, and the cache keeps only real folders",
                    folder.display()
                ),
            )
        }
        FolderUse::Place { .. } if is_link => (
            io::ErrorKind::InvalidInput,
            format!(
                "{} is a link, and fetchloom only places files in real folders",
                folder.display()
            ),
        ),
        FolderUse::Place { .. } => (
            io::ErrorKind::NotADirectory,
            format!("{} is a file where a folder should be", folder.display()),
        ),
    };
    Err(io::Error::new(kind, message))
}

fn check_object(object: &Path) -> io::Result<()> {
    if is_plain(&fs::symlink_metadata(object)?) {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        format!(
            "{} is a link or a special file, which fetchloom never places",
            object.display()
        ),
    ))
}

/// copies `object` to `destination`, failing when `destination` exists, and makes the copy
/// writable or read-only as asked.
fn copy_at(
    object: &Path,
    location: &Arc<Location>,
    writable: bool,
    threads: Threads,
    flush: bool,
    stop: &Stop,
) -> io::Result<Metadata> {
    let (staged, file) = stage_copy_at(object, location, writable, threads, stop)?;
    if flush {
        file.sync_all()?;
    }
    check_stop(stop)?;
    staged.publish()?;
    #[cfg(windows)]
    {
        if let Err(error) = set_file_writable(&file, writable) {
            let _ = fs::remove_file(location.path());
            return Err(error);
        }
    }
    file.metadata()
}

fn stage_copy_at(
    object: &Path,
    location: &Arc<Location>,
    writable: bool,
    threads: Threads,
    stop: &Stop,
) -> io::Result<(Staged, File)> {
    check_stop(stop)?;
    let (staged, file) = location.create(0o666, false)?;
    // publishing clears windows attributes, so seal through the claimed handle afterward.
    fill(object, &file, cfg!(windows) || writable, threads, stop)?;
    Ok((staged, file))
}

#[cfg(test)]
fn copy(
    object: &Path,
    destination: &Path,
    writable: bool,
    threads: Threads,
    flush: bool,
    stop: &Stop,
) -> io::Result<()> {
    let location = Arc::new(Location::standalone(destination)?);
    copy_at(object, &location, writable, threads, flush, stop).map(drop)
}

#[cfg(test)]
fn stage_copy(
    object: &Path,
    destination: &Path,
    writable: bool,
    threads: Threads,
    stop: &Stop,
) -> io::Result<(Staged, File)> {
    stage_copy_at(
        object,
        &Arc::new(Location::standalone(destination)?),
        writable,
        threads,
        stop,
    )
}

/// creates and fills a hidden file through its parent handle before giving it a final name.
#[cfg(any(target_os = "linux", test))]
fn publish_new(
    location: &Arc<Location>,
    mode: u32,
    write: impl FnOnce(&File) -> io::Result<()>,
) -> io::Result<Metadata> {
    let (staged, file) = location.create(mode, true)?;
    write(&file)?;
    staged.publish()
}

/// names the copies `waiting` holds once `flush` has put their bytes on disk, so no name ever
/// holds bytes a crash could still lose, and reads what `seen` keeps of each. the first is named
/// alone, as in placing. when the flush fails none is named, and their hidden files go.
fn publish_flushed<T: Send>(
    waiting: Vec<(usize, Staged)>,
    flush: impl FnOnce() -> io::Result<()>,
    threads: Threads,
    seen: &(impl Fn(&Metadata) -> T + Sync),
    stop: &Stop,
    grouped: &schedule::Work,
) -> Vec<(usize, io::Result<(Method, T)>)> {
    if waiting.is_empty() {
        return Vec::new();
    }
    if let Err(err) = check_stop(stop).and_then(|()| flush()) {
        return waiting
            .into_iter()
            .map(|(index, _)| {
                let message = format!("could not flush the copied files to disk: {err}");
                (index, Err(io::Error::new(err.kind(), message)))
            })
            .collect();
    }
    let publish = &|(index, staged): (usize, Staged)| {
        let _permit = grouped.enter(index);
        let named = check_stop(stop)
            .and_then(|()| staged.publish())
            .map(|metadata| (Method::Copy, seen(&metadata)));
        (index, named)
    };
    let mut waiting = waiting.into_iter();
    let first = waiting.next().map(publish);
    let rest = run_owned(waiting.collect(), threads, publish);
    first.into_iter().chain(rest).collect()
}

/// clones `object` to `destination`, failing when `destination` exists. the outer result says
/// whether the clone was made, the inner whether it was then made writable or read-only as asked.
/// the clone is made under a hidden name with its final mode and named only once complete, as a
/// copy is, since an interrupted clone would leave an empty file where the project expects one.
#[cfg(target_os = "linux")]
fn reflink(
    object: &Path,
    location: &Arc<Location>,
    writable: bool,
) -> io::Result<io::Result<Metadata>> {
    use std::os::unix::fs::PermissionsExt;
    let source = read_unfollowed(object)?;
    let mode = writable_as(source.metadata()?.permissions(), writable).mode() & 0o7777;
    let metadata = publish_new(location, mode, |file| {
        if file.metadata()?.permissions().mode() & 0o7777 != mode {
            file.set_permissions(Permissions::from_mode(mode))?;
        }
        rustix::fs::ioctl_ficlone(file, &source).map_err(io::Error::from)
    })?;
    Ok(Ok(metadata))
}

#[cfg(target_os = "macos")]
fn reflink(
    object: &Path,
    location: &Arc<Location>,
    writable: bool,
) -> io::Result<io::Result<Metadata>> {
    let source = read_unfollowed(object)?;
    location.clone_file(&source, writable).map(Ok)
}

#[cfg(windows)]
fn reflink(
    object: &Path,
    location: &Arc<Location>,
    writable: bool,
) -> io::Result<io::Result<Metadata>> {
    location.clone_file(object, writable).map(Ok)
}

/// whether a file has the reserved name of a copy or clone placement left unfinished.
pub(crate) fn is_copy_temporary(name: &str) -> bool {
    name.strip_prefix(".fetchloom-")
        .and_then(|name| name.strip_suffix(".copy"))
        .is_some_and(|random| {
            random.len() == 16 && random.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
}

/// copies the bytes of `object` into `output` and seals it as asked, all through the handle, so
/// a link swapped in for the name `output` claimed is never written through.
fn fill(
    object: &Path,
    mut output: &File,
    writable: bool,
    threads: Threads,
    stop: &Stop,
) -> io::Result<()> {
    let mut input = read_unfollowed(object)?;
    if threads.is_limited() {
        // a reader per copy would multiply the placement worker cap.
        copy_stoppable(&mut input, &mut output, stop)?;
    } else {
        transfer(input, output, stop)?;
    }
    check_stop(stop)?;
    set_file_writable(output, writable)
}

fn set_file_writable(output: &File, writable: bool) -> io::Result<()> {
    let before = output.metadata()?.permissions();
    let after = writable_as(before.clone(), writable);
    if before != after {
        output.set_permissions(after)?;
    }
    Ok(())
}

/// linux copies from file to file inside the kernel, with `copy_file_range`, which a network
/// filesystem can even run on the server.
#[cfg(target_os = "linux")]
fn transfer(input: File, output: &File, stop: &Stop) -> io::Result<()> {
    copy_stoppable(input, output, stop)
}

#[cfg(not(target_os = "linux"))]
fn transfer(mut input: File, mut output: &File, stop: &Stop) -> io::Result<()> {
    check_stop(stop)?;
    match usize::try_from(input.metadata()?.len()) {
        Ok(size) if size <= BLOCK => {
            let mut bytes = Vec::with_capacity(size);
            Read::by_ref(&mut input)
                .take(BLOCK as u64)
                .read_to_end(&mut bytes)?;
            output.write_all(&bytes)?;
            if bytes.len() == BLOCK {
                copy_stoppable(input, output, stop)
            } else {
                Ok(())
            }
        }
        _ => overlapped(input, output, stop),
    }
}

/// bounds each `io::copy` operation so a stop is checked between blocks.
const BLOCK: usize = 1 << 20;

fn check_stop(stop: &Stop) -> io::Result<()> {
    if stop.is_asked() {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "stopped at your request",
        ));
    }
    Ok(())
}

fn copy_stoppable(mut input: impl Read, mut output: impl Write, stop: &Stop) -> io::Result<()> {
    loop {
        check_stop(stop)?;
        if io::copy(&mut input.by_ref().take(BLOCK as u64), &mut output)? == 0 {
            return Ok(());
        }
    }
}

/// copies `input` into `output` a block at a time, reading the next block on another thread
/// while this one writes the last. with the disk never waiting on the other side, a copy keeps
/// pace with the operating system's own: 1 GiB took 0.62 s against 0.99 s one block at a time
/// on an nvme laptop.
#[cfg(any(not(target_os = "linux"), test))]
fn overlapped(mut input: File, mut output: &File, stop: &Stop) -> io::Result<()> {
    let (full, filled) = std::sync::mpsc::sync_channel::<io::Result<Vec<u8>>>(2);
    let (empty, spare) = std::sync::mpsc::sync_channel::<Vec<u8>>(3);
    for _ in 0..3 {
        let _ = empty.send(vec![0; BLOCK]);
    }
    let caller = std::thread::current().id();
    std::thread::scope(move |scope| {
        let worker = crate::threads::scoped(scope, move || {
            // a refused reader runs at join, when there is no concurrent queue consumer.
            if std::thread::current().id() == caller {
                return copy_stoppable(&mut input, output, stop);
            }
            while let Ok(mut block) = spare.recv() {
                block.resize(BLOCK, 0);
                let read = check_stop(stop).and_then(|()| input.read(&mut block));
                let sent = match read {
                    Ok(0) => return Ok(()),
                    Ok(read) => {
                        block.truncate(read);
                        full.send(Ok(block))
                    }
                    Err(error) => full.send(Err(error)),
                };
                if sent.is_err() {
                    return Ok(());
                }
            }
            Ok(())
        });
        if !worker.is_threaded() {
            drop(filled);
            drop(empty);
            return worker.join();
        }
        let mut written = Ok(());
        while let Ok(block) = filled.recv() {
            written = block.and_then(|block| {
                check_stop(stop)?;
                output.write_all(&block)?;
                let _ = empty.send(block);
                Ok(())
            });
            if written.is_err() {
                break;
            }
        }
        drop(filled);
        drop(empty);
        written.and(worker.join())
    })
}

/// `permissions` changed to let the file's owner write it, or no one.
#[cfg(unix)]
fn writable_as(mut permissions: Permissions, writable: bool) -> Permissions {
    use std::os::unix::fs::PermissionsExt;

    let mode = permissions.mode();
    permissions.set_mode(if writable {
        mode | 0o200
    } else {
        mode & !0o222
    });
    permissions
}

#[cfg(not(unix))]
fn writable_as(mut permissions: Permissions, writable: bool) -> Permissions {
    permissions.set_readonly(!writable);
    permissions
}

#[cfg(test)]
mod tests;
