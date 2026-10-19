//! Files being written into a tier, hashed as they are written.

use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

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
