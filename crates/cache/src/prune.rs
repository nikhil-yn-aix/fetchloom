//! Mark, grace, sweep.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::store::PruneReport;
use fetchloom_engine::digest::ContentDigest;
use fetchloom_engine::error::{Error, Surface, filesystem_failure};
use fetchloom_engine::seam::platform::Platform;

use crate::Cache;
use crate::record::{self, Mark};
use crate::storage::Placement;

pub const GRACE: Duration = Duration::from_secs(60);

struct Removable<L> {
    digest: ContentDigest,
    size: u64,
    held: L,
}

/// # Errors
/// `cache.corrupt` when the store cannot be walked or an object cannot be
/// removed. An object another user owns is counted as skipped, not an error.
pub fn run<P: Platform>(cache: &Cache<P>, grace: Duration) -> Result<PruneReport, Error> {
    let mut report = PruneReport::default();
    let now = nanos_now();
    let mut removable = Vec::new();

    for digest in cache.list()? {
        if cache.layout().pin_of(digest).exists() {
            report.kept += 1;
            clear_mark(cache, digest)?;
            continue;
        }

        if !cache.owns_object(digest)? {
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

        let size = cache.size_of(digest).unwrap_or_default();
        removable.push(Removable { digest, size, held });
    }

    remove_all(cache, removable, &mut report)?;

    sweep_quarantine(cache, &mut report)?;
    Ok(report)
}

fn remove_all<P: Platform>(
    cache: &Cache<P>,
    removable: Vec<Removable<P::Lock>>,
    report: &mut PruneReport,
) -> Result<(), Error> {
    let mut packed: BTreeMap<PathBuf, BTreeSet<ContentDigest>> = BTreeMap::new();
    for candidate in &removable {
        if let Some(Placement::Packed { pack, .. }) = cache.placement(candidate.digest) {
            packed.entry(pack).or_default().insert(candidate.digest);
        }
    }
    for (pack, digests) in &packed {
        cache.rewrite_pack(pack, digests)?;
    }

    for candidate in removable {
        let Removable { digest, size, held } = candidate;
        if !packed.values().any(|digests| digests.contains(&digest)) {
            cache.remove_object(digest)?;
        }
        remove(&cache.layout().outboard_of(digest))?;
        remove(&cache.object_record(digest))?;
        remove(&cache.layout().mark_of(digest))?;
        report.removed += 1;
        report.bytes_removed += size;

        drop(held);
        remove(&cache.layout().lock_of(digest))?;
        remove(&cache.layout().lock_owner_of(digest))?;
    }

    Ok(())
}

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

fn clear_mark<P: Platform>(cache: &Cache<P>, digest: ContentDigest) -> Result<(), Error> {
    remove(&cache.layout().mark_of(digest))
}

fn remove(path: &std::path::Path) -> Result<(), Error> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(reason) if reason.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(reason) => Err(filesystem_failure(Surface::Cache, path, &reason)),
    }
}

fn nanos_now() -> i128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i128::try_from(elapsed.as_nanos()).unwrap_or(i128::MAX)
        })
}
