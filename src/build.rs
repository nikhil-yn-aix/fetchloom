//! running a recipe's step in a folder of its own in the cache, reading `in/<name>` and writing
//! `out`, and taking in its result. only `fetchloom build` runs one: a step is the project's code.

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

/// the step's folder, `tmp/<16 hex>.build` in the cache, open to its owner alone. its lock keeps a
/// sweep away while the build runs, and dropping it clears the folder.
pub(crate) struct Staging {
    folder: PathBuf,
    lock: Option<File>,
    #[cfg(unix)]
    held: Held,
}

/// the cache's tmp/ and the step's folder in it, opened once from the cache's root without
/// following a link and checked to be this user's alone. taking in the result and clearing go
/// through them, so a folder someone who shares tmp/ puts in its place is never read or cleared.
#[cfg(unix)]
struct Held {
    tmp: File,
    folder: Arc<File>,
    name: std::ffi::OsString,
}

impl Staging {
    /// a new folder for a step in `cache`, with `in` and `out` made and empty.
    pub(crate) fn new(cache: &Cache) -> io::Result<Staging> {
        let (folder, lock) = cache.build_folder()?;
        #[cfg(unix)]
        {
            let held = hold(cache.root(), &folder, lock.as_ref())?;
            for name in ["in", "out"] {
                rustix::fs::mkdirat(&*held.folder, name, rustix::fs::Mode::from_raw_mode(0o700))?;
            }
            Ok(Staging { folder, lock, held })
        }
        #[cfg(not(unix))]
        {
            let staging = Staging { folder, lock };
            std::fs::create_dir(staging.folder.join("in"))?;
            std::fs::create_dir(staging.out())?;
            Ok(staging)
        }
    }

    /// the step's `out` folder to take its result from: on unix opened through the folder held
    /// since the step began.
    pub(crate) fn out_folder(&self) -> io::Result<capture::Out> {
        #[cfg(unix)]
        {
            let out = crate::cache::enter_at(&self.held.folder, "out")?;
            Ok(capture::Out::held(self.out(), out))
        }
        #[cfg(not(unix))]
        capture::Out::open(&self.out())
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

    /// whether the disk holding the cache clones files, as a probe in this folder finds: a step's
    /// copies of what it reads then take no room of their own until it changes them.
    pub(crate) fn clones(&self) -> bool {
        let probe = self.folder.join("probe");
        std::fs::write(&probe, b"probe").is_ok()
            && crate::place::Placer::without_hardlinks(&self.folder)
                .copy_file(&probe, &self.folder.join("clone"), false)
                .is_ok_and(|method| method == crate::place::Method::Reflink)
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
        #[cfg(unix)]
        let cleared = self.held.clear();
        #[cfg(not(unix))]
        let cleared = crate::cache::clear_tree(&self.folder);
        if let Err(err) = cleared {
            tracing::debug!(%err, folder = %self.folder.display(), "could not clear a build's folder, which the next build sweeps");
        }
    }
}

/// opens `folder`, a step's folder in the cache at `root`, from `root` without following a link,
/// and refuses it unless it is this user's, open to its owner alone, as made, and the one this run
/// made: it holds `lock`, the lock file this run holds, where the disk takes locks.
#[cfg(unix)]
fn hold(root: &Path, folder: &Path, lock: Option<&File>) -> io::Result<Held> {
    use rustix::fs::{AtFlags, Mode, OFlags};
    let name = folder
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "a build folder has no name"))?
        .to_owned();
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    // a cache folder someone named may itself be a link
    let root = File::open(root)?;
    let tmp = File::from(rustix::fs::openat(&root, "tmp", flags, Mode::empty())?);
    let opened = File::from(rustix::fs::openat(&tmp, &name, flags, Mode::empty())?);
    let another = match lock {
        Some(lock) => {
            let named = rustix::fs::statat(&opened, "lock", AtFlags::SYMLINK_NOFOLLOW)?;
            let held = rustix::fs::fstat(lock)?;
            (named.st_dev, named.st_ino) != (held.st_dev, held.st_ino)
        }
        None => false,
    };
    if !crate::cache::is_private(&rustix::fs::fstat(&opened)?) || another {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} is not this user's alone", folder.display()),
        ));
    }
    Ok(Held {
        tmp,
        folder: Arc::new(opened),
        name,
    })
}

#[cfg(unix)]
impl Held {
    /// clears the step's folder through its handle, then takes its name from tmp/ only while the
    /// name is still this folder's, not one put in its place.
    fn clear(&self) -> io::Result<()> {
        crate::cache::clear_folder(&self.tmp, &self.name, &self.folder)
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
        let staging = Staging::new(&cache).unwrap();
        let folder = staging.folder().to_path_buf();
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
        let staging = Staging::new(&cache).unwrap();
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

    /// a folder put in place of the new one before it is held, even one of this user's own, is
    /// refused: it lacks the lock this run made.
    #[cfg(unix)]
    #[test]
    fn a_folder_put_in_place_before_it_is_held_is_refused() {
        let (dir, cache) = crate::cache::tests::cache();
        let (folder, lock) = cache.build_folder().unwrap();
        std::fs::rename(&folder, dir.path().join("away")).unwrap();
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(folder.join("lock"), b"").unwrap();

        let refused = hold(cache.root(), &folder, lock.as_ref()).err().unwrap();

        assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied);
    }

    /// the step's own folder, its lock in place, is refused once others may open it, as a folder
    /// a co-user could have swapped in through would be.
    #[cfg(unix)]
    #[test]
    fn a_folder_others_can_open_is_refused_though_it_holds_the_lock() {
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
        let (_dir, cache) = crate::cache::tests::cache();
        let running = Staging::new(&cache).unwrap();
        let (left, lock) = cache.build_folder().unwrap();
        std::fs::create_dir(left.join("out")).unwrap();
        std::fs::write(left.join("out").join("x.csv"), b"x").unwrap();
        drop(lock);
        let next = Staging::new(&cache).unwrap();
        assert!(!left.exists(), "a free folder is swept");
        assert!(running.folder().exists(), "a held one stays");
        drop((running, next));
    }
}
