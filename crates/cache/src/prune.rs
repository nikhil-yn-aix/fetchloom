//! Mark, grace, sweep.
//!
//! The lock is the authority in both directions. An object under any lock is
//! never swept, and an object being swept is under the sweeper's exclusive
//! lock, so no reader can be part way through opening it. The grace period only
//! stops a prune from removing an object a run is about to lease.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fetchloom_engine::digest::ContentDigest;
use fetchloom_engine::error::{Error, ErrorKind};
use fetchloom_engine::seam::platform::Platform;
use fetchloom_engine::seam::store::{PruneReport, Store};

use crate::Cache;
use crate::failure;
use crate::record::{self, Mark};

/// How long an object stays marked before a sweep may remove it.
///
/// A race window rather than a retention policy, so it is not configurable: a
/// shorter one is a corruption and a longer one is a wait with no benefit.
pub const GRACE: Duration = Duration::from_secs(60);

/// Marks what nothing refers to, then removes what has been marked longer than
/// the grace period.
///
/// # Errors
///
/// Fails when the cache cannot be read or written.
pub fn run<P: Platform>(cache: &Cache<P>, grace: Duration) -> Result<PruneReport, Error> {
    let mut report = PruneReport::default();
    let now = nanos_now();

    for digest in cache.list()? {
        let object = cache.layout().object(digest);

        if cache.layout().pin_of(digest).exists() {
            report.kept += 1;
            clear_mark(cache, digest)?;
            continue;
        }

        if !cache.platform().owns(&object)? {
            report.skipped_other_owner += 1;
            continue;
        }

        let Some(held) = cache.platform().try_lock(&cache.layout().lock_of(digest))? else {
            report.kept += 1;
            clear_mark(cache, digest)?;
            continue;
        };

        let marked: Option<Mark> = record::read(&cache.layout().mark_of(digest))?;
        let Some(mark) = marked else {
            record::write(
                &cache.layout().mark_of(digest),
                &Mark { marked_nanos: now },
                cache.work(),
            )?;
            report.kept += 1;
            drop(held);
            continue;
        };

        if now.saturating_sub(mark.marked_nanos)
            < i128::try_from(grace.as_nanos()).unwrap_or(i128::MAX)
        {
            report.kept += 1;
            drop(held);
            continue;
        }

        let size = std::fs::metadata(&object).map_or(0, |found| found.len());
        remove(&object)?;
        remove(&cache.layout().outboard_of(digest))?;
        remove(&cache.object_record(digest))?;
        remove(&cache.layout().mark_of(digest))?;
        report.removed += 1;
        report.bytes_removed += size;

        drop(held);
        remove(&cache.layout().lock_of(digest))?;
        remove(&cache.layout().lock_owner_of(digest))?;
    }

    sweep_quarantine(cache, &mut report)?;
    Ok(report)
}

/// Removes the quarantined objects this user created.
///
/// A quarantined object is never served, so nothing can claim one between a
/// mark and a sweep, which is the only thing the grace period protects. It is
/// removed under its lock so a repair reading one is never removed underneath.
fn sweep_quarantine<P: Platform>(cache: &Cache<P>, report: &mut PruneReport) -> Result<(), Error> {
    for digest in cache.quarantined()? {
        let path = cache.layout().quarantined(digest);
        if !cache.platform().owns(&path)? {
            report.skipped_other_owner += 1;
            continue;
        }
        let Some(held) = cache.platform().try_lock(&cache.layout().lock_of(digest))? else {
            report.kept += 1;
            continue;
        };
        let size = std::fs::metadata(&path).map_or(0, |found| found.len());
        remove(&path)?;
        report.quarantined_removed += 1;
        report.bytes_removed += size;
        drop(held);
    }
    Ok(())
}

/// Removes a mark, because the object it named is referenced again.
fn clear_mark<P: Platform>(cache: &Cache<P>, digest: ContentDigest) -> Result<(), Error> {
    remove(&cache.layout().mark_of(digest))
}

fn remove(path: &std::path::Path) -> Result<(), Error> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(reason) if reason.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(reason) => Err(failure(ErrorKind::CacheCorrupt, path, &reason)),
    }
}

/// Returns this instant in nanoseconds since the epoch.
fn nanos_now() -> i128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i128::try_from(elapsed.as_nanos()).unwrap_or(i128::MAX)
        })
}
