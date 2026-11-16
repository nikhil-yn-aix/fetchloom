//! running a recipe's step in a folder of its own, reading `in/<name>` and writing `out`, and
//! taking in its result. only `fetchloom build` runs one: a step is the project's code.

pub(crate) mod capture;
pub(crate) mod compare;
pub(crate) mod inputs;
pub(crate) mod step;

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::sync::Arc;

use crate::cache::Cache;

/// the step's folder, open to its owner alone: `tmp/<16 hex>.build` in the cache, or on unix,
/// when someone else may write the cache's tmp/, `fetchloom-<16 hex>.build` in the folder TMPDIR
/// names. its lock keeps a sweep away while the build runs, and dropping it clears the folder.
pub(crate) struct Staging {
    folder: PathBuf,
    lock: Option<File>,
    #[cfg(unix)]
    held: Held,
    /// the cache's folder, which the step's folder is below.
    #[cfg(not(unix))]
    root: PathBuf,
    /// on windows, the folders from the cache's root down to the step's folder, its `in` and
    /// `out`, and once they are made, the folders of what the step reads, each held while it
    /// lives, so no one renames one of them or puts a junction in its place while the step and
    /// taking in its result reach them by path.
    #[cfg(windows)]
    holding: Vec<Box<dyn std::any::Any + Send + Sync>>,
    /// the folder TMPDIR names, when the step's folder is there.
    scratch: Option<PathBuf>,
    /// whether the step's folder is on another disk than the cache.
    elsewhere: bool,
}

/// the folder that holds the step's folder, the cache's tmp/ or TMPDIR's, and the step's folder,
/// each opened once without following a link, the step's checked to be this user's alone. taking
/// in the result and clearing go through them, so a folder put in its place is never read or
/// cleared.
#[cfg(unix)]
struct Held {
    parent: File,
    folder: Arc<File>,
    name: std::ffi::OsString,
}

impl Staging {
    /// a new folder for a step in `cache`, with `in` and `out` made and empty. on unix, when
    /// someone else may write the cache's tmp/, as in a cache a group shares, they could put a
    /// link in place of a folder there, where the step reaches its folders by path, so the step
    /// gets a folder of its own in `scratch`, the folder TMPDIR names, instead.
    pub(crate) fn new(cache: &Cache, scratch: &Path) -> io::Result<Staging> {
        #[cfg(unix)]
        {
            let (folder, lock, held, scratch) = if others_write_tmp(cache.root())? {
                cache.sweep_tmp();
                let (real, parent) = open_scratch(scratch)?;
                crate::cache::sweep_scratch(&parent, &real);
                let (folder, lock) = crate::cache::make_build_folder(&real, crate::cache::SCRATCH)
                    .map_err(|err| in_scratch(scratch, err))?;
                let held = hold_in(parent, &folder, lock.as_ref(), Some(scratch))?;
                (folder, lock, held, Some(real))
            } else {
                let (folder, lock) = cache.build_folder()?;
                let held = hold(cache.root(), &folder, lock.as_ref())?;
                (folder, lock, held, None)
            };
            // dropped on a failure from here, it clears the folder
            let mut staging = Staging {
                folder,
                lock,
                held,
                scratch,
                elsewhere: false,
            };
            staging.elsewhere = staging.scratch.is_some()
                && rustix::fs::fstat(&*staging.held.folder)?.st_dev
                    != rustix::fs::stat(cache.root())?.st_dev;
            for name in ["in", "out"] {
                let mode = rustix::fs::Mode::from_raw_mode(0o700);
                rustix::fs::mkdirat(&*staging.held.folder, name, mode)?;
            }
            Ok(staging)
        }
        #[cfg(not(unix))]
        {
            let _ = scratch;
            let (folder, lock) = cache.build_folder()?;
            // dropped on a failure from here, it clears the folder
            let mut staging = Staging {
                folder,
                lock,
                root: cache.root().to_path_buf(),
                #[cfg(windows)]
                holding: Vec::new(),
                scratch: None,
                elsewhere: false,
            };
            #[cfg(windows)]
            staging.hold(&staging.folder.join("in"))?;
            for name in ["in", "out"] {
                let made = staging.folder.join(name);
                std::fs::create_dir(&made)?;
                #[cfg(windows)]
                staging.hold(&made.join("member"))?;
            }
            Ok(staging)
        }
    }

    /// the step's `out` folder to take its result from: on unix opened through the folder held
    /// since the step began. a file in a folder of the step's own in TMPDIR is copied into the
    /// cache, never moved, since the cache may be on another disk.
    #[cfg_attr(
        not(unix),
        expect(
            clippy::unnecessary_wraps,
            reason = "unix opens the folder, which windows only names"
        )
    )]
    pub(crate) fn out_folder(&self) -> io::Result<capture::Out> {
        #[cfg(unix)]
        {
            let out = crate::cache::enter_at(&self.held.folder, "out")?;
            Ok(capture::Out::held(self.out(), out, self.scratch.is_none()))
        }
        #[cfg(not(unix))]
        Ok(capture::Out::below(self.root.clone(), self.out()))
    }

    /// holds every folder of what the step reads, once [`inputs::make`] made them, until the
    /// step's result is taken in: on windows, where a step reaches them by path, so no one
    /// renames one and puts a junction in its place. unix reaches the step's folder through a
    /// handle, in a folder no one else may change, so there is nothing to hold.
    #[cfg_attr(
        not(windows),
        expect(
            clippy::unused_self,
            clippy::unnecessary_wraps,
            reason = "only windows holds folders by path"
        )
    )]
    pub(crate) fn hold_inputs(&mut self) -> io::Result<()> {
        #[cfg(windows)]
        self.holding
            .push(Box::new(crate::place::hold_tree(&self.folder.join("in"))?));
        Ok(())
    }

    /// holds the folders from the cache's root down to the one `path` is in.
    #[cfg(windows)]
    fn hold(&mut self, path: &Path) -> io::Result<()> {
        let held = crate::place::hold_below(&self.root, path)?;
        self.holding.push(Box::new(held));
        Ok(())
    }

    /// the folder TMPDIR names, when the step's folder is there rather than in the cache.
    pub(crate) fn scratch(&self) -> Option<&Path> {
        self.scratch.as_deref()
    }

    /// the folder TMPDIR names, when the step's folder is there, on another disk than the cache.
    pub(crate) fn elsewhere(&self) -> Option<&Path> {
        self.scratch().filter(|_| self.elsewhere)
    }

    pub(crate) fn input(&self, name: &str) -> PathBuf {
        self.folder.join("in").join(name)
    }

    pub(crate) fn out(&self) -> PathBuf {
        self.folder.join("out")
    }

    pub(crate) fn folder(&self) -> &Path {
        &self.folder
    }

    /// whether the step's copies of what it reads take no room of their own on the disk holding
    /// `cache`: its folder is on another disk, or that disk clones files, as
    /// [`Cache::clones`] finds, so a copy takes no room until the step changes it.
    pub(crate) fn clones(&self, cache: &Cache) -> bool {
        self.elsewhere || cache.clones()
    }

    /// the bytes of the files the step wrote into `out` so far, found without following a link.
    pub(crate) fn written(&self) -> u64 {
        let mut bytes = 0;
        let mut folders = vec![self.out()];
        while let Some(folder) = folders.pop() {
            for entry in std::fs::read_dir(&folder).into_iter().flatten().flatten() {
                match entry.file_type() {
                    Ok(kind) if kind.is_dir() => folders.push(entry.path()),
                    Ok(kind) if kind.is_file() => {
                        bytes += entry.metadata().map_or(0, |metadata| metadata.len());
                    }
                    _ => {}
                }
            }
        }
        bytes
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        // windows keeps an open file's name on disks without posix deletes, such as fat and exfat,
        // so the folder could not go while its lock is open
        drop(self.lock.take());
        // a folder held without delete sharing cannot go, even for this process
        #[cfg(windows)]
        self.holding.clear();
        #[cfg(unix)]
        let cleared = self.held.clear();
        #[cfg(not(unix))]
        let cleared = crate::cache::clear_build(&self.root, &self.folder);
        if let Err(err) = cleared {
            tracing::debug!(%err, folder = %self.folder.display(), "could not clear a build's folder, which the next build sweeps");
        }
    }
}

/// whether someone else may change what the cache's tmp/ holds, as in a cache a group shares:
/// they may write the cache's folder, and so put another tmp/ in it, or enter it and write tmp/.
#[cfg(unix)]
fn others_write_tmp(root: &Path) -> io::Result<bool> {
    use rustix::fs::{Mode, OFlags};
    let user = rustix::process::geteuid().as_raw();
    let writes = |found: &rustix::fs::Stat| found.st_uid != user || found.st_mode & 0o022 != 0;
    // a cache folder someone named may itself be a link
    let root = File::open(root)?;
    let found = rustix::fs::fstat(&root)?;
    if writes(&found) {
        return Ok(true);
    }
    if found.st_mode & 0o011 == 0 {
        return Ok(false);
    }
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    match rustix::fs::openat(&root, "tmp", flags, Mode::empty()) {
        Ok(tmp) => Ok(writes(&rustix::fs::fstat(&tmp)?)),
        Err(rustix::io::Errno::NOENT) => Ok(false),
        Err(err) => Err(err.into()),
    }
}

/// opens `scratch`, the folder TMPDIR names, to hold a step's folder, and gives where it leads,
/// links followed, which names the step's folder, so no link stands on the way. it refuses a folder
/// where another user could swap the step's folder: each folder from the root of the disk down to
/// it must be this user's or root's, and none others may write unless it is sticky, as /tmp is,
/// where only an entry's owner may rename it.
#[cfg(unix)]
fn open_scratch(scratch: &Path) -> io::Result<(PathBuf, File)> {
    use std::os::unix::fs::MetadataExt;
    let real = std::fs::canonicalize(scratch).map_err(|err| match err.kind() {
        io::ErrorKind::NotFound => unfit(scratch, None, io::ErrorKind::NotFound, "does not exist"),
        _ => in_scratch(scratch, err),
    })?;
    let user = rustix::process::geteuid().as_raw();
    let mut at = PathBuf::new();
    let mut found = None;
    for part in real.components() {
        at.push(part);
        let metadata = std::fs::symlink_metadata(&at).map_err(|err| in_scratch(scratch, err))?;
        if !metadata.is_dir() {
            let what = if metadata.is_file() {
                "is a file, not a folder"
            } else {
                "is not a folder"
            };
            return Err(unfit(scratch, None, io::ErrorKind::NotADirectory, what));
        }
        if let Some(change) = changeable(metadata.uid(), metadata.mode(), user) {
            let above = (at != real).then_some(at.as_path());
            return Err(unfit(
                scratch,
                above,
                io::ErrorKind::PermissionDenied,
                change.said(),
            ));
        }
        found = Some((metadata.dev(), metadata.ino()));
    }
    let flags = rustix::fs::OFlags::RDONLY
        | rustix::fs::OFlags::DIRECTORY
        | rustix::fs::OFlags::NOFOLLOW
        | rustix::fs::OFlags::CLOEXEC;
    let opened = File::from(
        rustix::fs::open(&real, flags, rustix::fs::Mode::empty())
            .map_err(|err| in_scratch(scratch, err.into()))?,
    );
    let held = opened.metadata()?;
    if found != Some((held.dev(), held.ino())) {
        let said = "changed while fetchloom looked at it";
        return Err(unfit(scratch, None, io::ErrorKind::Other, said));
    }
    Ok((real, opened))
}

/// how another user could change what a folder holds.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Change {
    /// it is theirs, so they may give themselves any access to it.
    Owner,
    /// they may write it, and it is not sticky, so they may rename what it holds.
    Write,
}

#[cfg(unix)]
impl Change {
    fn said(self) -> &'static str {
        match self {
            Change::Owner => {
                "belongs to another user, who could swap the step's folder there for a link"
            }
            Change::Write => {
                "lets others write in it and is not sticky, as /tmp is, so they could swap the step's folder there for a link"
            }
        }
    }
}

/// how someone other than `user` could rename what a folder of `owner` with `mode` holds: as its
/// owner, unless that is root, or by writing it, unless it is sticky.
#[cfg(unix)]
fn changeable(owner: u32, mode: u32, user: u32) -> Option<Change> {
    if owner != user && owner != 0 {
        Some(Change::Owner)
    } else if mode & 0o022 != 0 && mode & 0o1000 == 0 {
        Some(Change::Write)
    } else {
        None
    }
}

/// the refusal of `scratch`, the folder TMPDIR names, as a place for a step, as `said` of it or
/// of `above`, a folder on the way to it.
#[cfg(unix)]
fn unfit(scratch: &Path, above: Option<&Path>, kind: io::ErrorKind, said: &str) -> io::Error {
    let named = match above {
        Some(above) => format!(
            "{}, which holds {}, the folder TMPDIR names,",
            above.display(),
            scratch.display()
        ),
        None => format!("{}, the folder TMPDIR names,", scratch.display()),
    };
    io::Error::new(kind, Unsafe(format!("{named} {said}")))
}

/// the refusal of the folder TMPDIR names as a place for a step: there is none, or another user
/// could swap the step's folder there.
#[cfg(unix)]
#[derive(Debug)]
struct Unsafe(String);

#[cfg(unix)]
impl std::fmt::Display for Unsafe {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[cfg(unix)]
impl std::error::Error for Unsafe {}

/// whether [`Staging::new`] failed because the folder TMPDIR names is no place for a step, which
/// another TMPDIR answers.
pub(crate) fn unsafe_scratch(err: &io::Error) -> bool {
    #[cfg(unix)]
    {
        matches!(err.get_ref(), Some(inner) if inner.is::<Unsafe>())
    }
    #[cfg(not(unix))]
    {
        let _ = err;
        false
    }
}

/// `err`, from the system, as met in `scratch`, the folder TMPDIR names.
#[cfg(unix)]
fn in_scratch(scratch: &Path, err: io::Error) -> io::Error {
    if err.raw_os_error().is_none() {
        return err;
    }
    io::Error::new(
        err.kind(),
        format!(
            "{} in {}, the folder TMPDIR names",
            crate::error::said(&err),
            scratch.display()
        ),
    )
}

/// opens `folder`, a step's folder in the cache at `root`, from `root` without following a link,
/// and refuses it unless it is this user's, no one else may write it, and it is the one this run
/// made: it holds `lock`, the lock file this run holds, where the disk takes locks. it is called
/// only when no one else may write the cache's tmp/, so a disk that keeps no folder modes, which
/// shows it as other than the 0700 it was made with, is no refusal.
#[cfg(unix)]
fn hold(root: &Path, folder: &Path, lock: Option<&File>) -> io::Result<Held> {
    use rustix::fs::{Mode, OFlags};
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    // a cache folder someone named may itself be a link
    let root = File::open(root)?;
    let tmp = File::from(rustix::fs::openat(&root, "tmp", flags, Mode::empty())?);
    hold_in(tmp, folder, lock, None)
}

/// [`hold`] for `folder` in the folder `parent` holds open: in `scratch`, the folder TMPDIR names,
/// when it is there, where it must be open to this user alone, as made, since a shared cache's
/// copies are made in it. a refused folder this run made goes; another stays as it is.
#[cfg(unix)]
fn hold_in(
    parent: File,
    folder: &Path,
    lock: Option<&File>,
    scratch: Option<&Path>,
) -> io::Result<Held> {
    use rustix::fs::{AtFlags, Mode, OFlags};
    let name = folder
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "a build folder has no name"))?
        .to_owned();
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let opened = File::from(rustix::fs::openat(&parent, &name, flags, Mode::empty())?);
    let another = match lock {
        Some(lock) => {
            let named = rustix::fs::statat(&opened, "lock", AtFlags::SYMLINK_NOFOLLOW)?;
            let held = rustix::fs::fstat(lock)?;
            (named.st_dev, named.st_ino) != (held.st_dev, held.st_ino)
        }
        None => false,
    };
    let found = rustix::fs::fstat(&opened)?;
    let refused = if another || !crate::cache::is_own(&found) {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} is not this user's alone", folder.display()),
        )
    } else if let Some(scratch) = scratch.filter(|_| !crate::cache::is_private(&found)) {
        let said = "is on a disk that keeps no folder permissions, so others could enter the step's folder there";
        unfit(scratch, None, io::ErrorKind::PermissionDenied, said)
    } else {
        return Ok(Held {
            parent,
            folder: Arc::new(opened),
            name,
        });
    };
    if !another && found.st_uid == rustix::process::geteuid().as_raw() {
        let _ = crate::cache::clear_folder(&parent, &name, &opened);
    }
    Err(refused)
}

#[cfg(unix)]
impl Held {
    /// clears the step's folder through its handle, then takes its name from the folder holding
    /// it only while the name is still this folder's, not one put in its place.
    fn clear(&self) -> io::Result<()> {
        crate::cache::clear_folder(&self.parent, &self.name, &self.folder)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// read-only files, a folder its owner lost access to and a link, never followed, all go.
    #[test]
    fn a_dropped_folder_leaves_nothing_and_follows_no_link() {
        let (dir, cache) = crate::cache::tests::cache();
        let outside = dir.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("keep"), b"mine").unwrap();
        let staging = Staging::new(&cache, dir.path()).unwrap();
        let folder = staging.folder().to_path_buf();
        assert!(
            folder.starts_with(cache.root()),
            "a private cache keeps its steps"
        );
        let deep = staging.out().join("a").join("b");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("x.csv"), b"x").unwrap();
        let mut permissions = std::fs::metadata(deep.join("x.csv")).unwrap().permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(deep.join("x.csv"), permissions).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::os::unix::fs::symlink(&outside, staging.out().join("link")).unwrap();
            std::fs::set_permissions(&deep, std::fs::Permissions::from_mode(0o500)).unwrap();
        }
        // fat and exfat make no junctions, and there the read-only file is the case
        #[cfg(windows)]
        let _ = crate::test_links::link_folder(&outside, &staging.out().join("link"));
        drop(staging);
        assert!(!folder.exists());
        assert_eq!(std::fs::read(outside.join("keep")).unwrap(), b"mine");
    }

    /// a folder put in the step's folder's place mid-step, as by someone who shares the cache's
    /// tmp/, is never read, sealed, moved into the cache or cleared; the step's own folder is.
    #[cfg(unix)]
    #[test]
    fn a_folder_swapped_in_mid_step_is_never_taken_or_cleared() {
        use std::sync::Arc;

        use crate::stop::Stop;
        use crate::threads::Threads;
        let (dir, cache) = crate::cache::tests::cache();
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(outside.join("out")).unwrap();
        let secret = outside.join("out").join("secret.csv");
        std::fs::write(&secret, b"someone's own bytes").unwrap();
        let staging = Staging::new(&cache, dir.path()).unwrap();
        std::fs::write(staging.out().join("made.csv"), b"what the step made").unwrap();
        let folder = staging.folder().to_path_buf();
        std::fs::rename(&folder, dir.path().join("moved")).unwrap();
        std::os::unix::fs::symlink(&outside, &folder).unwrap();

        let out = staging.out_folder().unwrap();
        let found = capture::read(&out, 100, Threads::default(), &Stop::default()).unwrap();
        capture::publish(
            &cache,
            &found,
            Threads::default(),
            &Arc::default(),
            &crate::work::Work::default(),
        )
        .unwrap();
        drop(staging);

        let paths: Vec<&str> = found
            .members
            .iter()
            .map(|member| member.path.as_str())
            .collect();
        assert_eq!(paths, ["made.csv"]);
        assert_eq!(std::fs::read(&secret).unwrap(), b"someone's own bytes");
        assert!(!std::fs::metadata(&secret).unwrap().permissions().readonly());
        let digest = crate::digest::sha256(b"someone's own bytes");
        assert!(cache.find(&digest, None).is_none());
        assert!(std::fs::symlink_metadata(&folder).unwrap().is_symlink());
        let moved = std::fs::read_dir(dir.path().join("moved")).unwrap();
        assert_eq!(
            moved.count(),
            0,
            "the step's own folder is cleared where it went"
        );
    }

    /// on windows, where a step reaches its folders by path, another user of a cache outside the
    /// user's profile cannot rename `out`, `in`, or a folder of what the step reads and put a
    /// junction in its place, as to the user's own startup folder, while the step runs: each is
    /// held from when it is made until the result is taken in.
    #[cfg(windows)]
    #[test]
    fn the_folders_a_step_reaches_cannot_be_swapped_while_it_runs() {
        let (dir, cache) = crate::cache::tests::cache();
        let mut staging = Staging::new(&cache, dir.path()).unwrap();
        let read = staging.input("raw").join("part");
        std::fs::create_dir_all(&read).unwrap();
        std::fs::write(read.join("x.csv"), b"x").unwrap();

        staging.hold_inputs().unwrap();

        let aside = dir.path().join("aside");
        for folder in [
            staging.out(),
            read,
            staging.input("raw"),
            staging.folder().join("in"),
        ] {
            let renamed = std::fs::rename(&folder, &aside);
            assert!(renamed.is_err(), "{} was renamed", folder.display());
        }
        let folder = staging.folder().to_path_buf();
        drop(staging);
        assert!(!folder.exists());
    }

    /// on windows, a folder of a step's result swapped for a junction once the result is walked,
    /// as by another user of a cache outside the user's profile, is never read through: the
    /// folders on the way stay held, so the swap fails and only the step's own bytes are taken.
    #[cfg(windows)]
    #[test]
    fn a_junction_swapped_in_mid_capture_is_never_read() {
        use std::sync::Arc;

        use crate::stop::Stop;
        use crate::threads::Threads;
        let (dir, cache) = crate::cache::tests::cache();
        let outside = dir.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        // fat and exfat make no junction, so no one can swap one in there
        if crate::test_links::link_folder(&outside, &dir.path().join("probe")).is_err() {
            return crate::test_skips::skip("symlinks", "no symbolic link can be made here");
        }
        std::fs::write(outside.join("bait.csv"), b"someone's own bytes").unwrap();
        let staging = Staging::new(&cache, dir.path()).unwrap();
        let inner = staging.out().join("zzz");
        std::fs::create_dir(&inner).unwrap();
        std::fs::write(inner.join("bait.csv"), b"what the step made").unwrap();
        let (swapped, moved, target) = (inner.clone(), dir.path().join("moved"), outside.clone());
        capture::WALKED.set(Some(Box::new(move || {
            // the held folders refuse the rename, which nothing refused before
            if std::fs::rename(&swapped, &moved).is_ok() {
                crate::test_links::link_folder(&target, &swapped).unwrap();
            }
        })));

        let out = staging.out_folder().unwrap();
        let found = capture::read(&out, 100, Threads::default(), &Stop::default());
        if let Ok(found) = &found {
            let work = crate::work::Work::default();
            capture::publish(&cache, found, Threads::default(), &Arc::default(), &work).unwrap();
        }

        let theirs = crate::digest::sha256(b"someone's own bytes");
        assert!(cache.find(&theirs, None).is_none(), "{found:?}");
        assert_eq!(
            std::fs::read(outside.join("bait.csv")).unwrap(),
            b"someone's own bytes"
        );
        assert!(capture::WALKED.take().is_none(), "the result was walked");
    }

    /// a folder put in place of the new one before it is held, even one of this user's own, is
    /// refused: it lacks the lock this run made. it is not this run's, so it stays as it was.
    #[cfg(unix)]
    #[test]
    fn a_folder_put_in_place_before_it_is_held_is_refused() {
        let (dir, cache) = crate::cache::tests::cache();
        let (folder, lock) = cache.build_folder().unwrap();
        if lock.is_none() {
            return crate::test_skips::skip("file-locks", "this disk takes no file locks");
        }
        std::fs::rename(&folder, dir.path().join("away")).unwrap();
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(folder.join("lock"), b"").unwrap();

        let refused = hold(cache.root(), &folder, lock.as_ref()).err().unwrap();

        assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied);
        assert!(folder.join("lock").exists());
    }

    /// a disk that keeps no folder modes, as vfat or exfat mounted for one user, shows the 0700
    /// a build makes its folder with as 0755: in a cache no one else may write, that folder is the
    /// step's all the same.
    #[cfg(unix)]
    #[test]
    fn a_step_folder_on_a_disk_that_keeps_no_folder_modes_is_held() {
        use std::os::unix::fs::PermissionsExt;
        let (_dir, cache) = crate::cache::tests::cache();
        let (folder, lock) = cache.build_folder().unwrap();
        std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o755)).unwrap();

        let held = hold(cache.root(), &folder, lock.as_ref());

        assert!(held.is_ok(), "{:?}", held.err());
    }

    /// in the folder TMPDIR names, a step's folder must be open to this user alone, since a
    /// shared cache's copies are made there: on a disk that shows the 0700 it was made with as
    /// 0755, it is refused with TMPDIR named, and it is not left behind.
    #[cfg(unix)]
    #[test]
    fn a_scratch_folder_others_could_enter_is_refused_and_cleared() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let scratch = dir.path().join("scratch");
        std::fs::create_dir(&scratch).unwrap();
        let (folder, lock) =
            crate::cache::make_build_folder(&scratch, crate::cache::SCRATCH).unwrap();
        std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o755)).unwrap();
        let parent = File::open(&scratch).unwrap();

        let refused = hold_in(parent, &folder, lock.as_ref(), Some(&scratch))
            .err()
            .unwrap();

        assert!(unsafe_scratch(&refused), "{refused}");
        assert_eq!(
            refused.to_string(),
            format!(
                "{}, the folder TMPDIR names, is on a disk that keeps no folder permissions, so others could enter the step's folder there",
                scratch.display()
            )
        );
        drop(lock);
        assert!(!folder.exists());
    }

    /// the step's own folder, its lock in place, is refused once others may write it, as a folder
    /// a co-user could have swapped in through would be.
    #[cfg(unix)]
    #[test]
    fn a_folder_others_can_write_is_refused_though_it_holds_the_lock() {
        use std::os::unix::fs::PermissionsExt;
        let (_dir, cache) = crate::cache::tests::cache();
        let (folder, lock) = cache.build_folder().unwrap();
        if lock.is_none() {
            return crate::test_skips::skip("file-locks", "this disk takes no file locks");
        }
        std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o770)).unwrap();

        let refused = hold(cache.root(), &folder, lock.as_ref()).err().unwrap();

        assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn a_left_folder_is_swept_only_once_free() {
        let (dir, cache) = crate::cache::tests::cache();
        let running = Staging::new(&cache, dir.path()).unwrap();
        let (left, lock) = cache.build_folder().unwrap();
        std::fs::create_dir(left.join("out")).unwrap();
        std::fs::write(left.join("out").join("x.csv"), b"x").unwrap();
        drop(lock);
        let next = Staging::new(&cache, dir.path()).unwrap();
        assert!(!left.exists(), "a free folder is swept");
        assert!(running.folder().exists(), "a held one stays");
        drop((running, next));
    }

    /// in a cache whose tmp/ others may write, as a group's, a step runs in a folder of its own
    /// outside the cache: a co-user who swaps every build folder in tmp/ for a link to a folder of
    /// theirs once the step's folder is made never gets what the step writes, and its result
    /// still enters the cache.
    #[cfg(unix)]
    #[test]
    fn a_step_in_a_shared_cache_writes_only_in_its_own_folder() {
        use std::os::unix::fs::PermissionsExt;
        use std::sync::Arc;

        use crate::stop::Stop;
        use crate::threads::Threads;
        let (dir, cache) = crate::cache::tests::cache();
        std::fs::set_permissions(cache.root(), std::fs::Permissions::from_mode(0o770)).unwrap();
        let scratch = dir.path().join("scratch");
        std::fs::create_dir(&scratch).unwrap();
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(outside.join("out")).unwrap();

        let staging = Staging::new(&cache, &scratch).unwrap();
        for entry in std::fs::read_dir(cache.root().join("tmp")).unwrap() {
            let path = entry.unwrap().path();
            if path
                .extension()
                .is_some_and(|extension| extension == "build")
            {
                std::fs::rename(&path, dir.path().join("moved")).unwrap();
                std::os::unix::fs::symlink(&outside, &path).unwrap();
            }
        }
        std::fs::write(staging.out().join("made.csv"), b"what the step made").unwrap();

        assert_eq!(std::fs::read_dir(outside.join("out")).unwrap().count(), 0);
        let out = staging.out_folder().unwrap();
        let found = capture::read(&out, 100, Threads::default(), &Stop::default()).unwrap();
        capture::publish(
            &cache,
            &found,
            Threads::default(),
            &Arc::default(),
            &crate::work::Work::default(),
        )
        .unwrap();
        let digest = crate::digest::sha256(b"what the step made");
        assert!(cache.find(&digest, None).is_some());
        let folder = staging.folder().to_path_buf();
        drop(staging);
        assert!(!folder.exists());
    }

    /// a folder on the way to the one TMPDIR names, or that one, holds no step when another user
    /// could rename what it holds: one of another user's, though sticky as a lab's shared scratch
    /// may be, or one others may write that is not sticky, as /tmp is.
    #[cfg(unix)]
    #[test]
    fn a_folder_another_user_could_change_holds_no_step() {
        let user = 1000;
        for (owner, mode, refused) in [
            (user, 0o40_700, None),
            (user, 0o40_755, None),
            (user, 0o41_777, None),
            (0, 0o40_755, None),
            (0, 0o41_777, None),
            (12345, 0o41_777, Some(Change::Owner)),
            (12345, 0o40_700, Some(Change::Owner)),
            (user, 0o40_775, Some(Change::Write)),
            (user, 0o42_770, Some(Change::Write)),
            (user, 0o40_757, Some(Change::Write)),
            (0, 0o40_775, Some(Change::Write)),
        ] {
            assert_eq!(changeable(owner, mode, user), refused, "{owner} {mode:o}");
        }
    }

    /// a shared cache's step never runs where a co-user could swap its folder: a TMPDIR of this
    /// user's that the group may write, as `mkdir` makes one under `umask 002`, or one in such a
    /// folder, is refused before any folder is made there, naming the folder and TMPDIR.
    #[cfg(unix)]
    #[test]
    fn a_shared_cache_refuses_a_scratch_folder_others_may_write() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, cache) = crate::cache::tests::cache();
        std::fs::set_permissions(cache.root(), std::fs::Permissions::from_mode(0o770)).unwrap();
        let group = dir.path().join("group");
        let above = dir.path().join("above");
        std::fs::create_dir_all(&group).unwrap();
        std::fs::create_dir_all(above.join("tmp")).unwrap();
        for folder in [&group, &above] {
            std::fs::set_permissions(folder, std::fs::Permissions::from_mode(0o775)).unwrap();
        }
        std::fs::set_permissions(above.join("tmp"), std::fs::Permissions::from_mode(0o700))
            .unwrap();

        for (scratch, named) in [
            (group.clone(), group.clone()),
            (above.join("tmp"), std::fs::canonicalize(&above).unwrap()),
        ] {
            let refused = Staging::new(&cache, &scratch).err().unwrap();

            assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied);
            assert!(unsafe_scratch(&refused), "{refused}");
            let said = refused.to_string();
            assert!(said.contains("TMPDIR"), "{said}");
            assert!(said.contains(&format!("{},", named.display())), "{said}");
            assert_eq!(std::fs::read_dir(&scratch).unwrap().count(), 0);
        }
    }

    /// the step's folders are named from where TMPDIR leads, so a link on the way, which someone
    /// else may be able to change, never stands between the step and its folders.
    #[cfg(unix)]
    #[test]
    fn a_step_in_scratch_reaches_its_folders_without_a_link() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, cache) = crate::cache::tests::cache();
        std::fs::set_permissions(cache.root(), std::fs::Permissions::from_mode(0o770)).unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let staging = Staging::new(&cache, &link).unwrap();

        let real = std::fs::canonicalize(&real).unwrap();
        assert!(
            staging.folder().starts_with(&real),
            "{}",
            staging.folder().display()
        );
        assert_eq!(staging.scratch(), Some(real.as_path()));
    }

    /// a file where TMPDIR names a folder, or nothing there, is said to be so, with TMPDIR named.
    #[cfg(unix)]
    #[test]
    fn a_scratch_folder_that_is_no_folder_is_named() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, cache) = crate::cache::tests::cache();
        std::fs::set_permissions(cache.root(), std::fs::Permissions::from_mode(0o770)).unwrap();
        let file = dir.path().join("file");
        std::fs::write(&file, b"").unwrap();

        for (scratch, said) in [
            (file, "is a file, not a folder"),
            (dir.path().join("missing"), "does not exist"),
        ] {
            let refused = Staging::new(&cache, &scratch).err().unwrap();

            assert!(unsafe_scratch(&refused), "{refused}");
            let expected = format!("{}, the folder TMPDIR names, {said}", scratch.display());
            assert_eq!(refused.to_string(), expected);
        }
    }

    /// a private folder a killed build left in TMPDIR goes at the next build once its lock is
    /// free; one a build still holds stays.
    #[cfg(unix)]
    #[test]
    fn a_folder_left_in_scratch_is_swept_once_free() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, cache) = crate::cache::tests::cache();
        std::fs::set_permissions(cache.root(), std::fs::Permissions::from_mode(0o770)).unwrap();
        let scratch = dir.path().join("scratch");
        let left = scratch.join("fetchloom-0123456789abcdef.build");
        std::fs::create_dir_all(left.join("out")).unwrap();
        std::fs::write(left.join("lock"), b"").unwrap();
        std::fs::write(left.join("out").join("x.csv"), b"x").unwrap();
        std::fs::set_permissions(&left, std::fs::Permissions::from_mode(0o700)).unwrap();

        let running = Staging::new(&cache, &scratch).unwrap();
        let next = Staging::new(&cache, &scratch).unwrap();

        assert!(!left.exists(), "a free folder is swept");
        assert!(running.folder().exists(), "a held one stays");
        drop((running, next));
    }
}
