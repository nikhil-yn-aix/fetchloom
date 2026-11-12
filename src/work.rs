//! counts of the work a run did. tests assert them, `-v` prints them and `--json` carries them.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};

use serde::Serialize;

use crate::digest::Sha256;

#[derive(Default)]
pub(crate) struct Work {
    pub(crate) requests: AtomicU64,
    pub(crate) bytes_down: AtomicU64,
    pub(crate) retries: AtomicU64,
    pub(crate) resumes: AtomicU64,
    pub(crate) objects_written: AtomicU64,
    /// downloads skipped because the cache already held their bytes, as a link's record or a
    /// stated MD5 showed.
    pub(crate) downloads_skipped: AtomicU64,
    pub(crate) reflinks: AtomicU64,
    pub(crate) hardlinks: AtomicU64,
    pub(crate) copies: AtomicU64,
    /// files placed from objects the run did not write itself.
    pub(crate) placed_from_cache: AtomicU64,
    /// pinned files a mirror gave, which the cache now holds, and their bytes.
    pub(crate) mirror_objects: AtomicU64,
    pub(crate) mirror_bytes: AtomicU64,
    /// pinned files the cache lacked that the project's data folder held, now in the cache, and
    /// their bytes.
    pub(crate) data_folder_objects: AtomicU64,
    pub(crate) data_folder_bytes: AtomicU64,
    /// the objects the run wrote, by the first 8 bytes of each digest, so placing tells them from
    /// objects the cache held. no clock can: a disk keeping whole seconds or a file server's clock
    /// may date a new one before the run began. a million objects clash by a chance near one in 37
    /// million, which only undercounts.
    wrote: Mutex<HashSet<u64>>,
}

/// the counters at one moment, as `--json` prints them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub(crate) struct Counts {
    pub(crate) requests: u64,
    pub(crate) bytes_down: u64,
    pub(crate) retries: u64,
    pub(crate) resumes: u64,
    pub(crate) objects_written: u64,
    pub(crate) downloads_skipped: u64,
    pub(crate) reflinks: u64,
    pub(crate) hardlinks: u64,
    pub(crate) copies: u64,
    pub(crate) placed_from_cache: u64,
    pub(crate) mirror_objects: u64,
    pub(crate) mirror_bytes: u64,
    pub(crate) data_folder_objects: u64,
    pub(crate) data_folder_bytes: u64,
}

impl Work {
    pub(crate) fn add(counter: &AtomicU64, n: u64) {
        counter.fetch_add(n, Ordering::Relaxed);
    }

    /// counts an object the run wrote to the cache, by download or unpacking, and notes it.
    pub(crate) fn wrote(&self, digest: &Sha256) {
        let new = self
            .wrote
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key(digest));
        if new {
            Work::add(&self.objects_written, 1);
        }
    }

    /// whether the cache held the object for `digest` before the run, which did not write it, so
    /// placing it takes no bytes the run downloaded or unpacked.
    pub(crate) fn cached_before(&self, digest: &Sha256) -> bool {
        !self
            .wrote
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(&key(digest))
    }

    pub(crate) fn counts(&self) -> Counts {
        let read = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
        Counts {
            requests: read(&self.requests),
            bytes_down: read(&self.bytes_down),
            retries: read(&self.retries),
            resumes: read(&self.resumes),
            objects_written: read(&self.objects_written),
            downloads_skipped: read(&self.downloads_skipped),
            reflinks: read(&self.reflinks),
            hardlinks: read(&self.hardlinks),
            copies: read(&self.copies),
            placed_from_cache: read(&self.placed_from_cache),
            mirror_objects: read(&self.mirror_objects),
            mirror_bytes: read(&self.mirror_bytes),
            data_folder_objects: read(&self.data_folder_objects),
            data_folder_bytes: read(&self.data_folder_bytes),
        }
    }
}

/// the first 8 bytes of `digest`, which tell objects apart well enough to count them.
fn key(digest: &Sha256) -> u64 {
    let mut first = [0; 8];
    first.copy_from_slice(&digest.bytes()[..8]);
    u64::from_le_bytes(first)
}
