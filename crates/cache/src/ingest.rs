//! Putting bytes into the cache whose digest is not known until they are read.
//!
//! A transfer named by a manifest knows its digest before it starts and takes a
//! lease on it. A local source does not, so the bytes are written once, hashed
//! as they arrive, and given their name at the end.

use std::io::{Read, Write};
use std::path::Path;

use fetchloom_engine::digest::ContentDigest;
use fetchloom_engine::error::{Error, ErrorKind};
use fetchloom_engine::partial_key::PartialKey;
use fetchloom_engine::seam::platform::{OwnerToken, Platform};
use fetchloom_engine::seam::store::Store;
use serde::Serialize;

use crate::record::{self, RecordedFingerprint};
use crate::{Cache, failure, seal_object};

/// How large a buffer the ingest reads through.
const BUFFER: usize = 1 << 20;

/// What one ingest did.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Ingested {
    /// The digest the bytes hash to.
    pub digest: ContentDigest,
    /// How many bytes they are.
    pub size: u64,
    /// Whether the cache already held them.
    pub was_present: bool,
    /// The writer this ingest waited for, when another process held the digest.
    pub waited_for: Option<OwnerToken>,
}

impl<P: Platform> Cache<P> {
    /// Reads a file once, hashing it as it is written into the cache.
    ///
    /// Returns the digest the bytes hash to, their length, and whether the
    /// cache already held them. The bytes are written to a name of this
    /// process's own and given the digest's name only once the digest is known,
    /// so nothing appears under a digest it does not hash to.
    ///
    /// # Errors
    ///
    /// Fails when the source cannot be read, when the cache cannot be written,
    /// and when the volume has no room.
    pub fn ingest(&self, source: &Path) -> Result<Ingested, Error> {
        let reading = std::fs::File::open(source)
            .map_err(|reason| failure(ErrorKind::CacheCorrupt, source, &reason))?;
        let length = reading
            .metadata()
            .map_err(|reason| failure(ErrorKind::CacheCorrupt, source, &reason))?
            .len();
        self.ingest_from(reading, length, &|reason| {
            failure(ErrorKind::CacheCorrupt, source, reason)
        })
    }

    /// Reads a stream once, hashing it as it is written into the cache.
    ///
    /// Takes the bytes, the length to reserve, which may be zero when the source
    /// did not state one, and what a failure to read those bytes means, which
    /// only the caller knows. Returns the digest the bytes hash to, their
    /// length, and whether the cache already held them.
    ///
    /// # Errors
    ///
    /// Fails when the stream cannot be read, when the cache cannot be written,
    /// and when the volume has no room.
    pub fn ingest_from(
        &self,
        reading: impl Read,
        length: u64,
        read_failure: &dyn Fn(&std::io::Error) -> Error,
    ) -> Result<Ingested, Error> {
        let mut reading = reading;
        let scratch = self.layout().partial().join(format!(
            "{}-{}.ingest",
            self.token().pid,
            self.token().start
        ));
        let _ = std::fs::remove_file(&scratch);
        let mut writing = self.platform().create_file_exclusive(&scratch)?;
        self.platform().preallocate(&writing, length)?;

        let mut hasher = blake3::Hasher::new();
        let mut buffer = vec![0u8; BUFFER];
        let mut written = 0u64;
        loop {
            let filled = reading
                .read(&mut buffer)
                .map_err(|reason| read_failure(&reason))?;
            if filled == 0 {
                break;
            }
            writing
                .write_all(&buffer[..filled])
                .map_err(|reason| failure(ErrorKind::CacheCorrupt, &scratch, &reason))?;
            hasher.update(&buffer[..filled]);
            written += filled as u64;
            self.work().read_bytes(filled as u64);
            self.work().wrote_bytes(filled as u64);
        }
        let digest = ContentDigest::from_bytes(*hasher.finalize().as_bytes());

        let lease = self.lease(PartialKey::of_content(digest))?;
        let waited_for = lease.waited_for().cloned();
        if self.contains(digest)? {
            drop(lease);
            let _ = std::fs::remove_file(&scratch);
            return Ok(Ingested {
                digest,
                size: written,
                was_present: true,
                waited_for,
            });
        }

        self.platform().flush(&writing, self.tier())?;
        drop(writing);

        let object = self.layout().object(digest);
        self.platform()
            .publish_file(&scratch, &object, self.tier())?;
        seal_object(&object)?;
        self.record_fingerprint(digest)?;
        drop(lease);

        Ok(Ingested {
            digest,
            size: written,
            was_present: false,
            waited_for,
        })
    }

    /// Records the fingerprint of a published object.
    pub(crate) fn record_fingerprint(&self, digest: ContentDigest) -> Result<(), Error> {
        let object = self.layout().object(digest);
        let fingerprint = self.platform().fingerprint(&object)?;
        let path = self.fingerprint_record(digest);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|reason| failure(ErrorKind::CacheCorrupt, parent, &reason))?;
        }
        record::write(&path, &RecordedFingerprint::from(fingerprint))
    }
}
