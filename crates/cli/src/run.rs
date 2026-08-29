//! Executing the commands this build implements.

use std::path::{Path, PathBuf};

use fetchloom_cache::Cache;
use fetchloom_engine::canonical;

use fetchloom_engine::digest::{ContentDigest, TreeDigest};
use fetchloom_engine::durability::DurabilityTier;
use fetchloom_engine::error::{Error, ErrorKind};
use fetchloom_engine::event::{Event, EventPayload, Sequence};
use fetchloom_engine::pool::Processor;
use fetchloom_engine::redact::SafeUrl;
use fetchloom_engine::seam::observer::Observer;
use fetchloom_engine::seam::platform::Platform;
use fetchloom_engine::threads::ThreadBudget;
use fetchloom_engine::tree::TreeEntry;
use fetchloom_platform::NativePlatform;

use crate::materialize;

/// What a completed run reports.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct RunResult {
    /// What the run did.
    pub status: &'static str,
    /// The dataset the run materialized.
    pub dataset: String,
    /// The tree the run produced.
    pub tree: TreeDigest,
    /// Where the tree was materialized.
    pub destination: PathBuf,
    /// How many entries the tree holds.
    pub entries: u64,
    /// How many bytes those entries hold.
    pub bytes: u64,
}

/// Turns a reference into the local path it names.
///
/// Takes a reference. Returns the path a `file:` location or a plain local path
/// names. Fails when the reference names something this build cannot resolve,
/// which is every scheme that would need the network.
///
/// # Errors
///
/// Returns a resolution failure naming what could not be resolved.
pub fn local_path(reference: &str) -> Result<PathBuf, Error> {
    if let Some(rest) = reference.strip_prefix("file://") {
        let trimmed = rest.strip_prefix('/').unwrap_or(rest);
        let looks_like_windows_path = trimmed.as_bytes().get(1).is_some_and(|byte| *byte == b':');
        let path = if looks_like_windows_path {
            PathBuf::from(trimmed)
        } else {
            PathBuf::from(format!("/{trimmed}"))
        };
        return Ok(path);
    }
    if reference.contains("://") {
        return Err(Error::new(
            ErrorKind::ReferenceUnresolved,
            format!(
                "this build resolves only a local path or a file: location, not {}",
                SafeUrl::new(reference)
            ),
        ));
    }
    let path = PathBuf::from(reference);
    if path.exists() {
        Ok(path)
    } else {
        Err(Error::new(
            ErrorKind::ReferenceUnresolved,
            format!(
                "check that {} names a path that exists",
                SafeUrl::new(reference)
            ),
        ))
    }
}

/// Returns the default destination for a source path.
///
/// Takes the resolved source. Returns the current directory joined with the
/// source's own last component.
#[must_use]
pub fn default_destination(source: &Path) -> PathBuf {
    let name = source
        .file_name()
        .map_or_else(|| PathBuf::from("dataset"), PathBuf::from);
    PathBuf::from(".").join(name)
}

fn failure(kind: ErrorKind, path: &Path, reason: &std::io::Error) -> Error {
    Error::new(kind, format!("{}: {reason}", path.display()))
}

/// Everything a materialization runs against, so one call does not take a list
/// of loose arguments.
#[derive(Clone, Copy)]
pub struct Materialization<'a> {
    /// The pool the digests are computed on.
    pub processor: &'a Processor,
    /// The platform the filesystem work goes through.
    pub platform: &'a NativePlatform,
    /// How far a write is pushed before publication.
    pub durability: DurabilityTier,
    /// The cache to read and write, when the run has one.
    pub cache: Option<&'a Cache<NativePlatform>>,
}

/// Materializes a local source tree into a destination.
///
/// Takes the processor pool, the source, the destination, and where to emit
/// events. Copies every entry into a staging directory beside the destination,
/// digests each file as it is copied, computes the tree digest, and only then
/// moves staging onto the destination, so a partially materialized destination
/// is never visible. Returns what the run produced.
///
/// # Errors
///
/// Fails when the source cannot be read, when an entry cannot be represented on
/// this platform, when the destination already exists, and when staging cannot
/// be published.
pub fn materialize_local(
    with: &Materialization<'_>,
    source: &Path,
    destination: &Path,
    observer: &dyn Observer,
    sequence: &Sequence,
) -> Result<RunResult, Error> {
    let Materialization {
        processor,
        platform,
        durability,
        cache,
    } = *with;
    let emit = |payload: EventPayload| observer.emit(&Event::new(sequence, payload));

    emit(EventPayload::ResolveStart);
    let walked = materialize::walk(source)?;
    emit(EventPayload::ResolveEnd { duration_ms: 0 });
    emit(EventPayload::PlanReady);

    if destination.exists() {
        return Err(Error::new(
            ErrorKind::DestinationForeign,
            format!(
                "remove {} or choose another destination with --output",
                destination.display()
            ),
        ));
    }

    let staging = staging_beside(destination);
    if staging.exists() {
        std::fs::remove_dir_all(&staging)
            .map_err(|reason| failure(ErrorKind::DestinationForeign, &staging, &reason))?;
    }
    std::fs::create_dir_all(&staging)
        .map_err(|reason| failure(ErrorKind::DestinationForeign, &staging, &reason))?;

    let outcome = fill_staging(processor, platform, cache, source, &staging, &walked, &emit);
    let mut entries = match outcome {
        Ok(entries) => entries,
        Err(error) => {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(error);
        }
    };
    entries.extend(walked.entries.iter().cloned());

    let tree = canonical::tree_digest(&entries);

    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|reason| failure(ErrorKind::DestinationForeign, parent, &reason))?;
    }
    if let Err(error) = platform.publish_directory(&staging, destination, durability) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(error);
    }
    for entry in platform.take_degradations() {
        emit(EventPayload::Degrade {
            requested: entry.requested,
            used: entry.used,
            reason: entry.reason,
        });
    }
    emit(EventPayload::PublishCommit);

    Ok(RunResult {
        status: "materialized",
        dataset: source.file_name().map_or_else(
            || "dataset".to_owned(),
            |name| name.to_string_lossy().into_owned(),
        ),
        tree,
        destination: destination.to_path_buf(),
        entries: entries.len() as u64,
        bytes: walked.bytes,
    })
}

fn fill_staging(
    processor: &Processor,
    platform: &NativePlatform,
    cache: Option<&Cache<NativePlatform>>,
    source: &Path,
    staging: &Path,
    walked: &materialize::Walked,
    emit: &dyn Fn(EventPayload),
) -> Result<Vec<TreeEntry>, Error> {
    emit(EventPayload::TransferStart {
        source: fetchloom_engine::redact::SafeUrl::new(&source.to_string_lossy()),
        expected_bytes: Some(walked.bytes),
    });

    for entry in &walked.entries {
        if let TreeEntry::Directory { path } = entry {
            let target = staging.join(path.as_str());
            std::fs::create_dir_all(&target)
                .map_err(|reason| failure(ErrorKind::DestinationForeign, &target, &reason))?;
        }
    }

    let mut entries = Vec::with_capacity(walked.files.len());
    let mut copied = 0u64;
    for file in &walked.files {
        let from = source.join(&file.relative);
        let to = staging.join(file.entry.as_str());
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|reason| failure(ErrorKind::DestinationForeign, parent, &reason))?;
        }
        let (size, content) = match cache {
            None => materialize::copy_file(processor, &from, &to)?,
            Some(held) => through_cache(held, platform, &from, &to, emit)?,
        };
        copied += size;
        emit(EventPayload::TransferProgress { bytes: copied });
        entries.push(TreeEntry::File {
            path: file.entry.clone(),
            mode: file.mode,
            size,
            content,
        });
    }

    emit(EventPayload::TransferEnd {
        bytes: copied,
        duration_ms: 0,
    });
    Ok(entries)
}

/// Puts one file through the cache and materializes it from there.
///
/// Reads the source once, hashing it as it is written into the cache, then
/// places the object at its destination by sharing blocks where the volume can.
/// A source the cache already holds is a hit, and nothing is written into the
/// cache for it.
fn through_cache(
    cache: &Cache<NativePlatform>,
    platform: &NativePlatform,
    from: &Path,
    to: &Path,
    emit: &dyn Fn(EventPayload),
) -> Result<(u64, ContentDigest), Error> {
    let ingested = cache.ingest(from)?;
    if ingested.was_present {
        emit(EventPayload::CacheHit {
            digest: ingested.digest,
        });
    } else {
        emit(EventPayload::CacheMiss {
            digest: ingested.digest,
        });
    }

    let lease = cache.read_lease(ingested.digest)?;
    let object = cache.layout().object(ingested.digest);
    platform.clone_or_copy(&object, to)?;
    drop(lease);
    Ok((ingested.size, ingested.digest))
}

fn staging_beside(destination: &Path) -> PathBuf {
    let name = destination.file_name().map_or_else(
        || "dataset".to_owned(),
        |name| name.to_string_lossy().into_owned(),
    );
    destination.with_file_name(format!(".{name}.fetchloom-staging"))
}

/// Recomputes the tree digest of a materialized directory.
///
/// Takes the directory to read. Returns its entries and their tree digest. This
/// build recomputes and reports; comparing against a receipt arrives with
/// receipts.
///
/// # Errors
///
/// Fails when the directory cannot be read and when an entry cannot be
/// represented on this platform.
pub fn verify_tree(path: &Path) -> Result<(TreeDigest, u64), Error> {
    if !path.exists() {
        return Err(Error::new(
            ErrorKind::ReferenceUnresolved,
            format!("check that {} names a path that exists", path.display()),
        ));
    }
    let walked = materialize::walk(path)?;
    let mut entries = walked.entries.clone();
    let processor = Processor::new(ThreadBudget::resolve(
        std::thread::available_parallelism().unwrap_or(std::num::NonZeroUsize::MIN),
        None,
    ))
    .map_err(|reason| {
        Error::new(
            ErrorKind::ResourceLimit,
            format!("the processor pool could not be built: {reason}"),
        )
    })?;

    for file in &walked.files {
        let full = path.join(&file.relative);
        let handle = std::fs::File::open(&full)
            .map_err(|reason| failure(ErrorKind::ReferenceUnresolved, &full, &reason))?;
        let size = handle
            .metadata()
            .map_err(|reason| failure(ErrorKind::ReferenceUnresolved, &full, &reason))?
            .len();
        let digests = fetchloom_engine::hashing::hash_stream(&processor, handle)
            .map_err(|reason| failure(ErrorKind::IntegrityMismatch, &full, &reason))?;
        entries.push(TreeEntry::File {
            path: file.entry.clone(),
            mode: file.mode,
            size,
            content: digests.content,
        });
    }

    Ok((canonical::tree_digest(&entries), entries.len() as u64))
}

/// Refuses a reference that would need the network while the network is
/// forbidden.
///
/// Takes the reference and whether the run is offline. Returns nothing when the
/// reference names something reachable without the network.
///
/// # Errors
///
/// Fails with a policy failure when the reference names a network location and
/// the run forbids network activity.
pub fn allowed_offline(reference: &str, offline: bool) -> Result<(), Error> {
    if !offline {
        return Ok(());
    }
    let network = reference.contains("://") && !reference.starts_with("file://");
    if network {
        return Err(Error::new(
            ErrorKind::PolicyOffline,
            format!(
                "run the command again without --offline to reach {}",
                SafeUrl::new(reference)
            ),
        ));
    }
    Ok(())
}
