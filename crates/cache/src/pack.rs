//! Small objects stored beside each other in one file.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;

use fetchloom_engine::compression::{Stored, Transform};
use fetchloom_engine::digest::{ContentDigest, InteropDigest};
use fetchloom_engine::error::{Error, ErrorKind, Surface, filesystem_failure};
use fetchloom_engine::seam::platform::Platform;

use crate::Cache;

const HEADER: usize = 32 + 32 + 8 + 8 + 1 + 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    pub offset: u64,
    pub length: u64,
    pub plain_length: u64,
    pub stored: Stored,
    pub interop: InteropDigest,
}

impl<P: Platform> Cache<P> {
    pub(crate) fn own_pack(&self) -> PathBuf {
        let token = self.token();
        self.layout()
            .packs()
            .join(format!("{}-{}.pack", token.boot.as_str(), token.pid))
    }

    pub(crate) fn append_to_pack(
        &self,
        digest: ContentDigest,
        interop: InteropDigest,
        bytes: &[u8],
        stored: Stored,
    ) -> Result<Entry, Error> {
        let path = self.own_pack();
        let body = body_of(bytes, stored, &path)?;
        let _appending = self
            .appending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut file = std::fs::File::options()
            .append(true)
            .create(true)
            .open(&path)
            .map_err(|reason| filesystem_failure(Surface::Cache, &path, &reason))?;
        let at = file
            .seek(SeekFrom::End(0))
            .map_err(|reason| filesystem_failure(Surface::Cache, &path, &reason))?;
        let entry = Entry {
            offset: at + HEADER as u64,
            length: body.len() as u64,
            plain_length: bytes.len() as u64,
            stored,
            interop,
        };
        file.write_all(&header_of(digest, &entry))
            .and_then(|()| file.write_all(&body))
            .map_err(|reason| filesystem_failure(Surface::Cache, &path, &reason))?;
        self.platform().flush(&file, self.tier())?;
        self.work().touched_file();
        self.work().wrote_bytes((HEADER + body.len()) as u64);
        Ok(entry)
    }

    /// # Errors
    /// `cache.corrupt` when the pack index cannot be read or does not parse.
    pub fn entries_in(&self, pack: &std::path::Path) -> Result<Vec<(ContentDigest, Entry)>, Error> {
        let mut file = match std::fs::File::open(pack) {
            Ok(file) => file,
            Err(reason) if reason.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(reason) => return Err(filesystem_failure(Surface::Cache, pack, &reason)),
        };
        let on_disk = file
            .metadata()
            .map_err(|reason| filesystem_failure(Surface::Cache, pack, &reason))?
            .len();
        let mut found = Vec::new();
        let mut at = 0u64;
        loop {
            let mut header = [0_u8; HEADER];
            if !read_exactly(&mut file, &mut header, pack)? {
                return Ok(found);
            }
            let mut name = [0_u8; 32];
            name.copy_from_slice(&header[..32]);
            let mut interop = [0_u8; 32];
            interop.copy_from_slice(&header[32..64]);
            let length = u64::from_le_bytes(
                header[64..72]
                    .try_into()
                    .map_err(|_| malformed(pack, "a length that is not eight bytes"))?,
            );
            let plain_length = u64::from_le_bytes(
                header[72..80]
                    .try_into()
                    .map_err(|_| malformed(pack, "a length that is not eight bytes"))?,
            );
            let stored = stored_of(header[80], header[81], pack)?;
            let digest = ContentDigest::from_bytes(name);
            let start = at + HEADER as u64;
            if start.saturating_add(length) > on_disk {
                return Ok(found);
            }
            if file.seek(SeekFrom::Start(start + length)).is_err() {
                return Ok(found);
            }
            found.push((
                digest,
                Entry {
                    offset: start,
                    length,
                    plain_length,
                    stored,
                    interop: InteropDigest::from_bytes(interop),
                },
            ));
            at = start + length;
        }
    }

    /// # Errors
    /// `cache.corrupt` when the pack directory cannot be walked. A directory
    /// that does not exist yet is an empty list rather than an error.
    pub fn packs(&self) -> Result<Vec<PathBuf>, Error> {
        let directory = self.layout().packs();
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(reason) if reason.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(reason) => return Err(filesystem_failure(Surface::Cache, &directory, &reason)),
        };
        let mut found: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|kind| kind == "pack"))
            .collect();
        found.sort();
        Ok(found)
    }
}

fn header_of(digest: ContentDigest, entry: &Entry) -> [u8; HEADER] {
    let mut header = [0_u8; HEADER];
    header[..32].copy_from_slice(digest.bytes());
    header[32..64].copy_from_slice(entry.interop.bytes());
    header[64..72].copy_from_slice(&entry.length.to_le_bytes());
    header[72..80].copy_from_slice(&entry.plain_length.to_le_bytes());
    let (level, transform) = match entry.stored {
        Stored::Raw => (0, 0),
        Stored::Zstd { level, transform } => (
            u8::try_from(level).unwrap_or(0),
            match transform {
                Transform::None => 0,
                Transform::Shuffle => 1,
            },
        ),
    };
    header[80] = level;
    header[81] = transform;
    header
}

fn stored_of(level: u8, transform: u8, pack: &std::path::Path) -> Result<Stored, Error> {
    if level == 0 {
        return Ok(Stored::Raw);
    }
    let transform = match transform {
        0 => Transform::None,
        1 => Transform::Shuffle,
        other => {
            return Err(malformed(
                pack,
                &format!("a byte transform numbered {other}"),
            ));
        }
    };
    Ok(Stored::Zstd {
        level: i32::from(level),
        transform,
    })
}

fn body_of(bytes: &[u8], stored: Stored, pack: &std::path::Path) -> Result<Vec<u8>, Error> {
    match stored {
        Stored::Raw => Ok(bytes.to_vec()),
        Stored::Zstd { level, transform } => {
            let mut framed = Vec::with_capacity(bytes.len());
            crate::compress::write_frames(
                &mut std::io::Cursor::new(bytes),
                &mut framed,
                pack,
                level,
                transform,
            )?;
            Ok(framed)
        }
    }
}

fn read_exactly(
    file: &mut std::fs::File,
    into: &mut [u8],
    pack: &std::path::Path,
) -> Result<bool, Error> {
    let mut filled = 0;
    while filled < into.len() {
        let taken = file
            .read(&mut into[filled..])
            .map_err(|reason| filesystem_failure(Surface::Cache, pack, &reason))?;
        if taken == 0 {
            return Ok(false);
        }
        filled += taken;
    }
    Ok(true)
}

fn malformed(pack: &std::path::Path, what: &str) -> Error {
    Error::new(
        ErrorKind::CacheCorrupt,
        format!(
            "run cache repair, because {} carries {what}",
            pack.display()
        ),
    )
}

pub(crate) type Index = std::collections::BTreeMap<ContentDigest, (PathBuf, Entry)>;

impl<P: Platform> Cache<P> {
    pub(crate) fn packed_index(&self) -> std::sync::Arc<Index> {
        let mut held = self
            .packed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(built) = held.as_ref() {
            return std::sync::Arc::clone(built);
        }
        let mut index = Index::new();
        for pack in self.packs().unwrap_or_default() {
            for (digest, entry) in self.entries_in(&pack).unwrap_or_default() {
                index.insert(digest, (pack.clone(), entry));
            }
        }
        let built = std::sync::Arc::new(index);
        *held = Some(std::sync::Arc::clone(&built));
        built
    }

    pub(crate) fn remember_packed(&self, digest: ContentDigest, pack: PathBuf, entry: Entry) {
        let mut held = self
            .packed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(built) = held.as_mut() {
            std::sync::Arc::make_mut(built).insert(digest, (pack, entry));
        }
    }
}

impl<P: Platform> Cache<P> {
    pub(crate) fn rewrite_pack(
        &self,
        pack: &std::path::Path,
        without: &std::collections::BTreeSet<ContentDigest>,
    ) -> Result<(), Error> {
        let held = self.platform().lock(&pack.with_extension("lock"))?;
        let kept: Vec<(ContentDigest, Entry)> = self
            .entries_in(pack)?
            .into_iter()
            .filter(|(digest, _)| !without.contains(digest))
            .collect();
        if kept.is_empty() {
            drop(held);
            let _ = std::fs::remove_file(pack);
            let _ = std::fs::remove_file(pack.with_extension("lock"));
            self.forget_pack(pack);
            return Ok(());
        }
        let mut source = std::fs::File::open(pack)
            .map_err(|reason| filesystem_failure(Surface::Cache, pack, &reason))?;
        let beside = pack.with_extension("rewriting");
        let mut writing = self.platform().create_file_exclusive(&beside)?;
        let mut moved = Vec::new();
        let mut at = 0u64;
        for (digest, entry) in kept {
            let mut bytes = vec![0_u8; usize::try_from(entry.length).unwrap_or(0)];
            source
                .seek(SeekFrom::Start(entry.offset))
                .and_then(|_| source.read_exact(&mut bytes))
                .map_err(|reason| filesystem_failure(Surface::Cache, pack, &reason))?;
            let moved_to = Entry {
                offset: at + HEADER as u64,
                ..entry
            };
            writing
                .write_all(&header_of(digest, &moved_to))
                .and_then(|()| writing.write_all(&bytes))
                .map_err(|reason| filesystem_failure(Surface::Cache, &beside, &reason))?;
            moved.push((digest, moved_to));
            at += HEADER as u64 + entry.length;
        }
        self.platform().flush(&writing, self.tier())?;
        drop(writing);
        drop(source);
        self.platform().publish_file(&beside, pack, self.tier())?;
        drop(held);
        self.forget_pack(pack);
        for (digest, entry) in moved {
            self.remember_packed(digest, pack.to_path_buf(), entry);
        }
        Ok(())
    }

    fn forget_pack(&self, pack: &std::path::Path) {
        let mut held = self
            .packed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(built) = held.as_mut() {
            std::sync::Arc::make_mut(built).retain(|_, (held_in, _)| held_in != pack);
        }
    }
}
