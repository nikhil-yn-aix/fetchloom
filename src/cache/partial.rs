//! unfinished downloads: the bytes so far, a record of what they are, and a lock so that two
//! processes never write the same download. anyone who can write to the cache could plant a link
//! among them, so none of them is ever opened through one.

use std::fs::{self, File, Metadata, OpenOptions, TryLockError};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::{Cache, Stored, Validators};
use crate::digest::{self, Hasher, Sha256};
use crate::place::{FolderUse, real_folder};
use crate::volume::is_plain;

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
    /// what the streams of a split download finished past the prefix.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    ranges: Vec<Range>,
}

/// bytes written past the prefix, from `start` up to `end`, and their SHA-256, which a resume
/// checks before it keeps them.
#[derive(Debug, Serialize, Deserialize)]
struct Range {
    start: u64,
    end: u64,
    sha256: Sha256,
}

/// a run of bytes one stream wrote past the prefix, hashed on its own for the checkpoint.
struct Run {
    start: u64,
    end: u64,
    hasher: Hasher,
}

impl Cache {
    /// claims the unfinished download of `url` for this process, unless another process has it.
    pub(crate) fn claim(&self, url: &str) -> io::Result<Claim> {
        let key = digest::sha256(url.as_bytes()).hex();
        let partial = self.root.join("partial");
        real_folder(&partial, FolderUse::Cache)?;
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
    /// how many bytes an earlier run kept from the first on, and how the server identified them
    /// then. a split download may have kept ranges past them with none at the start.
    pub(crate) fn resumable(&self) -> Option<(u64, Validators)> {
        let record = self.recorded()?;
        let kept = fs::symlink_metadata(&self.bytes).ok()?;
        let any = record.received > 0 || !record.ranges.is_empty();
        (any && continuable(&kept) && kept.len() >= record.received)
            .then_some((record.received, record.validators))
    }

    /// writes from byte `offset` on. the record is the truth: the bytes before `offset` are
    /// hashed again so the final hash covers the whole file, and bytes past it are cut off,
    /// unless `keep` keeps the ranges a split download finished there whose bytes still hash to
    /// what the record says. bytes before `offset` that no longer hash to what the record says,
    /// as after a power cut, or that this process may not continue, fail with
    /// [`io::ErrorKind::InvalidData`]. `md5` also computes the MD5, to check one a source stated.
    pub(crate) fn writer(
        &self,
        offset: u64,
        length: Option<u64>,
        validators: Validators,
        md5: bool,
        keep: bool,
    ) -> io::Result<PartialWriter> {
        let file = self.open(offset, keep)?;
        let mut writer = PartialWriter {
            cache: self.cache.clone(),
            file,
            _lock: self.lock.clone(),
            hasher: if md5 {
                Hasher::with_md5()
            } else {
                Hasher::default()
            },
            prefix: 0,
            pending: Vec::with_capacity(BUFFER),
            runs: Vec::new(),
            read_back: 0,
            saved: Instant::now(),
            bytes: self.bytes.clone(),
            record_path: self.record.clone(),
            record: Record {
                url: self.url.clone(),
                received: offset,
                sha256: digest::sha256(b""),
                length,
                validators,
                ranges: Vec::new(),
            },
        };
        if offset > 0 {
            hash_range(&writer.file, &mut writer.hasher, 0, offset).map_err(|err| {
                if err.kind() == io::ErrorKind::UnexpectedEof {
                    io::Error::new(io::ErrorKind::InvalidData, err)
                } else {
                    err
                }
            })?;
            writer.prefix = offset;
        }
        let recorded = self.recorded().filter(|record| record.received == offset);
        if offset > 0
            && recorded.as_ref().map(|record| record.sha256) != Some(writer.hasher.sha256_so_far())
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the bytes kept from an earlier download are not the ones it recorded",
            ));
        }
        if keep && let Some(record) = recorded {
            writer.keep(record.ranges, length)?;
        }
        writer.record.sha256 = writer.hasher.sha256_so_far();
        Ok(writer)
    }

    /// forgets the bytes so far, so the download starts again from the first byte.
    pub(crate) fn discard(&self) {
        let _ = fs::remove_file(&self.bytes);
        let _ = fs::remove_file(&self.record);
    }

    /// the file for the bytes, cut to their first `offset` unless `keep` keeps what lies past
    /// them. bytes an earlier run kept are used only when this process may continue them.
    /// otherwise a download from the first byte removes their name and makes a new file there: a
    /// read-only one was sealed by a run that did not finish publishing, and may already be a
    /// second name of the object, which must never become writable.
    fn open(&self, offset: u64, keep: bool) -> io::Result<File> {
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
                if let Err(err) = self.cache.volume.remove(&self.bytes)
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
        if !keep {
            file.set_len(offset)?;
        }
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
    open_plain(path, &mut options)
}

/// shares the no-follow checks so reading an object never needs write access to its bytes.
pub(crate) fn read_unfollowed(path: &Path) -> io::Result<File> {
    open_plain(path, OpenOptions::new().read(true))
}

fn open_plain(path: &Path, options: &mut OpenOptions) -> io::Result<File> {
    #[cfg(unix)]
    {
        use rustix::fs::OFlags;
        use std::os::unix::fs::OpenOptionsExt;
        // a fifo must not block opening before its handle can be refused
        options.custom_flags((OFlags::NOFOLLOW | OFlags::NONBLOCK).bits().cast_signed());
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
                "{} is a link or a special file, which fetchloom never reads or writes",
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

/// the writing end of a download: hashes every byte on its way to disk, in order. it keeps the
/// download claimed until it is done, since a stopped run may drop its claim while this still
/// writes.
pub(crate) struct PartialWriter {
    cache: Cache,
    file: File,
    _lock: Option<Arc<File>>,
    /// covers the prefix: the bytes from the first on that are written, in order.
    hasher: Hasher,
    prefix: u64,
    /// the end of the prefix, not yet in the file.
    pending: Vec<u8>,
    /// what other streams wrote past the prefix, by start.
    runs: Vec<Run>,
    /// bytes read back from the file to hash them, since another stream wrote them.
    read_back: u64,
    saved: Instant,
    bytes: PathBuf,
    record_path: PathBuf,
    record: Record,
}

impl PartialWriter {
    /// how many bytes from the first on are written and hashed.
    pub(crate) fn written(&self) -> u64 {
        self.prefix
    }

    /// how many bytes past the prefix are written.
    pub(crate) fn kept(&self) -> u64 {
        self.runs.iter().map(|run| run.end - run.start).sum()
    }

    /// the ranges up to `total` that nothing has written, in order.
    pub(crate) fn gaps(&self, total: u64) -> Vec<(u64, u64)> {
        let mut gaps = Vec::new();
        let mut at = self.prefix;
        for run in &self.runs {
            if run.start > at {
                gaps.push((at, run.start.min(total)));
            }
            at = at.max(run.end);
        }
        if at < total {
            gaps.push((at, total));
        }
        gaps
    }

    #[cfg(test)]
    pub(crate) fn read_back(&self) -> u64 {
        self.read_back
    }

    /// writes `bytes` at `at`. bytes that continue the prefix are hashed as they pass. bytes past
    /// it are hashed once the prefix reaches them, read back from the file, so a download that
    /// arrives in order reads nothing back.
    pub(crate) fn write_at(&mut self, at: u64, bytes: &[u8]) -> io::Result<()> {
        let end = at + bytes.len() as u64;
        let overlaps =
            at < self.prefix || self.runs.iter().any(|run| at < run.end && run.start < end);
        if overlaps {
            return Err(io::Error::other(
                "two streams of a download wrote the same bytes",
            ));
        }
        if at == self.prefix {
            self.hasher.update(bytes);
            if self.pending.len() + bytes.len() > BUFFER {
                self.flush_pending()?;
            }
            if bytes.len() >= BUFFER {
                write_all_at(&self.file, at, bytes)?;
            } else {
                self.pending.extend_from_slice(bytes);
            }
            self.prefix = end;
            self.absorb()?;
        } else {
            write_all_at(&self.file, at, bytes)?;
            if let Some(run) = self.runs.iter_mut().find(|run| run.end == at) {
                run.hasher.update(bytes);
                run.end = end;
            } else {
                let mut hasher = Hasher::default();
                hasher.update(bytes);
                let index = self.runs.partition_point(|run| run.start < at);
                self.runs.insert(
                    index,
                    Run {
                        start: at,
                        end,
                        hasher,
                    },
                );
            }
        }
        if self.saved.elapsed() >= SAVE_EVERY {
            self.save()?;
        }
        Ok(())
    }

    /// moves the prefix over the runs it reached, hashing them from the file.
    fn absorb(&mut self) -> io::Result<()> {
        while self
            .runs
            .first()
            .is_some_and(|run| run.start == self.prefix)
        {
            self.flush_pending()?;
            let run = self.runs.remove(0);
            hash_range(&self.file, &mut self.hasher, run.start, run.end)?;
            self.read_back += run.end - run.start;
            self.prefix = run.end;
        }
        Ok(())
    }

    fn flush_pending(&mut self) -> io::Result<()> {
        if !self.pending.is_empty() {
            let at = self.prefix - self.pending.len() as u64;
            write_all_at(&self.file, at, &self.pending)?;
            self.pending.clear();
        }
        Ok(())
    }

    /// takes on the ranges an earlier run recorded past the prefix whose bytes still hash to
    /// what it recorded. the others are fetched again.
    fn keep(&mut self, ranges: Vec<Range>, length: Option<u64>) -> io::Result<()> {
        let mut at = self.prefix;
        for range in ranges {
            let fits = range.start >= at
                && range.end > range.start
                && length.is_none_or(|length| range.end <= length);
            if !fits {
                continue;
            }
            let mut hasher = Hasher::default();
            match hash_range(&self.file, &mut hasher, range.start, range.end) {
                Ok(()) if hasher.sha256_so_far() == range.sha256 => {
                    self.runs.push(Run {
                        start: range.start,
                        end: range.end,
                        hasher,
                    });
                    at = range.end;
                }
                Ok(()) => {}
                Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => {}
                Err(err) => return Err(err),
            }
        }
        self.absorb()
    }

    /// records how far the download got, so a later run continues from there. the bytes are not
    /// flushed first, which would stall the download every second. the record holds their hash
    /// instead, and a resume that finds other bytes starts over, or for a range past the prefix,
    /// fetches it again.
    pub(crate) fn save(&mut self) -> io::Result<()> {
        self.flush_pending()?;
        self.record.received = self.prefix;
        self.record.sha256 = self.hasher.sha256_so_far();
        self.record.ranges = self
            .runs
            .iter()
            .map(|run| Range {
                start: run.start,
                end: run.end,
                sha256: run.hasher.sha256_so_far(),
            })
            .collect();
        let text = serde_json::to_vec(&self.record).map_err(io::Error::other)?;
        self.cache.write_hint(&self.record_path, &text)?;
        self.saved = Instant::now();
        Ok(())
    }

    /// puts the finished file into the cache under its hash. a download with bytes missing
    /// before its end fails with [`io::ErrorKind::InvalidData`].
    pub(crate) fn finish(mut self) -> io::Result<Stored> {
        self.flush_pending()?;
        if !self.runs.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "parts of the download are missing",
            ));
        }
        if self.read_back > 0 {
            tracing::debug!(
                read_back = self.read_back,
                "hashed bytes other streams wrote"
            );
        }
        let PartialWriter {
            cache,
            file,
            hasher,
            prefix,
            bytes,
            record_path,
            ..
        } = self;
        // bytes past the prefix are not in the hash, such as the end of an earlier version
        if file.metadata()?.len() != prefix {
            file.set_len(prefix)?;
        }
        file.sync_all()?;
        // sealed through the handle that wrote it: by name, a link swapped in since would be
        // sealed in its place
        let mut permissions = file.metadata()?.permissions();
        permissions.set_readonly(true);
        file.set_permissions(permissions)?;
        drop(file);
        let (digest, md5) = hasher.finish();
        let stored = cache.link_object(&bytes, digest, prefix, md5)?;
        let _ = fs::remove_file(&record_path);
        Ok(stored)
    }
}

impl Write for PartialWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.write_at(self.prefix, buffer)?;
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flush_pending()
    }
}

/// hashes the bytes of `file` from `start` up to `end`.
fn hash_range(file: &File, hasher: &mut Hasher, start: u64, end: u64) -> io::Result<()> {
    let mut buffer = vec![0; usize::try_from(end - start).map_or(BUFFER, |left| left.min(BUFFER))];
    let mut at = start;
    while at < end {
        let size = usize::try_from(end - at).map_or(buffer.len(), |left| left.min(buffer.len()));
        read_exact_at(file, at, &mut buffer[..size])?;
        hasher.update(&buffer[..size]);
        at += size as u64;
    }
    Ok(())
}

#[cfg(unix)]
fn write_all_at(file: &File, at: u64, bytes: &[u8]) -> io::Result<()> {
    std::os::unix::fs::FileExt::write_all_at(file, bytes, at)
}

#[cfg(unix)]
fn read_exact_at(file: &File, at: u64, buffer: &mut [u8]) -> io::Result<()> {
    std::os::unix::fs::FileExt::read_exact_at(file, buffer, at)
}

#[cfg(windows)]
fn write_all_at(file: &File, mut at: u64, mut bytes: &[u8]) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !bytes.is_empty() {
        match file.seek_write(bytes, at) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(written) => {
                bytes = &bytes[written..];
                at += written as u64;
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err),
        }
    }
    Ok(())
}

#[cfg(windows)]
fn read_exact_at(file: &File, mut at: u64, mut buffer: &mut [u8]) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buffer.is_empty() {
        match file.seek_read(buffer, at) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(read) => {
                buffer = &mut buffer[read..];
                at += read as u64;
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err),
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests;
