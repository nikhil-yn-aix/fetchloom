//! Reading an archive's tree without writing a byte anywhere.

use std::collections::HashMap;
use std::io::Read;

use fetchloom_engine::digest::ContentDigest;
use fetchloom_engine::error::{Error, ErrorKind};
use fetchloom_engine::hashing::hash_bytes;
use fetchloom_engine::limits::Limits;
use fetchloom_engine::seam::archive::{Archive, ArchiveMember, MemberKind};
use fetchloom_engine::selection::Selection;
use fetchloom_engine::tree::TreeEntry;

use crate::extract::{BUFFER_LEN, build_plan, io_failure, select_members};

pub fn resolve<A: Archive>(
    archive: &mut A,
    selection: &Selection,
    limits: Limits,
) -> Result<Vec<TreeEntry>, Error> {
    let members = archive.members()?;
    let applied = select_members(&members, selection)?;
    let by_path: HashMap<&str, usize> = members
        .iter()
        .enumerate()
        .map(|(index, member)| (member.path.as_str(), index))
        .collect();
    let plan = build_plan(&members, &applied)?;

    let mut counted = Counted::new(limits);
    let mut entries: Vec<TreeEntry> = Vec::new();

    for path in &plan.directories {
        counted.observe_entry()?;
        entries.push(TreeEntry::Directory { path: path.clone() });
    }

    let mut buffer = vec![0_u8; BUFFER_LEN];
    for applied_member in &plan.files {
        counted.observe_entry()?;
        let member = &members[applied_member.index];
        let source = source_member(member, &members, &by_path)?;
        let mut body = archive.open(source)?;
        let (size, content) = hash_body(&mut body, &mut buffer, member.path.as_str())?;
        counted.observe_bytes(size)?;
        entries.push(TreeEntry::File {
            path: applied_member.path.clone(),
            mode: member.mode,
            size,
            content,
        });
    }

    for applied_member in &plan.symlinks {
        counted.observe_entry()?;
        let member = &members[applied_member.index];
        let target = member.target.clone().unwrap_or_default();
        counted.observe_bytes(target.len() as u64)?;
        entries.push(TreeEntry::Symlink {
            path: applied_member.path.clone(),
            size: target.len() as u64,
            content: hash_bytes(&target),
        });
    }

    Ok(entries)
}

fn source_member<'a>(
    member: &'a ArchiveMember,
    members: &'a [ArchiveMember],
    by_path: &HashMap<&str, usize>,
) -> Result<&'a ArchiveMember, Error> {
    if member.kind != MemberKind::HardLink {
        return Ok(member);
    }
    let raw = member.target.as_deref().unwrap_or_default();
    let target = String::from_utf8_lossy(raw).into_owned();
    let Some(&index) = by_path.get(target.as_str()) else {
        return Err(Error::new(
            ErrorKind::ArchiveLinkEscape,
            format!(
                "member \"{}\" hard-links to \"{target}\", which this archive does not hold",
                member.path
            ),
        ));
    };
    Ok(&members[index])
}

fn hash_body(
    body: &mut dyn Read,
    buffer: &mut [u8],
    member: &str,
) -> Result<(u64, ContentDigest), Error> {
    let mut hasher = blake3::Hasher::new();
    let mut total: u64 = 0;
    loop {
        let read = body
            .read(buffer)
            .map_err(|error| io_failure(member, &error))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        total += read as u64;
    }
    Ok((
        total,
        ContentDigest::from_bytes(*hasher.finalize().as_bytes()),
    ))
}

struct Counted {
    entries: u64,
    bytes: u64,
    limits: Limits,
}

impl Counted {
    fn new(limits: Limits) -> Self {
        Self {
            entries: 0,
            bytes: 0,
            limits,
        }
    }

    fn observe_entry(&mut self) -> Result<(), Error> {
        self.entries += 1;
        if self.entries > self.limits.archive_entries {
            return Err(Error::new(
                ErrorKind::ArchiveBomb,
                format!(
                    "this archive holds more than {} entries, {} read so far",
                    self.limits.archive_entries, self.entries
                ),
            ));
        }
        Ok(())
    }

    fn observe_bytes(&mut self, count: u64) -> Result<(), Error> {
        self.bytes = self.bytes.saturating_add(count);
        if self.bytes > self.limits.expanded_bytes {
            return Err(Error::new(
                ErrorKind::ArchiveBomb,
                format!(
                    "this archive expands past {} bytes, {} read so far",
                    self.limits.expanded_bytes, self.bytes
                ),
            ));
        }
        Ok(())
    }
}
