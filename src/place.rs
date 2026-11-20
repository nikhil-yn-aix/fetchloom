//! putting cached objects where a project expects them, by reflink, else hardlink, else copy.

#[cfg(unix)]
mod parents;
#[cfg(windows)]
#[path = "place/parents_windows.rs"]
mod parents;
mod schedule;
mod synced;
pub(crate) use synced::remember as remember_synced;

use parents::{Location, Parents, Staged};

use std::borrow::Cow;
use std::collections::HashSet;
use std::fs::{self, File, Metadata, Permissions};
use std::io::{self, Read, Write};
use std::num::NonZero;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use serde::{Deserialize, Serialize};

use crate::space::Filesystem;
use crate::stop::Stop;
use crate::threads::Threads;
#[cfg(windows)]
use crate::volume::Volume;
use crate::volume::{aside_name, crosses_volumes, is_plain, read_unfollowed, refuses_links};

/// the most threads that place files or look up objects at once, since each call waits on the
/// disk and, on windows, the antivirus scanner: placing 10,000 files on windows took 7.5 s on
/// four threads, 5.1 s on eight, and little less on sixteen.
const THREADS: usize = 8;

/// the fewest items worth a thread of their own, since starting one costs more than a few calls.
const PER_THREAD: usize = 64;

/// the most threads a sync's placement into btrfs takes, placing files or naming those that waited
/// for its flush: each create, clone and rename there updates the same trees, and past a few
/// threads placing took longer and more cpu.
const BTRFS_THREADS: usize = 3;

/// hashing this many bytes repays starting a worker even for a short file list.
const HASH_BYTES: u64 = 4 << 20;

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

/// each folder a placer roots at that is opened elsewhere: a data folder this run reached through
/// a link it followed, with the folder found there then.
static OPENED_AT: std::sync::Mutex<Vec<(PathBuf, PathBuf)>> = std::sync::Mutex::new(Vec::new());

/// opens every placer rooted at `folder` at `found`, the folder a link on its way led to when the
/// project checked it, whatever the link says by then.
pub(crate) fn open_at(folder: &Path, found: PathBuf) -> io::Result<()> {
    let folder = std::path::absolute(folder)?;
    let mut opened = OPENED_AT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    opened.retain(|(root, _)| *root != folder);
    opened.push((folder, found));
    Ok(())
}

/// where a placer rooted at `root`, absolute, opens it, as [`open_at`] noted.
pub(crate) fn opened_at(root: &Path) -> Option<PathBuf> {
    OPENED_AT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .find(|(folder, _)| folder == root)
        .map(|(_, found)| found.clone())
}

/// puts objects into one folder tree, and remembers which methods its filesystem refuses.
pub(crate) struct Placer {
    threads: Threads,
    stop: Arc<Stop>,
    root: PathBuf,
    /// where the root is opened, when it is a data folder this run reached through a link it
    /// followed: the folder found there then, whatever the link says by now.
    opened_at: Option<PathBuf>,
    /// whether the root itself must be a real folder, as a project's data folder must. a folder
    /// someone named for `get` may be a link to one.
    checks_root: bool,
    /// whether placing takes the thread counts measured for sync's placement, which only sync's
    /// placer asks for, as `get` was not measured with them.
    sync_threads: bool,
    /// folders under the root already seen to be real folders, not links.
    folders: HashSet<PathBuf>,
    synced: Option<&'static str>,
    can_reflink: AtomicBool,
    can_hardlink: AtomicBool,
    other_volume: AtomicBool,
    read_only_cache: AtomicBool,
    /// whether the file at a path sits on a read-only mount, which a test may pretend.
    read_only: fn(&Path) -> bool,
    /// the volume under the root, which may refuse to remove a read-only file.
    #[cfg(windows)]
    volume: Volume,
    parents: Parents,
    links: OnceLock<bool>,
    filesystem: OnceLock<Filesystem>,
    #[cfg(test)]
    observed: Option<Box<Observation>>,
    /// what every hardlink fails with instead, as a disk would refuse it.
    #[cfg(test)]
    link_refusal: Option<fn() -> io::Error>,
    /// what every clone does instead of asking the filesystem, as a disk that clones would.
    #[cfg(all(test, target_os = "linux"))]
    clone_with: Option<fn(&File, &File) -> io::Result<()>>,
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

#[cfg(test)]
type Swap = Box<dyn FnOnce(&Path)>;

#[cfg(test)]
thread_local! {
    /// what a test does to a file's place once its folders are walked and before the file opens,
    /// as a co-user swapping one of them for a link meanwhile would, once.
    pub(crate) static WALKED: std::cell::RefCell<Option<Swap>> =
        const { std::cell::RefCell::new(None) };
}

/// one file to place; `writable` asks for a writable copy.
pub(crate) struct Order {
    pub(crate) object: PathBuf,
    pub(crate) destination: PathBuf,
    pub(crate) writable: bool,
    pub(crate) size: u64,
}

/// a file that is a copy of its own of `object`'s bytes, to link to `object` where it stands.
pub(crate) struct Relink {
    pub(crate) object: PathBuf,
    pub(crate) destination: PathBuf,
}

/// what became of a file [`Placer::relink_all`] was to link.
#[derive(Debug)]
pub(crate) enum Relinked {
    /// it is now a link of this kind to its object, and looks as the metadata says.
    Linked(Method, Metadata),
    /// no link to its object can stand here: the disk refused one, or allows none.
    Refused,
    /// it does not hold its object's bytes, as far as reading it could tell.
    Other,
    /// it stays as it is for now: it changed meanwhile, its bytes could not be read, or it could
    /// not be renamed over, as windows refuses while another program holds it open.
    Kept,
}

/// a link [`Placer::relink_all`] staged beside the file it is to take the place of.
struct StagedLink {
    method: Method,
    staged: Staged,
    /// whether it waits for the batch's one flush before it takes a name.
    waits: bool,
}

impl StagedLink {
    fn new(method: Method, staged: Staged, waits: bool) -> StagedLink {
        StagedLink {
            method,
            staged,
            waits,
        }
    }

    /// renames the link over its file while `unchanged` says the file still looks as it did.
    fn replace(self, unchanged: impl FnOnce(&Metadata) -> bool) -> Relinked {
        match self.staged.replace(unchanged) {
            Ok(Some(metadata)) => Relinked::Linked(self.method, metadata),
            Ok(None) => Relinked::Kept,
            Err(error) => {
                tracing::debug!(%error, "could not rename a link over its copy");
                Relinked::Kept
            }
        }
    }
}

/// how far [`Placer::relink_all`] got with one file.
enum Step {
    Done(Relinked),
    /// a clone that takes its name once the batch is flushed.
    Waiting(StagedLink),
}

/// whether `error`, from making a link, refuses that link for good, rather than failing for now,
/// as a full disk does: the disk links nothing, or not across volumes, the object has as many
/// names as the disk allows, or linux's `protected_hardlinks` or a permission refuses it.
fn refuses_link(error: &io::Error) -> bool {
    refuses_links(error)
        || crosses_volumes(error)
        || matches!(
            error.kind(),
            io::ErrorKind::TooManyLinks | io::ErrorKind::PermissionDenied
        )
}

/// what putting one file in place did.
enum Put<T> {
    /// placed it by a method, with what was kept of its metadata.
    Placed(Method, T),
    /// copied or cloned it under a hidden name, which it trades for its own once the batch is
    /// flushed.
    Waiting(Method, Staged),
}

impl<T> Put<T> {
    fn method(&self) -> Method {
        match self {
            Put::Placed(method, _) | Put::Waiting(method, _) => *method,
        }
    }
}

/// why a placer copied files it would rather have linked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Unlinked {
    /// the root sits inside a folder this sync client owns, which breaks links: "onedrive",
    /// "dropbox", "icloud drive" or "google drive".
    Synced(&'static str),
    /// hardlinks failed because the cache sits on another drive, which a cache on the same drive
    /// would fix.
    OtherDrive,
    /// hardlinks failed because the cache sits on a read-only mount, which no link may leave.
    ReadOnlyCache,
    /// the drive refused reflinks and hardlinks alike, as FAT and exFAT drives do.
    NoLinks,
}

impl Unlinked {
    /// explains why the named files were copied and how another placement may allow links.
    pub(crate) fn copied_because(self, place: impl std::fmt::Display, subject: &str) -> Because {
        match self {
            Self::Synced(client) => Because {
                said: format!(
                    "{place} is inside {client}, which breaks links, so {subject} copied"
                ),
                help: None,
            },
            Self::NoLinks => Because {
                said: format!("the drive holding {place} cannot link files, so {subject} copied"),
                help: None,
            },
            Self::ReadOnlyCache => Because {
                said: format!("the cache is read-only, so {subject} copied"),
                help: None,
            },
            Self::OtherDrive => Because {
                said: format!("the cache is on another drive than {place}, so {subject} copied"),
                help: Some(format!(
                    "to link files instead, set FETCHLOOM_CACHE to a folder on the drive of {place}. see https://nikhil-yn-aix.github.io/fetchloom/user/concepts/cache-and-placement/#links-need-one-drive"
                )),
            },
        }
    }
}

/// why files were copied rather than linked, and what lets fetchloom link them, when anything
/// does: one sentence for a hint, or a warning with its help on a line of its own.
#[derive(Debug)]
pub(crate) struct Because {
    pub(crate) said: String,
    pub(crate) help: Option<String>,
}

impl std::fmt::Display for Because {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.help {
            Some(help) => write!(f, "{}. {help}", self.said),
            None => f.write_str(&self.said),
        }
    }
}

impl Placer {
    /// prepares placement under `root`, which must be a real folder, and notes whether `root`
    /// sits inside a folder a sync client owns. capability probes use the opened root too.
    pub(crate) fn new(root: &Path) -> Placer {
        let root = std::path::absolute(root).unwrap_or_else(|_| root.to_path_buf());
        let opened_at = opened_at(&root);
        let real = opened_at.as_deref().unwrap_or(&root);
        let synced = synced::client_of(real);
        if let Some(client) = synced {
            tracing::debug!(root = %real.display(), client, "inside a synced folder, so no hardlinks");
        }
        let mut placer = Placer::starting_at(root, Method::Reflink, synced);
        #[cfg(windows)]
        if let Some(real) = &opened_at {
            placer.volume = Volume::new(real.clone());
        }
        placer.opened_at = opened_at;
        placer
    }

    /// prepares placement under a folder someone named, which may itself be a link, though the
    /// folders below it must be real. as fetchloom's one folder on its volume, a removal probes
    /// there, with a hidden file, whether the volume can hardlink.
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
            filesystem: OnceLock::new(),
            #[cfg(test)]
            observed: None,
            #[cfg(test)]
            link_refusal: None,
            #[cfg(all(test, target_os = "linux"))]
            clone_with: None,
            threads: Threads::default(),
            stop: Arc::default(),
            root,
            opened_at: None,
            checks_root: true,
            sync_threads: false,
            folders: HashSet::new(),
            synced,
            can_reflink: AtomicBool::new(first == Method::Reflink),
            can_hardlink: AtomicBool::new(first != Method::Copy),
            other_volume: AtomicBool::new(false),
            read_only_cache: AtomicBool::new(false),
            read_only: crate::volume::read_only,
            #[cfg(target_os = "linux")]
            batch: std::sync::OnceLock::new(),
            #[cfg(target_os = "linux")]
            unflushed: AtomicBool::new(false),
            #[cfg(target_os = "macos")]
            copy_flush: std::sync::Mutex::new(None),
        }
    }

    pub(crate) fn with_threads(mut self, threads: Threads) -> Placer {
        self.threads = threads;
        self
    }

    /// places with the thread counts measured for sync's placement, the one caller that asks: at
    /// most [`BTRFS_THREADS`] on btrfs.
    pub(crate) fn with_sync_threads(mut self) -> Placer {
        self.sync_threads = true;
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

    /// the folder this placer places into.
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// why files were copied rather than linked, as far as the filesystem told.
    pub(crate) fn unlinked(&self) -> Option<Unlinked> {
        if let Some(client) = self.synced {
            return Some(Unlinked::Synced(client));
        }
        if self.read_only_cache.load(Ordering::Relaxed) {
            return Some(Unlinked::ReadOnlyCache);
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
            Put::Waiting(method, staged) => staged.publish().map(|_| method),
        }
    }

    /// places every order and says how each went, in order, with what `seen` keeps of each file's
    /// metadata, read right after placing it. the first goes alone to learn which methods the
    /// filesystem allows; copies flushed as a batch take their names only after the flush.
    pub(crate) fn place_all<T: Send>(
        &mut self,
        count: usize,
        order: impl Fn(usize) -> Order + Sync,
        seen: impl Fn(&Metadata) -> T + Sync,
    ) -> Vec<io::Result<(Method, T)>> {
        let mut results = Vec::with_capacity(count);
        let mut folders = schedule::Folders::new(count);
        // whether each folder passed its check, so the check runs once per folder, not per file
        let mut checked = Vec::new();
        let ready = spread((0..count).filter_map(|index| {
            if let Err(err) = self.stop.check_io() {
                results.push((index, Err(err)));
                return None;
            }
            let order = order(index);
            let (group, first) = folders.record(index, &order.destination);
            if first {
                checked.push(false);
            }
            if !checked[group] {
                if let Err(err) = self.check_folders(&order.destination, true) {
                    results.push((index, Err(err)));
                    return None;
                }
                checked[group] = true;
            }
            // refused before any byte is copied, rather than failing at the disk's limit
            if !self.filesystem().holds(order.size) {
                results.push((index, Err(self.too_large(order.size))));
                return None;
            }
            Some((index, group))
        }));
        let this = &*self;
        let observe = |metadata: &Metadata| {
            #[cfg(test)]
            if let Some(observed) = &this.observed {
                observed(metadata);
            }
            seen(metadata)
        };
        let serial = !ready.is_empty() && this.filesystem().serializes();
        let grouped = folders.finish(serial);
        let threads = grouped.threads(this.threads);
        // the filesystem is asked only for work, as an empty placement must not latch a failed probe
        let threads = if this.sync_threads && !ready.is_empty() {
            placing_threads(threads, this.filesystem())
        } else {
            threads
        };
        // on linux a few copies flush one by one, since one flush of the filesystem also waits
        // for everything other programs wrote there
        let batch = (cfg!(not(target_os = "linux")) || count > crate::volume::MOST_FLUSHED_ALONE)
            && this.batches_copies();
        // keeping a whole Metadata for each of 100,000 files cost tens of megabytes
        let put = |index: &usize| {
            let _permit = grouped.enter(*index);
            let order = order(*index);
            let placed = this
                .put(&order.object, &order.destination, order.writable, batch)
                .map(|put| match put {
                    Put::Placed(method, metadata) => Put::Placed(method, observe(&metadata)),
                    Put::Waiting(method, staged) => Put::Waiting(method, staged),
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
                Ok(Put::Waiting(method, staged)) => waiting.push((index, method, staged)),
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
        in_order(count, results)
    }

    /// tries each method the filesystem may allow, best first. with `batch` a copy waits under its
    /// hidden name for [`Placer::flush_copies`]; without, it flushes on its own.
    fn put(
        &self,
        object: &Path,
        destination: &Path,
        writable: bool,
        batch: bool,
    ) -> io::Result<Put<Metadata>> {
        self.stop.check_io()?;
        let location = Arc::new(self.location(destination, false)?);
        let early_hardlink = cfg!(windows)
            && !writable
            && self.synced.is_none()
            && self.can_hardlink.load(Ordering::Relaxed);
        if early_hardlink {
            let checked = check_object(object)?;
            match self.link(&location, object, checked) {
                Ok(metadata) => return Ok(Put::Placed(Method::Hardlink, metadata)),
                Err(error) => self.learn(Method::Hardlink, error, object)?,
            }
        }
        if self.can_reflink.load(Ordering::Relaxed) {
            check_object(object)?;
            #[cfg(target_os = "linux")]
            let cloned = {
                let flush = Self::flushes_alone(&location, batch);
                self.clone_at(object, &location, writable, batch && !flush, flush)
                    .map(Ok)
            };
            #[cfg(not(target_os = "linux"))]
            let cloned = reflink(object, &location, writable)
                .map(|sealed| sealed.map(|metadata| Put::Placed(Method::Reflink, metadata)));
            match cloned {
                Ok(placed) => return placed,
                Err(error) => self.learn(Method::Reflink, error, object)?,
            }
        }
        // sync clients break hardlinks or upload every link as a full file
        if !early_hardlink
            && self.can_hardlink.load(Ordering::Relaxed)
            && !writable
            && self.synced.is_none()
        {
            let checked = check_object(object)?;
            match self.link(&location, object, checked) {
                Ok(metadata) => return Ok(Put::Placed(Method::Hardlink, metadata)),
                Err(error) => self.learn(Method::Hardlink, error, object)?,
            }
        }
        let flush = Self::flushes_alone(&location, batch);
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
            Put::Waiting(Method::Copy, staged)
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

    /// whether a file placed at `location` flushes on its own rather than in `batch`'s one flush,
    /// as outside a batch, or on a nested mount, which the root filesystem's batch flush does not
    /// cover.
    fn flushes_alone(location: &Location, batch: bool) -> bool {
        #[cfg(unix)]
        let nested = || location.other_device();
        #[cfg(not(unix))]
        let nested = || {
            let _ = location;
            false
        };
        !batch || nested()
    }

    /// whether copies may skip their own flush, because [`Placer::flush_copies`] flushes them all.
    #[cfg(target_os = "linux")]
    fn batches_copies(&self) -> bool {
        self.batch
            .get_or_init(|| {
                let release = fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default();
                crate::volume::syncfs_reports_errors(&release)
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

    /// checks every folder between the root and `path` once, refusing a link among them, so none
    /// inside the data folder sends a file elsewhere. with `make`, missing folders are made.
    pub(crate) fn check_folders(&mut self, path: &Path, make: bool) -> io::Result<()> {
        self.stop.check_io()?;
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

    /// removes a file fetchloom placed, checking its folders again since downloads ran, and with
    /// `prune` the folders it leaves empty, up to the root. a file `placed` as a hardlink is the
    /// cache's object, so its read-only flag is never cleared.
    pub(crate) fn remove(
        &mut self,
        path: &Path,
        prune: bool,
        placed: Option<Method>,
    ) -> io::Result<()> {
        self.stop.check_io()?;
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
            self.stop.check_io()?;
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

    /// opens the plain file at `path` to read through the folders the walk below the root holds,
    /// never following a link among them or in its place, so a folder swapped for a link once
    /// walked cannot send the read to another file. a link, a folder or a special file in its
    /// place is refused as `InvalidInput`.
    pub(crate) fn open_read(&self, path: &Path) -> io::Result<File> {
        let location = self.location(path, false)?;
        #[cfg(test)]
        if let Some(swap) = WALKED.take() {
            swap(path);
        }
        let refused = || {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "{} is a link or a special file, which fetchloom never reads or writes",
                    path.display()
                ),
            )
        };
        let file = match location.open_read() {
            Ok(file) => file,
            #[cfg(unix)]
            Err(err) if err.raw_os_error() == Some(rustix::io::Errno::LOOP.raw_os_error()) => {
                return Err(refused());
            }
            // a folder or a link in its place, which windows opens to read for no one
            #[cfg(windows)]
            Err(_) if location.metadata().is_ok_and(|found| !is_plain(&found)) => {
                return Err(refused());
            }
            Err(err) => return Err(err),
        };
        if is_plain(&file.metadata()?) {
            Ok(file)
        } else {
            Err(refused())
        }
    }

    /// what has the name `path` now, looked at through the folders the walk below the root holds,
    /// never following a link among them or in its place.
    pub(crate) fn metadata(&self, path: &Path) -> io::Result<Metadata> {
        self.location(path, false)?.metadata()
    }

    /// creates one new folder below the pinned root; existing names are refused.
    pub(crate) fn create_folder(&self, path: &Path) -> io::Result<()> {
        self.stop.check_io()?;
        self.location(path, false)?.create_folder()
    }

    /// creates a new file through its parent, with a handle that writes it even when its mode is
    /// read-only, and reads it, so what was written can be read back through the same file.
    pub(crate) fn create_file(&self, path: &Path, readonly: bool) -> io::Result<File> {
        self.stop.check_io()?;
        self.location(path, false)?.create_file(readonly)
    }

    /// [`Placer::create_file`] of a writable file anyone may read who reads the person's own
    /// files, as a file the person makes: on unix 0666 less the umask, where fetchloom's private
    /// files are its user's alone.
    pub(crate) fn create_shared_file(&self, path: &Path) -> io::Result<File> {
        self.stop.check_io()?;
        self.location(path, false)?.create_shared_file()
    }

    /// renames between two checked parents below the pinned root, replacing a file at `to`, so it
    /// suits only a folder no one else writes; [`Placer::move_aside`] never replaces.
    pub(crate) fn rename_file(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.stop.check_io()?;
        self.location(from, false)?
            .rename_to(&self.location(to, false)?)
    }

    /// moves the file at `path` to a new name beside it, `.fetchloom-<hex>.old`, never replacing
    /// anything, and returns it; a sync leaves such a file alone for a person to find.
    pub(crate) fn move_aside(&self, path: &Path) -> io::Result<PathBuf> {
        self.stop.check_io()?;
        let (path, parent) = self.within(path)?;
        let from = self.location(&path, false)?;
        loop {
            let aside = parent.join(aside_name());
            match from.rename_new_to(&self.location(&aside, false)?) {
                Ok(()) => return Ok(aside),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
    }

    /// moves the file at `path` to `aside`, a name beside it from [`aside_name`] that was written
    /// down before the move, never replacing anything there.
    pub(crate) fn move_aside_to(&self, path: &Path, aside: &Path) -> io::Result<()> {
        self.stop.check_io()?;
        self.location(path, false)?
            .rename_new_to(&self.location(aside, false)?)
    }

    /// gives a file [`Placer::move_aside`] moved to `aside` its name, `path`, back, never
    /// replacing a file there. a stop does not stop it, since it puts things back.
    pub(crate) fn move_back(&self, aside: &Path, path: &Path) -> io::Result<()> {
        self.location(aside, false)?
            .rename_new_to(&self.location(path, false)?)
    }

    /// gives the file at `from` a second name, `to`, between two checked parents below the pinned
    /// root. a name already taken is refused with `AlreadyExists`, so nothing is replaced.
    pub(crate) fn link_file(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.stop.check_io()?;
        self.location(from, false)?
            .link_to(&self.location(to, false)?)
    }

    /// gives `to` a file of its own with the bytes of the plain file at `from`, both below the
    /// pinned root: a reflink, else a flushed copy, never a hardlink, so a write to one leaves the
    /// other. a taken name is refused with `AlreadyExists`.
    pub(crate) fn copy_file(&self, from: &Path, to: &Path, writable: bool) -> io::Result<Method> {
        self.stop.check_io()?;
        self.check_parent(from)?;
        let location = Arc::new(self.location(to, false)?);
        if self.can_reflink.load(Ordering::Relaxed) {
            #[cfg(target_os = "linux")]
            let cloned = self
                .clone_at(from, &location, writable, false, true)
                .map(Ok);
            #[cfg(not(target_os = "linux"))]
            let cloned = reflink(from, &location, writable);
            match cloned {
                Ok(sealed) => return sealed.map(|_| Method::Reflink),
                Err(error) => self.learn(Method::Reflink, error, from)?,
            }
        }
        copy_at(from, &location, writable, self.threads, true, &self.stop)?;
        Ok(Method::Copy)
    }

    /// links each file `order` names in place to its object, as placing links a new file: a
    /// reflink, else a hardlink, a hardlink first on windows, never a hardlink inside a synced
    /// folder, and never a copy. each link is made under a hidden name beside its file; once
    /// `holds` says the file holds its object's bytes, the link is renamed over it while
    /// `unchanged` says it still looks as it did, so the file keeps its bytes until the link takes
    /// its name. a linux clone in a batch of many waits for one flush first, as a copy does. the
    /// first file goes alone to learn what the disk allows; `holds` runs outside the folder
    /// permits, so reading a file holds up no change in its folder.
    pub(crate) fn relink_all(
        &self,
        count: usize,
        order: impl Fn(usize) -> Relink + Sync,
        holds: impl Fn(usize) -> Option<bool> + Sync,
        unchanged: impl Fn(usize, &Metadata) -> bool + Sync,
    ) -> Vec<Relinked> {
        let mut folders = schedule::Folders::new(count);
        for index in 0..count {
            folders.record(index, &order(index).destination);
        }
        let grouped = folders.finish(count > 0 && self.filesystem().serializes());
        let threads = grouped.threads(self.threads);
        // as in placing: a few clones flush one by one, since one flush of the filesystem also
        // waits for everything other programs wrote there
        let batch = count > crate::volume::MOST_FLUSHED_ALONE && self.batches_copies();
        let relink = |index: &usize| {
            let Relink {
                object,
                destination,
            } = order(*index);
            let staged = {
                let _permit = grouped.enter(*index);
                self.stage_relink(&object, &destination, batch)
            };
            let step = match staged {
                Err(error) => {
                    tracing::debug!(path = %destination.display(), %error, "could not link a copy to its object");
                    Step::Done(Relinked::Kept)
                }
                Ok(None) => Step::Done(Relinked::Refused),
                Ok(Some(staged)) => match holds(*index) {
                    None => Step::Done(Relinked::Kept),
                    Some(false) => Step::Done(Relinked::Other),
                    Some(true) if staged.waits => Step::Waiting(staged),
                    Some(true) => {
                        let _permit = grouped.enter(*index);
                        Step::Done(staged.replace(|now| unchanged(*index, now)))
                    }
                },
            };
            (*index, step)
        };
        let indexes: Vec<usize> = (0..count).collect();
        let mut steps = Vec::with_capacity(count);
        if let Some((first, rest)) = indexes.split_first() {
            steps.push(relink(first));
            steps.extend(run_all(rest, threads, relink));
        }
        let mut results = Vec::with_capacity(count);
        let mut waiting = Vec::new();
        for (index, step) in steps {
            match step {
                Step::Done(relinked) => results.push((index, relinked)),
                Step::Waiting(staged) => waiting.push((index, staged)),
            }
        }
        if !waiting.is_empty() {
            // a clone takes a name only once its blocks are on disk, so a failed flush names none
            // and each copy stays as it was
            let flushed = self.stop.check_io().and_then(|()| self.flush_copies());
            if let Err(error) = &flushed {
                tracing::debug!(%error, "could not flush the clones made to link copies");
            }
            let replace = |(index, staged): (usize, StagedLink)| {
                let _permit = grouped.enter(index);
                let relinked = if flushed.is_ok() && !self.stop.is_asked() {
                    staged.replace(|now| unchanged(index, now))
                } else {
                    Relinked::Kept
                };
                (index, relinked)
            };
            results.extend(run_owned(waiting, threads, replace));
        }
        in_order(count, results)
    }

    /// a link to `object` staged under a hidden name beside `destination`, by the first method
    /// placing would use that the disk allows, with whether it waits for the batch's flush. none
    /// when the disk refused each it tried, or allows none here; an error that refuses nothing,
    /// as a full disk, is returned, so the copy is tried again another time.
    fn stage_relink(
        &self,
        object: &Path,
        destination: &Path,
        batch: bool,
    ) -> io::Result<Option<StagedLink>> {
        self.stop.check_io()?;
        let location = Arc::new(self.location(destination, false)?);
        let hardlinks = self.synced.is_none();
        let early_hardlink = cfg!(windows) && hardlinks;
        let mut failed = None;
        let mut refused = |method: Method, error: io::Error| -> io::Result<()> {
            if !refuses_link(&error) {
                failed = Some(io::Error::new(error.kind(), error.to_string()));
            }
            self.learn(method, error, object)
        };
        if early_hardlink && self.can_hardlink.load(Ordering::Relaxed) {
            check_object(object)?;
            match self.stage_hardlink(&location, object) {
                Ok(staged) => return Ok(Some(StagedLink::new(Method::Hardlink, staged, false))),
                Err(error) => refused(Method::Hardlink, error)?,
            }
        }
        if self.can_reflink.load(Ordering::Relaxed) {
            check_object(object)?;
            match self.stage_clone(object, &location, batch) {
                Ok((staged, waits)) => {
                    return Ok(Some(StagedLink::new(Method::Reflink, staged, waits)));
                }
                Err(error) => refused(Method::Reflink, error)?,
            }
        }
        if !early_hardlink && hardlinks && self.can_hardlink.load(Ordering::Relaxed) {
            check_object(object)?;
            match self.stage_hardlink(&location, object) {
                Ok(staged) => return Ok(Some(StagedLink::new(Method::Hardlink, staged, false))),
                Err(error) => refused(Method::Hardlink, error)?,
            }
        }
        failed.map_or(Ok(None), Err)
    }

    /// a clone of `object` under a hidden name at `location`, with the mode a placed clone gets,
    /// and whether it waits for the batch's flush: on linux a clone is held to a copy's rule,
    /// since a crash can empty one whose blocks are not on disk yet.
    #[cfg(target_os = "linux")]
    fn stage_clone(
        &self,
        object: &Path,
        location: &Arc<Location>,
        batch: bool,
    ) -> io::Result<(Staged, bool)> {
        let source = read_unfollowed(object)?;
        let mode = clone_mode(&source.metadata()?.permissions(), false);
        let waits = batch && !Self::flushes_alone(location, batch);
        let (staged, file) = location.create(mode, !waits)?;
        self.seal_clone(&file, &source, mode)?;
        if waits {
            self.unflushed.store(true, Ordering::Relaxed);
        } else {
            file.sync_all()?;
        }
        Ok((staged, waits))
    }

    #[cfg(target_os = "macos")]
    #[expect(
        clippy::unused_self,
        reason = "linux clones through the placer, which a test can stand in for"
    )]
    fn stage_clone(
        &self,
        object: &Path,
        location: &Arc<Location>,
        _batch: bool,
    ) -> io::Result<(Staged, bool)> {
        let source = read_unfollowed(object)?;
        location.stage_clone(&source).map(|staged| (staged, false))
    }

    #[cfg(windows)]
    #[expect(
        clippy::unused_self,
        reason = "linux clones through the placer, which a test can stand in for"
    )]
    fn stage_clone(
        &self,
        object: &Path,
        location: &Arc<Location>,
        _batch: bool,
    ) -> io::Result<(Staged, bool)> {
        location.stage_clone(object).map(|staged| (staged, false))
    }

    /// deletes a temporary file through its parent; it is not a recorded placement.
    pub(crate) fn delete_file(&self, path: &Path) -> io::Result<()> {
        self.stop.check_io()?;
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
        self.stop.check_io()?;
        self.location(path, false)?.remove_dir()
    }

    /// clones `object` to `location`, failing when a file is there, under a hidden name with its
    /// final mode, as an interrupted clone would leave an empty file. a crash can empty a clone
    /// whose blocks are not on disk yet, so it takes its name only once they are, as a copy does:
    /// with `batch` it waits for [`Placer::flush_copies`], else `flush` flushes it on its own.
    #[cfg(target_os = "linux")]
    fn clone_at(
        &self,
        object: &Path,
        location: &Arc<Location>,
        writable: bool,
        batch: bool,
        flush: bool,
    ) -> io::Result<Put<Metadata>> {
        let source = read_unfollowed(object)?;
        let mode = clone_mode(&source.metadata()?.permissions(), writable);
        let seal = |file: &File| self.seal_clone(file, &source, mode);
        if batch {
            let (staged, file) = location.create(mode, false)?;
            seal(&file)?;
            self.unflushed.store(true, Ordering::Relaxed);
            return Ok(Put::Waiting(Method::Reflink, staged));
        }
        let metadata = publish_new(location, mode, |file| {
            seal(file)?;
            if flush { file.sync_all() } else { Ok(()) }
        })?;
        Ok(Put::Placed(Method::Reflink, metadata))
    }

    /// gives `file`, a new hidden file, a clone's `mode` and the blocks of `source`.
    #[cfg(target_os = "linux")]
    fn seal_clone(&self, file: &File, source: &File, mode: u32) -> io::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        if file.metadata()?.permissions().mode() & 0o7777 != mode {
            file.set_permissions(Permissions::from_mode(mode))?;
        }
        self.clone_into(file, source)
    }

    /// clones the blocks of `source` into `file`, or does what a test asks instead.
    #[cfg(target_os = "linux")]
    #[cfg_attr(
        not(test),
        expect(clippy::unused_self, reason = "tests clone through the placer")
    )]
    fn clone_into(&self, file: &File, source: &File) -> io::Result<()> {
        #[cfg(test)]
        if let Some(clone) = self.clone_with {
            return clone(file, source);
        }
        rustix::fs::ioctl_ficlone(file, source).map_err(io::Error::from)
    }

    /// gives `object` a second name hidden beside `location`, as [`Location::stage_link`] does.
    #[cfg_attr(
        not(test),
        expect(clippy::unused_self, reason = "tests refuse links through the placer")
    )]
    fn stage_hardlink(&self, location: &Arc<Location>, object: &Path) -> io::Result<Staged> {
        #[cfg(test)]
        if let Some(refusal) = self.link_refusal {
            return Err(refusal());
        }
        location.stage_link(object)
    }

    /// links `object` at `location` as [`link_object`] does.
    #[cfg_attr(
        not(test),
        expect(clippy::unused_self, reason = "tests refuse links through the placer")
    )]
    fn link(&self, location: &Location, object: &Path, checked: Metadata) -> io::Result<Metadata> {
        #[cfg(test)]
        if let Some(refusal) = self.link_refusal {
            return Err(refusal());
        }
        link_object(location, object, checked)
    }

    /// the filesystem under the root, read once the root exists.
    fn filesystem(&self) -> Filesystem {
        *self.filesystem.get_or_init(|| {
            Filesystem::chosen().unwrap_or_else(|| {
                self.location(&self.root.join(".filesystem"), false)
                    .map_or(Filesystem::Other, |location| location.filesystem())
            })
        })
    }

    /// why a file of `size` bytes cannot be placed on the root's FAT disk.
    fn too_large(&self, size: u64) -> io::Error {
        io::Error::new(
            io::ErrorKind::FileTooLarge,
            format!(
                "{}; use a folder on a disk formatted as exFAT, NTFS, APFS or ext4",
                crate::space::too_large(self.root.display(), "this file", size)
            ),
        )
    }

    fn location(&self, destination: &Path, make: bool) -> io::Result<Location> {
        let below = self.below_root(destination)?;
        let root = self.opened_at.as_deref().unwrap_or(&self.root);
        self.parents.locate(root, &below, self.checks_root, make)
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
        let path = self.root.join(self.below_root(path)?);
        let parent = path.parent().unwrap_or(self.root.as_path()).to_path_buf();
        Ok((path, parent))
    }

    /// `path` as a path below the root, whose folder must be the root or inside it, with no `..`
    /// below the root.
    fn below_root<'a>(&self, path: &'a Path) -> io::Result<Cow<'a, Path>> {
        let absolute;
        // unix keeps `..` when it makes a path absolute, so a path already absolute compares the
        // same as given; windows resolves `..` there, as it did in the root
        let (path, below) = if cfg!(unix) && path.is_absolute() {
            (path, path.strip_prefix(&self.root).map(Cow::Borrowed))
        } else {
            absolute = std::path::absolute(path)?;
            let below = absolute
                .strip_prefix(&self.root)
                .map(|below| Cow::Owned(below.to_path_buf()));
            (absolute.as_path(), below)
        };
        let below = below.ok().filter(|below| below.parent().is_some());
        let Some(below) = below else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} is outside {}", path.display(), self.root.display()),
            ));
        };
        if below
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a placement path cannot contain .. below its root",
            ));
        }
        Ok(below)
    }

    /// learns from `method` failing with `error` for the file at `from`: a refused method is not
    /// tried again, and an error no method can avoid, as an existing destination, is returned. a
    /// hardlink refusal holds for all only when the probe agrees, as `protected_hardlinks` may
    /// refuse one file alone.
    fn learn(&self, method: Method, error: io::Error, from: &Path) -> io::Result<()> {
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
                // the cache is to blame only where this drive makes links, and linux refuses a
                // link out of a read-only mount of this same drive the same way
                let blamed = !links_nothing();
                let read_only = blamed && (self.read_only)(from);
                self.read_only_cache.store(read_only, Ordering::Relaxed);
                self.other_volume
                    .store(blamed && !read_only, Ordering::Relaxed);
            }
            Method::Hardlink if refuses_links(&error) && links_nothing() => {
                self.can_hardlink.store(false, Ordering::Relaxed);
                self.read_only_cache.store(false, Ordering::Relaxed);
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
    run_work(items, workers(items.len(), cap), None, job)
}

/// runs `job` on every item in order on up to [`THREADS`] workers, one per `per` items, for
/// coarse items such as the cache's 256 fan folders, each holding many objects.
pub(crate) fn run_coarse<T: Sync, R: Send>(
    items: &[T],
    cap: Threads,
    per: usize,
    job: impl Fn(&T) -> R + Sync,
) -> Vec<R> {
    let threads = crate::threads::available_parallelism().map_or(1, NonZero::get);
    run_work(
        items,
        cap.limit(threads.min(THREADS).min(items.len() / per.max(1))),
        None,
        job,
    )
}

/// runs hashing work in item order, using bytes as well as file count to repay worker startup,
/// reading `ahead` of the workers when given.
pub(crate) fn run_hashes<T: Sync, R: Send>(
    items: &[T],
    bytes: u64,
    cap: Threads,
    ahead: Option<&ReadAhead<'_, T>>,
    job: impl Fn(&T) -> R + Sync,
) -> Vec<R> {
    let processors = crate::threads::available_parallelism().map_or(1, NonZero::get);
    run_work(
        items,
        hash_workers(items.len(), bytes, processors, cap),
        ahead,
        job,
    )
}

/// small ceilings keep their existing hash-buffer count; larger groups can spend spare CPUs.
fn hash_workers(items: usize, bytes: u64, processors: usize, cap: Threads) -> usize {
    let by_count = items / PER_THREAD;
    let wanted = if cap.limit(THREADS) > 2 {
        let by_bytes = usize::try_from(bytes / HASH_BYTES).unwrap_or(usize::MAX);
        by_count.max(by_bytes)
    } else {
        by_count
    };
    cap.limit(processors.min(THREADS).min(wanted.max(1)))
        .min(items)
}

fn run_work<T: Sync, R: Send>(
    items: &[T],
    threads: usize,
    ahead: Option<&ReadAhead<'_, T>>,
    job: impl Fn(&T) -> R + Sync,
) -> Vec<R> {
    let take = |index: usize| {
        if let Some(ahead) = ahead {
            ahead.took(index);
        }
        items.get(index)
    };
    if threads < 2 {
        return (0..items.len())
            .map_while(|index| take(index).map(&job))
            .collect();
    }
    let next = AtomicUsize::new(0);
    let done: Vec<Vec<(usize, R)>> = std::thread::scope(|scope| {
        // collected so every worker starts before the first is joined, not one at a time
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                crate::threads::scoped(scope, || {
                    let mut done = Vec::new();
                    loop {
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        let Some(item) = take(index) else {
                            return done;
                        };
                        done.push((index, job(item)));
                    }
                })
            })
            .collect();
        workers
            .into_iter()
            .map(crate::threads::Scoped::join)
            .collect()
    });
    in_order(items.len(), done.into_iter().flatten())
}

/// how many files past the one it takes a worker checking files asks the kernel to read, when
/// they are out of memory: on ext4 over nvme, 256 gave `cache verify` and `status --verify` of
/// 10,000 and 100,000 small files 0.32 to 0.41 of their time, where 64 gave 0.41 to 0.46.
const READ_AHEAD: usize = 256;

/// the path of an item's file, borrowed where the item holds one.
pub(crate) type FilePath<'a, T> = &'a (dyn for<'x> Fn(&'x T) -> Cow<'x, Path> + Sync);

/// asks the kernel to read the files of `items` the workers take next, so a cold disk has more
/// reads waiting than there are workers: on each take, those up to [`READ_AHEAD`] past it not
/// asked for yet. a hint, which reads nothing into the process and takes no thread.
pub(crate) struct ReadAhead<'a, T> {
    items: &'a [T],
    path: FilePath<'a, T>,
    /// the items before this one were asked for, or taken.
    asked: AtomicUsize,
    advise: fn(&Path),
}

impl<'a, T> ReadAhead<'a, T> {
    /// reading ahead of `items` when [`cold`] finds them out of memory. in memory the hints gain
    /// nothing, and an open for each cost `cache verify` 43% more time on tmpfs.
    pub(crate) fn when_cold(items: &'a [T], path: FilePath<'a, T>) -> Option<Self> {
        cold(items, path).then(|| ReadAhead {
            items,
            path,
            asked: AtomicUsize::new(0),
            advise,
        })
    }

    /// asks for the files past `taken`, the item a worker just took.
    pub(crate) fn took(&self, taken: usize) {
        let upto = taken.saturating_add(1 + READ_AHEAD).min(self.items.len());
        let from = self.asked.fetch_max(upto, Ordering::Relaxed).max(taken + 1);
        for item in self.items.get(from..upto).unwrap_or_default() {
            (self.advise)(&(self.path)(item));
        }
    }
}

/// asks the kernel to read the file at `path` into memory, without waiting for it.
#[cfg(target_os = "linux")]
fn advise(path: &Path) {
    use rustix::fs::OFlags;
    use std::os::unix::fs::OpenOptionsExt;
    // a fifo would hold an open for reading until something wrote to it
    if let Ok(file) = fs::OpenOptions::new()
        .read(true)
        .custom_flags((OFlags::NOFOLLOW | OFlags::NONBLOCK).bits().cast_signed())
        .open(path)
    {
        let _ = rustix::fs::fadvise(&file, 0, None, rustix::fs::Advice::WillNeed);
    }
}

#[cfg(not(target_os = "linux"))]
fn advise(_: &Path) {}

/// whether a quarter or more of up to 8 files spread through `items` are out of memory, by a read
/// of each that may not wait for the disk. a filesystem that cannot say, as tmpfs, holds them.
#[cfg(target_os = "linux")]
fn cold<T>(items: &[T], path: FilePath<'_, T>) -> bool {
    use rustix::fs::OFlags;
    use std::os::unix::fs::OpenOptionsExt;
    let probes = items.len().min(8);
    let mut out = 0;
    for at in 0..probes {
        let Ok(file) = fs::OpenOptions::new()
            .read(true)
            .custom_flags((OFlags::NOFOLLOW | OFlags::NONBLOCK).bits().cast_signed())
            .open(path(&items[at * items.len() / probes]))
        else {
            continue;
        };
        let mut byte = [0; 1];
        let read = rustix::io::preadv2(
            &file,
            &mut [io::IoSliceMut::new(&mut byte)],
            0,
            rustix::io::ReadWriteFlags::NOWAIT,
        );
        if read == Err(rustix::io::Errno::AGAIN) {
            out += 1;
        }
    }
    out > 0 && out * 4 >= probes
}

#[cfg(not(target_os = "linux"))]
fn cold<T>(_: &[T], _: FilePath<'_, T>) -> bool {
    false
}

/// `results`, each tagged with the index of its item among `count`, in the order of the items.
/// each result moves twice, where sorting 100,000 large results moved each many times.
fn in_order<R>(count: usize, results: impl IntoIterator<Item = (usize, R)>) -> Vec<R> {
    let mut slots: Vec<Option<R>> = std::iter::repeat_with(|| None).take(count).collect();
    for (index, result) in results {
        if let Some(slot) = slots.get_mut(index) {
            *slot = Some(result);
        }
    }
    slots.into_iter().flatten().collect()
}

/// the most threads placing into `filesystem` takes, within `threads`: at most [`BTRFS_THREADS`]
/// on btrfs.
fn placing_threads(threads: Threads, filesystem: Filesystem) -> Threads {
    if filesystem == Filesystem::Btrfs {
        threads.at_most(BTRFS_THREADS)
    } else {
        threads
    }
}

/// how many threads share `items` items of work.
fn workers(items: usize, cap: Threads) -> usize {
    let threads = crate::threads::available_parallelism().map_or(1, NonZero::get);
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
        // collected so every worker starts before the first is joined, not one at a time
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

/// the orders, each with its folder's number, reordered so neighbours go to different folders,
/// since links made into one folder wait on each other.
fn spread(orders: impl IntoIterator<Item = (usize, usize)>) -> Vec<usize> {
    let mut seen: Vec<usize> = Vec::new();
    let mut ranked: Vec<(usize, usize)> = orders
        .into_iter()
        .map(|(index, group)| {
            if seen.len() <= group {
                seen.resize(group + 1, 0);
            }
            seen[group] += 1;
            (seen[group], index)
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

/// how files were placed, by `clones`, hardlinks `links` and `copies`, which hold `bytes`:
/// `cloned from the cache`, `linked to the cache`, `copied, 1.2 KiB`, or each way used, as in
/// `cloned and copied, 1.2 KiB`. a clone shares the cached copy's blocks until either is written,
/// so it is never called linked.
pub(crate) fn placed_how(clones: u64, links: u64, copies: u64, bytes: u64) -> String {
    let ways: Vec<&str> = [(clones, "cloned"), (links, "linked"), (copies, "copied")]
        .into_iter()
        .filter(|(count, _)| *count > 0)
        .map(|(_, way)| way)
        .collect();
    match ways.as_slice() {
        [] | ["linked"] => "linked to the cache".to_owned(),
        ["cloned"] => "cloned from the cache".to_owned(),
        [before @ .., last] => {
            let listed = if before.is_empty() {
                (*last).to_owned()
            } else {
                format!("{} and {last}", before.join(", "))
            };
            if copies > 0 {
                format!("{listed}, {}", crate::units::size(bytes))
            } else {
                format!("{listed} from the cache")
            }
        }
    }
}

/// removes the file at `path` below `root` through a handle on its folder, opened from `root`
/// without following a link, though `root` itself may be one, as a cache folder someone named.
#[cfg(unix)]
pub(crate) fn remove_below(root: &Path, path: &Path) -> io::Result<()> {
    let below = path.strip_prefix(root).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not inside {}", path.display(), root.display()),
        )
    })?;
    Parents::default()
        .locate(root, below, false, false)?
        .remove_file()
}

/// the folders between `root` and `path`, held until the result is dropped so none can be
/// replaced meanwhile, though `root` itself may be a link; a link among them is refused.
#[cfg(windows)]
pub(crate) fn hold_below(root: &Path, path: &Path) -> io::Result<impl Sized + use<>> {
    let below = path.strip_prefix(root).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not inside {}", path.display(), root.display()),
        )
    })?;
    Parents::default().locate(root, below, false, false)
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

/// a link or a file where the cache keeps a folder of its own, which fetchloom never puts there.
#[derive(Debug)]
pub(crate) struct NotCacheFolder {
    pub(crate) path: PathBuf,
    pub(crate) link: bool,
}

impl std::fmt::Display for NotCacheFolder {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let what = if self.link { "a link" } else { "a file" };
        write!(
            formatter,
            "{} is {what}, and the cache keeps only real folders",
            self.path.display()
        )
    }
}

impl std::error::Error for NotCacheFolder {}

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
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                NotCacheFolder {
                    path: folder.to_path_buf(),
                    link: is_link,
                },
            ));
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

/// links `object` at `location` and returns the object's metadata, `checked` before the link: a
/// hardlink is the same file, so its new name needs no look of its own.
fn link_object(location: &Location, object: &Path, checked: Metadata) -> io::Result<Metadata> {
    location.link(object)?;
    Ok(checked)
}

/// the object's metadata, once it is seen to be a plain file.
fn check_object(object: &Path) -> io::Result<Metadata> {
    let metadata = fs::symlink_metadata(object)?;
    if is_plain(&metadata) {
        return Ok(metadata);
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        format!(
            "{} is a link or a special file, which fetchloom never places",
            object.display()
        ),
    ))
}

/// copies `object` to `location`, failing when a file is there, writable or read-only as asked.
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
    stop.check_io()?;
    staged.publish()?;
    #[cfg(windows)]
    {
        if let Err(error) = set_named_writable(&file, location.path(), writable) {
            let _ = fs::remove_file(location.path());
            return Err(error);
        }
    }
    closed_look(file, location)
}

/// how a copy looks once `file`, the handle that wrote it at `location`, is closed. windows
/// stamps a written file's time when that handle closes, and fat keeps it coarser than an open
/// handle shows, so there the copy is read by name after closing, as git does on windows.
#[cfg(windows)]
fn closed_look(file: File, location: &Location) -> io::Result<Metadata> {
    drop(file);
    location.metadata()
}

/// how a copy looks once `file`, the handle that wrote it, is closed: unix reports an open
/// file's times as a later look by name does.
#[cfg(not(windows))]
#[expect(
    clippy::needless_pass_by_value,
    reason = "windows reads the look only once the handle closed"
)]
fn closed_look(file: File, _: &Location) -> io::Result<Metadata> {
    file.metadata()
}

fn stage_copy_at(
    object: &Path,
    location: &Arc<Location>,
    writable: bool,
    threads: Threads,
    stop: &Stop,
) -> io::Result<(Staged, File)> {
    stop.check_io()?;
    let (staged, file) = location.create(0o666, false)?;
    // publishing clears windows attributes, so seal through the claimed handle afterward
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
    waiting: Vec<(usize, Method, Staged)>,
    flush: impl FnOnce() -> io::Result<()>,
    threads: Threads,
    seen: &(impl Fn(&Metadata) -> T + Sync),
    stop: &Stop,
    grouped: &schedule::Work,
) -> Vec<(usize, io::Result<(Method, T)>)> {
    if waiting.is_empty() {
        return Vec::new();
    }
    if let Err(err) = stop.check_io().and_then(|()| flush()) {
        return waiting
            .into_iter()
            .map(|(index, ..)| {
                let message = format!("could not flush the copied files to disk: {err}");
                (index, Err(io::Error::new(err.kind(), message)))
            })
            .collect();
    }
    let publish = &|(index, method, staged): (usize, Method, Staged)| {
        let _permit = grouped.enter(index);
        let named = stop
            .check_io()
            .and_then(|()| staged.publish())
            .map(|metadata| (method, seen(&metadata)));
        (index, named)
    };
    let mut waiting = waiting.into_iter();
    let first = waiting.next().map(publish);
    let rest = run_owned(waiting.collect(), threads, publish);
    first.into_iter().chain(rest).collect()
}

/// the mode a clone of a file of `permissions` gets: its read and run bits, and its owner's write
/// bit as asked, never the setuid, setgid or sticky bit, which would let others run a placed file
/// as its owner, nor a write bit for others, which a co-user may have set on an object they wrote.
#[cfg(unix)]
fn clone_mode(permissions: &Permissions, writable: bool) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    let owner = if writable { 0o200 } else { 0 };
    permissions.mode() & 0o555 | owner
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

/// a new name of the kind [`is_copy_temporary`] knows, `.fetchloom-<16 hex>.copy`, for a file
/// written before it takes its own, which a later sync sweeps away if it never does.
pub(crate) fn copy_temporary_name() -> String {
    format!(".fetchloom-{:016x}.copy", fastrand::u64(..))
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
        // a reader per copy would multiply the placement worker cap
        copy_stoppable(&mut input, &mut output, stop)?;
    } else {
        transfer(input, output, stop)?;
    }
    stop.check_io()?;
    set_file_writable(output, writable)
}

/// sets the placed copy `output` writable as asked, through its own handle, and on unix without
/// the setuid, setgid or sticky bit, keeping the rest of the mode the umask gave it.
fn set_file_writable(output: &File, writable: bool) -> io::Result<()> {
    #[cfg(unix)]
    {
        set_mode(output, writable, copy_mode)
    }
    #[cfg(not(unix))]
    {
        let before = output.metadata()?.permissions();
        let after = writable_as(before.clone(), writable);
        if before != after {
            output.set_permissions(after)?;
        }
        Ok(())
    }
}

/// [`set_file_writable`] for a macos clone, which takes its object's mode: as [`clone_mode`]
/// says, so no bit a co-user set on an object they wrote reaches the placed file.
#[cfg(target_os = "macos")]
pub(super) fn set_clone_writable(output: &File, writable: bool) -> io::Result<()> {
    set_mode(output, writable, clone_mode)
}

/// sets the mode of `output` to what `mode` makes of it, writable as asked, through its handle.
#[cfg(unix)]
fn set_mode(output: &File, writable: bool, mode: fn(&Permissions, bool) -> u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let before = output.metadata()?.permissions();
    let after = mode(&before, writable);
    if before.mode() & 0o7777 != after {
        output.set_permissions(Permissions::from_mode(after))?;
    }
    Ok(())
}

/// the mode a copy made with `permissions` keeps: the bits the umask gave it, its owner's write bit
/// as asked, no write bit for anyone when read-only, never the setuid, setgid or sticky bit.
#[cfg(unix)]
fn copy_mode(permissions: &Permissions, writable: bool) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    let mode = permissions.mode() & 0o777;
    if writable {
        mode | 0o200
    } else {
        mode & !0o222
    }
}

/// [`set_file_writable`] for a file named at `path`, by that name too where windows ignored the
/// handle: clearing read-only through it leaves a file whose only attribute it was with none,
/// which windows takes as no change.
#[cfg(windows)]
fn set_named_writable(file: &File, path: &Path, writable: bool) -> io::Result<()> {
    set_file_writable(file, writable)?;
    let now = file.metadata()?.permissions();
    if now.readonly() == writable {
        fs::set_permissions(path, writable_as(now, writable))?;
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
    stop.check_io()?;
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

fn copy_stoppable(mut input: impl Read, mut output: impl Write, stop: &Stop) -> io::Result<()> {
    loop {
        stop.check_io()?;
        if io::copy(&mut input.by_ref().take(BLOCK as u64), &mut output)? == 0 {
            return Ok(());
        }
    }
}

/// copies `input` into `output` a block at a time, reading the next block on another thread while
/// this one writes the last, which keeps pace with the system's own copy: 1 GiB took 0.62 s
/// against 0.99 s one block at a time on an nvme laptop.
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
            // a refused reader runs at join, when there is no concurrent queue consumer
            if std::thread::current().id() == caller {
                return copy_stoppable(&mut input, output, stop);
            }
            while let Ok(mut block) = spare.recv() {
                block.resize(BLOCK, 0);
                let read = stop.check_io().and_then(|()| input.read(&mut block));
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
                stop.check_io()?;
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
#[cfg(not(unix))]
fn writable_as(mut permissions: Permissions, writable: bool) -> Permissions {
    permissions.set_readonly(!writable);
    permissions
}

#[cfg(test)]
mod tests;
