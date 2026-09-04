//! What a partial file records about where its bytes came from.

use serde::{Deserialize, Serialize};

use crate::redact::SafeUrl;
use crate::resume::ResumeRung;
use crate::seam::source::SourceIdentity;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRecord {
    pub location: SafeUrl,
    pub host: String,
    pub size: Option<u64>,
    pub identity: SourceIdentity,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub accepts_ranges: bool,
    pub written: u64,
    pub rung: ResumeRung,
}

impl SourceRecord {
    #[must_use]
    pub fn identifies_the_same_bytes_as(&self, now: &SourceIdentity) -> bool {
        !matches!(self.identity, SourceIdentity::None) && &self.identity == now
    }
}
