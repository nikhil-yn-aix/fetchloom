//! downloads finished but not yet on disk. on linux one flush of the cache's filesystem covers a
//! group of them, as a flush per file cost more than downloading many small files, and each takes
//! its name only after a flush that covers it, so a power cut never leaves damage under a hash.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use super::{Cache, Stored};
use crate::digest::{Md5, Sha256};

/// the most finished downloads that wait for one flush before they take their names. each keeps
/// its claim's file open until then, and linux lets a process open 1,024 files by default.
const GROUP: usize = 256;

/// how long the first download in a group waits for it to fill before it is flushed and named
/// anyway, so another run that wants one of them does not wait for this run's longest download.
pub(crate) const PUBLISH_WITHIN: Duration = Duration::from_secs(1);

/// a download written in full, read-only, still at its partial name and still claimed.
#[derive(Debug)]
pub(crate) struct Finished {
    pub(super) url: String,
    pub(super) bytes: PathBuf,
    pub(super) record: PathBuf,
    pub(super) digest: Sha256,
    pub(super) size: u64,
    pub(super) md5: Option<Md5>,
    pub(super) lock: Option<Arc<super::partial::Claimed>>,
}

/// the batch a cache is in, shared by every clone. `pending` is always locked before
/// `publishing`.
#[derive(Debug, Default)]
pub(super) struct Deferred {
    pending: Mutex<Option<Pending>>,
    /// held while a group is flushed and put in place, so groups take their names one after
    /// another and bytes thrown away wait for the group that may still name them.
    publishing: Mutex<Publishing>,
}

/// the downloads of one batch not yet flushed, and the handle that flushes them.
#[derive(Debug)]
struct Pending {
    #[cfg(target_os = "linux")]
    batch: Arc<super::object::Batch>,
    finished: Vec<Finished>,
    /// how many finished downloads wait for one flush.
    group: usize,
    /// the digest of every download in this batch, so a large batch is not searched once for
    /// each download.
    digests: HashSet<Sha256>,
    /// when the first download waiting in `finished` came.
    since: Option<Instant>,
}

/// downloads taken from a batch to be flushed once and named together.
struct Group {
    #[cfg(target_os = "linux")]
    batch: Arc<super::object::Batch>,
    finished: Vec<Finished>,
}

/// how the groups of a batch went.
#[derive(Debug, Default)]
struct Publishing {
    /// the first failure, which the batch reports when it ends.
    failed: Option<io::Error>,
    /// a flush failed. it may have taken the report of a write-back error in a later group's
    /// bytes, so no later group takes its names either.
    unflushed: bool,
}

impl Publishing {
    fn result(&self) -> io::Result<()> {
        match &self.failed {
            Some(error) => Err(match error.raw_os_error() {
                Some(code) => io::Error::from_raw_os_error(code),
                None => io::Error::new(error.kind(), error.to_string()),
            }),
            None => Ok(()),
        }
    }
}

impl Finished {
    /// puts the download in place under its hash, then lets other processes claim its link.
    pub(super) fn publish(self, cache: &Cache) -> io::Result<Stored> {
        let stored = cache.link_object(&self.bytes, self.digest, self.size, self.md5)?;
        let _ = fs::remove_file(&self.record);
        drop(self.lock);
        Ok(stored)
    }
}

impl Pending {
    fn take(&mut self) -> Group {
        self.since = None;
        Group {
            #[cfg(target_os = "linux")]
            batch: Arc::clone(&self.batch),
            finished: std::mem::take(&mut self.finished),
        }
    }
}

impl Group {
    /// flushes the cache's filesystem, which covers every download of the group.
    #[cfg_attr(
        not(target_os = "linux"),
        expect(
            clippy::unused_self,
            clippy::unnecessary_wraps,
            reason = "only linux flushes a whole filesystem at once"
        )
    )]
    fn flush(&self) -> io::Result<()> {
        #[cfg(target_os = "linux")]
        self.batch.flush()?;
        Ok(())
    }
}

/// how many finished downloads wait for one flush: a quarter of `open_files`, at most [`GROUP`],
/// leaving the rest to the downloads still running, the cache's own files and placing.
#[cfg_attr(
    not(target_os = "linux"),
    expect(dead_code, reason = "only linux flushes a group of downloads at once")
)]
fn group(open_files: Option<u64>) -> usize {
    open_files.map_or(GROUP, |open| {
        usize::try_from(open / 4).map_or(GROUP, |quarter| quarter.clamp(1, GROUP))
    })
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Cache {
    /// from now until [`Cache::publish_downloads`], finished downloads wait to be flushed a group
    /// at a time. false where the filesystem cannot report a failed batch flush.
    #[cfg_attr(
        not(target_os = "linux"),
        expect(
            unused_variables,
            clippy::unused_self,
            clippy::unnecessary_wraps,
            reason = "only linux flushes a whole filesystem at once"
        )
    )]
    pub(crate) fn defer_downloads(&self, open_files: Option<u64>) -> io::Result<bool> {
        #[cfg(target_os = "linux")]
        if let Some(batch) = self.batch()? {
            let mut pending = lock(&self.deferred.pending);
            *lock(&self.deferred.publishing) = Publishing::default();
            *pending = Some(Pending {
                batch: Arc::new(batch),
                finished: Vec::new(),
                group: group(open_files),
                digests: HashSet::new(),
                since: None,
            });
            return Ok(true);
        }
        Ok(false)
    }

    /// opens a batch as linux does, without its flush, so tests on other systems can follow when
    /// its downloads take their names.
    #[cfg(all(test, not(target_os = "linux")))]
    pub(super) fn defer_unflushed(&self) {
        *lock(&self.deferred.pending) = Some(Pending {
            finished: Vec::new(),
            group: GROUP,
            digests: HashSet::new(),
            since: None,
        });
    }

    /// keeps a finished download for the batch flush, answering as if it were in place, or hands
    /// it back when no batch covers it. a full group is flushed and named before this returns.
    pub(super) fn defer(
        &self,
        finished: Finished,
        metadata: &fs::Metadata,
    ) -> Result<Stored, Box<Finished>> {
        let mut pending = lock(&self.deferred.pending);
        let Some(batch) = pending.as_mut() else {
            return Err(Box::new(finished));
        };
        #[cfg(target_os = "linux")]
        if !batch.batch.covers(metadata) {
            return Err(Box::new(finished));
        }
        #[cfg(not(target_os = "linux"))]
        let _ = metadata;
        // the same bytes from another link in this batch are stored once
        let waiting = !batch.digests.insert(finished.digest);
        let stored = Stored {
            digest: finished.digest,
            size: finished.size,
            md5: finished.md5,
            new: !waiting && !self.holds_intact(&finished.digest, finished.size),
        };
        if batch.finished.is_empty() {
            batch.since = Some(Instant::now());
        }
        batch.finished.push(finished);
        if batch.finished.len() >= batch.group {
            let group = batch.take();
            // taken before the batch is let go, so bytes thrown away meanwhile wait for the group
            let mut publishing = lock(&self.deferred.publishing);
            drop(pending);
            self.publish_group(&mut publishing, group);
        }
        Ok(stored)
    }

    /// puts the downloads still waiting in place after one more flush, and reports the first
    /// failure of the batch. later downloads flush on their own again.
    pub(crate) fn publish_downloads(&self) -> io::Result<()> {
        let mut pending = lock(&self.deferred.pending);
        let Some(mut batch) = pending.take() else {
            return Ok(());
        };
        let mut publishing = lock(&self.deferred.publishing);
        drop(pending);
        self.publish_group(&mut publishing, batch.take());
        std::mem::take(&mut *publishing).failed.map_or(Ok(()), Err)
    }

    /// names the downloads waiting in the batch once the first of them came [`PUBLISH_WITHIN`]
    /// or more before `now`, after a flush that covers them, and says whether it did.
    pub(crate) fn publish_due(&self, now: Instant) -> io::Result<bool> {
        let mut pending = lock(&self.deferred.pending);
        let Some(batch) = pending.as_mut() else {
            return Ok(false);
        };
        let due = !batch.finished.is_empty()
            && batch
                .since
                .is_some_and(|since| now.saturating_duration_since(since) >= PUBLISH_WITHIN);
        if !due {
            return Ok(false);
        }
        let group = batch.take();
        let mut publishing = lock(&self.deferred.publishing);
        drop(pending);
        self.publish_group(&mut publishing, group);
        publishing.result()?;
        Ok(true)
    }

    /// releases this run's completed claims before it waits on another run's download.
    /// the batch stays open so later downloads still share their flushes.
    pub(crate) fn publish_waiting_downloads(&self) -> io::Result<bool> {
        let mut pending = lock(&self.deferred.pending);
        let Some(batch) = pending.as_mut() else {
            return Ok(false);
        };
        let group = batch.take();
        let any = !group.finished.is_empty();
        let mut publishing = lock(&self.deferred.publishing);
        drop(pending);
        self.publish_group(&mut publishing, group);
        publishing.result()?;
        Ok(any)
    }

    /// flushes the cache's filesystem once, then puts every download of `group` in place and
    /// lets go of its claim. after a failed flush, no download takes its name.
    fn publish_group(&self, publishing: &mut Publishing, group: Group) {
        if group.finished.is_empty() || publishing.unflushed {
            return;
        }
        if let Err(err) = group.flush() {
            publishing.unflushed = true;
            publishing.failed.get_or_insert(err);
            return;
        }
        for finished in group.finished {
            if let Err(err) = finished.publish(self) {
                publishing.failed.get_or_insert(err);
            }
        }
    }

    /// drops a deferred download whose bytes at `bytes` are being thrown away, so the flush
    /// never names the next download at that path after this one's hash.
    pub(super) fn forget_deferred(&self, bytes: &Path) {
        let mut pending = lock(&self.deferred.pending);
        if let Some(batch) = pending.as_mut() {
            let (forgotten, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut batch.finished)
                .into_iter()
                .partition(|item| item.bytes == bytes);
            batch.finished = kept;
            for item in forgotten {
                if !batch
                    .finished
                    .iter()
                    .any(|other| other.digest == item.digest)
                {
                    batch.digests.remove(&item.digest);
                }
            }
        }
        // a group taken from the batch may still name these bytes, so they wait until it has
        drop(lock(&self.deferred.publishing));
        drop(pending);
    }

    /// publishes a waiting download through the batch's original flush and error guard.
    /// false when no download of `url` waits.
    pub(crate) fn publish_download_of(&self, url: &str) -> io::Result<bool> {
        let mut pending = lock(&self.deferred.pending);
        let Some(batch) = pending.as_mut() else {
            return Ok(false);
        };
        let Some(index) = batch.finished.iter().position(|item| item.url == url) else {
            return Ok(false);
        };
        let group = Group {
            #[cfg(target_os = "linux")]
            batch: Arc::clone(&batch.batch),
            finished: vec![batch.finished.swap_remove(index)],
        };
        let mut publishing = lock(&self.deferred.publishing);
        drop(pending);
        self.publish_group(&mut publishing, group);
        publishing.result()?;
        Ok(true)
    }

    /// whether publishing these bytes would keep the object already under their hash, as
    /// `link_object` judges it, rather than put them in place as a new one.
    fn holds_intact(&self, digest: &Sha256, size: u64) -> bool {
        let path = self.object_path(digest);
        let found = fs::symlink_metadata(&path);
        matches!(
            super::existing(
                &found,
                size,
                || super::hash_object(&path, Some(size)),
                digest
            ),
            super::Existing::Keep
        )
    }
}

#[cfg(test)]
mod tests;
