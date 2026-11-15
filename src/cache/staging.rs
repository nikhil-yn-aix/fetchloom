//! per-archive staging folders and the folders recipes' steps run in. a held lock tells a live
//! writer from a killed run; without locks, the age rule of other temporary files holds.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;

use super::upkeep::GRACE;
use super::{Cache, Failure, NewObject};
use crate::place::Placer;
use crate::stop::Stop;
use crate::volume::open_unfollowed;

const SHARDS: usize = 16;

/// the extension of the folder a recipe's step runs in, which a sweep clears once no build holds
/// its lock.
const BUILD: &str = "build";

/// how the name of a step's folder in TMPDIR starts, as no other program's there does.
#[cfg(unix)]
pub(crate) const SCRATCH: &str = "fetchloom-";

#[cfg(test)]
thread_local! {
    /// what a test does to a folder's name once a clear has looked at the folder and before its
    /// owner gets its access back, as a co-user swapping it for a link meanwhile would, once.
    pub(super) static LOOKED: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

pub(crate) struct Staging {
    cache: Cache,
    folder: PathBuf,
    lock: Option<File>,
    namespace: Placer,
    shards: [AtomicBool; SHARDS],
}

impl Cache {
    /// starts an archive's staging folder after reclaiming abandoned ones. its lock gets its name
    /// only once held, so a sweep never takes the lock of a writer that is still opening it.
    pub(crate) fn staging(&self) -> io::Result<Arc<Staging>> {
        self.sweep_staging(SystemTime::now(), &Stop::default(), &mut Vec::new());
        let namespace = Placer::in_named_folder(&self.root);
        let mut staging = loop {
            let folder = self
                .root
                .join("tmp")
                .join(format!("{:016x}.stage", fastrand::u64(..)));
            match namespace.create_folder(&folder) {
                Ok(()) => {
                    break Staging {
                        cache: self.clone(),
                        folder,
                        lock: None,
                        namespace,
                        shards: std::array::from_fn(|_| AtomicBool::new(false)),
                    };
                }
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
                Err(err) => return Err(err),
            }
        };
        let opening = staging.folder.join("lock.new");
        let lock = staging.namespace.create_file(&opening, false)?;
        match lock.try_lock() {
            Ok(()) => {
                staging
                    .namespace
                    .rename_file(&opening, &staging.folder.join("lock"))?;
                staging.lock = Some(lock);
            }
            Err(TryLockError::WouldBlock) => return Err(io::ErrorKind::WouldBlock.into()),
            Err(TryLockError::Error(_)) => {
                drop(lock);
                staging
                    .namespace
                    .rename_file(&opening, &staging.folder.join("unlocked"))?;
            }
        }
        Ok(Arc::new(staging))
    }

    /// makes `tmp/<16 hex>.build` for one step of a build, as [`make_build_folder`] does, after
    /// clearing those of ended builds.
    pub(crate) fn build_folder(&self) -> io::Result<(PathBuf, Option<File>)> {
        self.sweep_tmp();
        make_build_folder(&self.root.join("tmp"), "")
    }

    /// clears what ended runs left in tmp/: archive staging, and build folders no build holds.
    pub(crate) fn sweep_tmp(&self) {
        self.sweep_staging(SystemTime::now(), &Stop::default(), &mut Vec::new());
    }

    /// whether the disk holding the cache clones files, as a probe in an archive's staging folder,
    /// reached through held folders, finds: a copy of a cached file then takes no room of its own
    /// until it changes.
    pub(crate) fn clones(&self) -> bool {
        let Ok(staging) = self.staging() else {
            return false;
        };
        let probe = staging.folder.join("probe");
        staging
            .namespace
            .create_file(&probe, false)
            .and_then(|mut file| io::Write::write_all(&mut file, b"probe"))
            .is_ok()
            && staging
                .namespace
                .copy_file(&probe, &staging.folder.join("clone"), false)
                .is_ok_and(|method| method == crate::place::Method::Reflink)
    }

    /// reclaims unlocked staging at once. a folder without a lock, one a writer is still opening
    /// or one on a filesystem without locks, waits for the age rule. a held lock wins over age.
    pub(super) fn sweep_staging(
        &self,
        now: SystemTime,
        stop: &Stop,
        not_removed: &mut Vec<Failure>,
    ) -> u64 {
        let mut removed = 0;
        let Ok(entries) = super::upkeep::scan_entries(&self.root.join("tmp"), not_removed, stop)
        else {
            return 0;
        };
        for entry in entries {
            if stop.is_asked() {
                break;
            }
            if !entry
                .file_type()
                .is_ok_and(|kind| kind.is_dir() && !kind.is_symlink())
            {
                continue;
            }
            let folder = entry.path();
            if folder
                .extension()
                .is_some_and(|extension| extension == BUILD)
            {
                // a pretending sweep only counts, since a build's folder is never a cache file
                match self.sweep_build(&folder, now, stop, !self.volume.is_pretending()) {
                    Ok(swept) => removed += u64::from(swept),
                    Err(error) => {
                        not_removed.push(Failure::new(&folder, &error));
                        removed += 1;
                    }
                }
                continue;
            }
            if folder
                .extension()
                .is_none_or(|extension| extension != "stage")
            {
                continue;
            }
            let lock = match open_unfollowed(&folder.join("lock"), false) {
                Ok(lock) => match lock.try_lock() {
                    Ok(()) => Some(lock),
                    Err(TryLockError::WouldBlock) => continue,
                    Err(TryLockError::Error(_)) => None,
                },
                Err(_) => None,
            };
            if lock.is_some() || old_staging(&folder, now, stop) {
                removed += self.clear_staging(&folder, lock, stop, not_removed);
            }
        }
        removed
    }

    /// [`sweep_build_at`] for the build folder at `folder` in tmp/, reached from the cache's root.
    #[cfg(unix)]
    fn sweep_build(
        &self,
        folder: &Path,
        now: SystemTime,
        stop: &Stop,
        clear: bool,
    ) -> io::Result<bool> {
        let tmp = super::folder::walk(&self.root, Path::new("tmp"), false)?;
        sweep_build_at(&tmp, folder, now, stop, clear)
    }

    /// clears the build folder at `folder` in tmp/ as [`sweep_build_at`] does on unix. windows
    /// says no owner safely, so the folders from the cache's root down to it are held while std's
    /// `remove_dir_all` clears it through handles that never follow a reparse point.
    #[cfg(not(unix))]
    fn sweep_build(
        &self,
        folder: &Path,
        now: SystemTime,
        stop: &Stop,
        clear: bool,
    ) -> io::Result<bool> {
        let lock = match open_unfollowed(&folder.join("lock"), false) {
            Ok(lock) => match lock.try_lock() {
                Ok(()) => Some(lock),
                Err(TryLockError::WouldBlock) => return Ok(false),
                Err(TryLockError::Error(_)) => None,
            },
            Err(_) => None,
        };
        if lock.is_none() && !old_staging(folder, now, stop) {
            return Ok(false);
        }
        if clear {
            drop(lock);
            clear_held(&self.root, folder, || {})?;
        }
        Ok(true)
    }

    fn clear_staging(
        &self,
        folder: &Path,
        lock: Option<File>,
        stop: &Stop,
        not_removed: &mut Vec<Failure>,
    ) -> u64 {
        let namespace = Placer::in_named_folder(&self.root);
        if let Err(error) = namespace.check_parent(&folder.join(".member")) {
            if error.kind() != io::ErrorKind::NotFound {
                not_removed.push(Failure::new(folder, &error));
            }
            return 0;
        }
        let remove = |path: &Path| {
            if self.volume.is_pretending() {
                self.remove_inside(path)
            } else {
                namespace.delete_file(path)
            }
        };
        let entries = match fs::read_dir(folder) {
            Ok(entries) => entries,
            Err(err) => {
                not_removed.push(Failure::new(folder, &err));
                return 0;
            }
        };
        let mut removed = 0;
        let mut cleared = true;
        for entry in entries {
            if stop.is_asked() {
                cleared = false;
                break;
            }
            let entry = match entry {
                Ok(entry) => entry,
                Err(err) => {
                    not_removed.push(Failure::new(folder, &err));
                    cleared = false;
                    continue;
                }
            };
            if entry.file_name() == "lock" || entry.file_name() == "unlocked" {
                continue;
            }
            if entry
                .file_type()
                .is_ok_and(|kind| kind.is_dir() && !kind.is_symlink())
            {
                let (inside, emptied) = self.clear_shard(&entry.path(), stop, not_removed);
                removed += inside;
                cleared &= emptied;
                continue;
            }
            match remove(&entry.path()) {
                Ok(()) => removed += 1,
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(err) => {
                    not_removed.push(Failure::new(&entry.path(), &err));
                    cleared = false;
                }
            }
        }
        // no writer reuses a staging folder. closing its lock before deleting it lets windows
        // remove the folder too, without an open child waiting for deletion
        drop(lock);
        if cleared {
            for name in ["lock", "unlocked"] {
                let path = folder.join(name);
                super::upkeep::removed(&path, remove(&path), not_removed);
            }
            let folder_removed = if self.volume.is_pretending() {
                self.check_inside(folder)
                    .and_then(|()| self.volume.remove_folder(folder))
            } else {
                namespace.delete_folder(folder)
            };
            super::upkeep::removed(folder, folder_removed, not_removed);
        }
        removed
    }

    fn clear_shard(
        &self,
        shard: &Path,
        stop: &Stop,
        not_removed: &mut Vec<Failure>,
    ) -> (u64, bool) {
        let namespace = Placer::in_named_folder(&self.root);
        if let Err(error) = namespace.check_parent(&shard.join(".member")) {
            if error.kind() == io::ErrorKind::NotFound {
                return (0, true);
            }
            not_removed.push(Failure::new(shard, &error));
            return (0, false);
        }
        let entries = match fs::read_dir(shard) {
            Ok(entries) => entries,
            Err(error) => {
                not_removed.push(Failure::new(shard, &error));
                return (0, false);
            }
        };
        let mut removed = 0;
        for entry in entries {
            if stop.is_asked() {
                return (removed, false);
            }
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    not_removed.push(Failure::new(shard, &error));
                    return (removed, false);
                }
            };
            let result = if self.volume.is_pretending() {
                self.remove_inside(&entry.path())
            } else {
                namespace.delete_file(&entry.path())
            };
            match result {
                Ok(()) => removed += 1,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => {
                    not_removed.push(Failure::new(&entry.path(), &error));
                    return (removed, false);
                }
            }
        }
        let result = if self.volume.is_pretending() {
            self.check_inside(shard)
                .and_then(|()| self.volume.remove_folder(shard))
        } else {
            namespace.delete_folder(shard)
        };
        match result {
            Ok(()) => (removed, true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => (removed, true),
            Err(error) => {
                not_removed.push(Failure::new(shard, &error));
                (removed, false)
            }
        }
    }
}

/// a large member can keep growing without changing its folder's timestamp.
fn old_staging(folder: &Path, now: SystemTime, stop: &Stop) -> bool {
    fs::metadata(folder)
        .and_then(|metadata| metadata.modified())
        .is_ok_and(|modified| now.duration_since(modified).is_ok_and(|age| age > GRACE))
        && all_old(folder, true, now, stop)
}

fn all_old(folder: &Path, deeper: bool, now: SystemTime, stop: &Stop) -> bool {
    fs::read_dir(folder).is_ok_and(|mut entries| {
        entries.all(|entry| {
            !stop.is_asked()
                && entry.is_ok_and(|entry| {
                    let inside = deeper && entry.file_type().is_ok_and(|kind| kind.is_dir());
                    entry
                        .metadata()
                        .and_then(|metadata| metadata.modified())
                        .is_ok_and(|modified| {
                            now.duration_since(modified).is_ok_and(|age| age > GRACE)
                        })
                        && (!inside || all_old(&entry.path(), false, now, stop))
                })
        })
    })
}

/// makes `<prefix><16 hex>.build` in `parent` for one step of a build, only its owner's on unix.
/// its lock, named once held, tells a sweep the build still runs.
pub(crate) fn make_build_folder(
    parent: &Path,
    prefix: &str,
) -> io::Result<(PathBuf, Option<File>)> {
    let folder = loop {
        let folder = parent.join(format!("{prefix}{:016x}.{BUILD}", fastrand::u64(..)));
        #[cfg(unix)]
        let made = std::os::unix::fs::DirBuilderExt::mode(&mut fs::DirBuilder::new(), 0o700)
            .create(&folder);
        #[cfg(not(unix))]
        let made = fs::create_dir(&folder);
        match made {
            Ok(()) => break folder,
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
            Err(err) => return Err(err),
        }
    };
    let opening = folder.join("lock.new");
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&opening)?;
    match lock.try_lock() {
        Ok(()) => {
            fs::rename(&opening, folder.join("lock"))?;
            Ok((folder, Some(lock)))
        }
        Err(TryLockError::WouldBlock) => Err(io::ErrorKind::WouldBlock.into()),
        Err(TryLockError::Error(_)) => {
            drop(lock);
            fs::rename(&opening, folder.join("unlocked"))?;
            Ok((folder, None))
        }
    }
}

/// clears what killed builds left in `scratch`, the folder TMPDIR names, which `parent` holds
/// open: the folders [`make_build_folder`] names there that this user owns and no one else may
/// open, once no build holds their lock, as [`sweep_build_at`] says.
#[cfg(unix)]
pub(crate) fn sweep_scratch(parent: &File, scratch: &Path) {
    let Ok(entries) = fs::read_dir(scratch) else {
        return;
    };
    let suffix = format!(".{BUILD}");
    for entry in entries.flatten() {
        let name = entry.file_name();
        let named = name
            .to_str()
            .is_some_and(|name| name.starts_with(SCRATCH) && name.ends_with(&suffix));
        if !named || !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let now = SystemTime::now();
        if let Err(err) = sweep_build_at(parent, &entry.path(), now, &Stop::default(), true) {
            tracing::debug!(%err, folder = %entry.path().display(), "could not clear a build's folder in TMPDIR");
        }
    }
}

/// clears the build folder at `path`, which the folder `parent` holds by its name, once no build
/// holds its lock, or on a disk without locks once it is old, and says whether it went, or would
/// with `clear` false. only a folder this user owns and no one else may open, as
/// [`Cache::build_folder`] makes one, is cleared, through a handle on it: a folder someone put
/// there under a build's name, as one of this user's that others can write, stays as it is.
#[cfg(unix)]
pub(crate) fn sweep_build_at(
    parent: &File,
    path: &Path,
    now: SystemTime,
    stop: &Stop,
    clear: bool,
) -> io::Result<bool> {
    use rustix::fs::{Mode, OFlags};
    let Some(name) = path.file_name() else {
        return Ok(false);
    };
    let Ok(Some(folder)) = private_at(parent, name) else {
        return Ok(false);
    };
    // nfs and smb lock byte ranges for flock, and lock no handle open only to read
    let flags = OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let opened = match rustix::fs::openat(&folder, "lock", flags | OFlags::RDWR, Mode::empty()) {
        Err(rustix::io::Errno::ACCESS) => {
            rustix::fs::openat(&folder, "lock", flags | OFlags::RDONLY, Mode::empty())
        }
        opened => opened,
    };
    let lock = match opened {
        Ok(lock) => {
            let lock = File::from(lock);
            match crate::volume::try_lock(&lock) {
                Ok(()) => Some(lock),
                Err(TryLockError::WouldBlock) => return Ok(false),
                Err(TryLockError::Error(_)) => None,
            }
        }
        Err(_) => None,
    };
    if lock.is_none() && !old_staging(path, now, stop) {
        return Ok(false);
    }
    if clear {
        drop(lock);
        clear_folder(parent, name, &folder)?;
    }
    Ok(true)
}

/// the folder `name` in the folder `parent` holds, opened without following a link, when this
/// user owns it and no one else may write it, as [`is_own`] says; none for any other folder.
#[cfg(unix)]
pub(crate) fn private_at(parent: &File, name: &std::ffi::OsStr) -> io::Result<Option<File>> {
    use rustix::fs::{Mode, OFlags};
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let folder = File::from(rustix::fs::openat(parent, name, flags, Mode::empty())?);
    Ok(is_own(&rustix::fs::fstat(&folder)?).then_some(folder))
}

/// whether a folder is this user's and open to no one else: 0700, as a build's folder is made,
/// a setgid bit aside, which a folder made in a group's cache takes from its parent.
#[cfg(unix)]
pub(crate) fn is_private(found: &rustix::fs::Stat) -> bool {
    found.st_uid == rustix::process::geteuid().as_raw() && found.st_mode & 0o777 == 0o700
}

/// whether a folder is this user's and no one else may write in it, as a build's folder is,
/// whatever else its mode says: a disk that keeps no folder modes, as vfat or exfat mounted for
/// one user, shows the 0700 a build makes as 0755. another user can move only a folder they may
/// write into tmp/ under a build's name, since a moved folder's `..` changes.
#[cfg(unix)]
pub(crate) fn is_own(found: &rustix::fs::Stat) -> bool {
    found.st_uid == rustix::process::geteuid().as_raw() && found.st_mode & 0o022 == 0
}

/// clears what the folder `folder` holds open holds, then takes its name from the folder `parent`
/// holds only while the name is still that folder's, not one put in its place. it goes on past a
/// failure and returns the first.
#[cfg(unix)]
pub(crate) fn clear_folder(parent: &File, name: &std::ffi::OsStr, folder: &File) -> io::Result<()> {
    use rustix::fs::AtFlags;
    let mut first = None;
    for child in names_at(folder)? {
        match clear_at(folder, child.as_c_str()) {
            Err(err) if err.kind() != io::ErrorKind::NotFound => {
                first.get_or_insert(err);
            }
            _ => {}
        }
    }
    if let Some(err) = first {
        return Err(err);
    }
    let named = rustix::fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW)?;
    let held = rustix::fs::fstat(folder)?;
    if (named.st_dev, named.st_ino) == (held.st_dev, held.st_ino) {
        rustix::fs::unlinkat(parent, name, AtFlags::REMOVEDIR)?;
    }
    Ok(())
}

/// removes `folder`, below the cache's `root`, and all it holds: on windows with the folders from
/// `root` down to it held so none is a link or becomes one meanwhile, as [`clear_held`] says.
#[cfg(not(unix))]
pub(crate) fn clear_build(root: &Path, folder: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        clear_held(root, folder, || {})
    }
    #[cfg(not(windows))]
    {
        let _ = root;
        fs::remove_dir_all(folder)
    }
}

/// removes `folder` and all it holds with the folders from `root` down to it held, running
/// `meanwhile` once they are. windows says no owner of a build folder safely, so the tree is removed
/// through handles that never follow a reparse point, as std's `remove_dir_all` does there. a disk
/// without posix deletes, such as fat or exfat, deletes no read-only file, so when that removal
/// fails, the read-only files left are made writable and it is tried once more.
#[cfg(windows)]
pub(super) fn clear_held(root: &Path, folder: &Path, meanwhile: impl FnOnce()) -> io::Result<()> {
    let _held = crate::place::hold_below(root, folder)?;
    meanwhile();
    if fs::remove_dir_all(folder).is_ok() {
        return Ok(());
    }
    writable_below(root, folder)?;
    fs::remove_dir_all(folder)
}

/// makes each read-only file in `folder`, below the cache's `root`, writable, with every folder
/// from `root` down to the one it is in held, so none is swapped for a junction meanwhile.
#[cfg(windows)]
fn writable_below(root: &Path, folder: &Path) -> io::Result<()> {
    let mut held = Vec::new();
    let mut folders = vec![folder.to_path_buf()];
    while let Some(current) = folders.pop() {
        held.push(crate::place::hold_below(root, &current.join("member"))?);
        for entry in fs::read_dir(&current)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                folders.push(entry.path());
            } else if kind.is_file() && entry.metadata()?.permissions().readonly() {
                crate::volume::set_read_only(&entry.path(), false)?;
            }
        }
    }
    Ok(())
}

/// removes `name`, in the folder `parent` holds open, and all it holds, a link as itself, never
/// following one, first giving a folder back the access a step may have taken from its owner. it
/// goes on past a failure and returns the first.
#[cfg(unix)]
pub(crate) fn clear_at(parent: &File, name: impl rustix::path::Arg + Copy) -> io::Result<()> {
    use rustix::fs::{AtFlags, FileType};
    let found = rustix::fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW)?;
    if FileType::from_raw_mode(found.st_mode) != FileType::Directory {
        return Ok(rustix::fs::unlinkat(parent, name, AtFlags::empty())?);
    }
    let folder = enter_at(parent, name)?;
    let names = names_at(&folder)?;
    let mut first = None;
    for child in &names {
        match clear_at(&folder, child.as_c_str()) {
            Err(err) if err.kind() != io::ErrorKind::NotFound => {
                first.get_or_insert(err);
            }
            _ => {}
        }
    }
    drop(folder);
    let removed = rustix::fs::unlinkat(parent, name, AtFlags::REMOVEDIR).map_err(io::Error::from);
    first.map_or(removed, Err)
}

/// opens the folder `name` in the folder `parent` holds open, never following a link, first giving
/// it back the access a step may have taken from its owner. only a folder this user owns gets it,
/// through a handle on that very folder, so a link put at its name meanwhile changes nothing.
#[cfg(target_os = "linux")]
pub(crate) fn enter_at(parent: &File, name: impl rustix::path::Arg + Copy) -> io::Result<File> {
    use std::os::fd::AsRawFd;

    use rustix::fs::{Mode, OFlags};
    use rustix::io::Errno;
    // a path handle needs no access to the folder, and with these flags a link is refused
    let flags = OFlags::PATH | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let held = rustix::fs::openat(parent, name, flags, Mode::empty()).map_err(|err| match err {
        Errno::NOTDIR | Errno::LOOP => not_a_folder(),
        err => err.into(),
    })?;
    let found = rustix::fs::fstat(&held)?;
    if found.st_mode & 0o700 != 0o700 && found.st_uid == rustix::process::geteuid().as_raw() {
        #[cfg(test)]
        if let Some(swap) = LOOKED.take() {
            swap();
        }
        // fchmod refuses a path handle, and its name in /proc/self/fd leads to the folder held
        let mode = Mode::from_raw_mode((found.st_mode & 0o7777) | 0o700);
        rustix::fs::chmod(format!("/proc/self/fd/{}", held.as_raw_fd()).as_str(), mode)?;
    }
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
    Ok(File::from(rustix::fs::openat(
        &held,
        ".",
        flags,
        Mode::empty(),
    )?))
}

/// [`enter_at`] where no path handle exists: the mode changes by name without following a link,
/// and the folder opened must be the one looked at.
#[cfg(all(unix, not(target_os = "linux")))]
pub(crate) fn enter_at(parent: &File, name: impl rustix::path::Arg + Copy) -> io::Result<File> {
    use rustix::fs::{AtFlags, FileType, Mode, OFlags};
    let found = rustix::fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW)?;
    if FileType::from_raw_mode(found.st_mode) != FileType::Directory {
        return Err(not_a_folder());
    }
    if found.st_mode & 0o700 != 0o700 && found.st_uid == rustix::process::geteuid().as_raw() {
        #[cfg(test)]
        if let Some(swap) = LOOKED.take() {
            swap();
        }
        let mode = Mode::from_raw_mode((found.st_mode & 0o7777) | 0o700);
        rustix::fs::chmodat(parent, name, mode, AtFlags::SYMLINK_NOFOLLOW)?;
    }
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let folder = File::from(rustix::fs::openat(parent, name, flags, Mode::empty())?);
    let opened = rustix::fs::fstat(&folder)?;
    if (opened.st_dev, opened.st_ino) != (found.st_dev, found.st_ino) {
        return Err(not_a_folder());
    }
    Ok(folder)
}

#[cfg(unix)]
fn not_a_folder() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "a link or a file is where a folder should be",
    )
}

/// the names in the folder `folder` holds open.
#[cfg(unix)]
pub(crate) fn names_at(folder: &File) -> io::Result<Vec<std::ffi::CString>> {
    let mut names = Vec::new();
    for entry in rustix::fs::Dir::read_from(folder)? {
        let entry = entry?;
        if entry.file_name() != c"." && entry.file_name() != c".." {
            names.push(entry.file_name().to_owned());
        }
    }
    Ok(names)
}

impl Staging {
    pub(super) fn create_member(&self, path: &Path) -> io::Result<File> {
        self.namespace.create_file(path, true)
    }

    pub(super) fn remove_member(&self, path: &Path) -> io::Result<()> {
        self.namespace.delete_file(path)
    }

    pub(super) fn check_member_parent(&self, path: &Path) -> io::Result<()> {
        self.namespace.check_parent(path)
    }

    pub(crate) fn new_object(self: &Arc<Self>) -> io::Result<NewObject> {
        let shard = fastrand::usize(..self.shards.len());
        let folder = self.folder.join(format!("{shard:02x}"));
        if !self.shards[shard].load(Ordering::Relaxed) {
            match self.namespace.create_folder(&folder) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    self.namespace.check_parent(&folder.join(".member"))?;
                }
                Err(error) => return Err(error),
            }
            self.shards[shard].store(true, Ordering::Relaxed);
        }
        self.cache.new_object_in(&folder, Some(Arc::clone(self)))
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        self.cache.clear_staging(
            &self.folder,
            self.lock.take(),
            &Stop::default(),
            &mut Vec::new(),
        );
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Write;

    use super::*;

    use super::super::tests::cache;

    #[cfg(unix)]
    fn member_path(staging: &Staging) -> PathBuf {
        fs::read_dir(&staging.folder)
            .unwrap()
            .map(Result::unwrap)
            .filter(|entry| entry.file_type().unwrap().is_dir())
            .flat_map(|entry| fs::read_dir(entry.path()).unwrap().map(Result::unwrap))
            .find(|entry| entry.path().extension().is_some_and(|ext| ext == "new"))
            .unwrap()
            .path()
    }

    #[cfg(unix)]
    #[test]
    fn clearing_never_follows_a_link_swapped_in_for_the_folder() {
        let (dir, cache) = cache();
        let victim = dir.path().join("victim");
        fs::create_dir(&victim).unwrap();
        fs::write(victim.join("keep"), b"mine").unwrap();
        let folder = cache.root().join("tmp").join("0123456789abcdef.stage");
        std::os::unix::fs::symlink(&victim, &folder).unwrap();
        assert_eq!(
            cache.clear_staging(
                &folder,
                None,
                &crate::stop::Stop::default(),
                &mut Vec::new()
            ),
            0
        );
        assert_eq!(fs::read(victim.join("keep")).unwrap(), b"mine");
    }

    #[cfg(unix)]
    #[test]
    fn member_cleanup_never_follows_a_swapped_staging_folder() {
        use std::io::Write as _;
        let (dir, cache) = cache();
        let staging = cache.staging().unwrap();
        let mut object = staging.new_object().unwrap();
        object.write_all(b"partial").unwrap();
        let member = member_path(&staging);
        let relative = member.strip_prefix(&staging.folder).unwrap();
        let outside = dir.path().join("outside");
        let victim = outside.join(relative);
        fs::create_dir_all(victim.parent().unwrap()).unwrap();
        fs::write(&victim, b"foreign").unwrap();
        fs::rename(&staging.folder, cache.root().join("tmp/saved.stage")).unwrap();
        std::os::unix::fs::symlink(&outside, &staging.folder).unwrap();
        drop(object);
        assert_eq!(fs::read(&victim).unwrap(), b"foreign");
    }

    #[test]
    fn a_staging_sweep_dry_run_records_but_does_not_remove_files() {
        let (_dir, cache) = cache();
        let folder = cache.root().join("tmp/0123456789abcdef.stage");
        fs::create_dir_all(folder.join("03")).unwrap();
        fs::write(folder.join("lock"), b"").unwrap();
        let member = folder.join("03/member.new");
        fs::write(&member, b"keep").unwrap();
        let pretending = cache.pretending();
        let mut failures = Vec::new();
        let removed = pretending.sweep_staging(
            std::time::SystemTime::UNIX_EPOCH,
            &crate::stop::Stop::default(),
            &mut failures,
        );
        assert_eq!(removed, 1);
        assert_eq!(fs::read(&member).unwrap(), b"keep");
        assert!(folder.join("lock").exists());
        assert!(pretending.pretended().contains(&member));
        assert!(pretending.pretended().contains(&folder));
        assert_eq!(failures.len(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn publication_rejects_a_staging_parent_swapped_before_it_starts() {
        use std::io::Write as _;
        let (dir, cache) = cache();
        let staging = cache.staging().unwrap();
        let mut object = staging.new_object().unwrap();
        object.write_all(b"member").unwrap();
        let member = member_path(&staging);
        let relative = member.strip_prefix(&staging.folder).unwrap();
        let sealed = object.seal(true).unwrap();
        let outside = dir.path().join("outside");
        let victim = outside.join(relative);
        fs::create_dir_all(victim.parent().unwrap()).unwrap();
        fs::write(&victim, b"foreign").unwrap();
        fs::rename(&staging.folder, cache.root().join("tmp/saved.stage")).unwrap();
        std::os::unix::fs::symlink(&outside, &staging.folder).unwrap();
        assert!(sealed.publish(&cache).is_err());
        assert_eq!(fs::read(&victim).unwrap(), b"foreign");
        assert!(cache.objects().is_empty());
    }

    #[test]
    fn staging_members_stay_in_sixteen_shards_until_cleanup() {
        let (_dir, cache) = cache();
        let staging = cache.staging().unwrap();
        let mut objects = Vec::new();
        for _ in 0..32 {
            let mut object = staging.new_object().unwrap();
            object.write_all(b"member").unwrap();
            objects.push(object);
        }
        let entries: Vec<_> = fs::read_dir(&staging.folder)
            .unwrap()
            .map(Result::unwrap)
            .collect();
        let folders: Vec<_> = entries
            .iter()
            .filter(|entry| entry.file_type().unwrap().is_dir())
            .collect();
        assert_eq!(entries.len(), folders.len() + 1);
        assert!(!folders.is_empty());
        for folder in &folders {
            let name = folder.file_name().to_string_lossy().into_owned();
            assert_eq!(name.len(), 2);
            assert!(usize::from_str_radix(&name, 16).unwrap() < 16);
        }
        let files: usize = folders
            .iter()
            .map(|folder| fs::read_dir(folder.path()).unwrap().count())
            .sum();
        assert_eq!(files, 32);
        drop(objects);
        drop(staging);
        assert_eq!(fs::read_dir(cache.root().join("tmp")).unwrap().count(), 0);
    }

    #[test]
    fn staging_sweep_clears_abandoned_shards() {
        let (_dir, cache) = cache();
        let folder = cache.root().join("tmp").join("0123456789abcdef.stage");
        fs::create_dir_all(folder.join("03")).unwrap();
        fs::write(folder.join("lock"), b"").unwrap();
        fs::write(folder.join("03").join("member.new"), b"partial").unwrap();
        assert_eq!(
            cache.sweep_staging(SystemTime::UNIX_EPOCH, &Stop::default(), &mut Vec::new()),
            1
        );
        assert!(!folder.exists());
    }

    #[test]
    fn staging_shards_keep_the_live_lock() {
        let (_dir, cache) = cache();
        let staging = cache.staging().unwrap();
        let object = staging.new_object().unwrap();
        if staging.lock.is_none() {
            return crate::test_skips::skip("file-locks", "this disk takes no file locks");
        }
        let mut paths = vec![staging.folder.clone()];
        let mut at = 0;
        while at < paths.len() {
            if paths[at].is_dir() {
                paths.extend(
                    fs::read_dir(&paths[at])
                        .unwrap()
                        .map(|entry| entry.unwrap().path()),
                );
            }
            at += 1;
        }
        let latest = paths
            .iter()
            .map(|path| fs::metadata(path).unwrap().modified().unwrap())
            .max()
            .unwrap();
        let later = latest + GRACE + std::time::Duration::from_secs(1);
        assert_eq!(
            cache.sweep_staging(later, &Stop::default(), &mut Vec::new()),
            0
        );
        assert!(staging.folder.is_dir());
        drop(object);
    }

    #[cfg(unix)]
    #[test]
    fn staging_shards_include_young_members_in_the_age_check() {
        let (_dir, cache) = cache();
        let folder = cache.root().join("tmp").join("0123456789abcdef.stage");
        let shard = folder.join("03");
        fs::create_dir_all(&shard).unwrap();
        let member = shard.join("member.new");
        fs::write(&member, b"partial").unwrap();
        let now = SystemTime::UNIX_EPOCH + GRACE + GRACE;
        for path in [&member, &shard, &folder] {
            File::open(path)
                .unwrap()
                .set_modified(SystemTime::UNIX_EPOCH)
                .unwrap();
        }
        assert!(old_staging(&folder, now, &Stop::default()));
        File::open(&member).unwrap().set_modified(now).unwrap();
        assert!(!old_staging(&folder, now, &Stop::default()));
    }

    /// a build still running keeps its folder where an exclusive lock needs a handle open to write,
    /// as on nfs, whose flock locks byte ranges: a sweep must not take that refusal for a disk
    /// without locks and clear the folder once it is old.
    #[cfg(unix)]
    #[test]
    fn a_running_build_keeps_its_folder_where_locks_need_a_writer() {
        let (_dir, cache) = cache();
        let (folder, lock) = cache.build_folder().unwrap();
        if lock.is_none() {
            return crate::test_skips::skip("file-locks", "this disk takes no file locks");
        }
        fs::write(folder.join("kept"), b"the step's work").unwrap();
        let later = SystemTime::now() + GRACE + GRACE;

        crate::volume::LOCKS_NEED_A_WRITER.set(true);
        cache.sweep_staging(later, &Stop::default(), &mut Vec::new());
        crate::volume::LOCKS_NEED_A_WRITER.set(false);

        assert_eq!(fs::read(folder.join("kept")).unwrap(), b"the step's work");
        drop(lock);
    }

    /// a build folder a killed build left on a disk that keeps no folder modes, which shows the
    /// 0700 it was made with as 0755, is swept as any other once no build holds its lock.
    #[cfg(unix)]
    #[test]
    fn a_folder_left_on_a_disk_that_keeps_no_folder_modes_is_swept() {
        use std::os::unix::fs::PermissionsExt;
        let (_dir, cache) = cache();
        let folder = cache.root().join("tmp").join("0123456789abcdef.build");
        fs::create_dir_all(folder.join("out")).unwrap();
        fs::write(folder.join("lock"), b"").unwrap();
        fs::write(folder.join("out").join("x.csv"), b"x").unwrap();
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o755)).unwrap();

        cache.sweep_staging(SystemTime::now(), &Stop::default(), &mut Vec::new());

        assert!(!folder.exists());
    }

    /// a folder of this user's that others can write, as `builds/<key>` in a group's cache, put in
    /// tmp/ under a build's name by a co-user, is no build's folder: a sweep leaves all it holds.
    #[cfg(unix)]
    #[test]
    fn a_sweep_leaves_a_build_folder_others_can_write() {
        use std::os::unix::fs::PermissionsExt;
        let (_dir, cache) = cache();
        let folder = cache.root().join("tmp").join("x.build");
        fs::create_dir_all(folder.join("kept")).unwrap();
        fs::write(folder.join("lock"), b"").unwrap();
        fs::write(folder.join("kept").join("a.csv"), b"mine").unwrap();
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o775)).unwrap();

        cache.sweep_staging(SystemTime::now(), &Stop::default(), &mut Vec::new());

        assert_eq!(
            fs::read(folder.join("kept").join("a.csv")).unwrap(),
            b"mine"
        );
    }

    /// a folder in a build's folder that its step took its owner's access from gets it back
    /// through a handle on that very folder, so a link put at its name once a sweep looked at it
    /// leaves what the link leads to as it was.
    #[cfg(unix)]
    #[test]
    fn access_given_back_never_reaches_a_link_swapped_in() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, cache) = cache();
        let target = dir.path().join("target");
        fs::create_dir(&target).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o750)).unwrap();
        let (folder, lock) = cache.build_folder().unwrap();
        let shut = folder.join("shut");
        fs::create_dir(&shut).unwrap();
        fs::set_permissions(&shut, fs::Permissions::from_mode(0o077)).unwrap();
        drop(lock);
        let (swapped, aside) = (shut.clone(), folder.join("aside"));
        let link = target.clone();
        LOOKED.set(Some(Box::new(move || {
            // macos renames no folder its owner may not write, even within one folder
            fs::set_permissions(&swapped, fs::Permissions::from_mode(0o700)).unwrap();
            fs::rename(&swapped, &aside).unwrap();
            std::os::unix::fs::symlink(&link, &swapped).unwrap();
        })));

        cache.sweep_staging(SystemTime::now(), &Stop::default(), &mut Vec::new());

        let mode = fs::metadata(&target).unwrap().permissions().mode();
        assert_eq!(mode & 0o7777, 0o750);
        assert!(
            LOOKED.take().is_none(),
            "the sweep looked at the shut folder"
        );
    }

    #[cfg(unix)]
    #[test]
    fn staging_shard_cleanup_never_enters_a_link() {
        let (dir, cache) = cache();
        let victim = dir.path().join("victim");
        fs::create_dir(&victim).unwrap();
        fs::write(victim.join("keep"), b"mine").unwrap();
        let shard = cache.root().join("tmp").join("linked");
        std::os::unix::fs::symlink(&victim, &shard).unwrap();
        assert_eq!(
            cache.clear_shard(&shard, &Stop::default(), &mut Vec::new()),
            (0, false)
        );
        assert_eq!(fs::read(victim.join("keep")).unwrap(), b"mine");
    }

    #[cfg(unix)]
    #[test]
    fn a_swapped_staging_parent_cannot_redirect_shards_or_members() {
        for prepared in [false, true] {
            let (dir, cache) = cache();
            let staging = cache.staging().unwrap();
            if prepared {
                for shard in 0..SHARDS {
                    fs::create_dir(staging.folder.join(format!("{shard:02x}"))).unwrap();
                    staging.shards[shard].store(true, Ordering::Relaxed);
                }
            }
            let outside = dir.path().join("outside");
            fs::create_dir(&outside).unwrap();
            fs::rename(&staging.folder, cache.root().join("tmp/saved.stage")).unwrap();
            std::os::unix::fs::symlink(&outside, &staging.folder).unwrap();
            assert!(staging.new_object().is_err());
            assert_eq!(fs::read_dir(outside).unwrap().count(), 0);
        }
    }
}
