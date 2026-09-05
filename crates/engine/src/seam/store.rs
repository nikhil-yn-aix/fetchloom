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

    /// # Errors
    /// `cache.corrupt` when the recorded fingerprint cannot be read.
    fn format_fingerprint(&self) -> Result<CacheFormatFingerprint, Error>;

    /// # Errors
    /// `cache.corrupt` when the store cannot be asked. An object that is not
    /// held is `false` rather than an error.
    fn contains(&self, digest: ContentDigest) -> Result<bool, Error>;

    /// # Errors
    /// `cache.corrupt` when the object is missing or unreadable, and
    /// `cache.locking_unsupported` when the read lease cannot be taken.
    fn open(&self, digest: ContentDigest) -> Result<Self::Reader, Error>;

    /// # Errors
    /// `cache.locked` when the lock file is replaced under every attempt,
    /// `cache.locking_unsupported` when the volume cannot express a lock, and
    /// `cache.corrupt` when the partial directory cannot be written.
    fn lease(&self, key: PartialKey) -> Result<Self::Lease, Error>;

    fn waited(&self, lease: &Self::Lease) -> bool;

    /// # Errors
    /// `resource.disk` when the length asked for does not fit, and
    /// `cache.corrupt` when the partial cannot be created.
    fn begin(&self, lease: &Self::Lease, length: u64) -> Result<Self::Writer, Error>;

    /// # Errors
    /// The kinds `begin` gives, and `cache.corrupt` when the bytes already on
    /// disk cannot be read back to rebuild the hasher.
    fn resume(&self, lease: &Self::Lease, length: u64, valid: u64) -> Result<Self::Writer, Error>;

    /// # Errors
    /// `cache.corrupt` when the record cannot be written, and `resource.disk`
    /// when the volume is full.
    fn record_source(&self, key: PartialKey, record: &SourceRecord) -> Result<(), Error>;

    /// # Errors
    /// `cache.corrupt` when a record exists and does not parse. No record is
    /// `None` rather than an error.
    fn recorded_source(&self, key: PartialKey) -> Result<Option<SourceRecord>, Error>;

    /// # Errors
    /// `cache.corrupt` when the partial exists and cannot be removed. A
    /// partial that is already gone is not an error.
    fn discard_partial(&self, key: PartialKey) -> Result<(), Error>;

    /// # Errors
    /// `cache.cross_volume` when the partial and the object directory are on
    /// different volumes, `resource.disk` when the volume is full, and
    /// `cache.corrupt` when the publication cannot be completed.
    fn commit(
        &self,
        lease: Self::Lease,
        writer: Self::Writer,
    ) -> Result<crate::hashing::Digests, Error>;

    /// # Errors
    /// `cache.corrupt` when the store cannot be asked.
    fn has_outboard(&self, digest: ContentDigest) -> Result<bool, Error>;

    /// # Errors
    /// `cache.corrupt` when the outboard is missing or unreadable.
    fn open_outboard(&self, digest: ContentDigest) -> Result<Self::Reader, Error>;

    /// # Errors
    /// `cache.corrupt` when the partial or the outboard cannot be read.
    /// A prefix that does not check out is a shorter answer, not an error.
    fn verified_prefix(
        &self,
        key: PartialKey,
        digest: ContentDigest,
        on_disk: u64,
    ) -> Result<u64, Error>;

    /// # Errors
    /// `resource.disk` when the volume is full, and `cache.corrupt` when the
    /// outboard cannot be written.
    fn write_outboard(&self, digest: ContentDigest, tree: &[u8]) -> Result<(), Error>;

    /// # Errors
    /// `cache.cross_volume` when the destination volume has no staging
    /// directory the cache can publish from, and `cache.corrupt` when the
    /// staging directory cannot be created.
    fn stage(&self, destination_volume: &std::path::Path) -> Result<PathBuf, Error>;

    /// # Errors
    /// `cache.corrupt` when the pin cannot be written, and `resource.disk`
    /// when the volume is full.
    fn pin(&self, digest: ContentDigest) -> Result<(), Error>;

    /// # Errors
    /// `cache.corrupt` when the pin exists and cannot be removed. A pin that
    /// is already gone is not an error.
    fn unpin(&self, digest: ContentDigest) -> Result<(), Error>;

    /// # Errors
    /// `cache.corrupt` when the object directory cannot be walked.
    fn list(&self) -> Result<Vec<ContentDigest>, Error>;

    /// # Errors
    /// `cache.corrupt` when the store cannot be walked. An object another user
    /// owns is skipped and counted, not an error.
    fn prune(&self, grace: Duration) -> Result<PruneReport, Error>;

    /// # Errors
    /// `cache.corrupt` when the store cannot be walked.
    fn status(&self) -> Result<CacheStatus, Error>;
}
