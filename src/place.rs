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

use crate::volume::{Volume, crosses_volumes, refuses_links};

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
}

/// one file to place: the object, where it goes, and whether it must be a writable copy.
pub(crate) struct Order<'a> {
    pub(crate) object: &'a Path,
    pub(crate) destination: &'a Path,
    pub(crate) writable: bool,
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
    /// the drive refused reflinks and hardlinks alike, as FAT and exFAT drives do.
    NoLinks,
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
            root,
            checks_root: true,
            folders: HashSet::new(),
            synced,
            can_reflink: AtomicBool::new(first == Method::Reflink),
            can_hardlink: AtomicBool::new(first != Method::Copy),
            other_volume: AtomicBool::new(false),
        }
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

    /// places every order and says how each went, in order, with the metadata of each placed file
    /// read right after it was placed, so a later edit is never taken for the placed file. the
    /// first goes alone, so it learns which methods the filesystem allows before the rest go
    /// several at once.
    pub(crate) fn place_all(&mut self, orders: &[Order]) -> Vec<io::Result<(Method, Metadata)>> {
        let mut results = Vec::with_capacity(orders.len());
        let mut ready = Vec::with_capacity(orders.len());
        for (index, order) in orders.iter().enumerate() {
            match self.check_folders(order.destination, true) {
                Ok(()) => ready.push((index, order)),
                Err(err) => results.push((index, Err(err))),
            }
        }
        let ready = spread(ready);
        let this = &*self;
        let put = |(index, order): &(usize, &Order)| {
            let placed = this
                .put(order.object, order.destination, order.writable)
                .and_then(|method| Ok((method, fs::symlink_metadata(order.destination)?)));
            (*index, placed)
        };
        if let Some((first, rest)) = ready.split_first() {
            results.push(put(first));
            results.extend(run_all(rest, put));
        }
        results.sort_unstable_by_key(|(index, _)| *index);
        results.into_iter().map(|(_, result)| result).collect()
    }

    /// tries each method the filesystem may allow, best first.
    fn put(&self, object: &Path, destination: &Path, writable: bool) -> io::Result<Method> {
        if self.can_reflink.load(Ordering::Relaxed) {
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
            match fs::hard_link(object, destination) {
                Ok(()) => return Ok(Method::Hardlink),
                Err(error) => self.learn(Method::Hardlink, error)?,
            }
        }
        copy(object, destination, writable)?;
        Ok(Method::Copy)
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
                real_folder(&self.root, false)?;
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
                real_folder(&folder, make)?;
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
            real_folder(&folder, false)?;
        }
        for part in parent
            .strip_prefix(&self.root)
            .unwrap_or(&parent)
            .components()
        {
            folder.push(part);
            real_folder(&folder, false)?;
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

/// runs `job` on every item, on a few threads when there are many, and returns the results in
/// the order of `items`.
pub(crate) fn run_all<T: Sync, R: Send>(items: &[T], job: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let threads = std::thread::available_parallelism().map_or(1, NonZero::get);
    let threads = threads.min(THREADS).min(items.len() / PER_THREAD);
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
fn spread<'o, 'p>(orders: Vec<(usize, &'o Order<'p>)>) -> Vec<(usize, &'o Order<'p>)> {
    let mut seen: HashMap<&Path, usize> = HashMap::new();
    let mut ranked: Vec<(usize, (usize, &Order))> = orders
        .into_iter()
        .map(|item| {
            let destination = item.1.destination;
            let rank = seen
                .entry(destination.parent().unwrap_or(destination))
                .or_default();
            *rank += 1;
            (*rank, item)
        })
        .collect();
    ranked.sort_by_key(|(rank, _)| *rank);
    ranked.into_iter().map(|(_, item)| item).collect()
}

/// checks that `folder` is a real folder, not a link to one, and makes it when `make` asks and it
/// is missing.
fn real_folder(folder: &Path, make: bool) -> io::Result<()> {
    match fs::symlink_metadata(folder) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "{} is a link, and fetchloom only places files in real folders",
                folder.display()
            ),
        )),
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            format!("{} is a file where a folder should be", folder.display()),
        )),
        Err(err) if err.kind() == io::ErrorKind::NotFound && make => {
            match fs::create_dir(folder) {
                // another process made it first
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                    real_folder(folder, false)
                }
                created => created,
            }
        }
        Err(err) => Err(err),
    }
}

/// copies `object` to `destination`, failing when `destination` exists, and makes the copy
/// writable or read-only as asked.
fn copy(object: &Path, destination: &Path, writable: bool) -> io::Result<()> {
    let claimed = File::create_new(destination)?;
    undo_on_error(fill(object, &claimed, writable), destination)
}

/// copies the bytes of `object` into `output` and seals it as asked, all through the handle, so
/// a link swapped in for the name `output` claimed is never written through.
fn fill(object: &Path, output: &File, writable: bool) -> io::Result<()> {
    transfer(File::open(object)?, output)?;
    output.set_permissions(writable_as(output.metadata()?.permissions(), writable))
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

/// makes the file at `path` writable or read-only, as `writable_as` describes.
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

/// `permissions` changed to make the file writable, or read-only.
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
mod tests {
    use std::path::PathBuf;

    use tempfile::TempDir;

    use super::*;

    const CONTENTS: &str = "station,temperature\nlerwick,9.1\n";

    /// a temporary folder holding one cached object, read-only like every object in the cache
    /// unless `read_only` is false.
    fn cache(read_only: bool) -> (TempDir, PathBuf) {
        let folder = TempDir::new().unwrap();
        let object = folder.path().join("object");
        fs::write(&object, CONTENTS).unwrap();
        if read_only {
            let mut permissions = fs::metadata(&object).unwrap().permissions();
            permissions.set_readonly(true);
            fs::set_permissions(&object, permissions).unwrap();
        }
        (folder, object)
    }

    #[test]
    fn a_hardlink_is_chosen_on_one_volume() {
        let (folder, object) = cache(true);
        let destination = folder.path().join("placed.csv");
        let mut placer = Placer::starting_at(folder.path().to_path_buf(), Method::Hardlink, None);
        let expected = if makes_hardlinks(folder.path()) {
            Method::Hardlink
        } else {
            Method::Copy
        };

        let method = placer.place(&object, &destination, false).unwrap();

        assert_eq!(method, expected);
        assert_eq!(fs::read_to_string(&destination).unwrap(), CONTENTS);
    }

    #[test]
    fn starting_below_hardlinks_falls_back_to_a_copy() {
        let (folder, object) = cache(true);
        let destination = folder.path().join("placed.csv");
        let mut placer = Placer::starting_at(folder.path().to_path_buf(), Method::Copy, None);

        let method = placer.place(&object, &destination, false).unwrap();

        assert_eq!(method, Method::Copy);
        assert_eq!(fs::read_to_string(&destination).unwrap(), CONTENTS);
    }

    #[test]
    fn missing_parent_folders_are_created() {
        let (folder, object) = cache(true);
        let destination = folder
            .path()
            .join("data")
            .join("weather")
            .join("placed.csv");
        let mut placer = Placer::starting_at(folder.path().to_path_buf(), Method::Hardlink, None);

        placer.place(&object, &destination, false).unwrap();

        assert_eq!(fs::read_to_string(&destination).unwrap(), CONTENTS);
    }

    #[test]
    fn an_existing_destination_is_an_error_whatever_the_method() {
        for first in [Method::Reflink, Method::Hardlink, Method::Copy] {
            let (folder, object) = cache(true);
            let destination = folder.path().join("placed.csv");
            fs::write(&destination, "edited by hand").unwrap();
            let mut placer = Placer::starting_at(folder.path().to_path_buf(), first, None);

            let error = placer.place(&object, &destination, false).unwrap_err();

            assert_eq!(error.kind(), io::ErrorKind::AlreadyExists, "{first:?}");
            assert_eq!(
                fs::read_to_string(&destination).unwrap(),
                "edited by hand",
                "{first:?}"
            );
        }
    }

    #[test]
    fn a_hardlink_keeps_the_objects_read_only_flag() {
        let (folder, object) = cache(true);
        // FAT32 and exFAT make no hardlinks
        if !makes_hardlinks(folder.path()) {
            return;
        }
        let destination = folder.path().join("placed.csv");
        let mut placer = Placer::starting_at(folder.path().to_path_buf(), Method::Hardlink, None);

        let method = placer.place(&object, &destination, false).unwrap();

        assert_eq!(method, Method::Hardlink);
        assert!(is_read_only(&destination));
        assert!(is_read_only(&object));
    }

    #[test]
    fn a_copy_is_made_read_only() {
        let (folder, object) = cache(false);
        let destination = folder.path().join("placed.csv");
        let mut placer = Placer::starting_at(folder.path().to_path_buf(), Method::Copy, None);

        placer.place(&object, &destination, false).unwrap();

        assert!(is_read_only(&destination));
    }

    #[test]
    fn a_writable_placement_is_a_writable_copy() {
        let (folder, object) = cache(true);
        let destination = folder.path().join("placed.csv");
        let mut placer = Placer::starting_at(folder.path().to_path_buf(), Method::Hardlink, None);

        let method = placer.place(&object, &destination, true).unwrap();

        assert_eq!(method, Method::Copy);
        assert!(!is_read_only(&destination));
        assert!(is_read_only(&object));
    }

    #[test]
    fn a_copy_goes_only_into_the_file_that_claimed_the_name() {
        let (folder, object) = cache(true);
        // FAT32 and exFAT cannot hardlink, so nobody can swap in a second name of a file
        if !makes_hardlinks(folder.path()) {
            return;
        }
        let destination = folder.path().join("placed.csv");
        let outside = folder.path().join("outside.csv");
        fs::write(&outside, "someone else's file").unwrap();
        let claimed = File::create_new(&destination).unwrap();
        // someone swaps a link to another file in for the name once it is claimed
        fs::rename(&destination, folder.path().join("claimed.csv")).unwrap();
        fs::hard_link(&outside, &destination).unwrap();

        fill(&object, &claimed, false).unwrap();

        assert_eq!(fs::read_to_string(&outside).unwrap(), "someone else's file");
        assert!(!is_read_only(&outside));
        let copied = folder.path().join("claimed.csv");
        assert_eq!(fs::read_to_string(&copied).unwrap(), CONTENTS);
        assert!(is_read_only(&copied));
    }

    #[test]
    fn a_file_of_many_blocks_is_copied_byte_for_byte() {
        let folder = TempDir::new().unwrap();
        let object = folder.path().join("object");
        let bytes: Vec<u8> = (0..(3 << 20) + 123)
            .map(|at| u8::try_from(at % 251).unwrap())
            .collect();
        fs::write(&object, &bytes).unwrap();
        let destination = folder.path().join("placed.bin");

        copy(&object, &destination, false).unwrap();

        assert_eq!(fs::read(&destination).unwrap(), bytes);
        assert!(is_read_only(&destination));
    }

    #[test]
    fn a_synced_folder_gets_a_copy_instead_of_a_hardlink() {
        let (folder, object) = cache(true);
        let destination = folder.path().join("placed.csv");
        let mut placer = Placer::starting_at(
            folder.path().to_path_buf(),
            Method::Hardlink,
            Some("onedrive"),
        );

        let method = placer.place(&object, &destination, false).unwrap();

        assert_eq!(method, Method::Copy);
    }

    #[test]
    fn the_whole_ladder_never_copies_within_one_volume() {
        let (folder, object) = cache(true);
        // FAT32 and exFAT link nothing, so there every rung ends in a copy
        if !makes_hardlinks(folder.path()) {
            return;
        }
        let mut placer = Placer::starting_at(folder.path().to_path_buf(), Method::Reflink, None);

        for name in ["first.csv", "second.csv"] {
            let destination = folder.path().join(name);
            let method = placer.place(&object, &destination, false).unwrap();
            assert_ne!(method, Method::Copy, "{name}");
            assert_eq!(fs::read_to_string(&destination).unwrap(), CONTENTS);
        }
    }

    #[test]
    fn a_method_the_filesystem_refuses_is_not_tried_again() {
        let refusals = [
            io::Error::from(io::ErrorKind::Unsupported),
            io::Error::from(io::ErrorKind::CrossesDevices),
            // how FAT32 and exFAT refuse a hardlink: ERROR_INVALID_FUNCTION on windows, EPERM on
            // linux
            io::Error::from_raw_os_error(1),
            // how NTFS refuses a reflink: "incorrect function", wrapped in an HRESULT.
            #[cfg(windows)]
            io::Error::from_raw_os_error(0x8007_0001_u32.cast_signed()),
            // ENOTSUP, how a mac volume without clones refuses one.
            #[cfg(target_os = "macos")]
            io::Error::from_raw_os_error(45),
        ];
        for refusal in refusals {
            let (folder, object) = cache(true);
            let mut placer =
                Placer::starting_at(folder.path().to_path_buf(), Method::Hardlink, None);
            // a volume whose own probe agrees that it links nothing
            placer.volume = Volume::refusing(folder.path().to_path_buf(), false);
            let described = refusal.to_string();

            placer.learn(Method::Hardlink, refusal).unwrap();
            let method = placer
                .place(&object, &folder.path().join("placed.csv"), false)
                .unwrap();

            assert_eq!(method, Method::Copy, "{described}");
        }
    }

    #[test]
    fn a_link_refused_for_one_file_only_is_tried_for_the_next() {
        let (folder, object) = cache(true);
        // FAT32 and exFAT link nothing, so there no refusal is for one file only
        if !makes_hardlinks(folder.path()) {
            return;
        }
        let mut placer = Placer::starting_at(folder.path().to_path_buf(), Method::Hardlink, None);

        // code 1 is EPERM on linux, which protected_hardlinks answers for another user's object
        // on a volume that links
        placer
            .learn(Method::Hardlink, io::Error::from_raw_os_error(1))
            .unwrap();
        let method = placer
            .place(&object, &folder.path().join("placed.csv"), false)
            .unwrap();

        assert_eq!(method, Method::Hardlink);
        assert_eq!(placer.unlinked(), None);
    }

    #[test]
    fn too_many_links_falls_back_for_that_file_alone() {
        let (folder, object) = cache(true);
        // FAT32 and exFAT make no hardlinks to fall back from
        if !makes_hardlinks(folder.path()) {
            return;
        }
        let mut placer = Placer::starting_at(folder.path().to_path_buf(), Method::Hardlink, None);

        placer
            .learn(Method::Hardlink, io::ErrorKind::TooManyLinks.into())
            .unwrap();
        let method = placer
            .place(&object, &folder.path().join("placed.csv"), false)
            .unwrap();

        assert_eq!(method, Method::Hardlink);
    }

    #[test]
    fn a_drive_that_refuses_every_link_is_told_from_a_cache_elsewhere() {
        let folder = TempDir::new().unwrap();
        let placer =
            |synced| Placer::starting_at(folder.path().to_path_buf(), Method::Reflink, synced);
        let refused = || io::Error::from(io::ErrorKind::Unsupported);

        let linking = placer(None);
        assert_eq!(linking.unlinked(), None);
        let mut no_links = placer(None);
        no_links.volume = Volume::refusing(folder.path().to_path_buf(), false);
        no_links.learn(Method::Reflink, refused()).unwrap();
        no_links.learn(Method::Hardlink, refused()).unwrap();
        assert_eq!(no_links.unlinked(), Some(Unlinked::NoLinks));
        let elsewhere = placer(None);
        elsewhere.learn(Method::Reflink, refused()).unwrap();
        elsewhere
            .learn(Method::Hardlink, io::ErrorKind::CrossesDevices.into())
            .unwrap();
        // a cache on this drive helps only where this drive makes links, which FAT does not
        let blamed = if makes_hardlinks(folder.path()) {
            Unlinked::OtherDrive
        } else {
            Unlinked::NoLinks
        };
        assert_eq!(elsewhere.unlinked(), Some(blamed));
        let synced = placer(Some("onedrive"));
        synced.learn(Method::Reflink, refused()).unwrap();
        assert_eq!(synced.unlinked(), Some(Unlinked::Synced("onedrive")));
    }

    #[test]
    fn a_cache_elsewhere_is_not_blamed_for_a_drive_that_links_nothing() {
        let folder = TempDir::new().unwrap();
        let mut placer = Placer::starting_at(folder.path().to_path_buf(), Method::Reflink, None);
        placer.volume = Volume::refusing(folder.path().to_path_buf(), false);

        placer
            .learn(Method::Reflink, io::ErrorKind::CrossesDevices.into())
            .unwrap();
        placer
            .learn(Method::Hardlink, io::ErrorKind::CrossesDevices.into())
            .unwrap();

        assert_eq!(placer.unlinked(), Some(Unlinked::NoLinks));
    }

    /// whether the filesystem holding `folder` makes hardlinks, as NTFS, ext4 and APFS do and FAT
    /// and exFAT do not.
    pub(super) fn makes_hardlinks(folder: &Path) -> bool {
        let original = folder.join("probe");
        let link = folder.join("probe-link");
        fs::write(&original, CONTENTS).unwrap();
        let made = fs::hard_link(&original, &link).is_ok();
        for probe in [link, original] {
            let _ = fs::remove_file(probe);
        }
        made
    }

    #[test]
    fn many_files_are_placed_on_several_threads_and_reported_in_order() {
        let (folder, object) = cache(true);
        let expected = if makes_hardlinks(folder.path()) {
            Method::Hardlink
        } else {
            Method::Copy
        };
        let root = folder.path().join("data");
        let mut placer = Placer::starting_at(root.clone(), Method::Hardlink, None);
        let destinations: Vec<PathBuf> = (0..300)
            .map(|index| {
                root.join(format!("{}", index % 7))
                    .join(format!("{index}.csv"))
            })
            .collect();
        let mut orders: Vec<Order> = destinations
            .iter()
            .map(|destination| Order {
                object: &object,
                destination,
                writable: false,
            })
            .collect();
        // one order that cannot succeed, to see its error land in its own place
        let taken = root.join("taken.csv");
        fs::create_dir_all(&root).unwrap();
        fs::write(&taken, "mine").unwrap();
        orders.insert(
            150,
            Order {
                object: &object,
                destination: &taken,
                writable: false,
            },
        );

        let results = placer.place_all(&orders);

        assert_eq!(results.len(), 301);
        for (index, result) in results.iter().enumerate() {
            match index {
                150 => assert_eq!(
                    result.as_ref().unwrap_err().kind(),
                    io::ErrorKind::AlreadyExists
                ),
                _ => assert_eq!(result.as_ref().unwrap().0, expected, "{index}"),
            }
        }
        for destination in &destinations {
            assert_eq!(fs::read_to_string(destination).unwrap(), CONTENTS);
        }
        assert_eq!(fs::read_to_string(&taken).unwrap(), "mine");
    }

    #[test]
    fn neighbouring_orders_go_to_different_folders() {
        let object = Path::new("object");
        let paths: Vec<PathBuf> = ["a/1", "a/2", "a/3", "b/1", "c/1", "c/2"]
            .iter()
            .map(PathBuf::from)
            .collect();
        let orders: Vec<Order> = paths
            .iter()
            .map(|destination| Order {
                object,
                destination,
                writable: false,
            })
            .collect();
        let spread = spread(orders.iter().enumerate().collect());
        let indexes: Vec<usize> = spread.iter().map(|(index, _)| *index).collect();
        assert_eq!(indexes, [0, 3, 4, 1, 5, 2]);
    }

    #[test]
    fn work_spread_over_threads_comes_back_in_order() {
        let items: Vec<u64> = (0..1000).collect();
        let doubled = run_all(&items, |item| item * 2);
        assert_eq!(
            doubled,
            items.iter().map(|item| item * 2).collect::<Vec<_>>()
        );
        assert_eq!(run_all(&Vec::<u64>::new(), |item| *item), Vec::<u64>::new());
    }

    fn is_read_only(path: &Path) -> bool {
        fs::metadata(path).unwrap().permissions().readonly()
    }

    #[test]
    fn a_link_at_the_root_is_never_followed() {
        let (folder, object) = cache(true);
        let outside = folder.path().join("outside");
        fs::create_dir(&outside).unwrap();
        let root = folder.path().join("data");
        if !link_folder(&outside, &root) {
            return;
        }
        let mut placer = Placer::new(&root, folder.path());

        let error = placer
            .place(&object, &root.join("placed.csv"), false)
            .unwrap_err();

        assert!(error.to_string().contains("is a link"), "{error}");
        assert!(!outside.join("placed.csv").exists());
    }

    #[test]
    fn a_link_inside_the_root_is_never_followed() {
        let (folder, object) = cache(true);
        let root = folder.path().join("data");
        let outside = folder.path().join("outside");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&outside).unwrap();
        let link = root.join("dataset");
        if !link_folder(&outside, &link) {
            return;
        }
        let mut placer = Placer::new(&root, folder.path());

        let error = placer
            .place(&object, &link.join("placed.csv"), false)
            .unwrap_err();

        assert!(error.to_string().contains("is a link"), "{error}");
        assert!(!outside.join("placed.csv").exists());
        assert!(placer.check_folders(&link.join("old.csv"), false).is_err());
    }

    /// makes `link` point at the folder `target`: a junction on windows, which needs no admin.
    pub(super) fn link_folder(target: &Path, link: &Path) -> bool {
        #[cfg(windows)]
        let made = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .is_ok_and(|output| output.status.success());
        #[cfg(unix)]
        let made = std::os::unix::fs::symlink(target, link).is_ok();
        made
    }

    #[test]
    fn a_folder_emptied_by_a_removal_is_made_again_for_the_next_file() {
        let (folder, object) = cache(true);
        let root = folder.path().join("data");
        let mut placer = Placer::new(&root, folder.path());
        let first = root.join("set").join("inner").join("a.csv");
        let method = placer.place(&object, &first, false).unwrap();

        placer.remove(&first, true, Some(method)).unwrap();
        assert!(!root.join("set").exists(), "emptied folders go");
        assert!(root.exists(), "the root stays");
        let second = root.join("set").join("b.csv");
        placer.place(&object, &second, false).unwrap();

        assert_eq!(fs::read_to_string(&second).unwrap(), CONTENTS);
    }

    /// a placer that copies into `root` as if `root` sat on FAT32 or exFAT, with `scratch` as
    /// fetchloom's own folder there.
    fn on_fat(root: &Path, scratch: &Path) -> Placer {
        let mut placer = Placer::starting_at(root.to_path_buf(), Method::Copy, None);
        placer.volume = Volume::refusing(scratch.to_path_buf(), false);
        placer
    }

    #[test]
    fn a_placed_read_only_file_is_removed_on_fat() {
        for recorded in [Some(Method::Copy), Some(Method::Reflink), None] {
            let (folder, object) = cache(true);
            let root = folder.path().join("data");
            let own = folder.path().join(".fetchloom");
            fs::create_dir(&own).unwrap();
            let mut placer = on_fat(&root, &own);
            let destination = root.join("placed.csv");
            placer.place(&object, &destination, false).unwrap();
            assert!(is_read_only(&destination));

            placer.remove(&destination, false, recorded).unwrap();

            assert!(!destination.exists(), "{recorded:?}");
            assert_eq!(
                fs::read_dir(&own).unwrap().count(),
                0,
                "the probe leaves nothing"
            );
        }
    }

    #[test]
    fn a_removal_on_fat_leaves_nothing_else_in_a_named_folder() {
        let (folder, object) = cache(true);
        let root = folder.path().join("fetched");
        let mut placer = on_fat(&root, &root);
        for name in ["kept.csv", "replaced.csv"] {
            placer.place(&object, &root.join(name), false).unwrap();
        }

        placer
            .remove(&root.join("replaced.csv"), false, None)
            .unwrap();

        let left: Vec<String> = fs::read_dir(&root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left, ["kept.csv"]);
    }

    #[test]
    fn a_recorded_hardlink_keeps_its_flag_even_on_fat() {
        let (folder, object) = cache(true);
        let root = folder.path().join("data");
        let mut placer = on_fat(&root, folder.path());
        let destination = root.join("placed.csv");
        placer.place(&object, &destination, false).unwrap();

        let refused = placer
            .remove(&destination, false, Some(Method::Hardlink))
            .unwrap_err();

        assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied);
        assert!(is_read_only(&destination));
    }
}
