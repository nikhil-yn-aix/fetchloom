//! keeping the cache tidy: which projects use it, how big it is, freeing what no project needs,
//! and hashing every object again.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use super::Cache;
use crate::digest::{self, Sha256};

/// how long an object no project needs stays, so a download a project has not locked yet, or a
/// `get` from this morning, survives a gc.
pub(super) const GRACE: Duration = Duration::from_hours(24);

pub(crate) struct Object {
    pub(crate) digest: Sha256,
    pub(crate) size: u64,
    path: PathBuf,
    modified: SystemTime,
}

/// what the projects using a cache need from it.
#[derive(Debug, Default)]
pub(crate) struct Needed {
    /// objects a project places, and archives a project has not unpacked on this machine yet.
    pub(crate) objects: HashSet<Sha256>,
    /// archives whose record tells what a project places from them.
    pub(crate) unpacked: HashSet<Sha256>,
}

#[derive(Debug, Default, Serialize)]
pub(crate) struct Freed {
    pub(crate) objects: u64,
    pub(crate) bytes: u64,
    /// unfinished downloads and temporary files nobody came back for.
    pub(crate) leftovers: u64,
}

#[derive(Debug, Default, Serialize)]
pub(crate) struct Checked {
    pub(crate) objects: u64,
    pub(crate) bytes: u64,
    /// objects whose bytes no longer matched their name.
    pub(crate) changed: Vec<Sha256>,
    /// changed objects whose repair marks could not be written.
    pub(crate) stuck: Vec<Sha256>,
    /// objects that could not be read, left untouched and without a new change mark.
    pub(crate) unreadable: Vec<Sha256>,
}

#[derive(Serialize, Deserialize)]
struct Registration {
    root: PathBuf,
}

impl Cache {
    /// remembers that the project at `root` uses this cache, so gc keeps what it needs.
    pub(crate) fn register(&self, root: &Path) -> io::Result<()> {
        let path = self.registration(root);
        if path.exists() {
            return Ok(());
        }
        let text = serde_json::to_vec(&Registration {
            root: root.to_path_buf(),
        })
        .map_err(io::Error::other)?;
        self.write_atomically(&path, &text)
    }

    /// the projects that registered.
    pub(crate) fn projects(&self) -> Vec<PathBuf> {
        let Ok(entries) = fs::read_dir(self.root.join("projects")) else {
            return Vec::new();
        };
        let mut roots: Vec<PathBuf> = entries
            .flatten()
            .filter_map(|entry| fs::read(entry.path()).ok())
            .filter_map(|bytes| serde_json::from_slice::<Registration>(&bytes).ok())
            .map(|registration| registration.root)
            .collect();
        roots.sort();
        roots
    }

    pub(crate) fn forget(&self, root: &Path) {
        let _ = fs::remove_file(self.registration(root));
    }

    pub(crate) fn objects(&self) -> Vec<Object> {
        let mut objects = Vec::new();
        let Ok(fans) = fs::read_dir(self.root.join("objects")) else {
            return objects;
        };
        for fan in fans.flatten() {
            let prefix = fan.file_name().to_string_lossy().into_owned();
            let Ok(entries) = fs::read_dir(fan.path()) else {
                continue;
            };
            for entry in entries.flatten() {
                let name = format!("{prefix}{}", entry.file_name().to_string_lossy());
                let (Some(digest), Ok(metadata)) = (Sha256::from_hex(&name), entry.metadata())
                else {
                    continue;
                };
                objects.push(Object {
                    digest,
                    size: metadata.len(),
                    path: entry.path(),
                    modified: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                });
            }
        }
        objects
    }

    /// removes every object not in `needed` that is older than a day, the records of archives no
    /// project uses, abandoned archive staging, and other temporary files left that long.
    pub(crate) fn sweep(&self, needed: &Needed, now: SystemTime) -> Freed {
        let old = |modified: SystemTime| now.duration_since(modified).is_ok_and(|age| age > GRACE);
        let mut freed = Freed::default();
        for object in self.objects() {
            if !needed.objects.contains(&object.digest)
                && old(object.modified)
                && self.volume.remove(&object.path).is_ok()
            {
                freed.objects += 1;
                freed.bytes += object.size;
            }
        }
        for archive in self.unpacked_archives() {
            let path = self.unpacked_path(&archive);
            let stale = fs::metadata(&path)
                .and_then(|metadata| metadata.modified())
                .is_ok_and(old);
            if !needed.unpacked.contains(&archive) && stale {
                let _ = fs::remove_file(path);
            }
        }
        freed.leftovers += self.sweep_partials(&old);
        freed.leftovers += self.sweep_staging(now);
        self.forget_changed(&needed.objects);
        for entry in fs::read_dir(self.root.join("tmp"))
            .into_iter()
            .flatten()
            .flatten()
        {
            let stale = entry
                .metadata()
                .ok()
                .and_then(|metadata| metadata.modified().ok())
                .is_some_and(old);
            if stale && self.volume.remove(&entry.path()).is_ok() {
                freed.leftovers += 1;
            }
        }
        freed
    }

    /// removes unfinished downloads that nobody holds and whose bytes have not grown in a day. a
    /// download that runs for days keeps writing, so it is never mistaken for an abandoned one.
    /// the empty lock files stay: a run waiting for the download may have opened one already,
    /// and a lock taken on a file that lost its name would let a later run own the download too.
    fn sweep_partials(&self, old: &dyn Fn(SystemTime) -> bool) -> u64 {
        let folder = self.root.join("partial");
        let mut removed = 0;
        for entry in fs::read_dir(&folder).into_iter().flatten().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(key) = name.strip_suffix(".lock") else {
                continue;
            };
            let Ok(lock) = super::partial::open_unfollowed(&entry.path(), false) else {
                continue;
            };
            if lock.try_lock().is_err() {
                continue;
            }
            let bytes = folder.join(format!("{key}.part"));
            let abandoned =
                fs::metadata(&bytes).map_or(true, |metadata| metadata.modified().is_ok_and(old));
            if !abandoned {
                continue;
            }
            for leftover in [bytes, folder.join(format!("{key}.json"))] {
                if self.volume.remove(&leftover).is_ok() {
                    removed += 1;
                }
            }
        }
        removed
    }

    /// hashes every object again on `threads` threads and marks changed bytes for replacement.
    /// `progress` hears each object's size after its read attempt.
    pub(crate) fn verify_all(&self, threads: usize, progress: &(dyn Fn(u64) + Sync)) -> Checked {
        let objects = self.objects();
        let next = AtomicUsize::new(0);
        let checked = Mutex::new(Checked {
            objects: objects.len() as u64,
            bytes: objects.iter().map(|object| object.size).sum(),
            ..Checked::default()
        });
        std::thread::scope(|scope| {
            for _ in 0..threads.max(1) {
                scope.spawn(|| {
                    while let Some(object) = objects.get(next.fetch_add(1, Ordering::Relaxed)) {
                        let actual = Sha256::of_file(&object.path);
                        progress(object.size);
                        match actual {
                            Ok(actual) if actual == object.digest => continue,
                            Err(_) => {
                                checked
                                    .lock()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                                    .unreadable
                                    .push(object.digest);
                                continue;
                            }
                            Ok(_) => {}
                        }
                        let marked = self.mark_changed(&object.digest).is_ok();
                        let mut found = checked
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        found.changed.push(object.digest);
                        if !marked {
                            found.stuck.push(object.digest);
                        }
                    }
                });
            }
        });
        checked
            .into_inner()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn registration(&self, root: &Path) -> PathBuf {
        let key = digest::sha256(root.as_os_str().as_encoded_bytes()).hex();
        self.root.join("projects").join(format!("{key}.json"))
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::super::tests::{cache, fat, hold, seal, store};
    use super::*;

    #[test]
    fn projects_register_once_and_can_be_forgotten() {
        let (dir, cache) = cache();
        let root = dir.path().join("thesis");
        cache.register(&root).unwrap();
        cache.register(&root).unwrap();
        assert_eq!(cache.projects(), vec![root.clone()]);
        cache.forget(&root);
        assert_eq!(cache.projects(), Vec::<PathBuf>::new());
    }

    #[test]
    fn a_sweep_keeps_what_is_needed_and_what_is_new() {
        for (_dir, cache) in [cache(), fat()] {
            let needed = store(&cache, b"needed");
            let unneeded = store(&cache, b"unneeded");
            let later = SystemTime::now() + GRACE + Duration::from_secs(60);
            let keep = Needed {
                objects: HashSet::from([needed.digest]),
                ..Needed::default()
            };
            assert_eq!(
                cache.sweep(&keep, SystemTime::now()).objects,
                0,
                "nothing is old yet"
            );
            let freed = cache.sweep(&keep, later);
            assert_eq!((freed.objects, freed.bytes), (1, 8));
            assert!(cache.find(&needed.digest, Some(6)).is_some());
            assert!(cache.find(&unneeded.digest, Some(8)).is_none());
        }
    }

    #[test]
    fn a_sweep_removes_what_stopped_runs_left_sealed() {
        for (_dir, cache) in [cache(), fat()] {
            // a download sealed by a run that died while publishing it, and a member sealed by
            // one that died while unpacking
            let partial = cache.root.join("partial");
            fs::write(partial.join("left.lock"), b"").unwrap();
            fs::write(partial.join("left.json"), b"{}").unwrap();
            let sealed = [
                partial.join("left.part"),
                cache.root.join("tmp").join("0123456789abcdef.new"),
            ];
            for path in &sealed {
                fs::write(path, b"abc").unwrap();
                seal(path);
            }
            let later = SystemTime::now() + GRACE + Duration::from_secs(60);
            assert_eq!(cache.sweep(&Needed::default(), later).leftovers, 3);
            assert!(sealed.iter().all(|path| !path.exists()));
            let left: Vec<String> = fs::read_dir(partial)
                .unwrap()
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect();
            assert_eq!(left, ["left.lock"], "a run may hold the lock open");
        }
    }

    #[test]
    fn a_sweep_removes_unlocked_staging_without_waiting() {
        for (_dir, cache) in [cache(), fat()] {
            let folder = cache.root().join("tmp").join("0123456789abcdef.stage");
            fs::create_dir(&folder).unwrap();
            fs::write(folder.join("lock"), b"").unwrap();
            let member = folder.join("member.new");
            fs::write(&member, b"unfinished").unwrap();
            seal(&member);
            let freed = cache.sweep(&Needed::default(), SystemTime::now());
            assert_eq!(freed.leftovers, 1);
            assert!(!folder.exists());
        }
    }

    #[test]
    fn staging_without_locking_keeps_the_age_rule() {
        for (_dir, cache) in [cache(), fat()] {
            let folder = cache.root().join("tmp").join("0123456789abcdef.stage");
            fs::create_dir(&folder).unwrap();
            fs::write(folder.join("unlocked"), b"").unwrap();
            let member = folder.join("member.new");
            fs::write(&member, b"unfinished").unwrap();
            let now = SystemTime::now();
            let written = now + GRACE;
            fs::OpenOptions::new()
                .write(true)
                .open(&member)
                .unwrap()
                .set_modified(written)
                .unwrap();
            seal(&member);
            assert_eq!(cache.sweep(&Needed::default(), now).leftovers, 0);
            assert_eq!(
                cache
                    .sweep(&Needed::default(), written + Duration::from_secs(60))
                    .leftovers,
                0,
                "a recent write keeps a member in an older folder"
            );
            assert!(member.exists());
            let later = written + GRACE + Duration::from_secs(60);
            assert_eq!(cache.sweep(&Needed::default(), later).leftovers, 1);
            assert!(!folder.exists());
        }
    }

    #[test]
    fn a_run_that_opened_a_lock_before_a_sweep_owns_the_download_alone() {
        let (_dir, cache) = cache();
        let url = "https://example.org/raced";
        // every download leaves its lock file behind
        drop(cache.claim(url).unwrap());
        let key = digest::sha256(url.as_bytes()).hex();
        let lock = cache.root.join("partial").join(format!("{key}.lock"));
        // a run waiting for the download opens the lock, gc sweeps, then the run takes the lock
        let opened = super::super::partial::open_unfollowed(&lock, true).unwrap();
        cache.sweep(
            &Needed::default(),
            SystemTime::now() + GRACE + Duration::from_secs(60),
        );
        opened.try_lock().unwrap();
        assert!(
            matches!(cache.claim(url).unwrap(), super::super::Claim::Busy),
            "a second run owns the same download"
        );
    }

    #[test]
    fn a_sweep_keeps_the_records_of_archives_in_use() {
        let (_dir, cache) = cache();
        let used = digest::sha256(b"used archive");
        let unused = digest::sha256(b"unused archive");
        for archive in [used, unused] {
            cache.record_unpacked(&archive, &[]).unwrap();
        }
        let keep = Needed {
            unpacked: HashSet::from([used]),
            ..Needed::default()
        };
        cache.sweep(&keep, SystemTime::now() + GRACE + Duration::from_secs(60));
        assert_eq!(cache.unpacked_archives(), [used]);
    }

    #[test]
    fn verifying_marks_objects_whose_bytes_changed() {
        let (_dir, cache) = cache();
        let good = store(&cache, b"good");
        let bad = store(&cache, b"bad!");
        let mut permissions = fs::metadata(cache.object_path(&bad.digest))
            .unwrap()
            .permissions();
        #[expect(
            clippy::permissions_set_readonly_false,
            reason = "the test damages an object on purpose"
        )]
        permissions.set_readonly(false);
        fs::set_permissions(cache.object_path(&bad.digest), permissions).unwrap();
        fs::OpenOptions::new()
            .write(true)
            .open(cache.object_path(&bad.digest))
            .unwrap()
            .write_all(b"BAD!")
            .unwrap();
        let seen = AtomicUsize::new(0);
        let checked = cache.verify_all(3, &|bytes| {
            seen.fetch_add(usize::try_from(bytes).unwrap(), Ordering::Relaxed);
        });
        assert_eq!(checked.objects, 2);
        assert_eq!(checked.changed, vec![bad.digest]);
        assert_eq!(seen.load(Ordering::Relaxed), 8);
        assert!(cache.find(&good.digest, Some(4)).is_some());
        assert!(cache.find(&bad.digest, Some(4)).is_none());
        assert_eq!(
            cache.changed().into_keys().collect::<Vec<_>>(),
            [bad.digest],
            "projects linking to it can learn it changed"
        );
    }

    #[test]
    fn unreadable_objects_are_kept_without_a_change_mark() {
        let (_dir, cache) = cache();
        let stored = store(&cache, b"intact");
        let object = cache.object_path(&stored.digest);
        #[cfg(windows)]
        let held = {
            use std::os::windows::fs::OpenOptionsExt;
            const FILE_SHARE_DELETE: u32 = 4;
            fs::OpenOptions::new()
                .read(true)
                .share_mode(FILE_SHARE_DELETE)
                .open(&object)
                .unwrap()
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if rustix::process::geteuid().is_root() {
                return;
            }
            fs::set_permissions(&object, fs::Permissions::from_mode(0o000)).unwrap();
        }
        assert!(cache.verify(&stored.digest).is_err());
        assert!(object.exists());
        assert!(cache.changed().is_empty());
        let checked = cache.verify_all(2, &|_| {});
        #[cfg(windows)]
        drop(held);
        assert!(
            checked.changed.is_empty(),
            "a read error is not changed bytes"
        );
        assert_eq!(checked.stuck, vec![]);
        assert!(object.exists(), "unreadable objects must stay in the cache");
        assert!(cache.changed().is_empty());
        assert_eq!(checked.objects, 1);
        assert_eq!(checked.bytes, 6);
        assert_eq!(
            serde_json::to_value(&checked).unwrap()["unreadable"],
            serde_json::json!([stored.digest])
        );
    }

    #[test]
    fn a_changed_object_held_open_is_still_marked() {
        let (_dir, cache) = cache();
        let bad = store(&cache, b"bad!");
        let object = cache.object_path(&bad.digest);
        damage(&object);
        let held = hold(&object);
        let checked = cache.verify_all(2, &|_| {});
        drop(held);
        assert_eq!(checked.changed, vec![bad.digest]);
        assert_eq!(checked.stuck, []);
        assert_eq!(fs::read(&object).unwrap(), b"BAD!");
    }

    #[test]
    fn objects_whose_bytes_rotted_wait_for_replacement() {
        for (_dir, cache) in [cache(), fat()] {
            let rotten = [store(&cache, b"rot1"), store(&cache, b"rot2")];
            for object in &rotten {
                damage(&cache.object_path(&object.digest));
                // bytes that rot on disk leave the read-only flag as it was
                seal(&cache.object_path(&object.digest));
            }
            assert!(!cache.verify(&rotten[0].digest).unwrap());
            let checked = cache.verify_all(2, &|_| {});
            assert_eq!(checked.changed.len(), 2);
            assert!(
                rotten
                    .iter()
                    .all(|object| checked.changed.contains(&object.digest))
            );
            assert_eq!(checked.stuck, vec![]);
            assert!(
                rotten
                    .iter()
                    .all(|object| cache.object_path(&object.digest).exists())
            );
        }
    }

    fn damage(path: &Path) {
        let mut permissions = fs::metadata(path).unwrap().permissions();
        #[expect(
            clippy::permissions_set_readonly_false,
            reason = "the test damages an object on purpose"
        )]
        permissions.set_readonly(false);
        fs::set_permissions(path, permissions).unwrap();
        fs::OpenOptions::new()
            .write(true)
            .open(path)
            .unwrap()
            .write_all(b"BAD!")
            .unwrap();
    }
}
