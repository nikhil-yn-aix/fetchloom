//! What a partial file records about where its bytes came from.

use serde::{Deserialize, Serialize};

use crate::redact::SafeUrl;
use crate::resume::ResumeRung;
use crate::seam::source::SourceIdentity;

/// What a partial file records about the response its bytes came from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRecord {
    /// Where the bytes came from, redacted.
    pub location: SafeUrl,
    /// The host that served them.
    pub host: String,
    /// The length the source stated, when it stated one.
    pub size: Option<u64>,
    /// What the source said identifies the bytes.
    pub identity: SourceIdentity,
    /// The entity tag as it was received, when there was one.
    pub etag: Option<String>,
    /// The last modified value as it was received, when there was one.
    pub last_modified: Option<String>,
    /// Whether the source accepted a range.
    pub accepts_ranges: bool,
    /// How many bytes of the partial are known to have arrived.
    pub written: u64,
    /// The rung the transfer was on when the record was written.
    pub rung: ResumeRung,
}

impl SourceRecord {
    /// Reports whether a response identifies the same bytes this record does.
    #[must_use]
    pub fn identifies_the_same_bytes_as(&self, now: &SourceIdentity) -> bool {
        !matches!(self.identity, SourceIdentity::None) && &self.identity == now
    }
}
