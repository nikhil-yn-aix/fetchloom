//! downloads finished but not yet on disk. on linux one flush of the cache's filesystem covers a
//! group of downloads, where a flush per file cost more than downloading many small files.
//! a download takes its name in the cache only after a flush that covers it, so a power cut can
//! never leave a damaged object under a hash.

use std::collections::HashSet;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use super::{Cache, Stored};
use crate::digest::{Md5, Sha256};

/// how many finished downloads wait for one flush before they take their names. each keeps its
/// claim's file open until then, and linux lets a process open 1,024 files by default.
const GROUP: usize = 256;

/// a download written in full, read-only, still at its partial name and still claimed.
#[derive(Debug)]
pub(crate) struct Finished {
    pub(super) url: String,
    pub(super) bytes: PathBuf,
    pub(super) record: PathBuf,
    pub(super) digest: Sha256,
    pub(super) size: u64,
    pub(super) md5: Option<Md5>,
    pub(super) lock: Option<Arc<File>>,
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
    /// the digest of every download in this batch, so a large batch is not searched once for
    /// each download.
    digests: HashSet<Sha256>,
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

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Cache {
    /// from now until [`Cache::publish_downloads`], finished downloads wait to be flushed
    /// together, a group at a time. false where the filesystem cannot report a failed batch
    /// flush, so each download flushes on its own as before.
    #[cfg_attr(
        not(target_os = "linux"),
        expect(
            clippy::unused_self,
            clippy::unnecessary_wraps,
            reason = "only linux flushes a whole filesystem at once"
        )
    )]
    pub(crate) fn defer_downloads(&self) -> io::Result<bool> {
        #[cfg(target_os = "linux")]
        if let Some(batch) = self.batch()? {
            let mut pending = lock(&self.deferred.pending);
            *lock(&self.deferred.publishing) = Publishing::default();
            *pending = Some(Pending {
                batch: Arc::new(batch),
                finished: Vec::new(),
                digests: HashSet::new(),
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
            digests: HashSet::new(),
        });
    }

    /// keeps a finished download for the batch flush, answering as if it were in place, or
    /// hands it back when no batch is open. once a group of downloads waits, they are flushed and
    /// put in place before this one returns.
    pub(super) fn defer(&self, finished: Finished) -> Result<Stored, Box<Finished>> {
        let mut pending = lock(&self.deferred.pending);
        let Some(batch) = pending.as_mut() else {
            return Err(Box::new(finished));
        };
        // the same bytes from another link in this batch are stored once
        let waiting = !batch.digests.insert(finished.digest);
        let stored = Stored {
            digest: finished.digest,
            size: finished.size,
            md5: finished.md5,
            new: !waiting && !self.object_path(&finished.digest).exists(),
        };
        batch.finished.push(finished);
        if batch.finished.len() >= GROUP {
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

    /// puts a deferred download of `url` in place now, with its own flush, so another download
    /// of the same link in this batch can claim it instead of waiting for the batch to end.
    /// false when no download of `url` waits.
    pub(crate) fn publish_download_of(&self, url: &str) -> io::Result<bool> {
        let finished = {
            let mut pending = lock(&self.deferred.pending);
            let Some(batch) = pending.as_mut() else {
                return Ok(false);
            };
            let Some(index) = batch.finished.iter().position(|item| item.url == url) else {
                return Ok(false);
            };
            batch.finished.swap_remove(index)
        };
        File::open(&finished.bytes)?.sync_all()?;
        finished.publish(self)?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests;
