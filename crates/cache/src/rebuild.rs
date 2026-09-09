//! Rebuilding the derived data the cache can regenerate from what it holds.

use std::io::Read;

use fetchloom_engine::error::{Error, ErrorKind, Surface, filesystem_failure};
use fetchloom_engine::hashing;
use fetchloom_engine::limits::OUTBOARD_THRESHOLD;
use fetchloom_engine::seam::platform::{Liveness, Platform};
use serde::Serialize;

use crate::record::{self, ObjectRecord};
use crate::{Cache, owner_record_of, source_record_of};

use fetchloom_engine::limits::STREAM_BUFFER_BYTES as BUFFER;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct RebuildReport {
    pub trees_rebuilt: u64,
    pub records_rebuilt: u64,
    pub locks_released: u64,
    pub orphans_removed: u64,
    pub needing_a_source: Vec<String>,
    pub held: u64,
}

/// # Errors
/// `cache.corrupt` when the store cannot be walked or a record cannot be
/// rewritten. An object another process holds is counted as held, not an
/// error.
pub fn run<P: Platform>(cache: &Cache<P>) -> Result<RebuildReport, Error> {
    let mut report = RebuildReport::default();
    for digest in cache.list()? {
        let Some(held) = cache
            .platform()
            .try_lock_shared(&cache.layout().lock_of(digest))?
        else {
            report.held += 1;
            continue;
        };
        let wants_tree = !cache.has_outboard(digest)? || tree_does_not_check_out(cache, digest)?;
        let wants_record = !cache.is_packed(digest) && !cache.object_record(digest).is_file();
        if !wants_tree && !wants_record {
            drop(held);
            continue;
        }

        let digests = read_object(cache, digest)?;
        if digests.content != digest {
            drop(held);
            let recorded = cache.recorded_source_of(digest);
            cache.quarantine(digest, recorded.0, recorded.1)?;
            report.needing_a_source.push(digest.to_string());
            continue;
        }

        if wants_tree && digests.length > OUTBOARD_THRESHOLD {
            let tree = digests
                .outboard
                .as_ref()
                .ok_or_else(|| missing_tree(digest))?;
            cache.write_outboard(digest, tree.as_bytes())?;
            report.trees_rebuilt += 1;
        }
        if wants_record {
            write_record(cache, digest, digests.interop)?;
            report.records_rebuilt += 1;
        }
        drop(held);
    }

    release_dead_locks(cache, &mut report)?;
    remove_orphans(cache, &mut report)?;
    Ok(report)
}

fn missing_tree(digest: fetchloom_engine::digest::ContentDigest) -> Error {
    Error::new(
        ErrorKind::CacheCorrupt,
        format!(
            "run cache clear, because {digest} is above the outboard threshold and one pass over it produced no tree"
        ),
    )
}

fn tree_does_not_check_out<P: Platform>(
    cache: &Cache<P>,
    digest: fetchloom_engine::digest::ContentDigest,
) -> Result<bool, Error> {
    let found = cache.localize(digest)?;
    Ok(found.not_localized == Some(crate::diagnosis::NotLocalized::TreeDoesNotCheckOut))
}

fn read_object<P: Platform>(
    cache: &Cache<P>,
    digest: fetchloom_engine::digest::ContentDigest,
) -> Result<hashing::Digests, Error> {
    let mut file = cache.read(digest)?;
    let mut pair = hashing::Pair::new();
    let mut buffer = vec![0u8; BUFFER];
    loop {
        let filled = file.read(&mut buffer).map_err(|reason| {
            filesystem_failure(Surface::Cache, &cache.layout().objects(), &reason)
        })?;
        if filled == 0 {
            break;
        }
        pair.update(cache.processor(), &buffer[..filled]);
        cache.work().read_bytes(filled as u64);
    }
    Ok(pair.finish())
}

fn write_record<P: Platform>(
    cache: &Cache<P>,
    digest: fetchloom_engine::digest::ContentDigest,
    interop: fetchloom_engine::digest::InteropDigest,
) -> Result<(), Error> {
    let fingerprint = cache.fingerprint_of(digest)?;
    let path = cache.object_record(digest);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|reason| filesystem_failure(Surface::Cache, parent, &reason))?;
    }
    record::write(
        &path,
        &ObjectRecord::new(fingerprint, interop),
        cache.work(),
    )
}

fn release_dead_locks<P: Platform>(
    cache: &Cache<P>,
    report: &mut RebuildReport,
) -> Result<(), Error> {
    let entries = std::fs::read_dir(cache.layout().locks())
        .map_err(|reason| filesystem_failure(Surface::Cache, &cache.layout().locks(), &reason))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|kind| kind != "owner") {
            continue;
        }
        let Some(holder) = record::read_owner(&path)? else {
            continue;
        };
        if cache.platform().liveness(&holder) != Liveness::Stale {
            continue;
        }
        let lock = path.with_extension("lock");
        if cache.platform().try_lock(&lock)?.is_none() {
            continue;
        }
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&lock);
        report.locks_released += 1;
    }
    Ok(())
}

fn remove_orphans<P: Platform>(cache: &Cache<P>, report: &mut RebuildReport) -> Result<(), Error> {
    for directory in [cache.layout().partial(), cache.layout().staging()] {
        let entries = std::fs::read_dir(&directory)
            .map_err(|reason| filesystem_failure(Surface::Cache, &directory, &reason))?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path
                .extension()
                .is_some_and(|kind| kind == "owner" || kind == "source")
            {
                continue;
            }
            if !orphaned(cache, &path)? {
                continue;
            }
            let removed = if path.is_dir() {
                std::fs::remove_dir_all(&path)
            } else {
                std::fs::remove_file(&path)
            };
            match removed {
                Ok(()) => {}
                Err(reason) if reason.kind() == std::io::ErrorKind::NotFound => continue,
                Err(reason) => return Err(filesystem_failure(Surface::Cache, &path, &reason)),
            }
            let _ = std::fs::remove_file(owner_record_of(&path));
            let _ = std::fs::remove_file(source_record_of(&path));
            report.orphans_removed += 1;
        }
    }
    Ok(())
}

fn orphaned<P: Platform>(cache: &Cache<P>, path: &std::path::Path) -> Result<bool, Error> {
    if let Some(name) = path.file_name().and_then(|name| name.to_str())
        && let Some(digest) = crate::layout::digest_of(name)
        && cache.contains(digest)?
    {
        return Ok(true);
    }
    let Some(holder) = record::read_owner(&owner_record_of(path))? else {
        return Ok(false);
    };
    Ok(cache.platform().liveness(&holder) == Liveness::Stale)
}
