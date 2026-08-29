//! Rereading every object and quarantining the ones that no longer hash to the
//! name they are stored under.

use fetchloom_engine::digest::ContentDigest;
use fetchloom_engine::error::{Error, ErrorKind};
use fetchloom_engine::seam::platform::Platform;
use fetchloom_engine::seam::store::Store;
use serde::Serialize;

use crate::Cache;

/// What one verification run found.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct VerifyReport {
    /// How many objects hashed to the name they are stored under.
    pub verified: u64,
    /// The objects that did not, each now in quarantine.
    pub quarantined: Vec<String>,
    /// How many objects another writer held, so they were not read.
    pub held: u64,
}

/// Rereads and rehashes every object, quarantining each mismatch.
///
/// An object a writer holds is left alone and counted, because reading it while
/// it is being written would report a mismatch that is not one.
///
/// # Errors
///
/// Fails when the cache cannot be read or an object cannot be moved.
pub fn run<P: Platform>(cache: &Cache<P>) -> Result<VerifyReport, Error> {
    let mut report = VerifyReport::default();
    for digest in cache.list()? {
        let Some(held) = cache
            .platform()
            .try_lock_shared(&cache.layout().lock_of(digest))?
        else {
            report.held += 1;
            continue;
        };
        if cache.object_is_its_digest(digest)? {
            report.verified += 1;
            drop(held);
            continue;
        }
        drop(held);
        quarantine(cache, digest)?;
        report.quarantined.push(digest.to_string());
    }
    Ok(report)
}

/// Moves an object out of `objects/` and into `quarantine/`.
///
/// # Errors
///
/// Fails when the object cannot be moved, and when the two directories are on
/// different volumes.
pub fn quarantine<P: Platform>(cache: &Cache<P>, digest: ContentDigest) -> Result<(), Error> {
    let held = cache.platform().lock(&cache.layout().lock_of(digest))?;
    let from = cache.layout().object(digest);
    let to = cache.layout().quarantined(digest);
    let moved = cache
        .platform()
        .publish_file(&from, &to, cache.tier())
        .map_err(|reason| {
            Error::new(
                ErrorKind::CacheCorrupt,
                format!(
                    "remove {} by hand, because it failed verification and could not be quarantined: {}",
                    from.display(),
                    reason.next_action()
                ),
            )
        });
    let _ = std::fs::remove_file(cache.fingerprint_record(digest));
    drop(held);
    moved
}
