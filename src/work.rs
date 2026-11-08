//! counts of the work a run did. tests assert them, `-v` prints them and `--json` carries them.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

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
    /// files placed from bytes the cache held before the run began.
    pub(crate) placed_from_cache: AtomicU64,
    /// when the run began, in nanoseconds since 1970, so placing can tell an object the cache
    /// already held from one the run downloaded or unpacked.
    pub(crate) began: u64,
}

impl Default for Work {
    fn default() -> Work {
        Work {
            requests: AtomicU64::default(),
            bytes_down: AtomicU64::default(),
            retries: AtomicU64::default(),
            resumes: AtomicU64::default(),
            objects_written: AtomicU64::default(),
            downloads_skipped: AtomicU64::default(),
            reflinks: AtomicU64::default(),
            hardlinks: AtomicU64::default(),
            copies: AtomicU64::default(),
            placed_from_cache: AtomicU64::default(),
            began: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |since| {
                    u64::try_from(since.as_nanos()).unwrap_or(u64::MAX)
                }),
        }
    }
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
}

impl Work {
    pub(crate) fn add(counter: &AtomicU64, n: u64) {
        counter.fetch_add(n, Ordering::Relaxed);
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
        }
    }
}
