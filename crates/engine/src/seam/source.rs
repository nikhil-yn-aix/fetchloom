//! The Source seam: probing, validators, ranges, and retry guidance.

use std::io::Read;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::credential::Credential;
use crate::degrade::Degradation;
use crate::digest::{ContentDigest, InteropDigest};
use crate::error::Error;
use crate::redact::SafeUrl;
use crate::reference::Host;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct ByteRange {
    pub start: u64,
    pub end: u64,
}

impl ByteRange {
    #[must_use]
    pub fn length(self) -> u64 {
        self.end.saturating_sub(self.start)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SourceIdentity {
    ContentAddress(ContentDigest),
    ImmutableVersion(String),
    StrongValidator(String),
    WeakValidator(String),
    None,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cost {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub egress_charged: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requester_pays: Option<bool>,
}

impl Cost {
    #[must_use]
    pub fn is_unknown(self) -> bool {
        self.egress_charged.is_none() && self.requester_pays.is_none()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SourceMetadata {
    pub location: SafeUrl,
    pub host: Host,
    pub size: Option<u64>,
    pub content: Option<ContentDigest>,
    pub interop: Option<InteropDigest>,
    pub identity: SourceIdentity,
    pub last_modified: Option<String>,
    pub supports_ranges: bool,
    pub time_to_first_byte: Duration,
    pub retry_after: Option<Duration>,
    pub cost: Cost,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ListingEntry {
    pub location: SafeUrl,
    pub path: String,
    pub size: Option<u64>,
    pub content: Option<ContentDigest>,
    pub interop: Option<InteropDigest>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Listing {
    pub entries: Vec<ListingEntry>,
    pub skipped: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Validator {
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

impl Validator {
    #[must_use]
    pub fn can_be_asked_with(&self) -> bool {
        self.etag.is_some() || self.last_modified.is_some()
    }
}

pub enum Revalidated<B> {
    Unchanged,
    Changed(Box<Served<B>>),
}

#[derive(Debug)]
pub struct Served<B> {
    pub metadata: SourceMetadata,
    pub body: B,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Serves {
    Object,
    Container,
}

pub trait Source {
    type Body: Read;

    fn serves(&self, reference: &str) -> Option<Serves>;

    fn take_degradations(&self) -> Vec<Degradation>;

    /// # Errors
    /// `network.refused`, `network.timeout`, `network.tls` or `network.status`
    /// as the source answers, `reference.unresolved` when the location names
    /// nothing, and `policy.credential_missing` or `policy.credential_invalid`
    /// when the source asks for one it did not get.
    fn probe(
        &self,
        location: &str,
        credential: Option<&Credential>,
    ) -> Result<SourceMetadata, Error>;

    /// `resuming` is the strong entity tag a rung three resume stands on, sent
    /// as `If-Range` so a changed source serves the whole object rather than
    /// appending to a partial of something else. `None` on every other rung.
    ///
    /// # Errors
    /// The kinds `probe` gives, `source.unsupported_range` when a range was
    /// asked for and the whole object was served, and `resource.limit` when
    /// the object is larger than the run allows.
    fn fetch(
        &self,
        location: &str,
        range: Option<ByteRange>,
        credential: Option<&Credential>,
        resuming: Option<&str>,
    ) -> Result<Served<Self::Body>, Error>;

    /// # Errors
    /// The kinds `fetch` gives. A validator the source no longer honors is a
    /// `Revalidated::Changed` answer rather than an error.
    fn revalidate(
        &self,
        location: &str,
        validator: &Validator,
        credential: Option<&Credential>,
    ) -> Result<Revalidated<Self::Body>, Error>;

    /// # Errors
    /// The kinds `probe` gives, and `reference.unresolved` when the location
    /// is not a container this source can list.
    fn list(&self, location: &str, credential: Option<&Credential>) -> Result<Listing, Error>;
}
