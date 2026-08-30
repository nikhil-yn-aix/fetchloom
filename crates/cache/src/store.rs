//! Objects, partials, leases, pins, and the publication that puts an object in
//! `objects/`.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use fetchloom_engine::digest::ContentDigest;
use fetchloom_engine::error::{Error, ErrorKind};
use fetchloom_engine::hashing;
use fetchloom_engine::identity::CacheFormatFingerprint;
use fetchloom_engine::partial_key::PartialKey;
use fetchloom_engine::pool::Processor;
use fetchloom_engine::seam::platform::{OwnerToken, Platform};
use fetchloom_engine::seam::store::{CacheStatus, PruneReport, Store};
use fetchloom_engine::source_record::SourceRecord;
use fetchloom_engine::verification::VerificationPolicy;
use fetchloom_engine::work::WorkCounter;

use crate::layout::{digest_of, name_of};
use crate::record::{self, ObjectRecord};
use crate::{Cache, failure, owner_record_of, seal_object, source_record_of};

/// How many bytes a resume reads back at a time to rebuild the digest.
const RESUME_BUFFER_BYTES: usize = 1 << 20;

/// A completed object held open, with the lease that keeps it from being
/// pruned while it is being read.
#[derive(Debug)]
pub struct ObjectReader<L> {
    file: std::fs::File,
    lease: L,
}

impl<L> ObjectReader<L> {
    /// Returns the lease held for as long as this object is open.
    pub fn lease(&self) -> &L {
        &self.lease
    }
}

impl<L> Read for ObjectReader<L> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.file.read(buffer)
    }
}

/// An object being written, hashed as its bytes arrive.
///
/// Both digests are taken in the one pass the bytes make, on separate threads
/// of the processor pool, so neither serializes the other and neither costs a
/// second read.
pub struct PartialWriter {
    file: std::fs::File,
    path: PathBuf,
    pair: hashing::Pair,
    written: u64,
    work: Arc<WorkCounter>,
    processor: Arc<Processor>,
}

impl PartialWriter {
    /// Returns how many bytes have been written.
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

/// The single-writer claim on one key.
#[derive(Debug)]
pub struct WriteLease<L> {
    key: PartialKey,
    waited_for: Option<OwnerToken>,
    lock: L,
}

impl<L> WriteLease<L> {
    /// Returns the key this claim is on.
    #[must_use]
    pub fn key(&self) -> PartialKey {
        self.key
    }

    /// Returns the holder this claim waited for, when it waited.
    #[must_use]
    pub fn waited_for(&self) -> Option<&OwnerToken> {
        self.waited_for.as_ref()
    }

    /// Returns the lock the claim is held by.
    pub fn lock(&self) -> &L {
        &self.lock
    }
}

impl<P: Platform> Cache<P> {
    /// Takes the lease a reader holds while it has an object open.
    ///
    /// # Errors
    ///
    /// Fails when the volume cannot express advisory locking.
    pub fn read_lease(&self, digest: ContentDigest) -> Result<P::Lock, Error> {
        self.platform.lock_shared(&self.layout.lock_of(digest))
    }

    /// Reports whether an object is present right now.
    ///
    /// Takes no lock, because the writer holding a digest asks this question
    /// while it holds that digest exclusively, and because the answer is only
    /// ever a reason to open the object. Opening it takes the lease and looks
    /// again, which is what makes a hit safe.
    fn present(&self, digest: ContentDigest) -> bool {
        self.layout.object(digest).is_file()
    }

    /// Checks an object against the verification policy this cache was opened
    /// with.
    fn check(&self, digest: ContentDigest) -> Result<(), Error> {
        match self.policy {
            VerificationPolicy::Never => Ok(()),
            VerificationPolicy::Fingerprint => self.check_fingerprint(digest),
            VerificationPolicy::Always => self.check_bytes(digest),
        }
    }

    fn check_fingerprint(&self, digest: ContentDigest) -> Result<(), Error> {
        let path = self.layout.object(digest);
        let recorded: ObjectRecord =
            record::read(&self.object_record(digest))?.ok_or_else(|| {
                Error::new(
                    ErrorKind::CacheCorrupt,
                    format!(
                        "run cache verify, because {} has no recorded fingerprint",
                        path.display()
                    ),
                )
            })?;
        let now = self.platform.fingerprint(&path)?;
        if recorded.matches(now) {
            return Ok(());
        }
        Err(Error::new(
            ErrorKind::CacheCorrupt,
            format!(
                "run cache verify, because {} changed since it was published",
                path.display()
            ),
        ))
    }

    fn check_bytes(&self, digest: ContentDigest) -> Result<(), Error> {
        let found = self.hash_object(digest)?;
        if found == digest {
            return Ok(());
        }
        Err(Error::new(
            ErrorKind::CacheCorrupt,
            format!(
                "run cache verify, because {} holds {found} rather than {digest}",
                self.layout.object(digest).display()
            ),
        ))
    }

    /// Reports whether an object still hashes to the name it is stored under.
    ///
    /// # Errors
    ///
    /// Fails when the object cannot be read.
    pub fn object_is_its_digest(&self, digest: ContentDigest) -> Result<bool, Error> {
        Ok(self.hash_object(digest)? == digest)
    }

    fn hash_object(&self, digest: ContentDigest) -> Result<ContentDigest, Error> {
        let path = self.layout.object(digest);
        let mut file = std::fs::File::open(&path)
            .map_err(|reason| failure(ErrorKind::CacheCorrupt, path.as_path(), &reason))?;
        let mut hasher = blake3::Hasher::new();
        let mut buffer = vec![0u8; 1 << 20];
        loop {
            let filled = file
                .read(&mut buffer)
                .map_err(|reason| failure(ErrorKind::CacheCorrupt, path.as_path(), &reason))?;
            if filled == 0 {
                break;
            }
            hasher.update(&buffer[..filled]);
            self.work().read_bytes(filled as u64);
        }
        Ok(ContentDigest::from_bytes(*hasher.finalize().as_bytes()))
    }

    /// Returns where the fingerprint of an object is recorded.
    pub(crate) fn object_record(&self, digest: ContentDigest) -> PathBuf {
        self.layout.records().join(name_of(digest))
    }

    /// Lists every object that failed verification.
    ///
    /// # Errors
    ///
    /// Fails when the cache cannot be read.
    pub fn quarantined(&self) -> Result<Vec<ContentDigest>, Error> {
        Self::digests_in(&self.layout.quarantine())
    }
    /// Lists the digests a directory of the cache names.
    pub(crate) fn digests_in(directory: &std::path::Path) -> Result<Vec<ContentDigest>, Error> {
        let entries = std::fs::read_dir(directory)
            .map_err(|reason| failure(ErrorKind::CacheCorrupt, directory, &reason))?;
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
        let path = self.layout.object(digest);
        if !path.is_file() {
            return Err(Error::new(
                ErrorKind::CacheCorrupt,
                format!(
                    "fetch it again, because {} holds no such object",
                    path.display()
                ),
            ));
        }
        self.check(digest)?;
        let file = std::fs::File::open(&path)
            .map_err(|reason| failure(ErrorKind::CacheCorrupt, path.as_path(), &reason))?;
        Ok(ObjectReader { file, lease })
    }

    fn lease(&self, key: PartialKey) -> Result<Self::Lease, Error> {
        let path = self.layout.lock_of(key.name());
        let (lock, waited_for) = if let Some(held) = self.platform.try_lock(&path)? {
            (held, None)
        } else {
            let holder = record::read_owner(&self.layout.lock_owner_of(key.name()))?;
            (self.platform.lock(&path)?, holder)
        };
        record::write(&self.layout.lock_owner_of(key.name()), &self.token)?;
        Ok(WriteLease {
            key,
            waited_for,
            lock,
        })
    }

    fn begin(&self, lease: &Self::Lease, length: u64) -> Result<Self::Writer, Error> {
        let path = self.layout.partial_of(lease.key.name());
        if path.exists() {
            std::fs::remove_file(&path)
                .map_err(|reason| failure(ErrorKind::CacheCorrupt, path.as_path(), &reason))?;
        }
        let file = self.platform.create_file_exclusive(&path)?;
        self.platform.preallocate(&file, length)?;
        record::write(&owner_record_of(&path), &self.token)?;
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
            .map_err(|reason| failure(ErrorKind::CacheCorrupt, path.as_path(), &reason))?;
        shortened
            .set_len(valid)
            .map_err(|reason| failure(ErrorKind::CacheCorrupt, path.as_path(), &reason))?;
        drop(shortened);

        let mut existing = std::fs::File::open(&path)
            .map_err(|reason| failure(ErrorKind::CacheCorrupt, path.as_path(), &reason))?;
        let mut pair = hashing::Pair::new();
        let mut buffer = vec![0u8; RESUME_BUFFER_BYTES];
        let mut written = 0u64;
        loop {
            let taken = existing
                .read(&mut buffer)
                .map_err(|reason| failure(ErrorKind::CacheCorrupt, path.as_path(), &reason))?;
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
            .map_err(|reason| failure(ErrorKind::CacheCorrupt, path.as_path(), &reason))?;
        record::write(&owner_record_of(&path), &self.token)?;
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
            Err(reason) => Err(failure(ErrorKind::CacheCorrupt, partial.as_path(), &reason)),
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
        drop(writer.file);

        let object = self.layout.object(found);
        self.platform
            .publish_file(&writer.path, &object, self.tier)?;
        let _ = std::fs::remove_file(owner_record_of(&writer.path));
        seal_object(&object)?;

        self.record_object(found, digests.interop)?;
        let _ = std::fs::remove_file(self.layout.mark_of(found));
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
            .map_err(|reason| failure(ErrorKind::CacheCorrupt, path.as_path(), &reason))?;
        Ok(ObjectReader { file, lease })
    }

    fn write_outboard(&self, digest: ContentDigest, tree: &[u8]) -> Result<(), Error> {
        let path = self.layout.outboard_of(digest);
        std::fs::write(&path, tree)
            .map_err(|reason| failure(ErrorKind::CacheCorrupt, path.as_path(), &reason))
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
        record::write(&owner_record_of(&path), &self.token)?;
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
            .map_err(|reason| failure(ErrorKind::CacheCorrupt, path.as_path(), &reason));
        drop(held);
        written
    }

    fn unpin(&self, digest: ContentDigest) -> Result<(), Error> {
        let path = self.layout.pin_of(digest);
        std::fs::remove_file(&path).map_err(|reason| {
            Error::new(
                ErrorKind::CacheCorrupt,
                format!("pin {digest} before unpinning it, because nothing pins it: {reason}"),
            )
        })
    }

    fn list(&self) -> Result<Vec<ContentDigest>, Error> {
        Self::digests_in(&self.layout.objects())
    }

    fn prune(&self, grace: Duration) -> Result<PruneReport, Error> {
        crate::prune::run(self, grace)
    }

    fn status(&self) -> Result<CacheStatus, Error> {
        let objects = self.list()?;
        let mut bytes = 0;
        for digest in &objects {
            if let Ok(found) = std::fs::metadata(self.layout.object(*digest)) {
                bytes += found.len();
            }
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
