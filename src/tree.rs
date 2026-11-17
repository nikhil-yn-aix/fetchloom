//! the identity of a dataset's effective tree: the files a pipeline reading its folder sees, after
//! archives unpack and members tables choose, by their paths in the dataset, sizes and SHA-256.
//! data.lock keeps it for each dataset another was made from, so nothing else may change it: not
//! a link, a time, a note or where the dataset lands.
//!
//! version 1 is the SHA-256 of the line `fetchloom tree 1`, then a line per file sorted by path,
//! its path, size and SHA-256 in hex between tabs, as an archive's `unpacks_to` lists its files.
//! a later version starts with its own line, so identities of two versions never meet. no path
//! in a pin holds a tab or a line break, so the text reads back one way only.

use std::fmt;

use crate::digest::Sha256;

/// the SHA-256 of a tree's text, as data.lock writes it: `sha256:` and 64 hex digits.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct Identity(Sha256);

impl Identity {
    /// the identity `text` gives as data.lock writes it, `sha256:` and 64 hex digits.
    pub(crate) fn parse(text: &str) -> Option<Identity> {
        let hex = text.strip_prefix("sha256:")?;
        (hex.len() == 64).then_some(())?;
        Sha256::from_hex(hex).map(Identity)
    }
}

impl fmt::Display for Identity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "sha256:{}", self.0)
    }
}

impl fmt::Debug for Identity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "tree {self}")
    }
}
