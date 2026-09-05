//! The bounds no run may exceed.

use std::num::NonZeroU64;
use std::time::Duration;

use serde::{Deserialize, Serialize};

pub const OUTBOARD_CHUNK_GROUP: u64 = 1_048_576;

pub const OUTBOARD_THRESHOLD: u64 = 67_108_864;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limits {
    pub archive_entries: u64,
    pub expanded_bytes: u64,
    pub expansion_ratio: u64,
    pub nesting_depth: u32,
    pub resident_memory: u64,
    pub redirects: u32,
    pub manifest_size: u64,
    pub manifest_nodes: u64,
    pub record_size: u64,
    pub record_nodes: u64,
    pub retry_attempts: u32,
    pub retry_ceiling: Duration,
    pub listing_entries: u64,
    pub listing_bytes: u64,
    pub probed_candidates: u32,
    pub credential_offer_threshold: Duration,
    pub connections_per_host: usize,
    pub connect_timeout: Duration,
    pub response_timeout: Duration,
    pub idle_timeout: Duration,
    pub idle_connection_age: Duration,
    pub repair_spans: u64,
    pub split_threshold: u64,
    pub repair_whole_percent: u64,
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
            record_size: 268_435_456,
            record_nodes: 8_000_000,
            retry_attempts: 5,
            retry_ceiling: Duration::from_secs(60),
            listing_entries: 500_000,
            listing_bytes: 16_777_216,
            probed_candidates: 4,
            credential_offer_threshold: Duration::from_secs(120),
            connections_per_host: 4,
            connect_timeout: Duration::from_secs(10),
            response_timeout: Duration::from_secs(30),
            idle_timeout: Duration::from_secs(30),
            idle_connection_age: Duration::from_secs(60),
            repair_spans: 64,
            split_threshold: OUTBOARD_THRESHOLD,
            repair_whole_percent: 50,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Bandwidth(NonZeroU64);

impl Bandwidth {
    #[must_use]
    pub fn new(bytes_per_second: NonZeroU64) -> Self {
        Self(bytes_per_second)
    }

    #[must_use]
    pub fn bytes_per_second(self) -> u64 {
        self.0.get()
    }
}

impl std::fmt::Display for Bandwidth {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

impl std::str::FromStr for Bandwidth {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let refused = || {
            format!(
                "write {text} as a count of bytes per second, with an optional k, m or g for a \
                 multiple of 1024, because a rate is read in one form and no other"
            )
        };
        let (digits, scale) = match text.as_bytes().last() {
            Some(b'k' | b'K') => (&text[..text.len() - 1], 1024),
            Some(b'm' | b'M') => (&text[..text.len() - 1], 1024 * 1024),
            Some(b'g' | b'G') => (&text[..text.len() - 1], 1024 * 1024 * 1024),
            _ => (text, 1),
        };
        let count: u64 = digits.parse().map_err(|_| refused())?;
        let rate = count.checked_mul(scale).ok_or_else(refused)?;
        NonZeroU64::new(rate).map(Self).ok_or_else(refused)
    }
}

pub const PACK_THRESHOLD: u64 = OUTBOARD_CHUNK_GROUP;

pub const STREAM_BUFFER_BYTES: usize = 1 << 20;

pub const COMPRESSION_FRAME_BYTES: u64 = OUTBOARD_CHUNK_GROUP;

const _: () = assert!(
    COMPRESSION_FRAME_BYTES == OUTBOARD_CHUNK_GROUP
        && COMPRESSION_FRAME_BYTES == PACK_THRESHOLD
        && COMPRESSION_FRAME_BYTES == STREAM_BUFFER_BYTES as u64,
    "a compression frame covers exactly one outboard chunk group, one packed object and one stream buffer, so a ranged verify or a localized repair decompresses the frames covering the range it asked for and no others"
);
