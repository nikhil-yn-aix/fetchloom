//! objects found changed, remembered after they leave the cache, so a project holding a hardlink
//! to the changed bytes learns it from one listing instead of reading every file it placed. an
//! object still in the cache that a mark is newer than is hashed before anything is placed from
//! it.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use super::Cache;
use crate::digest::Sha256;
use crate::place::{FolderUse, real_folder};

/// an object in doubt that could not be hashed or sealed, or that held other bytes and could
/// not be removed.
#[derive(Debug)]
pub(crate) struct Doubt {
    pub(crate) object: PathBuf,
    /// whether hashing found other bytes, so the object stays only because removing it failed.
    pub(crate) changed: bool,
    pub(crate) error: io::Error,
}

impl Cache {
    /// the object for `digest` when the cache holds it, with `size` when one is known, and its
    /// bytes can be trusted. a mark in `marks` newer than the object puts it in doubt, so it is
    /// hashed first, as is a writable object. intact writable objects are sealed again; changed
    /// ones are marked and removed, so they are fetched again.
    pub(crate) fn trusted(
        &self,
        digest: &Sha256,
        size: Option<u64>,
        marks: &HashMap<Sha256, u64>,
    ) -> Result<Option<PathBuf>, Doubt> {
        let path = self.object_path(digest);
        let Ok(metadata) = fs::metadata(&path) else {
            return Ok(None);
        };
        if !metadata.is_file() || size.is_some_and(|size| metadata.len() != size) {
            return Ok(None);
        }
        let mut permissions = metadata.permissions();
        let writable = !permissions.readonly();
        let written = metadata.modified().map_or(0, nanos);
        if !writable && marks.get(digest).is_none_or(|marked| *marked <= written) {
            return Ok(Some(path));
        }
        match Sha256::of_file(&path) {
            Ok(actual) if actual == *digest => {
                if writable {
                    permissions.set_readonly(true);
                    fs::set_permissions(&path, permissions).map_err(|error| Doubt {
                        object: path.clone(),
                        changed: false,
                        error,
                    })?;
                }
                Ok(Some(path))
            }
            Ok(_) => {
                if let Err(err) = self.mark_changed(digest) {
                    tracing::debug!(%err, "could not remember a changed object");
                }
                match self.drop_object(digest) {
                    Ok(()) => Ok(None),
                    Err(error) => Err(Doubt {
                        object: path,
                        changed: true,
                        error,
                    }),
                }
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(Doubt {
                object: path,
                changed: false,
                error,
            }),
        }
    }

    /// remembers that the object for `digest` no longer held its bytes, as of now.
    pub(crate) fn mark_changed(&self, digest: &Sha256) -> io::Result<()> {
        let folder = self.changed_folder();
        real_folder(&folder, FolderUse::Cache)?;
        self.write_atomically(&folder.join(digest.hex()), b"")
    }

    /// every object found changed, with when it was found, in nanoseconds since 1970 like the
    /// times a project records for its files.
    pub(crate) fn changed(&self) -> HashMap<Sha256, u64> {
        let folder = self.changed_folder();
        if !fs::symlink_metadata(&folder).is_ok_and(|metadata| {
            let kind = metadata.file_type();
            kind.is_dir() && !kind.is_symlink()
        }) {
            return HashMap::new();
        }
        fs::read_dir(folder)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| {
                if !entry.file_type().ok()?.is_file() {
                    return None;
                }
                let digest = Sha256::from_hex(&entry.file_name().to_string_lossy())?;
                let found = entry.metadata().ok()?.modified().ok()?;
                Some((digest, nanos(found)))
            })
            .collect()
    }

    /// forgets changed objects that no project needs, since no project can hold a link to them
    /// that it would still put back.
    pub(crate) fn forget_changed(&self, needed: &HashSet<Sha256>) {
        for digest in self.changed().into_keys() {
            if !needed.contains(&digest)
                && real_folder(&self.changed_folder(), FolderUse::Cache).is_ok()
            {
                let _ = fs::remove_file(self.changed_folder().join(digest.hex()));
            }
        }
    }

    fn changed_folder(&self) -> PathBuf {
        self.root.join("changed")
    }
}

/// `time` in nanoseconds since 1970, as projects record the times of their files.
fn nanos(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH).map_or(0, |since| {
        u64::try_from(since.as_nanos()).unwrap_or(u64::MAX)
    })
}

#[cfg(test)]
mod tests {
    use super::super::tests::{cache, store};
    use super::*;

    #[test]
    fn marking_a_changed_object_never_writes_through_a_link() {
        let (dir, cache) = cache();
        let outside = dir.path().join("outside");
        fs::write(&outside, b"keep these bytes").unwrap();
        let digest = crate::digest::sha256(b"object");
        fs::create_dir_all(cache.changed_folder()).unwrap();
        let mark = cache.changed_folder().join(digest.hex());
        // a filesystem without hardlinks, such as FAT, cannot hold this trap
        if fs::hard_link(&outside, &mark).is_err() {
            return;
        }

        cache.mark_changed(&digest).unwrap();

        assert_eq!(fs::read(outside).unwrap(), b"keep these bytes");
        assert_eq!(fs::read(mark).unwrap(), b"");
    }

    #[test]
    fn writable_objects_are_hashed_and_resealed_before_use() {
        for damaged in [false, true] {
            let (_dir, cache) = cache();
            let stored = store(&cache, b"good");
            let object = cache.object_path(&stored.digest);
            let mut permissions = fs::metadata(&object).unwrap().permissions();
            #[expect(
                clippy::permissions_set_readonly_false,
                reason = "the test models a user clearing a hardlink's flag"
            )]
            permissions.set_readonly(false);
            fs::set_permissions(&object, permissions).unwrap();
            if damaged {
                fs::write(&object, b"evil").unwrap();
            }

            let found = cache
                .trusted(&stored.digest, Some(4), &HashMap::new())
                .unwrap();

            if damaged {
                assert!(found.is_none());
                assert!(!object.exists());
                assert!(cache.changed().contains_key(&stored.digest));
            } else {
                assert_eq!(found.as_deref(), Some(object.as_path()));
                assert!(
                    fs::metadata(&object).unwrap().permissions().readonly(),
                    "a trusted object must be read-only"
                );
                assert_eq!(fs::read(&object).unwrap(), b"good");
                assert!(cache.changed().is_empty());
            }
        }
    }

    #[test]
    fn a_changed_object_is_remembered_until_no_project_needs_it() {
        let (_dir, cache) = cache();
        assert!(cache.changed().is_empty(), "nothing marked");
        let kept = store(&cache, b"kept").digest;
        let dropped = store(&cache, b"dropped").digest;
        for digest in [kept, dropped] {
            cache.mark_changed(&digest).unwrap();
        }
        let changed = cache.changed();
        assert_eq!(changed.len(), 2);
        assert!(changed.values().all(|found| *found > 0));
        cache.forget_changed(&HashSet::from([kept]));
        assert_eq!(cache.changed().into_keys().collect::<Vec<_>>(), [kept]);
    }
}
