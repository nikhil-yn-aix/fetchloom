//! per-archive staging folders. a held lock distinguishes live writers from killed runs; a
//! filesystem without locks keeps the age rule used for other temporary files.

use std::fs::{self, File, TryLockError};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use super::partial::open_unfollowed;
use super::upkeep::GRACE;
use super::{Cache, NewObject};
use crate::place::{FolderUse, real_folder};

pub(crate) struct Staging {
    cache: Cache,
    folder: PathBuf,
    lock: Option<File>,
}

impl Cache {
    /// starts an archive's staging folder after reclaiming abandoned ones. its lock gets its name
    /// only once held, so a sweep never takes the lock of a writer that is still opening it.
    pub(crate) fn staging(&self) -> io::Result<Arc<Staging>> {
        self.sweep_staging(SystemTime::now());
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
    pub(super) fn sweep_staging(&self, now: SystemTime) -> u64 {
        let mut removed = 0;
        for entry in fs::read_dir(self.root.join("tmp"))
            .into_iter()
            .flatten()
            .flatten()
        {
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
            if lock.is_some() || old_staging(&folder, now) {
                removed += self.clear_staging(&folder, lock);
            }
        }
        removed
    }

    fn clear_staging(&self, folder: &Path, lock: Option<File>) -> u64 {
        // someone able to rename inside tmp could swap a link in for the folder while it was used
        if real_folder(folder, FolderUse::Cache).is_err() {
            return 0;
        }
        let Ok(entries) = fs::read_dir(folder) else {
            return 0;
        };
        let mut removed = 0;
        let mut cleared = true;
        for entry in entries {
            let Ok(entry) = entry else {
                cleared = false;
                continue;
            };
            if entry.file_name() == "lock" || entry.file_name() == "unlocked" {
                continue;
            }
            match self.volume.remove(&entry.path()) {
                Ok(()) => removed += 1,
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(_) => cleared = false,
            }
        }
        // no writer reuses a staging folder. closing its lock before deleting it lets windows
        // remove the folder too, without an open child waiting for deletion.
        drop(lock);
        if cleared {
            let _ = self.volume.remove(&folder.join("lock"));
            let _ = self.volume.remove(&folder.join("unlocked"));
            let _ = fs::remove_dir(folder);
        }
        removed
    }
}

/// a large member can keep growing without changing its folder's timestamp.
fn old_staging(folder: &Path, now: SystemTime) -> bool {
    let old = |modified| now.duration_since(modified).is_ok_and(|age| age > GRACE);
    fs::metadata(folder)
        .and_then(|metadata| metadata.modified())
        .is_ok_and(old)
        && fs::read_dir(folder).is_ok_and(|mut entries| {
            entries.all(|entry| {
                entry
                    .and_then(|entry| entry.metadata())
                    .and_then(|metadata| metadata.modified())
                    .is_ok_and(old)
            })
        })
}

impl Staging {
    pub(crate) fn new_object(self: &Arc<Self>) -> io::Result<NewObject> {
        self.cache
            .new_object_in(&self.folder, Some(Arc::clone(self)))
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        self.cache.clear_staging(&self.folder, self.lock.take());
    }
}


#[cfg(all(test, unix))]
mod tests {
    use std::fs;

    use super::super::tests::cache;

    #[test]
    fn clearing_never_follows_a_link_swapped_in_for_the_folder() {
        let (dir, cache) = cache();
        let victim = dir.path().join("victim");
        fs::create_dir(&victim).unwrap();
        fs::write(victim.join("keep"), b"mine").unwrap();
        let folder = cache.root().join("tmp").join("0123456789abcdef.stage");
        std::os::unix::fs::symlink(&victim, &folder).unwrap();
        assert_eq!(cache.clear_staging(&folder, None), 0);
        assert_eq!(fs::read(victim.join("keep")).unwrap(), b"mine");
    }
}