//! Rereading every object and quarantining the ones that no longer hash to the
//! name they are stored under.

use fetchloom_engine::error::{Error, ErrorKind};
use fetchloom_engine::seam::platform::Platform;
use fetchloom_engine::seam::store::Store;
use serde::Serialize;

use crate::Cache;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct VerifyReport {
    pub verified: u64,
    pub quarantined: Vec<String>,
    pub held: u64,
}

/// # Errors
/// `cache.corrupt` when the store cannot be walked or a mismatched object
/// cannot be quarantined. An object that does not hash to its name is
/// quarantined and counted, not an error, and so is one whose stored form no
/// longer decodes, because bytes that cannot be read are not the bytes the
/// digest names.
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
        let intact = match cache.object_is_its_digest(digest) {
            Ok(intact) => intact,
            Err(reason) if reason.kind() == ErrorKind::CacheCorrupt => false,
            Err(reason) => return Err(reason),
        };
        if intact {
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
