//! Where a run's own observations of an artifact are kept.

use fetchloom_engine::error::Error;
use fetchloom_engine::seam::platform::Platform;
use fetchloom_engine::trust::{ArtifactKey, Witness};
use serde::{Deserialize, Serialize};

use crate::Cache;
use crate::record;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Witnesses {
    pub(crate) observed: Vec<Witness>,
}

impl<P: Platform> Cache<P> {
    /// # Errors
    /// `cache.corrupt` when a witness record exists and does not parse. No
    /// record is an empty list rather than an error.
    pub fn witnesses(&self, key: &ArtifactKey) -> Result<Vec<Witness>, Error> {
        let held: Option<Witnesses> = record::read(&self.layout().witness_of(key))?;
        Ok(held.unwrap_or_default().observed)
    }

    /// # Errors
    /// The kinds `witnesses` gives, `cache.corrupt` when the record cannot be
    /// written, and `resource.disk` when the volume is full.
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

fn same_observation(one: &Witness, other: &Witness) -> bool {
    one.digest == other.digest
        && one.machine == other.machine
        && one.origin == other.origin
        && one.run == other.run
}
