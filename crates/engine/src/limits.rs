//! The bounds no run may exceed.

use std::num::NonZeroU64;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// The size of one outboard leaf, in bytes.
pub const OUTBOARD_CHUNK_GROUP: u64 = 1_048_576;

/// The object size at or below which no outboard tree is stored.
pub const OUTBOARD_THRESHOLD: u64 = 67_108_864;

/// Every configurable bound, at its default.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limits {
    /// Most entries one archive may contain.
    pub archive_entries: u64,
    /// Most bytes one archive may expand to.
    pub expanded_bytes: u64,
    /// Most times an archive may expand relative to its own size.
    pub expansion_ratio: u64,
    /// Deepest nesting an archive may contain.
    pub nesting_depth: u32,
    /// Most resident memory a run may hold.
    pub resident_memory: u64,
    /// Most redirects one request may follow.
    pub redirects: u32,
    /// Largest manifest that will be read.
    pub manifest_size: u64,
    /// Most nodes one manifest may contain.
    pub manifest_nodes: u64,
    /// Attempts made per transient failure.
    pub retry_attempts: u32,
    /// Longest a backoff may wait.
    pub retry_ceiling: Duration,
    /// Most entries one listing may return.
    pub listing_entries: u64,
    /// Most candidate sources probed in parallel.
    pub probed_candidates: u32,
    /// Projected transfer time above which an optional credential is offered.
    pub credential_offer_threshold: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            archive_entries: 1_000_000,
            expanded_bytes: 1_099_511_627_776,
            expansion_ratio: 200,
            nesting_depth: 64,
            resident_memory: 1_073_741_824,
            redirects: 10,
            manifest_size: 16_777_216,
            manifest_nodes: 100_000,
            retry_attempts: 5,
            retry_ceiling: Duration::from_secs(60),
            listing_entries: 500_000,
            probed_candidates: 4,
            credential_offer_threshold: Duration::from_secs(120),
        }
    }
}

/// A ceiling on how fast a run may transfer, in bytes per second.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Bandwidth(NonZeroU64);

impl Bandwidth {
    /// Builds a ceiling from a rate in bytes per second.
    #[must_use]
    pub fn new(bytes_per_second: NonZeroU64) -> Self {
        Self(bytes_per_second)
    }

    /// Returns the rate in bytes per second.
    #[must_use]
    pub fn bytes_per_second(self) -> u64 {
        self.0.get()
    }
}
