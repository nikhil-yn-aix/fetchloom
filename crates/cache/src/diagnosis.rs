//! What travels beside a quarantined object.

use std::ops::Range;

use fetchloom_engine::digest::ContentDigest;
use fetchloom_engine::error::Error;
use fetchloom_engine::seam::platform::Platform;
use fetchloom_engine::timestamp::Timestamp;
use serde::{Deserialize, Serialize};

use crate::Cache;
use crate::record;

/// Why the damage in a quarantined object could not be narrowed to byte ranges.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotLocalized {
    /// The object is at or below the outboard threshold, so no tree was stored.
    NoTreeStored,
    /// A tree was stored and does not check out against the digest.
    TreeDoesNotCheckOut,
    /// The object's own bytes could not be read.
    ObjectUnreadable,
}

/// One half-open span of an object, as a diagnosis records it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DamagedRange {
    /// The first byte of the span.
    pub start: u64,
    /// One past the last byte of the span.
    pub end: u64,
}

/// What was found when an object failed verification.
///
/// Enough for a person to understand the damage without running anything
/// again.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Diagnosis {
    /// The digest the object is named by, and what it should hash to.
    pub digest: ContentDigest,
    /// What the bytes actually hash to, absent when they could not be read.
    pub found: Option<ContentDigest>,
    /// The length of the object on disk.
    pub size: u64,
    /// The byte ranges that failed against the tree, ascending.
    pub damaged: Vec<DamagedRange>,
    /// Why `damaged` is empty, absent when it is not.
    ///
    /// An empty list means the damage could not be narrowed, never that there
    /// was none.
    pub localized: Option<NotLocalized>,
    /// The redacted location the bytes came from, when the cache recorded one.
    pub source: Option<String>,
    /// What the source said identified those bytes, when it said anything.
    pub validator: Option<String>,
    /// When the object was moved into quarantine.
    pub quarantined_at: Timestamp,
    /// The command that fetches the damaged bytes again.
    pub next_action: String,
}

impl Diagnosis {
    /// Returns how many bytes the diagnosis found damaged.
    #[must_use]
    pub fn damaged_bytes(&self) -> u64 {
        self.damaged
            .iter()
            .map(|span| span.end.saturating_sub(span.start))
            .sum()
    }

    /// Returns the damaged spans as ranges.
    #[must_use]
    pub fn ranges(&self) -> Vec<Range<u64>> {
        self.damaged
            .iter()
            .map(|span| span.start..span.end)
            .collect()
    }
}

impl<P: Platform> Cache<P> {
    /// Writes the diagnosis beside a quarantined object.
    ///
    /// # Errors
    ///
    /// Fails when the record cannot be written.
    pub fn write_diagnosis(&self, found: &Diagnosis) -> Result<(), Error> {
        record::write(
            &self.layout().diagnosis_of(found.digest),
            found,
            self.work(),
        )
    }

    /// Reads the diagnosis beside a quarantined object.
    ///
    /// Returns nothing when none was written.
    ///
    /// # Errors
    ///
    /// Fails when one is present and does not parse.
    pub fn read_diagnosis(&self, digest: ContentDigest) -> Result<Option<Diagnosis>, Error> {
        record::read(&self.layout().diagnosis_of(digest))
    }
}

/// Turns byte ranges into the spans a diagnosis records.
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
