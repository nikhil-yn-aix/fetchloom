//! new objects written in one go, such as the members of an archive.

use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Write};

use tempfile::TempPath;

use super::{Cache, Stored};
use crate::digest::{Hasher, Md5, Sha256};

const BUFFER: usize = 256 * 1024;

/// a new object being written into `tmp/`, hashed as it goes.
pub(crate) struct ObjectWriter {
    cache: Cache,
    file: BufWriter<File>,
    temp: TempPath,
    hasher: Hasher,
    written: u64,
}

/// an object written in full and waiting to be published. many can wait together, so their
/// flushes to disk run in parallel instead of one at a time.
pub(crate) struct Closed {
    cache: Cache,
    temp: TempPath,
    digest: Sha256,
    md5: Option<Md5>,
    size: u64,
}

impl Cache {
    /// starts a new object. `md5` also computes its MD5, to check one a source stated.
    pub(crate) fn object_writer(&self, md5: bool) -> io::Result<ObjectWriter> {
        let (file, temp) = tempfile::NamedTempFile::new_in(self.root.join("tmp"))?.into_parts();
        Ok(ObjectWriter {
            cache: self.clone(),
            file: BufWriter::with_capacity(BUFFER, file),
            temp,
            hasher: if md5 {
                Hasher::with_md5()
            } else {
                Hasher::default()
            },
            written: 0,
        })
    }
}

impl ObjectWriter {
    pub(crate) fn written(&self) -> u64 {
        self.written
    }

    /// hands every byte to the operating system. the bytes may not be on disk yet.
    pub(crate) fn close(self) -> io::Result<Closed> {
        let ObjectWriter {
            cache,
            file,
            temp,
            hasher,
            written,
        } = self;
        file.into_inner().map_err(io::IntoInnerError::into_error)?;
        let (digest, md5) = hasher.finish();
        Ok(Closed {
            cache,
            temp,
            digest,
            md5,
            size: written,
        })
    }
}

impl Write for ObjectWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let written = self.file.write(buffer)?;
        self.hasher.update(&buffer[..written]);
        self.written += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

impl Closed {
    pub(crate) fn digest(&self) -> Sha256 {
        self.digest
    }

    pub(crate) fn size(&self) -> u64 {
        self.size
    }

    /// waits until the bytes are on disk, so a power cut cannot leave a published object empty.
    pub(crate) fn sync(&self) -> io::Result<()> {
        // windows flushes a file only through a handle that may write to it
        OpenOptions::new().write(true).open(&self.temp)?.sync_all()
    }

    /// puts the object into the cache under its hash.
    pub(crate) fn publish(self) -> io::Result<Stored> {
        let Closed {
            cache,
            temp,
            digest,
            md5,
            size,
        } = self;
        let stored = cache.publish(&temp, digest, size, md5)?;
        // publishing moved or removed the file, so there is nothing left to delete
        let _ = temp.keep();
        Ok(stored)
    }
}
