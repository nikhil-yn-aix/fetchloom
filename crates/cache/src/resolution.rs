//! What a reference last resolved to, and the validator the source gave for it.

use fetchloom_engine::digest::{ContentDigest, RESOLUTION_KEY_CONTEXT};
use fetchloom_engine::error::Error;
use fetchloom_engine::redact::SafeUrl;
use fetchloom_engine::seam::platform::Platform;
use serde::{Deserialize, Serialize};

use crate::Cache;
use crate::record;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Resolution {
    pub digest: ContentDigest,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Located {
    location: SafeUrl,
    resolution: Resolution,
}

#[must_use]
pub(crate) fn key_of(location: &str) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new_derive_key(RESOLUTION_KEY_CONTEXT);
    hasher.update(location.as_bytes());
    *hasher.finalize().as_bytes()
}

impl<P: Platform> Cache<P> {
    /// # Errors
    /// `cache.corrupt` when a record exists and does not parse. No record is
    /// `None` rather than an error.
    pub fn resolution(&self, location: &str) -> Result<Option<Resolution>, Error> {
        let held: Option<Located> = record::read(&self.layout().resolution_of(&key_of(location)))?;
        Ok(held.map(|found| found.resolution))
    }

    #[must_use]
    pub fn recorded_source_of(&self, digest: ContentDigest) -> (Option<String>, Option<String>) {
        let Ok(entries) = std::fs::read_dir(self.layout().resolutions()) else {
            return (None, None);
        };
        for entry in entries.flatten() {
            let Ok(Some(found)) = record::read::<Located>(&entry.path()) else {
                continue;
            };
            if found.resolution.digest == digest {
                return (
                    Some(found.location.as_str().to_owned()),
                    found.resolution.etag.or(found.resolution.last_modified),
                );
            }
        }
        (None, None)
    }

    /// # Errors
    /// `cache.corrupt` when the record cannot be written, and `resource.disk`
    /// when the volume is full.
    pub fn record_resolution(&self, location: &str, found: &Resolution) -> Result<(), Error> {
        record::write(
            &self.layout().resolution_of(&key_of(location)),
            &Located {
                location: SafeUrl::new(location),
                resolution: found.clone(),
            },
            self.work(),
        )
    }
}
