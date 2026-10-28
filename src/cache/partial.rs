//! unfinished downloads: the bytes so far, a record of what they are, and a lock so that two
//! processes never write the same download.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::{Cache, Stored, Validators, key_of};
use crate::digest::Hasher;

/// how often a download records its progress, so a killed run loses little.
const SAVE_EVERY: u64 = 64 * 1024 * 1024;
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
    _lock: Option<Arc<File>>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Record {
    url: String,
    received: u64,
    length: Option<u64>,
    validators: Validators,
}

impl Cache {
    /// claims the unfinished download of `url` for this process, unless another process has it.
    pub(crate) fn claim(&self, url: &str) -> io::Result<Claim> {
        let key = key_of(url);
        let partial = self.root.join("partial");
        // the lock gets its own file because windows locks are mandatory: a locked download
        // could not be read through the object it turns into
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(partial.join(format!("{key}.lock")))?;
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
            _lock: lock,
        }))
    }
}

impl Partial {
    /// how many bytes an earlier run kept, and how the server identified them then.
    pub(crate) fn resumable(&self) -> Option<(u64, Validators)> {
        let text = fs::read(&self.record).ok()?;
        let record: Record = serde_json::from_slice(&text).ok()?;
        let on_disk = fs::metadata(&self.bytes).ok()?.len();
        let usable = record.url == self.url && record.received > 0 && on_disk >= record.received;
        usable.then_some((record.received, record.validators))
    }

    /// writes from byte `offset` on. the record is the truth: bytes past it are cut off, and the
    /// bytes before it are hashed again so the final hash covers the whole file.
    pub(crate) fn writer(
        &self,
        offset: u64,
        length: Option<u64>,
        validators: Validators,
    ) -> io::Result<PartialWriter> {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&self.bytes)?;
        file.set_len(offset)?;
        let mut hasher = Hasher::default();
        if offset > 0 {
            let mut buffer = vec![0; BUFFER];
            loop {
                let read = file.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                hasher.update(&buffer[..read]);
            }
        }
        file.seek(SeekFrom::Start(offset))?;
        Ok(PartialWriter {
            cache: self.cache.clone(),
            file: BufWriter::with_capacity(BUFFER, file),
            hasher,
            written: offset,
            saved: offset,
            bytes: self.bytes.clone(),
            record_path: self.record.clone(),
            record: Record {
                url: self.url.clone(),
                received: offset,
                length,
                validators,
            },
        })
    }

    /// forgets the bytes so far, so the download starts again from the first byte.
    pub(crate) fn discard(&self) {
        let _ = fs::remove_file(&self.bytes);
        let _ = fs::remove_file(&self.record);
    }
}

/// the writing end of a download: hashes every byte on its way to disk.
pub(crate) struct PartialWriter {
    cache: Cache,
    file: BufWriter<File>,
    hasher: Hasher,
    written: u64,
    saved: u64,
    bytes: PathBuf,
    record_path: PathBuf,
    record: Record,
}

impl PartialWriter {
    pub(crate) fn written(&self) -> u64 {
        self.written
    }

    /// records how far the download got, so a later run continues from there. the bytes reach
    /// the disk first, so the record never promises bytes a power cut could still take.
    pub(crate) fn save(&mut self) -> io::Result<()> {
        self.file.flush()?;
        self.file.get_ref().sync_data()?;
        self.record.received = self.written;
        let text = serde_json::to_vec(&self.record).map_err(io::Error::other)?;
        self.cache.write_atomically(&self.record_path, &text)?;
        self.saved = self.written;
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
        drop(file);
        let stored = cache.publish(&bytes, hasher.finish(), written)?;
        let _ = fs::remove_file(&record_path);
        Ok(stored)
    }
}

impl Write for PartialWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let written = self.file.write(buffer)?;
        self.hasher.update(&buffer[..written]);
        self.written += written as u64;
        if self.written - self.saved >= SAVE_EVERY {
            self.save()?;
        }
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{cache, download};
    use super::*;

    fn owned(cache: &Cache, url: &str) -> Partial {
        match cache.claim(url).unwrap() {
            Claim::Owned(partial) => partial,
            Claim::Busy => panic!("nothing else holds {url}"),
        }
    }

    #[test]
    fn a_saved_partial_resumes_and_hashes_the_whole_file() {
        let (_dir, cache) = cache();
        let url = "https://example.org/big";
        let validators = Validators {
            etag: Some("\"v1\"".to_owned()),
            last_modified: None,
        };
        let partial = owned(&cache, url);
        let mut writer = partial.writer(0, Some(6), validators.clone()).unwrap();
        writer.write_all(b"abc").unwrap();
        writer.save().unwrap();
        // bytes written after the last save are lost on a crash, so the record must win
        writer.write_all(b"XYZ").unwrap();
        drop(writer);
        let (received, seen) = partial.resumable().unwrap();
        assert_eq!(received, 3);
        assert_eq!(seen, validators);

        let mut resumed = partial.writer(received, Some(6), seen).unwrap();
        assert_eq!(resumed.written(), 3);
        resumed.write_all(b"def").unwrap();
        let stored = resumed.finish().unwrap();
        assert_eq!(fs::read(&stored.path).unwrap(), b"abcdef");
        assert_eq!(stored.size, 6);
        assert!(
            partial.resumable().is_none(),
            "a finished download leaves nothing to resume"
        );
    }

    #[test]
    fn a_download_belongs_to_one_process_at_a_time() {
        let (_dir, cache) = cache();
        let url = "https://example.org/shared";
        let first = owned(&cache, url);
        assert!(matches!(cache.claim(url).unwrap(), Claim::Busy));
        assert!(matches!(
            cache.claim("https://example.org/other").unwrap(),
            Claim::Owned(_)
        ));
        drop(first);
        assert!(matches!(cache.claim(url).unwrap(), Claim::Owned(_)));
    }

    #[test]
    fn a_published_object_can_be_read_while_its_download_is_still_claimed() {
        let (_dir, cache) = cache();
        let url = "https://example.org/a";
        let partial = owned(&cache, url);
        let mut writer = partial.writer(0, None, Validators::default()).unwrap();
        writer.write_all(b"abc").unwrap();
        let stored = writer.finish().unwrap();
        assert_eq!(fs::read(&stored.path).unwrap(), b"abc");
        drop(partial);
        assert_eq!(download(&cache, url, b"abc").path, stored.path);
    }
}
