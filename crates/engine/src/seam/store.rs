//! The Store seam: what a transfer needs of somewhere to put bytes it is still
//! fetching, and nothing else. Opening a finished object, its tree, staging,
//! pins, prune, status and the format fingerprint are asked of one store by
//! name, so they are inherent to it rather than part of this seam.

use std::io::Write;

use crate::digest::ContentDigest;
use crate::error::Error;
use crate::partial_key::PartialKey;
use crate::source_record::SourceRecord;

pub trait Store {
    type Writer: Write;
    type Lease: Send;

    /// # Errors
    /// `cache.corrupt` when the store cannot be asked. An object that is not
    /// held is `false` rather than an error.
    fn contains(&self, digest: ContentDigest) -> Result<bool, Error>;

    /// # Errors
    /// `cache.locked` when the lock file is replaced under every attempt,
    /// `cache.locking_unsupported` when the volume cannot express a lock, and
    /// `cache.corrupt` when the partial directory cannot be written.
    fn lease(&self, key: PartialKey) -> Result<Self::Lease, Error>;

    fn waited(&self, lease: &Self::Lease) -> bool;

    /// # Errors
    /// `resource.disk` when the length asked for does not fit, `cache.corrupt`
    /// when the partial cannot be created, and, where bytes are already on
    /// disk, `cache.corrupt` when they cannot be read back to rebuild the
    /// hasher.
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
    /// `cache.corrupt` when the partial or the outboard cannot be read.
    /// A prefix that does not check out is a shorter answer, not an error.
    fn verified_prefix(
        &self,
        key: PartialKey,
        digest: ContentDigest,
        on_disk: u64,
    ) -> Result<u64, Error>;
}
