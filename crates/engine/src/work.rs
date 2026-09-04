//! What a run did, counted so that identical inputs count identically.

use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Work {
    pub bytes_read: u64,
    pub bytes_written: u64,
    pub requests: u64,
    pub file_operations: u64,
}

#[derive(Debug, Default)]
pub struct WorkCounter {
    bytes_read: AtomicU64,
    bytes_written: AtomicU64,
    requests: AtomicU64,
    file_operations: AtomicU64,
}

impl WorkCounter {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn read_bytes(&self, count: u64) {
        self.bytes_read.fetch_add(count, Ordering::Relaxed);
    }

    pub fn wrote_bytes(&self, count: u64) {
        self.bytes_written.fetch_add(count, Ordering::Relaxed);
    }

    pub fn issued_request(&self) {
        self.requests.fetch_add(1, Ordering::Relaxed);
    }

    pub fn touched_file(&self) {
        self.file_operations.fetch_add(1, Ordering::Relaxed);
        if crate::cancel::requested() {
            crate::cancel::stop();
        }
    }

    #[must_use]
    pub fn taken(&self) -> Work {
        Work {
            bytes_read: self.bytes_read.load(Ordering::Relaxed),
            bytes_written: self.bytes_written.load(Ordering::Relaxed),
            requests: self.requests.load(Ordering::Relaxed),
            file_operations: self.file_operations.load(Ordering::Relaxed),
        }
    }
}
