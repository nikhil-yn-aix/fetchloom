//! a mirror on a disk this machine reads, as a lab nas or a usb drive. its files are copied, never
//! linked, so a change there never reaches a project. [`Target`] writes one, checking each file.

use std::collections::{BTreeSet, HashMap};
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, SystemTime};

use super::{LARGEST_TREE, MARKER, MARKER_LINE, Wanted, below};
use crate::cache::{Cache, Copied, Stored};
use crate::digest::{Block, Hasher, Sha256};
use crate::place::Placer;
use crate::stop::Stop;
use crate::threads::Threads;
use crate::volume::{is_plain, read_unfollowed};

/// copies from or to one folder mirror running at once, so a nas is not flooded.
const COPIES: usize = 8;

/// why a folder mirror cannot be used this run.
pub(crate) enum Missing {
    /// nothing is there, as when its disk is not mounted.
    Folder,
    /// the folder holds no marker, so it is not a mirror's top, or it is the empty folder a disk
    /// mounts on.
    Marker,
    Unreadable(io::Error),
}

/// what a folder mirror gave for one wanted file.
pub(crate) enum Gave {
    /// the pinned bytes, now in the cache.
    Stored(Stored),
    /// nothing at the file's place.
    Absent,
    /// a copy of another size or with other bytes, or a link or a special file in its place.
    Damaged,
    Unreadable(io::Error),
}

pub(crate) enum Tree {
    Bytes(Vec<u8>),
    /// none there, or one too large to read, so the archive itself is needed.
    Absent,
    /// a link or a special file in its place.
    Damaged,
}

/// checks that a mirror is at `root`: a folder whose marker's first line is [`MARKER_LINE`].
pub(crate) fn probe(root: &Path) -> Result<(), Missing> {
    match fs::metadata(root) {
        Ok(found) if found.is_dir() => {}
        Ok(_) => return Err(Missing::Folder),
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Err(Missing::Folder),
        Err(err) => return Err(Missing::Unreadable(err)),
    }
    let file = match read_unfollowed(&root.join(MARKER)) {
        Ok(file) => file,
        Err(err)
            if matches!(
                err.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::InvalidInput
            ) =>
        {
            return Err(Missing::Marker);
        }
        Err(err) => return Err(Missing::Unreadable(err)),
    };
    let mut first = String::new();
    // a marker that is not text is no marker
    let _ = file.take(256).read_to_string(&mut first);
    if first.lines().next().map(str::trim) == Some(MARKER_LINE) {
        Ok(())
    } else {
        Err(Missing::Marker)
    }
}

/// copies each of `wanted` the folder mirror at `root` holds into the cache, each on the disk
/// before any takes its name, saying what each gave in order. it fails on a cache write or a stop.
pub(crate) fn copy_all(
    cache: &Cache,
    root: &Path,
    wanted: &[Wanted],
    threads: Threads,
    stop: &Stop,
) -> io::Result<Vec<Gave>> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let batch = if wanted.len() > crate::cache::MOST_FLUSHED_ALONE {
        cache.batch()?
    } else {
        None
    };
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let flush = batch.is_none();
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let flush = true;
    let copied = crate::place::run_all(wanted, threads.at_most(COPIES), |want| {
        cache.copy_in(
            &place(root, "objects", &want.digest),
            &want.digest,
            want.size,
            flush,
            stop,
        )
    });
    let mut gave = Vec::with_capacity(wanted.len());
    let mut sealed = Vec::new();
    for copied in copied {
        gave.push(match copied? {
            Copied::Sealed(object) => {
                sealed.push((gave.len(), object));
                Gave::Absent
            }
            Copied::Unread(err) if err.kind() == io::ErrorKind::NotFound => Gave::Absent,
            Copied::Unread(err) => Gave::Unreadable(err),
            Copied::OtherSize | Copied::OtherBytes | Copied::NotPlain => Gave::Damaged,
        });
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if let Some(batch) = batch {
        batch.flush()?;
    }
    for (index, object) in sealed {
        gave[index] = Gave::Stored(object.publish(cache)?);
    }
    Ok(gave)
}

/// the tree whose bytes hash to `tree` on the folder mirror at `root`, read a block at a time and
/// never past [`LARGEST_TREE`]. it fails on a read or a stop.
pub(crate) fn read_tree(root: &Path, tree: &Sha256, stop: &Stop) -> io::Result<Tree> {
    let path = place(root, "trees", tree);
    let mut file = match read_unfollowed(&path) {
        Ok(file) => file,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Tree::Absent),
        Err(err) if err.kind() == io::ErrorKind::InvalidInput => return Ok(Tree::Damaged),
        Err(err) => return Err(err),
    };
    let mut bytes = Vec::new();
    let mut block = vec![0; 1 << 20];
    loop {
        stop.check_io()?;
        let read = match file.read(&mut block) {
            Ok(0) => return Ok(Tree::Bytes(bytes)),
            Ok(read) => read,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        };
        if (bytes.len() + read) as u64 > LARGEST_TREE {
            tracing::debug!(path = %path.display(), "a tree past 64 MiB, so the archive itself is needed");
            return Ok(Tree::Absent);
        }
        bytes.extend_from_slice(&block[..read]);
    }
}

/// a copy on a folder mirror as it was when it hashed to its name: its size and modified time. its
/// number is left out, since a network disk may give a file a new one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Seen {
    size: u64,
    modified: Option<SystemTime>,
}

impl Seen {
    fn of(found: &fs::Metadata) -> Seen {
        Seen {
            size: found.len(),
            modified: found.modified().ok(),
        }
    }
}

/// what a folder mirror holds of one wanted file, read whole and hashed now.
#[derive(Debug)]
pub(crate) enum Held {
    /// a copy whose bytes hash to its name, as it was then.
    Verified(Seen),
    /// nothing at the file's place.
    Absent,
    /// a copy of another size or with other bytes, a link or a special file in its place, or a
    /// copy that changed while it was read.
    Damaged,
    Unreadable(io::Error),
}

/// what the folder mirror at `root` holds of each of `wanted`, in order, each copy opened without
/// following a link and hashed whole from its disk where the system allows. `read` hears the bytes
/// each look settles; only a stop fails it.
pub(crate) fn check_all(
    root: &Path,
    wanted: &[Wanted],
    threads: Threads,
    read: &(dyn Fn(u64) + Sync),
    stop: &Stop,
) -> io::Result<Vec<Held>> {
    let held = crate::place::run_all(wanted, threads.at_most(COPIES), |want| {
        let mut settled = 0;
        let held = check(
            &place(root, "objects", &want.digest),
            want,
            stop,
            &mut |bytes| {
                settled += bytes;
                read(bytes);
            },
        );
        read(want.size.saturating_sub(settled));
        held
    });
    stop.check_io()?;
    Ok(held)
}

/// what the copy at `path` is of `want`, read as [`check_all`] reads it, telling `read` each
/// block's size.
fn check(path: &Path, want: &Wanted, stop: &Stop, read: &mut dyn FnMut(u64)) -> Held {
    match fs::symlink_metadata(path) {
        Ok(found) if is_plain(&found) && found.len() == want.size => {}
        Ok(_) => return Held::Damaged,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Held::Absent,
        Err(err) => return Held::Unreadable(err),
    }
    let mut file = match read_unfollowed(path) {
        Ok(file) => file,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Held::Absent,
        // a link or a special file put in its place since the look
        Err(err) if err.kind() == io::ErrorKind::InvalidInput => return Held::Damaged,
        Err(err) => return Held::Unreadable(err),
    };
    let before = match file.metadata() {
        Ok(found) if found.len() == want.size => Seen::of(&found),
        Ok(_) => return Held::Damaged,
        Err(err) => return Held::Unreadable(err),
    };
    past_memory(&file);
    let sum = hashed(&mut file, want.size.saturating_add(1), stop, |block| {
        read(block.len() as u64);
        true
    });
    match sum {
        Ok((size, digest)) if size == want.size && digest == want.digest => {}
        Ok(_) => return Held::Damaged,
        Err(err) => return Held::Unreadable(err),
    }
    match file.metadata() {
        Ok(found) if Seen::of(&found) == before => Held::Verified(before),
        Ok(_) => Held::Damaged,
        Err(err) => Held::Unreadable(err),
    }
}

/// whether the copy of `digest` at `root` still looks as `seen` says, by one look and no read.
pub(crate) fn still(root: &Path, digest: &Sha256, seen: &Seen) -> bool {
    fs::symlink_metadata(place(root, "objects", digest))
        .is_ok_and(|found| is_plain(&found) && Seen::of(&found) == *seen)
}

/// the folder a push writes files in before they take their names.
const TMP: &str = "tmp";

/// a push of this many files or more writes them in 16 folders below `tmp/`, by the first digit
/// of their hash, since in one folder every writer waits on its lock, and the kernel spins
/// meanwhile; fewer files pay more for making the folders than they save.
const SHARD_FROM: usize = 500;

/// how long a killed push's unfinished file stays in `tmp/` before a later push removes it.
const STALE: Duration = Duration::from_hours(24);

/// a file a push writes: a pinned file under [`OBJECTS`](super::OBJECTS) or an archive's file list
/// under [`TREES`](super::TREES), named by the SHA-256 its bytes must have.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Item {
    pub(crate) folder: &'static str,
    pub(crate) digest: Sha256,
    pub(crate) size: u64,
}

/// what a folder mirror holds at an item's name, after one look.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum There {
    Absent,
    /// a plain file of the item's size, taken for its bytes: whoever reads it checks them.
    Present,
    /// a file of another size, a link, or anything else that is no plain file.
    Damaged,
}

/// what a folder named as a mirror to push to is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum State {
    /// a mirror: its marker says so.
    Mirror,
    /// not there yet in a folder that is, or empty: a push starts a mirror there.
    New,
}

/// why a folder cannot take a push.
#[derive(Debug)]
pub(crate) enum Refused {
    /// it holds files, and no marker this fetchloom reads says it is a mirror.
    NotEmpty,
    /// neither it nor the folder above it is there, as when a disk is not mounted.
    NoParent,
    /// a file, or anything else that is no folder, stands in its place.
    NotAFolder,
    Failed(io::Error),
}

/// what writing one item gave.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Put {
    /// written, checked and given its name.
    Copied,
    /// written in place of a damaged copy, which then went.
    Replaced,
    /// another push gave the name a file of the item's size first, and that file stays.
    Present,
}

/// why an item was not written.
#[derive(Debug)]
pub(crate) enum Fault {
    /// the bytes to push could not be read.
    Source(io::Error),
    /// the bytes to push are not the ones the item names.
    OtherBytes,
    /// the mirror's copy, read back from its disk, holds other bytes than were written.
    ReadBack,
    /// another file took the place of the one written before it was read back: another account's,
    /// one of another size, or on windows any with a new number.
    Replaced,
    /// writing the mirror failed, or a stop was asked, which is `Interrupted`.
    Mirror(io::Error),
}

/// why a push's files were not all written and named.
#[derive(Debug)]
pub(crate) enum Failed {
    /// the file at this place among them, and why.
    File(usize, Fault),
    /// the flush that puts them all on the disk, or a stop asked before it, `Interrupted`.
    #[cfg_attr(
        all(not(test), not(any(target_os = "linux", target_os = "macos"))),
        expect(
            dead_code,
            reason = "windows flushes each file as it writes it, so no batch flush fails there"
        )
    )]
    Flush(io::Error),
}

/// one flush of a filesystem for a whole batch of new files, as the cache makes its new objects
/// durable. windows has none, so each file there is flushed as it is written.
#[cfg(any(target_os = "linux", target_os = "macos"))]
type Batch = crate::cache::Batch;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
type Batch = std::convert::Infallible;

/// a file a push wrote into `tmp/` and hashed, not named yet. dropped before it is named, it
/// goes.
struct Written<'a> {
    temp: Temporary<'a>,
    item: Item,
    there: There,
    /// the file as written, so the file read back is that one.
    identity: Identity,
}

/// which file a pushed file is: its device and its number there, and on unix the account that
/// owns it.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Identity {
    device: u64,
    number: u64,
    #[cfg(unix)]
    owner: u32,
}

/// what is at `root`, a folder named as a mirror to push to, read without changing anything, or why
/// a push cannot write there. a folder holding only a stopped push's `tmp/` is still new.
pub(crate) fn look(root: &Path) -> Result<State, Refused> {
    match probe(root) {
        Ok(()) => return Ok(State::Mirror),
        Err(Missing::Unreadable(err)) => return Err(Refused::Failed(err)),
        Err(Missing::Folder) => {
            #[cfg(test)]
            tests::between_looks(root);
            match fs::metadata(root) {
                // a push starting this mirror at the same time made the folder since
                Ok(found) if found.is_dir() => {}
                Ok(_) => return Err(Refused::NotAFolder),
                Err(err) if err.kind() == io::ErrorKind::NotFound => {
                    let parent = std::path::absolute(root)
                        .ok()
                        .and_then(|root| root.parent().map(Path::to_path_buf));
                    return if parent.is_some_and(|parent| parent.is_dir()) {
                        Ok(State::New)
                    } else {
                        Err(Refused::NoParent)
                    };
                }
                Err(err) => return Err(Refused::Failed(err)),
            }
        }
        Err(Missing::Marker) => {}
    }
    let mut names = Vec::new();
    for entry in fs::read_dir(root).map_err(Refused::Failed)? {
        names.push(entry.map_err(Refused::Failed)?.file_name());
    }
    let tmp = || fs::symlink_metadata(root.join(TMP)).is_ok_and(|found| found.is_dir());
    if names.is_empty() || (names.len() == 1 && names[0] == TMP && tmp()) {
        return Ok(State::New);
    }
    // a push starting this mirror at the same time writes its marker before anything else
    if probe(root).is_ok() {
        return Ok(State::Mirror);
    }
    Err(Refused::NotEmpty)
}

/// looks once at the name of each of `items` in the folder mirror at `root`, as [`look`] found it,
/// and says what is there, in order. each folder is checked once through the mirror's top, so a
/// link among them fails it, as a name that cannot be looked at does; a new mirror holds nothing.
pub(crate) fn survey(
    root: &Path,
    state: State,
    items: &[Item],
    threads: Threads,
) -> io::Result<Vec<There>> {
    if state == State::New {
        return Ok(vec![There::Absent; items.len()]);
    }
    let placer = Placer::in_named_folder(root);
    // whether the folder of each item is there, by the item's folder and its hash's first byte
    let mut folders: HashMap<(&str, u8), bool> = HashMap::new();
    for item in items {
        let key = (item.folder, item.digest.bytes()[0]);
        if folders.contains_key(&key) {
            continue;
        }
        let name = place(root, item.folder, &item.digest);
        let there = match placer.check_parent(&name) {
            Ok(()) => true,
            Err(err) if err.kind() == io::ErrorKind::NotFound => false,
            Err(err) => return Err(no_folder(root, &name, err)),
        };
        folders.insert(key, there);
    }
    let looked = crate::place::run_all(items, threads.at_most(COPIES), |item| {
        if !folders[&(item.folder, item.digest.bytes()[0])] {
            return Ok(There::Absent);
        }
        match fs::symlink_metadata(place(root, item.folder, &item.digest)) {
            Ok(found) if is_plain(&found) && found.len() == item.size => Ok(There::Present),
            Ok(_) => Ok(There::Damaged),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(There::Absent),
            Err(err) => Err(err),
        }
    });
    looked.into_iter().collect()
}

/// `err`, from opening the folder `name` goes in below `root`, in words that say what is wrong
/// with that folder.
fn no_folder(root: &Path, name: &Path, err: io::Error) -> io::Error {
    let Some(folder) = name
        .ancestors()
        .skip(1)
        .take_while(|folder| *folder != root)
        .filter(|folder| {
            fs::symlink_metadata(folder).is_ok_and(|found| !found.is_dir() || found.is_symlink())
        })
        .last()
    else {
        return err;
    };
    let what = if fs::symlink_metadata(folder).is_ok_and(|found| found.is_symlink()) {
        "a link"
    } else {
        "no folder"
    };
    io::Error::new(
        err.kind(),
        format!(
            "{} is {what}, and push writes only into real folders",
            folder.display()
        ),
    )
}

/// a folder mirror a push writes to, every folder reached through a handle on its top without
/// following a link, so a link someone planted cannot send a write elsewhere.
pub(crate) struct Target {
    root: PathBuf,
    /// never stopped itself, so a file left unfinished by a stop is still removed.
    placer: Placer,
    stop: std::sync::Arc<Stop>,
    /// how many files make a push write them in the folders below `tmp/`, [`SHARD_FROM`].
    shard_from: usize,
    /// whether this push writes them there, as [`Target::prepare`] decided.
    sharded: bool,
}

impl Target {
    /// opens the folder mirror at `root` for writing, starting one, folder then marker, when
    /// [`look`] finds none, and says whether it did; or why a push cannot write there.
    pub(crate) fn open(root: &Path, stop: std::sync::Arc<Stop>) -> Result<(Target, bool), Refused> {
        let state = look(root)?;
        let mut target = Target {
            root: root.to_path_buf(),
            placer: Placer::in_named_folder(root),
            stop,
            shard_from: SHARD_FROM,
            sharded: false,
        };
        if state == State::Mirror {
            return Ok((target, false));
        }
        match fs::create_dir(root) {
            Err(err) if err.kind() != io::ErrorKind::AlreadyExists => {
                return Err(Refused::Failed(err));
            }
            _ => {}
        }
        let started = target.start().map_err(Refused::Failed)?;
        Ok((target, started))
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// writes the marker, unless another push wrote it first, and puts it on the disk, its name
    /// too, before any file is pushed. says whether this one wrote it.
    fn start(&mut self) -> io::Result<bool> {
        self.placer
            .check_folders(&self.root.join(TMP).join(MARKER), true)?;
        let line = format!("{MARKER_LINE}\n");
        let mut temp = self.temporary(None)?;
        temp.file()?.write_all(line.as_bytes())?;
        flush(temp.file()?)?;
        temp.close();
        match self.placer.move_back(&temp.path, &self.root.join(MARKER)) {
            Ok(()) => {
                temp.named = true;
                self.keep_marker()?;
                Ok(true)
            }
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists && probe(&self.root).is_ok() => {
                Ok(false)
            }
            Err(err) => Err(err),
        }
    }

    /// flushes the mirror's top folder once it gained the marker's name, since a power cut could
    /// keep pushed files and lose the marker, and every later push would then refuse the folder.
    fn keep_marker(&self) -> io::Result<()> {
        #[cfg(all(test, target_os = "linux"))]
        if let Some(folder) = tests::failing(tests::Failing::MarkerFlush, &self.root) {
            return flush(&folder);
        }
        crate::project::sync_directory(&self.root)
    }

    /// makes `tmp/`, the folders below it when `items` are many, and the folders `items` go in,
    /// each checked once through the mirror's top, so a link among them fails it, never followed.
    pub(crate) fn prepare(&mut self, items: &[Item]) -> io::Result<()> {
        self.sharded = items.len() >= self.shard_from;
        let tmp = self.root.join(TMP).join(MARKER);
        let shards = (0..16)
            .filter(|_| self.sharded)
            .map(|shard| self.root.join(TMP).join(format!("{shard:x}")).join(MARKER));
        let names = std::iter::once(tmp).chain(shards).chain(
            items
                .iter()
                .map(|item| place(&self.root, item.folder, &item.digest)),
        );
        for name in names {
            if let Err(err) = self.placer.check_folders(&name, true) {
                return Err(no_folder(&self.root, &name, err));
            }
        }
        Ok(())
    }

    /// gives `temp`, holding `item`'s checked bytes, the item's name without taking it from another
    /// file: a damaged copy moves aside until then, and a file another push named meanwhile stays.
    fn publish(&self, temp: &mut Temporary<'_>, item: &Item, there: There) -> Result<Put, Fault> {
        let name = place(&self.root, item.folder, &item.digest);
        let aside = match there {
            There::Damaged => match self.placer.move_aside(&name) {
                Ok(aside) => Some(aside),
                Err(err) if err.kind() == io::ErrorKind::NotFound => None,
                Err(err) => return Err(Fault::Mirror(err)),
            },
            There::Absent | There::Present => None,
        };
        let named = self.placer.move_back(&temp.path, &name);
        if named.is_ok() {
            temp.named = true;
        }
        let taken = matches!(&named, Err(err) if err.kind() == io::ErrorKind::AlreadyExists);
        if let Some(aside) = &aside {
            if named.is_ok() || taken {
                self.remove_aside(aside);
            } else {
                // the damaged copy gets its name back, so a failed push leaves the mirror as it was
                let _ = self.placer.move_back(aside, &name);
            }
        }
        match named {
            Ok(()) if aside.is_some() => Ok(Put::Replaced),
            Ok(()) => Ok(Put::Copied),
            Err(_) if taken && holds(&name, item.size) => Ok(Put::Present),
            Err(err) => Err(Fault::Mirror(err)),
        }
    }

    /// removes a damaged copy moved aside, a file or, as one never is, a folder.
    fn remove_aside(&self, aside: &Path) {
        if self.placer.delete_file(aside).is_err() {
            let _ = self.placer.delete_folder(aside);
        }
    }

    /// writes each of `items` from `source` where [`survey`] found what it says, saying what each
    /// gave, in order. every file goes read-only into `tmp/` and onto the disk before any is read
    /// back, hashed again and named, so a name never points at bytes not on the disk. `written` and
    /// `named` hear each file's size; [`Target::prepare`] makes the folders first. on a failure,
    /// files named before it keep their names and the others go.
    pub(crate) fn put_all<S: Read>(
        &self,
        items: &[(Item, There)],
        threads: Threads,
        source: &(dyn Fn(&Item) -> io::Result<S> + Sync),
        written: &(dyn Fn(u64) + Sync),
        named: &(dyn Fn(u64) + Sync),
    ) -> Result<Vec<Put>, Failed> {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        let batch = if items.len() > crate::cache::MOST_FLUSHED_ALONE {
            crate::cache::batch_in(&self.root.join(TMP)).map_err(Failed::Flush)?
        } else {
            None
        };
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        let batch: Option<Batch> = None;
        #[cfg(all(test, target_os = "linux"))]
        let batch = tests::failing_batch(&self.root).or(batch);
        let failed = AtomicBool::new(false);
        let threads = threads.at_most(COPIES);
        let wrote = crate::place::run_all(items, threads, |(item, there)| {
            if failed.load(Ordering::Relaxed) {
                return None;
            }
            let result = source(item)
                .map_err(Fault::Source)
                .and_then(|mut bytes| self.write(item, &mut bytes, *there, batch.as_ref()));
            match &result {
                Ok(_) => written(item.size),
                Err(_) => failed.store(true, Ordering::Relaxed),
            }
            Some(result)
        });
        let pending = waiting(wrote)?;
        #[cfg(test)]
        if tests::killed_before_naming(&self.root, &pending) {
            // a killed push runs no destructors, so what it wrote stays where it was
            std::mem::forget(pending);
            return Err(Failed::Flush(io::ErrorKind::Interrupted.into()));
        }
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        if let Some(batch) = batch {
            batch.flush().map_err(Failed::Flush)?;
        }
        let puts = crate::place::run_all(&pending, threads, |slot| {
            if failed.load(Ordering::Relaxed) {
                return None;
            }
            let one = slot.lock().unwrap_or_else(PoisonError::into_inner).take()?;
            let size = one.item.size;
            let result = self.name(one);
            match &result {
                Ok(_) => named(size),
                Err(_) => failed.store(true, Ordering::Relaxed),
            }
            Some(result)
        });
        gathered(puts)
    }

    /// writes `item` from `source` into a new read-only file in `tmp/`, hashed as it is written,
    /// and puts it on the disk now, or leaves that to `batch`'s one flush, as [`settle`] says.
    fn write(
        &self,
        item: &Item,
        source: &mut impl Read,
        there: There,
        batch: Option<&Batch>,
    ) -> Result<Written<'_>, Fault> {
        let shard = self.sharded.then(|| item.digest.hex()[..1].to_owned());
        let mut temp = self.temporary(shard.as_deref()).map_err(Fault::Mirror)?;
        #[cfg(debug_assertions)]
        held(&self.stop);
        let file = temp.file().map_err(Fault::Mirror)?;
        let most = item.size.saturating_add(1);
        if copy_hashed(source, file, most, &self.stop)? != (item.size, item.digest) {
            return Err(Fault::OtherBytes);
        }
        settle(file, batch).map_err(Fault::Mirror)?;
        #[cfg(test)]
        tests::before_read_back(file);
        let identity = identity(file).map_err(Fault::Mirror)?;
        // a batch of thousands of files must not hold one open each
        temp.close();
        Ok(Written {
            temp,
            item: *item,
            there,
            identity,
        })
    }

    /// reads `written` back from its disk, which it is on now, hashes it again and gives it its
    /// name: never when the file read back is not the one written, or holds other bytes.
    fn name(&self, mut written: Written<'_>) -> Result<Put, Fault> {
        let item = written.item;
        let mut file = read_unfollowed(&written.temp.path).map_err(Fault::Mirror)?;
        let found = identity(&file).map_err(Fault::Mirror)?;
        if found != written.identity {
            #[cfg(unix)]
            renumbered(&file, found, &written)?;
            #[cfg(not(unix))]
            return Err(Fault::Replaced);
        }
        let most = item.size.saturating_add(1);
        if read_back(&mut file, most, &self.stop).map_err(Fault::Mirror)?
            != (item.size, item.digest)
        {
            return Err(Fault::ReadBack);
        }
        drop(file);
        self.publish(&mut written.temp, &item, written.there)
    }

    /// removes unfinished push files in `tmp/` and the folders below it a day older than `now`,
    /// and nothing else there; a link in place of such a folder is never followed.
    pub(crate) fn sweep(&self, now: SystemTime) {
        let Ok(entries) = fs::read_dir(self.root.join(TMP)) else {
            return;
        };
        for entry in entries.flatten() {
            let shard = entry.file_name().to_str().is_some_and(is_shard)
                && entry.file_type().is_ok_and(|kind| kind.is_dir());
            if !shard {
                self.sweep_one(&entry, now);
            } else if let Ok(inner) = fs::read_dir(entry.path()) {
                for entry in inner.flatten() {
                    self.sweep_one(&entry, now);
                }
            }
        }
    }

    fn sweep_one(&self, entry: &fs::DirEntry, now: SystemTime) {
        let unfinished = entry.file_name().to_str().is_some_and(is_unfinished);
        let old = entry
            .metadata()
            .and_then(|found| found.modified())
            .is_ok_and(|modified| now.duration_since(modified).is_ok_and(|age| age > STALE));
        if unfinished && old {
            let _ = self.placer.delete_file(&entry.path());
        }
    }

    /// puts the names a push gave on the disk, the folders `written` went in then the mirror's own,
    /// failing on one it cannot flush. a name lost to a power cut before this only costs a copy.
    pub(crate) fn flush(&self, written: &[Item]) -> io::Result<()> {
        let mut folders: BTreeSet<PathBuf> = BTreeSet::new();
        for item in written {
            let name = place(&self.root, item.folder, &item.digest);
            folders.extend(name.parent().map(Path::to_path_buf));
            folders.insert(self.root.join(item.folder));
        }
        folders.insert(self.root.clone());
        for folder in folders {
            crate::project::sync_directory(&folder)?;
        }
        Ok(())
    }

    /// a new read-only file in `tmp/`, or in its folder `shard`, named `.<16 hex digits>.part`.
    fn temporary(&self, shard: Option<&str>) -> io::Result<Temporary<'_>> {
        let folder = self.root.join(TMP);
        let folder = shard.map_or_else(|| folder.clone(), |shard| folder.join(shard));
        loop {
            let path = folder.join(format!(".{:016x}.part", fastrand::u64(..)));
            match self.placer.create_file(&path, true) {
                Ok(file) => {
                    return Ok(Temporary {
                        target: self,
                        path,
                        file: Some(file),
                        named: false,
                    });
                }
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
                Err(err) => return Err(err),
            }
        }
    }
}

/// a file a push is writing in the mirror's `tmp/`. dropped before it takes its name, it goes.
struct Temporary<'a> {
    target: &'a Target,
    path: PathBuf,
    file: Option<File>,
    named: bool,
}

impl Temporary<'_> {
    fn file(&mut self) -> io::Result<&mut File> {
        self.file
            .as_mut()
            .ok_or_else(|| io::Error::other("the file was closed"))
    }

    /// closes the file, which windows needs before it takes its name.
    fn close(&mut self) {
        self.file = None;
    }
}

impl Drop for Temporary<'_> {
    fn drop(&mut self) {
        if !self.named {
            self.close();
            let _ = self.target.placer.delete_file(&self.path);
        }
    }
}

/// whether `name` is that of a file a push writes in `tmp/` before it takes its own.
/// whether `name` is one of the folders below `tmp/` a large push writes in.
fn is_shard(name: &str) -> bool {
    matches!(name.as_bytes(), [b'0'..=b'9' | b'a'..=b'f'])
}

fn is_unfinished(name: &str) -> bool {
    name.strip_prefix('.')
        .and_then(|name| name.strip_suffix(".part"))
        .is_some_and(|random| {
            random.len() == 16 && random.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
}

/// whether `name` is a plain file of `size` bytes, which a mirror's reader takes for its bytes
/// after checking them.
fn holds(name: &Path, size: u64) -> bool {
    fs::symlink_metadata(name).is_ok_and(|found| is_plain(&found) && found.len() == size)
}

/// whether the folder mirror at `root` holds a plain file of `size` bytes for `digest`, unread.
pub(crate) fn has(root: &Path, digest: &Sha256, size: u64) -> bool {
    holds(&place(root, super::OBJECTS, digest), size)
}

/// the files `wrote` gives, each in a slot a worker takes it from to name it, or the first that
/// failed, in order. the others go with `wrote`.
fn waiting(
    wrote: Vec<Option<Result<Written<'_>, Fault>>>,
) -> Result<Vec<Mutex<Option<Written<'_>>>>, Failed> {
    let mut pending = Vec::with_capacity(wrote.len());
    for (at, one) in wrote.into_iter().enumerate() {
        match one {
            Some(Ok(written)) => pending.push(Mutex::new(Some(written))),
            Some(Err(fault)) => return Err(Failed::File(at, fault)),
            None => {}
        }
    }
    Ok(pending)
}

/// what naming each file gave, in order, or the first that failed.
fn gathered(puts: Vec<Option<Result<Put, Fault>>>) -> Result<Vec<Put>, Failed> {
    let mut gave = Vec::with_capacity(puts.len());
    for (at, put) in puts.into_iter().enumerate() {
        match put {
            Some(Ok(put)) => gave.push(put),
            Some(Err(fault)) => return Err(Failed::File(at, fault)),
            None => {}
        }
    }
    Ok(gave)
}

/// puts `file`, just written, on its disk now unless `batch`'s one flush will: on linux when the
/// file is on its filesystem; on macos after a plain fsync, the batch emptying the drive's cache.
fn settle(file: &File, batch: Option<&Batch>) -> io::Result<()> {
    let Some(batch) = batch else {
        return flush(file);
    };
    #[cfg(target_os = "linux")]
    {
        if batch.covers(&file.metadata()?) {
            Ok(())
        } else {
            flush(file)
        }
    }
    #[cfg(target_os = "macos")]
    {
        let _ = batch;
        rustix::fs::fsync(file).map_err(Into::into)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    match *batch {}
}

/// lets `file`, opened again by `written`'s temporary name, be read back and named though its
/// number changed, as on a network disk that forgot it: smb with `noserverino`, sshfs without
/// `use_ino`. it must be the item's size and owned by the written file's account, so no other can
/// change it once named, else [`Fault::Replaced`], and is made read-only and flushed first.
#[cfg(unix)]
fn renumbered(file: &File, found: Identity, written: &Written<'_>) -> Result<(), Fault> {
    let held = file.metadata().map_err(Fault::Mirror)?;
    if found.owner != written.identity.owner || held.len() != written.item.size {
        return Err(Fault::Replaced);
    }
    read_only(file, held.permissions(), &written.temp.path);
    #[cfg(all(test, target_os = "linux"))]
    if let Some(handle) = tests::failing(tests::Failing::RenumberedFlush, &written.temp.path) {
        return flush(&handle).map_err(Fault::Mirror);
    }
    flush(file).map_err(Fault::Mirror)
}

/// makes `file`, at `path`, read-only through its own handle. a disk that refuses, as some network
/// disks do, leaves it as it was, and it still takes its name, its bytes checked all the same.
#[cfg(unix)]
fn read_only(file: &File, mut permissions: fs::Permissions, path: &Path) {
    permissions.set_readonly(true);
    #[cfg(all(test, target_os = "linux"))]
    let refusing = tests::failing(tests::Failing::ReadOnly, path);
    #[cfg(all(test, target_os = "linux"))]
    let file = refusing.as_ref().unwrap_or(file);
    if let Err(err) = file.set_permissions(permissions) {
        tracing::debug!(%err, file = %path.display(), "could not make a pushed file read-only");
    }
}

/// which file `file` is, so a file opened again by its name can be told to be the one written.
fn identity(file: &File) -> io::Result<Identity> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let found = file.metadata()?;
        Ok(Identity {
            device: found.dev(),
            number: found.ino(),
            owner: found.uid(),
        })
    }
    #[cfg(windows)]
    {
        let found = winapi_util::file::information(file)?;
        Ok(Identity {
            device: found.volume_serial_number(),
            number: found.file_index(),
        })
    }
}

/// holds a copy whose part is in `tmp/` until a stop or the file `FETCHLOOM_TEST_HOLD_PUSH` names
/// appears, so a CLI test can stop a push mid-copy on any disk. release builds never read it.
#[cfg(debug_assertions)]
fn held(stop: &Stop) {
    let Some(release) = std::env::var_os("FETCHLOOM_TEST_HOLD_PUSH").map(PathBuf::from) else {
        return;
    };
    while !stop.is_asked() && !release.exists() {
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// copies and hashes up to `most` bytes of `source` into `file`, looking for a stop between blocks.
fn copy_hashed(
    source: &mut impl Read,
    file: &mut File,
    most: u64,
    stop: &Stop,
) -> Result<(u64, Sha256), Fault> {
    let mut wrote = Ok(());
    let read = hashed(source, most, stop, |block| {
        wrote = file.write_all(block);
        wrote.is_ok()
    });
    wrote.map_err(Fault::Mirror)?;
    match read {
        Err(err) if err.kind() == io::ErrorKind::Interrupted => Err(Fault::Mirror(err)),
        read => read.map_err(Fault::Source),
    }
}

/// up to `most` bytes of `file` hashed from its start, read from its disk as [`past_memory`] asks.
fn read_back(file: &mut File, most: u64, stop: &Stop) -> io::Result<(u64, Sha256)> {
    past_memory(file);
    file.seek(SeekFrom::Start(0))?;
    hashed(file, most, stop, |_| true)
}

/// asks that `file` be read from its disk: on linux its pages are dropped from memory first, and
/// macos is asked to keep none of what is read. a network disk may still answer from memory.
fn past_memory(file: &File) {
    #[cfg(target_os = "linux")]
    if let Err(err) = rustix::fs::fadvise(file, 0, None, rustix::fs::Advice::DontNeed) {
        tracing::debug!(%err, "could not drop a mirror's file from memory");
    }
    #[cfg(target_os = "macos")]
    if let Err(err) = rustix::fs::fcntl_nocache(file, true) {
        tracing::debug!(%err, "could not read a mirror's file past memory");
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let _ = file;
}

/// hashes up to `most` bytes of `source` a block at a time, handing each to `each` until it says
/// no. a stop asked between blocks is `Interrupted`.
fn hashed(
    source: &mut impl Read,
    most: u64,
    stop: &Stop,
    mut each: impl FnMut(&[u8]) -> bool,
) -> io::Result<(u64, Sha256)> {
    let mut block = Block::sized(most);
    let block = block.get();
    let mut hasher = Hasher::default();
    let mut read = 0_u64;
    while read < most {
        stop.check_io()?;
        let want = usize::try_from(most - read).map_or(block.len(), |left| left.min(block.len()));
        let got = match source.read(&mut block[..want]) {
            Ok(0) => break,
            Ok(got) => got,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        };
        if !each(&block[..got]) {
            break;
        }
        hasher.update(&block[..got]);
        read += got as u64;
    }
    Ok((read, hasher.finish().0))
}

/// puts what `file` holds on its disk. macos flushes the drive's own cache too, unless the drive
/// takes no such flush, as some network ones do, which then get the plain one.
fn flush(file: &File) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        match rustix::fs::fcntl_fullfsync(file) {
            Err(rustix::io::Errno::NOTSUP | rustix::io::Errno::INVAL) => {
                rustix::fs::fsync(file).map_err(Into::into)
            }
            result => result.map_err(Into::into),
        }
    }
    #[cfg(not(target_os = "macos"))]
    file.sync_all()
}

/// where the folder mirror at `root` keeps `kind`, `objects` or `trees`, for `digest`.
fn place(root: &Path, kind: &str, digest: &Sha256) -> PathBuf {
    below(kind, digest)
        .iter()
        .fold(root.to_path_buf(), |path, part| path.join(part))
}

#[cfg(test)]
pub(crate) mod tests {
    use std::cell::Cell;
    use std::sync::Arc;

    use super::*;
    use crate::mirror::OBJECTS;

    thread_local! {
        /// whether a pushed file's first byte is changed between writing it and reading it back,
        /// as a disk that loses a write would leave it.
        static LOSING_A_WRITE: Cell<bool> = const { Cell::new(false) };
        /// what happens between a push writing its files and naming them.
        static BETWEEN: Cell<Between> = const { Cell::new(Between::Nothing) };
        /// which of a push's flushes fails, as a disk whose write-back failed makes it fail.
        #[cfg(target_os = "linux")]
        static FAILING: Cell<Option<Failing>> = const { Cell::new(None) };
        /// what another push does to a mirror's folder between a look that found none and the
        /// next, once.
        static BETWEEN_LOOKS: Cell<Option<fn(&Path)>> = const { Cell::new(None) };
    }

    /// runs what a test has another push do between two looks at `root`.
    pub(super) fn between_looks(root: &Path) {
        if let Some(other) = BETWEEN_LOOKS.take() {
            other(root);
        }
    }

    /// what a test makes happen between a push writing its files and naming them.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Between {
        Nothing,
        /// the push is killed.
        Kill,
        /// each file comes back under its temporary name as a new file of the same bytes, as a
        /// disk that forgot the file's number and gave it a new one shows it.
        Renumber,
        /// as `Renumber`, with the first byte changed.
        Alter,
        /// as `Renumber`, one byte longer.
        Grow,
        /// as `Renumber`, by another account: the push takes the file it wrote for that account's,
        /// since a test cannot make a file another account owns.
        #[cfg(unix)]
        Foreign,
    }

    /// whether the push is killed once it has written its files, `pending`, and before it names
    /// them in `root`, as a test asks, which may instead put another file in each one's place.
    pub(super) fn killed_before_naming(
        root: &Path,
        pending: &[Mutex<Option<Written<'_>>>],
    ) -> bool {
        let between = BETWEEN.get();
        match between {
            Between::Nothing => return false,
            Between::Kill => return true,
            _ => {}
        }
        // listed first: fat and exfat list a file made during the walk, here the swapped copy
        let paths: Vec<PathBuf> = fs::read_dir(root.join(TMP))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        for path in paths {
            let mut bytes = fs::read(&path).unwrap();
            match between {
                Between::Alter => bytes[0] ^= 1,
                Between::Grow => bytes.push(b'\n'),
                _ => {}
            }
            fs::rename(&path, path.with_extension("swapped")).unwrap();
            fs::write(&path, bytes).unwrap();
        }
        #[cfg(unix)]
        if between == Between::Foreign {
            for slot in pending {
                if let Some(written) = slot.lock().unwrap().as_mut() {
                    written.identity.owner = written.identity.owner.wrapping_add(1);
                }
            }
        }
        #[cfg(not(unix))]
        let _ = pending;
        false
    }

    /// a step of a push that a test can make fail.
    #[cfg(target_os = "linux")]
    #[derive(Clone, Copy, PartialEq, Eq)]
    pub(super) enum Failing {
        /// the flush of a new mirror's top folder, once the marker has its name.
        MarkerFlush,
        /// the one flush of a batch of files.
        BatchFlush,
        /// the flush of a file that came back with a new number, before it is read back.
        RenumberedFlush,
        /// making a file that came back with a new number read-only.
        ReadOnly,
    }

    /// a handle on `path` that `step` fails on, when a test makes it fail: fsync, syncfs and
    /// fchmod refuse an `O_PATH` handle, which is a real error without a failing disk.
    #[cfg(target_os = "linux")]
    pub(super) fn failing(step: Failing, path: &Path) -> Option<File> {
        (FAILING.get() == Some(step)).then(|| {
            let handle = rustix::fs::open(
                path,
                rustix::fs::OFlags::PATH | rustix::fs::OFlags::CLOEXEC,
                rustix::fs::Mode::empty(),
            )
            .unwrap();
            File::from(handle)
        })
    }

    /// a batch whose flush fails, when a test asks.
    #[cfg(target_os = "linux")]
    pub(super) fn failing_batch(root: &Path) -> Option<Batch> {
        failing(Failing::BatchFlush, &root.join(TMP)).map(crate::cache::Batch::with_file_for_test)
    }

    /// changes the file a push wrote before it is read back, when a test asks.
    pub(super) fn before_read_back(mut file: &File) {
        if LOSING_A_WRITE.get() {
            file.seek(SeekFrom::Start(0)).unwrap();
            file.write_all(b"X").unwrap();
        }
    }

    /// makes a folder mirror at `root` holding `files`, with its marker.
    pub(crate) fn mirror(root: &Path, files: &[&[u8]]) {
        fs::create_dir_all(root).unwrap();
        fs::write(root.join(MARKER), format!("{MARKER_LINE}\n")).unwrap();
        for bytes in files {
            let path = place(root, "objects", &crate::digest::sha256(bytes));
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, bytes).unwrap();
        }
    }

    fn wanted(bytes: &[u8]) -> Wanted {
        Wanted {
            digest: crate::digest::sha256(bytes),
            size: bytes.len() as u64,
            label: String::from_utf8_lossy(bytes).into_owned(),
        }
    }

    #[test]
    fn a_folder_without_its_marker_is_no_mirror() {
        let folder = tempfile::tempdir().unwrap();
        let root = folder.path().join("mirror");
        assert!(matches!(probe(&root), Err(Missing::Folder)));
        fs::create_dir(&root).unwrap();
        assert!(matches!(probe(&root), Err(Missing::Marker)));
        fs::write(root.join(MARKER), "fetchloom mirror 2\n").unwrap();
        assert!(matches!(probe(&root), Err(Missing::Marker)));
        mirror(&root, &[]);
        assert!(probe(&root).is_ok());
    }

    #[test]
    fn each_wanted_file_says_what_the_mirror_gave() {
        let folder = tempfile::tempdir().unwrap();
        let cache = Cache::open(folder.path().join("cache")).unwrap();
        let root = folder.path().join("mirror");
        mirror(&root, &[b"kept"]);
        // the mirror's copy of "damaged" holds other bytes than the pin
        let damaged = place(&root, "objects", &crate::digest::sha256(b"damaged"));
        fs::create_dir_all(damaged.parent().unwrap()).unwrap();
        fs::write(damaged, b"DAMAGED").unwrap();
        let gave = copy_all(
            &cache,
            &root,
            &[wanted(b"kept"), wanted(b"absent"), wanted(b"damaged")],
            Threads::default(),
            &Stop::default(),
        )
        .unwrap();
        assert!(
            matches!(&gave[0], Gave::Stored(stored) if stored.digest == crate::digest::sha256(b"kept"))
        );
        assert!(matches!(gave[1], Gave::Absent));
        assert!(matches!(gave[2], Gave::Damaged));
        assert!(
            cache
                .find(&crate::digest::sha256(b"kept"), Some(4))
                .is_some()
        );
        assert!(
            cache
                .find(&crate::digest::sha256(b"damaged"), None)
                .is_none()
        );
    }

    fn item(bytes: &[u8]) -> Item {
        Item {
            folder: OBJECTS,
            digest: crate::digest::sha256(bytes),
            size: bytes.len() as u64,
        }
    }

    /// a copy is held only as a plain file whose bytes hash to its name, and only while a later
    /// look finds it as it was then. every byte wanted is told as settled, read or not.
    #[test]
    fn each_copy_is_hashed_whole_and_looked_at_again() {
        let folder = tempfile::tempdir().unwrap();
        let root = folder.path().join("mirror");
        mirror(&root, &[b"held"]);
        let copy = |bytes: &[u8]| place(&root, "objects", &crate::digest::sha256(bytes));
        for (bytes, there) in [(&b"same"[..], &b"SAME"[..]), (b"short", b"longer")] {
            fs::create_dir_all(copy(bytes).parent().unwrap()).unwrap();
            fs::write(copy(bytes), there).unwrap();
        }
        let wants = [
            wanted(b"held"),
            wanted(b"same"),
            wanted(b"short"),
            wanted(b"absent"),
        ];
        let told = std::sync::atomic::AtomicU64::new(0);
        let read = |bytes| {
            told.fetch_add(bytes, Ordering::Relaxed);
        };
        let held = check_all(&root, &wants, Threads::default(), &read, &Stop::default()).unwrap();
        let Held::Verified(seen) = held[0] else {
            panic!("{held:?}");
        };
        assert!(matches!(
            held[1..],
            [Held::Damaged, Held::Damaged, Held::Absent]
        ));
        assert_eq!(told.into_inner(), 4 + 4 + 5 + 6);
        let digest = crate::digest::sha256(b"held");
        assert!(still(&root, &digest, &seen));
        let file = fs::OpenOptions::new()
            .write(true)
            .open(copy(b"held"))
            .unwrap();
        file.set_modified(SystemTime::now() - Duration::from_hours(1))
            .unwrap();
        assert!(!still(&root, &digest, &seen), "written since");
        #[cfg(unix)]
        {
            let elsewhere = folder.path().join("held");
            fs::write(&elsewhere, b"held").unwrap();
            fs::remove_file(copy(b"held")).unwrap();
            std::os::unix::fs::symlink(&elsewhere, copy(b"held")).unwrap();
            let held = check_all(
                &root,
                &wants[..1],
                Threads::default(),
                &|_| {},
                &Stop::default(),
            );
            assert!(matches!(held.unwrap()[..], [Held::Damaged]));
        }
    }

    /// a mirror at `mirror` in a new folder, opened for writing, with the folders for `items`.
    fn target(folder: &Path, items: &[Item]) -> Target {
        sharded_from(folder, items, SHARD_FROM)
    }

    /// [`target`], writing below `tmp/` from `shard_from` files.
    fn sharded_from(folder: &Path, items: &[Item], shard_from: usize) -> Target {
        let (mut target, _) = Target::open(&folder.join("mirror"), Arc::default()).unwrap();
        target.shard_from = shard_from;
        target.prepare(items).unwrap();
        target
    }

    /// the files the mirror at `root` holds in `tmp/` and its folders, none once a push is done.
    fn unfinished(root: &Path) -> Vec<PathBuf> {
        let mut found = Vec::new();
        for entry in fs::read_dir(root.join(TMP)).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                found.extend(
                    fs::read_dir(&path)
                        .unwrap()
                        .map(|entry| entry.unwrap().path()),
                );
            } else {
                found.push(path);
            }
        }
        found
    }

    /// files without a marker may be anything, or an unmounted disk's, so their folder is refused.
    #[test]
    fn a_push_starts_a_mirror_only_where_nothing_is() {
        let folder = tempfile::tempdir().unwrap();
        let root = folder.path().join("mirror");
        assert_eq!(look(&root).unwrap(), State::New);
        assert!(matches!(
            look(&folder.path().join("unmounted/mirror")),
            Err(Refused::NoParent)
        ));
        let (_, started) = Target::open(&root, Arc::default()).unwrap();
        assert!(started);
        assert!(probe(&root).is_ok());
        assert_eq!(look(&root).unwrap(), State::Mirror);
        let (_, started) = Target::open(&root, Arc::default()).unwrap();
        assert!(!started);
        let full = folder.path().join("thesis");
        fs::create_dir(&full).unwrap();
        fs::write(full.join("draft.tex"), b"chapter one").unwrap();
        assert!(matches!(look(&full), Err(Refused::NotEmpty)));
        assert!(matches!(
            Target::open(&full, Arc::default()),
            Err(Refused::NotEmpty)
        ));
        assert!(!full.join(MARKER).exists());
        fs::write(folder.path().join("file"), b"").unwrap();
        assert!(matches!(
            look(&folder.path().join("file")),
            Err(Refused::NotAFolder)
        ));
    }

    /// another push makes the folder, or the whole mirror, between this one's looks; this one takes
    /// it as new or as that mirror, and a later push finds one mirror with its marker.
    #[test]
    fn a_folder_another_push_makes_between_two_looks_is_taken() {
        let folder_only: fn(&Path) = |root| fs::create_dir(root).unwrap();
        let whole_mirror: fn(&Path) = |root| drop(Target::open(root, Arc::default()).unwrap());
        for (other, starts) in [(folder_only, true), (whole_mirror, false)] {
            let folder = tempfile::tempdir().unwrap();
            let root = folder.path().join("mirror");
            BETWEEN_LOOKS.set(Some(other));
            let (_, started) = Target::open(&root, Arc::default()).unwrap();
            assert_eq!(started, starts);
            assert!(probe(&root).is_ok());
            let (_, started) = Target::open(&root, Arc::default()).unwrap();
            assert!(!started);
        }
    }

    /// a file takes its name only checked, as a read-only file, and nothing is left behind.
    #[test]
    fn a_pushed_file_takes_its_name_only_once_checked() {
        let folder = tempfile::tempdir().unwrap();
        let pushed = item(b"pinned bytes");
        let target = target(folder.path(), &[pushed]);
        let put = put_one(&target, &pushed, b"pinned bytes", There::Absent);
        assert_eq!(put.unwrap(), Put::Copied);
        let name = place(&target.root, OBJECTS, &pushed.digest);
        assert_eq!(fs::read(&name).unwrap(), b"pinned bytes");
        assert!(fs::metadata(&name).unwrap().permissions().readonly());
        assert_eq!(unfinished(&target.root), Vec::<PathBuf>::new());
        let other = item(b"pinned too");
        let put = put_one(&target, &other, b"other bytes", There::Absent);
        assert!(matches!(put, Err(Fault::OtherBytes)));
        assert!(!place(&target.root, OBJECTS, &other.digest).exists());
        assert_eq!(unfinished(&target.root), Vec::<PathBuf>::new());
    }

    /// a write the disk lost shows when the file is read back, before it takes a name.
    #[test]
    fn bytes_that_read_back_otherwise_never_take_a_name() {
        let folder = tempfile::tempdir().unwrap();
        let pushed = item(b"pinned bytes");
        let target = target(folder.path(), &[pushed]);
        LOSING_A_WRITE.set(true);
        let put = put_one(&target, &pushed, b"pinned bytes", There::Absent);
        LOSING_A_WRITE.set(false);
        assert!(matches!(put, Err(Fault::ReadBack)), "{put:?}");
        assert!(!place(&target.root, OBJECTS, &pushed.digest).exists());
        assert_eq!(unfinished(&target.root), Vec::<PathBuf>::new());
    }

    /// one look tells a wrong object from a good one.
    #[test]
    fn a_wrong_object_on_the_target_is_replaced_and_a_good_one_kept() {
        let folder = tempfile::tempdir().unwrap();
        let root = folder.path().join("mirror");
        mirror(&root, &[b"good"]);
        let (good, wrong) = (item(b"good"), item(b"wrong"));
        let wrong_name = place(&root, OBJECTS, &wrong.digest);
        fs::create_dir_all(wrong_name.parent().unwrap()).unwrap();
        fs::write(&wrong_name, b"wrong, and longer").unwrap();
        let good_name = place(&root, OBJECTS, &good.digest);
        let before = fs::metadata(&good_name).unwrap().modified().unwrap();
        let there = survey(&root, State::Mirror, &[good, wrong], Threads::default()).unwrap();
        assert_eq!(there, [There::Present, There::Damaged]);
        let (mut target, _) = Target::open(&root, Arc::default()).unwrap();
        target.prepare(&[wrong]).unwrap();
        let put = put_one(&target, &wrong, b"wrong", There::Damaged);
        assert_eq!(put.unwrap(), Put::Replaced);
        assert_eq!(fs::read(&wrong_name).unwrap(), b"wrong");
        assert_eq!(fs::read(&good_name).unwrap(), b"good");
        assert_eq!(
            fs::metadata(&good_name).unwrap().modified().unwrap(),
            before
        );
        let names: Vec<_> = fs::read_dir(wrong_name.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, [wrong_name.file_name().unwrap()]);
    }

    /// when two pushes of one file run at once, the second to finish keeps the first one's copy.
    #[test]
    fn a_name_another_push_gave_first_keeps_its_file() {
        let folder = tempfile::tempdir().unwrap();
        let pushed = item(b"pinned bytes");
        let target = target(folder.path(), &[pushed]);
        let name = place(&target.root, OBJECTS, &pushed.digest);
        fs::write(&name, b"pinned bytes").unwrap();
        let put = put_one(&target, &pushed, b"pinned bytes", There::Absent);
        assert_eq!(put.unwrap(), Put::Present);
        assert_eq!(fs::read(&name).unwrap(), b"pinned bytes");
        assert_eq!(unfinished(&target.root), Vec::<PathBuf>::new());
    }

    /// what pushing `item` alone from `bytes` into `target` gives, where [`survey`] found `there`.
    fn put_one(target: &Target, item: &Item, bytes: &[u8], there: There) -> Result<Put, Fault> {
        let source = |_: &Item| Ok(bytes);
        match target.put_all(
            &[(*item, there)],
            Threads::default(),
            &source,
            &|_| {},
            &|_| {},
        ) {
            Ok(puts) => Ok(puts[0]),
            Err(Failed::File(0, fault)) => Err(fault),
            Err(failed) => panic!("{failed:?}"),
        }
    }

    /// files named `items`, each holding the bytes `contents` gives, written by a push into a new
    /// mirror in `folder`.
    fn put_all(folder: &Path, contents: &[Vec<u8>]) -> (Target, Result<Vec<Put>, Failed>) {
        put_all_from(folder, contents, SHARD_FROM)
    }

    /// [`put_all`], writing below `tmp/` from `shard_from` files.
    fn put_all_from(
        folder: &Path,
        contents: &[Vec<u8>],
        shard_from: usize,
    ) -> (Target, Result<Vec<Put>, Failed>) {
        let items: Vec<(Item, There)> = contents
            .iter()
            .map(|bytes| (item(bytes), There::Absent))
            .collect();
        let bytes: HashMap<Sha256, &[u8]> = contents
            .iter()
            .map(|bytes| (crate::digest::sha256(bytes), bytes.as_slice()))
            .collect();
        let only: Vec<Item> = items.iter().map(|(item, _)| *item).collect();
        let target = sharded_from(folder, &only, shard_from);
        let source = |item: &Item| Ok(io::Cursor::new(bytes[&item.digest]));
        let put = target.put_all(&items, Threads::default(), &source, &|_| {}, &|_| {});
        (target, put)
    }

    /// more files than the batch rule flushes one by one, each different.
    fn many() -> Vec<Vec<u8>> {
        (0..=crate::cache::MOST_FLUSHED_ALONE)
            .map(|n| format!("file {n}").into_bytes())
            .collect()
    }

    /// past the count the cache flushes one by one, the files reach the disk together first.
    #[test]
    fn a_batch_of_files_is_named_once_it_is_on_the_disk() {
        let folder = tempfile::tempdir().unwrap();
        let contents = many();
        let (target, put) = put_all(folder.path(), &contents);
        assert_eq!(put.unwrap(), vec![Put::Copied; contents.len()]);
        for bytes in &contents {
            let name = place(&target.root, OBJECTS, &crate::digest::sha256(bytes));
            assert_eq!(&fs::read(name).unwrap(), bytes);
        }
        assert_eq!(unfinished(&target.root), Vec::<PathBuf>::new());
    }

    /// a name must never point at bytes that may not be on the disk.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_failed_batch_flush_names_nothing() {
        let folder = tempfile::tempdir().unwrap();
        let contents = many();
        FAILING.set(Some(Failing::BatchFlush));
        let (target, put) = put_all(folder.path(), &contents);
        FAILING.set(None);
        assert!(matches!(put, Err(Failed::Flush(_))), "{put:?}");
        for bytes in &contents {
            assert!(!place(&target.root, OBJECTS, &crate::digest::sha256(bytes)).exists());
        }
        assert_eq!(unfinished(&target.root), Vec::<PathBuf>::new());
    }

    /// each file stays under its temporary name, which no reader asks for and a later push sweeps.
    #[test]
    fn a_push_killed_before_naming_leaves_no_named_file() {
        let folder = tempfile::tempdir().unwrap();
        let contents = many();
        BETWEEN.set(Between::Kill);
        let (target, put) = put_all(folder.path(), &contents);
        BETWEEN.set(Between::Nothing);
        assert!(put.is_err());
        for bytes in &contents {
            assert!(!place(&target.root, OBJECTS, &crate::digest::sha256(bytes)).exists());
        }
        let left = unfinished(&target.root);
        assert_eq!(left.len(), contents.len());
        assert!(left.iter().all(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(is_unfinished)
        }));
    }

    /// a push of many files writes each in the folder below `tmp/` its hash's first digit names,
    /// so its writers do not all wait on one folder, and names them all from there.
    #[test]
    fn a_large_push_writes_below_tmp_by_hash() {
        let folder = tempfile::tempdir().unwrap();
        let contents = many();
        BETWEEN.set(Between::Kill);
        let (target, put) = put_all_from(folder.path(), &contents, 2);
        BETWEEN.set(Between::Nothing);
        assert!(put.is_err());
        let left = unfinished(&target.root);
        assert_eq!(left.len(), contents.len());
        for path in &left {
            let digest = crate::digest::sha256(&fs::read(path).unwrap());
            let shard = path.parent().unwrap();
            assert_eq!(shard.parent().unwrap(), target.root.join(TMP));
            assert_eq!(
                shard.file_name().unwrap(),
                &digest.hex()[..1],
                "{}",
                path.display()
            );
        }
        target.sweep(SystemTime::now() + STALE + Duration::from_secs(60));
        assert_eq!(unfinished(&target.root), Vec::<PathBuf>::new());
        let folder = tempfile::tempdir().unwrap();
        let (target, put) = put_all_from(folder.path(), &contents, 2);
        assert_eq!(put.unwrap(), vec![Put::Copied; contents.len()]);
        for bytes in &contents {
            let name = place(&target.root, OBJECTS, &crate::digest::sha256(bytes));
            assert_eq!(&fs::read(name).unwrap(), bytes);
        }
        assert_eq!(unfinished(&target.root), Vec::<PathBuf>::new());
    }

    /// what pushing one file gives when `between` happens between writing it and reading it
    /// back, and, when it took its name with its bytes, whether it is read-only there.
    fn one_put(between: Between) -> (Result<Vec<Put>, Failed>, Option<bool>) {
        let folder = tempfile::tempdir().unwrap();
        let contents = vec![b"pinned bytes".to_vec()];
        BETWEEN.set(between);
        let (target, put) = put_all(folder.path(), &contents);
        BETWEEN.set(Between::Nothing);
        let name = place(&target.root, OBJECTS, &crate::digest::sha256(&contents[0]));
        let named = fs::read(&name)
            .is_ok_and(|bytes| bytes == contents[0])
            .then(|| fs::metadata(&name).unwrap().permissions().readonly());
        (put, named)
    }

    /// what `run` logs at debug level and above, as a person running with `-vv` reads it.
    #[cfg(target_os = "linux")]
    fn logged<T>(run: impl FnOnce() -> T) -> (T, String) {
        #[derive(Clone, Default)]
        struct Lines(Arc<Mutex<Vec<u8>>>);
        impl Write for Lines {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let lines = Lines::default();
        let writer = lines.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || writer.clone())
            .with_ansi(false)
            .with_max_level(tracing::Level::DEBUG)
            .finish();
        let value = tracing::subscriber::with_default(subscriber, run);
        let text = String::from_utf8(lines.0.lock().unwrap().clone()).unwrap();
        (value, text)
    }

    /// a renumbered file of the same account and size is made read-only, flushed on its own handle
    /// and read back. on windows, which never renumbers, a new file in its place is someone else's.
    #[test]
    fn a_file_renumbered_before_its_read_back_still_takes_its_name() {
        let (put, named) = one_put(Between::Renumber);
        #[cfg(unix)]
        {
            assert_eq!(put.unwrap(), vec![Put::Copied]);
            assert_eq!(named, Some(true), "named, and read-only");
        }
        #[cfg(not(unix))]
        {
            assert!(
                matches!(put, Err(Failed::File(0, Fault::Replaced))),
                "{put:?}"
            );
            assert_eq!(named, None);
        }
    }

    /// the file's bytes are checked anyway, and the disk's refusal shows with `-vv`.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_renumbered_file_the_disk_will_not_make_read_only_is_named_anyway() {
        FAILING.set(Some(Failing::ReadOnly));
        let ((put, named), logs) = logged(|| one_put(Between::Renumber));
        FAILING.set(None);
        assert_eq!(put.unwrap(), vec![Put::Copied]);
        assert_eq!(named, Some(false), "named, keeping its mode");
        assert!(
            logs.contains("could not make a pushed file read-only"),
            "{logs}"
        );
    }

    /// unix reads the swapped file back and finds the bytes changed, while windows refuses any file
    /// with a new number as replaced.
    #[test]
    fn a_file_swapped_before_its_read_back_never_takes_a_name() {
        let (put, named) = one_put(Between::Alter);
        #[cfg(unix)]
        assert!(
            matches!(put, Err(Failed::File(0, Fault::ReadBack))),
            "{put:?}"
        );
        #[cfg(not(unix))]
        assert!(
            matches!(put, Err(Failed::File(0, Fault::Replaced))),
            "{put:?}"
        );
        assert_eq!(named, None);
    }

    /// another account's file is refused even with the same bytes, since that account could change
    /// them once named.
    #[cfg(unix)]
    #[test]
    fn a_file_another_account_put_in_its_place_never_takes_a_name() {
        let (put, named) = one_put(Between::Foreign);
        assert!(
            matches!(put, Err(Failed::File(0, Fault::Replaced))),
            "{put:?}"
        );
        assert_eq!(named, None);
    }

    /// on linux the test makes the flush fail, so a refusal shows it was never tried.
    #[test]
    fn a_file_of_another_size_in_its_place_is_refused_unflushed() {
        #[cfg(target_os = "linux")]
        FAILING.set(Some(Failing::RenumberedFlush));
        let (put, named) = one_put(Between::Grow);
        #[cfg(target_os = "linux")]
        FAILING.set(None);
        assert!(
            matches!(put, Err(Failed::File(0, Fault::Replaced))),
            "{put:?}"
        );
        assert_eq!(named, None);
    }

    /// the failed flush stops the push with the disk's error.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_renumbered_file_that_cannot_be_flushed_never_takes_a_name() {
        FAILING.set(Some(Failing::RenumberedFlush));
        let (put, named) = one_put(Between::Renumber);
        FAILING.set(None);
        let badf = Some(rustix::io::Errno::BADF.raw_os_error());
        assert!(
            matches!(&put, Err(Failed::File(0, Fault::Mirror(err))) if err.raw_os_error() == badf),
            "{put:?}"
        );
        assert_eq!(named, None);
    }

    /// a new mirror's marker, its name included, is on the disk before any file is pushed.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_marker_that_cannot_be_flushed_stops_the_push_before_any_file() {
        let folder = tempfile::tempdir().unwrap();
        let root = folder.path().join("mirror");
        FAILING.set(Some(Failing::MarkerFlush));
        let opened = Target::open(&root, Arc::default());
        FAILING.set(None);
        let Err(Refused::Failed(err)) = opened else {
            panic!("the push went on with a marker that is not on the disk");
        };
        assert_eq!(
            err.raw_os_error(),
            Some(rustix::io::Errno::BADF.raw_os_error())
        );
        assert!(!root.join(OBJECTS).exists());
        assert!(!root.join(crate::mirror::TREES).exists());
        assert_eq!(unfinished(&root), Vec::<PathBuf>::new());
    }

    /// a link someone planted in the mirror is refused, so no write follows it out of the mirror.
    #[cfg(unix)]
    #[test]
    fn a_link_planted_in_objects_is_refused() {
        let folder = tempfile::tempdir().unwrap();
        let root = folder.path().join("mirror");
        mirror(&root, &[]);
        let outside = folder.path().join("home");
        fs::create_dir(&outside).unwrap();
        let pushed = item(b"pinned bytes");
        let fan = place(&root, OBJECTS, &pushed.digest);
        let fan = fan.parent().unwrap();
        fs::create_dir_all(fan.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&outside, fan).unwrap();
        let looked = survey(&root, State::Mirror, &[pushed], Threads::default()).unwrap_err();
        assert!(looked.to_string().contains("is a link"), "{looked}");
        let (mut target, _) = Target::open(&root, Arc::default()).unwrap();
        let made = target.prepare(&[pushed]).unwrap_err();
        assert!(made.to_string().contains("is a link"), "{made}");
        assert!(put_one(&target, &pushed, b"pinned bytes", There::Absent).is_err());
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
    }

    /// a link planted in place of a folder below `tmp/` is refused before a large push writes.
    #[cfg(unix)]
    #[test]
    fn a_link_planted_below_tmp_is_refused() {
        let folder = tempfile::tempdir().unwrap();
        let root = folder.path().join("mirror");
        mirror(&root, &[]);
        let outside = folder.path().join("home");
        fs::create_dir(&outside).unwrap();
        fs::create_dir(root.join(TMP)).unwrap();
        std::os::unix::fs::symlink(&outside, root.join(TMP).join("7")).unwrap();
        let (mut target, _) = Target::open(&root, Arc::default()).unwrap();
        target.shard_from = 1;
        let made = target.prepare(&[item(b"pinned bytes")]).unwrap_err();
        assert!(made.to_string().contains("is a link"), "{made}");
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
    }

    /// a stop leaves no file named, and nothing half written behind.
    #[test]
    fn an_interrupted_push_names_nothing() {
        let folder = tempfile::tempdir().unwrap();
        let pushed = item(b"pinned bytes");
        let mut target = target(folder.path(), &[pushed]);
        let stop = Arc::new(Stop::default());
        stop.ask();
        target.stop = stop;
        let put = put_one(&target, &pushed, b"pinned bytes", There::Absent);
        assert!(
            matches!(&put, Err(Fault::Mirror(err)) if err.kind() == io::ErrorKind::Interrupted),
            "{put:?}"
        );
        assert!(!place(&target.root, OBJECTS, &pushed.digest).exists());
        assert_eq!(unfinished(&target.root), Vec::<PathBuf>::new());
    }

    /// a younger unfinished file may be a running push's, and other files are no push's.
    #[test]
    fn old_unfinished_files_are_swept_and_others_kept() {
        let folder = tempfile::tempdir().unwrap();
        let target = target(folder.path(), &[]);
        let tmp = target.root.join(TMP);
        let (old, young, other) = (
            tmp.join(".0123456789abcdef.part"),
            tmp.join(".fedcba9876543210.part"),
            tmp.join("notes.part"),
        );
        let now = SystemTime::now();
        for (path, age) in [(&old, 2), (&young, 0), (&other, 2)] {
            fs::write(path, b"unfinished").unwrap();
            File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_modified(now - Duration::from_secs(age * 24 * 60 * 60))
                .unwrap();
        }
        target.sweep(now);
        assert!(!old.exists());
        assert!(young.exists());
        assert!(other.exists());
    }

    /// the folders below `tmp/` are swept as `tmp/` is, and a link in place of one is not followed.
    #[test]
    fn old_unfinished_files_below_tmp_are_swept() {
        let folder = tempfile::tempdir().unwrap();
        let target = target(folder.path(), &[]);
        let shard = target.root.join(TMP).join("c");
        fs::create_dir(&shard).unwrap();
        let outside = folder.path().join("home");
        fs::create_dir(&outside).unwrap();
        let (old, young, away) = (
            shard.join(".0123456789abcdef.part"),
            shard.join(".fedcba9876543210.part"),
            outside.join(".0123456789abcdef.part"),
        );
        let now = SystemTime::now();
        for (path, age) in [(&old, 2), (&young, 0), (&away, 2)] {
            fs::write(path, b"unfinished").unwrap();
            File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_modified(now - Duration::from_secs(age * 24 * 60 * 60))
                .unwrap();
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, target.root.join(TMP).join("d")).unwrap();
        target.sweep(now);
        assert!(!old.exists());
        assert!(young.exists());
        assert!(away.exists());
    }

    #[test]
    fn a_tree_is_read_whole_up_to_its_bound() {
        let folder = tempfile::tempdir().unwrap();
        let root = folder.path().join("mirror");
        mirror(&root, &[]);
        let tree = crate::digest::sha256(b"tree");
        let read = || read_tree(&root, &tree, &Stop::default()).unwrap();
        assert!(matches!(read(), Tree::Absent));
        let path = place(&root, "trees", &tree);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"a.csv\t1\tff\n").unwrap();
        assert!(matches!(read(), Tree::Bytes(bytes) if bytes == b"a.csv\t1\tff\n"));
        fs::File::create(&path)
            .unwrap()
            .set_len(LARGEST_TREE + 1)
            .unwrap();
        assert!(matches!(read(), Tree::Absent));
    }
}
