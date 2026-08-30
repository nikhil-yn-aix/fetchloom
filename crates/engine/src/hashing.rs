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

use crate::digest::{ContentDigest, InteropDigest};
use crate::pool::Processor;

/// The number of bytes at which pairing the two digests across the processor
/// pool starts to pay.
///
/// Measured on this workspace's own pairing rather than taken from either
/// hash's own figure: below a quarter of this, pairing across the pool runs at
/// a third of the speed of pairing on one thread, between them the two are
/// level, and at this size and above the pool is ahead by a tenth or more.
pub const POOL_THRESHOLD: usize = 1 << 20;

/// The content and interop digest of the same object bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Digests {
    /// The BLAKE3 root of the object bytes.
    pub content: ContentDigest,
    /// The SHA-256 of the same bytes.
    pub interop: InteropDigest,
}

/// The two digests of one object, updated together.
///
/// This is the only place the pairing is written down, so there is one answer
/// to what the two digests cover and one rule for where they run.
#[derive(Clone, Debug, Default)]
pub struct Pair {
    content: blake3::Hasher,
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

    /// Returns the two digests.
    #[must_use]
    pub fn finish(self) -> Digests {
        let content = ContentDigest::from_bytes(*self.content.finalize().as_bytes());
        let interop_bytes: [u8; 32] = self.interop.finalize().into();
        Digests {
            content,
            interop: InteropDigest::from_bytes(interop_bytes),
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
