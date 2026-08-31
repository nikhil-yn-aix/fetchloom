//! Rereading every object and quarantining the ones that no longer hash to the
//! name they are stored under.

use fetchloom_engine::error::Error;
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
    /// How many objects another writer held and were not read.
    pub held: u64,
}

/// Rereads and rehashes every object, quarantining each mismatch.
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
        let recorded = cache.recorded_source_of(digest);
        cache.quarantine(digest, recorded.0, recorded.1)?;
        report.quarantined.push(digest.to_string());
    }
    Ok(report)
}
