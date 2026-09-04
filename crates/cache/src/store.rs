//! Objects, partials, leases, pins, and the publication that puts an object in
//! `objects/`.

use std::io::{Read, Seek, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use fetchloom_engine::digest::ContentDigest;
use fetchloom_engine::error::{Error, ErrorKind, Surface, filesystem_failure};
use fetchloom_engine::hashing;
use fetchloom_engine::identity::CacheFormatFingerprint;
use fetchloom_engine::partial_key::PartialKey;
use fetchloom_engine::pool::Processor;
use fetchloom_engine::seam::platform::{OwnerToken, Platform};
use fetchloom_engine::seam::policy::IoMode;
use fetchloom_engine::seam::store::{CacheStatus, PruneReport, Store};
use fetchloom_engine::source_record::SourceRecord;
use fetchloom_engine::verification::VerificationPolicy;
use fetchloom_engine::work::WorkCounter;

use crate::layout::{digest_of, name_of};
use crate::record::{self, ObjectRecord};
use crate::{Cache, owner_record_of, source_record_of};

use fetchloom_engine::limits::STREAM_BUFFER_BYTES as RESUME_BUFFER_BYTES;

#[derive(Debug)]
pub struct ObjectReader<L> {
    bytes: crate::storage::Bytes,
    lease: L,
}

impl<L> ObjectReader<L> {
    pub fn lease(&self) -> &L {
        &self.lease
    }
}

impl<L> Read for ObjectReader<L> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.bytes.read(buffer)
    }
}

impl<L> Seek for ObjectReader<L> {
    fn seek(&mut self, to: std::io::SeekFrom) -> std::io::Result<u64> {
        self.bytes.seek(to)
    }
}

pub struct PartialWriter {
    file: std::fs::File,
    path: PathBuf,
    pair: hashing::Pair,
    written: u64,
    work: Arc<WorkCounter>,
    processor: Arc<Processor>,
}

impl PartialWriter {
    #[must_use]
    pub fn written(&self) -> u64 {
        self.written
    }
}

impl Write for PartialWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let taken = self.file.write(bytes)?;
        self.pair.update(&self.processor, &bytes[..taken]);
        self.written += taken as u64;
        self.work.wrote_bytes(taken as u64);
        Ok(taken)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

#[derive(Debug)]
pub struct WriteLease<L> {
    key: PartialKey,
    waited_for: Option<OwnerToken>,
    lock: L,
}

impl<L> WriteLease<L> {
    #[must_use]
    pub fn key(&self) -> PartialKey {
        self.key
    }

    #[must_use]
    pub fn waited_for(&self) -> Option<&OwnerToken> {
        self.waited_for.as_ref()
    }

    pub fn lock(&self) -> &L {
        &self.lock
    }
}

impl<P: Platform> Cache<P> {
    pub fn read_lease(&self, digest: ContentDigest) -> Result<P::Lock, Error> {
        self.platform.lock_shared(&self.layout.lock_of(digest))
    }

    fn present(&self, digest: ContentDigest) -> bool {
        self.holds(digest)
    }

    pub fn check_hit(&self, digest: ContentDigest) -> Result<(), Error> {
        self.check(digest)
    }

    fn check(&self, digest: ContentDigest) -> Result<(), Error> {
        match self.policy {
            VerificationPolicy::Never => Ok(()),
            VerificationPolicy::Fingerprint => self.check_fingerprint(digest),
            VerificationPolicy::Always => self.check_bytes(digest),
        }
    }

    fn check_fingerprint(&self, digest: ContentDigest) -> Result<(), Error> {
        if self.is_packed(digest) {
            return self.check_bytes(digest);
        }
        let recorded: ObjectRecord =
            record::read(&self.object_record(digest))?.ok_or_else(|| {
                Error::new(
                    ErrorKind::CacheCorrupt,
                    format!("run cache verify, because {digest} has no recorded fingerprint"),
                )
            })?;
        let now = self.fingerprint_of(digest)?;
        if recorded.matches(now) {
            return Ok(());
        }
        Err(Error::new(
            ErrorKind::CacheCorrupt,
            format!("run cache verify, because {digest} changed since it was published"),
        ))
    }

    fn check_bytes(&self, digest: ContentDigest) -> Result<(), Error> {
        if self.has_outboard(digest)? {
            let found = self.localize(digest)?;
            if found.not_localized.is_none() && found.damaged.is_empty() {
                return Ok(());
            }
            if let Some(first) = found.damaged.first() {
                return Err(Error::new(
                    ErrorKind::IntegrityRangeMismatch,
                    format!(
                        "run repair on {digest}, because {} of its bytes do not match its tree, the first of them from {} to {}",
                        found
                            .damaged
                            .iter()
                            .map(|span| span.end - span.start)
                            .sum::<u64>(),
                        first.start,
                        first.end
                    ),
                ));
            }
        }
        let found = self.hash_object(digest)?;
        if found == digest {
            return Ok(());
        }
        Err(Error::new(
            ErrorKind::CacheCorrupt,
            format!(
                "run cache verify, because {} holds {found} rather than {digest}",
                self.layout.objects().display()
            ),
        ))
    }

    pub fn object_is_its_digest(&self, digest: ContentDigest) -> Result<bool, Error> {
        Ok(self.hash_object(digest)? == digest)
    }

    pub fn object_record(&self, digest: ContentDigest) -> PathBuf {
        self.layout.records().join(name_of(digest))
    }

    pub fn quarantined(&self) -> Result<Vec<ContentDigest>, Error> {
        Self::digests_in(&self.layout.quarantine())
    }
    pub(crate) fn digests_in(directory: &std::path::Path) -> Result<Vec<ContentDigest>, Error> {
        let entries = std::fs::read_dir(directory)
            .map_err(|reason| filesystem_failure(Surface::Cache, directory, &reason))?;
        let mut found = Vec::new();
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str()
                && let Some(digest) = digest_of(name)
            {
                found.push(digest);
            }
        }
        found.sort();
        Ok(found)
    }
}

impl<P: Platform> Store for Cache<P> {
    type Reader = ObjectReader<P::Lock>;
    type Writer = PartialWriter;
    type Lease = WriteLease<P::Lock>;

    fn format_fingerprint(&self) -> Result<CacheFormatFingerprint, Error> {
        Ok(crate::format::fingerprint())
    }

    fn contains(&self, digest: ContentDigest) -> Result<bool, Error> {
        Ok(self.present(digest))
    }

    fn open(&self, digest: ContentDigest) -> Result<Self::Reader, Error> {
        let lease = self.read_lease(digest)?;
        self.check(digest)?;
        let bytes = self.read(digest)?;
        Ok(ObjectReader { bytes, lease })
    }

    fn lease(&self, key: PartialKey) -> Result<Self::Lease, Error> {
        let path = self.layout.lock_of(key.name());
        let (lock, waited_for) = if let Some(held) = self.platform.try_lock(&path)? {
            (held, None)
        } else {
            let holder = record::read_owner(&self.layout.lock_owner_of(key.name()))?;
            (self.platform.lock(&path)?, holder)
        };
        record::write(
            &self.layout.lock_owner_of(key.name()),
            &self.token,
            &self.work,
        )?;
        Ok(WriteLease {
            key,
            waited_for,
            lock,
        })
    }

    fn waited(&self, lease: &Self::Lease) -> bool {
        lease.waited_for().is_some()
    }

    fn begin(&self, lease: &Self::Lease, length: u64) -> Result<Self::Writer, Error> {
        let path = self.layout.partial_of(lease.key.name());
        if path.exists() {
            std::fs::remove_file(&path)
                .map_err(|reason| filesystem_failure(Surface::Cache, path.as_path(), &reason))?;
        }
        let file = self.platform.create_file_exclusive(&path)?;
        self.platform.preallocate(&file, length)?;
        record::write(&owner_record_of(&path), &self.token, &self.work)?;
        Ok(PartialWriter {
            file,
            path,
            pair: hashing::Pair::new(),
            written: 0,
            work: Arc::clone(self.work()),
            processor: Arc::clone(self.processor()),
        })
    }

    fn resume(&self, lease: &Self::Lease, length: u64, valid: u64) -> Result<Self::Writer, Error> {
        let path = self.layout.partial_of(lease.key.name());
        if !path.exists() || valid == 0 {
            return self.begin(lease, length);
        }
        let shortened = std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .map_err(|reason| filesystem_failure(Surface::Cache, path.as_path(), &reason))?;
        shortened
            .set_len(valid)
            .map_err(|reason| filesystem_failure(Surface::Cache, path.as_path(), &reason))?;
        drop(shortened);

        let mut existing = std::fs::File::open(&path)
            .map_err(|reason| filesystem_failure(Surface::Cache, path.as_path(), &reason))?;
        let mut pair = hashing::Pair::new();
        let mut buffer = vec![0u8; RESUME_BUFFER_BYTES];
        let mut written = 0u64;
        loop {
            let taken = existing
                .read(&mut buffer)
                .map_err(|reason| filesystem_failure(Surface::Cache, path.as_path(), &reason))?;
            if taken == 0 {
                break;
            }
            pair.update(self.processor(), &buffer[..taken]);
            written += taken as u64;
            self.work().read_bytes(taken as u64);
        }
        drop(existing);

        let file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .map_err(|reason| filesystem_failure(Surface::Cache, path.as_path(), &reason))?;
        record::write(&owner_record_of(&path), &self.token, &self.work)?;
        Ok(PartialWriter {
            file,
            path,
            pair,
            written,
            work: Arc::clone(self.work()),
            processor: Arc::clone(self.processor()),
        })
    }

    fn record_source(&self, key: PartialKey, record: &SourceRecord) -> Result<(), Error> {
        record::write(
            &source_record_of(&self.layout.partial_of(key.name())),
            record,
            &self.work,
        )
    }

    fn recorded_source(&self, key: PartialKey) -> Result<Option<SourceRecord>, Error> {
        let partial = self.layout.partial_of(key.name());
        if !partial.exists() {
            return Ok(None);
        }
        record::read(&source_record_of(&partial))
    }

    fn discard_partial(&self, key: PartialKey) -> Result<(), Error> {
        let partial = self.layout.partial_of(key.name());
        let _ = std::fs::remove_file(source_record_of(&partial));
        let _ = std::fs::remove_file(owner_record_of(&partial));
        match std::fs::remove_file(&partial) {
            Ok(()) => Ok(()),
            Err(reason) if reason.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(reason) => Err(filesystem_failure(
                Surface::Cache,
                partial.as_path(),
                &reason,
            )),
        }
    }

    fn commit(&self, lease: Self::Lease, writer: Self::Writer) -> Result<hashing::Digests, Error> {
        let digests = writer.pair.finish();
        let found = digests.content;
        if let Some(expected) = lease.key.expected()
            && found != expected
        {
            let _ = std::fs::remove_file(&writer.path);
            let _ = std::fs::remove_file(owner_record_of(&writer.path));
            return Err(Error::new(
                ErrorKind::IntegrityMismatch,
                format!(
                    "fetch it again, because the bytes written hash to {found} rather than {expected}"
                ),
            ));
        }

        self.platform.flush(&writer.file, self.tier)?;
        if self.io_mode == IoMode::Uncached && writer.written > 0 {
            self.platform
                .release_written(&writer.file, 0, writer.written)?;
        }
        drop(writer.file);

        self.publish_object(&writer.path, &digests)?;
        let _ = std::fs::remove_file(owner_record_of(&writer.path));

        self.finish_publication(&digests)?;
        drop(lease);
        Ok(digests)
    }

    fn has_outboard(&self, digest: ContentDigest) -> Result<bool, Error> {
        Ok(self.layout.outboard_of(digest).is_file())
    }

    fn open_outboard(&self, digest: ContentDigest) -> Result<Self::Reader, Error> {
        let lease = self.read_lease(digest)?;
        let path = self.layout.outboard_of(digest);
        let file = std::fs::File::open(&path)
            .map_err(|reason| filesystem_failure(Surface::Cache, path.as_path(), &reason))?;
        let bytes = crate::storage::Bytes::whole(file, &path)?;
        Ok(ObjectReader { bytes, lease })
    }

    fn verified_prefix(
        &self,
        key: PartialKey,
        digest: ContentDigest,
        on_disk: u64,
    ) -> Result<u64, Error> {
        crate::repair::verified_prefix(self, key, digest, on_disk)
    }

    fn write_outboard(&self, digest: ContentDigest, tree: &[u8]) -> Result<(), Error> {
        let path = self.layout.outboard_of(digest);
        let beside = self.scratch_path().with_extension("outboard");
        let _ = std::fs::remove_file(&beside);
        let mut writing = self.platform.create_file_exclusive(&beside)?;
        writing
            .write_all(tree)
            .map_err(|reason| filesystem_failure(Surface::Cache, beside.as_path(), &reason))?;
        self.work.wrote_bytes(tree.len() as u64);
        self.platform.flush(&writing, self.tier)?;
        drop(writing);
        self.platform.publish_file(&beside, &path, self.tier)
    }

    fn stage(&self, destination_volume: &std::path::Path) -> Result<PathBuf, Error> {
        let volume = self.platform.volume_id(destination_volume)?;
        let here = self.platform.volume_id(&self.layout.staging())?;
        if volume != here {
            return Err(Error::new(
                ErrorKind::DestinationCrossVolume,
                format!(
                    "put the destination on the same volume as the cache, because {} is on another volume and a publish is a rename and never a copy",
                    destination_volume.display()
                ),
            ));
        }
        let name = format!("{}-{}", self.token.pid, self.token.start);
        let path = self.layout.staging().join(name);
        self.platform.create_directory_exclusive(&path)?;
        record::write(&owner_record_of(&path), &self.token, &self.work)?;
        Ok(path)
    }

    fn pin(&self, digest: ContentDigest) -> Result<(), Error> {
        let held = self.read_lease(digest)?;
        if !self.present(digest) {
            return Err(Error::new(
                ErrorKind::CacheCorrupt,
                format!("fetch {digest} before pinning it, because the cache does not hold it"),
            ));
        }
        let path = self.layout.pin_of(digest);
        let written = std::fs::write(&path, [])
            .map_err(|reason| filesystem_failure(Surface::Cache, path.as_path(), &reason));
        if written.is_ok() {
            self.work.touched_file();
        }
        drop(held);
        written
    }

    fn unpin(&self, digest: ContentDigest) -> Result<(), Error> {
        let path = self.layout.pin_of(digest);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(reason) if reason.kind() == std::io::ErrorKind::NotFound => Err(Error::new(
                ErrorKind::CacheCorrupt,
                format!("pin {digest} before unpinning it, because nothing pins it"),
            )),
            Err(reason) => Err(filesystem_failure(Surface::Cache, &path, &reason)),
        }
    }

    fn list(&self) -> Result<Vec<ContentDigest>, Error> {
        let mut found = Self::digests_in(&self.layout.objects())?;
        found.extend(self.packed_index().keys().copied());
        found.sort_unstable();
        found.dedup();
        Ok(found)
    }

    fn prune(&self, grace: Duration) -> Result<PruneReport, Error> {
        crate::prune::run(self, grace)
    }

    fn status(&self) -> Result<CacheStatus, Error> {
        let objects = self.list()?;
        let mut bytes = 0;
        for digest in &objects {
            bytes += self.size_of(*digest).unwrap_or_default();
        }
        Ok(CacheStatus {
            root: self.layout.root().to_path_buf(),
            objects: objects.len() as u64,
            bytes,
            partials: Self::digests_in(&self.layout.partial())?.len() as u64,
            pins: Self::digests_in(&self.layout.pins())?.len() as u64,
            quarantined: Self::digests_in(&self.layout.quarantine())?.len() as u64,
        })
    }
}
