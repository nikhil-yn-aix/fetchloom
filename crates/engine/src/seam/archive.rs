//! The Archive seam: enumeration, selection, and bounded extraction.

use std::io::Read;

use serde::Serialize;

use crate::error::Error;
use crate::manifest::ArchiveFormat;
use crate::tree::Mode;

/// What one archive member is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MemberKind {
    /// A file.
    File,
    /// A directory.
    Directory,
    /// A symbolic link.
    Symlink,
    /// A hard link to another member.
    HardLink,
    /// Anything else, which is rejected.
    Other,
}

/// One member of an archive, as its headers describe it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ArchiveMember {
    /// The path exactly as the archive wrote it, with nothing normalized.
    pub path: String,
    /// What the member is.
    pub kind: MemberKind,
    /// The length of the member in bytes.
    pub size: u64,
    /// The permission bits the archive recorded, reduced to the two a tree
    /// digest carries.
    pub mode: Mode,
    /// The target bytes, for a link.
    pub target: Option<Vec<u8>>,
}

/// A packed collection of members.
pub trait Archive {
    /// The bytes of one member, streamed.
    type Body: Read;

    /// Returns the format this reader handles.
    fn format(&self) -> ArchiveFormat;

    /// Lists the members without extracting anything.
    ///
    /// # Errors
    ///
    /// Fails when the archive is truncated or malformed, when its headers
    /// disagree, and when it exceeds the entry limit.
    fn members(&mut self) -> Result<Vec<ArchiveMember>, Error>;

    /// Opens the bytes of one member.
    ///
    /// # Errors
    ///
    /// Fails when the member is absent, when the archive is truncated, and
    /// when the member expands past a limit.
    fn open(&mut self, member: &ArchiveMember) -> Result<Self::Body, Error>;
}
