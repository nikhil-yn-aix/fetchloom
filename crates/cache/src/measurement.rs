//! What a run learned about a host, kept so the next run starts where it
//! finished.

use fetchloom_engine::digest::MEASUREMENT_KEY_CONTEXT;
use fetchloom_engine::error::Error;
use fetchloom_engine::seam::platform::Platform;
use fetchloom_engine::tuning::HostMeasurement;
use serde::{Deserialize, Serialize};

use crate::Cache;
use crate::record;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Measured {
    host: String,
    measurement: HostMeasurement,
}

#[must_use]
pub fn key_of(host: &str) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new_derive_key(MEASUREMENT_KEY_CONTEXT);
    hasher.update(host.as_bytes());
    *hasher.finalize().as_bytes()
}

impl<P: Platform> Cache<P> {
    #[must_use]
    pub fn measurement(&self, host: &str) -> Option<HostMeasurement> {
        let held: Option<Measured> =
            record::read(&self.layout().measurement_of(&key_of(host))).ok()?;
        held.map(|found| found.measurement)
    }

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
