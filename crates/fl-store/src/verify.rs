//! Verification: rehash objects and delete those whose bytes no longer match their name.

use std::fs;

use fl_model::digest::Digest;
use fl_model::document::Work;

use crate::error::{StoreError, io};
use crate::files;
use crate::fingerprint::hash_file;
use crate::layout::object_digest;
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
        let dir = self.layout().dir("objects");
        for fan in fs::read_dir(&dir).map_err(io("list", &dir))? {
            let fan = fan.map_err(io("list", &dir))?.path();
            for object in fs::read_dir(&fan).map_err(io("list", &fan))? {
                let path = object.map_err(io("list", &fan))?.path();
                let digest = object_digest(&fan, &path)?;
                if select(&digest) {
                    objects.push((digest, path));
                }
            }
        }
        let results = parallel::map(&objects, JOBS, |(digest, path)| {
            let actual = hash_file(path)?;
            if actual == *digest {
                return Ok(None);
            }
            files::remove(path)?;
            Ok(Some(*digest))
        });
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
