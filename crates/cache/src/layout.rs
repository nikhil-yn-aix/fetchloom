//! Where every kind of cache entry lives.

use std::path::{Path, PathBuf};

use fetchloom_engine::digest::ContentDigest;

pub(crate) const DIRECTORIES: [&str; 10] = [
    "objects",
    "packs",
    "receipts",
    "outboard",
    "partial",
    "staging",
    "quarantine",
    "meta",
    "locks",
    "pins",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    root: PathBuf,
}

impl Layout {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub fn objects(&self) -> PathBuf {
        self.root.join("objects")
    }

    #[must_use]
    pub fn outboard(&self) -> PathBuf {
        self.root.join("outboard")
    }

    #[must_use]
    pub fn partial(&self) -> PathBuf {
        self.root.join("partial")
    }

    #[must_use]
    pub fn staging(&self) -> PathBuf {
        self.root.join("staging")
    }

    #[must_use]
    pub fn quarantine(&self) -> PathBuf {
        self.root.join("quarantine")
    }

    #[must_use]
    pub fn quarantined(&self, digest: ContentDigest) -> PathBuf {
        self.quarantine().join(name_of(digest))
    }

    #[must_use]
    pub fn diagnosis_of(&self, digest: ContentDigest) -> PathBuf {
        self.quarantine()
            .join(format!("{}.diagnosis", name_of(digest)))
    }

    #[must_use]
    pub fn receipts(&self) -> PathBuf {
        self.root.join("receipts")
    }

    #[must_use]
    pub fn receipt_of(&self, key: ContentDigest) -> PathBuf {
        self.receipts().join(name_of(key))
    }

    #[must_use]
    pub fn meta(&self) -> PathBuf {
        self.root.join("meta")
    }

    #[must_use]
    pub fn packs(&self) -> PathBuf {
        self.root.join("packs")
    }

    #[must_use]
    pub fn locks(&self) -> PathBuf {
        self.root.join("locks")
    }

    #[must_use]
    pub fn pins(&self) -> PathBuf {
        self.root.join("pins")
    }

    #[must_use]
    pub fn format(&self) -> PathBuf {
        self.root.join("format")
    }

    #[must_use]
    pub fn recovered(&self) -> PathBuf {
        self.meta().join("recovered")
    }

    #[must_use]
    pub fn marks(&self) -> PathBuf {
        self.meta().join("prune")
    }

    #[must_use]
    pub fn object(&self, digest: ContentDigest) -> PathBuf {
        self.objects().join(name_of(digest))
    }

    #[must_use]
    pub fn outboard_of(&self, digest: ContentDigest) -> PathBuf {
        self.outboard().join(name_of(digest))
    }

    #[must_use]
    pub fn partial_of(&self, digest: ContentDigest) -> PathBuf {
        self.partial().join(name_of(digest))
    }

    #[must_use]
    pub fn lock_of(&self, digest: ContentDigest) -> PathBuf {
        self.locks().join(format!("{}.lock", name_of(digest)))
    }

    #[must_use]
    pub fn lock_owner_of(&self, digest: ContentDigest) -> PathBuf {
        self.locks().join(format!("{}.owner", name_of(digest)))
    }

    #[must_use]
    pub fn pin_of(&self, digest: ContentDigest) -> PathBuf {
        self.pins().join(name_of(digest))
    }

    #[must_use]
    pub fn records(&self) -> PathBuf {
        self.meta().join("object")
    }

    #[must_use]
    pub fn witnesses(&self) -> PathBuf {
        self.meta().join("witness")
    }

    #[must_use]
    pub fn witness_of(&self, key: &fetchloom_engine::trust::ArtifactKey) -> PathBuf {
        self.witnesses().join(hexadecimal(key.bytes()))
    }

    #[must_use]
    pub fn measurements(&self) -> PathBuf {
        self.meta().join("host")
    }

    #[must_use]
    pub fn measurement_of(&self, key: &[u8; 32]) -> PathBuf {
        self.measurements().join(hexadecimal(key))
    }

    #[must_use]
    pub fn resolutions(&self) -> PathBuf {
        self.meta().join("resolution")
    }

    #[must_use]
    pub fn resolution_of(&self, key: &[u8; 32]) -> PathBuf {
        self.resolutions().join(hexadecimal(key))
    }

    #[must_use]
    pub fn mark_of(&self, digest: ContentDigest) -> PathBuf {
        self.marks().join(name_of(digest))
    }
}

#[must_use]
pub fn name_of(digest: ContentDigest) -> String {
    hexadecimal(digest.bytes())
}

fn hexadecimal(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut name = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(name, "{byte:02x}");
    }
    name
}

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
