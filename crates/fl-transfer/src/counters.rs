//! Counts of the work a client did, shared by every transfer it runs.

use std::sync::atomic::{AtomicU64, Ordering};

use fl_model::document::Work;

/// Requests, bytes received, retries and resumes, counted as they happen.
#[derive(Debug, Default)]
pub struct Counters {
    requests: AtomicU64,
    bytes_down: AtomicU64,
    retries: AtomicU64,
    resumes: AtomicU64,
}

/// A reading of [`Counters`].
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Counted {
    /// Requests sent, every redirect hop and range part included.
    pub requests: u64,
    /// Body bytes received.
    pub bytes_down: u64,
    /// Attempts made again after a failure.
    pub retries: u64,
    /// Requests that continued from bytes already kept.
    pub resumes: u64,
}

impl Counters {
    pub(crate) fn request(&self) {
        self.requests.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn received(&self, bytes: u64) {
        self.bytes_down.fetch_add(bytes, Ordering::Relaxed);
    }

    pub(crate) fn retry(&self) {
        self.retries.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn resume(&self) {
        self.resumes.fetch_add(1, Ordering::Relaxed);
    }

    /// The counts so far.
    #[must_use]
    pub fn read(&self) -> Counted {
        Counted {
            requests: self.requests.load(Ordering::Relaxed),
            bytes_down: self.bytes_down.load(Ordering::Relaxed),
            retries: self.retries.load(Ordering::Relaxed),
            resumes: self.resumes.load(Ordering::Relaxed),
        }
    }
}

impl Counted {
    /// Adds these counts to the work counters.
    pub fn add_to(&self, work: &mut Work) {
        work.requests += self.requests;
        work.bytes_down += self.bytes_down;
        work.retries += self.retries;
        work.resumes += self.resumes;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_add_into_work() {
        let counters = Counters::default();
        counters.request();
        counters.request();
        counters.received(10);
        counters.retry();
        counters.resume();
        let mut work = Work::default();
        counters.read().add_to(&mut work);
        counters.read().add_to(&mut work);
        assert_eq!(
            (work.requests, work.bytes_down, work.retries, work.resumes),
            (4, 20, 2, 2)
        );
    }
}
