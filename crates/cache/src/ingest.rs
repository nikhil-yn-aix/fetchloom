//! Putting bytes into the cache whose digest is not known until they are read.

use std::io::{Read, Write};
use std::path::Path;

use fetchloom_engine::digest::{ContentDigest, InteropDigest};
use fetchloom_engine::error::{Error, Surface, filesystem_failure};
use fetchloom_engine::hashing::{self, Digests};
use fetchloom_engine::seam::platform::Platform;
use fetchloom_engine::seam::store::Store;
use serde::Serialize;

use crate::Cache;
use crate::record::{self, ObjectRecord};

use fetchloom_engine::limits::STREAM_BUFFER_BYTES as BUFFER;

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
}

impl<P: Platform> Cache<P> {
    /// Reads a file once, hashing it as it is written into the cache.
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
            });
        }
        let reading = std::fs::File::open(source)
            .map_err(|reason| filesystem_failure(Surface::Cache, source, &reason))?;
        self.ingest_from(reading, length, &|reason| {
            filesystem_failure(Surface::Cache, source, reason)
        })
    }

    /// Reads a file and returns what it hashes to and how long it is, writing
    /// nothing.
    fn digest_of(&self, source: &Path) -> Result<(Digests, u64), Error> {
        let mut reading = std::fs::File::open(source)
            .map_err(|reason| filesystem_failure(Surface::Cache, source, &reason))?;
        let mut pair = hashing::Pair::new();
        let mut buffer = vec![0u8; BUFFER];
        let mut length = 0u64;
        loop {
            let filled = reading
                .read(&mut buffer)
                .map_err(|reason| filesystem_failure(Surface::Cache, source, &reason))?;
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
    /// # Errors
    ///
    /// Fails when the file cannot be read, when the cache cannot be written,
    /// and when the volume has no room.
    pub fn adopt(&self, digests: &Digests, length: u64, source: &Path) -> Result<Ingested, Error> {
        let digest = digests.content;
        let present = Ingested {
            digest,
            interop: digests.interop,
            size: length,
            was_present: true,
        };
        if self.contains(digest)? {
            return Ok(present);
        }
        if length <= fetchloom_engine::limits::PACK_THRESHOLD {
            let bytes = std::fs::read(source)
                .map_err(|reason| filesystem_failure(Surface::Cache, source, &reason))?;
            self.work().read_bytes(bytes.len() as u64);
            self.pack_bytes(digest, digests.interop, &bytes)?;
            self.finish_publication(digests)?;
            return Ok(Ingested {
                digest,
                interop: digests.interop,
                size: length,
                was_present: false,
            });
        }
        let scratch = self.scratch_path();
        let _ = std::fs::remove_file(&scratch);
        self.platform().clone_or_copy(source, &scratch)?;
        let written = std::fs::OpenOptions::new()
            .write(true)
            .open(&scratch)
            .map_err(|reason| filesystem_failure(Surface::Cache, &scratch, &reason))?;
        self.platform().flush(&written, self.tier())?;
        drop(written);
        self.publish_scratch(&scratch, digests)?;
        Ok(Ingested {
            digest,
            interop: digests.interop,
            size: length,
            was_present: false,
        })
    }

    /// Returns a name inside the cache that only this writer writes.
    pub(crate) fn scratch_path(&self) -> std::path::PathBuf {
        self.layout().partial().join(format!(
            "{}-{}-{}.ingest",
            self.token().pid,
            self.token().start,
            SCRATCHES.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ))
    }

    fn publish_scratch(&self, scratch: &Path, digests: &Digests) -> Result<(), Error> {
        self.publish_object(scratch, digests)?;
        self.finish_publication(digests)
    }

    /// Returns the interop digest recorded for an object.
    ///
    /// # Errors
    ///
    /// Fails when the record is present and cannot be read or does not parse.
    pub fn recorded_interop(&self, digest: ContentDigest) -> Result<Option<InteropDigest>, Error> {
        if let Some(crate::storage::Placement::Packed { entry, .. }) = self.placement(digest) {
            return Ok(Some(entry.interop));
        }
        let record: Option<ObjectRecord> = record::read(&self.object_record(digest))?;
        Ok(record.map(|record| record.interop))
    }

    /// Reads a stream once, hashing it as it is written into the cache.
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
                .map_err(|reason| filesystem_failure(Surface::Cache, &scratch, &reason))?;
            pair.update(self.processor(), &buffer[..filled]);
            written += filled as u64;
            self.work().read_bytes(filled as u64);
            self.work().wrote_bytes(filled as u64);
        }
        let digests = pair.finish();
        let digest = digests.content;

        if self.contains(digest)? {
            drop(writing);
            let _ = std::fs::remove_file(&scratch);
            return Ok(Ingested {
                digest,
                interop: digests.interop,
                size: written,
                was_present: true,
            });
        }

        self.platform().flush(&writing, self.tier())?;
        drop(writing);
        self.publish_scratch(&scratch, &digests)?;

        Ok(Ingested {
            digest,
            interop: digests.interop,
            size: written,
            was_present: false,
        })
    }

    /// Records everything the cache knows about a published object that is not
    /// in its bytes.
    ///
    /// # Errors
    ///
    /// Fails when the object cannot be fingerprinted, when the record cannot be
    /// written, and when the tree cannot be written.
    pub(crate) fn finish_publication(&self, digests: &Digests) -> Result<(), Error> {
        let digest = digests.content;
        if self.is_packed(digest) {
            if let Some(tree) = digests.outboard.as_ref() {
                self.write_outboard(digest, tree.as_bytes())?;
            }
            let _ = std::fs::remove_file(self.layout().mark_of(digest));
            return Ok(());
        }
        let fingerprint = self.fingerprint_of(digest)?;
        let path = self.object_record(digest);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|reason| filesystem_failure(Surface::Cache, parent, &reason))?;
        }
        record::write(
            &path,
            &ObjectRecord::new(fingerprint, digests.interop),
            self.work(),
        )?;
        if let Some(tree) = digests.outboard.as_ref() {
            self.write_outboard(digest, tree.as_bytes())?;
        }
        let _ = std::fs::remove_file(self.layout().mark_of(digest));
        Ok(())
    }
}

/// Numbers the scratch name a writer takes, so that two writers in one process
/// never take the same one.
static SCRATCHES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
