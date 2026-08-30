//! What a reference last resolved to, and the validator the source gave for it.
//!
//! Derived data: losing it costs one transfer. It exists so a warm run of a
//! reference no digest pins can ask one conditional question instead of
//! fetching the whole object to learn the answer.

use fetchloom_engine::digest::{ContentDigest, RESOLUTION_KEY_CONTEXT};
use fetchloom_engine::error::Error;
use fetchloom_engine::seam::platform::Platform;
use serde::{Deserialize, Serialize};

use crate::Cache;
use crate::record;

/// What one reference resolved to the last time a run fetched it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Resolution {
    /// The object the reference resolved to.
    pub digest: ContentDigest,
    /// The entity tag the source gave, when it gave one.
    pub etag: Option<String>,
    /// The last modified value the source gave, when it gave one.
    pub last_modified: Option<String>,
}

impl Resolution {
    /// Reports whether anything here can be turned into a conditional request.
    #[must_use]
    pub fn can_be_asked_about(&self) -> bool {
        self.etag.is_some() || self.last_modified.is_some()
    }
}

/// One resolution and the reference it was recorded for.
///
/// The location is held so a quarantine can ask which reference an object came
/// from, which is the question the key cannot be run backwards to answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Located {
    /// The reference, redacted where it was constructed.
    location: String,
    /// What it resolved to.
    resolution: Resolution,
}

/// Returns the name a reference's resolution is filed under.
///
/// Takes the location as the run wrote it. The location is hashed under its own
/// derived key, so the name is a value no filesystem refuses and can never be
/// mistaken for an object digest.
#[must_use]
pub fn key_of(location: &str) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new_derive_key(RESOLUTION_KEY_CONTEXT);
    hasher.update(location.as_bytes());
    *hasher.finalize().as_bytes()
}

impl<P: Platform> Cache<P> {
    /// Returns what a reference last resolved to.
    ///
    /// Returns nothing when this cache has never fetched it.
    ///
    /// # Errors
    ///
    /// Fails when a record is present and does not parse.
    pub fn resolution(&self, location: &str) -> Result<Option<Resolution>, Error> {
        let held: Option<Located> = record::read(&self.layout().resolution_of(&key_of(location)))?;
        Ok(held.map(|found| found.resolution))
    }

    /// Returns the location and validator this cache recorded for an object.
    ///
    /// Reads every resolution record, because they are keyed by the reference
    /// that was fetched and this asks the question the other way round. Only a
    /// quarantine asks it, and a quarantine is rare enough to pay for a walk of
    /// a directory holding one small record per reference this cache has ever
    /// fetched.
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
                    Some(found.location),
                    found.resolution.etag.or(found.resolution.last_modified),
                );
            }
        }
        (None, None)
    }

    /// Records what a reference resolved to and the validator that came with
    /// it.
    ///
    /// # Errors
    ///
    /// Fails when the record cannot be written.
    pub fn record_resolution(&self, location: &str, found: &Resolution) -> Result<(), Error> {
        record::write(
            &self.layout().resolution_of(&key_of(location)),
            &Located {
                location: location.to_owned(),
                resolution: found.clone(),
            },
            self.work(),
        )
    }
}
