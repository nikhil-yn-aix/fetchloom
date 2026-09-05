//! Content and interop digests computed in one streaming pass.

use std::io::{self, Read};

use sha2::{Digest as _, Sha256};

use blake3::hazmat::{ChainingValue, HasherExt as _};

use crate::digest::{ContentDigest, InteropDigest};
use crate::outboard::{self, GROUP_LEN, Outboard};
use crate::pool::Processor;

const POOL_THRESHOLD: usize = 1 << 20;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Digests {
    pub content: ContentDigest,
    pub interop: InteropDigest,
    pub outboard: Option<Outboard>,
    pub length: u64,
}

#[derive(Clone, Debug)]
struct Groups {
    current: blake3::Hasher,
    filled: u64,
    finished: Vec<ChainingValue>,
    length: u64,
}

impl Default for Groups {
    fn default() -> Self {
        Self {
            current: blake3::Hasher::new(),
            filled: 0,
            finished: Vec::new(),
            length: 0,
        }
    }
}

impl Groups {
    fn update(&mut self, chunk: &[u8]) {
        self.fill(chunk, false);
    }

    fn update_parallel(&mut self, chunk: &[u8]) {
        self.fill(chunk, true);
    }

    fn fill(&mut self, mut chunk: &[u8], across_threads: bool) {
        while !chunk.is_empty() {
            if self.filled == GROUP_LEN {
                self.finished.push(self.current.finalize_non_root());
                self.current = blake3::Hasher::new();
                self.current.set_input_offset(self.length);
                self.filled = 0;
            }
            let room = usize_of(GROUP_LEN - self.filled);
            let take = room.min(chunk.len());
            if across_threads {
                self.current.update_rayon(&chunk[..take]);
            } else {
                self.current.update(&chunk[..take]);
            }
            self.filled += take as u64;
            self.length += take as u64;
            chunk = &chunk[take..];
        }
    }

    fn finish(mut self) -> (ContentDigest, Option<Outboard>) {
        if self.finished.is_empty() {
            let root = ContentDigest::from_bytes(*self.current.finalize().as_bytes());
            return (root, None);
        }
        self.finished.push(self.current.finalize_non_root());
        outboard::tree_of(self.length, &self.finished)
    }
}

fn usize_of(value: u64) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

#[derive(Clone, Debug, Default)]
pub struct Pair {
    content: Groups,
    interop: Sha256,
}

impl Pair {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn update(&mut self, processor: &Processor, chunk: &[u8]) {
        if chunk.len() < POOL_THRESHOLD {
            self.content.update(chunk);
            self.interop.update(chunk);
            return;
        }
        let content = &mut self.content;
        let interop = &mut self.interop;
        processor.install(|| {
            rayon::join(
                || {
                    content.update_parallel(chunk);
                },
                || {
                    interop.update(chunk);
                },
            );
        });
    }

    #[must_use]
    pub fn finish(self) -> Digests {
        let interop_bytes: [u8; 32] = self.interop.finalize().into();
        let length = self.content.length;
        let (content, outboard) = self.content.finish();
        Digests {
            content,
            interop: InteropDigest::from_bytes(interop_bytes),
            outboard,
            length,
        }
    }
}

#[derive(Debug)]
pub struct Digester {
    buffer: Vec<u8>,
}

impl Default for Digester {
    fn default() -> Self {
        Self::new()
    }
}

impl Digester {
    #[must_use]
    pub fn new() -> Self {
        Self {
            buffer: vec![0u8; POOL_THRESHOLD],
        }
    }

    /// # Errors
    /// Whatever the reader reports, unchanged, so the caller decides which
    /// surface a read failure belongs to.
    pub fn hash(&mut self, processor: &Processor, reader: impl Read) -> io::Result<Digests> {
        let mut reader = reader;
        let mut pair = Pair::new();
        loop {
            let filled = fill(&mut reader, &mut self.buffer)?;
            if filled == 0 {
                break;
            }
            pair.update(processor, &self.buffer[..filled]);
            if filled < self.buffer.len() {
                break;
            }
        }
        Ok(pair.finish())
    }
}

pub(crate) fn fill(reader: &mut impl Read, buffer: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        let read = reader.read(&mut buffer[filled..])?;
        if read == 0 {
            break;
        }
        filled += read;
    }
    Ok(filled)
}

#[must_use]
pub fn hash_bytes(bytes: &[u8]) -> ContentDigest {
    ContentDigest::from_bytes(*blake3::hash(bytes).as_bytes())
}

#[cfg(test)]
mod tests {
    use super::POOL_THRESHOLD;
    use crate::limits::OUTBOARD_CHUNK_GROUP;

    #[test]
    fn the_threshold_is_the_chunk_a_stream_is_read_in() {
        assert_eq!(
            POOL_THRESHOLD as u64, OUTBOARD_CHUNK_GROUP,
            "a chunk that fills the buffer must be the chunk the pool is entered for"
        );
    }
}
