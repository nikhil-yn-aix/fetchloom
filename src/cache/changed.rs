//! objects found changed, remembered after they leave the cache, so a project holding a hardlink
//! to the changed bytes learns it from one listing instead of reading every file it placed.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::PathBuf;
use std::time::UNIX_EPOCH;

use super::Cache;
use crate::digest::Sha256;

impl Cache {
    /// remembers that the object for `digest` no longer held its bytes, as of now.
    pub(crate) fn mark_changed(&self, digest: &Sha256) -> io::Result<()> {
        let folder = self.changed_folder();
        super::real_folder(&folder)?;
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
                let since = found.duration_since(UNIX_EPOCH).ok()?;
                Some((digest, u64::try_from(since.as_nanos()).unwrap_or(u64::MAX)))
            })
            .collect()
    }

    /// forgets changed objects that no project needs, since no project can hold a link to them
    /// that it would still put back.
    pub(crate) fn forget_changed(&self, needed: &HashSet<Sha256>) {
        for digest in self.changed().into_keys() {
            if !needed.contains(&digest) && super::real_folder(&self.changed_folder()).is_ok() {
                let _ = fs::remove_file(self.changed_folder().join(digest.hex()));
            }
        }
    }

    fn changed_folder(&self) -> PathBuf {
        self.root.join("changed")
    }
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
