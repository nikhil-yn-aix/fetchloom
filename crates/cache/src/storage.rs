//! The one lookup that answers where an object's bytes are.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use fetchloom_engine::digest::ContentDigest;
use fetchloom_engine::error::{Error, ErrorKind, Surface, filesystem_failure};
use fetchloom_engine::identity::Fingerprint;
use fetchloom_engine::seam::platform::Platform;

use crate::Cache;
use fetchloom_engine::hashing::Digests;

/// Where one object's bytes are.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Placement {
    /// A file of its own.
    Loose(PathBuf),
    /// A span of a pack this cache holds beside other small objects.
    Packed {
        /// The pack the bytes are in.
        pack: PathBuf,
        /// Where they start and how many there are.
        entry: crate::pack::Entry,
    },
}

impl Placement {
    /// Returns the file the bytes are in.
    #[must_use]
    pub fn container(&self) -> &Path {
        match self {
            Self::Loose(path) => path,
            Self::Packed { pack, .. } => pack,
        }
    }

    /// Returns the offset the bytes start at within that file.
    #[must_use]
    pub fn offset(&self) -> u64 {
        match self {
            Self::Loose(_) => 0,
            Self::Packed { entry, .. } => entry.offset,
        }
    }

    /// Returns how many bytes the object holds, when the placement states it.
    #[must_use]
    pub fn length(&self) -> Option<u64> {
        match self {
            Self::Loose(_) => None,
            Self::Packed { entry, .. } => Some(entry.length),
        }
    }
}

/// A bounded, seekable view of one object's bytes.
#[derive(Debug)]
pub struct Bytes {
    file: std::fs::File,
    start: u64,
    length: u64,
    at: u64,
}

impl Bytes {
    /// Views a whole file as bytes.
    ///
    /// # Errors
    ///
    /// Fails when the length cannot be read.
    pub fn whole(file: std::fs::File, path: &Path) -> Result<Self, Error> {
        let length = file
            .metadata()
            .map_err(|reason| filesystem_failure(Surface::Cache, path, &reason))?
            .len();
        Ok(Self {
            file,
            start: 0,
            length,
            at: 0,
        })
    }

    /// Returns how many bytes the object holds.
    #[must_use]
    pub fn length(&self) -> u64 {
        self.length
    }
}

impl Read for Bytes {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let left = self.length.saturating_sub(self.at);
        if left == 0 {
            return Ok(0);
        }
        let want = usize::try_from(left)
            .unwrap_or(usize::MAX)
            .min(buffer.len());
        let filled = self.file.read(&mut buffer[..want])?;
        self.at += filled as u64;
        Ok(filled)
    }
}

impl Seek for Bytes {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        let wanted = match to {
            SeekFrom::Start(offset) => offset,
            SeekFrom::End(offset) => self.length.saturating_add_signed(offset),
            SeekFrom::Current(offset) => self.at.saturating_add_signed(offset),
        };
        let landed = self.file.seek(SeekFrom::Start(self.start + wanted))?;
        self.at = landed - self.start;
        Ok(self.at)
    }
}

impl<P: Platform> Cache<P> {
    /// Returns where the bytes of an object are, when the cache holds it.
    #[must_use]
    pub fn placement(&self, digest: ContentDigest) -> Option<Placement> {
        let loose = self.layout().object(digest);
        if loose.is_file() {
            return Some(Placement::Loose(loose));
        }
        let (pack, entry) = self.packed_index().get(&digest).cloned()?;
        Some(Placement::Packed { pack, entry })
    }

    /// Returns whether the cache holds an object.
    #[must_use]
    pub fn holds(&self, digest: ContentDigest) -> bool {
        self.placement(digest).is_some()
    }

    /// Returns how many bytes an object holds, or nothing when it is absent.
    #[must_use]
    pub fn size_of(&self, digest: ContentDigest) -> Option<u64> {
        let placed = self.placement(digest)?;
        match placed {
            Placement::Loose(path) => std::fs::metadata(path).ok().map(|found| found.len()),
            Placement::Packed { entry, .. } => Some(entry.length),
        }
    }

    /// Opens an object for reading.
    ///
    /// # Errors
    ///
    /// Fails when the cache does not hold the object and when its bytes cannot
    /// be read.
    pub fn read(&self, digest: ContentDigest) -> Result<Bytes, Error> {
        let placed = self.placement(digest).ok_or_else(|| absent(digest))?;
        let container = placed.container().to_path_buf();
        let mut file = std::fs::File::open(&container)
            .map_err(|reason| filesystem_failure(Surface::Cache, &container, &reason))?;
        let length = file
            .metadata()
            .map_err(|reason| filesystem_failure(Surface::Cache, &container, &reason))?
            .len();
        let start = placed.offset();
        file.seek(SeekFrom::Start(start))
            .map_err(|reason| filesystem_failure(Surface::Cache, &container, &reason))?;
        Ok(Bytes {
            file,
            start,
            length: placed.length().unwrap_or(length - start),
            at: 0,
        })
    }

    /// Returns the tuple that records an object is probably unchanged.
    ///
    /// # Errors
    ///
    /// Fails when the cache does not hold the object and when the platform
    /// refuses the query.
    pub fn fingerprint_of(&self, digest: ContentDigest) -> Result<Fingerprint, Error> {
        let placed = self.placement(digest).ok_or_else(|| absent(digest))?;
        self.platform().fingerprint(placed.container())
    }

    /// Returns whether an object is packed beside others.
    #[must_use]
    pub fn is_packed(&self, digest: ContentDigest) -> bool {
        matches!(self.placement(digest), Some(Placement::Packed { .. }))
    }
}

fn absent(digest: ContentDigest) -> Error {
    Error::new(
        ErrorKind::CacheCorrupt,
        format!("fetch {digest} again, because this cache does not hold it"),
    )
}

impl<P: Platform> Cache<P> {
    /// Publishes a scratch file as the object with the given digest.
    ///
    /// # Errors
    ///
    /// Fails when the file cannot be placed under its final name.
    pub fn publish_object(&self, from: &Path, digests: &Digests) -> Result<(), Error> {
        let digest = digests.content;
        let length = std::fs::metadata(from)
            .map_err(|reason| filesystem_failure(Surface::Cache, from, &reason))?
            .len();
        if length <= fetchloom_engine::limits::PACK_THRESHOLD {
            let bytes = std::fs::read(from)
                .map_err(|reason| filesystem_failure(Surface::Cache, from, &reason))?;
            self.pack_bytes(digest, digests.interop, &bytes)?;
            let _ = std::fs::remove_file(from);
            return Ok(());
        }
        let to = self.layout().object(digest);
        self.platform().publish_file(from, &to, self.tier())?;
        crate::seal_object(&to)
    }

    /// Packs an object this run already holds in memory.
    ///
    /// # Errors
    ///
    /// Fails when the pack cannot be written.
    pub fn pack_bytes(
        &self,
        digest: ContentDigest,
        interop: fetchloom_engine::digest::InteropDigest,
        bytes: &[u8],
    ) -> Result<(), Error> {
        let entry = self.append_to_pack(digest, interop, bytes)?;
        self.remember_packed(digest, self.own_pack(), entry);
        Ok(())
    }

    /// Removes the object with the given digest.
    ///
    /// # Errors
    ///
    /// Fails when the bytes are there and cannot be removed.
    pub fn remove_object(&self, digest: ContentDigest) -> Result<(), Error> {
        if let Some(Placement::Packed { pack, .. }) = self.placement(digest) {
            let mut only = std::collections::BTreeSet::new();
            only.insert(digest);
            return self.rewrite_pack(&pack, &only);
        }
        let path = self.layout().object(digest);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(reason) if reason.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(reason) => Err(filesystem_failure(Surface::Cache, &path, &reason)),
        }
    }

    /// Moves the object with the given digest into quarantine.
    ///
    /// # Errors
    ///
    /// Fails when the bytes are there and cannot be moved.
    pub fn quarantine_object(&self, digest: ContentDigest) -> Result<(), Error> {
        let Some(placed) = self.placement(digest) else {
            return Ok(());
        };
        if matches!(placed, Placement::Packed { .. }) {
            let aside = self.layout().quarantined(digest);
            let _ = std::fs::remove_file(&aside);
            self.place_object(digest, &aside)?;
            return self.remove_object(digest);
        }
        let from = placed.container().to_path_buf();
        let to = self.layout().quarantined(digest);
        self.platform()
            .publish_file(&from, &to, self.tier())
            .map_err(|reason| {
                Error::new(
                    ErrorKind::CacheCorrupt,
                    format!(
                        "remove {} by hand, because it failed verification and could not be quarantined: {}",
                        from.display(),
                        reason.next_action()
                    ),
                )
            })
    }

    /// Returns whether this user created the object with the given digest.
    ///
    /// # Errors
    ///
    /// Fails when the platform refuses the query.
    pub fn owns_object(&self, digest: ContentDigest) -> Result<bool, Error> {
        match self.placement(digest) {
            Some(placed) => self.platform().owns(placed.container()),
            None => Ok(false),
        }
    }
}

impl<P: Platform> Cache<P> {
    /// Places a copy of an object's bytes at a path that does not exist.
    ///
    /// # Errors
    ///
    /// Fails when the cache does not hold the object and when the bytes cannot
    /// be placed.
    pub fn place_object(&self, digest: ContentDigest, at: &Path) -> Result<(), Error> {
        let placed = self.placement(digest).ok_or_else(|| absent(digest))?;
        match placed {
            Placement::Loose(from) => self
                .platform()
                .clone_or_copy(&from, at)
                .map(|_mechanism| ()),
            Placement::Packed { .. } => {
                let mut reading = self.read(digest)?;
                let mut file = self.platform().create_file_exclusive(at)?;
                let mut buffer = vec![0_u8; 1 << 16];
                loop {
                    let filled = reading
                        .read(&mut buffer)
                        .map_err(|reason| filesystem_failure(Surface::Destination, at, &reason))?;
                    if filled == 0 {
                        break;
                    }
                    file.write_all(&buffer[..filled])
                        .map_err(|reason| filesystem_failure(Surface::Destination, at, &reason))?;
                    self.work().wrote_bytes(filled as u64);
                }
                self.platform().flush(&file, self.tier())
            }
        }
    }
}
