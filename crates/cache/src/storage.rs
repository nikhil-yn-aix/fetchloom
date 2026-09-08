//! The one lookup that answers where an object's bytes are.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use fetchloom_engine::compression::{CompressionChoice, Stored};
use fetchloom_engine::digest::ContentDigest;
use fetchloom_engine::error::{Error, ErrorKind, Surface, filesystem_failure};
use fetchloom_engine::identity::Fingerprint;
use fetchloom_engine::seam::platform::Platform;

use crate::Cache;
use fetchloom_engine::hashing::Digests;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Placement {
    Loose {
        path: PathBuf,
        compressed: bool,
    },
    Packed {
        pack: PathBuf,
        entry: crate::pack::Entry,
    },
}

impl Placement {
    #[must_use]
    pub fn container(&self) -> &Path {
        match self {
            Self::Loose { path, .. } => path,
            Self::Packed { pack, .. } => pack,
        }
    }

    #[must_use]
    pub fn offset(&self) -> u64 {
        match self {
            Self::Loose { .. } => 0,
            Self::Packed { entry, .. } => entry.offset,
        }
    }

    #[must_use]
    pub fn length(&self) -> Option<u64> {
        match self {
            Self::Loose { .. } => None,
            Self::Packed { entry, .. } => Some(entry.length),
        }
    }

    #[must_use]
    pub fn is_compressed(&self) -> bool {
        match self {
            Self::Loose { compressed, .. } => *compressed,
            Self::Packed { entry, .. } => entry.framed,
        }
    }

    #[must_use]
    pub fn is_packed(&self) -> bool {
        matches!(self, Self::Packed { .. })
    }
}

#[derive(Debug)]
enum Source {
    Raw { file: std::fs::File, start: u64 },
    Framed(Box<crate::compress::Frames>),
}

/// The uncompressed bytes of one object, however the cache stored them.
#[derive(Debug)]
pub struct Bytes {
    source: Source,
    path: PathBuf,
    length: u64,
    at: u64,
}

impl Bytes {
    /// # Errors
    /// `cache.corrupt` when the open file cannot be asked how long it is.
    pub fn whole(file: std::fs::File, path: &Path) -> Result<Self, Error> {
        let length = file
            .metadata()
            .map_err(|reason| filesystem_failure(Surface::Cache, path, &reason))?
            .len();
        Ok(Self {
            source: Source::Raw { file, start: 0 },
            path: path.to_path_buf(),
            length,
            at: 0,
        })
    }

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
        let filled = match &mut self.source {
            Source::Raw { file, .. } => file.read(&mut buffer[..want])?,
            Source::Framed(frames) => frames
                .read_at(self.at, &mut buffer[..want], &self.path)
                .map_err(|reason| std::io::Error::other(reason.next_action().to_owned()))?,
        };
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
        match &mut self.source {
            Source::Raw { file, start } => {
                let landed = file.seek(SeekFrom::Start(*start + wanted))?;
                self.at = landed - *start;
            }
            Source::Framed(_) => self.at = wanted.min(self.length),
        }
        Ok(self.at)
    }
}

impl<P: Platform> Cache<P> {
    #[must_use]
    pub fn placement(&self, digest: ContentDigest) -> Option<Placement> {
        let loose = self.layout().object(digest);
        if loose.is_file() {
            return Some(Placement::Loose {
                path: loose,
                compressed: false,
            });
        }
        let compressed = self.layout().compressed_object(digest);
        if compressed.is_file() {
            return Some(Placement::Loose {
                path: compressed,
                compressed: true,
            });
        }
        let (pack, entry) = self.packed_index().get(&digest).cloned()?;
        Some(Placement::Packed { pack, entry })
    }

    #[must_use]
    pub fn holds(&self, digest: ContentDigest) -> bool {
        self.placement(digest).is_some()
    }

    /// The object's own length, which is the length of its uncompressed bytes
    /// whether or not the cache stored it compressed.
    #[must_use]
    pub fn size_of(&self, digest: ContentDigest) -> Option<u64> {
        match self.placement(digest)? {
            Placement::Loose {
                path,
                compressed: false,
            } => std::fs::metadata(path).ok().map(|found| found.len()),
            Placement::Loose {
                path,
                compressed: true,
            } => {
                let file = std::fs::File::open(&path).ok()?;
                crate::compress::Frames::open(file, &path)
                    .ok()
                    .map(|frames| frames.plain_length())
            }
            Placement::Packed { entry, .. } => Some(entry.plain_length),
        }
    }

    /// # Errors
    /// `cache.corrupt` when the cache holds no such object, when its container
    /// cannot be opened or seeked to the object's offset, or when a compressed
    /// object carries no frame table this build reads.
    pub fn read(&self, digest: ContentDigest) -> Result<Bytes, Error> {
        let placed = self.placement(digest).ok_or_else(|| absent(digest))?;
        let container = placed.container().to_path_buf();
        let mut file = std::fs::File::open(&container)
            .map_err(|reason| filesystem_failure(Surface::Cache, &container, &reason))?;
        let on_disk = file
            .metadata()
            .map_err(|reason| filesystem_failure(Surface::Cache, &container, &reason))?
            .len();
        let start = placed.offset();
        let span = placed.length().unwrap_or(on_disk - start);
        if placed.is_compressed() {
            let dictionary = if placed.is_packed() {
                crate::pack::dictionary_in(&container)?
            } else {
                Vec::new()
            };
            let held = if dictionary.is_empty() {
                None
            } else {
                Some(dictionary.as_slice())
            };
            let frames = crate::compress::Frames::open_at(file, start, span, &container, held)?;
            return Ok(Bytes {
                length: frames.plain_length(),
                source: Source::Framed(Box::new(frames)),
                path: container,
                at: 0,
            });
        }
        file.seek(SeekFrom::Start(start))
            .map_err(|reason| filesystem_failure(Surface::Cache, &container, &reason))?;
        Ok(Bytes {
            source: Source::Raw { file, start },
            path: container,
            length: span,
            at: 0,
        })
    }

    /// # Errors
    /// `cache.corrupt` when the cache holds no such object or its container
    /// cannot be stat'd.
    pub fn fingerprint_of(&self, digest: ContentDigest) -> Result<Fingerprint, Error> {
        let placed = self.placement(digest).ok_or_else(|| absent(digest))?;
        self.platform().fingerprint(placed.container())
    }

    #[must_use]
    pub(crate) fn is_packed(&self, digest: ContentDigest) -> bool {
        matches!(self.placement(digest), Some(Placement::Packed { .. }))
    }
}

impl<P: Platform> Cache<P> {
    /// Compression that was asked for and did not happen is a degradation, so
    /// an object the probe decided against says so by name.
    fn record_decision(&self, digest: ContentDigest, decision: crate::compress::Decision) {
        if decision.stored != Stored::Raw || self.compression() == CompressionChoice::None {
            return;
        }
        self.degradations.record(
            format!("{digest} written {}", self.compression()),
            "it written raw",
            decision.reason(),
        );
    }
}

fn absent(digest: ContentDigest) -> Error {
    Error::new(
        ErrorKind::CacheCorrupt,
        format!("fetch {digest} again, because this cache does not hold it"),
    )
}

impl<P: Platform> Cache<P> {
    pub(crate) fn publish_object(&self, from: &Path, digests: &Digests) -> Result<(), Error> {
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
        let decision = self.decide_storage(from)?;
        self.record_decision(digest, decision);
        match decision.stored {
            Stored::Raw => {
                let to = self.layout().object(digest);
                self.platform().publish_file(from, &to, self.tier())?;
                crate::seal_object(&to)
            }
            Stored::Zstd { level, stride, .. } => {
                let beside = self.scratch_path().with_extension("compressing");
                let _ = std::fs::remove_file(&beside);
                self.claimed_scratch(&beside)?;
                let mut reading = std::fs::File::open(from)
                    .map_err(|reason| filesystem_failure(Surface::Cache, from, &reason))?;
                let mut writing = self.platform().create_file_exclusive(&beside)?;
                let written = crate::compress::write_frames(
                    &mut reading,
                    &mut writing,
                    &beside,
                    level,
                    stride,
                    None,
                )?;
                drop(reading);
                self.work().read_bytes(length);
                self.work().wrote_bytes(written);
                self.platform().flush(&writing, self.tier())?;
                drop(writing);
                let to = self.layout().compressed_object(digest);
                self.platform().publish_file(&beside, &to, self.tier())?;
                let _ = std::fs::remove_file(from);
                crate::seal_object(&to)
            }
        }
    }

    /// Reads the head of the object about to be published and compresses it,
    /// so what is decided comes from the bytes rather than from a name.
    fn decide_storage(&self, from: &Path) -> Result<crate::compress::Decision, Error> {
        let choice = self.compression();
        if choice == CompressionChoice::None {
            return crate::compress::decide(&[], choice);
        }
        let mut head = vec![0u8; fetchloom_engine::compression::PROBE_HEAD_BYTES];
        let mut file = std::fs::File::open(from)
            .map_err(|reason| filesystem_failure(Surface::Cache, from, &reason))?;
        let mut filled = 0;
        while filled < head.len() {
            let taken = file
                .read(&mut head[filled..])
                .map_err(|reason| filesystem_failure(Surface::Cache, from, &reason))?;
            if taken == 0 {
                break;
            }
            filled += taken;
        }
        head.truncate(filled);
        crate::compress::decide(&head, choice)
    }

    pub(crate) fn pack_bytes(
        &self,
        digest: ContentDigest,
        interop: fetchloom_engine::digest::InteropDigest,
        bytes: &[u8],
    ) -> Result<(), Error> {
        let decision = crate::compress::decide(bytes, self.compression())?;
        let entry = self.append_to_pack(digest, interop, bytes, decision.stored)?;
        self.remember_packed(digest, self.own_pack(), entry);
        Ok(())
    }

    /// # Errors
    /// `cache.corrupt` when the object cannot be removed or, for a packed
    /// object, when the pack cannot be rewritten without it. An object that is
    /// already gone is not an error.
    pub fn remove_object(&self, digest: ContentDigest) -> Result<(), Error> {
        if let Some(Placement::Packed { pack, .. }) = self.placement(digest) {
            let mut only = std::collections::BTreeSet::new();
            only.insert(digest);
            return self.rewrite_pack(&pack, &only);
        }
        for path in [
            self.layout().object(digest),
            self.layout().compressed_object(digest),
        ] {
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(reason) if reason.kind() == std::io::ErrorKind::NotFound => {}
                Err(reason) => return Err(filesystem_failure(Surface::Cache, &path, &reason)),
            }
        }
        Ok(())
    }

    pub(crate) fn quarantine_object(&self, digest: ContentDigest) -> Result<(), Error> {
        let Some(placed) = self.placement(digest) else {
            return Ok(());
        };
        if matches!(placed, Placement::Packed { .. }) {
            let aside = self.layout().quarantined(digest);
            let _ = std::fs::remove_file(&aside);
            match self.place_object(digest, &aside) {
                Ok(()) => {}
                Err(reason) if reason.kind() == ErrorKind::CacheCorrupt => {
                    self.place_stored_bytes(&placed, &aside)?;
                }
                Err(reason) => return Err(reason),
            }
            return self.remove_object(digest);
        }
        let from = placed.container().to_path_buf();
        let to = self.layout().quarantined(digest);
        self.platform()
            .publish_file(&from, &to, self.tier())
            .map_err(|reason| {
                Error::new(
                    reason.kind(),
                    format!(
                        "remove {} by hand, because it failed verification and could not be quarantined: {}",
                        from.display(),
                        reason.next_action()
                    ),
                )
            })
    }

    /// Copies an entry out of its pack exactly as it is stored, for an object
    /// whose stored form no longer decodes. Quarantine keeps the bytes a
    /// localized repair needs, and bytes that cannot be decoded are still the
    /// only ones there are.
    fn place_stored_bytes(&self, placed: &Placement, aside: &Path) -> Result<(), Error> {
        use std::io::{Read, Seek, SeekFrom, Write};

        let from = placed.container();
        let mut file = std::fs::File::open(from)
            .map_err(|reason| filesystem_failure(Surface::Cache, from, &reason))?;
        let span = placed.length().unwrap_or_default();
        file.seek(SeekFrom::Start(placed.offset()))
            .map_err(|reason| filesystem_failure(Surface::Cache, from, &reason))?;
        let mut stored = vec![0_u8; usize::try_from(span).unwrap_or_default()];
        file.read_exact(&mut stored)
            .map_err(|reason| filesystem_failure(Surface::Cache, from, &reason))?;
        let mut into = self.platform().create_file_exclusive(aside)?;
        into.write_all(&stored)
            .map_err(|reason| filesystem_failure(Surface::Cache, aside, &reason))?;
        self.platform().flush(&into, self.tier())
    }

    pub(crate) fn owns_object(&self, digest: ContentDigest) -> Result<bool, Error> {
        match self.placement(digest) {
            Some(placed) => self.platform().owns(placed.container()),
            None => Ok(false),
        }
    }
}

impl<P: Platform> Cache<P> {
    /// # Errors
    /// `cache.corrupt` when the cache holds no such object, `resource.disk`
    /// when the volume is full, and the destination's own filesystem kind
    /// when the clone or copy is refused.
    pub fn place_object(&self, digest: ContentDigest, at: &Path) -> Result<(), Error> {
        let placed = self.placement(digest).ok_or_else(|| absent(digest))?;
        match placed {
            Placement::Loose {
                path: from,
                compressed: false,
            } => self
                .platform()
                .clone_or_copy(&from, at)
                .map(|_mechanism| ()),
            Placement::Loose {
                compressed: true, ..
            }
            | Placement::Packed { .. } => {
                let mut reading = self.read(digest)?;
                let mut file = self.platform().create_file_exclusive(at)?;
                let mut buffer = vec![0_u8; fetchloom_engine::limits::STREAM_BUFFER_BYTES];
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
