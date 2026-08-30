//! Where every kind of cache entry lives.

use std::path::{Path, PathBuf};

use fetchloom_engine::digest::ContentDigest;

/// The directories a cache root holds.
pub(crate) const DIRECTORIES: [&str; 9] = [
    "objects",
    "receipts",
    "outboard",
    "partial",
    "staging",
    "quarantine",
    "meta",
    "locks",
    "pins",
];

/// The paths of one cache root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    root: PathBuf,
}

impl Layout {
    /// Names the paths under a cache root.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Returns the cache root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Returns the directory holding completed objects.
    #[must_use]
    pub fn objects(&self) -> PathBuf {
        self.root.join("objects")
    }

    /// Returns the directory holding chunk trees.
    #[must_use]
    pub fn outboard(&self) -> PathBuf {
        self.root.join("outboard")
    }

    /// Returns the directory holding in-progress transfers.
    #[must_use]
    pub fn partial(&self) -> PathBuf {
        self.root.join("partial")
    }

    /// Returns the directory holding extraction trees not yet published.
    #[must_use]
    pub fn staging(&self) -> PathBuf {
        self.root.join("staging")
    }

    /// Returns the directory holding objects that failed verification.
    #[must_use]
    pub fn quarantine(&self) -> PathBuf {
        self.root.join("quarantine")
    }

    /// Returns the quarantined object with the given digest.
    #[must_use]
    pub fn quarantined(&self, digest: ContentDigest) -> PathBuf {
        self.quarantine().join(name_of(digest))
    }

    /// Returns the diagnosis written beside a quarantined object.
    #[must_use]
    pub fn diagnosis_of(&self, digest: ContentDigest) -> PathBuf {
        self.quarantine()
            .join(format!("{}.diagnosis", name_of(digest)))
    }

    /// Returns the directory holding receipts.
    #[must_use]
    pub fn receipts(&self) -> PathBuf {
        self.root.join("receipts")
    }

    /// Returns the receipt stored under the given key.
    #[must_use]
    pub fn receipt_of(&self, key: ContentDigest) -> PathBuf {
        self.receipts().join(name_of(key))
    }

    /// Returns the directory holding resolution metadata.
    #[must_use]
    pub fn meta(&self) -> PathBuf {
        self.root.join("meta")
    }

    /// Returns the directory holding advisory locks.
    #[must_use]
    pub fn locks(&self) -> PathBuf {
        self.root.join("locks")
    }

    /// Returns the directory holding pin records.
    #[must_use]
    pub fn pins(&self) -> PathBuf {
        self.root.join("pins")
    }

    /// Returns the file holding the cache format fingerprint.
    #[must_use]
    pub fn format(&self) -> PathBuf {
        self.root.join("format")
    }

    /// Returns the file holding the boot of the last recovery.
    #[must_use]
    pub fn recovered(&self) -> PathBuf {
        self.meta().join("recovered")
    }

    /// Returns the directory holding prune marks.
    #[must_use]
    pub fn marks(&self) -> PathBuf {
        self.meta().join("prune")
    }

    /// Returns the completed object with the given digest.
    #[must_use]
    pub fn object(&self, digest: ContentDigest) -> PathBuf {
        self.objects().join(name_of(digest))
    }

    /// Returns the chunk tree of the object with the given digest.
    #[must_use]
    pub fn outboard_of(&self, digest: ContentDigest) -> PathBuf {
        self.outboard().join(name_of(digest))
    }

    /// Returns the in-progress transfer of the object with the given digest.
    #[must_use]
    pub fn partial_of(&self, digest: ContentDigest) -> PathBuf {
        self.partial().join(name_of(digest))
    }

    /// Returns the lock file of the object with the given digest.
    #[must_use]
    pub fn lock_of(&self, digest: ContentDigest) -> PathBuf {
        self.locks().join(format!("{}.lock", name_of(digest)))
    }

    /// Returns the owner record of the lock on the given digest.
    #[must_use]
    pub fn lock_owner_of(&self, digest: ContentDigest) -> PathBuf {
        self.locks().join(format!("{}.owner", name_of(digest)))
    }

    /// Returns the pin record of the object with the given digest.
    #[must_use]
    pub fn pin_of(&self, digest: ContentDigest) -> PathBuf {
        self.pins().join(name_of(digest))
    }

    /// Returns the directory holding one record per published object.
    #[must_use]
    pub fn records(&self) -> PathBuf {
        self.meta().join("object")
    }

    /// Returns the directory holding one record per artifact key.
    #[must_use]
    pub fn witnesses(&self) -> PathBuf {
        self.meta().join("witness")
    }

    /// Returns the witnesses recorded for one artifact key.
    #[must_use]
    pub fn witness_of(&self, key: &fetchloom_engine::trust::ArtifactKey) -> PathBuf {
        self.witnesses().join(hexadecimal(key.bytes()))
    }

    /// Returns the directory holding one record per reference resolved.
    #[must_use]
    pub fn resolutions(&self) -> PathBuf {
        self.meta().join("resolution")
    }

    /// Returns what a reference last resolved to.
    #[must_use]
    pub fn resolution_of(&self, key: &[u8; 32]) -> PathBuf {
        self.resolutions().join(hexadecimal(key))
    }

    /// Returns the prune mark of the object with the given digest.
    #[must_use]
    pub fn mark_of(&self, digest: ContentDigest) -> PathBuf {
        self.marks().join(name_of(digest))
    }
}

/// Returns the file name a digest is stored under.
///
/// The text form of a digest carries its algorithm and a colon, which is not a
/// name every filesystem accepts, so the name is the hexadecimal alone.
#[must_use]
pub fn name_of(digest: ContentDigest) -> String {
    hexadecimal(digest.bytes())
}

/// Returns the lowercase hexadecimal of some bytes, which is how every key in
/// the cache becomes a file name every filesystem accepts.
fn hexadecimal(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut name = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(name, "{byte:02x}");
    }
    name
}

/// Returns the digest a file name stands for.
///
/// Returns nothing when the name is not the hexadecimal of a digest, which is
/// how a foreign file in the cache is ignored rather than misread.
#[must_use]
pub fn digest_of(name: &str) -> Option<ContentDigest> {
    if name.len() != 64 {
        return None;
    }
    let mut bytes = [0u8; 32];
    for (index, pair) in name.as_bytes().as_chunks::<2>().0.iter().enumerate() {
        let text = std::str::from_utf8(pair).ok()?;
        bytes[index] = u8::from_str_radix(text, 16).ok()?;
    }
    Some(ContentDigest::from_bytes(bytes))
}
