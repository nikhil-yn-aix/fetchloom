//! unfinished downloads: the bytes so far, a record of what they are, and a lock so that two
//! processes never write the same download.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::{Cache, Stored, Validators};
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
    _lock: Option<Arc<File>>,
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
        let record = self.recorded()?;
        let on_disk = fs::metadata(&self.bytes).ok()?.len();
        (record.received > 0 && on_disk >= record.received)
            .then_some((record.received, record.validators))
    }

    /// writes from byte `offset` on. the record is the truth: bytes past it are cut off, and the
    /// bytes before it are hashed again so the final hash covers the whole file. bytes that no
    /// longer hash to what the record says, as after a power cut, fail with
    /// [`io::ErrorKind::InvalidData`]. `md5` also computes the MD5, to check one a source stated.
    pub(crate) fn writer(
        &self,
        offset: u64,
        length: Option<u64>,
        validators: Validators,
        md5: bool,
    ) -> io::Result<PartialWriter> {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&self.bytes)?;
        file.set_len(offset)?;
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

    fn recorded(&self) -> Option<Record> {
        let text = fs::read(&self.record).ok()?;
        let record: Record = serde_json::from_slice(&text).ok()?;
        (record.url == self.url).then_some(record)
    }
}

/// the writing end of a download: hashes every byte on its way to disk.
pub(crate) struct PartialWriter {
    cache: Cache,
    file: BufWriter<File>,
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
        drop(file);
        let (digest, md5) = hasher.finish();
        let stored = cache.publish(&bytes, digest, written, md5)?;
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
mod tests {
    use super::super::tests::{cache, store};
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
        let mut writer = partial
            .writer(0, Some(6), validators.clone(), true)
            .unwrap();
        writer.write_all(b"abc").unwrap();
        writer.save().unwrap();
        // bytes written after the last save are lost on a crash, so the record must win
        writer.write_all(b"XYZ").unwrap();
        drop(writer);
        let (received, seen) = partial.resumable().unwrap();
        assert_eq!(received, 3);
        assert_eq!(seen, validators);

        let mut resumed = partial.writer(received, Some(6), seen, true).unwrap();
        assert_eq!(resumed.written(), 3);
        resumed.write_all(b"def").unwrap();
        let stored = resumed.finish().unwrap();
        assert_eq!(fs::read(&stored.path).unwrap(), b"abcdef");
        assert_eq!(stored.size, 6);
        assert_eq!(
            stored.md5.map(|md5| md5.hex()).as_deref(),
            Some("e80b5017098950fc58aad83c8c14978e"),
            "md5 covers the resumed prefix too"
        );
        assert!(
            partial.resumable().is_none(),
            "a finished download leaves nothing to resume"
        );
    }

    #[test]
    fn bytes_that_differ_from_their_record_are_never_continued() {
        let (_dir, cache) = cache();
        let partial = owned(&cache, "https://example.org/big");
        let mut writer = partial
            .writer(0, Some(6), Validators::default(), false)
            .unwrap();
        writer.write_all(b"abc").unwrap();
        writer.save().unwrap();
        drop(writer);
        // a power cut can keep the record and lose the bytes it counts
        fs::write(&partial.bytes, b"\0\0\0").unwrap();
        let (received, validators) = partial.resumable().unwrap();
        let Err(error) = partial.writer(received, Some(6), validators, false) else {
            panic!("zeros were continued as if they were abc");
        };
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
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
        let mut writer = partial
            .writer(0, None, Validators::default(), false)
            .unwrap();
        writer.write_all(b"abc").unwrap();
        let stored = writer.finish().unwrap();
        assert_eq!(fs::read(&stored.path).unwrap(), b"abc");
        drop(partial);
        assert_eq!(store(&cache, b"abc").path, stored.path);
    }
}
