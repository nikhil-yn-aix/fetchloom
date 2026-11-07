//! per-archive staging folders. a held lock distinguishes live writers from killed runs; a
//! filesystem without locks keeps the age rule used for other temporary files.

use std::fs::{self, File, TryLockError};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;

use super::partial::open_unfollowed;
use super::upkeep::GRACE;
use super::{Cache, NewObject};
use crate::place::{FolderUse, real_folder};
use crate::stop::Stop;

pub(crate) struct Staging {
    cache: Cache,
    folder: PathBuf,
    lock: Option<File>,
    /// which of the cache's `stage_shards` subfolders this staging has made.
    shards: Box<[AtomicBool]>,
}

impl Cache {
    /// starts an archive's staging folder after reclaiming abandoned ones. its lock gets its name
    /// only once held, so a sweep never takes the lock of a writer that is still opening it.
    pub(crate) fn staging(&self) -> io::Result<Arc<Staging>> {
        self.sweep_staging(SystemTime::now(), &Stop::default(), &mut Vec::new());
        let mut staging = loop {
            let folder = self
                .root
                .join("tmp")
                .join(format!("{:016x}.stage", fastrand::u64(..)));
            match fs::create_dir(&folder) {
                Ok(()) => {
                    break Staging {
                        cache: self.clone(),
                        folder,
                        lock: None,
                        shards: (0..self.stage_shards)
                            .map(|_| AtomicBool::new(false))
                            .collect(),
                    };
                }
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
                Err(err) => return Err(err),
            }
        };
        let opening = staging.folder.join("lock.new");
        let lock = open_unfollowed(&opening, true)?;
        match lock.try_lock() {
            Ok(()) => {
                fs::rename(&opening, staging.folder.join("lock"))?;
                staging.lock = Some(lock);
            }
            Err(TryLockError::WouldBlock) => return Err(io::ErrorKind::WouldBlock.into()),
            Err(TryLockError::Error(_)) => {
                drop(lock);
                fs::rename(&opening, staging.folder.join("unlocked"))?;
            }
        }
        Ok(Arc::new(staging))
    }

    /// reclaims unlocked staging at once. a folder without a lock, one a writer is still opening
    /// or one on a filesystem without locks, waits for the age rule. a held lock wins over age.
    pub(super) fn sweep_staging(
        &self,
        now: SystemTime,
        stop: &Stop,
        not_removed: &mut Vec<PathBuf>,
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

    fn clear_staging(
        &self,
        folder: &Path,
        lock: Option<File>,
        stop: &Stop,
        not_removed: &mut Vec<PathBuf>,
    ) -> u64 {
        // someone able to rename inside tmp could swap a link in for the folder, or for tmp
        // itself, while it was used
        if real_folder(folder, FolderUse::Cache).is_err() || self.check_inside(folder).is_err() {
            not_removed.push(folder.to_path_buf());
            return 0;
        }
        let Ok(entries) = fs::read_dir(folder) else {
            not_removed.push(folder.to_path_buf());
            return 0;
        };
        let mut removed = 0;
        let mut cleared = true;
        for entry in entries {
            if stop.is_asked() {
                cleared = false;
                break;
            }
            let Ok(entry) = entry else {
                not_removed.push(folder.to_path_buf());
                cleared = false;
                continue;
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
            match self.remove_inside(&entry.path()) {
                Ok(()) => removed += 1,
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(_) => {
                    not_removed.push(entry.path());
                    cleared = false;
                }
            }
        }
        // no writer reuses a staging folder. closing its lock before deleting it lets windows
        // remove the folder too, without an open child waiting for deletion.
        drop(lock);
        if cleared {
            for name in ["lock", "unlocked"] {
                let path = folder.join(name);
                super::upkeep::removed(&path, self.remove_inside(&path), not_removed);
            }
            let folder_removed = self
                .check_inside(folder)
                .and_then(|()| self.volume.remove_folder(folder));
            super::upkeep::removed(folder, folder_removed, not_removed);
        }
        removed
    }

    /// removes the files in one subfolder of a staging folder, then the subfolder. says how many
    /// files went and whether the subfolder is gone.
    fn clear_shard(
        &self,
        shard: &Path,
        stop: &Stop,
        not_removed: &mut Vec<PathBuf>,
    ) -> (u64, bool) {
        let Ok(entries) = fs::read_dir(shard) else {
            not_removed.push(shard.to_path_buf());
            return (0, false);
        };
        let mut removed = 0;
        for entry in entries {
            if stop.is_asked() {
                return (removed, false);
            }
            let Ok(entry) = entry else {
                not_removed.push(shard.to_path_buf());
                return (removed, false);
            };
            match self.remove_inside(&entry.path()) {
                Ok(()) => removed += 1,
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(_) => {
                    not_removed.push(entry.path());
                    return (removed, false);
                }
            }
        }
        match self
            .check_inside(shard)
            .and_then(|()| self.volume.remove_folder(shard))
        {
            Ok(()) => (removed, true),
            Err(err) if err.kind() == io::ErrorKind::NotFound => (removed, true),
            Err(_) => {
                not_removed.push(shard.to_path_buf());
                (removed, false)
            }
        }
    }
}

/// a large member can keep growing without changing its folder's timestamp.
fn old_staging(folder: &Path, now: SystemTime, stop: &Stop) -> bool {
    fs::metadata(folder)
        .and_then(|metadata| metadata.modified())
        .is_ok_and(|modified| is_old(now, modified))
        && all_old(folder, true, now, stop)
}

/// whether every entry in `folder` is past the grace period, and with `deeper`, every entry in
/// its subfolders too, where staging spread over subfolders keeps its members.
fn all_old(folder: &Path, deeper: bool, now: SystemTime, stop: &Stop) -> bool {
    fs::read_dir(folder).is_ok_and(|mut entries| {
        entries.all(|entry| {
            !stop.is_asked()
                && entry.is_ok_and(|entry| {
                    let inside = deeper
                        && entry
                            .file_type()
                            .is_ok_and(|kind| kind.is_dir() && !kind.is_symlink());
                    entry
                        .metadata()
                        .and_then(|metadata| metadata.modified())
                        .is_ok_and(|modified| is_old(now, modified))
                        && (!inside || all_old(&entry.path(), false, now, stop))
                })
        })
    })
}

fn is_old(now: SystemTime, modified: SystemTime) -> bool {
    now.duration_since(modified).is_ok_and(|age| age > GRACE)
}

impl Staging {
    pub(crate) fn new_object(self: &Arc<Self>) -> io::Result<NewObject> {
        if self.shards.is_empty() {
            return self
                .cache
                .new_object_in(&self.folder, Some(Arc::clone(self)));
        }
        let shard = fastrand::usize(..self.shards.len());
        let folder = self.folder.join(format!("{shard:02x}"));
        if !self.shards[shard].load(Ordering::Relaxed) {
            real_folder(&folder, FolderUse::Cache)?;
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
    use std::time::Duration;

    use super::super::tests::cache;
    use super::*;

    #[test]
    fn spread_staging_makes_objects_in_subfolders_and_clears_them() {
        let (_dir, mut cache) = cache();
        cache.stage_shards = 4;
        let staging = cache.staging().unwrap();
        let mut objects: Vec<NewObject> = (0..16).map(|_| staging.new_object().unwrap()).collect();
        for object in &mut objects {
            object.write_all(b"member").unwrap();
        }
        let shards: Vec<_> = fs::read_dir(&staging.folder)
            .unwrap()
            .map(|entry| entry.unwrap())
            .filter(|entry| entry.file_type().unwrap().is_dir())
            .collect();
        assert!(!shards.is_empty() && shards.len() <= 4);
        let staged: usize = shards
            .iter()
            .map(|shard| fs::read_dir(shard.path()).unwrap().count())
            .sum();
        assert_eq!(staged, 16);
        drop(objects);
        drop(staging);
        assert_eq!(fs::read_dir(cache.root().join("tmp")).unwrap().count(), 0);
    }

    #[test]
    fn a_sweep_clears_spread_staging_a_killed_run_left() {
        let (_dir, cache) = cache();
        let folder = cache.root().join("tmp").join("0123456789abcdef.stage");
        fs::create_dir_all(folder.join("03")).unwrap();
        fs::write(folder.join("lock"), b"").unwrap();
        fs::write(folder.join("03").join("0123456789abcdef.new"), b"part").unwrap();

        let removed = cache.sweep_staging(SystemTime::now(), &Stop::default(), &mut Vec::new());

        assert_eq!(removed, 1);
        assert_eq!(fs::read_dir(cache.root().join("tmp")).unwrap().count(), 0);
    }

    #[test]
    fn a_young_member_in_a_subfolder_keeps_staging_without_a_lock() {
        let (_dir, cache) = cache();
        let folder = cache.root().join("tmp").join("0123456789abcdef.stage");
        fs::create_dir_all(folder.join("03")).unwrap();
        let member = folder.join("03").join("0123456789abcdef.new");
        fs::write(&member, b"part").unwrap();
        let later = SystemTime::now() + GRACE + Duration::from_secs(60);
        fs::File::options()
            .write(true)
            .open(&member)
            .unwrap()
            .set_modified(later + GRACE)
            .unwrap();

        assert!(all_old(&folder, false, later, &Stop::default()));
        assert!(!all_old(&folder, true, later, &Stop::default()));
        assert!(!old_staging(&folder, later, &Stop::default()));
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
}
