//! The outboard chunk tree, its construction, and its range verification walk.

use std::io::{Read, Seek, SeekFrom};
use std::ops::Range;

use blake3::hazmat::{self, ChainingValue, HasherExt, Mode};

use crate::digest::ContentDigest;
use crate::error::{Error, ErrorKind};
use crate::limits::{OUTBOARD_CHUNK_GROUP, OUTBOARD_THRESHOLD};

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
    /// Returns the encoded outboard bytes: an eight-byte little-endian length
    /// followed by sixty-four-byte parent nodes in pre-order.
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

/// Returns the content digest of an object and, when it is large enough, its
/// outboard tree, from the chaining value of each of its leaf groups.
///
/// # Panics
///
/// Panics when `leaves` does not hold exactly one entry per group, or when the
/// object holds one group or none, both of which are bugs in the caller rather
/// than inputs to validate. An object of one group has no parent node to build
/// a digest out of and is hashed directly.
#[must_use]
pub fn tree_of(object_len: u64, leaves: &[ChainingValue]) -> (ContentDigest, Option<Outboard>) {
    let whole = Subtree::whole(object_len);
    assert!(
        whole.group_count > 1,
        "an object of one group or none is hashed directly rather than merged"
    );
    assert_eq!(
        leaves.len() as u64,
        whole.group_count,
        "one leaf chaining value per group"
    );

    let mut bytes =
        Vec::with_capacity(HEADER_LEN + NODE_LEN * usize_from_u64(whole.parent_count()));
    bytes.extend_from_slice(&object_len.to_le_bytes());
    let (left, right) = whole.split();
    let node_at = bytes.len();
    bytes.extend_from_slice(&[0u8; NODE_LEN]);
    let left_cv = write_subtree(&left, leaves, &mut bytes);
    let right_cv = write_subtree(&right, leaves, &mut bytes);
    bytes[node_at..node_at + 32].copy_from_slice(&left_cv);
    bytes[node_at + 32..node_at + NODE_LEN].copy_from_slice(&right_cv);

    let root = hazmat::merge_subtrees_root(&left_cv, &right_cv, Mode::Hash);
    let digest = ContentDigest::from_bytes(*root.as_bytes());
    let outboard = (object_len > OUTBOARD_THRESHOLD).then_some(Outboard { bytes });
    (digest, outboard)
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

/// Fills the raw bytes of one leaf group into a buffer the walk owns and
/// reuses.
type GroupBytes<'a> = &'a mut dyn FnMut(u64, &mut Vec<u8>) -> Result<(), Error>;

/// Why a walk stopped before it finished.
enum Stopped {
    /// The tree or the object could not be read.
    Unreadable(Error),
    /// A node of the tree does not check out against the digest.
    TreeCorrupt(Range<u64>),
}

impl From<Error> for Stopped {
    fn from(error: Error) -> Self {
        Self::Unreadable(error)
    }
}

fn tree_corrupt(range: &Range<u64>) -> Error {
    corrupt(format!(
        "discard the tree and rebuild it, because the node covering bytes {}..{} does not check out against the digest, so it says nothing about the object",
        range.start, range.end
    ))
}

/// Verifies that the bytes in `range` match the outboard tree, walking only the
/// nodes needed to authenticate that range.
///
/// # Errors
///
/// Returns `cache.corrupt` when the outboard's recorded length disagrees with
/// `object_len` or when the outboard cannot be read; nothing has been
/// authenticated yet in either case. Returns `integrity.range_mismatch`, naming
/// the byte range of the node that failed, when a node or a leaf does not match
/// the chaining value its parent named.
pub fn verify_range(
    outboard: &mut (impl Read + Seek),
    object_len: u64,
    content: ContentDigest,
    range: Range<u64>,
    group_bytes: GroupBytes<'_>,
) -> Result<(), Error> {
    let mut damaged = Vec::new();
    let mut buffer = Vec::with_capacity(usize_from_u64(GROUP_LEN));
    match walk(
        outboard,
        object_len,
        content,
        &range,
        group_bytes,
        &mut buffer,
        &mut damaged,
    ) {
        Ok(()) => {}
        Err(Stopped::Unreadable(error)) => return Err(error),
        Err(Stopped::TreeCorrupt(at)) => return Err(range_mismatch(at)),
    }
    match damaged.first() {
        None => Ok(()),
        Some(group) => Err(range_mismatch(group_range(*group, object_len))),
    }
}

/// Returns every byte range of an object whose bytes do not match the tree,
/// ascending, with adjacent ranges merged.
///
/// # Errors
///
/// Returns `cache.corrupt` when the tree cannot be read, when its recorded
/// length disagrees with `object_len`, and when any node in it does not check
/// out against the digest. Returns whatever the callback fails with.
pub fn find_damage(
    outboard: &mut (impl Read + Seek),
    object_len: u64,
    content: ContentDigest,
    group_bytes: GroupBytes<'_>,
) -> Result<Vec<Range<u64>>, Error> {
    let mut damaged = Vec::new();
    let mut buffer = Vec::with_capacity(usize_from_u64(GROUP_LEN));
    match walk(
        outboard,
        object_len,
        content,
        &(0..object_len),
        group_bytes,
        &mut buffer,
        &mut damaged,
    ) {
        Ok(()) => Ok(merge(&damaged, object_len)),
        Err(Stopped::Unreadable(error)) => Err(error),
        Err(Stopped::TreeCorrupt(at)) => Err(tree_corrupt(&at)),
    }
}

/// Turns ascending group indices into ascending byte ranges, joining ones that
/// touch.
fn merge(groups: &[u64], object_len: u64) -> Vec<Range<u64>> {
    let mut spans: Vec<Range<u64>> = Vec::new();
    for group in groups {
        let range = group_range(*group, object_len);
        match spans.last_mut() {
            Some(last) if last.end == range.start => last.end = range.end,
            _ => spans.push(range),
        }
    }
    spans
}

fn group_range(group: u64, object_len: u64) -> Range<u64> {
    let start = group * GROUP_LEN;
    start..(start + GROUP_LEN).min(object_len)
}

fn walk(
    outboard: &mut (impl Read + Seek),
    object_len: u64,
    content: ContentDigest,
    range: &Range<u64>,
    group_bytes: GroupBytes<'_>,
    buffer: &mut Vec<u8>,
    damaged: &mut Vec<u64>,
) -> Result<(), Stopped> {
    let mut header = [0u8; HEADER_LEN];
    read_exact_at(outboard, 0, &mut header)?;
    let recorded_len = u64::from_le_bytes(header);
    if recorded_len != object_len {
        return Err(Stopped::Unreadable(corrupt(format!(
            "outboard records length {recorded_len} but the object is {object_len} bytes; discard it and rebuild by a full rehash"
        ))));
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
        &mut Descent {
            range,
            group_bytes,
            buffer,
            damaged,
        },
    )
}

/// Everything the descent carries down the tree that is not the node itself.
struct Descent<'a> {
    /// The byte range the walk is filtered to.
    range: &'a Range<u64>,
    /// Where the bytes of one leaf group come from.
    group_bytes: GroupBytes<'a>,
    /// The buffer those bytes are read into, allocated once.
    buffer: &'a mut Vec<u8>,
    /// The leaf groups found not to match, ascending.
    damaged: &'a mut Vec<u64>,
}

fn verify_subtree(
    outboard: &mut (impl Read + Seek),
    node_offset: u64,
    subtree: &Subtree,
    expectation: Expectation,
    descent: &mut Descent<'_>,
) -> Result<(), Stopped> {
    let subtree_range = subtree.byte_range();
    if !overlaps(&subtree_range, descent.range) {
        return Ok(());
    }

    if subtree.group_count == 1 {
        return check_leaf(subtree.start_group, expectation, descent);
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
        return Err(Stopped::TreeCorrupt(subtree_range));
    }

    let (left, right) = subtree.split();
    let left_offset = node_offset + NODE_LEN as u64;
    let right_offset = left_offset + NODE_LEN as u64 * left.parent_count();
    verify_subtree(
        outboard,
        left_offset,
        &left,
        Expectation::Cv(left_cv),
        descent,
    )?;
    verify_subtree(
        outboard,
        right_offset,
        &right,
        Expectation::Cv(right_cv),
        descent,
    )
}

/// Checks one leaf group against the chaining value its parent stores for it.
fn check_leaf(
    group: u64,
    expectation: Expectation,
    descent: &mut Descent<'_>,
) -> Result<(), Stopped> {
    (descent.group_bytes)(group, descent.buffer)?;
    let Expectation::Cv(expected) = expectation else {
        unreachable!("a root expectation only reaches a subtree with more than one group")
    };
    if descent.buffer.is_empty() {
        descent.damaged.push(group);
        return Ok(());
    }
    if leaf_chaining_value(group, descent.buffer) != expected {
        descent.damaged.push(group);
    }
    Ok(())
}

fn usize_from_u64(value: u64) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}
