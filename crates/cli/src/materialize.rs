//! Walking a local tree into entries, and copying it into a destination.
//!
//! The file operations here go through the standard library rather than the
//! Platform seam, because no implementation of that seam exists yet. Atomic
//! publication, capability detection, and advisory locking are therefore absent
//! and every one of them is reported as a degradation rather than assumed.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use fetchloom_engine::digest::ContentDigest;
use fetchloom_engine::error::{Error, ErrorKind};
use fetchloom_engine::hashing;
use fetchloom_engine::pool::Processor;
use fetchloom_engine::tree::{EntryPath, Mode, TreeEntry};
use fetchloom_engine::work::WorkCounter;

/// The mode every file a filesystem walk finds is recorded under.
///
/// A bare filesystem tree is neither an archive nor a manifest, so it states
/// no mode Materialization accepts. Reading one back from a stat answers
/// differently on a volume that carries an executable bit and one that does
/// not, and the same tree then digests differently on Windows and on Linux.
/// Every walked file is `0644` on every platform instead, and the run reports
/// that it read none.
pub const WALKED_MODE: Mode = Mode::ReadWrite;

/// One file found while walking a source tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceFile {
    /// Where the file sits under the source root.
    pub relative: PathBuf,
    /// The entry path the tree digest records it under.
    pub entry: EntryPath,
    /// The mode the entry is recorded under, always [`WALKED_MODE`].
    pub mode: Mode,
}

/// One symbolic link found while walking a source tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceLink {
    /// The entry path the tree digest records it under.
    pub entry: EntryPath,
    /// The target bytes, canonicalized to forward slashes.
    pub target: Vec<u8>,
}

/// Everything a walk of a source tree found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Walked {
    /// The entries, unsorted; the tree digest sorts them itself.
    pub entries: Vec<TreeEntry>,
    /// The files whose bytes still have to be copied.
    pub files: Vec<SourceFile>,
    /// The symbolic links, with the target bytes needed to recreate them.
    pub links: Vec<SourceLink>,
    /// How many bytes those files hold.
    pub bytes: u64,
    /// The directory every relative path is taken from.
    ///
    /// A reference naming a directory walks that directory. A reference naming
    /// one object walks its parent and takes only that object, so a single
    /// object materializes as a destination holding one entry.
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

/// Walks a source tree and returns the entries and files it holds.
///
/// Takes the root of the tree. Returns every directory, file, and symbolic link
/// under it as an entry, with files also listed for copying. The root itself is
/// not an entry.
///
/// # Errors
///
/// Returns the entry that cannot be represented, or the read that failed.
pub fn walk(root: &Path) -> Result<Walked, Error> {
    let metadata = fs::symlink_metadata(root).map_err(|reason| read_failure(root, &reason))?;
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

fn read_failure(path: &Path, reason: &std::io::Error) -> Error {
    if reason.kind() == std::io::ErrorKind::NotFound {
        return Error::new(
            ErrorKind::ReferenceUnresolved,
            format!(
                "name a path that exists, because nothing is at {}",
                path.display()
            ),
        );
    }
    Error::new(
        ErrorKind::ReferenceUnresolved,
        format!("make {} readable: {reason}", path.display()),
    )
}

fn walk_into(root: &Path, directory: &Path, walked: &mut Walked) -> Result<(), Error> {
    let listing = fs::read_dir(directory).map_err(|reason| read_failure(directory, &reason))?;
    let mut children: Vec<PathBuf> = Vec::new();
    for entry in listing {
        let entry = entry.map_err(|reason| read_failure(directory, &reason))?;
        children.push(entry.path());
    }
    children.sort();

    for path in children {
        let metadata =
            fs::symlink_metadata(&path).map_err(|reason| read_failure(&path, &reason))?;
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
        let target = fs::read_link(path).map_err(|reason| read_failure(path, &reason))?;
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

/// Copies one file and returns its length and content digest.
///
/// Takes the processor pool, the source, and the destination, which must not
/// already exist. Copies the bytes and digests them in the same pass, so the
/// file is never read twice.
///
/// # Errors
///
/// Fails when the source cannot be read or the destination cannot be written.
pub fn copy_file(
    processor: &Processor,
    work: &WorkCounter,
    from: &Path,
    to: &Path,
) -> Result<(u64, ContentDigest), Error> {
    let source = fs::File::open(from).map_err(|reason| read_failure(from, &reason))?;
    let target = fs::File::create(to).map_err(|reason| read_failure(to, &reason))?;
    let length = source
        .metadata()
        .map_err(|reason| read_failure(from, &reason))?
        .len();
    let mut tee = Tee {
        source,
        target,
        path: to.to_path_buf(),
        work,
    };
    let digests =
        hashing::hash_stream(processor, &mut tee).map_err(|reason| read_failure(from, &reason))?;
    tee.target
        .flush()
        .map_err(|reason| read_failure(to, &reason))?;
    Ok((length, digests.content))
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
