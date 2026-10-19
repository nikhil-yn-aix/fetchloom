//! Where things live inside a tier.

use std::path::{Path, PathBuf};

use fl_model::digest::{Algorithm, Digest};

use crate::error::StoreError;

/// The directories of a tier, all below `<tier>/v1`.
pub(crate) const DIRECTORIES: [&str; 6] = ["objects", "tmp", "partial", "trees", "steps", "roots"];

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
}
