//! Files being written into a tier, hashed as they are written.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::error::{StoreError, io};

/// A file in a tier's `tmp/` that hashes every byte written to it with BLAKE3.
///
/// It becomes an object through [`Tier::ingest`](crate::tier::Tier::ingest). Dropped without
/// being ingested, its file is removed.
#[derive(Debug)]
pub struct Staged {
    pub(crate) file: File,
    pub(crate) scratch: Scratch,
    pub(crate) hasher: blake3::Hasher,
    pub(crate) written: u64,
    pub(crate) expected: Option<u64>,
}

impl Write for Staged {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let count = self.file.write(buf)?;
        self.hasher.update(&buf[..count]);
        self.written += count as u64;
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

/// A file that is removed when this is dropped, unless it was kept.
#[derive(Debug)]
pub(crate) struct Scratch {
    path: PathBuf,
    kept: bool,
}

impl Scratch {
    /// Takes charge of removing `path`.
    pub(crate) const fn new(path: PathBuf) -> Self {
        Self { path, kept: false }
    }

    /// The file's path.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Leaves the file where it is, because it was renamed away.
    pub(crate) fn keep(mut self) {
        self.kept = true;
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if !self.kept {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

const NAME_ATTEMPTS: usize = 64;

/// The size of every buffer the store reads files through.
pub(crate) const BUFFER: usize = 1 << 20;

/// Creates a new file in `dir` under a name no other writer holds: the process id and a counter,
/// in hex, tried again with the next counter when the name exists, which happens only when a
/// dead process or another host left it.
pub(crate) fn create_unique(dir: &Path) -> Result<(File, PathBuf), StoreError> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let mut path = dir.to_owned();
    for _ in 0..NAME_ATTEMPTS {
        let count = NEXT.fetch_add(1, Ordering::Relaxed);
        path = dir.join(format!("{:x}-{count:x}", process::id()));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((file, path)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(io("create", &path)(error)),
        }
    }
    Err(io("create", &path)(io::ErrorKind::AlreadyExists.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_taken_name_is_skipped_for_the_next_one() {
        let temp = tempfile::tempdir().unwrap();
        let taken: Vec<PathBuf> = (0..2)
            .map(|count| temp.path().join(format!("{:x}-{count:x}", process::id())))
            .collect();
        for path in &taken {
            std::fs::write(path, b"").unwrap();
        }
        let (_file, path) = create_unique(temp.path()).unwrap();
        assert!(!taken.contains(&path));
        assert_eq!(path.parent(), Some(temp.path()));
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 3);
    }

    #[test]
    fn a_missing_directory_is_not_found() {
        let temp = tempfile::tempdir().unwrap();
        let error = create_unique(&temp.path().join("missing")).unwrap_err();
        assert!(
            matches!(&error, StoreError::Io { source, .. } if source.kind() == io::ErrorKind::NotFound),
            "{error}"
        );
    }

    #[test]
    fn reserve_sets_the_length() {
        let temp = tempfile::tempdir().unwrap();
        let (file, _path) = create_unique(temp.path()).unwrap();
        crate::platform::reserve(&file, 4096).unwrap();
        assert_eq!(file.metadata().unwrap().len(), 4096);
    }
}
