//! The canonical entry stream and the tree and manifest digests over it.

use crate::digest::{MANIFEST_DIGEST_CONTEXT, ManifestDigest, TREE_DIGEST_CONTEXT, TreeDigest};
use crate::tree::TreeEntry;

/// Encodes entries into the canonical byte stream the tree digest is taken
/// over.
///
/// Entries are sorted ascending by their raw path bytes before encoding, so
/// the order they are given in does not affect the result. Each entry is
/// framed by its type tag, an eight-byte little-endian path length, the path
/// bytes, and then the fixed field list its type carries; a field that does
/// not apply to a type is absent rather than a placeholder value.
#[must_use]
pub fn encode_entries(entries: &[TreeEntry]) -> Vec<u8> {
    let mut sorted: Vec<&TreeEntry> = entries.iter().collect();
    sorted.sort_by(|left, right| left.path().as_bytes().cmp(right.path().as_bytes()));

    let mut stream = Vec::new();
    for entry in sorted {
        encode_entry(entry, &mut stream);
    }
    stream
}

fn encode_entry(entry: &TreeEntry, out: &mut Vec<u8>) {
    out.push(entry.tag());
    let path = entry.path().as_bytes();
    out.extend_from_slice(&(path.len() as u64).to_le_bytes());
    out.extend_from_slice(path);
    match entry {
        TreeEntry::File {
            mode,
            size,
            content,
            ..
        } => {
            out.extend_from_slice(&mode.bits().to_le_bytes());
            out.extend_from_slice(&size.to_le_bytes());
            out.extend_from_slice(content.bytes());
        }
        TreeEntry::Directory { .. } => {}
        TreeEntry::Symlink { size, content, .. } => {
            out.extend_from_slice(&size.to_le_bytes());
            out.extend_from_slice(content.bytes());
        }
    }
}

/// Computes the tree digest of a materialized directory from its entries.
///
/// Takes every entry the directory contains, in any order. Returns the
/// BLAKE3 `derive_key` digest of their canonical encoding, domain-separated
/// from every other digest this crate produces.
#[must_use]
pub fn tree_digest(entries: &[TreeEntry]) -> TreeDigest {
    let mut sorted: Vec<&TreeEntry> = entries.iter().collect();
    sorted.sort_by(|left, right| left.path().as_bytes().cmp(right.path().as_bytes()));

    let mut hasher = blake3::Hasher::new_derive_key(TREE_DIGEST_CONTEXT);
    let mut entry_bytes = Vec::new();
    for entry in sorted {
        entry_bytes.clear();
        encode_entry(entry, &mut entry_bytes);
        hasher.update(&entry_bytes);
    }
    TreeDigest::from_bytes(*hasher.finalize().as_bytes())
}

/// Computes the manifest digest over a manifest's canonical JSON bytes.
///
/// Takes the canonical JSON encoding of a manifest, never its source text.
/// Returns the BLAKE3 `derive_key` digest of those bytes, domain-separated
/// from every other digest this crate produces.
#[must_use]
pub fn manifest_digest(canonical_json: &[u8]) -> ManifestDigest {
    let mut hasher = blake3::Hasher::new_derive_key(MANIFEST_DIGEST_CONTEXT);
    hasher.update(canonical_json);
    ManifestDigest::from_bytes(*hasher.finalize().as_bytes())
}
