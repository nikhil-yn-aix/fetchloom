//! running a recipe's step and taking in what it wrote. a step runs in a folder of its own in the
//! cache: it finds a private copy of each dataset it reads in `in/<name>` and writes its result
//! into `out`. only `fetchloom build` runs one, since a step is the project's code.

pub(crate) mod capture;
pub(crate) mod compare;
pub(crate) mod inputs;
pub(crate) mod step;

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

use crate::cache::Cache;

/// the folder one step runs in, `tmp/<16 hex>.build` in the cache, which only its owner may
/// enter. the lock it holds tells a sweep that the step's build still runs, and it is cleared
/// when dropped, so a step fetchloom saw end leaves nothing behind.
pub(crate) struct Staging {
    folder: PathBuf,
    lock: Option<File>,
}

impl Staging {
    /// a new folder for a step in `cache`, with `in` and `out` made and empty.
    ///
    /// # Errors
    ///
    /// the cache's `tmp` folder cannot be written.
    pub(crate) fn new(cache: &Cache) -> io::Result<Staging> {
        let (folder, lock) = cache.build_folder()?;
        let staging = Staging { folder, lock };
        std::fs::create_dir(staging.folder.join("in"))?;
        std::fs::create_dir(staging.out())?;
        Ok(staging)
    }

    /// the folder the step finds its private copy of the dataset `name` in.
    pub(crate) fn input(&self, name: &str) -> PathBuf {
        self.folder.join("in").join(name)
    }

    /// the folder the step writes its result into.
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
        if let Err(err) = crate::cache::clear_tree(&self.folder) {
            tracing::debug!(%err, folder = %self.folder.display(), "could not clear a build's folder, which the next build sweeps");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// a step's folder is gone once it is dropped, whatever the step left in it: read-only
    /// files, a folder it took its owner's access from, and a link, which is never followed.
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

    /// a folder a build left behind is swept once no build holds its lock, and kept while one
    /// does.
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
