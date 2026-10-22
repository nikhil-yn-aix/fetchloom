//! Where things live inside a tier.

use std::fs;
use std::path::{Path, PathBuf};

use fl_model::digest::{Algorithm, Digest};

use crate::error::{StoreError, io};

/// The directories of a tier, all below `<tier>/v1`.
pub(crate) const DIRECTORIES: [&str; 7] = [
    "objects", "tmp", "partial", "trees", "steps", "roots", "index",
];

const BLAKE3_PREFIX: &str = "blake3:";

/// The paths of one tier.
#[derive(Clone, Debug)]
pub(crate) struct Layout {
    root: PathBuf,
}

impl Layout {
    /// The layout of the tier at `tier`.
    pub(crate) fn new(tier: &Path) -> Self {
        Self {
            root: tier.join("v1"),
        }
    }

    /// `<tier>/v1`.
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// One of [`DIRECTORIES`].
    pub(crate) fn dir(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    /// The record in directory `dir` named by `digest`'s hex, such as a tree object or a step
    /// record.
    pub(crate) fn named(&self, dir: &str, digest: &Digest) -> PathBuf {
        let text = digest.to_string();
        let hex = text.split_once(':').map_or(text.as_str(), |(_, hex)| hex);
        self.dir(dir).join(hex)
    }

    /// Where the index entry for the stated `digest` lives: its algorithm, two hex characters of
    /// fan out, then the rest. BLAKE3 digests name objects directly and have no entry.
    pub(crate) fn indexed(&self, digest: &Digest) -> Option<PathBuf> {
        if matches!(digest, Digest::Blake3(_)) {
            return None;
        }
        let text = digest.to_string();
        let (algorithm, hex) = text.split_once(':')?;
        let (fan, rest) = hex.split_at(2);
        Some(self.dir("index").join(algorithm).join(fan).join(rest))
    }

    /// Where the object with BLAKE3 digest `blake3` lives: two hex characters of fan out, then the
    /// other sixty two.
    pub(crate) fn object(&self, blake3: &[u8; 32]) -> PathBuf {
        let text = Digest::Blake3(*blake3).to_string();
        let (fan, rest) = text[BLAKE3_PREFIX.len()..].split_at(2);
        let mut path = self.dir("objects");
        path.push(fan);
        path.push(rest);
        path
    }
}

/// The entries of a tier directory, without hidden names: an NFS client leaves `.nfs` files
/// beside a file deleted while open, and they are not the store's to judge.
pub(crate) fn entries(dir: &Path) -> Result<Vec<PathBuf>, StoreError> {
    let mut found = Vec::new();
    for entry in fs::read_dir(dir).map_err(io("list", dir))? {
        let entry = entry.map_err(io("list", dir))?;
        if !entry.file_name().as_encoded_bytes().starts_with(b".") {
            found.push(entry.path());
        }
    }
    Ok(found)
}

/// The digest an object file is named by, from its fan out directory and its file name.
pub(crate) fn object_digest(fan: &Path, object: &Path) -> Result<Digest, StoreError> {
    let hex = [fan, object]
        .iter()
        .map(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("")
        })
        .collect::<String>();
    Digest::from_hex(Algorithm::Blake3, &hex).map_err(|_| StoreError::Foreign {
        path: object.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn objects_fan_out_on_their_first_two_hex_characters() {
        let layout = Layout::new(Path::new("tier"));
        let mut blake3 = [0x11; 32];
        blake3[0] = 0x9f;
        let expected = Path::new("tier")
            .join("v1")
            .join("objects")
            .join("9f")
            .join("1".repeat(62));
        assert_eq!(layout.object(&blake3), expected);
    }

    #[test]
    fn index_entries_fan_out_by_algorithm_and_first_two_hex_characters() {
        let layout = Layout::new(Path::new("tier"));
        let expected = Path::new("tier")
            .join("v1")
            .join("index")
            .join("md5")
            .join("ab")
            .join("ab".repeat(15));
        assert_eq!(layout.indexed(&Digest::Md5([0xab; 16])), Some(expected));
        assert_eq!(layout.indexed(&Digest::Blake3([0xab; 32])), None);
    }

    #[test]
    fn entries_leave_out_hidden_names() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join(".nfs0001"), b"").unwrap();
        fs::write(temp.path().join("ab"), b"").unwrap();
        assert_eq!(entries(temp.path()).unwrap(), [temp.path().join("ab")]);
    }
}
