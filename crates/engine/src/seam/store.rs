//! The Store seam: content-addressed objects, partials, staging, and leases.

use std::io::{Read, Seek, Write};
use std::path::PathBuf;
use std::time::Duration;

use serde::Serialize;

use crate::digest::ContentDigest;
use crate::error::Error;
use crate::identity::CacheFormatFingerprint;
use crate::partial_key::PartialKey;
use crate::source_record::SourceRecord;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct PruneReport {
    pub removed: u64,
    pub bytes_removed: u64,
    pub kept: u64,
    pub skipped_other_owner: u64,
    pub quarantined_removed: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CacheStatus {
    pub root: PathBuf,
    pub objects: u64,
    pub bytes: u64,
    pub partials: u64,
    pub pins: u64,
    pub quarantined: u64,
}

pub trait Store {
    type Reader: Read + Seek;
    type Writer: Write;
    type Lease: Send;

    fn format_fingerprint(&self) -> Result<CacheFormatFingerprint, Error>;

    fn contains(&self, digest: ContentDigest) -> Result<bool, Error>;

    fn open(&self, digest: ContentDigest) -> Result<Self::Reader, Error>;

    fn lease(&self, key: PartialKey) -> Result<Self::Lease, Error>;

    fn waited(&self, lease: &Self::Lease) -> bool;

    fn begin(&self, lease: &Self::Lease, length: u64) -> Result<Self::Writer, Error>;

    fn resume(&self, lease: &Self::Lease, length: u64, valid: u64) -> Result<Self::Writer, Error>;

    fn record_source(&self, key: PartialKey, record: &SourceRecord) -> Result<(), Error>;

    fn recorded_source(&self, key: PartialKey) -> Result<Option<SourceRecord>, Error>;

    fn discard_partial(&self, key: PartialKey) -> Result<(), Error>;

    fn commit(
        &self,
        lease: Self::Lease,
        writer: Self::Writer,
    ) -> Result<crate::hashing::Digests, Error>;

    fn has_outboard(&self, digest: ContentDigest) -> Result<bool, Error>;

    fn open_outboard(&self, digest: ContentDigest) -> Result<Self::Reader, Error>;

    fn verified_prefix(
        &self,
        key: PartialKey,
        digest: ContentDigest,
        on_disk: u64,
    ) -> Result<u64, Error>;

    fn write_outboard(&self, digest: ContentDigest, tree: &[u8]) -> Result<(), Error>;

    fn stage(&self, destination_volume: &std::path::Path) -> Result<PathBuf, Error>;

    fn pin(&self, digest: ContentDigest) -> Result<(), Error>;

    fn unpin(&self, digest: ContentDigest) -> Result<(), Error>;

    fn list(&self) -> Result<Vec<ContentDigest>, Error>;

    fn prune(&self, grace: Duration) -> Result<PruneReport, Error>;

    fn status(&self) -> Result<CacheStatus, Error>;
}
