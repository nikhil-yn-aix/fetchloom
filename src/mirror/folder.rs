//! a mirror on a disk this machine reads, such as a lab nas, a usb drive or a network share. its
//! files are copied into the cache, cloned where the disk can, and never linked, so a later
//! change on the mirror cannot reach a project. `fetchloom push` writes one through [`Target`]:
//! each file is written under a temporary name, checked, and only then given its own.

use std::collections::{BTreeSet, HashMap};
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use super::{LARGEST_TREE, MARKER, MARKER_LINE, Wanted, below};
use crate::cache::{Cache, Copied, Stored, read_unfollowed};
use crate::digest::{Hasher, Sha256};
use crate::place::Placer;
use crate::stop::Stop;
use crate::threads::Threads;
use crate::volume::is_plain;

/// copies from or to one folder mirror running at once, so a nas is not flooded.
pub(crate) const COPIES: usize = 8;

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

/// a tree a folder mirror gave.
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

/// copies each of `wanted` the folder mirror at `root` holds into the cache, a few at once within
/// `threads`, and says what each gave, in order. past `MOST_FLUSHED_ALONE` files they reach the
/// disk with one flush, where the system can, before any takes its name.
///
/// # Errors
///
/// a failure to write the cache, and `Interrupted` when `stop` asks.
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

/// the tree whose bytes hash to `tree`, as the folder mirror at `root` holds it, read a block at a
/// time and never past [`LARGEST_TREE`].
///
/// # Errors
///
/// a failure to read it, and `Interrupted` when `stop` asks.
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
        if stop.is_asked() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "stopped at your request",
            ));
        }
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

/// the folder a push writes files in before they take their names.
const TMP: &str = "tmp";

/// how long a file a push left unfinished in `tmp/` stays before a later push removes it, as a
/// push that was killed leaves one.
const STALE: Duration = Duration::from_hours(24);

/// how much a push reads before it looks for a stop again.
const BLOCK: usize = 1 << 20;

/// a file a push writes into a mirror: a pinned file under [`OBJECTS`], or under [`TREES`] the
/// list of what an archive unpacks to, each named by the SHA-256 its bytes must have.
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
    /// writing the mirror failed, or a stop was asked, which is `Interrupted`.
    Mirror(io::Error),
}

/// what is at `root`, a folder named as a mirror to push to, read without changing anything. a
/// folder holding only the `tmp/` a push that stopped before its marker made is still new.
///
/// # Errors
///
/// why a push cannot write there.
pub(crate) fn look(root: &Path) -> Result<State, Refused> {
    match probe(root) {
        Ok(()) => return Ok(State::Mirror),
        Err(Missing::Unreadable(err)) => return Err(Refused::Failed(err)),
        Err(Missing::Folder) => {
            return match fs::metadata(root) {
                Ok(_) => Err(Refused::NotAFolder),
                Err(err) if err.kind() == io::ErrorKind::NotFound => {
                    let parent = std::path::absolute(root)
                        .ok()
                        .and_then(|root| root.parent().map(Path::to_path_buf));
                    if parent.is_some_and(|parent| parent.is_dir()) {
                        Ok(State::New)
                    } else {
                        Err(Refused::NoParent)
                    }
                }
                Err(err) => Err(Refused::Failed(err)),
            };
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
/// a few at once within `threads`, and says what is there, in order. the folders the items go in
/// are checked once each through the mirror's top, so a link among them is refused, never
/// followed; a mirror not started yet holds nothing.
///
/// # Errors
///
/// a folder that is a link or no folder, and a name that cannot be looked at.
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

/// a folder mirror a push writes to. every folder in it is reached through a handle on the
/// mirror's top without following a link, as placing reaches a project's folders, so a link
/// someone planted in the mirror cannot send a write elsewhere.
pub(crate) struct Target {
    root: PathBuf,
    /// never stopped itself, so a file left unfinished by a stop is still removed.
    placer: Placer,
    stop: std::sync::Arc<Stop>,
}

impl Target {
    /// opens the folder mirror at `root` for writing, and starts one there, the folder and then
    /// its marker, when [`look`] finds none yet. says whether it started one.
    ///
    /// # Errors
    ///
    /// why a push cannot write there.
    pub(crate) fn open(root: &Path, stop: std::sync::Arc<Stop>) -> Result<(Target, bool), Refused> {
        let state = look(root)?;
        let mut target = Target {
            root: root.to_path_buf(),
            placer: Placer::in_named_folder(root),
            stop,
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

    /// the mirror's top folder.
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// writes the marker, unless another push wrote it first. says whether this one did.
    fn start(&mut self) -> io::Result<bool> {
        self.placer
            .check_folders(&self.root.join(TMP).join(MARKER), true)?;
        let line = format!("{MARKER_LINE}\n");
        let mut temp = self.temporary()?;
        temp.file()?.write_all(line.as_bytes())?;
        flush(temp.file()?)?;
        temp.close();
        match self.placer.move_back(&temp.path, &self.root.join(MARKER)) {
            Ok(()) => {
                temp.named = true;
                Ok(true)
            }
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists && probe(&self.root).is_ok() => {
                Ok(false)
            }
            Err(err) => Err(err),
        }
    }

    /// makes `tmp/` and the folders `items` go in, each checked once through the mirror's top: a
    /// link among them is refused, never followed.
    ///
    /// # Errors
    ///
    /// a folder that is a link or no folder, or one that cannot be made.
    pub(crate) fn prepare(&mut self, items: &[Item]) -> io::Result<()> {
        let tmp = self.root.join(TMP).join(MARKER);
        let names = std::iter::once(tmp).chain(
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

    /// writes `item` from `source`, which must hold its bytes, where [`survey`] found `there`. the
    /// bytes go into a new read-only file in `tmp/`, hashed as they are written; once they are on
    /// the disk they are read back from it and hashed again, and only then does the file take
    /// its name. it never takes it from another file: a damaged copy is moved aside first and
    /// removed once the new file has the name, and a name another push gave a file of the
    /// item's size meanwhile keeps that file. [`Target::prepare`] makes the folders first.
    ///
    /// # Errors
    ///
    /// a [`Fault`]; whatever went wrong, the file written goes and no name changes.
    pub(crate) fn put(
        &self,
        item: &Item,
        source: &mut impl Read,
        there: There,
    ) -> Result<Put, Fault> {
        let most = item.size.saturating_add(1);
        let mut temp = self.temporary().map_err(Fault::Mirror)?;
        let file = temp.file().map_err(Fault::Mirror)?;
        let written = copy_hashed(source, file, most, &self.stop)?;
        if written != (item.size, item.digest) {
            return Err(Fault::OtherBytes);
        }
        flush(file).map_err(Fault::Mirror)?;
        #[cfg(test)]
        tests::before_read_back(file);
        if read_back(file, most, &self.stop).map_err(Fault::Mirror)? != (item.size, item.digest) {
            return Err(Fault::ReadBack);
        }
        temp.close();
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

    /// removes the files pushes left unfinished in the mirror's `tmp/` more than a day before
    /// `now`, as a push that was killed leaves them. nothing else there is touched.
    pub(crate) fn sweep(&self, now: SystemTime) {
        let Ok(entries) = fs::read_dir(self.root.join(TMP)) else {
            return;
        };
        for entry in entries.flatten() {
            let unfinished = entry.file_name().to_str().is_some_and(is_unfinished);
            let old = entry
                .metadata()
                .and_then(|found| found.modified())
                .is_ok_and(|modified| now.duration_since(modified).is_ok_and(|age| age > STALE));
            if unfinished && old {
                let _ = self.placer.delete_file(&entry.path());
            }
        }
    }

    /// puts the names a push gave on the disk: the folders `written` went in, then the mirror's
    /// own. a name a power cut lost before this would only make a later push copy its file
    /// again.
    ///
    /// # Errors
    ///
    /// a folder that could not be flushed.
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

    /// a new read-only file in `tmp/`, named `.<16 hex digits>.part`.
    fn temporary(&self) -> io::Result<Temporary<'_>> {
        loop {
            let path = self
                .root
                .join(TMP)
                .join(format!(".{:016x}.part", fastrand::u64(..)));
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

/// copies at most `most` bytes of `source` into `file`, hashing them, and says how many there
/// were and their SHA-256. a stop is looked for between blocks.
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

/// what `file` holds on its disk, at most `most` bytes, read again from its start and hashed. on
/// linux its pages are dropped from memory first, and macos is asked to keep none of what is
/// read, so the bytes come from the disk where the system allows; a network disk may still
/// answer from memory.
fn read_back(file: &mut File, most: u64, stop: &Stop) -> io::Result<(u64, Sha256)> {
    #[cfg(target_os = "linux")]
    if let Err(err) = rustix::fs::fadvise(&*file, 0, None, rustix::fs::Advice::DontNeed) {
        tracing::debug!(%err, "could not drop a pushed file from memory");
    }
    #[cfg(target_os = "macos")]
    if let Err(err) = rustix::fs::fcntl_nocache(&*file, true) {
        tracing::debug!(%err, "could not read a pushed file past memory");
    }
    file.seek(SeekFrom::Start(0))?;
    hashed(file, most, stop, |_| true)
}

/// reads at most `most` bytes of `source` a block at a time, handing each block to `each` until
/// it says no, and hashes them. says how many were read and their SHA-256. a stop asked between
/// blocks is `Interrupted`.
fn hashed(
    source: &mut impl Read,
    most: u64,
    stop: &Stop,
    mut each: impl FnMut(&[u8]) -> bool,
) -> io::Result<(u64, Sha256)> {
    let mut block = vec![0; usize::try_from(most).map_or(BLOCK, |most| most.clamp(1, BLOCK))];
    let mut hasher = Hasher::default();
    let mut read = 0_u64;
    while read < most {
        if stop.is_asked() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "stopped at your request",
            ));
        }
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

    /// a mirror at `mirror` in a new folder, opened for writing, with the folders for `items`.
    fn target(folder: &Path, items: &[Item]) -> Target {
        let (mut target, _) = Target::open(&folder.join("mirror"), Arc::default()).unwrap();
        target.prepare(items).unwrap();
        target
    }

    /// what the mirror at `root` holds in `tmp/`, which is nothing once a push is done.
    fn unfinished(root: &Path) -> Vec<PathBuf> {
        fs::read_dir(root.join(TMP))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect()
    }

    /// a push starts a mirror in a folder that is not there yet or is empty, and refuses one
    /// that holds files without a marker, since it may be anything, or an unmounted disk's.
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

    /// a file takes its name only checked, as a read-only file, and nothing is left behind.
    #[test]
    fn a_pushed_file_takes_its_name_only_once_checked() {
        let folder = tempfile::tempdir().unwrap();
        let pushed = item(b"pinned bytes");
        let target = target(folder.path(), &[pushed]);
        let put = target.put(&pushed, &mut &b"pinned bytes"[..], There::Absent);
        assert_eq!(put.unwrap(), Put::Copied);
        let name = place(&target.root, OBJECTS, &pushed.digest);
        assert_eq!(fs::read(&name).unwrap(), b"pinned bytes");
        assert!(fs::metadata(&name).unwrap().permissions().readonly());
        assert_eq!(unfinished(&target.root), Vec::<PathBuf>::new());
        let other = item(b"pinned too");
        let put = target.put(&other, &mut &b"other bytes"[..], There::Absent);
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
        let put = target.put(&pushed, &mut &b"pinned bytes"[..], There::Absent);
        LOSING_A_WRITE.set(false);
        assert!(matches!(put, Err(Fault::ReadBack)), "{put:?}");
        assert!(!place(&target.root, OBJECTS, &pushed.digest).exists());
        assert_eq!(unfinished(&target.root), Vec::<PathBuf>::new());
    }

    /// one look tells a good copy from a damaged one; the damaged one is replaced, the good one
    /// left as it is.
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
        let put = target.put(&wrong, &mut &b"wrong"[..], There::Damaged);
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

    /// two pushes of one file at once: the second to finish keeps the first one's copy.
    #[test]
    fn a_name_another_push_gave_first_keeps_its_file() {
        let folder = tempfile::tempdir().unwrap();
        let pushed = item(b"pinned bytes");
        let target = target(folder.path(), &[pushed]);
        let name = place(&target.root, OBJECTS, &pushed.digest);
        fs::write(&name, b"pinned bytes").unwrap();
        let put = target.put(&pushed, &mut &b"pinned bytes"[..], There::Absent);
        assert_eq!(put.unwrap(), Put::Present);
        assert_eq!(fs::read(&name).unwrap(), b"pinned bytes");
        assert_eq!(unfinished(&target.root), Vec::<PathBuf>::new());
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
        assert!(
            target
                .put(&pushed, &mut &b"pinned bytes"[..], There::Absent)
                .is_err()
        );
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
        let put = target.put(&pushed, &mut &b"pinned bytes"[..], There::Absent);
        assert!(
            matches!(&put, Err(Fault::Mirror(err)) if err.kind() == io::ErrorKind::Interrupted),
            "{put:?}"
        );
        assert!(!place(&target.root, OBJECTS, &pushed.digest).exists());
        assert_eq!(unfinished(&target.root), Vec::<PathBuf>::new());
    }

    /// a file a killed push left in `tmp/` goes once it is a day old; a younger one may be a
    /// running push's, and other files are not a push's.
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
