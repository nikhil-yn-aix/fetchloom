//! unfinished downloads: the bytes so far, a record of them, and a lock keeping each to one
//! process. none is ever opened through a link, which anyone who can write the cache could plant.

use std::fs::{self, File, Metadata, OpenOptions, TryLockError};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use bytes::Bytes;
use serde::{Deserialize, Serialize};

use super::deferred::Finished;
use super::{Cache, Stored, Validators};
use crate::digest::{self, Hasher, Sha256};
use crate::place::{FolderUse, real_folder};
use crate::volume::{is_plain, open_unfollowed};

mod hashing;

pub(crate) use hashing::{set_batches as set_hash_batches, set_workers as set_hash_workers};

use hashing::HashPipe;

/// how often a download records its progress, so a killed run loses about this much of it.
const SAVE_EVERY: Duration = Duration::from_secs(1);
/// how much a download writes before it starts writing to the disk while it arrives, so a small
/// one leaves that to the system.
const WRITEBACK_AFTER: u64 = 64 << 20;
/// how much more it writes between those starts.
const WRITEBACK_EVERY: u64 = 32 << 20;
const BUFFER: usize = 1024 * 1024;
/// the largest download hashed without a thread of its own.
const SMALL: u64 = 1024 * 1024;

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
    lock: Option<Arc<Claimed>>,
}

/// the locked file keeping a download to one process. the last clone removes it, still locked,
/// when nothing is left to resume; a run that opened it before then notices in [`take_lock`].
#[derive(Debug)]
pub(super) struct Claimed {
    file: File,
    path: PathBuf,
    bytes: PathBuf,
    record: PathBuf,
}

impl Drop for Claimed {
    fn drop(&mut self) {
        let gone = |path: &Path| {
            fs::symlink_metadata(path).is_err_and(|err| err.kind() == io::ErrorKind::NotFound)
        };
        // a name that moved on to another run's file stays with that run
        if gone(&self.bytes)
            && gone(&self.record)
            && names(&self.path, &self.file).unwrap_or(false)
            && let Err(err) = fs::remove_file(&self.path)
        {
            tracing::debug!(%err, "could not remove the lock file of a finished download");
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct Record {
    // deferred publication still needs the original URL in memory
    #[serde(serialize_with = "super::serialize_url")]
    url: String,
    received: u64,
    /// the SHA-256 of the first `received` bytes, which a resume checks before it trusts them.
    sha256: Sha256,
    #[serde(deserialize_with = "Option::deserialize")]
    length: Option<u64>,
    validators: Validators,
    /// what the streams of a split download finished past the prefix.
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
        let partial = self.partial_folder(&key);
        // the shard is checked as a real folder, and so is the folder it is made in, so a link
        // in place of either never takes a download out of the cache
        for folder in [self.root.join("partial"), partial.clone()] {
            // a folder that cannot be made is refused by the one it would be made in
            let within = folder.parent().unwrap_or(&self.root);
            real_folder(&folder, FolderUse::Cache).map_err(|err| self.explained(within, err))?;
        }
        // the lock gets its own file because windows locks are mandatory: a locked download
        // could not be read through the object it turns into
        let path = partial.join(format!("{key}.lock"));
        let opened = open_claim(&path).map_err(|err| self.explained(&partial, err))?;
        let (folder, stem, lock) = match take_lock(&path, opened) {
            Ok(lock) => (partial, key, Some(lock)),
            Err(TryLockError::WouldBlock) => return Ok(Claim::Busy),
            // without locks, a private file keeps processes apart, at the cost of resuming
            Err(TryLockError::Error(_)) => (
                self.root.join("tmp"),
                format!("{key}.{:016x}", fastrand::u64(..)),
                None,
            ),
        };
        let bytes = folder.join(format!("{stem}.part"));
        let record = folder.join(format!("{stem}.json"));
        let lock = lock.map(|file| {
            Arc::new(Claimed {
                file,
                path,
                bytes: bytes.clone(),
                record: record.clone(),
            })
        });
        Ok(Claim::Owned(Partial {
            cache: self.clone(),
            url: url.to_owned(),
            bytes,
            record,
            lock,
        }))
    }
}

/// locks the claim file `opened` from `path`, opening `path` again until the file locked is the
/// one it names: gc removes lock files nobody holds, and a lock on a file with no name keeps
/// nobody out. an error that keeps the lock from being checked counts as having no locks.
pub(super) fn take_lock(path: &Path, mut opened: File) -> Result<File, TryLockError> {
    loop {
        opened.try_lock()?;
        if names(path, &opened).map_err(TryLockError::Error)? {
            return Ok(opened);
        }
        opened = open_claim(path).map_err(TryLockError::Error)?;
    }
}

/// opens the claim file at `path`, creating it. windows denies opening a removed file until its
/// last handle closes, keeping its name meanwhile, so there a refusal is retried for half a second.
fn open_claim(path: &Path) -> io::Result<File> {
    let retries = if cfg!(windows) { 50 } else { 0 };
    retry_denied(retries, Duration::from_millis(10), || {
        open_unfollowed(path, true)
    })
}

/// `open`, tried up to `retries` more times, `pause` apart, while it is refused as denied. a
/// refusal that outlasts them is a real one and comes back as it was.
fn retry_denied(
    retries: u32,
    pause: Duration,
    mut open: impl FnMut() -> io::Result<File>,
) -> io::Result<File> {
    let mut refusals = 0;
    loop {
        match open() {
            Err(err) if err.kind() == io::ErrorKind::PermissionDenied && refusals < retries => {
                refusals += 1;
                std::thread::sleep(pause);
            }
            opened => return opened,
        }
    }
}

/// the lock file `opened` from `path`, locked, when no download holds it and `path` still names
/// it, rather than a new file another run made: one gc may remove with what it guarded.
pub(super) fn unheld(path: &Path, opened: File) -> Option<File> {
    opened.try_lock().ok()?;
    names(path, &opened).ok()?.then_some(opened)
}

/// whether `path` still names the file `file` has open.
pub(crate) fn names(path: &Path, file: &File) -> io::Result<bool> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let held = file.metadata()?;
        match fs::symlink_metadata(path) {
            Ok(named) => Ok(named.dev() == held.dev() && named.ino() == held.ino()),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(err) => Err(err),
        }
    }
    #[cfg(windows)]
    {
        let held = winapi_util::file::information(file)?;
        let named = match crate::volume::read_unfollowed(path) {
            Ok(named) => winapi_util::file::information(&named)?,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(err) => return Err(err),
        };
        Ok(held.volume_serial_number() == named.volume_serial_number()
            && held.file_index() == named.file_index())
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

    /// writes from byte `offset` on, after hashing the bytes before it again, which fail with
    /// [`io::ErrorKind::InvalidData`] when they no longer hash as recorded or may not be continued.
    /// bytes past it are cut unless `keep` keeps a split download's ranges that still hash as
    /// recorded. `md5` also computes the MD5, to check one a source stated.
    pub(crate) fn writer(
        &self,
        offset: u64,
        length: Option<u64>,
        validators: Validators,
        md5: bool,
        keep: bool,
    ) -> io::Result<PartialWriter> {
        let lean = offset == 0 && !keep && length.is_some_and(|length| length <= SMALL);
        let (file, begun) = self
            .open(offset, keep, lean)
            .map_err(|err| self.cache.explained(folder_of(&self.bytes), err))?;
        let file = Arc::new(file);
        let mut hasher = if md5 {
            Hasher::with_md5()
        } else {
            Hasher::default()
        };
        if offset > 0 {
            hash_range(&file, &mut hasher, 0, offset).map_err(|err| {
                if err.kind() == io::ErrorKind::UnexpectedEof {
                    io::Error::new(io::ErrorKind::InvalidData, err)
                } else {
                    err
                }
            })?;
        }
        let sha256 = hasher.sha256_so_far();
        let recorded = if lean {
            None
        } else {
            self.recorded().filter(|record| record.received == offset)
        };
        if offset > 0 && recorded.as_ref().map(|record| record.sha256) != Some(sha256) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the bytes kept from an earlier download are not the ones it recorded",
            ));
        }
        // a small download hashes faster than a thread starts, so it hashes on the writer's own
        let small = length.is_some_and(|length| length <= SMALL);
        let hasher = if small {
            HashPipe::inline(hasher, Arc::clone(&file), offset)
        } else {
            HashPipe::new(hasher, Arc::clone(&file), offset)?
        };
        let mut writer = PartialWriter {
            cache: self.cache.clone(),
            file,
            hasher,
            _lock: self.lock.clone(),
            prefix: offset,
            pending: Vec::with_capacity(BUFFER),
            lean,
            begun,
            runs: Vec::new(),
            read_back: 0,
            writeback: cfg!(target_os = "linux")
                .then(|| Writeback::new(WRITEBACK_AFTER, WRITEBACK_EVERY)),
            saved: Instant::now(),
            bytes: self.bytes.clone(),
            record_path: self.record.clone(),
            record: Record {
                url: self.url.clone(),
                received: offset,
                sha256,
                length,
                validators,
                ranges: Vec::new(),
            },
        };
        if keep && let Some(record) = recorded {
            writer.keep(record.ranges, length)?;
        }
        writer.record.sha256 = writer.hasher.sha256_so_far(writer.prefix)?;
        Ok(writer)
    }

    /// forgets the bytes so far, so the download starts again from the first byte.
    pub(crate) fn discard(&self) {
        self.cache.forget_deferred(&self.bytes);
        let _ = fs::remove_file(&self.bytes);
        let _ = fs::remove_file(&self.record);
    }

    /// forgets a download that kept no byte, as one refused at its question, since a later run
    /// could continue nothing. its lock goes once the claim ends.
    pub(crate) fn discard_unstarted(&self) {
        if !fs::symlink_metadata(&self.bytes).is_ok_and(|kept| kept.len() > 0) {
            self.discard();
        }
    }

    /// the file for the bytes, cut to `offset` unless `keep`, with its [`super::object::begun`]
    /// time when this run made it. from the first byte, kept bytes it may not continue get a new
    /// file, as a read-only one may be a name of the object, which must never become writable.
    fn open(&self, offset: u64, keep: bool, lean: bool) -> io::Result<(File, Option<SystemTime>)> {
        if lean
            && let Ok(file) = OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&self.bytes)
        {
            let begun = super::object::begun(&file);
            return Ok((file, begun));
        }
        // windows reports no link count in metadata, so a second name is asked for apart
        let kept = open_unfollowed(&self.bytes, false).ok().filter(|file| {
            file.metadata().is_ok_and(|metadata| continuable(&metadata))
                && !crate::place::has_other_names(&self.bytes)
        });
        let (file, begun) = match kept {
            Some(file) => (file, None),
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
                let file = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create_new(true)
                    .open(&self.bytes)?;
                let begun = super::object::begun(&file);
                (file, begun)
            }
        };
        if !keep {
            file.set_len(offset)?;
        }
        Ok((file, begun))
    }

    fn recorded(&self) -> Option<Record> {
        let text = fs::read(&self.record).ok()?;
        let mut record: Record = serde_json::from_slice(&text).ok()?;
        if !super::recorded_url_matches(&record.url, &self.url) {
            return None;
        }
        record.url.clone_from(&self.url);
        Some(record)
    }
}

/// whether this process may write on into the file `metadata` describes: a writable plain file,
/// on unix this user's own with no other name, so writing changes nothing outside the download.
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

/// keeps the download claimed while its writer and hash worker still use the unfinished file.
pub(crate) struct PartialWriter {
    cache: Cache,
    file: Arc<File>,
    hasher: HashPipe,
    // the worker must stop before another writer can claim the download
    _lock: Option<Arc<Claimed>>,
    prefix: u64,
    /// the end of the prefix, not yet in the file.
    pending: Vec<u8>,
    lean: bool,
    /// when the file this run made began, which its seal dates the download past.
    begun: Option<SystemTime>,
    /// what other streams wrote past the prefix, by start.
    runs: Vec<Run>,
    /// bytes read back from the file to hash them, since another stream wrote them.
    read_back: u64,
    /// what was written since the disk last started writing it, on linux.
    writeback: Option<Writeback>,
    saved: Instant,
    bytes: PathBuf,
    record_path: PathBuf,
    record: Record,
}

impl PartialWriter {
    /// counts the prefix accepted by both queues, so callers never need to wait for hashing.
    pub(crate) fn written(&self) -> u64 {
        self.prefix
    }

    /// the file's first `most` bytes, or all of a shorter prefix.
    pub(crate) fn head(&mut self, most: usize) -> io::Result<Vec<u8>> {
        self.flush_pending()?;
        let mut head =
            vec![0; usize::try_from(self.prefix).map_or(most, |prefix| prefix.min(most))];
        read_exact_at(&self.file, 0, &mut head)?;
        Ok(head)
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

    /// copies borrowed bytes so hashing can continue after the caller reuses its buffer.
    pub(crate) fn write_at(&mut self, at: u64, bytes: &[u8]) -> io::Result<()> {
        self.write_chunk(at, &Bytes::copy_from_slice(bytes))
    }

    /// shares response chunks with the hash worker, avoiding a copy on the download path.
    /// bytes past the prefix are read back only once the prefix reaches them.
    pub(crate) fn write_chunk(&mut self, at: u64, bytes: &Bytes) -> io::Result<()> {
        self.put_chunk(at, bytes)
            .map_err(|err| self.cache.explained(folder_of(&self.bytes), err))
    }

    fn put_chunk(&mut self, at: u64, bytes: &Bytes) -> io::Result<()> {
        let end = at + bytes.len() as u64;
        let overlaps =
            at < self.prefix || self.runs.iter().any(|run| at < run.end && run.start < end);
        if overlaps {
            return Err(io::Error::other(
                "two streams of a download wrote the same bytes",
            ));
        }
        if at == self.prefix {
            if self.pending.len() + bytes.len() > BUFFER {
                self.flush_pending()?;
            }
            if bytes.len() >= BUFFER {
                write_all_at(&self.file, at, bytes)?;
                self.wrote(at, bytes.len());
            }
            self.hasher.update(bytes.clone())?;
            if bytes.len() < BUFFER {
                self.pending.extend_from_slice(bytes);
            }
            self.prefix = end;
            self.absorb()?;
        } else {
            write_all_at(&self.file, at, bytes)?;
            self.wrote(at, bytes.len());
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

    /// flushes before queuing read-back, so the hash worker only reads bytes already written.
    fn absorb(&mut self) -> io::Result<()> {
        while self
            .runs
            .first()
            .is_some_and(|run| run.start == self.prefix)
        {
            self.flush_pending()?;
            let run = self.runs.remove(0);
            self.hasher.range(run.start, run.end)?;
            self.read_back += run.end - run.start;
            self.prefix = run.end;
        }
        Ok(())
    }

    fn flush_pending(&mut self) -> io::Result<()> {
        if !self.pending.is_empty() {
            let at = self.prefix - self.pending.len() as u64;
            write_all_at(&self.file, at, &self.pending)?;
            self.wrote(at, self.pending.len());
            self.pending.clear();
        }
        Ok(())
    }

    /// notes `len` bytes written at `at`, and starts the disk writing what was written since it
    /// last did, once [`Writeback`] says to.
    fn wrote(&mut self, at: u64, len: usize) {
        if let Some(writeback) = &mut self.writeback {
            for (start, end) in writeback.wrote(at, len as u64) {
                start_writeback(&self.file, start, end);
            }
        }
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

    /// records the prefix written so far with its hash, without a durable flush, which would stall
    /// every second; a resume rechecks the hashes in case a power cut lost bytes the record counts.
    pub(crate) fn save(&mut self) -> io::Result<()> {
        self.checkpoint()
            .map_err(|err| self.cache.explained(folder_of(&self.record_path), err))
    }

    fn checkpoint(&mut self) -> io::Result<()> {
        self.flush_pending()?;
        if self.lean && self.runs.is_empty() && self.record.length == Some(self.prefix) {
            return Ok(());
        }
        self.record.received = self.prefix;
        self.record.sha256 = self.hasher.sha256_so_far(self.prefix)?;
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
    pub(crate) fn finish(self) -> io::Result<Stored> {
        let cache = self.cache.clone();
        let folder = folder_of(&self.bytes).to_path_buf();
        self.seal_and_publish()
            .map_err(|err| cache.explained(&folder, err))
    }

    fn seal_and_publish(mut self) -> io::Result<Stored> {
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
            record,
            lean,
            begun,
            _lock: lock,
            ..
        } = self;
        // read-back must finish before resizing could hide a short read
        let (digest, md5) = hasher.finish(prefix)?;
        // bytes past the prefix are not in the hash, such as the end of an earlier version
        let metadata = file.metadata()?;
        if metadata.len() != prefix {
            file.set_len(prefix)?;
        }
        // dated while the handle may still set times, before the flag and the flush
        if let Err(err) = super::object::date_by_birth(&file, begun) {
            tracing::debug!(%err, "could not date a download by its birth");
        }
        // sealed through the handle that wrote it: by name, a link swapped in since would be
        // sealed in its place
        let mut permissions = if lean && metadata.len() == prefix {
            metadata.permissions()
        } else {
            file.metadata()?.permissions()
        };
        permissions.set_readonly(true);
        file.set_permissions(permissions)?;
        let finished = Finished {
            url: record.url,
            bytes,
            record: record_path,
            digest,
            size: prefix,
            md5,
            lock,
        };
        // in a batch, one flush covers every download before any takes its name
        match cache.defer(finished, &metadata) {
            Ok(stored) => Ok(stored),
            Err(finished) => {
                file.sync_all()?;
                drop(file);
                let objects = cache.object_path(&finished.digest);
                (*finished)
                    .publish(&cache)
                    .map_err(|err| cache.explained(folder_of(&objects), err))
            }
        }
    }
}

fn folder_of(path: &Path) -> &Path {
    path.parent().unwrap_or(path)
}

/// when a download starts the disk writing, rather than leave every byte for the final flush:
/// once it has written `after` bytes, then each time it has written `every` more.
#[derive(Debug)]
struct Writeback {
    after: u64,
    every: u64,
    /// all the bytes written.
    written: u64,
    /// the ranges written since the disk last started, one per stream, and their bytes.
    waiting: Vec<(u64, u64)>,
    waiting_bytes: u64,
}

impl Writeback {
    fn new(after: u64, every: u64) -> Writeback {
        Writeback {
            after,
            every,
            written: 0,
            waiting: Vec::new(),
            waiting_bytes: 0,
        }
    }

    /// notes `len` bytes written at `at`, and says which ranges the disk should start writing
    /// now: every one written since it last started, once it is time.
    fn wrote(&mut self, at: u64, len: u64) -> Vec<(u64, u64)> {
        if len == 0 {
            return Vec::new();
        }
        self.written += len;
        self.waiting_bytes += len;
        match self.waiting.iter_mut().find(|range| range.1 == at) {
            Some(range) => range.1 = at + len,
            None => self.waiting.push((at, at + len)),
        }
        if self.written < self.after || self.waiting_bytes < self.every {
            return Vec::new();
        }
        self.waiting_bytes = 0;
        std::mem::take(&mut self.waiting)
    }
}

/// starts the disk writing `file` from `start` up to `end`, without waiting: `fadvise` with
/// `DONTNEED` starts write-back as `sync_file_range` would, which needs unsafe code. pages still
/// dirty or being written stay in memory; one already written may leave.
#[cfg(target_os = "linux")]
fn start_writeback(file: &File, start: u64, end: u64) {
    // no length would mean the rest of the file
    let Some(len) = std::num::NonZeroU64::new(end.saturating_sub(start)) else {
        return;
    };
    if let Err(err) = rustix::fs::fadvise(file, start, Some(len), rustix::fs::Advice::DontNeed) {
        tracing::debug!(%err, "could not start writing a download to the disk");
    }
}

#[cfg(not(target_os = "linux"))]
fn start_writeback(_: &File, _: u64, _: u64) {}

impl Write for PartialWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.write_at(self.prefix, buffer)?;
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flush_pending()
    }
}

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
