//! downloads finished but not yet on disk. on linux one flush of the cache's filesystem covers a
//! whole batch of downloads, where a flush per file cost more than downloading many small files.
//! a download takes its name in the cache only after that flush, so a power cut can never leave
//! a damaged object under a hash.

use std::fs::{self, File};
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, PoisonError};

use super::{Cache, Stored};
use crate::digest::{Md5, Sha256};

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

/// the downloads of one batch, and the handle that flushes them together.
#[derive(Debug)]
pub(super) struct Pending {
    #[cfg(target_os = "linux")]
    batch: super::object::Batch,
    finished: Vec<Finished>,
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

impl Cache {
    /// from now until [`Cache::publish_downloads`], finished downloads wait to be flushed
    /// together. false where the filesystem cannot report a failed batch flush, so each download
    /// flushes on its own as before.
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
            *self.pending.lock().unwrap_or_else(PoisonError::into_inner) = Some(Pending {
                batch,
                finished: Vec::new(),
            });
            return Ok(true);
        }
        Ok(false)
    }

    /// keeps a finished download for the batch flush, answering as if it were in place, or
    /// hands it back when no batch is open.
    pub(super) fn defer(&self, finished: Finished) -> Result<Stored, Box<Finished>> {
        let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(pending) = pending.as_mut() else {
            return Err(Box::new(finished));
        };
        let stored = Stored {
            digest: finished.digest,
            size: finished.size,
            md5: finished.md5,
            new: !self.object_path(&finished.digest).exists(),
        };
        pending.finished.push(finished);
        Ok(stored)
    }

    /// flushes the cache's filesystem once, then puts every deferred download in place. later
    /// downloads flush on their own again.
    pub(crate) fn publish_downloads(&self) -> io::Result<()> {
        let Some(pending) = self
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        else {
            return Ok(());
        };
        if pending.finished.is_empty() {
            return Ok(());
        }
        #[cfg(target_os = "linux")]
        pending.batch.flush()?;
        let mut first_error = None;
        for finished in pending.finished {
            if let Err(err) = finished.publish(self) {
                first_error.get_or_insert(err);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    /// puts a deferred download of `url` in place now, with its own flush, so another download
    /// of the same link in this batch can claim it instead of waiting for the batch to end.
    /// false when no download of `url` waits.
    pub(crate) fn publish_download_of(&self, url: &str) -> io::Result<bool> {
        let finished = {
            let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
            let Some(pending) = pending.as_mut() else {
                return Ok(false);
            };
            let Some(index) = pending.finished.iter().position(|item| item.url == url) else {
                return Ok(false);
            };
            pending.finished.swap_remove(index)
        };
        File::open(&finished.bytes)?.sync_all()?;
        finished.publish(self)?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests;
