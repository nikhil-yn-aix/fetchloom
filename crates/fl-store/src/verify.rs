//! Verification: rehash objects and delete those whose bytes no longer match their name.

use std::io;
use std::path::Path;

use fl_model::digest::Digest;
use fl_model::document::Work;

use crate::error::StoreError;
use crate::files;
use crate::fingerprint::hash_file;
use crate::layout::{entries, object_digest};
use crate::parallel;
use crate::tier::Tier;

/// The number of objects hashed at once.
const JOBS: usize = 4;

/// What a verification found.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Verified {
    /// The objects rehashed.
    pub checked: u64,
    /// The objects whose bytes did not match their name, now deleted.
    pub damaged: Vec<Digest>,
}

impl Verified {
    /// Adds the rehashed objects to the work counters.
    pub fn add_to(&self, work: &mut Work) {
        work.files_rehashed += self.checked;
    }
}

impl Tier {
    /// Rehashes every object `select` picks, deletes each one whose bytes no longer hash to its
    /// name, and names them, so the next sync fetches them again.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Foreign`] for a file in `objects/` not named like an object, and
    /// [`StoreError::Io`] when an object cannot be read or deleted.
    pub fn verify(&self, select: impl Fn(&Digest) -> bool + Sync) -> Result<Verified, StoreError> {
        let mut objects = Vec::new();
        for fan in entries(&self.layout().dir("objects"))? {
            for path in entries(&fan)? {
                let digest = object_digest(&fan, &path)?;
                if select(&digest) {
                    objects.push((digest, path));
                }
            }
        }
        let results = parallel::map(&objects, JOBS, |(digest, path)| check(digest, path));
        let mut verified = Verified {
            checked: objects.len() as u64,
            damaged: Vec::new(),
        };
        for result in results {
            verified.damaged.extend(result?);
        }
        verified.damaged.sort_unstable();
        Ok(verified)
    }
}

/// Rehashes the object at `path` and removes it when its bytes no longer hash to `digest`,
/// naming it. An object already gone, taken by gc while verify ran, is passed over.
fn check(digest: &Digest, path: &Path) -> Result<Option<Digest>, StoreError> {
    let actual = match hash_file(path) {
        Err(StoreError::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
            return Ok(None);
        }
        hashed => hashed?,
    };
    if actual == *digest {
        return Ok(None);
    }
    files::remove(path)?;
    Ok(Some(*digest))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn a_gone_object_is_passed_over_and_an_unreadable_one_is_an_error() {
        let temp = tempfile::tempdir().unwrap();
        let digest = Digest::Blake3(*blake3::hash(b"kept").as_bytes());
        let kept = temp.path().join("kept");
        fs::write(&kept, b"kept").unwrap();
        assert_eq!(check(&digest, &kept).unwrap(), None);
        assert_eq!(check(&digest, &temp.path().join("gone")).unwrap(), None);
        assert!(check(&digest, temp.path()).is_err());
        let damaged = temp.path().join("damaged");
        fs::write(&damaged, b"changed").unwrap();
        assert_eq!(check(&digest, &damaged).unwrap(), Some(digest));
        assert!(!damaged.exists());
    }
}
