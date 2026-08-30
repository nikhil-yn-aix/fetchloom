//! Where a run's own observations of an artifact are kept.
//!
//! A witness is written only by a run on this machine that transferred the
//! bytes in full and verified them as they arrived. Nothing read from a source,
//! a bundle, a lock, a receipt or a plan ever becomes one, so no remote party
//! can put a witness here.

use fetchloom_engine::error::Error;
use fetchloom_engine::seam::platform::Platform;
use fetchloom_engine::trust::{ArtifactKey, Witness};
use serde::{Deserialize, Serialize};

use crate::Cache;
use crate::record;

/// Every observation recorded for one artifact key.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Witnesses {
    /// The observations, in the order they were recorded.
    pub observed: Vec<Witness>,
}

impl<P: Platform> Cache<P> {
    /// Returns every witness recorded for an artifact.
    ///
    /// Returns nothing recorded when the cache holds no record, which is what a
    /// cache that has never fetched the artifact holds.
    ///
    /// # Errors
    ///
    /// Fails when a record is present and does not parse.
    pub fn witnesses(&self, key: &ArtifactKey) -> Result<Vec<Witness>, Error> {
        let held: Option<Witnesses> = record::read(&self.layout().witness_of(key))?;
        Ok(held.unwrap_or_default().observed)
    }

    /// Records one observation of an artifact, keeping every earlier one.
    ///
    /// An observation identical to one already recorded is not written again,
    /// because the same observation twice is still one observation and a record
    /// that grew on every run would count it as two.
    ///
    /// # Errors
    ///
    /// Fails when the record cannot be read or written.
    pub fn record_witness(&self, key: &ArtifactKey, seen: Witness) -> Result<(), Error> {
        let mut observed = self.witnesses(key)?;
        if observed.iter().any(|held| same_observation(held, &seen)) {
            return Ok(());
        }
        observed.push(seen);
        record::write(
            &self.layout().witness_of(key),
            &Witnesses { observed },
            self.work(),
        )
    }
}

/// Reports whether two records are the same observation.
///
/// The instant is not compared, because one machine observing one origin in one
/// run twice at two times is one observation and recording it under two
/// instants is how that would be counted as two.
fn same_observation(one: &Witness, other: &Witness) -> bool {
    one.digest == other.digest
        && one.machine == other.machine
        && one.origin == other.origin
        && one.run == other.run
}
