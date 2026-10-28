//! new objects written in one go, such as the members of an archive. each is created read-only in
//! `tmp/`, written and hashed in one pass, flushed and closed, then linked into place, so putting
//! it in the cache costs two calls to the filesystem.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

use tempfile::TempPath;

use super::{Cache, Stored};
use crate::digest::{Hasher, Sha256};

/// a new object being written into `tmp/`. dropped before it is sealed, it leaves nothing.
pub(crate) struct NewObject {
    file: File,
    temp: TempPath,
    hasher: Hasher,
    written: u64,
}

/// an object written in full and flushed to disk, waiting in `tmp/` to be put in place. dropped
/// before it is published, it leaves nothing.
pub(crate) struct Sealed {
    temp: TempPath,
    pub(crate) digest: Sha256,
    pub(crate) size: u64,
}

impl Cache {
    pub(crate) fn new_object(&self) -> io::Result<NewObject> {
        loop {
            let path = self
                .root
                .join("tmp")
                .join(format!("{:016x}.new", fastrand::u64(..)));
            match create_sealed(&path) {
                Ok(file) => {
                    return Ok(NewObject {
                        file,
                        temp: TempPath::try_from_path(path)?,
                        hasher: Hasher::default(),
                        written: 0,
                    });
                }
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
                Err(err) => return Err(err),
            }
        }
    }
}

/// creates a file read-only from the start. the handle that created it may still write, so the
/// file never needs sealing later.
fn create_sealed(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_ATTRIBUTE_READONLY: u32 = 1;
        options.attributes(FILE_ATTRIBUTE_READONLY);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o444);
    }
    options.open(path)
}

impl NewObject {
    pub(crate) fn written(&self) -> u64 {
        self.written
    }

    /// flushes the bytes to disk, so a power cut cannot leave the object empty, and closes it.
    pub(crate) fn seal(self) -> io::Result<Sealed> {
        let NewObject {
            file,
            temp,
            hasher,
            written,
        } = self;
        file.sync_all()?;
        drop(file);
        let (digest, _) = hasher.finish();
        Ok(Sealed {
            temp,
            digest,
            size: written,
        })
    }
}

impl Sealed {
    /// puts the object in place under its hash.
    pub(crate) fn publish(self, cache: &Cache) -> io::Result<Stored> {
        let Sealed { temp, digest, size } = self;
        let stored = cache.link_object(&temp, digest, size, None)?;
        // linking moved or removed the file, so there is nothing left to delete
        let _ = temp.keep();
        Ok(stored)
    }
}

impl Write for NewObject {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let written = self.file.write(buffer)?;
        self.hasher.update(&buffer[..written]);
        self.written += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
