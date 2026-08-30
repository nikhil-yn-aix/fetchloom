//! Putting bytes into the cache whose digest is not known until they are read.
//!
//! A transfer named by a manifest knows its digest before it starts and takes a
//! lease on it. A local source does not, so the bytes are written once, hashed
//! as they arrive, and given their name at the end.

use std::io::{Read, Write};
use std::path::Path;

use fetchloom_engine::digest::{ContentDigest, InteropDigest};
use fetchloom_engine::error::{Error, ErrorKind};
use fetchloom_engine::hashing;
use fetchloom_engine::partial_key::PartialKey;
use fetchloom_engine::seam::platform::{OwnerToken, Platform};
use fetchloom_engine::seam::store::Store;
use serde::Serialize;

use crate::record::{self, ObjectRecord};
use crate::{Cache, failure, seal_object};

/// How large a buffer the ingest reads through.
const BUFFER: usize = 1 << 20;

/// What one ingest did.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Ingested {
    /// The digest the bytes hash to.
    pub digest: ContentDigest,
    /// The interop digest of the same bytes, taken in the same pass.
    pub interop: InteropDigest,
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
        let (digests, length) = self.digest_of(source)?;
        if self.contains(digests.content)? {
            return Ok(Ingested {
                digest: digests.content,
                interop: digests.interop,
                size: length,
                was_present: true,
                waited_for: None,
            });
        }
        let reading = std::fs::File::open(source)
            .map_err(|reason| failure(ErrorKind::CacheCorrupt, source, &reason))?;
        self.ingest_from(reading, length, &|reason| {
            failure(ErrorKind::CacheCorrupt, source, reason)
        })
    }

    /// Reads a file and returns what it hashes to and how long it is, writing
    /// nothing.
    ///
    /// A file already in the cache must cost the cache no write at all, and
    /// only its digest says whether it is. The cost is that a file the cache
    /// does not hold is read twice, once to learn its name and once to store
    /// it; the alternative was writing every byte of every file the cache
    /// already held, on every run.
    fn digest_of(&self, source: &Path) -> Result<(hashing::Digests, u64), Error> {
        let mut reading = std::fs::File::open(source)
            .map_err(|reason| failure(ErrorKind::CacheCorrupt, source, &reason))?;
        let mut pair = hashing::Pair::new();
        let mut buffer = vec![0u8; BUFFER];
        let mut length = 0u64;
        loop {
            let filled = reading
                .read(&mut buffer)
                .map_err(|reason| failure(ErrorKind::CacheCorrupt, source, &reason))?;
            if filled == 0 {
                break;
            }
            pair.update(self.processor(), &buffer[..filled]);
            length += filled as u64;
            self.work().read_bytes(filled as u64);
        }
        Ok((pair.finish(), length))
    }

    /// Puts a file whose digest is already known into the cache.
    ///
    /// Takes the digest the file's bytes hash to, its length, and where it
    /// sits. Returns what the cache did with it. The bytes are cloned
    /// where the volume can share blocks and copied where it cannot, so a
    /// caller that had to write the file anyway pays no second read of the
    /// source to fill the cache, and pays nothing at all when the cache
    /// already holds the object.
    ///
    /// The caller states the digest, so the caller is what makes it true. This
    /// is for a file this process just wrote and hashed in the same pass, not
    /// for one whose digest was taken on trust.
    ///
    /// # Errors
    ///
    /// Fails when the file cannot be read, when the cache cannot be written,
    /// and when the volume has no room.
    pub fn adopt(
        &self,
        digests: hashing::Digests,
        length: u64,
        source: &Path,
    ) -> Result<Ingested, Error> {
        let digest = digests.content;
        let present = |waited_for| Ingested {
            digest,
            interop: digests.interop,
            size: length,
            was_present: true,
            waited_for,
        };
        if self.contains(digest)? {
            return Ok(present(None));
        }
        let scratch = self.scratch_path();
        let _ = std::fs::remove_file(&scratch);
        self.platform().clone_or_copy(source, &scratch)?;

        let lease = self.lease(PartialKey::of_content(digest))?;
        let waited_for = lease.waited_for().cloned();
        if self.contains(digest)? {
            drop(lease);
            let _ = std::fs::remove_file(&scratch);
            return Ok(present(waited_for));
        }
        self.work().wrote_bytes(length);
        let written = std::fs::OpenOptions::new()
            .write(true)
            .open(&scratch)
            .map_err(|reason| failure(ErrorKind::CacheCorrupt, &scratch, &reason))?;
        self.platform().flush(&written, self.tier())?;
        drop(written);
        self.publish_scratch(&scratch, digest, digests.interop)?;
        drop(lease);
        Ok(Ingested {
            digest,
            interop: digests.interop,
            size: length,
            was_present: false,
            waited_for,
        })
    }

    fn scratch_path(&self) -> std::path::PathBuf {
        self.layout().partial().join(format!(
            "{}-{}.ingest",
            self.token().pid,
            self.token().start
        ))
    }

    fn publish_scratch(
        &self,
        scratch: &Path,
        digest: ContentDigest,
        interop: InteropDigest,
    ) -> Result<(), Error> {
        let object = self.layout().object(digest);
        self.platform()
            .publish_file(scratch, &object, self.tier())?;
        seal_object(&object)?;
        self.record_object(digest, interop)
    }

    /// Returns the interop digest recorded for an object.
    ///
    /// Returns nothing when the cache holds no record for the object, which is
    /// what a cache that never held it holds.
    ///
    /// # Errors
    ///
    /// Fails when the record is present and cannot be read or does not parse.
    pub fn recorded_interop(&self, digest: ContentDigest) -> Result<Option<InteropDigest>, Error> {
        let record: Option<ObjectRecord> = record::read(&self.object_record(digest))?;
        Ok(record.map(|record| record.interop))
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
        let scratch = self.scratch_path();
        let _ = std::fs::remove_file(&scratch);
        let mut writing = self.platform().create_file_exclusive(&scratch)?;
        self.platform().preallocate(&writing, length)?;

        let mut pair = hashing::Pair::new();
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
            pair.update(self.processor(), &buffer[..filled]);
            written += filled as u64;
            self.work().read_bytes(filled as u64);
            self.work().wrote_bytes(filled as u64);
        }
        let digests = pair.finish();
        let digest = digests.content;

        let lease = self.lease(PartialKey::of_content(digest))?;
        let waited_for = lease.waited_for().cloned();
        if self.contains(digest)? {
            drop(lease);
            let _ = std::fs::remove_file(&scratch);
            return Ok(Ingested {
                digest,
                interop: digests.interop,
                size: written,
                was_present: true,
                waited_for,
            });
        }

        self.platform().flush(&writing, self.tier())?;
        drop(writing);

        self.publish_scratch(&scratch, digest, digests.interop)?;
        drop(lease);

        Ok(Ingested {
            digest,
            interop: digests.interop,
            size: written,
            was_present: false,
            waited_for,
        })
    }

    /// Records everything the cache knows about a published object.
    ///
    /// One record rather than one per fact: holding an object costs what it
    /// takes in files rather than in bytes, and a second record is a second
    /// create, a second write, and a second name in a directory that already
    /// holds one per object.
    ///
    /// # Errors
    ///
    /// Fails when the object cannot be fingerprinted or the record cannot be
    /// written.
    pub(crate) fn record_object(
        &self,
        digest: ContentDigest,
        interop: InteropDigest,
    ) -> Result<(), Error> {
        let object = self.layout().object(digest);
        let fingerprint = self.platform().fingerprint(&object)?;
        let path = self.object_record(digest);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|reason| failure(ErrorKind::CacheCorrupt, parent, &reason))?;
        }
        record::write(&path, &ObjectRecord::new(fingerprint, interop))
    }
}
