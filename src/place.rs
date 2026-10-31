//! putting cached objects where a project expects them, by reflink, else hardlink, else copy.

mod synced;

use std::collections::{HashMap, HashSet};
use std::fs::{self, File, Metadata, Permissions};
use std::io;
#[cfg(not(target_os = "linux"))]
use std::io::{Read, Write};
use std::num::NonZero;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use serde::{Deserialize, Serialize};

use crate::cache::read_unfollowed;
use crate::threads::Threads;
use crate::volume::{Volume, crosses_volumes, is_plain, refuses_links};

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
    threads: Threads,
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
    volume: Volume,
    /// on linux 5.8 and later, the root's filesystem, opened before the first copy so one syncfs
    /// flushes every copy and reports a write-back that failed since. a flush per copy cost 33
    /// times the placing of 10,000 small copies.
    #[cfg(target_os = "linux")]
    batch: std::sync::OnceLock<Option<File>>,
    #[cfg(target_os = "linux")]
    unflushed: AtomicBool,
}

/// one file to place: the object, where it goes, and whether it must be a writable copy.
pub(crate) struct Order {
    pub(crate) object: PathBuf,
    pub(crate) destination: PathBuf,
    pub(crate) writable: bool,
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
    /// sits inside a folder a sync client owns. `scratch` is a folder of fetchloom's own on the
    /// same volume, such as the project's `.fetchloom`, where a removal may test whether the
    /// volume can hardlink.
    pub(crate) fn new(root: &Path, scratch: &Path) -> Placer {
        let root = std::path::absolute(root).unwrap_or_else(|_| root.to_path_buf());
        let synced = synced::client(&root, &|name| std::env::var_os(name));
        Placer {
            volume: Volume::new(scratch.to_path_buf()),
            ..Placer::starting_at(root, Method::Reflink, synced)
        }
    }

    /// prepares placement under a folder someone named, which may be a link to a folder. the
    /// folders below it must still be real. it is the one folder fetchloom writes to on its
    /// volume, so a removal tests there, with a hidden file, whether the volume can hardlink.
    pub(crate) fn in_named_folder(root: &Path) -> Placer {
        Placer {
            checks_root: false,
            ..Placer::new(root, root)
        }
    }

    /// a placer that skips the methods above `first`, as if the filesystem had refused them.
    fn starting_at(root: PathBuf, first: Method, synced: Option<&'static str>) -> Placer {
        Placer {
            volume: Volume::new(root.clone()),
            threads: Threads::default(),
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
        }
    }

    pub(crate) fn with_threads(mut self, threads: Threads) -> Placer {
        self.threads = threads;
        self
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
    pub(crate) fn place(
        &mut self,
        object: &Path,
        destination: &Path,
        writable: bool,
    ) -> io::Result<Method> {
        self.check_folders(destination, true)?;
        self.put(object, destination, writable)
    }

    /// places every order and says how each went, in order, with what `seen` keeps of the metadata
    /// of each placed file, read right after it was placed, so a later edit is never taken for the
    /// placed file. the first goes alone, so it learns which methods the filesystem allows before
    /// the rest go several at once. orders are rebuilt for checks and placement so only active
    /// workers keep their paths.
    pub(crate) fn place_all<T: Send>(
        &mut self,
        count: usize,
        order: impl Fn(usize) -> Order + Sync,
        seen: impl Fn(&Metadata) -> T + Sync,
    ) -> Vec<io::Result<(Method, T)>> {
        let mut results = Vec::with_capacity(count);
        let ready = spread((0..count).filter_map(|index| {
            let order = order(index);
            match self.check_folders(&order.destination, true) {
                Ok(()) => Some((index, order)),
                Err(err) => {
                    results.push((index, Err(err)));
                    None
                }
            }
        }));
        let this = &*self;
        // keeping a whole Metadata for each of 100,000 files cost tens of megabytes
        let put = |index: &usize| {
            let order = order(*index);
            let placed = this
                .put(&order.object, &order.destination, order.writable)
                .and_then(|method| Ok((method, seen(&fs::symlink_metadata(&order.destination)?))));
            (*index, placed)
        };
        if let Some((first, rest)) = ready.split_first() {
            results.push(put(first));
            results.extend(run_all(rest, self.threads, put));
        }
        results.sort_unstable_by_key(|(index, _)| *index);
        results.into_iter().map(|(_, result)| result).collect()
    }

    /// tries each method the filesystem may allow, best first.
    fn put(&self, object: &Path, destination: &Path, writable: bool) -> io::Result<Method> {
        if self.can_reflink.load(Ordering::Relaxed) {
            check_object(object)?;
            match reflink_copy::reflink(object, destination) {
                Ok(()) => {
                    seal(destination, writable)?;
                    return Ok(Method::Reflink);
                }
                Err(error) => self.learn(Method::Reflink, error)?,
            }
        }
        // sync clients break hardlinks or upload every link as a full file.
        if self.can_hardlink.load(Ordering::Relaxed) && !writable && self.synced.is_none() {
            check_object(object)?;
            match fs::hard_link(object, destination) {
                Ok(()) => return Ok(Method::Hardlink),
                Err(error) => self.learn(Method::Hardlink, error)?,
            }
        }
        let flush = !self.batches_copies();
        copy(object, destination, writable, self.threads, flush)?;
        #[cfg(target_os = "linux")]
        if !flush {
            self.unflushed.store(true, Ordering::Relaxed);
        }
        Ok(Method::Copy)
    }

    /// whether copies may skip their own flush, because [`Placer::flush_copies`] flushes them all.
    #[cfg(target_os = "linux")]
    fn batches_copies(&self) -> bool {
        self.batch
            .get_or_init(|| {
                let release = fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default();
                crate::cache::syncfs_reports_errors(&release)
                    .then(|| File::open(&self.root).ok())
                    .flatten()
            })
            .is_some()
    }

    #[cfg(not(target_os = "linux"))]
    #[expect(clippy::unused_self, reason = "only linux batches copies")]
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
        }
        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    #[expect(
        clippy::unused_self,
        clippy::unnecessary_wraps,
        reason = "other systems flush each copy as it is made"
    )]
    pub(crate) fn flush_copies(&self) -> io::Result<()> {
        Ok(())
    }

    /// checks every folder between the root and `path`, refusing a link among them, so a link
    /// inside the data folder can never send a file elsewhere. with `make`, missing folders are
    /// made one at a time. each folder is checked once.
    pub(crate) fn check_folders(&mut self, path: &Path, make: bool) -> io::Result<()> {
        let (_, parent) = self.within(path)?;
        if self.folders.contains(&parent) {
            return Ok(());
        }
        if !self.folders.contains(&self.root) {
            if make {
                fs::create_dir_all(&self.root)?;
            }
            if self.checks_root {
                real_folder(&self.root, FolderUse::Place { make: false })?;
            }
            self.folders.insert(self.root.clone());
        }
        let mut folder = self.root.clone();
        for part in parent
            .strip_prefix(&self.root)
            .unwrap_or(&parent)
            .components()
        {
            folder.push(part);
            if !self.folders.contains(&folder) {
                real_folder(&folder, FolderUse::Place { make })?;
                self.folders.insert(folder.clone());
            }
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
        let (path, parent) = self.within(path)?;
        let mut folder = self.root.clone();
        if self.checks_root {
            real_folder(&folder, FolderUse::Place { make: false })?;
        }
        for part in parent
            .strip_prefix(&self.root)
            .unwrap_or(&parent)
            .components()
        {
            folder.push(part);
            real_folder(&folder, FolderUse::Place { make: false })?;
        }
        if placed == Some(Method::Hardlink) {
            self.volume.remove_hardlink(&path)?;
        } else {
            self.volume.remove(&path)?;
        }
        let mut folder = path.parent();
        while prune && let Some(current) = folder {
            if current == self.root || fs::remove_dir(current).is_err() {
                break;
            }
            self.folders.remove(current);
            folder = current.parent();
        }
        Ok(())
    }

    /// `path` made absolute, and its folder, which must be the root or inside it.
    fn within(&self, path: &Path) -> io::Result<(PathBuf, PathBuf)> {
        let path = std::path::absolute(path)?;
        match path.parent() {
            Some(parent) if parent.starts_with(&self.root) => {
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
        let links_nothing = || self.volume.can_hardlink() == Some(false);
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
    let threads = std::thread::available_parallelism().map_or(1, NonZero::get);
    let threads = cap.limit(threads.min(THREADS).min(items.len() / PER_THREAD));
    if threads < 2 {
        return items.iter().map(job).collect();
    }
    let next = AtomicUsize::new(0);
    let mut done: Vec<(usize, R)> = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                scope.spawn(|| {
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
            .flat_map(|worker| {
                worker
                    .join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
            })
            .collect()
    });
    done.sort_unstable_by_key(|(index, _)| *index);
    done.into_iter().map(|(_, result)| result).collect()
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
fn copy(
    object: &Path,
    destination: &Path,
    writable: bool,
    threads: Threads,
    flush: bool,
) -> io::Result<()> {
    let destination = std::path::absolute(destination)?;
    let parent = fs::canonicalize(destination.parent().unwrap_or_else(|| Path::new(".")))?;
    let destination = parent.join(destination.file_name().unwrap_or_default());
    let (temporary, claimed) = loop {
        let temporary = parent.join(format!(".fetchloom-{:016x}.copy", fastrand::u64(..)));
        match crate::volume::create_hidden(&temporary) {
            Ok(file) => break (temporary, file),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
            Err(err) => return Err(err),
        }
    };
    let temporary = tempfile::TempPath::try_from_path(temporary)?;
    // publishing clears windows attributes, so seal through the claimed handle afterward.
    #[cfg(windows)]
    let staged_writable = true;
    #[cfg(not(windows))]
    let staged_writable = writable;
    fill(object, &claimed, staged_writable, threads)?;
    if flush {
        claimed.sync_all()?;
    }
    temporary
        .persist_noclobber(&destination)
        .map_err(|error| error.error)?;
    #[cfg(windows)]
    undo_on_error(set_file_writable(&claimed, writable), &destination)?;
    Ok(())
}

/// whether a file has the reserved name of an unfinished placement copy.
pub(crate) fn is_copy_temporary(name: &str) -> bool {
    name.strip_prefix(".fetchloom-")
        .and_then(|name| name.strip_suffix(".copy"))
        .is_some_and(|random| {
            random.len() == 16 && random.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
}

/// copies the bytes of `object` into `output` and seals it as asked, all through the handle, so
/// a link swapped in for the name `output` claimed is never written through.
fn fill(object: &Path, mut output: &File, writable: bool, threads: Threads) -> io::Result<()> {
    let mut input = read_unfollowed(object)?;
    if threads.is_limited() {
        // a reader per copy would multiply the placement worker cap.
        io::copy(&mut input, &mut output)?;
    } else {
        transfer(input, output)?;
    }
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
fn transfer(mut input: File, mut output: &File) -> io::Result<()> {
    io::copy(&mut input, &mut output).map(drop)
}

#[cfg(not(target_os = "linux"))]
fn transfer(mut input: File, mut output: &File) -> io::Result<()> {
    match usize::try_from(input.metadata()?.len()) {
        Ok(size) if size <= BLOCK => {
            let mut bytes = Vec::with_capacity(size);
            input.read_to_end(&mut bytes)?;
            output.write_all(&bytes)
        }
        _ => overlapped(input, output),
    }
}

/// how much a large copy reads at a time.
#[cfg(not(target_os = "linux"))]
const BLOCK: usize = 1 << 20;

/// copies `input` into `output` a block at a time, reading the next block on another thread
/// while this one writes the last. with the disk never waiting on the other side, a copy keeps
/// pace with the operating system's own: 1 GiB took 0.62 s against 0.99 s one block at a time
/// on an nvme laptop.
#[cfg(not(target_os = "linux"))]
fn overlapped(mut input: File, mut output: &File) -> io::Result<()> {
    let (full, filled) = std::sync::mpsc::sync_channel::<io::Result<Vec<u8>>>(2);
    let (empty, spare) = std::sync::mpsc::sync_channel::<Vec<u8>>(3);
    for _ in 0..3 {
        let _ = empty.send(vec![0; BLOCK]);
    }
    std::thread::scope(move |scope| {
        scope.spawn(move || {
            while let Ok(mut block) = spare.recv() {
                block.resize(BLOCK, 0);
                let read = input.read(&mut block);
                let sent = match read {
                    Ok(0) => return,
                    Ok(read) => {
                        block.truncate(read);
                        full.send(Ok(block))
                    }
                    Err(err) => full.send(Err(err)),
                };
                if sent.is_err() {
                    return;
                }
            }
        });
        let mut written = Ok(());
        while let Ok(block) = filled.recv() {
            written = block.and_then(|block| {
                output.write_all(&block)?;
                let _ = empty.send(block);
                Ok(())
            });
            if written.is_err() {
                break;
            }
        }
        // the reader stops once nobody takes its blocks, so the scope never waits on it
        drop(filled);
        drop(empty);
        written
    })
}

/// makes a new reflink at `destination` writable or read-only, as asked. a hardlink is never
/// sealed, because its flag is the object's.
fn seal(destination: &Path, writable: bool) -> io::Result<()> {
    undo_on_error(set_writable(destination, writable), destination)
}

fn set_writable(path: &Path, writable: bool) -> io::Result<()> {
    let permissions = writable_as(fs::metadata(path)?.permissions(), writable);
    fs::set_permissions(path, permissions)
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

/// removes the file just made at `destination` when a later step failed, so placing it again
/// starts clean.
fn undo_on_error<T>(result: io::Result<T>, destination: &Path) -> io::Result<T> {
    if result.is_err() {
        let _ = fs::remove_file(destination);
    }
    result
}

#[cfg(test)]
mod tests;
