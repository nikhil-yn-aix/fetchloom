//! Finding which bytes of a cached object are wrong, and putting them right.

use std::io::{Read, Seek, SeekFrom, Write};
use std::ops::Range;
use std::path::{Path, PathBuf};

use fetchloom_engine::digest::ContentDigest;
use fetchloom_engine::error::{Error, ErrorKind, Surface, filesystem_failure};
use fetchloom_engine::hashing;
use fetchloom_engine::outboard::{GROUP_LEN, find_damage};
use fetchloom_engine::seam::platform::Platform;
use fetchloom_engine::seam::store::Store;
use fetchloom_engine::timestamp::Timestamp;

use crate::Cache;
use crate::diagnosis::{Diagnosis, NotLocalized, spans_of};

use fetchloom_engine::limits::STREAM_BUFFER_BYTES as BUFFER;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Held {
    Published(PathBuf),
    Quarantined(PathBuf),
}

impl Held {
    #[must_use]
    pub(crate) fn path(&self) -> &std::path::Path {
        match self {
            Self::Published(path) | Self::Quarantined(path) => path,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Localized {
    pub object_len: u64,
    pub damaged: Vec<Range<u64>>,
    pub not_localized: Option<NotLocalized>,
}

impl<P: Platform> Cache<P> {
    #[must_use]
    pub fn locate(&self, digest: ContentDigest) -> Option<Held> {
        if let Some(placed) = self.placement(digest) {
            return Some(Held::Published(placed.container().to_path_buf()));
        }
        let quarantined = self.layout().quarantined(digest);
        if quarantined.is_file() {
            return Some(Held::Quarantined(quarantined));
        }
        None
    }

    /// # Errors
    /// `cache.corrupt` when the cache holds no such object or the object's
    /// own bytes cannot be read. An object whose damage cannot be narrowed is
    /// a `Localized` saying why, not an error.
    pub fn localize(&self, digest: ContentDigest) -> Result<Localized, Error> {
        let Some(held) = self.locate(digest) else {
            return Err(Error::new(
                ErrorKind::CacheCorrupt,
                format!("fetch {digest} again, because this cache holds no such object"),
            ));
        };
        let object_len = match held {
            Held::Published(_) => self.size_of(digest).unwrap_or_default(),
            Held::Quarantined(ref at) => std::fs::metadata(at)
                .map_err(|reason| filesystem_failure(Surface::Cache, at, &reason))?
                .len(),
        };

        if !self.has_outboard(digest)? {
            return Ok(Localized {
                object_len,
                damaged: Vec::new(),
                not_localized: Some(NotLocalized::NoTreeStored),
            });
        }

        let path = held.path().to_path_buf();
        let mut tree = std::fs::File::open(self.layout().outboard_of(digest))
            .map_err(|reason| filesystem_failure(Surface::Cache, &path, &reason))?;
        let recorded_len = recorded_length(&mut tree).unwrap_or(object_len);
        let mut object = std::fs::File::open(&path)
            .map_err(|reason| filesystem_failure(Surface::Cache, &path, &reason))?;
        let work = self.work();
        let found = find_damage(&mut tree, recorded_len, digest, &mut |group, into| {
            read_group(&mut object, &path, group, object_len, into, work)
        });
        match found {
            Ok(damaged) => Ok(Localized {
                object_len: recorded_len,
                damaged,
                not_localized: None,
            }),
            Err(refused) if refused.kind() == ErrorKind::CacheCorrupt => Ok(Localized {
                object_len,
                damaged: Vec::new(),
                not_localized: Some(NotLocalized::TreeDoesNotCheckOut),
            }),
            Err(refused) => Err(refused),
        }
    }

    /// # Errors
    /// `cache.locked` or `cache.locking_unsupported` when the object cannot be
    /// locked for the move, and `cache.corrupt` when it cannot be moved or its
    /// diagnosis cannot be written.
    pub fn quarantine(
        &self,
        digest: ContentDigest,
        source: Option<String>,
        validator: Option<String>,
    ) -> Result<Diagnosis, Error> {
        let localized = self.localize(digest).unwrap_or(Localized {
            object_len: 0,
            damaged: Vec::new(),
            not_localized: Some(NotLocalized::ObjectUnreadable),
        });
        let found = self.hash_object(digest).ok();

        let held = self.platform().lock(&self.layout().lock_of(digest))?;
        self.quarantine_object(digest)?;
        let _ = std::fs::remove_file(self.object_record(digest));
        drop(held);

        let diagnosis = Diagnosis {
            digest,
            found,
            size: localized.object_len,
            damaged: spans_of(&localized.damaged),
            localized: localized.not_localized,
            source,
            validator,
            quarantined_at: Timestamp::now(),
            next_action: format!("fetchloom repair {digest}"),
        };
        self.write_diagnosis(&diagnosis)?;
        Ok(diagnosis)
    }

    /// # Errors
    /// `cache.corrupt` when the quarantined object cannot be opened for
    /// writing.
    pub fn begin_repair(&self, digest: ContentDigest) -> Result<RepairWriter, Error> {
        let Some(held) = self.locate(digest) else {
            return Err(Error::new(
                ErrorKind::CacheCorrupt,
                format!("fetch {digest} again, because this cache holds no such object"),
            ));
        };
        let path = self.layout().partial_of(digest);
        let _ = std::fs::remove_file(&path);
        match held {
            Held::Published(_) => self.place_object(digest, &path)?,
            Held::Quarantined(ref at) => {
                self.platform().clone_or_copy(at, &path).map(|_| ())?;
            }
        }
        let file = std::fs::OpenOptions::new()
            .write(true)
            .truncate(false)
            .open(&path)
            .map_err(|reason| filesystem_failure(Surface::Cache, &path, &reason))?;
        Ok(RepairWriter {
            file,
            path,
            digest,
            written: 0,
        })
    }

    /// # Errors
    /// `integrity.truncated` when the body ends before the span it was asked
    /// for, and `cache.corrupt` when the object cannot be written at that
    /// offset.
    pub fn patch(
        &self,
        writer: &mut RepairWriter,
        span: Range<u64>,
        bytes: impl Read,
    ) -> Result<u64, Error> {
        let mut bytes = bytes;
        writer
            .file
            .seek(SeekFrom::Start(span.start))
            .map_err(|reason| filesystem_failure(Surface::Cache, &writer.path, &reason))?;
        let mut buffer = vec![0u8; BUFFER];
        let mut left = span.end.saturating_sub(span.start);
        let mut moved = 0u64;
        while left > 0 {
            let want = usize::try_from(left.min(BUFFER as u64)).unwrap_or(BUFFER);
            let filled = bytes
                .read(&mut buffer[..want])
                .map_err(|reason| filesystem_failure(Surface::Cache, &writer.path, &reason))?;
            if filled == 0 {
                break;
            }
            writer
                .file
                .write_all(&buffer[..filled])
                .map_err(|reason| filesystem_failure(Surface::Cache, &writer.path, &reason))?;
            self.work().wrote_bytes(filled as u64);
            left -= filled as u64;
            moved += filled as u64;
        }
        writer.written += moved;
        if left > 0 {
            return Err(Error::new(
                ErrorKind::IntegrityTruncated,
                format!(
                    "fetch the range again, because the source ended the body {left} bytes before the span it was asked for"
                ),
            )
            .with_retryable(true));
        }
        Ok(moved)
    }

    /// # Errors
    /// `integrity.mismatch` when the repaired object still does not hash to
    /// its name, and `cache.corrupt` when it cannot be truncated to length or
    /// published back out of quarantine.
    pub fn finish_repair(
        &self,
        writer: RepairWriter,
        length: u64,
    ) -> Result<hashing::Digests, Error> {
        let RepairWriter {
            file, path, digest, ..
        } = writer;
        file.set_len(length)
            .map_err(|reason| filesystem_failure(Surface::Cache, &path, &reason))?;
        self.platform().flush(&file, self.tier())?;
        drop(file);

        let mut reading = std::fs::File::open(&path)
            .map_err(|reason| filesystem_failure(Surface::Cache, &path, &reason))?;
        let mut pair = hashing::Pair::new();
        let mut buffer = vec![0u8; BUFFER];
        loop {
            let filled = reading
                .read(&mut buffer)
                .map_err(|reason| filesystem_failure(Surface::Cache, &path, &reason))?;
            if filled == 0 {
                break;
            }
            pair.update(self.processor(), &buffer[..filled]);
            self.work().read_bytes(filled as u64);
        }
        drop(reading);
        let digests = pair.finish();

        if digests.content != digest {
            let _ = std::fs::remove_file(&path);
            return Err(Error::new(
                ErrorKind::IntegrityMismatch,
                format!(
                    "fetch {digest} again in full, because the repaired bytes hash to {} and the ranges the tree named were not the only ones that were wrong",
                    digests.content
                ),
            ));
        }

        self.remove_object(digest)?;
        self.publish_object(&path, &digests)?;
        self.finish_publication(&digests)?;
        let _ = std::fs::remove_file(self.layout().quarantined(digest));
        let _ = std::fs::remove_file(self.layout().diagnosis_of(digest));
        Ok(digests)
    }

    pub(crate) fn hash_object(&self, digest: ContentDigest) -> Result<ContentDigest, Error> {
        let Some(held) = self.locate(digest) else {
            return Err(Error::new(
                ErrorKind::CacheCorrupt,
                format!("fetch {digest} again, because this cache holds no such object"),
            ));
        };
        let path = held.path().to_path_buf();
        let mut file: Box<dyn Read> = match held {
            Held::Published(_) => Box::new(self.read(digest)?),
            Held::Quarantined(ref at) => Box::new(
                std::fs::File::open(at)
                    .map_err(|reason| filesystem_failure(Surface::Cache, at, &reason))?,
            ),
        };
        let mut pair = hashing::Pair::new();
        let mut buffer = vec![0u8; BUFFER];
        loop {
            let filled = file
                .read(&mut buffer)
                .map_err(|reason| filesystem_failure(Surface::Cache, &path, &reason))?;
            if filled == 0 {
                break;
            }
            pair.update(self.processor(), &buffer[..filled]);
            self.work().read_bytes(filled as u64);
        }
        Ok(pair.finish().content)
    }
}

pub struct RepairWriter {
    file: std::fs::File,
    path: PathBuf,
    digest: ContentDigest,
    written: u64,
}

fn read_group(
    object: &mut std::fs::File,
    path: &Path,
    group: u64,
    object_len: u64,
    into: &mut Vec<u8>,
    work: &fetchloom_engine::work::WorkCounter,
) -> Result<(), Error> {
    let start = group * GROUP_LEN;
    let end = (start + GROUP_LEN).min(object_len);
    into.clear();
    if start >= object_len {
        return Ok(());
    }
    object
        .seek(SeekFrom::Start(start))
        .map_err(|reason| filesystem_failure(Surface::Cache, path, &reason))?;
    let want = usize::try_from(end - start).unwrap_or(usize::MAX);
    into.resize(want, 0);
    let mut filled = 0;
    while filled < want {
        let read = object
            .read(&mut into[filled..])
            .map_err(|reason| filesystem_failure(Surface::Cache, path, &reason))?;
        if read == 0 {
            break;
        }
        filled += read;
    }
    into.truncate(filled);
    work.read_bytes(filled as u64);
    Ok(())
}

fn recorded_length(tree: &mut std::fs::File) -> Option<u64> {
    let mut header = [0u8; 8];
    tree.seek(SeekFrom::Start(0)).ok()?;
    tree.read_exact(&mut header).ok()?;
    tree.seek(SeekFrom::Start(0)).ok()?;
    Some(u64::from_le_bytes(header))
}

/// # Errors
/// `cache.corrupt` when the partial or its outboard cannot be read. A prefix
/// that does not check out is a shorter answer, not an error.
pub fn verified_prefix<P: Platform>(
    cache: &Cache<P>,
    key: fetchloom_engine::partial_key::PartialKey,
    digest: ContentDigest,
    on_disk: u64,
) -> Result<u64, Error> {
    let path = cache.layout().partial_of(key.name());
    if on_disk < GROUP_LEN || !path.is_file() || !cache.has_outboard(digest)? {
        return Ok(0);
    }
    let whole_groups = on_disk / GROUP_LEN;
    let mut tree = std::fs::File::open(cache.layout().outboard_of(digest))
        .map_err(|reason| filesystem_failure(Surface::Cache, &path, &reason))?;
    let mut header = [0u8; 8];
    tree.read_exact(&mut header)
        .map_err(|reason| filesystem_failure(Surface::Cache, &path, &reason))?;
    let object_len = u64::from_le_bytes(header);

    let mut partial = std::fs::File::open(&path)
        .map_err(|reason| filesystem_failure(Surface::Cache, &path, &reason))?;
    let work = cache.work();
    let found = find_damage(&mut tree, object_len, digest, &mut |group, into| {
        if group >= whole_groups {
            into.clear();
            return Ok(());
        }
        read_group(&mut partial, &path, group, on_disk, into, work)
    });
    match found {
        Ok(damaged) => Ok(damaged
            .first()
            .map_or(whole_groups * GROUP_LEN, |first| first.start)),
        Err(_) => Ok(0),
    }
}
