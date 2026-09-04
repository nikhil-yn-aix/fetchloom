//! The canonical entry stream and the tree and manifest digests over it.

use std::cmp::Ordering;

use crate::digest::{MANIFEST_DIGEST_CONTEXT, ManifestDigest, TREE_DIGEST_CONTEXT, TreeDigest};
use crate::tree::TreeEntry;

#[must_use]
pub fn compare(left: &[u8], right: &[u8]) -> Ordering {
    left.cmp(right)
}

#[must_use]
pub fn encode_entries(entries: &[TreeEntry]) -> Vec<u8> {
    let mut sorted: Vec<&TreeEntry> = entries.iter().collect();
    sorted.sort_by(|left, right| compare(left.path().as_bytes(), right.path().as_bytes()));

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

#[must_use]
pub fn tree_digest(entries: &[TreeEntry]) -> TreeDigest {
    let mut sorted: Vec<&TreeEntry> = entries.iter().collect();
    sorted.sort_by(|left, right| compare(left.path().as_bytes(), right.path().as_bytes()));

    let mut hasher = blake3::Hasher::new_derive_key(TREE_DIGEST_CONTEXT);
    let mut entry_bytes = Vec::new();
    for entry in sorted {
        entry_bytes.clear();
        encode_entry(entry, &mut entry_bytes);
        hasher.update(&entry_bytes);
    }
    TreeDigest::from_bytes(*hasher.finalize().as_bytes())
}

#[must_use]
pub fn manifest_digest(canonical_json: &[u8]) -> ManifestDigest {
    let mut hasher = blake3::Hasher::new_derive_key(MANIFEST_DIGEST_CONTEXT);
    hasher.update(canonical_json);
    ManifestDigest::from_bytes(*hasher.finalize().as_bytes())
}
