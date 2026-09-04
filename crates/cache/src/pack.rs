//! Small objects stored beside each other in one file.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;

use fetchloom_engine::digest::{ContentDigest, InteropDigest};
use fetchloom_engine::error::{Error, ErrorKind, Surface, filesystem_failure};
use fetchloom_engine::seam::platform::Platform;

use crate::Cache;

const HEADER: usize = 32 + 32 + 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    pub offset: u64,
    pub length: u64,
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
    ) -> Result<Entry, Error> {
        let _appending = self
            .appending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let path = self.own_pack();
        let mut file = std::fs::File::options()
            .append(true)
            .create(true)
            .open(&path)
            .map_err(|reason| filesystem_failure(Surface::Cache, &path, &reason))?;
        let at = file
            .seek(SeekFrom::End(0))
            .map_err(|reason| filesystem_failure(Surface::Cache, &path, &reason))?;
        let mut header = [0_u8; HEADER];
        header[..32].copy_from_slice(digest.bytes());
        header[32..64].copy_from_slice(interop.bytes());
        header[64..].copy_from_slice(&(bytes.len() as u64).to_le_bytes());
        file.write_all(&header)
            .and_then(|()| file.write_all(bytes))
            .map_err(|reason| filesystem_failure(Surface::Cache, &path, &reason))?;
        self.platform().flush(&file, self.tier())?;
        self.work().touched_file();
        self.work().wrote_bytes((HEADER + bytes.len()) as u64);
        Ok(Entry {
            offset: at + HEADER as u64,
            length: bytes.len() as u64,
            interop,
        })
    }

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
                header[64..]
                    .try_into()
                    .map_err(|_| malformed(pack, "a length that is not eight bytes"))?,
            );
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
                    interop: InteropDigest::from_bytes(interop),
                },
            ));
            at = start + length;
        }
    }

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
            let mut header = [0_u8; HEADER];
            header[..32].copy_from_slice(digest.bytes());
            header[32..64].copy_from_slice(entry.interop.bytes());
            header[64..].copy_from_slice(&entry.length.to_le_bytes());
            writing
                .write_all(&header)
                .and_then(|()| writing.write_all(&bytes))
                .map_err(|reason| filesystem_failure(Surface::Cache, &beside, &reason))?;
            moved.push((
                digest,
                Entry {
                    offset: at + HEADER as u64,
                    length: entry.length,
                    interop: entry.interop,
                },
            ));
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
