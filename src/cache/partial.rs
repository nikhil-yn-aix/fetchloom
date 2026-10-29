//! unfinished downloads: the bytes so far, a record of what they are, and a lock so that two
//! processes never write the same download. anyone who can write to the cache could plant a link
//! among them, so none of them is ever opened through one.

use std::fs::{self, File, Metadata, OpenOptions, TryLockError};
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::{Cache, Stored, Validators, real_folder};
use crate::digest::{self, Hasher, Sha256};

/// how often a download records its progress, so a killed run loses about this much of it.
const SAVE_EVERY: Duration = Duration::from_secs(1);
const BUFFER: usize = 1024 * 1024;

pub(crate) enum Claim {
    Owned(Partial),
    /// another process is downloading the same URL right now.
    Busy,
}

/// an unfinished download of one URL. this process owns it until every clone is dropped.
#[derive(Clone)]
pub(crate) struct Partial {
    cache: Cache,
    url: String,
    bytes: PathBuf,
    record: PathBuf,
    lock: Option<Arc<File>>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Record {
    url: String,
    received: u64,
    /// the SHA-256 of the first `received` bytes, which a resume checks before it trusts them.
    sha256: Sha256,
    length: Option<u64>,
    validators: Validators,
}

impl Cache {
    /// claims the unfinished download of `url` for this process, unless another process has it.
    pub(crate) fn claim(&self, url: &str) -> io::Result<Claim> {
        let key = digest::sha256(url.as_bytes()).hex();
        let partial = self.root.join("partial");
        real_folder(&partial)?;
        // the lock gets its own file because windows locks are mandatory: a locked download
        // could not be read through the object it turns into
        let lock = open_unfollowed(&partial.join(format!("{key}.lock")), true)?;
        let (folder, stem, lock) = match lock.try_lock() {
            Ok(()) => (partial, key, Some(Arc::new(lock))),
            Err(TryLockError::WouldBlock) => return Ok(Claim::Busy),
            // without locks, a private file keeps processes apart, at the cost of resuming
            Err(TryLockError::Error(_)) => (
                self.root.join("tmp"),
                format!("{key}.{:016x}", fastrand::u64(..)),
                None,
            ),
        };
        Ok(Claim::Owned(Partial {
            cache: self.clone(),
            url: url.to_owned(),
            bytes: folder.join(format!("{stem}.part")),
            record: folder.join(format!("{stem}.json")),
            lock,
        }))
    }
}

impl Partial {
    /// how many bytes an earlier run kept, and how the server identified them then.
    pub(crate) fn resumable(&self) -> Option<(u64, Validators)> {
        let record = self.recorded()?;
        let kept = fs::symlink_metadata(&self.bytes).ok()?;
        (record.received > 0 && continuable(&kept) && kept.len() >= record.received)
            .then_some((record.received, record.validators))
    }

    /// writes from byte `offset` on. the record is the truth: bytes past it are cut off, and the
    /// bytes before it are hashed again so the final hash covers the whole file. bytes that no
    /// longer hash to what the record says, as after a power cut, or that this process may not
    /// continue, fail with [`io::ErrorKind::InvalidData`]. `md5` also computes the MD5, to check
    /// one a source stated.
    pub(crate) fn writer(
        &self,
        offset: u64,
        length: Option<u64>,
        validators: Validators,
        md5: bool,
    ) -> io::Result<PartialWriter> {
        let mut file = self.open(offset)?;
        let mut hasher = if md5 {
            Hasher::with_md5()
        } else {
            Hasher::default()
        };
        if offset > 0 {
            let mut buffer = vec![0; BUFFER];
            loop {
                let read = file.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                hasher.update(&buffer[..read]);
            }
            let recorded = self
                .recorded()
                .filter(|record| record.received == offset)
                .map(|record| record.sha256);
            if recorded != Some(hasher.sha256_so_far()) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "the bytes kept from an earlier download are not the ones it recorded",
                ));
            }
        }
        file.seek(SeekFrom::Start(offset))?;
        Ok(PartialWriter {
            cache: self.cache.clone(),
            file: BufWriter::with_capacity(BUFFER, file),
            _lock: self.lock.clone(),
            record: Record {
                url: self.url.clone(),
                received: offset,
                sha256: hasher.sha256_so_far(),
                length,
                validators,
            },
            hasher,
            written: offset,
            saved: Instant::now(),
            bytes: self.bytes.clone(),
            record_path: self.record.clone(),
        })
    }

    /// forgets the bytes so far, so the download starts again from the first byte.
    pub(crate) fn discard(&self) {
        let _ = fs::remove_file(&self.bytes);
        let _ = fs::remove_file(&self.record);
    }

    /// the file for the bytes, cut to their first `offset`. bytes an earlier run kept are used
    /// only when this process may continue them. otherwise a download from the first byte
    /// removes their name and makes a new file there: a read-only one was sealed by a run that
    /// did not finish publishing, and may already be a second name of the object, which must
    /// never become writable.
    fn open(&self, offset: u64) -> io::Result<File> {
        let kept = open_unfollowed(&self.bytes, false)
            .ok()
            .filter(|file| file.metadata().is_ok_and(|metadata| continuable(&metadata)));
        let file = match kept {
            Some(file) => file,
            None if offset > 0 => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "the bytes kept from an earlier download cannot be continued",
                ));
            }
            None => {
                if let Err(err) = fs::remove_file(&self.bytes)
                    && err.kind() != io::ErrorKind::NotFound
                {
                    return Err(err);
                }
                OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create_new(true)
                    .open(&self.bytes)?
            }
        };
        file.set_len(offset)?;
        Ok(file)
    }

    fn recorded(&self) -> Option<Record> {
        let text = fs::read(&self.record).ok()?;
        let record: Record = serde_json::from_slice(&text).ok()?;
        (record.url == self.url).then_some(record)
    }
}

/// opens `path` to read and write without following a link at its end, and refuses anything
/// that is not a plain file. `create` makes a missing file.
pub(super) fn open_unfollowed(path: &Path, create: bool) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create(create)
        .truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(rustix::fs::OFlags::NOFOLLOW.bits().cast_signed());
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // a link opens as itself rather than as what it points at, and is refused below
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let refused = || {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "{} is a link or a special file, which fetchloom never writes into",
                path.display()
            ),
        )
    };
    let opened = options.open(path);
    // what unix answers when the name is a link
    #[cfg(unix)]
    let opened = opened.map_err(|err| {
        if err.raw_os_error() == Some(rustix::io::Errno::LOOP.raw_os_error()) {
            refused()
        } else {
            err
        }
    });
    let file = opened?;
    if is_plain(&file.metadata()?) {
        Ok(file)
    } else {
        Err(refused())
    }
}

/// whether `metadata` describes a plain file, not a link, a folder or a device.
pub(super) fn is_plain(metadata: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return false;
        }
    }
    metadata.file_type().is_file()
}

/// whether this process may write on into the file `metadata` describes: a plain file, still
/// writable, and on unix this user's own with no other name, so that writing into it can change
/// nothing outside the download.
fn continuable(metadata: &Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != rustix::process::geteuid().as_raw() || metadata.nlink() != 1 {
            return false;
        }
    }
    is_plain(metadata) && !metadata.permissions().readonly()
}

/// the writing end of a download: hashes every byte on its way to disk. it keeps the download
/// claimed until it is done, since a stopped run may drop its claim while this still writes.
pub(crate) struct PartialWriter {
    cache: Cache,
    file: BufWriter<File>,
    _lock: Option<Arc<File>>,
    hasher: Hasher,
    written: u64,
    saved: Instant,
    bytes: PathBuf,
    record_path: PathBuf,
    record: Record,
}

impl PartialWriter {
    pub(crate) fn written(&self) -> u64 {
        self.written
    }

    /// records how far the download got, so a later run continues from there. the bytes are not
    /// flushed first, which would stall the download every second. the record holds their hash
    /// instead, and a resume that finds other bytes starts over.
    pub(crate) fn save(&mut self) -> io::Result<()> {
        self.file.flush()?;
        self.record.received = self.written;
        self.record.sha256 = self.hasher.sha256_so_far();
        let text = serde_json::to_vec(&self.record).map_err(io::Error::other)?;
        self.cache.write_atomically(&self.record_path, &text)?;
        self.saved = Instant::now();
        Ok(())
    }

    /// puts the finished file into the cache under its hash.
    pub(crate) fn finish(self) -> io::Result<Stored> {
        let PartialWriter {
            cache,
            file,
            hasher,
            written,
            bytes,
            record_path,
            ..
        } = self;
        let file = file.into_inner().map_err(io::IntoInnerError::into_error)?;
        file.sync_all()?;
        // sealed through the handle that wrote it: by name, a link swapped in since would be
        // sealed in its place
        let mut permissions = file.metadata()?.permissions();
        permissions.set_readonly(true);
        file.set_permissions(permissions)?;
        drop(file);
        let (digest, md5) = hasher.finish();
        let stored = cache.link_object(&bytes, digest, written, md5)?;
        let _ = fs::remove_file(&record_path);
        Ok(stored)
    }
}

impl Write for PartialWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let written = self.file.write(buffer)?;
        self.hasher.update(&buffer[..written]);
        self.written += written as u64;
        if self.saved.elapsed() >= SAVE_EVERY {
            self.save()?;
        }
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

#[cfg(test)]
mod tests;
