//! What a run did, counted so that identical inputs count identically.

use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

/// How much work one run did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Work {
    /// How many bytes the run read from a file.
    pub bytes_read: u64,
    /// How many bytes the run wrote to a file.
    pub bytes_written: u64,
    /// How many requests the run issued to a source, retries and probes
    /// included.
    pub requests: u64,
    /// How many files and directories the run created, renames it performed,
    /// and flushes it issued.
    pub file_operations: u64,
}

/// Where every part of a run counts the work it did.
///
/// Shared by reference: one run has one set of numbers.
#[derive(Debug, Default)]
pub struct WorkCounter {
    bytes_read: AtomicU64,
    bytes_written: AtomicU64,
    requests: AtomicU64,
    file_operations: AtomicU64,
}

impl WorkCounter {
    /// Starts a counter that has counted nothing.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Counts bytes read from a file.
    pub fn read_bytes(&self, count: u64) {
        self.bytes_read.fetch_add(count, Ordering::Relaxed);
    }

    /// Counts bytes written to a file.
    pub fn wrote_bytes(&self, count: u64) {
        self.bytes_written.fetch_add(count, Ordering::Relaxed);
    }

    /// Counts one request issued to a source.
    pub fn issued_request(&self) {
        self.requests.fetch_add(1, Ordering::Relaxed);
    }

    /// Counts one file created, renamed, or flushed.
    ///
    /// A file operation is also the boundary between one unit of work and the
    /// next, so it is where a run that has been asked to stop stops. It is
    /// counted first, because the operation it counts has already happened and
    /// a run that ends must still say what it did.
    pub fn touched_file(&self) {
        self.file_operations.fetch_add(1, Ordering::Relaxed);
        if crate::cancel::requested() {
            crate::cancel::stop();
        }
    }

    /// Returns what has been counted so far, leaving the counter running.
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
