//! Small objects stored beside each other in one file.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;

use fetchloom_engine::compression::Stored;
use fetchloom_engine::digest::{ContentDigest, InteropDigest};
use fetchloom_engine::error::{Error, ErrorKind, Surface, filesystem_failure};
use fetchloom_engine::seam::platform::Platform;

use crate::Cache;

/// The bytes an entry states about itself before its own bytes begin.
pub const ENTRY_HEADER: usize = 32 + 32 + 8 + 8 + 1;

const HEADER: usize = ENTRY_HEADER;

const PACK_MAGIC: [u8; 4] = *b"FLP1";

/// What a pack states about itself before its first entry.
pub const PREAMBLE: usize = 4 + 4 + 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    pub offset: u64,
    pub length: u64,
    pub plain_length: u64,
    pub framed: bool,
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
        let body = body_of(bytes, stored, None, &path)?;
        let _appending = self
            .appending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut file = std::fs::File::options()
            .append(true)
            .create(true)
            .open(&path)
            .map_err(|reason| filesystem_failure(Surface::Cache, &path, &reason))?;
        let mut at = file
            .seek(SeekFrom::End(0))
            .map_err(|reason| filesystem_failure(Surface::Cache, &path, &reason))?;
        if at == 0 {
            let preamble = preamble_of(&[])?;
            file.write_all(&preamble)
                .map_err(|reason| filesystem_failure(Surface::Cache, &path, &reason))?;
            at = preamble.len() as u64;
        }
        let entry = Entry {
            offset: at + HEADER as u64,
            length: body.len() as u64,
            plain_length: bytes.len() as u64,
            framed: stored != Stored::Raw,
            interop,
        };
        file.write_all(&header_of(digest, &entry))
            .and_then(|()| file.write_all(&body))
            .map_err(|reason| filesystem_failure(Surface::Cache, &path, &reason))?;
        self.platform().flush(&file, self.tier())?;
        self.work().touched_file();
        self.work().wrote_bytes(body.len() as u64);
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
        let mut at = preamble_span(pack)?;
        if file.seek(SeekFrom::Start(at)).is_err() {
            return Ok(found);
        }
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
            let framed = framed_of(header[80], pack)?;
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
                    framed,
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
    header[80] = u8::from(entry.framed);
    header
}

fn framed_of(byte: u8, pack: &std::path::Path) -> Result<bool, Error> {
    match byte {
        0 => Ok(false),
        1 => Ok(true),
        other => Err(malformed(
            pack,
            &format!("an entry that states neither raw nor framed but {other}"),
        )),
    }
}

fn body_of(
    bytes: &[u8],
    stored: Stored,
    dictionary: Option<&[u8]>,
    pack: &std::path::Path,
) -> Result<Vec<u8>, Error> {
    match stored {
        Stored::Raw => Ok(bytes.to_vec()),
        Stored::Zstd { level, stride, .. } => {
            let mut framed = Vec::with_capacity(bytes.len());
            crate::compress::write_frames(
                &mut std::io::Cursor::new(bytes),
                &mut framed,
                pack,
                level,
                stride,
                dictionary,
            )?;
            Ok(framed)
        }
    }
}

/// The dictionary a pack states every framed entry in it was compressed
/// against, empty when the pack states none.
///
/// # Errors
/// `cache.corrupt` when the pack does not open with a preamble this build
/// reads, or states a dictionary longer than the file holds.
/// How many bytes the preamble occupies, without checking the dictionary it
/// holds. Scanning a pack needs to know where its entries start, and an entry
/// header is readable whether or not the dictionary beside it still is.
///
/// # Errors
/// `cache.corrupt` when the pack does not open with a preamble this build
/// reads.
pub fn preamble_span(pack: &std::path::Path) -> Result<u64, Error> {
    let mut file = match std::fs::File::open(pack) {
        Ok(file) => file,
        Err(reason) if reason.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(reason) => return Err(filesystem_failure(Surface::Cache, pack, &reason)),
    };
    let mut preamble = [0_u8; PREAMBLE];
    if !read_exactly(&mut file, &mut preamble, pack)? {
        return Ok(0);
    }
    if preamble[..4] != PACK_MAGIC {
        return Err(malformed(pack, "no pack preamble"));
    }
    let length = u32::from_le_bytes(
        preamble[4..8]
            .try_into()
            .map_err(|_| malformed(pack, "a dictionary length that is not four bytes"))?,
    );
    Ok(PREAMBLE as u64 + u64::from(length))
}

/// # Errors
/// `cache.corrupt` when the pack does not open with a preamble this build
/// reads, states a dictionary longer than the file holds, or holds one that no
/// longer hashes to what the pack recorded beside it.
pub fn dictionary_in(pack: &std::path::Path) -> Result<Vec<u8>, Error> {
    let mut file = match std::fs::File::open(pack) {
        Ok(file) => file,
        Err(reason) if reason.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(reason) => return Err(filesystem_failure(Surface::Cache, pack, &reason)),
    };
    let mut preamble = [0_u8; PREAMBLE];
    if !read_exactly(&mut file, &mut preamble, pack)? {
        return Ok(Vec::new());
    }
    if preamble[..4] != PACK_MAGIC {
        return Err(malformed(pack, "no pack preamble"));
    }
    let length = u32::from_le_bytes(
        preamble[4..8]
            .try_into()
            .map_err(|_| malformed(pack, "a dictionary length that is not four bytes"))?,
    ) as usize;
    if length == 0 {
        return Ok(Vec::new());
    }
    let mut dictionary = vec![0_u8; length];
    if !read_exactly(&mut file, &mut dictionary, pack)? {
        return Err(malformed(
            pack,
            "a dictionary length reaching past the end of the pack",
        ));
    }
    let mut stated = [0_u8; 32];
    stated.copy_from_slice(&preamble[8..40]);
    if fetchloom_engine::hashing::hash_bytes(&dictionary).bytes() != &stated {
        return Err(crate::compact::unreadable(
            pack,
            "the dictionary it states does not hash to what the pack recorded when it was written",
        ));
    }
    Ok(dictionary)
}

fn preamble_of(dictionary: &[u8]) -> Result<Vec<u8>, Error> {
    let length = u32::try_from(dictionary.len()).map_err(|_| {
        Error::new(
            ErrorKind::CacheCorrupt,
            "a dictionary longer than a four byte length describes".to_owned(),
        )
    })?;
    let mut preamble = Vec::with_capacity(PREAMBLE + dictionary.len());
    preamble.extend_from_slice(&PACK_MAGIC);
    preamble.extend_from_slice(&length.to_le_bytes());
    preamble.extend_from_slice(fetchloom_engine::hashing::hash_bytes(dictionary).bytes());
    preamble.extend_from_slice(dictionary);
    Ok(preamble)
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
        let preamble = preamble_of(&dictionary_in(pack)?)?;
        writing
            .write_all(&preamble)
            .map_err(|reason| filesystem_failure(Surface::Cache, &beside, &reason))?;
        let mut moved = Vec::new();
        let mut at = preamble.len() as u64;
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

impl<P: Platform> Cache<P> {
    /// Writes `objects` into `pack`, compressed against `dictionary`, which the
    /// pack then states in its preamble so nothing else has to supply it.
    ///
    /// # Errors
    /// `cache.corrupt` when the pack cannot be written or published.
    pub(crate) fn rewrite_pack_with(
        &self,
        pack: &std::path::Path,
        objects: &[(ContentDigest, Vec<u8>)],
        dictionary: Option<&[u8]>,
    ) -> Result<(), Error> {
        let held = self.platform().lock(&pack.with_extension("lock"))?;
        let stored = crate::compact::stored_for(dictionary, self.compression());
        let beside = pack.with_extension("compacting");
        let _ = std::fs::remove_file(&beside);
        let mut writing = self.platform().create_file_exclusive(&beside)?;
        let preamble = preamble_of(dictionary.unwrap_or(&[]))?;
        writing
            .write_all(&preamble)
            .map_err(|reason| filesystem_failure(Surface::Cache, &beside, &reason))?;
        let mut moved = Vec::new();
        let mut at = preamble.len() as u64;
        for (digest, bytes) in objects {
            let body = body_of(bytes, stored, dictionary, &beside)?;
            let entry = Entry {
                offset: at + HEADER as u64,
                length: body.len() as u64,
                plain_length: bytes.len() as u64,
                framed: stored != Stored::Raw,
                interop: self.interop_of(*digest)?,
            };
            writing
                .write_all(&header_of(*digest, &entry))
                .and_then(|()| writing.write_all(&body))
                .map_err(|reason| filesystem_failure(Surface::Cache, &beside, &reason))?;
            at += HEADER as u64 + entry.length;
            moved.push((*digest, entry));
        }
        self.platform().flush(&writing, self.tier())?;
        drop(writing);
        self.platform().publish_file(&beside, pack, self.tier())?;
        drop(held);
        self.forget_pack(pack);
        for (digest, entry) in moved {
            self.remember_packed(digest, pack.to_path_buf(), entry);
        }
        Ok(())
    }

    fn interop_of(&self, digest: ContentDigest) -> Result<InteropDigest, Error> {
        self.packed_index()
            .get(&digest)
            .map(|(_, entry)| entry.interop)
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::CacheCorrupt,
                    format!("run cache verify, because {digest} left its pack while it was being compacted"),
                )
            })
    }
}
