//! The outboard chunk tree, its construction, and its range verification walk.

use std::io::{self, Read, Seek, SeekFrom};
use std::ops::Range;

use blake3::hazmat::{self, ChainingValue, HasherExt, Mode};
use sha2::Digest as _;

use crate::digest::ContentDigest;
use crate::error::{Error, ErrorKind};
use crate::hashing::{self, Digests};
use crate::limits::{OUTBOARD_CHUNK_GROUP, OUTBOARD_THRESHOLD};
use crate::pool::Processor;

const NODE_LEN: usize = 64;
const HEADER_LEN: usize = 8;

/// The length in bytes of one outboard leaf group.
pub const GROUP_LEN: u64 = OUTBOARD_CHUNK_GROUP;

/// A stored outboard tree: a length header followed by parent nodes in
/// pre-order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outboard {
    bytes: Vec<u8>,
}

impl Outboard {
    /// Returns the encoded outboard bytes: an eight-byte little-endian
    /// length followed by sixty-four-byte parent nodes in pre-order.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns the number of leaf groups this outboard covers.
    #[must_use]
    pub fn leaf_count(&self) -> u64 {
        u64::try_from((self.bytes.len() - HEADER_LEN) / NODE_LEN).unwrap_or(u64::MAX) + 1
    }
}

/// The digests and, when the object is large enough, the outboard tree
/// produced by one streaming pass over its bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuildOutput {
    /// The content and interop digest of the object.
    pub digests: Digests,
    /// The outboard tree, present only when the object exceeds the outboard
    /// threshold.
    pub outboard: Option<Outboard>,
}

/// Reads `reader` once and returns its digests and, when it is large enough,
/// its outboard tree.
///
/// Streams through a single buffer sized to one outboard chunk group,
/// allocated once and reused for both the digest pass and the leaf hashing
/// the outboard needs, so the object is never read twice.
///
/// # Errors
///
/// Fails when `reader` fails.
pub fn build_stream(processor: &Processor, mut reader: impl Read) -> io::Result<BuildOutput> {
    let mut buffer = vec![0u8; usize_from_u64(GROUP_LEN)];
    let mut content = blake3::Hasher::new();
    let mut interop = sha2::Sha256::new();
    let mut leaves: Vec<ChainingValue> = Vec::new();
    let mut group_index: u64 = 0;
    let mut object_len: u64 = 0;

    loop {
        let filled = hashing::fill(&mut reader, &mut buffer)?;
        if filled == 0 {
            break;
        }
        let chunk = &buffer[..filled];
        hashing::update_digests(processor, &mut content, &mut interop, chunk);
        leaves.push(leaf_chaining_value(group_index, chunk));
        object_len += u64::try_from(filled).unwrap_or(u64::MAX);
        group_index += 1;
    }

    let digests = hashing::finish(&content, interop);
    let outboard = build_outboard(object_len, &leaves);
    Ok(BuildOutput { digests, outboard })
}

/// Builds the outboard tree for an object of the given length from its
/// per-group leaf chaining values, when the object is large enough to need
/// one.
///
/// Takes the total object length and one chaining value per leaf group, in
/// order. Returns `None` at or below the outboard threshold, because the
/// content digest already authenticates an object that small whole.
///
/// # Panics
///
/// Panics when `leaves` does not have exactly one entry per group implied by
/// `object_len`, which is a bug in the caller rather than an input to
/// validate.
#[must_use]
pub fn build_outboard(object_len: u64, leaves: &[ChainingValue]) -> Option<Outboard> {
    if object_len <= OUTBOARD_THRESHOLD {
        return None;
    }
    let whole = Subtree::whole(object_len);
    assert_eq!(
        leaves.len() as u64,
        whole.group_count,
        "one leaf chaining value per group"
    );
    assert!(
        whole.group_count >= 65,
        "a stored outboard always has at least sixty-five leaves"
    );
    let mut bytes =
        Vec::with_capacity(HEADER_LEN + NODE_LEN * usize_from_u64(whole.parent_count()));
    bytes.extend_from_slice(&object_len.to_le_bytes());
    write_subtree(&whole, leaves, &mut bytes);
    Some(Outboard { bytes })
}

fn write_subtree(subtree: &Subtree, leaves: &[ChainingValue], out: &mut Vec<u8>) -> ChainingValue {
    if subtree.group_count == 1 {
        return leaves[usize_from_u64(subtree.start_group)];
    }
    let (left, right) = subtree.split();
    let node_at = out.len();
    out.extend_from_slice(&[0u8; NODE_LEN]);
    let left_cv = write_subtree(&left, leaves, out);
    let right_cv = write_subtree(&right, leaves, out);
    out[node_at..node_at + 32].copy_from_slice(&left_cv);
    out[node_at + 32..node_at + NODE_LEN].copy_from_slice(&right_cv);
    hazmat::merge_subtrees_non_root(&left_cv, &right_cv, Mode::Hash)
}

fn leaf_chaining_value(group_index: u64, bytes: &[u8]) -> ChainingValue {
    let mut hasher = blake3::Hasher::new();
    hasher.set_input_offset(group_index * GROUP_LEN);
    hasher.update(bytes);
    hasher.finalize_non_root()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Subtree {
    start_group: u64,
    group_count: u64,
    byte_len: u64,
}

impl Subtree {
    fn whole(object_len: u64) -> Self {
        Self {
            start_group: 0,
            group_count: object_len.div_ceil(GROUP_LEN),
            byte_len: object_len,
        }
    }

    fn parent_count(self) -> u64 {
        self.group_count.saturating_sub(1)
    }

    fn split(self) -> (Self, Self) {
        let left_len = hazmat::left_subtree_len(self.byte_len);
        let left_group_count = left_len / GROUP_LEN;
        let right_group_count = self.group_count - left_group_count;
        let right_len = self.byte_len - left_len;
        (
            Self {
                start_group: self.start_group,
                group_count: left_group_count,
                byte_len: left_len,
            },
            Self {
                start_group: self.start_group + left_group_count,
                group_count: right_group_count,
                byte_len: right_len,
            },
        )
    }

    fn byte_range(self) -> Range<u64> {
        let start = self.start_group * GROUP_LEN;
        start..start + self.byte_len
    }
}

fn overlaps(a: &Range<u64>, b: &Range<u64>) -> bool {
    a.start < b.end && b.start < a.end
}

fn corrupt(reason: impl Into<String>) -> Error {
    Error::new(ErrorKind::CacheCorrupt, reason)
}

fn range_mismatch(range: Range<u64>) -> Error {
    Error::new(
        ErrorKind::IntegrityRangeMismatch,
        format!(
            "bytes {}..{} do not match the outboard tree; refetch that range",
            range.start, range.end
        ),
    )
}

fn read_exact_at(
    outboard: &mut (impl Read + Seek),
    offset: u64,
    out: &mut [u8],
) -> Result<(), Error> {
    outboard.seek(SeekFrom::Start(offset)).map_err(|_| {
        corrupt("outboard could not be read; discard it and rebuild by a full rehash")
    })?;
    outboard.read_exact(out).map_err(|_| {
        corrupt(
            "outboard is shorter than its recorded shape; discard it and rebuild by a full rehash",
        )
    })
}

fn read_node(
    outboard: &mut (impl Read + Seek),
    offset: u64,
) -> Result<(ChainingValue, ChainingValue), Error> {
    let mut bytes = [0u8; NODE_LEN];
    read_exact_at(outboard, offset, &mut bytes)?;
    let mut left = [0u8; 32];
    let mut right = [0u8; 32];
    left.copy_from_slice(&bytes[..32]);
    right.copy_from_slice(&bytes[32..]);
    Ok((left, right))
}

#[derive(Clone, Copy)]
enum Expectation {
    Root(ContentDigest),
    Cv(ChainingValue),
}

/// Verifies that the bytes in `range` match the outboard tree, walking only
/// the nodes needed to authenticate that range.
///
/// Takes an outboard opened for reading and seeking, the object's recorded
/// length, its content digest, the half-open byte range to verify, and a
/// callback returning the raw bytes of a leaf group by its zero-based index.
/// Returns nothing on success.
///
/// # Errors
///
/// Returns `cache.corrupt` when the outboard's recorded length disagrees
/// with `object_len` or when the outboard cannot be read; nothing has been
/// authenticated yet in either case. Returns `integrity.range_mismatch`,
/// naming the byte range of the node that failed, when a node or a leaf does
/// not match the chaining value its parent named.
pub fn verify_range(
    outboard: &mut (impl Read + Seek),
    object_len: u64,
    content: ContentDigest,
    range: Range<u64>,
    group_bytes: &mut impl FnMut(u64) -> Vec<u8>,
) -> Result<(), Error> {
    let mut header = [0u8; HEADER_LEN];
    read_exact_at(outboard, 0, &mut header)?;
    let recorded_len = u64::from_le_bytes(header);
    if recorded_len != object_len {
        return Err(corrupt(format!(
            "outboard records length {recorded_len} but the object is {object_len} bytes; discard it and rebuild by a full rehash"
        )));
    }

    let whole = Subtree::whole(object_len);
    if whole.group_count <= 1 {
        return Ok(());
    }

    verify_subtree(
        outboard,
        HEADER_LEN as u64,
        &whole,
        Expectation::Root(content),
        &range,
        group_bytes,
    )
}

fn verify_subtree(
    outboard: &mut (impl Read + Seek),
    node_offset: u64,
    subtree: &Subtree,
    expectation: Expectation,
    range: &Range<u64>,
    group_bytes: &mut impl FnMut(u64) -> Vec<u8>,
) -> Result<(), Error> {
    let subtree_range = subtree.byte_range();
    if !overlaps(&subtree_range, range) {
        return Ok(());
    }

    if subtree.group_count == 1 {
        let bytes = group_bytes(subtree.start_group);
        let actual = leaf_chaining_value(subtree.start_group, &bytes);
        let expected = match expectation {
            Expectation::Cv(cv) => cv,
            Expectation::Root(_) => {
                unreachable!("a root expectation only reaches a subtree with more than one group")
            }
        };
        if actual != expected {
            return Err(range_mismatch(subtree_range));
        }
        return Ok(());
    }

    let (left_cv, right_cv) = read_node(outboard, node_offset)?;
    let matches = match expectation {
        Expectation::Cv(cv) => {
            hazmat::merge_subtrees_non_root(&left_cv, &right_cv, Mode::Hash) == cv
        }
        Expectation::Root(content) => {
            hazmat::merge_subtrees_root(&left_cv, &right_cv, Mode::Hash).as_bytes()
                == content.bytes()
        }
    };
    if !matches {
        return Err(range_mismatch(subtree_range));
    }

    let (left, right) = subtree.split();
    let left_offset = node_offset + NODE_LEN as u64;
    let right_offset = left_offset + NODE_LEN as u64 * left.parent_count();
    verify_subtree(
        outboard,
        left_offset,
        &left,
        Expectation::Cv(left_cv),
        range,
        group_bytes,
    )?;
    verify_subtree(
        outboard,
        right_offset,
        &right,
        Expectation::Cv(right_cv),
        range,
        group_bytes,
    )
}

fn usize_from_u64(value: u64) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}
