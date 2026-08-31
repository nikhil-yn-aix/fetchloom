//! The one lookup that answers where an object's bytes are.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use fetchloom_engine::digest::ContentDigest;
use fetchloom_engine::error::{Error, ErrorKind, Surface, filesystem_failure};
use fetchloom_engine::identity::Fingerprint;
use fetchloom_engine::seam::platform::Platform;

use crate::Cache;

/// Where one object's bytes are.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Placement {
    /// A file of its own.
    Loose(PathBuf),
}

impl Placement {
    /// Returns the file the bytes are in.
    #[must_use]
    pub fn container(&self) -> &Path {
        match self {
            Self::Loose(path) => path,
        }
    }

    /// Returns the offset the bytes start at within that file.
    #[must_use]
    pub fn offset(&self) -> u64 {
        match self {
            Self::Loose(_) => 0,
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
        loose.is_file().then_some(Placement::Loose(loose))
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
            length: length - start,
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
    pub fn publish_object(&self, from: &Path, digest: ContentDigest) -> Result<(), Error> {
        let to = self.layout().object(digest);
        self.platform().publish_file(from, &to, self.tier())?;
        crate::seal_object(&to)
    }

    /// Removes the object with the given digest.
    ///
    /// # Errors
    ///
    /// Fails when the bytes are there and cannot be removed.
    pub fn remove_object(&self, digest: ContentDigest) -> Result<(), Error> {
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
        }
    }
}
