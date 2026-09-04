//! The entries a materialized directory is made of.

use std::fmt;

use serde::Serialize;

use crate::digest::ContentDigest;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(into = "u32")]
pub enum Mode {
    ReadWrite,
    Executable,
}

impl Mode {
    #[must_use]
    pub fn reduce(source_mode: u32) -> Self {
        if source_mode & 0o111 == 0 {
            Self::ReadWrite
        } else {
            Self::Executable
        }
    }

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryPathError {
    Empty,
    Absolute,
    TrailingSeparator,
    EmptyComponent,
    RelativeComponent,
    Backslash,
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

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct EntryPath(String);

impl EntryPath {
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

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

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

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TreeEntry {
    File {
        path: EntryPath,
        mode: Mode,
        size: u64,
        content: ContentDigest,
    },
    Directory {
        path: EntryPath,
    },
    Symlink {
        path: EntryPath,
        size: u64,
        content: ContentDigest,
    },
}

impl TreeEntry {
    #[must_use]
    pub fn path(&self) -> &EntryPath {
        match self {
            Self::File { path, .. } | Self::Directory { path } | Self::Symlink { path, .. } => path,
        }
    }

    #[must_use]
    pub fn tag(&self) -> u8 {
        match self {
            Self::File { .. } => 0x01,
            Self::Directory { .. } => 0x02,
            Self::Symlink { .. } => 0x03,
        }
    }
}
