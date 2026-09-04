//! What travels beside a quarantined object.

use std::ops::Range;

use fetchloom_engine::digest::ContentDigest;
use fetchloom_engine::error::Error;
use fetchloom_engine::seam::platform::Platform;
use fetchloom_engine::timestamp::Timestamp;
use serde::{Deserialize, Serialize};

use crate::Cache;
use crate::record;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotLocalized {
    NoTreeStored,
    TreeDoesNotCheckOut,
    ObjectUnreadable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DamagedRange {
    pub start: u64,
    pub end: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Diagnosis {
    pub digest: ContentDigest,
    pub found: Option<ContentDigest>,
    pub size: u64,
    pub damaged: Vec<DamagedRange>,
    pub localized: Option<NotLocalized>,
    pub source: Option<String>,
    pub validator: Option<String>,
    pub quarantined_at: Timestamp,
    pub next_action: String,
}

impl Diagnosis {
    #[must_use]
    pub fn damaged_bytes(&self) -> u64 {
        self.damaged
            .iter()
            .map(|span| span.end.saturating_sub(span.start))
            .sum()
    }

    #[must_use]
    pub fn ranges(&self) -> Vec<Range<u64>> {
        self.damaged
            .iter()
            .map(|span| span.start..span.end)
            .collect()
    }
}

impl<P: Platform> Cache<P> {
    pub fn write_diagnosis(&self, found: &Diagnosis) -> Result<(), Error> {
        record::write(
            &self.layout().diagnosis_of(found.digest),
            found,
            self.work(),
        )
    }

    pub fn read_diagnosis(&self, digest: ContentDigest) -> Result<Option<Diagnosis>, Error> {
        record::read(&self.layout().diagnosis_of(digest))
    }
}

#[must_use]
pub fn spans_of(damaged: &[Range<u64>]) -> Vec<DamagedRange> {
    damaged
        .iter()
        .map(|span| DamagedRange {
            start: span.start,
            end: span.end,
        })
        .collect()
}
