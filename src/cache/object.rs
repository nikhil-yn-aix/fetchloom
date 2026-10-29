//! new objects written in one go, such as the members of an archive. each is created read-only in
//! `tmp/`, written and hashed in one pass, flushed and closed, then linked into place, so putting
//! it in the cache costs two calls to the filesystem.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::{Cache, Stored};
use crate::digest::{Hasher, Sha256};
use crate::volume::Volume;

/// a new object being written into `tmp/`. dropped before it is sealed, it leaves nothing.
pub(crate) struct NewObject {
    file: File,
    temp: Temporary,
    hasher: Hasher,
    written: u64,
}

/// an object written in full and flushed to disk, waiting in `tmp/` to be put in place. dropped
/// before it is published, it leaves nothing.
pub(crate) struct Sealed {
    temp: Temporary,
    pub(crate) digest: Sha256,
    pub(crate) size: u64,
}

/// the name of a file this process made in `tmp/`. dropped before the file is published, it
/// removes the file.
struct Temporary {
    path: PathBuf,
    volume: Arc<Volume>,
    published: bool,
}

impl Drop for Temporary {
    fn drop(&mut self) {
        if !self.published {
            let _ = self.volume.remove(&self.path);
        }
    }
}

impl Cache {
    /// flushes every file written to the cache's filesystem with one call, as git's batch mode
    /// does: on linux one flush per small file costs far more than writing it.
    #[cfg(target_os = "linux")]
    pub(crate) fn flush_written(&self) -> io::Result<()> {
        let folder = File::open(self.root.join("tmp"))?;
        rustix::fs::syncfs(&folder)?;
        Ok(())
    }

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
                        temp: Temporary {
                            path,
                            volume: Arc::clone(&self.volume),
                            published: false,
                        },
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
    /// closes the object. with `flush` its bytes reach the disk first, so a power cut cannot
    /// leave it empty; without, the caller flushes a whole batch at once before publishing any.
    pub(crate) fn seal(self, flush: bool) -> io::Result<Sealed> {
        let NewObject {
            file,
            temp,
            hasher,
            written,
        } = self;
        if flush {
            file.sync_all()?;
        }
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
        let Sealed {
            mut temp,
            digest,
            size,
        } = self;
        let stored = cache.link_object(&temp.path, digest, size, None)?;
        // linking moved or removed the file, and whatever has its name now is not ours to touch
        temp.published = true;
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

#[cfg(test)]
mod tests {
    use std::fs;

    use super::super::tests::fat;
    use super::*;

    #[test]
    fn objects_never_published_leave_nothing_on_fat() {
        let (_dir, cache) = fat();
        let mut written = cache.new_object().unwrap();
        written.write_all(b"abc").unwrap();
        let mut sealed = cache.new_object().unwrap();
        sealed.write_all(b"def").unwrap();
        let sealed = sealed.seal(true).unwrap();
        drop((written, sealed));
        assert_eq!(fs::read_dir(cache.root().join("tmp")).unwrap().count(), 0);
    }
}
