//! Roots: the lock files that synced from a tier, for which gc keeps everything they need.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::error::{StoreError, io};
use crate::mode;
use crate::staged::{Scratch, create_unique};
use crate::tier::Tier;

/// A lock file registered in a tier.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Root {
    /// The record in the tier's `roots/` directory.
    pub record: PathBuf,
    /// The absolute path of the lock file.
    pub lock: PathBuf,
}

impl Root {
    /// Whether the lock file is gone. A lock that cannot be checked for another reason, such as
    /// an unreadable directory, counts as present.
    #[must_use]
    pub fn is_dead(&self) -> bool {
        matches!(fs::symlink_metadata(&self.lock), Err(error) if error.kind() == io::ErrorKind::NotFound)
    }
}

impl Tier {
    /// Registers the lock file at `lock` as a root, named by the BLAKE3 hash of its absolute
    /// path. Registering a lock again changes nothing.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Unrepresentable`] for a path that is not UTF-8, and
    /// [`StoreError::Io`] when the record cannot be written.
    pub fn register_root(&self, lock: &Path) -> Result<(), StoreError> {
        let absolute = std::path::absolute(lock).map_err(io("resolve", lock))?;
        let text = absolute
            .to_str()
            .ok_or_else(|| StoreError::Unrepresentable {
                path: absolute.to_string_lossy().into_owned(),
                reason: "a lock path must be UTF-8 to be registered as a root",
            })?;
        let record = self
            .layout()
            .dir("roots")
            .join(blake3::hash(text.as_bytes()).to_hex().as_str());
        if fs::symlink_metadata(&record).is_ok() {
            return Ok(());
        }
        let (mut file, path) = create_unique(&self.layout().dir("tmp"))?;
        let scratch = Scratch::new(path);
        file.write_all(text.as_bytes())
            .and_then(|()| mode::seal(&file))
            .map_err(io("write", scratch.path()))?;
        drop(file);
        self.place(scratch, &record).map(|_| ())
    }

    /// Every registered root.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Io`] when `roots/` or a record cannot be read.
    pub fn roots(&self) -> Result<Vec<Root>, StoreError> {
        let dir = self.layout().dir("roots");
        let mut roots = Vec::new();
        for entry in fs::read_dir(&dir).map_err(io("list", &dir))? {
            let record = entry.map_err(io("list", &dir))?.path();
            let lock = fs::read_to_string(&record).map_err(io("read", &record))?;
            roots.push(Root {
                record,
                lock: PathBuf::from(lock),
            });
        }
        roots.sort_unstable_by(|a, b| a.lock.cmp(&b.lock));
        Ok(roots)
    }

    /// Removes the record of `root`.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Io`] when the record cannot be removed.
    pub fn drop_root(&self, root: &Root) -> Result<(), StoreError> {
        match fs::remove_file(&root.record) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => {
                Err(io("remove", &root.record)(error))
            }
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tier::Sharing;

    #[test]
    fn a_lock_registered_twice_is_one_root_until_it_dies() {
        let temp = tempfile::tempdir().unwrap();
        let tier = Tier::open(&temp.path().join("tier"), Sharing::Private).unwrap();
        let lock = temp.path().join("project").join("data.lock");
        fs::create_dir_all(lock.parent().unwrap()).unwrap();
        fs::write(&lock, b"version = 1\n").unwrap();
        tier.register_root(&lock).unwrap();
        tier.register_root(&lock).unwrap();
        let roots = tier.roots().unwrap();
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].lock, std::path::absolute(&lock).unwrap());
        assert!(!roots[0].is_dead());
        fs::remove_file(&lock).unwrap();
        assert!(roots[0].is_dead());
        tier.drop_root(&roots[0]).unwrap();
        assert!(tier.roots().unwrap().is_empty());
        assert_eq!(
            fs::read_dir(temp.path().join("tier/v1/tmp"))
                .unwrap()
                .count(),
            0
        );
    }
}
