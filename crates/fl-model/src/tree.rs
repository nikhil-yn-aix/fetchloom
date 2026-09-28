//! Trees: the files of a dataset, and the hash that is its identity.

use crate::canonical::Encoder;
use crate::digest::Digest;
use crate::path::DataPath;

/// One file of a tree.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TreeEntry {
    /// Where the file is inside the dataset.
    pub path: DataPath,
    /// Its size in bytes.
    pub size: u64,
    /// Its BLAKE3 digest.
    pub blake3: [u8; 32],
}

/// The files of a dataset after extraction and filtering, sorted by path, each path once.
///
/// Its hash is the identity of the dataset, the same on every platform.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Tree {
    entries: Vec<TreeEntry>,
}

impl Tree {
    /// Sorts `entries` by path bytes.
    ///
    /// # Errors
    ///
    /// Returns [`TreeError::Duplicate`] when a path appears more than once.
    pub fn new(mut entries: Vec<TreeEntry>) -> Result<Self, TreeError> {
        if !entries.is_sorted_by(|a, b| a.path <= b.path) {
            entries.sort_unstable_by(|a, b| a.path.cmp(&b.path));
        }
        if let Some(pair) = entries.windows(2).find(|pair| pair[0].path == pair[1].path) {
            return Err(TreeError::Duplicate(pair[0].path.clone()));
        }
        Ok(Self { entries })
    }

    /// The entries, sorted by path.
    #[must_use]
    pub fn entries(&self) -> &[TreeEntry] {
        &self.entries
    }

    /// The tree hash: BLAKE3 under the context `fetchloom tree v1` over, for each entry in order,
    /// the path length as a little endian u64, the path bytes, the size as a little endian u64
    /// and the 32 digest bytes.
    #[must_use]
    pub fn hash(&self) -> Digest {
        let mut encoder = Encoder::new("fetchloom tree v1");
        for entry in &self.entries {
            encoder
                .str(entry.path.as_str())
                .u64(entry.size)
                .fixed(&entry.blake3);
        }
        Digest::Blake3(encoder.finish())
    }
}

/// Why entries do not make a tree.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TreeError {
    /// Two entries share a path.
    #[error("path `{0}` appears twice in one dataset")]
    Duplicate(DataPath),
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn entry(path: &str, size: u64, byte: u8) -> TreeEntry {
        TreeEntry {
            path: DataPath::new(path).unwrap(),
            size,
            blake3: [byte; 32],
        }
    }

    #[test]
    fn sorts_entries_by_path_bytes() {
        let tree = Tree::new(vec![entry("b", 1, 1), entry("a/b", 2, 2), entry("a", 3, 3)]).unwrap();
        let paths: Vec<&str> = tree.entries().iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, ["a", "a/b", "b"]);
    }

    #[test]
    fn refuses_a_path_listed_twice() {
        let err =
            Tree::new(vec![entry("a", 1, 1), entry("b", 1, 1), entry("a", 2, 2)]).unwrap_err();
        assert_eq!(err.to_string(), "path `a` appears twice in one dataset");
    }

    #[test]
    fn hash_is_blake3_over_length_prefixed_fields_under_the_tree_context() {
        let tree = Tree::new(vec![entry("b.txt", 7, 2), entry("a", 1834, 1)]).unwrap();
        let mut bytes = Vec::new();
        for (path, size, byte) in [("a", 1834_u64, 1_u8), ("b.txt", 7, 2)] {
            bytes.extend_from_slice(&(path.len() as u64).to_le_bytes());
            bytes.extend_from_slice(path.as_bytes());
            bytes.extend_from_slice(&size.to_le_bytes());
            bytes.extend_from_slice(&[byte; 32]);
        }
        let expected = blake3::Hasher::new_derive_key("fetchloom tree v1")
            .update(&bytes)
            .finalize();
        assert_eq!(tree.hash(), Digest::Blake3(*expected.as_bytes()));
        assert_eq!(
            tree.hash().to_string(),
            "blake3:e27c52e63e853bafe8c336b0e078a195b1d20f8ac393630886767c07ae7902dd"
        );
    }

    #[test]
    fn hashes_trees_larger_than_one_buffer() {
        let entries: Vec<TreeEntry> = (0..5000)
            .map(|i| entry(&format!("dir/file-{i:05}.bin"), i, 9))
            .collect();
        let tree = Tree::new(entries).unwrap();
        let mut bytes = Vec::new();
        for e in tree.entries() {
            bytes.extend_from_slice(&(e.path.as_str().len() as u64).to_le_bytes());
            bytes.extend_from_slice(e.path.as_str().as_bytes());
            bytes.extend_from_slice(&e.size.to_le_bytes());
            bytes.extend_from_slice(&e.blake3);
        }
        assert!(bytes.len() > 128 * 1024);
        let expected = blake3::Hasher::new_derive_key("fetchloom tree v1")
            .update(&bytes)
            .finalize();
        assert_eq!(tree.hash(), Digest::Blake3(*expected.as_bytes()));
    }

    #[test]
    fn empty_and_different_trees_hash_differently() {
        let empty = Tree::new(Vec::new()).unwrap().hash();
        let one = Tree::new(vec![entry("a", 1, 1)]).unwrap().hash();
        let size = Tree::new(vec![entry("a", 2, 1)]).unwrap().hash();
        let content = Tree::new(vec![entry("a", 1, 2)]).unwrap().hash();
        let path = Tree::new(vec![entry("b", 1, 1)]).unwrap().hash();
        let split = Tree::new(vec![entry("ab", 1, 1)]).unwrap().hash();
        let hashes = [empty, one, size, content, path, split];
        for (i, a) in hashes.iter().enumerate() {
            for b in &hashes[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }

    fn files_and_a_shuffle() -> impl Strategy<Value = (Vec<TreeEntry>, Vec<TreeEntry>)> {
        let files = prop::collection::btree_map(
            "[a-z]{1,6}(/[a-z]{1,6}){0,3}",
            (any::<u64>(), any::<u8>()),
            0..40,
        );
        files.prop_flat_map(|files| {
            let entries: Vec<TreeEntry> = files
                .iter()
                .map(|(path, &(size, byte))| entry(path, size, byte))
                .collect();
            (Just(entries.clone()), Just(entries).prop_shuffle())
        })
    }

    proptest! {
        #[test]
        fn any_order_of_the_same_entries_hashes_the_same((entries, shuffled) in files_and_a_shuffle()) {
            let sorted = Tree::new(entries).unwrap().hash();
            prop_assert_eq!(Tree::new(shuffled).unwrap().hash(), sorted);
        }
    }
}
