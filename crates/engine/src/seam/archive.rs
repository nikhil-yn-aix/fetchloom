//! The Archive seam: enumeration, selection, and bounded extraction.

use std::io::Read;

use serde::Serialize;

use crate::error::Error;
use crate::manifest::ArchiveFormat;
use crate::tree::Mode;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MemberKind {
    File,
    Directory,
    Symlink,
    HardLink,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ArchiveMember {
    pub path: String,
    pub kind: MemberKind,
    pub size: u64,
    pub mode: Mode,
    pub target: Option<Vec<u8>>,
}

pub trait Archive {
    type Body: Read;

    fn format(&self) -> ArchiveFormat;

    fn members(&mut self) -> Result<Vec<ArchiveMember>, Error>;

    fn open(&mut self, member: &ArchiveMember) -> Result<Self::Body, Error>;
}
