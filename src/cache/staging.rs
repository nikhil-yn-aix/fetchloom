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
use super::{Cache, Failure, NewObject};
use crate::place::Placer;
use crate::stop::Stop;

pub(crate) struct Staging {
    cache: Cache,
    folder: PathBuf,
    lock: Option<File>,
    namespace: Placer,
    shards: Box<[AtomicBool]>,
}

impl Cache {
    /// starts an archive's staging folder after reclaiming abandoned ones. its lock gets its name
    /// only once held, so a sweep never takes the lock of a writer that is still opening it.
    pub(crate) fn staging(&self) -> io::Result<Arc<Staging>> {
        self.sweep_staging(SystemTime::now(), &Stop::default(), &mut Vec::new());
        let namespace = Placer::in_named_folder(&self.root);
        let shards = self
            .experiments()
            .number("ARCH_STAGE_SHARDS")
            .unwrap_or(0)
            .min(256);
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
                        shards: (0..shards).map(|_| AtomicBool::new(false)).collect(),
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
        // remove the folder too, without an open child waiting for deletion.
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
        if self.shards.is_empty() {
            return self
                .cache
                .new_object_in(&self.folder, Some(Arc::clone(self)));
        }
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
    use crate::exp::Experiments;

    use super::super::tests::cache;

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
        let name = fs::read_dir(&staging.folder)
            .unwrap()
            .map(Result::unwrap)
            .find(|entry| entry.path().extension().is_some_and(|ext| ext == "new"))
            .unwrap()
            .file_name();
        let outside = dir.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join(&name), b"foreign").unwrap();
        fs::rename(&staging.folder, cache.root().join("tmp/saved.stage")).unwrap();
        std::os::unix::fs::symlink(&outside, &staging.folder).unwrap();
        drop(object);
        assert_eq!(fs::read(outside.join(&name)).unwrap(), b"foreign");
    }

    #[cfg(unix)]
    #[test]
    fn a_swapped_staging_folder_cannot_receive_new_members() {
        let (dir, cache) = cache();
        let staging = cache.staging().unwrap();
        let outside = dir.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::rename(&staging.folder, cache.root().join("tmp/saved.stage")).unwrap();
        std::os::unix::fs::symlink(&outside, &staging.folder).unwrap();
        assert!(staging.new_object().is_err());
        assert_eq!(fs::read_dir(outside).unwrap().count(), 0);
    }
    #[test]
    fn a_staging_sweep_dry_run_records_but_does_not_remove_files() {
        let (_dir, cache) = cache();
        let folder = cache.root().join("tmp/0123456789abcdef.stage");
        fs::create_dir(&folder).unwrap();
        fs::write(folder.join("lock"), b"").unwrap();
        let member = folder.join("member.new");
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
        let name = fs::read_dir(&staging.folder)
            .unwrap()
            .map(Result::unwrap)
            .find(|entry| entry.path().extension().is_some_and(|ext| ext == "new"))
            .unwrap()
            .file_name();
        let sealed = object.seal(true).unwrap();
        let outside = dir.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join(&name), b"foreign").unwrap();
        fs::rename(&staging.folder, cache.root().join("tmp/saved.stage")).unwrap();
        std::os::unix::fs::symlink(&outside, &staging.folder).unwrap();
        assert!(sealed.publish(&cache).is_err());
        assert_eq!(fs::read(outside.join(&name)).unwrap(), b"foreign");
        assert!(cache.objects().is_empty());
    }

    #[test]
    fn staging_shards_are_opt_in_bounded_and_cleaned() {
        for (value, count) in [
            (None, 0),
            (Some("0"), 0),
            (Some("4"), 4),
            (Some("18446744073709551615"), 256),
        ] {
            let (_dir, cache) = cache();
            let exp = value.map_or_else(Experiments::default, |value| {
                Experiments::default().with("ARCH_STAGE_SHARDS", value)
            });
            let cache = cache.with_experiments(exp);
            let staging = cache.staging().unwrap();
            assert_eq!(staging.shards.len(), count);
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
            if count == 0 {
                assert!(folders.is_empty());
                assert_eq!(entries.len(), 33);
            } else {
                assert!(!folders.is_empty() && folders.len() <= count);
                let files: usize = folders
                    .iter()
                    .map(|folder| fs::read_dir(folder.path()).unwrap().count())
                    .sum();
                assert_eq!(files, 32);
            }
            drop(objects);
            drop(staging);
            assert_eq!(fs::read_dir(cache.root().join("tmp")).unwrap().count(), 0);
        }
    }

    #[test]
    fn staging_sweep_clears_abandoned_shards_with_the_switch_off() {
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
        for value in ["0", "4"] {
            let (_dir, cache) = cache();
            let cache =
                cache.with_experiments(Experiments::default().with("ARCH_STAGE_SHARDS", value));
            let staging = cache.staging().unwrap();
            let object = staging.new_object().unwrap();
            if staging.lock.is_none() {
                continue;
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
            let cache =
                cache.with_experiments(Experiments::default().with("ARCH_STAGE_SHARDS", "1"));
            let staging = cache.staging().unwrap();
            if prepared {
                drop(staging.new_object().unwrap());
            }
            let outside = dir.path().join("outside");
            fs::create_dir(&outside).unwrap();
            fs::rename(&staging.folder, cache.root().join("tmp/saved.stage")).unwrap();
            std::os::unix::fs::symlink(&outside, &staging.folder).unwrap();
            assert!(staging.new_object().is_err());
            assert_eq!(fs::read_dir(outside).unwrap().count(), 0);
        }
    }

    #[test]
    fn a_sharded_dry_run_keeps_members_and_reports_their_names() {
        let (_dir, cache) = cache();
        let folder = cache.root().join("tmp/0123456789abcdef.stage");
        fs::create_dir_all(folder.join("03")).unwrap();
        fs::write(folder.join("lock"), b"").unwrap();
        let member = folder.join("03/member.new");
        fs::write(&member, b"keep").unwrap();
        let pretending = cache.pretending();
        assert_eq!(
            pretending.sweep_staging(SystemTime::UNIX_EPOCH, &Stop::default(), &mut Vec::new()),
            1
        );
        assert_eq!(fs::read(&member).unwrap(), b"keep");
        assert!(pretending.pretended().contains(&member));
    }
}
