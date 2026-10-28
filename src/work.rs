//! counts of the work a run did. tests assert them, `-v` prints them and `--json` carries them.

use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;

#[derive(Default)]
pub(crate) struct Work {
    pub(crate) requests: AtomicU64,
    pub(crate) bytes_down: AtomicU64,
    pub(crate) retries: AtomicU64,
    pub(crate) resumes: AtomicU64,
    pub(crate) objects_written: AtomicU64,
    pub(crate) cache_hits: AtomicU64,
    pub(crate) reflinks: AtomicU64,
    pub(crate) hardlinks: AtomicU64,
    pub(crate) copies: AtomicU64,
}

/// the counters at one moment, as `--json` prints them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub(crate) struct Counts {
    pub(crate) requests: u64,
    pub(crate) bytes_down: u64,
    pub(crate) retries: u64,
    pub(crate) resumes: u64,
    pub(crate) objects_written: u64,
    pub(crate) cache_hits: u64,
    pub(crate) reflinks: u64,
    pub(crate) hardlinks: u64,
    pub(crate) copies: u64,
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
            cache_hits: read(&self.cache_hits),
            reflinks: read(&self.reflinks),
            hardlinks: read(&self.hardlinks),
            copies: read(&self.copies),
        }
    }
}
