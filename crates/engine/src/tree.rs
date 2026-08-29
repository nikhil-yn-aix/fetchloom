//! The entries a materialized directory is made of.

use std::fmt;

use serde::Serialize;

use crate::digest::ContentDigest;

/// The permission bits an entry may carry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(into = "u32")]
pub enum Mode {
    /// Readable and writable by its owner, readable by everyone.
    ReadWrite,
    /// Readable, writable, and executable by its owner, readable and
    /// executable by everyone.
    Executable,
}

impl Mode {
    /// Reduces a source mode to the only two modes a tree digest records.
    ///
    /// Takes the mode an archive or a source file carried. Returns the
    /// executable mode when any execute bit is set and the read and write mode
    /// otherwise.
    #[must_use]
    pub fn reduce(source_mode: u32) -> Self {
        if source_mode & 0o111 == 0 {
            Self::ReadWrite
        } else {
            Self::Executable
        }
    }

    /// Returns the permission bits this mode stands for.
    #[must_use]
    pub fn bits(self) -> u32 {
        match self {
            Self::ReadWrite => 0o644,
            Self::Executable => 0o755,
        }
    }
}

impl From<Mode> for u32 {
    fn from(mode: Mode) -> Self {
        mode.bits()
    }
}

/// Why a path could not become an entry path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryPathError {
    /// The path was empty.
    Empty,
    /// The path began with a separator.
    Absolute,
    /// The path ended with a separator.
    TrailingSeparator,
    /// The path contained two separators in a row.
    EmptyComponent,
    /// The path contained a component that names a directory relative to
    /// another one.
    RelativeComponent,
    /// The path contained a backslash, which is never a separator here.
    Backslash,
    /// The path contained a byte no filesystem accepts inside a name.
    ControlCharacter,
}

impl fmt::Display for EntryPathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::Empty => "entry path is empty",
            Self::Absolute => "entry path begins with a separator",
            Self::TrailingSeparator => "entry path ends with a separator",
            Self::EmptyComponent => "entry path has an empty component",
            Self::RelativeComponent => "entry path has a relative component",
            Self::Backslash => "entry path contains a backslash",
            Self::ControlCharacter => "entry path contains a control character",
        };
        f.write_str(text)
    }
}

impl std::error::Error for EntryPathError {}

/// A path relative to the destination root, separated by forward slashes.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct EntryPath(String);

impl EntryPath {
    /// Checks a path and returns it as an entry path.
    ///
    /// Takes a path relative to the destination root. Returns it unchanged,
    /// with no normalization applied. Fails when the path is empty, begins or
    /// ends with a separator, has an empty or relative component, contains a
    /// backslash, or contains a control character.
    ///
    /// # Errors
    ///
    /// Returns the rule the path broke.
    pub fn new(path: &str) -> Result<Self, EntryPathError> {
        if path.is_empty() {
            return Err(EntryPathError::Empty);
        }
        if path.starts_with('/') {
            return Err(EntryPathError::Absolute);
        }
        if path.ends_with('/') {
            return Err(EntryPathError::TrailingSeparator);
        }
        if path.contains('\\') {
            return Err(EntryPathError::Backslash);
        }
        if path.chars().any(char::is_control) {
            return Err(EntryPathError::ControlCharacter);
        }
        for component in path.split('/') {
            if component.is_empty() {
                return Err(EntryPathError::EmptyComponent);
            }
            if component == "." || component == ".." {
                return Err(EntryPathError::RelativeComponent);
            }
        }
        Ok(Self(path.to_owned()))
    }

    /// Returns the path text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Returns the raw bytes entries are ordered by.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }
}

impl fmt::Display for EntryPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// One entry of a materialized directory.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TreeEntry {
    /// A file, carrying its mode, its length, and the digest of its bytes.
    File {
        /// Where the entry sits under the destination root.
        path: EntryPath,
        /// The reduced permission bits, taken from the source.
        mode: Mode,
        /// The length of the file in bytes.
        size: u64,
        /// The digest of the file bytes.
        content: ContentDigest,
    },
    /// A directory, whether or not it contains anything.
    Directory {
        /// Where the entry sits under the destination root.
        path: EntryPath,
    },
    /// A symbolic link, carrying the length and digest of its target bytes.
    Symlink {
        /// Where the entry sits under the destination root.
        path: EntryPath,
        /// The length of the target in bytes.
        size: u64,
        /// The digest of the target bytes.
        content: ContentDigest,
    },
}

impl TreeEntry {
    /// Returns where this entry sits under the destination root.
    #[must_use]
    pub fn path(&self) -> &EntryPath {
        match self {
            Self::File { path, .. } | Self::Directory { path } | Self::Symlink { path, .. } => path,
        }
    }

    /// Returns the type tag this entry is encoded with.
    #[must_use]
    pub fn tag(&self) -> u8 {
        match self {
            Self::File { .. } => 0x01,
            Self::Directory { .. } => 0x02,
            Self::Symlink { .. } => 0x03,
        }
    }
}
