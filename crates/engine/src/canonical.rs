//! The canonical entry stream and the tree and manifest digests over it.

use crate::digest::{MANIFEST_DIGEST_CONTEXT, ManifestDigest, TREE_DIGEST_CONTEXT, TreeDigest};
use crate::tree::TreeEntry;

/// Encodes entries into the canonical byte stream the tree digest is taken
/// over.
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
#[must_use]
pub fn manifest_digest(canonical_json: &[u8]) -> ManifestDigest {
    let mut hasher = blake3::Hasher::new_derive_key(MANIFEST_DIGEST_CONTEXT);
    hasher.update(canonical_json);
    ManifestDigest::from_bytes(*hasher.finalize().as_bytes())
}
