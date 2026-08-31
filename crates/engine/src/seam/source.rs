//! The Source seam: probing, validators, ranges, and retry guidance.

use std::io::Read;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::credential::Credential;
use crate::digest::{ContentDigest, InteropDigest};
use crate::error::Error;
use crate::redact::SafeUrl;
use crate::reference::Host;

/// A half-open span of bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct ByteRange {
    /// The first byte of the span.
    pub start: u64,
    /// One past the last byte of the span.
    pub end: u64,
}

impl ByteRange {
    /// Returns how many bytes the span covers.
    #[must_use]
    pub fn length(self) -> u64 {
        self.end.saturating_sub(self.start)
    }
}

/// What a source says identifies the bytes it is serving.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SourceIdentity {
    /// The source addresses the bytes by their content.
    ContentAddress(ContentDigest),
    /// The source names an immutable version of the object.
    ImmutableVersion(String),
    /// The source supplies a validator that changes whenever the bytes do.
    StrongValidator(String),
    /// The source supplies a validator that may not change when the bytes do.
    WeakValidator(String),
    /// The source supplies nothing that identifies the bytes.
    None,
}

/// What a bounded metadata request learned about an object.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SourceMetadata {
    /// Where the object is, redacted when it was recorded.
    pub location: SafeUrl,
    /// The host serving it.
    pub host: Host,
    /// The length in bytes, when the source states it.
    pub size: Option<u64>,
    /// The content digest the source claims, when it claims one.
    pub content: Option<ContentDigest>,
    /// The interop digest the source claims, when it claims one.
    pub interop: Option<InteropDigest>,
    /// What identifies the bytes the source is serving.
    pub identity: SourceIdentity,
    /// The last modified value the source gave, when it gave one.
    pub last_modified: Option<String>,
    /// Whether the source can serve part of the object.
    pub supports_ranges: bool,
    /// How long the metadata request took.
    pub time_to_first_byte: Duration,
    /// How long the source asked to be left alone for, when it asked.
    pub retry_after: Option<Duration>,
}

/// One entry a listing returned.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ListingEntry {
    /// Where the entry is, redacted when it was recorded.
    pub location: SafeUrl,
    /// The entry's path relative to the listed prefix.
    pub path: String,
    /// The length in bytes, when the listing states it.
    pub size: Option<u64>,
}

/// What a run recorded that a conditional request can be built from.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Validator {
    /// The entity tag the source gave when the bytes were fetched.
    pub etag: Option<String>,
    /// The last modified value it gave.
    pub last_modified: Option<String>,
}

impl Validator {
    /// Reports whether there is anything here to ask with.
    #[must_use]
    pub fn can_be_asked_with(&self) -> bool {
        self.etag.is_some() || self.last_modified.is_some()
    }
}

/// What one conditional request learned.
pub enum Revalidated<B> {
    /// The source restated the validator it already gave.
    Unchanged,
    /// The bytes are different, and the response carries them.
    Changed(Box<Served<B>>),
}

/// What a source answered with: the bytes, and what the response said about
/// them, including the location that answered.
#[derive(Debug)]
pub struct Served<B> {
    /// What the response said about the object.
    pub metadata: SourceMetadata,
    /// The bytes, from the first one.
    pub body: B,
}

/// Somewhere bytes can be fetched from.
pub trait Source {
    /// The bytes of an object, streamed.
    type Body: Read;

    /// Makes a bounded metadata request costing kilobytes.
    ///
    /// # Errors
    ///
    /// Fails when the source is unreachable, refuses the request, or requires a
    /// credential that was not supplied.
    fn probe(
        &self,
        location: &str,
        credential: Option<&Credential>,
    ) -> Result<SourceMetadata, Error>;

    /// Opens the bytes of an object, or of one span of it.
    ///
    /// # Errors
    ///
    /// Fails when the source is unreachable, refuses the request, cannot serve
    /// the span, or serves an object whose identity has changed.
    fn fetch(
        &self,
        location: &str,
        range: Option<ByteRange>,
        credential: Option<&Credential>,
    ) -> Result<Served<Self::Body>, Error>;

    /// Asks whether an object a run already holds is still what a reference
    /// names, in one request.
    ///
    /// # Errors
    ///
    /// Fails when the source is unreachable or refuses the request, exactly as
    /// a fetch does.
    fn revalidate(
        &self,
        location: &str,
        validator: &Validator,
        credential: Option<&Credential>,
    ) -> Result<Revalidated<Self::Body>, Error>;

    /// Lists the entries at or below a prefix, without crawling.
    ///
    /// # Errors
    ///
    /// Fails when the container is unreachable, when its index is one Fetchloom
    /// does not recognize, and when the listing exceeds its limit.
    fn list(
        &self,
        location: &str,
        credential: Option<&Credential>,
    ) -> Result<Vec<ListingEntry>, Error>;
}
