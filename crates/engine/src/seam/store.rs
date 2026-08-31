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

/// What one prune run did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct PruneReport {
    /// How many objects were removed.
    pub removed: u64,
    /// How many bytes those objects held.
    pub bytes_removed: u64,
    /// How many objects were kept, being pinned, leased, or referenced.
    pub kept: u64,
    /// How many objects another user created and were kept.
    pub skipped_other_owner: u64,
    /// How many quarantined objects were removed.
    pub quarantined_removed: u64,
}

/// What the cache currently holds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CacheStatus {
    /// Where the cache directory is.
    pub root: PathBuf,
    /// How many completed objects it holds.
    pub objects: u64,
    /// How many bytes those objects hold.
    pub bytes: u64,
    /// How many in-progress transfers it holds.
    pub partials: u64,
    /// How many pin records it holds.
    pub pins: u64,
    /// How many objects failed verification and are kept for diagnosis.
    pub quarantined: u64,
}

/// Content-addressed storage that one run reads from and writes to.
pub trait Store {
    /// A completed object opened for reading, seekable.
    type Reader: Read + Seek;
    /// An in-progress object opened for writing.
    type Writer: Write;
    /// A held single-writer claim on one key. Releasing it is dropping it.
    type Lease: Send;

    /// Returns the fingerprint of the cache format on disk.
    ///
    /// # Errors
    ///
    /// Fails when the fingerprint cannot be read.
    fn format_fingerprint(&self) -> Result<CacheFormatFingerprint, Error>;

    /// Reports whether a completed object is present.
    ///
    /// # Errors
    ///
    /// Fails when the cache cannot be read.
    fn contains(&self, digest: ContentDigest) -> Result<bool, Error>;

    /// Opens a completed object at its first byte.
    ///
    /// # Errors
    ///
    /// Fails when the object is absent, when the cache format does not match,
    /// and when the recorded fingerprint no longer matches the file on disk.
    fn open(&self, digest: ContentDigest) -> Result<Self::Reader, Error>;

    /// Takes the single-writer claim on one key, waiting for another writer
    /// and reusing its result rather than starting a second transfer.
    ///
    /// # Errors
    ///
    /// Fails when the volume cannot express advisory locking and when the wait
    /// ends without the claim.
    fn lease(&self, key: PartialKey) -> Result<Self::Lease, Error>;

    /// Reports whether this lease waited for another writer to finish.
    ///
    /// Takes a lease this store handed out. Returns true when the claim was
    /// held by someone else and this one waited for it, which is the fact a
    /// run reports with `cache.wait`.
    fn waited(&self, lease: &Self::Lease) -> bool;

    /// Opens an in-progress object for writing, reserving its full length.
    ///
    /// # Errors
    ///
    /// Fails when the volume has no room and when the claim is not held.
    fn begin(&self, lease: &Self::Lease, length: u64) -> Result<Self::Writer, Error>;

    /// Opens an in-progress object for writing, keeping the first `valid` bytes
    /// and hashing them back into the digest as it goes.
    ///
    /// Takes the claim, the length the source stated, and how many bytes of the
    /// partial are known to have arrived. Discards anything past `valid`.
    ///
    /// # Errors
    ///
    /// Fails when the partial cannot be read back, when the volume has no room,
    /// and when the claim is not held.
    fn resume(&self, lease: &Self::Lease, length: u64, valid: u64) -> Result<Self::Writer, Error>;

    /// Records where the bytes of an in-progress object came from.
    ///
    /// # Errors
    ///
    /// Fails when the record cannot be written beside the partial.
    fn record_source(&self, key: PartialKey, record: &SourceRecord) -> Result<(), Error>;

    /// Reads what a partial recorded about where its bytes came from.
    ///
    /// Returns nothing when no partial exists or none was recorded.
    ///
    /// # Errors
    ///
    /// Fails when a record exists and cannot be read.
    fn recorded_source(&self, key: PartialKey) -> Result<Option<SourceRecord>, Error>;

    /// Discards an in-progress object and everything recorded beside it.
    ///
    /// # Errors
    ///
    /// Fails when the partial exists and cannot be removed.
    fn discard_partial(&self, key: PartialKey) -> Result<(), Error>;

    /// Publishes an in-progress object as a completed one.
    ///
    /// Takes the claim and the writer. Returns the digest the written bytes
    /// hash to, which is the object's name.
    ///
    /// # Errors
    ///
    /// Fails when the claim states a digest the bytes do not hash to, when
    /// the two directories are on different volumes, and when the rename
    /// does not complete.
    fn commit(
        &self,
        lease: Self::Lease,
        writer: Self::Writer,
    ) -> Result<crate::hashing::Digests, Error>;

    /// Reports whether an outboard tree is stored for an object.
    ///
    /// Takes no lock, for the same reason asking whether an object is present
    /// takes none: the answer is only ever a reason to open the tree, and
    /// opening it takes the lease and looks again.
    ///
    /// # Errors
    ///
    /// Fails when the cache cannot be read.
    fn has_outboard(&self, digest: ContentDigest) -> Result<bool, Error>;

    /// Opens the outboard tree for an object.
    ///
    /// # Errors
    ///
    /// Fails when the object is below the outboard threshold, when no outboard
    /// is stored, and when the cache cannot be read.
    fn open_outboard(&self, digest: ContentDigest) -> Result<Self::Reader, Error>;

    /// Returns how many bytes at the start of a partial transfer match the
    /// object's outboard tree.
    ///
    /// Takes the key the partial is named by, the digest the run expects, and
    /// how many bytes are recorded as having arrived. Returns the offset of the
    /// first leaf group that is bad or missing, which is where a resume on the
    /// first rung appends. The answer is never past the last group boundary at
    /// or below what is on disk.
    ///
    /// # Errors
    ///
    /// Fails when the tree cannot be read and when it does not check out
    /// against the digest.
    fn verified_prefix(
        &self,
        key: PartialKey,
        digest: ContentDigest,
        on_disk: u64,
    ) -> Result<u64, Error>;

    /// Stores the outboard tree for an object.
    ///
    /// # Errors
    ///
    /// Fails when the cache cannot be written.
    fn write_outboard(&self, digest: ContentDigest, tree: &[u8]) -> Result<(), Error>;

    /// Creates a staging directory on the volume a destination is on.
    ///
    /// # Errors
    ///
    /// Fails when the volume cannot be written to.
    fn stage(&self, destination_volume: &std::path::Path) -> Result<PathBuf, Error>;

    /// Marks an object as never removable by prune.
    ///
    /// # Errors
    ///
    /// Fails when the object is absent and when the cache cannot be written.
    fn pin(&self, digest: ContentDigest) -> Result<(), Error>;

    /// Removes a mark that kept an object from being pruned.
    ///
    /// # Errors
    ///
    /// Fails when no such mark exists and when the cache cannot be written.
    fn unpin(&self, digest: ContentDigest) -> Result<(), Error>;

    /// Lists every completed object.
    ///
    /// # Errors
    ///
    /// Fails when the cache cannot be read.
    fn list(&self) -> Result<Vec<ContentDigest>, Error>;

    /// Removes objects that are neither pinned, leased, nor referenced, after
    /// a grace period.
    ///
    /// # Errors
    ///
    /// Fails when the cache cannot be written.
    fn prune(&self, grace: Duration) -> Result<PruneReport, Error>;

    /// Reports what the cache holds.
    ///
    /// # Errors
    ///
    /// Fails when the cache cannot be read.
    fn status(&self) -> Result<CacheStatus, Error>;
}
