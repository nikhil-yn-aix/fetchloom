//! Content and interop digests computed in one streaming pass.

use std::io::{self, Read};

use sha2::{Digest as _, Sha256};

use crate::digest::{ContentDigest, InteropDigest};
use crate::limits::OUTBOARD_CHUNK_GROUP;
use crate::pool::Processor;

/// The content and interop digest of the same object bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Digests {
    /// The BLAKE3 root of the object bytes.
    pub content: ContentDigest,
    /// The SHA-256 of the same bytes.
    pub interop: InteropDigest,
}

/// Reads `reader` once and returns the content and interop digest of its bytes.
///
/// Streams through a single buffer sized to one outboard chunk group,
/// allocated once and reused. The content and interop digest are updated on
/// separate threads of the given processor pool so neither serializes the
/// other.
///
/// # Errors
///
/// Fails when `reader` fails.
pub fn hash_stream(processor: &Processor, mut reader: impl Read) -> io::Result<Digests> {
    let mut buffer = vec![0u8; usize_from_u64(OUTBOARD_CHUNK_GROUP)];
    let mut content = blake3::Hasher::new();
    let mut interop = Sha256::new();
    loop {
        let filled = fill(&mut reader, &mut buffer)?;
        if filled == 0 {
            break;
        }
        update_digests(processor, &mut content, &mut interop, &buffer[..filled]);
    }
    Ok(finish(&content, interop))
}

/// Updates a content hasher and an interop hasher with the same bytes.
///
/// The two updates run on separate threads of `processor`'s pool through
/// `rayon::join`, so neither update serializes the other. Shared by every
/// caller that streams bytes through both digests, so there is exactly one
/// place this pairing happens.
pub(crate) fn update_digests(
    processor: &Processor,
    content: &mut blake3::Hasher,
    interop: &mut Sha256,
    chunk: &[u8],
) {
    processor.install(|| {
        rayon::join(|| content.update(chunk), || interop.update(chunk));
    });
}

/// Finalizes a content hasher and an interop hasher into their digests.
pub(crate) fn finish(content: &blake3::Hasher, interop: Sha256) -> Digests {
    let content = ContentDigest::from_bytes(*content.finalize().as_bytes());
    let interop_bytes: [u8; 32] = interop.finalize().into();
    let interop = InteropDigest::from_bytes(interop_bytes);
    Digests { content, interop }
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

fn usize_from_u64(value: u64) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

/// Returns the content digest of bytes already held in memory.
///
/// Takes a slice short enough to hold, such as a symbolic link target. Returns
/// the same digest `hash_stream` would return for the same bytes.
#[must_use]
pub fn hash_bytes(bytes: &[u8]) -> ContentDigest {
    ContentDigest::from_bytes(*blake3::hash(bytes).as_bytes())
}
