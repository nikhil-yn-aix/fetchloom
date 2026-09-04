//! Walking a local tree into entries, and copying it into a destination.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use fetchloom_engine::error::{Error, ErrorKind, Surface, filesystem_failure};
use fetchloom_engine::hashing;
use fetchloom_engine::pool::Processor;
use fetchloom_engine::tree::{EntryPath, Mode, TreeEntry};
use fetchloom_engine::work::WorkCounter;

pub const WALKED_MODE: Mode = Mode::ReadWrite;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceFile {
    pub relative: PathBuf,
    pub entry: EntryPath,
    pub mode: Mode,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceLink {
    pub entry: EntryPath,
    pub target: Vec<u8>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Walked {
    pub entries: Vec<TreeEntry>,
    pub files: Vec<SourceFile>,
    pub links: Vec<SourceLink>,
    pub bytes: u64,
    pub root: PathBuf,
}

fn unrepresentable(path: &Path, reason: &str) -> Error {
    Error::new(
        ErrorKind::DestinationUnrepresentable,
        format!("rename {} so that it {reason}", path.display()),
    )
}

fn entry_path_of(relative: &Path) -> Result<EntryPath, Error> {
    let mut parts = Vec::new();
    for component in relative.components() {
        let std::path::Component::Normal(part) = component else {
            return Err(unrepresentable(
                relative,
                "contains no path component that leaves the destination",
            ));
        };
        let Some(text) = part.to_str() else {
            return Err(unrepresentable(relative, "is valid Unicode"));
        };
        parts.push(text);
    }
    let joined = parts.join("/");
    EntryPath::new(&joined).map_err(|reason| unrepresentable(relative, &reason.to_string()))
}

pub fn walk(root: &Path) -> Result<Walked, Error> {
    let metadata = fs::symlink_metadata(root)
        .map_err(|reason| filesystem_failure(Surface::Source, root, &reason))?;
    let mut walked = Walked::default();
    if metadata.is_dir() {
        walked.root = root.to_path_buf();
        walk_into(root, root, &mut walked)?;
        return Ok(walked);
    }
    walked.root = root
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    let one = walked.root.clone();
    walk_one(&one, root, &metadata, &mut walked)?;
    Ok(walked)
}

fn walk_into(root: &Path, directory: &Path, walked: &mut Walked) -> Result<(), Error> {
    let listing = fs::read_dir(directory)
        .map_err(|reason| filesystem_failure(Surface::Source, directory, &reason))?;
    let mut children: Vec<PathBuf> = Vec::new();
    for entry in listing {
        let entry =
            entry.map_err(|reason| filesystem_failure(Surface::Source, directory, &reason))?;
        children.push(entry.path());
    }
    children.sort();

    for path in children {
        let metadata = fs::symlink_metadata(&path)
            .map_err(|reason| filesystem_failure(Surface::Source, &path, &reason))?;
        walk_one(root, &path, &metadata, walked)?;
    }
    Ok(())
}

fn walk_one(
    root: &Path,
    path: &Path,
    metadata: &fs::Metadata,
    walked: &mut Walked,
) -> Result<(), Error> {
    let relative = path.strip_prefix(root).unwrap_or(path).to_path_buf();
    let entry_path = entry_path_of(&relative)?;

    if metadata.is_symlink() {
        let target = fs::read_link(path)
            .map_err(|reason| filesystem_failure(Surface::Source, path, &reason))?;
        let bytes = target.to_string_lossy().replace('\\', "/").into_bytes();
        walked.entries.push(TreeEntry::Symlink {
            path: entry_path.clone(),
            size: bytes.len() as u64,
            content: hashing::hash_bytes(&bytes),
        });
        walked.links.push(SourceLink {
            entry: entry_path,
            target: bytes,
        });
    } else if metadata.is_dir() {
        walked
            .entries
            .push(TreeEntry::Directory { path: entry_path });
        walk_into(root, path, walked)?;
    } else {
        walked.bytes += metadata.len();
        walked.files.push(SourceFile {
            relative,
            entry: entry_path,
            mode: WALKED_MODE,
        });
    }
    Ok(())
}

pub fn copy_file(
    digester: &mut hashing::Digester,
    processor: &Processor,
    work: &WorkCounter,
    from: &Path,
    to: &Path,
) -> Result<(u64, hashing::Digests), Error> {
    let source = fs::File::open(from)
        .map_err(|reason| filesystem_failure(Surface::Source, from, &reason))?;
    let target = fs::File::create(to)
        .map_err(|reason| filesystem_failure(Surface::Destination, to, &reason))?;
    work.touched_file();
    let length = source
        .metadata()
        .map_err(|reason| filesystem_failure(Surface::Source, from, &reason))?
        .len();
    let mut tee = Tee {
        source,
        target,
        path: to.to_path_buf(),
        work,
    };
    let digests = digester
        .hash(processor, &mut tee)
        .map_err(|reason| filesystem_failure(Surface::Source, from, &reason))?;
    tee.target
        .flush()
        .map_err(|reason| filesystem_failure(Surface::Destination, to, &reason))?;
    Ok((length, digests))
}

struct Tee<'a> {
    source: fs::File,
    target: fs::File,
    path: PathBuf,
    work: &'a WorkCounter,
}

impl Read for Tee<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let read = self.source.read(buffer)?;
        if read > 0 {
            self.target.write_all(&buffer[..read])?;
            self.work.read_bytes(read as u64);
            self.work.wrote_bytes(read as u64);
        }
        Ok(read)
    }
}

impl std::fmt::Debug for Tee<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tee")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}
