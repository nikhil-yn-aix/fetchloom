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
const GRACE: Duration = Duration::from_hours(24);

/// one object in the cache.
pub(crate) struct Object {
    pub(crate) digest: Sha256,
    pub(crate) size: u64,
    path: PathBuf,
    modified: SystemTime,
}

/// what a sweep freed.
#[derive(Debug, Default, Serialize)]
pub(crate) struct Freed {
    pub(crate) objects: u64,
    pub(crate) bytes: u64,
    /// unfinished downloads and temporary files nobody came back for.
    pub(crate) leftovers: u64,
}

/// what verifying found.
#[derive(Debug, Default, Serialize)]
pub(crate) struct Checked {
    pub(crate) objects: u64,
    pub(crate) bytes: u64,
    /// objects whose bytes no longer matched their name, now removed.
    pub(crate) changed: Vec<Sha256>,
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

    /// every object the cache holds.
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

    /// removes every object not in `needed` that is older than a day, and the unfinished downloads
    /// and temporary files left that long.
    pub(crate) fn sweep(&self, needed: &HashSet<Sha256>, now: SystemTime) -> Freed {
        let old = |modified: SystemTime| now.duration_since(modified).is_ok_and(|age| age > GRACE);
        let mut freed = Freed::default();
        for object in self.objects() {
            if !needed.contains(&object.digest)
                && old(object.modified)
                && fs::remove_file(&object.path).is_ok()
            {
                freed.objects += 1;
                freed.bytes += object.size;
            }
        }
        freed.leftovers += self.sweep_partials(&old);
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
            if stale && fs::remove_file(entry.path()).is_ok() {
                freed.leftovers += 1;
            }
        }
        freed
    }

    /// removes unfinished downloads that nobody holds and whose bytes have not grown in a day. a
    /// download that runs for days keeps writing, so it is never mistaken for an abandoned one.
    fn sweep_partials(&self, old: &dyn Fn(SystemTime) -> bool) -> u64 {
        let folder = self.root.join("partial");
        let mut removed = 0;
        for entry in fs::read_dir(&folder).into_iter().flatten().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(key) = name.strip_suffix(".lock") else {
                continue;
            };
            let Ok(lock) = fs::OpenOptions::new().write(true).open(entry.path()) else {
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
                if fs::remove_file(leftover).is_ok() {
                    removed += 1;
                }
            }
            drop(lock);
            let _ = fs::remove_file(entry.path());
        }
        removed
    }

    /// hashes every object again on `threads` threads and removes any whose bytes changed.
    /// `progress` hears the bytes as they are read.
    pub(crate) fn verify_all(&self, threads: usize, progress: &(dyn Fn(u64) + Sync)) -> Checked {
        let objects = self.objects();
        let next = AtomicUsize::new(0);
        let changed = Mutex::new(Vec::new());
        std::thread::scope(|scope| {
            for _ in 0..threads.max(1) {
                scope.spawn(|| {
                    while let Some(object) = objects.get(next.fetch_add(1, Ordering::Relaxed)) {
                        let intact = Sha256::of_file(&object.path)
                            .is_ok_and(|actual| actual == object.digest);
                        progress(object.size);
                        if !intact && fs::remove_file(&object.path).is_ok() {
                            changed
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .push(object.digest);
                        }
                    }
                });
            }
        });
        Checked {
            objects: objects.len() as u64,
            bytes: objects.iter().map(|object| object.size).sum(),
            changed: changed
                .into_inner()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        }
    }

    fn registration(&self, root: &Path) -> PathBuf {
        let key = digest::sha256(root.as_os_str().as_encoded_bytes()).hex();
        self.root.join("projects").join(format!("{key}.json"))
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::super::tests::{cache, store};
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
        let (_dir, cache) = cache();
        let needed = store(&cache, b"needed");
        let unneeded = store(&cache, b"unneeded");
        let later = SystemTime::now() + GRACE + Duration::from_secs(60);
        assert_eq!(
            cache
                .sweep(&HashSet::from([needed.digest]), SystemTime::now())
                .objects,
            0,
            "nothing is old yet"
        );
        let freed = cache.sweep(&HashSet::from([needed.digest]), later);
        assert_eq!((freed.objects, freed.bytes), (1, 8));
        assert!(cache.find(&needed.digest, Some(6)).is_some());
        assert!(cache.find(&unneeded.digest, Some(8)).is_none());
    }

    #[test]
    fn verifying_removes_objects_whose_bytes_changed() {
        let (_dir, cache) = cache();
        let good = store(&cache, b"good");
        let bad = store(&cache, b"bad!");
        let mut permissions = fs::metadata(&bad.path).unwrap().permissions();
        #[expect(
            clippy::permissions_set_readonly_false,
            reason = "the test damages an object on purpose"
        )]
        permissions.set_readonly(false);
        fs::set_permissions(&bad.path, permissions).unwrap();
        fs::OpenOptions::new()
            .write(true)
            .open(&bad.path)
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
    }
}
