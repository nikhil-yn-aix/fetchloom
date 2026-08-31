//! What a run learned about a host, kept so the next run starts where it
//! finished.

use fetchloom_engine::digest::MEASUREMENT_KEY_CONTEXT;
use fetchloom_engine::error::Error;
use fetchloom_engine::seam::platform::Platform;
use fetchloom_engine::tuning::HostMeasurement;
use serde::{Deserialize, Serialize};

use crate::Cache;
use crate::record;

/// One measurement and the host it was taken against.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Measured {
    /// The host, as the source named it.
    host: String,
    /// What the run learned about it.
    measurement: HostMeasurement,
}

/// Returns the name a host's measurement is filed under.
#[must_use]
pub fn key_of(host: &str) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new_derive_key(MEASUREMENT_KEY_CONTEXT);
    hasher.update(host.as_bytes());
    *hasher.finalize().as_bytes()
}

impl<P: Platform> Cache<P> {
    /// Returns what this cache recorded about a host, and nothing when the
    /// record is absent or unreadable.
    ///
    /// A measurement is derived data that only ever makes a run faster or
    /// slower, so one that cannot be read is discarded rather than reported:
    /// the run measures again.
    #[must_use]
    pub fn measurement(&self, host: &str) -> Option<HostMeasurement> {
        let held: Option<Measured> =
            record::read(&self.layout().measurement_of(&key_of(host))).ok()?;
        held.map(|found| found.measurement)
    }

    /// Records what a run learned about a host.
    ///
    /// # Errors
    ///
    /// Fails when the record cannot be written.
    pub fn record_measurement(
        &self,
        host: &str,
        measurement: &HostMeasurement,
    ) -> Result<(), Error> {
        record::write(
            &self.layout().measurement_of(&key_of(host)),
            &Measured {
                host: host.to_owned(),
                measurement: *measurement,
            },
            self.work(),
        )
    }

    /// Returns every host measurement this cache holds, by host, ascending.
    #[must_use]
    pub fn measurements(&self) -> Vec<(String, HostMeasurement)> {
        let Ok(entries) = std::fs::read_dir(self.layout().measurements()) else {
            return Vec::new();
        };
        let mut found: Vec<(String, HostMeasurement)> = entries
            .flatten()
            .filter_map(|entry| record::read::<Measured>(&entry.path()).ok().flatten())
            .map(|held| (held.host, held.measurement))
            .collect();
        found.sort_by(|left, right| left.0.cmp(&right.0));
        found
    }
}
