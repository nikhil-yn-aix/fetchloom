//! The Source seam: probing, validators, ranges, and retry guidance.

use std::io::Read;
use std::time::Duration;

use serde::Serialize;

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
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
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

/// Somewhere bytes can be fetched from.
pub trait Source {
    /// The bytes of an object, streamed.
    type Body: Read;

    /// Makes a bounded metadata request costing kilobytes.
    ///
    /// # Errors
    ///
    /// Fails when the source is unreachable, refuses the request, or requires
    /// a credential that was not supplied.
    fn probe(
        &self,
        location: &str,
        credential: Option<&Credential>,
    ) -> Result<SourceMetadata, Error>;

    /// Opens the bytes of an object, or of one span of it.
    ///
    /// Takes no span to fetch the whole object. Takes a span only when the
    /// source reported that it supports ranges.
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
    ) -> Result<Self::Body, Error>;

    /// Lists the entries at or below a prefix, without crawling.
    ///
    /// # Errors
    ///
    /// Fails when the container is unreachable, when its index is one
    /// Fetchloom does not recognize, and when the listing exceeds its limit.
    fn list(
        &self,
        location: &str,
        credential: Option<&Credential>,
    ) -> Result<Vec<ListingEntry>, Error>;
}
