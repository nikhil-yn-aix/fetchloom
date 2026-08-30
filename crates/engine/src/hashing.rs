//! Content and interop digests computed in one streaming pass.
//!
//! There is one place the two digests are paired and one rule for where the
//! pairing runs. A chunk large enough that entering the processor pool
//! disappears into the work is paired across it, so neither digest serializes
//! the other. A shorter chunk is paired on the calling thread, because the
//! scope costs more than the parallelism returns. The number the rule turns on
//! was measured rather than assumed.

use std::io::{self, Read};

use sha2::{Digest as _, Sha256};

use blake3::hazmat::{ChainingValue, HasherExt as _};

use crate::digest::{ContentDigest, InteropDigest};
use crate::outboard::{self, GROUP_LEN, Outboard};
use crate::pool::Processor;

/// The number of bytes at which pairing the two digests across the processor
/// pool starts to pay.
///
/// Measured on this workspace's own pairing rather than taken from either
/// hash's own figure: below a quarter of this, pairing across the pool runs at
/// a third of the speed of pairing on one thread, between them the two are
/// level, and at this size and above the pool is ahead by a tenth or more.
pub const POOL_THRESHOLD: usize = 1 << 20;

/// Everything one streaming pass over an object's bytes produces.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Digests {
    /// The BLAKE3 root of the object bytes.
    pub content: ContentDigest,
    /// The SHA-256 of the same bytes.
    pub interop: InteropDigest,
    /// The chunk tree, present only above the outboard threshold.
    pub outboard: Option<Outboard>,
    /// How many bytes the pass covered.
    pub length: u64,
}

/// The BLAKE3 side of one pass: the chaining value of each leaf group, and the
/// group still filling.
///
/// A group's chaining value is the only hash taken of its bytes. The root is
/// merged from those values rather than taken again, so an object is hashed
/// once whether or not it ends up large enough to store a tree.
#[derive(Clone, Debug)]
struct Groups {
    /// The hasher covering the group currently filling.
    current: blake3::Hasher,
    /// How many bytes of that group have arrived.
    filled: u64,
    /// The chaining value of every group already finished, in order.
    finished: Vec<ChainingValue>,
    /// How many bytes have arrived in total.
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
    /// Takes bytes, closing a group only once the next group has a byte in it.
    ///
    /// A group is left open at a boundary because an object that ends exactly
    /// there is one group whose root is taken directly, and one that continues
    /// needs that group's value as a leaf. Which of the two it is is not known
    /// until the next byte arrives or does not.
    fn update(&mut self, mut chunk: &[u8]) {
        while !chunk.is_empty() {
            if self.filled == GROUP_LEN {
                self.finished.push(self.current.finalize_non_root());
                self.current = blake3::Hasher::new();
                self.current.set_input_offset(self.length);
                self.filled = 0;
            }
            let room = usize_of(GROUP_LEN - self.filled);
            let take = room.min(chunk.len());
            self.current.update(&chunk[..take]);
            self.filled += take as u64;
            self.length += take as u64;
            chunk = &chunk[take..];
        }
    }

    /// Returns the content digest and, above the threshold, the tree.
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

/// The two digests of one object and its tree, updated together.
///
/// This is the only place the pairing is written down, so there is one answer
/// to what the two digests cover and one rule for where they run.
#[derive(Clone, Debug, Default)]
pub struct Pair {
    content: Groups,
    interop: Sha256,
}

impl Pair {
    /// Starts a pair that has seen nothing.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Updates both digests with the same bytes.
    ///
    /// Takes the pool the two updates may run on and the chunk they both
    /// cover. A chunk at or above the threshold runs on two threads of the
    /// pool and a shorter one runs on this thread.
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
                    content.update(chunk);
                },
                || {
                    interop.update(chunk);
                },
            );
        });
    }

    /// Returns the two digests, the tree, and the length covered.
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

/// The one buffer a run reads every stream it hashes through.
///
/// Held for as long as a run has streams to hash. A buffer the size of one
/// chunk group costs about ten microseconds to allocate, which is three times
/// what hashing a small file costs, so allocating one per file is most of the
/// price of hashing a directory of them.
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
    /// Allocates the one buffer every stream this digester reads will use.
    #[must_use]
    pub fn new() -> Self {
        Self {
            buffer: vec![0u8; POOL_THRESHOLD],
        }
    }

    /// Reads `reader` once and returns the content and interop digest of its
    /// bytes.
    ///
    /// Takes the pool the pairing may run on and the bytes to read. Nothing is
    /// read twice and nothing is allocated per stream or per chunk.
    ///
    /// # Errors
    ///
    /// Fails when `reader` fails.
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

/// Fills `buffer` from `reader`, reading until it is full or the reader ends.
///
/// Returns the number of bytes filled, which is less than the buffer length
/// only at the end of the reader.
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

/// Returns the content digest of bytes already held in memory.
///
/// Takes a slice short enough to hold, such as a symbolic link target. Returns
/// the same digest a stream of the same bytes would return.
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
