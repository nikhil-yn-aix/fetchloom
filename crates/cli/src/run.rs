//! Executing the commands this build implements.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fetchloom_cache::Cache;
use fetchloom_cache::ingest::Ingested;
use fetchloom_engine::canonical;

use fetchloom_engine::degrade::DegradeQueue;
use fetchloom_engine::digest::{ContentDigest, TreeDigest};
use fetchloom_engine::durability::DurabilityTier;
use fetchloom_engine::error::{Error, ErrorKind, Layer};
use fetchloom_engine::event::{Event, EventPayload, Sequence, Span};
use fetchloom_engine::hashing;
use fetchloom_engine::limits::Limits;
use fetchloom_engine::manifest::ArchiveFormat;
use fetchloom_engine::pool::Processor;
use fetchloom_engine::receipt::{Receipt, RecordedFingerprint};
use fetchloom_engine::reconcile::{ReconcileOutcome, Reconciled, reconcile};
use fetchloom_engine::redact::SafeUrl;
use fetchloom_engine::seam::observer::Observer;
use fetchloom_engine::seam::platform::Platform;
use fetchloom_engine::selection::{Candidate, Selection};
use fetchloom_engine::threads::ThreadBudget;
use fetchloom_engine::transfer::{SleepingPause, Transfer};
use fetchloom_engine::tree::{EntryPath, Mode, TreeEntry};
use fetchloom_engine::trust::{ArtifactKey, RunId, TrustClass, Witness, classify};
use fetchloom_engine::work::{Work, WorkCounter};
use fetchloom_platform::NativePlatform;
use fetchloom_sources::HttpSource;

use crate::materialize;

/// The one object a run resolved, when it resolved one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordedArtifact {
    /// The name the artifact is recorded under.
    pub id: String,
    /// The digest the bytes hash to.
    pub digest: ContentDigest,
    /// The interop digest of the same bytes, when the run learned it.
    pub interop: Option<fetchloom_engine::digest::InteropDigest>,
    /// The length of the object in bytes.
    pub size: u64,
    /// Where the bytes came from, redacted as it was recorded.
    pub source: SafeUrl,
    /// The digest supplied before the run, when one was.
    pub prior: Option<ContentDigest>,
    /// The origin that served the bytes, present only when this run moved them.
    pub observed: Option<String>,
}

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
    /// What the run read, wrote, and asked for.
    pub work: Work,
    /// What the run may claim about the bytes it produced.
    pub trust: TrustClass,
    /// The entry paths the run materialized with the executable mode, in
    /// ascending order. Never part of the machine-readable result.
    #[serde(skip)]
    pub executable: Vec<String>,
    /// The object the run resolved, when it resolved one.
    #[serde(skip)]
    pub artifact: Option<RecordedArtifact>,
}

/// Returns the entry paths that carry the executable mode, in ascending order.
#[must_use]
pub fn executable_paths(entries: &[TreeEntry]) -> Vec<String> {
    let mut paths: Vec<String> = entries
        .iter()
        .filter_map(|entry| match entry {
            TreeEntry::File {
                path,
                mode: Mode::Executable,
                ..
            } => Some(path.as_str().to_owned()),
            _ => None,
        })
        .collect();
    paths.sort();
    paths
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

/// Resolves a path the user named against the working directory.
///
/// Takes the path named by `--output` or `--cache-dir`. Returns it unchanged
/// when it is already absolute, and joined onto the working directory
/// otherwise. Never touches the filesystem.
///
/// # Errors
///
/// Fails when the working directory cannot be read.
pub fn resolve_path(path: &Path) -> Result<PathBuf, Error> {
    std::path::absolute(path).map_err(|reason| {
        Error::new(
            ErrorKind::DestinationUnrepresentable,
            format!("{}: {reason}", path.display()),
        )
    })
}

fn failure(kind: ErrorKind, path: &Path, reason: &std::io::Error) -> Error {
    Error::new(kind, format!("{}: {reason}", path.display()))
}

/// Returns the directory a path sits in, treating a single-component
/// relative path as sitting in the working directory.
///
/// `Path::parent` returns `Some("")` for a path with one relative component,
/// and an empty path passed to a filesystem call resolves to nothing rather
/// than to the working directory it stands for.
fn containing_directory(path: &Path) -> PathBuf {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

/// Everything a materialization runs against.
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
    /// Where the run counts the work it did.
    pub work: &'a Arc<WorkCounter>,
    /// Whether a recognized archive is extracted or kept as a file.
    pub extract: bool,
    /// The one buffer every stream this run hashes is read through.
    pub digester: &'a std::cell::RefCell<fetchloom_engine::hashing::Digester>,
    /// What a destination entry and a cache hit are both checked against before
    /// they are reused.
    pub verify: fetchloom_engine::verification::VerificationPolicy,
}

/// Materializes a local source tree into a destination.
///
/// Takes what the materialization runs against, the source, the destination,
/// the selection that decides which members land and under what path,
/// whether a modified or foreign entry may be overwritten or removed, and
/// whether the destination's own contents are accepted as correct instead.
/// When the destination does not exist, every selected entry is staged and
/// published as one atomic rename. When it does, each entry is reconciled
/// against the tree this run resolved: an entry that already matches is left
/// untouched, a missing entry is restored alone, and a modified or foreign
/// entry stops the run unless `force` or `adopt` says otherwise. Returns what
/// the run produced.
///
/// # Errors
///
/// Fails when the source cannot be read, when the selection matches nothing
/// or a layout leaves a member with no path or a collision, when a
/// destination entry is modified or foreign and neither `force` nor `adopt`
/// was given, and when staging cannot be published.
#[expect(
    clippy::too_many_arguments,
    reason = "the selection, force, and adopt flags each name a contract behavior of their own"
)]
pub fn materialize_local(
    with: &Materialization<'_>,
    source: &Path,
    destination: &Path,
    selection: &Selection,
    force: bool,
    adopt: bool,
    pinned: Option<ContentDigest>,
    observer: &dyn Observer,
    sequence: &Sequence,
) -> Result<RunResult, Error> {
    let emit = |payload: EventPayload| observer.emit(&Event::new(sequence, payload));

    let resolving = Span::start();
    emit(EventPayload::ResolveStart);

    let dataset = source.file_name().map_or_else(
        || "dataset".to_owned(),
        |name| name.to_string_lossy().into_owned(),
    );

    if let Some(ingested) = object_to_resolve(with, source)? {
        emit(EventPayload::ResolveEnd {
            duration_ms: resolving.elapsed_ms(),
        });
        emit(if ingested.was_present {
            EventPayload::CacheHit {
                digest: ingested.digest,
            }
        } else {
            EventPayload::CacheMiss {
                digest: ingested.digest,
            }
        });
        emit(EventPayload::PlanReady);
        return materialize_object(
            with,
            ingested.digest,
            ingested.size,
            &dataset,
            destination,
            selection,
            force,
            adopt,
            Some(ingested.interop),
            &SafeUrl::new(&source.to_string_lossy()),
            &Provenance {
                prior: pinned,
                observed: None,
            },
            &emit,
        );
    }

    let walked = materialize::walk(source)?;
    let walked = apply_selection(walked, selection)?;
    report_unread_modes(!walked.files.is_empty(), &emit);
    emit(EventPayload::ResolveEnd {
        duration_ms: resolving.elapsed_ms(),
    });
    emit(EventPayload::PlanReady);

    if destination.exists() {
        return reconcile_existing(with, destination, &walked, force, adopt, &dataset, &emit);
    }

    materialize_fresh(with, &walked, destination, &dataset, &emit)
}

fn materialize_fresh(
    with: &Materialization<'_>,
    walked: &materialize::Walked,
    destination: &Path,
    dataset: &str,
    emit: &dyn Fn(EventPayload),
) -> Result<RunResult, Error> {
    let Materialization {
        platform,
        durability,
        ..
    } = *with;

    let staging = staging_beside(destination);
    if staging.exists() {
        std::fs::remove_dir_all(&staging)
            .map_err(|reason| failure(ErrorKind::DestinationForeign, &staging, &reason))?;
    }
    with.platform.create_directories(&staging)?;

    let outcome = fill_staging(with, &staging, walked, emit);
    let mut entries = match outcome {
        Ok(entries) => entries,
        Err(error) => {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(error);
        }
    };
    entries.extend(walked.entries.iter().cloned());

    let tree = canonical::tree_digest(&entries);

    {
        let parent = containing_directory(destination);
        with.platform.create_directories(&parent)?;
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
        dataset: dataset.to_owned(),
        tree,
        destination: destination.to_path_buf(),
        entries: entries.len() as u64,
        bytes: walked.bytes,
        work: with.work.taken(),
        trust: provisional_trust(with, None),
        executable: executable_paths(&entries),
        artifact: None,
    })
}

fn entry_size(entry: &TreeEntry) -> u64 {
    match entry {
        TreeEntry::File { size, .. } | TreeEntry::Symlink { size, .. } => *size,
        TreeEntry::Directory { .. } => 0,
    }
}

/// Returns a destination's entries with every mode taken from the resolved
/// tree.
///
/// Takes the tree the run resolved and the entries a walk of the destination
/// found. An entry the resolved tree names carries the mode that tree states.
/// An entry the resolved tree does not name keeps the mode the walk gave it.
fn with_resolved_modes(resolved: &[TreeEntry], found: Vec<TreeEntry>) -> Vec<TreeEntry> {
    let modes: HashMap<&str, Mode> = resolved
        .iter()
        .filter_map(|entry| match entry {
            TreeEntry::File { path, mode, .. } => Some((path.as_str(), *mode)),
            TreeEntry::Directory { .. } | TreeEntry::Symlink { .. } => None,
        })
        .collect();
    found
        .into_iter()
        .map(|entry| match entry {
            TreeEntry::File {
                path,
                mode,
                size,
                content,
            } => {
                let mode = modes.get(path.as_str()).copied().unwrap_or(mode);
                TreeEntry::File {
                    path,
                    mode,
                    size,
                    content,
                }
            }
            other => other,
        })
        .collect()
}

/// Everything settling a run against an existing destination needs to know.
struct Settlement<'a> {
    /// The tree the run resolved, whatever the source was.
    resolved: &'a [TreeEntry],
    /// Whether the run may overwrite a modified entry and remove a foreign one.
    force: bool,
    /// Whether the run accepts the destination as it stands.
    adopt: bool,
    /// What the run calls the dataset.
    dataset: &'a str,
    /// The object the run resolved, when it resolved one.
    artifact: Option<RecordedArtifact>,
}

/// Decides one run against an existing destination.
///
/// Takes what the run resolved, how to rebuild the destination whole when
/// `--force` says to, and how to restore the entries reconcile found missing.
///
/// # Errors
///
/// Fails with `destination.modified` or `destination.foreign` naming every
/// path when neither `--force` nor `--adopt` was given, and with whatever
/// rebuilding or restoring fails with.
fn settle(
    with: &Materialization<'_>,
    destination: &Path,
    settlement: &Settlement<'_>,
    emit: &dyn Fn(EventPayload),
    rebuild: &dyn Fn() -> Result<RunResult, Error>,
    restore: &dyn Fn(&[Reconciled]) -> Result<(), Error>,
) -> Result<RunResult, Error> {
    let Settlement {
        resolved,
        force,
        adopt,
        dataset,
        ..
    } = *settlement;
    let artifact = settlement.artifact.clone();

    let found = destination_entries(with, destination, resolved)?;
    let destination_tree = with_resolved_modes(resolved, found);
    let outcomes = reconcile(resolved, &destination_tree);
    for found in &outcomes {
        emit(EventPayload::ReconcileOutcomeReached {
            path: found.path.as_str().to_owned(),
            outcome: found.outcome,
        });
    }

    if outcomes
        .iter()
        .all(|found| found.outcome == ReconcileOutcome::Unchanged)
    {
        return Ok(RunResult {
            status: "unchanged",
            dataset: dataset.to_owned(),
            tree: canonical::tree_digest(resolved),
            destination: destination.to_path_buf(),
            entries: resolved.len() as u64,
            bytes: resolved.iter().map(entry_size).sum(),
            work: with.work.taken(),
            trust: provisional_trust(with, artifact.as_ref()),
            executable: executable_paths(resolved),
            artifact,
        });
    }

    if adopt {
        return Ok(RunResult {
            status: "adopted",
            dataset: dataset.to_owned(),
            tree: canonical::tree_digest(&destination_tree),
            destination: destination.to_path_buf(),
            entries: destination_tree.len() as u64,
            bytes: destination_tree.iter().map(entry_size).sum(),
            work: with.work.taken(),
            trust: provisional_trust(with, artifact.as_ref()),
            executable: executable_paths(&destination_tree),
            artifact,
        });
    }

    let modified: Vec<&str> = outcomes
        .iter()
        .filter(|found| found.outcome == ReconcileOutcome::Modified)
        .map(|found| found.path.as_str())
        .collect();
    let foreign: Vec<&str> = outcomes
        .iter()
        .filter(|found| found.outcome == ReconcileOutcome::Foreign)
        .map(|found| found.path.as_str())
        .collect();

    if !force && (!modified.is_empty() || !foreign.is_empty()) {
        let kind = if modified.is_empty() {
            ErrorKind::DestinationForeign
        } else {
            ErrorKind::DestinationModified
        };
        let mut named: Vec<String> = modified
            .iter()
            .map(|path| format!("{path} (modified)"))
            .collect();
        named.extend(foreign.iter().map(|path| format!("{path} (foreign)")));
        return Err(Error::new(
            kind,
            format!(
                "run again with --force to overwrite the modified entries and remove the foreign ones, or --adopt to accept the destination as it stands: {}",
                named.join(", ")
            ),
        ));
    }

    if force {
        return rebuild();
    }

    restore(&outcomes)?;
    Ok(RunResult {
        status: "restored",
        dataset: dataset.to_owned(),
        tree: canonical::tree_digest(resolved),
        destination: destination.to_path_buf(),
        entries: resolved.len() as u64,
        bytes: resolved.iter().map(entry_size).sum(),
        work: with.work.taken(),
        trust: provisional_trust(with, artifact.as_ref()),
        executable: executable_paths(resolved),
        artifact,
    })
}

fn reconcile_existing(
    with: &Materialization<'_>,
    destination: &Path,
    walked: &materialize::Walked,
    force: bool,
    adopt: bool,
    dataset: &str,
    emit: &dyn Fn(EventPayload),
) -> Result<RunResult, Error> {
    let mut resolved: Vec<TreeEntry> = walked.entries.clone();
    resolved.extend(hash_files(
        with.digester,
        &walked.root,
        &walked.files,
        with.processor,
        with.work,
    )?);

    settle(
        with,
        destination,
        &Settlement {
            resolved: &resolved,
            force,
            adopt,
            dataset,
            artifact: None,
        },
        emit,
        &|| materialize_fresh(with, walked, destination, dataset, emit),
        &|outcomes| restore_missing(with, destination, &resolved, walked, outcomes, emit),
    )
}

fn temp_beside(to: &Path) -> PathBuf {
    let name = to.file_name().map_or_else(
        || "entry".to_owned(),
        |name| name.to_string_lossy().into_owned(),
    );
    to.with_file_name(format!(".{name}.fetchloom-restore"))
}

fn restore_missing(
    with: &Materialization<'_>,
    destination: &Path,
    resolved: &[TreeEntry],
    walked: &materialize::Walked,
    outcomes: &[Reconciled],
    emit: &dyn Fn(EventPayload),
) -> Result<(), Error> {
    let missing: HashSet<&str> = outcomes
        .iter()
        .filter(|found| found.outcome == ReconcileOutcome::Restored)
        .map(|found| found.path.as_str())
        .collect();

    let mut directories: Vec<&EntryPath> = resolved
        .iter()
        .filter_map(|entry| match entry {
            TreeEntry::Directory { path } if missing.contains(path.as_str()) => Some(path),
            _ => None,
        })
        .collect();
    directories.sort_by_key(|path| path.as_str().matches('/').count());
    for path in directories {
        let target = destination.join(path.as_str());
        with.platform.create_directories(&target)?;
    }

    let gave_up = std::cell::Cell::new(false);
    for file in &walked.files {
        if !missing.contains(file.entry.as_str()) {
            continue;
        }
        let to = destination.join(file.entry.as_str());
        {
            let parent = containing_directory(&to);
            with.platform.create_directories(&parent)?;
        }
        let from = walked.root.join(&file.relative);
        let temp = temp_beside(&to);
        if let Err(error) = place_file(with, &gave_up, &from, &temp, emit) {
            let _ = std::fs::remove_file(&temp);
            return Err(error);
        }
        with.platform.publish_file(&temp, &to, with.durability)?;
    }

    for entry in with.platform.take_degradations() {
        emit(EventPayload::Degrade {
            requested: entry.requested,
            used: entry.used,
            reason: entry.reason,
        });
    }
    emit(EventPayload::PublishCommit);
    Ok(())
}

fn fill_staging(
    with: &Materialization<'_>,
    staging: &Path,
    walked: &materialize::Walked,
    emit: &dyn Fn(EventPayload),
) -> Result<Vec<TreeEntry>, Error> {
    let source = walked.root.as_path();
    let moving = Span::start();
    emit(EventPayload::TransferStart {
        source: fetchloom_engine::redact::SafeUrl::new(&source.to_string_lossy()),
        expected_bytes: Some(walked.bytes),
    });

    for entry in &walked.entries {
        if let TreeEntry::Directory { path } = entry {
            let target = staging.join(path.as_str());
            with.platform.create_directories(&target)?;
        }
    }

    let mut entries = Vec::with_capacity(walked.files.len());
    let mut copied = 0u64;
    let gave_up = std::cell::Cell::new(false);
    for file in &walked.files {
        let from = source.join(&file.relative);
        let to = staging.join(file.entry.as_str());
        {
            let parent = containing_directory(&to);
            with.platform.create_directories(&parent)?;
        }
        let (size, digests) = place_file(with, &gave_up, &from, &to, emit)?;
        copied += size;
        emit(EventPayload::TransferProgress { bytes: copied });
        entries.push(TreeEntry::File {
            path: file.entry.clone(),
            mode: file.mode,
            size,
            content: digests.content,
        });
    }

    for link in &walked.links {
        let at = staging.join(link.entry.as_str());
        with.platform
            .create_symlink(&link.target, &at)
            .map_err(|reason| {
                Error::new(
                    ErrorKind::DestinationUnrepresentable,
                    format!(
                        "materialize {} somewhere this platform creates symbolic links, because {}",
                        link.entry.as_str(),
                        reason.next_action()
                    ),
                )
            })?;
    }

    emit(EventPayload::TransferEnd {
        bytes: copied,
        duration_ms: moving.elapsed_ms(),
    });
    Ok(entries)
}

/// Places one file at its target, through the cache when one is open.
///
/// Takes whether an earlier file in this run already gave up on the cache, in
/// which case this one goes straight to a byte copy. Returns the length and
/// content digest that were written.
///
/// # Errors
///
/// Fails when the source cannot be read or the target cannot be written.
fn place_file(
    with: &Materialization<'_>,
    gave_up: &std::cell::Cell<bool>,
    from: &Path,
    to: &Path,
    emit: &dyn Fn(EventPayload),
) -> Result<(u64, hashing::Digests), Error> {
    let Materialization {
        processor, cache, ..
    } = *with;
    let usable = cache.filter(|_| !gave_up.get());
    match usable {
        None => materialize::copy_file(
            &mut with.digester.borrow_mut(),
            processor,
            with.work,
            from,
            to,
        ),
        Some(held) => match through_cache(
            &mut with.digester.borrow_mut(),
            held,
            processor,
            with.work,
            from,
            to,
            emit,
        ) {
            Ok(measured) => Ok(measured),
            Err(refused)
                if refused.layer() == Layer::Cache || refused.layer() == Layer::Resource =>
            {
                gave_up.set(true);
                emit(EventPayload::Degrade {
                    requested: "keeping this object in the cache".to_owned(),
                    used: "no cache for the rest of this run, so nothing more is retained"
                        .to_owned(),
                    reason: refused.next_action().to_owned(),
                });
                let _ = std::fs::remove_file(to);
                materialize::copy_file(
                    &mut with.digester.borrow_mut(),
                    processor,
                    with.work,
                    from,
                    to,
                )
            }
            Err(refused) => Err(refused),
        },
    }
}

/// Materializes one file and offers it to the cache.
///
/// Reads the source once, hashing it as it is written to its destination, and
/// then hands the cache that file. A source the cache already holds is neither
/// read again nor written.
fn through_cache(
    digester: &mut hashing::Digester,
    cache: &Cache<NativePlatform>,
    processor: &Processor,
    work: &WorkCounter,
    from: &Path,
    to: &Path,
    emit: &dyn Fn(EventPayload),
) -> Result<(u64, hashing::Digests), Error> {
    let (size, digests) = materialize::copy_file(digester, processor, work, from, to)?;
    let digest = digests.content;
    let adopted = cache.adopt(&digests, size, to)?;
    if adopted.was_present {
        emit(EventPayload::CacheHit { digest });
    } else {
        emit(EventPayload::CacheMiss { digest });
    }
    Ok((size, digests))
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
/// Takes the directory to read and the receipt that describes it, when one
/// describes it. A receipt supplies the mode of each file and never a digest.
/// Returns the entries and their tree digest.
///
/// # Errors
///
/// Fails when the directory cannot be read and when an entry cannot be
/// represented on this platform.
pub fn verify_tree(
    path: &Path,
    receipt: Option<&Receipt>,
    emit: &dyn Fn(EventPayload),
) -> Result<(TreeDigest, u64), Error> {
    if !path.exists() {
        return Err(Error::new(
            ErrorKind::ReferenceUnresolved,
            format!("check that {} names a path that exists", path.display()),
        ));
    }
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
    let work = WorkCounter::new();
    let digester = std::cell::RefCell::new(hashing::Digester::new());
    let walked = materialize::walk(path)?;
    let mut entries = walked.entries.clone();
    entries.extend(hash_files(
        &digester,
        &walked.root,
        &walked.files,
        &processor,
        &work,
    )?);
    let Some(receipt) = receipt else {
        report_unread_modes(
            entries
                .iter()
                .any(|entry| matches!(entry, TreeEntry::File { .. })),
            emit,
        );
        return Ok((canonical::tree_digest(&entries), entries.len() as u64));
    };
    let entries = with_receipt_modes(receipt, entries);
    Ok((canonical::tree_digest(&entries), entries.len() as u64))
}

/// Returns a walk's entries with every mode taken from the receipt.
///
/// An entry the receipt does not list as executable carries the read and write
/// mode, which is the only other mode a tree digest records.
fn with_receipt_modes(receipt: &Receipt, found: Vec<TreeEntry>) -> Vec<TreeEntry> {
    found
        .into_iter()
        .map(|entry| match entry {
            TreeEntry::File {
                path,
                mode: _,
                size,
                content,
            } => {
                let mode = receipt.mode_of(path.as_str());
                TreeEntry::File {
                    path,
                    mode,
                    size,
                    content,
                }
            }
            other => other,
        })
        .collect()
}

/// Reports that a tree's modes were not read from what was walked.
///
/// Takes whether the walk found any file at all and where to emit. Nothing is
/// emitted for a tree holding no file.
fn report_unread_modes(found_a_file: bool, emit: &dyn Fn(EventPayload)) {
    if !found_a_file {
        return;
    }
    emit(EventPayload::Degrade {
        requested: "the mode each file carries".to_owned(),
        used: format!("{:04o} for every file", u32::from(materialize::WALKED_MODE)),
        reason:
            "a filesystem tree states no mode, and reading one back from a volume that carries an \
             executable bit would digest the same tree differently than a volume that does not"
                .to_owned(),
    });
}

/// Walks a directory and hashes every file it holds, without writing
/// anything anywhere.
///
/// Takes the root the files are relative to, the files a walk of it found,
/// the processor pool the hashing runs on, and where the bytes read are
/// counted. Returns one `TreeEntry::File` per file, carrying the digest of
/// its bytes.
///
/// # Errors
///
/// Fails when a file cannot be opened or read.
fn hash_files(
    digester: &std::cell::RefCell<hashing::Digester>,
    root: &Path,
    files: &[materialize::SourceFile],
    processor: &Processor,
    work: &WorkCounter,
) -> Result<Vec<TreeEntry>, Error> {
    let mut entries = Vec::with_capacity(files.len());
    for file in files {
        let full = root.join(&file.relative);
        let handle = std::fs::File::open(&full)
            .map_err(|reason| failure(ErrorKind::ReferenceUnresolved, &full, &reason))?;
        let size = handle
            .metadata()
            .map_err(|reason| failure(ErrorKind::ReferenceUnresolved, &full, &reason))?
            .len();
        let counted = CountedRead {
            inner: handle,
            work,
        };
        let digests = digester
            .borrow_mut()
            .hash(processor, counted)
            .map_err(|reason| failure(ErrorKind::IntegrityMismatch, &full, &reason))?;
        entries.push(TreeEntry::File {
            path: file.entry.clone(),
            mode: file.mode,
            size,
            content: digests.content,
        });
    }
    Ok(entries)
}

/// A reader that counts every byte it yields as read work, and writes
/// nothing anywhere.
struct CountedRead<'a, R> {
    inner: R,
    work: &'a WorkCounter,
}

impl<R: Read> Read for CountedRead<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let read = self.inner.read(buffer)?;
        if read > 0 {
            self.work.read_bytes(read as u64);
        }
        Ok(read)
    }
}

/// Walks a destination directory and returns the entries it currently holds.
///
/// Takes the destination, the processor pool, and where bytes read are
/// counted. Returns every directory, symlink, and hashed file found.
///
/// # Errors
///
/// Fails when the destination cannot be read or a file cannot be hashed.
fn destination_entries(
    with: &Materialization<'_>,
    destination: &Path,
    resolved: &[TreeEntry],
) -> Result<Vec<TreeEntry>, Error> {
    let walked = materialize::walk(destination)?;
    let mut entries = walked.entries.clone();
    let recorded = recorded_fingerprints(with, destination, resolved);
    let mut to_hash = Vec::with_capacity(walked.files.len());
    for file in &walked.files {
        match unchanged_by_fingerprint(with, &walked.root, file, &recorded, resolved) {
            Some(entry) => entries.push(entry),
            None => to_hash.push(file.clone()),
        }
    }
    entries.extend(hash_files(
        with.digester,
        &walked.root,
        &to_hash,
        with.processor,
        with.work,
    )?);
    Ok(entries)
}

/// Returns the fingerprints the run that wrote this destination recorded, when
/// they are a cached answer to the question this run is asking.
///
/// A recorded fingerprint says that a file still holds what the run that wrote
/// the receipt published, so it answers only for the tree that receipt
/// describes. A destination with no receipt, and one whose receipt describes a
/// different tree, has none, and every file is hashed.
fn recorded_fingerprints(
    with: &Materialization<'_>,
    destination: &Path,
    resolved: &[TreeEntry],
) -> std::collections::BTreeMap<String, RecordedFingerprint> {
    let wanted = canonical::tree_digest(resolved);
    with.cache
        .and_then(|cache| cache.read_receipt(destination).ok().flatten())
        .filter(|receipt| receipt.tree == Some(wanted))
        .map(|receipt| receipt.fingerprints)
        .unwrap_or_default()
}

/// Returns the entry a file can be reported as without reading its bytes.
///
/// Answers only when the policy allows a fingerprint to decide, when one was
/// recorded for the path, when it still matches, and when the resolved tree
/// names that path. Every other case reads the bytes.
fn unchanged_by_fingerprint(
    with: &Materialization<'_>,
    root: &Path,
    file: &materialize::SourceFile,
    recorded: &std::collections::BTreeMap<String, RecordedFingerprint>,
    resolved: &[TreeEntry],
) -> Option<TreeEntry> {
    use fetchloom_engine::verification::VerificationPolicy;

    if with.verify == VerificationPolicy::Always {
        return None;
    }
    let held = recorded.get(file.entry.as_str())?;
    let now = with.platform.fingerprint(&root.join(&file.relative)).ok()?;
    if !held.matches(now) {
        return None;
    }
    let entry = resolved
        .iter()
        .find(|entry| entry.path() == &file.entry)?
        .clone();
    match entry {
        TreeEntry::File {
            path,
            size,
            content,
            ..
        } => Some(TreeEntry::File {
            path,
            mode: file.mode,
            size,
            content,
        }),
        _ => None,
    }
}

enum SelectionCandidate {
    Directory,
    Symlink {
        size: u64,
        content: ContentDigest,
        target: Vec<u8>,
    },
    File(materialize::SourceFile),
}

/// Applies a selection to what a walk of a source found.
///
/// Takes the walk and the selection. Returns a walk holding only the
/// selected members, renamed under the selection's layout, with every
/// ancestor directory a selected member needs.
///
/// # Errors
///
/// Fails with `reference.unresolved` when the selection matches nothing, and
/// with `destination.unrepresentable` when the layout leaves a member with
/// no path, and with `archive.collision` when two members land on the same
/// path.
/// Returns every member of a walked tree and what each one is, in the order a
/// selection is applied to them.
fn offer(walked: &materialize::Walked) -> (Vec<String>, Vec<SelectionCandidate>) {
    let mut members: Vec<String> = Vec::new();
    let mut candidates: Vec<SelectionCandidate> = Vec::new();
    for entry in &walked.entries {
        match entry {
            TreeEntry::Directory { path } => {
                members.push(path.as_str().to_owned());
                candidates.push(SelectionCandidate::Directory);
            }
            TreeEntry::Symlink {
                path,
                size,
                content,
            } => {
                members.push(path.as_str().to_owned());
                let target = walked
                    .links
                    .iter()
                    .find(|link| link.entry == *path)
                    .map(|link| link.target.clone())
                    .unwrap_or_default();
                candidates.push(SelectionCandidate::Symlink {
                    size: *size,
                    content: *content,
                    target,
                });
            }
            TreeEntry::File { .. } => {}
        }
    }
    for file in &walked.files {
        members.push(file.entry.as_str().to_owned());
        candidates.push(SelectionCandidate::File(file.clone()));
    }
    (members, candidates)
}

fn apply_selection(
    walked: materialize::Walked,
    selection: &Selection,
) -> Result<materialize::Walked, Error> {
    let (members, candidates) = offer(&walked);

    let offered: Vec<Candidate<'_>> = members
        .iter()
        .zip(&candidates)
        .map(|(path, candidate)| Candidate {
            path,
            directory: matches!(candidate, SelectionCandidate::Directory),
        })
        .collect();
    let applied = selection.apply(&offered)?;

    let mut new_entries: Vec<TreeEntry> = Vec::new();
    let mut new_files: Vec<materialize::SourceFile> = Vec::new();
    let mut new_links: Vec<materialize::SourceLink> = Vec::new();
    let mut claimed: HashMap<String, String> = HashMap::new();

    for member in &applied.members {
        let original = &members[member.index];
        if let Some(earlier) = claimed.insert(member.path.as_str().to_owned(), original.clone()) {
            return Err(Error::new(
                ErrorKind::ArchiveCollision,
                format!(
                    "rename {earlier} or {original} so --layout does not flatten both onto {}",
                    member.path
                ),
            ));
        }
        match &candidates[member.index] {
            SelectionCandidate::Directory => new_entries.push(TreeEntry::Directory {
                path: member.path.clone(),
            }),
            SelectionCandidate::Symlink {
                size,
                content,
                target,
            } => {
                new_entries.push(TreeEntry::Symlink {
                    path: member.path.clone(),
                    size: *size,
                    content: *content,
                });
                new_links.push(materialize::SourceLink {
                    entry: member.path.clone(),
                    target: target.clone(),
                });
            }
            SelectionCandidate::File(file) => {
                let mut rewritten = file.clone();
                rewritten.entry = member.path.clone();
                new_files.push(rewritten);
            }
        }
    }
    for directory in &applied.directories {
        new_entries.push(TreeEntry::Directory {
            path: directory.clone(),
        });
    }

    let mut bytes = 0u64;
    for file in &new_files {
        let full = walked.root.join(&file.relative);
        let metadata = std::fs::symlink_metadata(&full)
            .map_err(|reason| failure(ErrorKind::ReferenceUnresolved, &full, &reason))?;
        bytes += metadata.len();
    }

    Ok(materialize::Walked {
        entries: new_entries,
        files: new_files,
        links: new_links,
        bytes,
        root: walked.root,
    })
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

/// Materializes one object named by an HTTP or HTTPS reference.
///
/// Takes what the materialization runs against, the location, and the
/// destination. Streams the object into the cache, hashing it as it arrives,
/// and publishes a destination holding that one entry.
///
/// # Errors
///
/// Fails when the source is unreachable or refuses the request, when the cache
/// cannot be written, and when the destination already exists.
#[expect(
    clippy::too_many_arguments,
    reason = "the selection, force, and adopt flags each name a contract behavior of their own"
)]
pub fn materialize_remote(
    with: &Materialization<'_>,
    location: &str,
    destination: &Path,
    selection: &Selection,
    force: bool,
    adopt: bool,
    pinned: Option<ContentDigest>,
    observer: &dyn Observer,
    sequence: &Sequence,
) -> Result<RunResult, Error> {
    let emit = |payload: EventPayload| observer.emit(&Event::new(sequence, payload));
    let name = object_name(location);

    let resolving = Span::start();
    emit(EventPayload::ResolveStart);
    let source = HttpSource::new(Limits::default(), Arc::clone(with.work));
    let pause = SleepingPause;
    let limits = Limits::default();
    emit(EventPayload::ResolveEnd {
        duration_ms: resolving.elapsed_ms(),
    });
    emit(EventPayload::PlanReady);

    let Some(cache) = with.cache else {
        return Err(Error::new(
            ErrorKind::CacheCorrupt,
            "run without --no-cache, because this build streams a remote object through the cache",
        ));
    };

    let degradations = DegradeQueue::new();
    let transfer = Transfer {
        store: cache,
        source: &source,
        pause: &pause,
        limits: &limits,
        degradations: &degradations,
        observer,
        sequence,
    };
    let transferred = transfer.run(pinned, &prior_from(cache), &[location.to_owned()])?;
    remember(cache, location, &transferred)?;
    for entry in source.take_degradations() {
        emit(EventPayload::Degrade {
            requested: entry.requested,
            used: entry.used,
            reason: entry.reason,
        });
    }
    for entry in degradations.take() {
        emit(EventPayload::Degrade {
            requested: entry.requested,
            used: entry.used,
            reason: entry.reason,
        });
    }

    let interop = match transferred.interop {
        Some(interop) => Some(interop),
        None => cache.recorded_interop(transferred.digest)?,
    };
    let size = if transferred.bytes_kept + transferred.bytes_transferred > 0 {
        transferred.bytes_kept + transferred.bytes_transferred
    } else {
        std::fs::metadata(cache.layout().object(transferred.digest))
            .map(|found| found.len())
            .unwrap_or_default()
    };
    materialize_object(
        with,
        transferred.digest,
        size,
        &name,
        destination,
        selection,
        force,
        adopt,
        interop,
        &SafeUrl::new(location),
        &Provenance {
            prior: pinned,
            observed: observation(&transferred, std::slice::from_ref(&location.to_owned())),
        },
        &emit,
    )
}

/// Materializes one cached object into a destination, reconciling when the
/// destination already exists.
///
/// Takes what the materialization runs against, the object the run resolved,
/// the name it materializes under, and the flags reconcile answers to. An
/// object that is a recognized archive resolves to the tree it holds; one that
/// is not resolves to a destination holding that single file. Either way an
/// existing destination is reconciled against that tree.
///
/// # Errors
///
/// Fails when the object cannot be read, when the destination is modified or
/// foreign and neither `--force` nor `--adopt` was given, and when staging
/// cannot be published.
#[expect(
    clippy::too_many_arguments,
    reason = "the selection, force, and adopt flags each name a contract behavior of their own"
)]
fn materialize_object(
    with: &Materialization<'_>,
    digest: ContentDigest,
    size: u64,
    dataset: &str,
    destination: &Path,
    selection: &Selection,
    force: bool,
    adopt: bool,
    interop: Option<fetchloom_engine::digest::InteropDigest>,
    source: &SafeUrl,
    provenance: &Provenance,
    emit: &dyn Fn(EventPayload),
) -> Result<RunResult, Error> {
    if provenance.observed.is_none()
        && let Some(cache) = with.cache
        && cache.locate(digest).is_some()
    {
        cache.check_hit(digest)?;
    }
    let recorded = RecordedArtifact {
        id: dataset.to_owned(),
        digest,
        interop,
        size,
        source: source.clone(),
        prior: provenance.prior,
        observed: provenance.observed.clone(),
    };
    let published = |emit: &dyn Fn(EventPayload)| -> Result<RunResult, Error> {
        let entries =
            publish_one_object(with, digest, size, dataset, destination, selection, emit)?;
        emit(EventPayload::PublishCommit);
        Ok(RunResult {
            status: "materialized",
            dataset: dataset.to_owned(),
            tree: canonical::tree_digest(&entries),
            destination: destination.to_path_buf(),
            entries: entries.len() as u64,
            bytes: size,
            work: with.work.taken(),
            trust: provisional_trust(with, Some(&recorded)),
            executable: executable_paths(&entries),
            artifact: Some(recorded.clone()),
        })
    };
    if !destination.exists() {
        return published(emit);
    }

    let resolved = object_tree(with, digest, size, dataset, selection, emit)?;
    settle(
        with,
        destination,
        &Settlement {
            resolved: &resolved,
            force,
            adopt,
            dataset,
            artifact: Some(recorded.clone()),
        },
        emit,
        &|| published(emit),
        &|outcomes| {
            restore_object(
                with,
                digest,
                dataset,
                destination,
                &resolved,
                outcomes,
                emit,
            )
        },
    )
}

/// Returns the tree one cached object resolves to, having written nothing.
fn object_tree(
    with: &Materialization<'_>,
    digest: ContentDigest,
    size: u64,
    name: &str,
    selection: &Selection,
    emit: &dyn Fn(EventPayload),
) -> Result<Vec<TreeEntry>, Error> {
    let Some(format) = packed_format(with, digest, name)? else {
        return Ok(vec![TreeEntry::File {
            path: EntryPath::new(name)
                .map_err(|reason| Error::new(ErrorKind::ReferenceUnresolved, reason.to_string()))?,
            size,
            mode: Mode::ReadWrite,
            content: digest,
        }]);
    };
    let mut reader = open_archive(with, digest, format, name)?;
    let result = fetchloom_archive::resolve(&mut reader, selection, Limits::default());
    for entry in reader.take_degradations() {
        emit(EventPayload::Degrade {
            requested: entry.requested,
            used: entry.used,
            reason: entry.reason,
        });
    }
    result
}

/// Restores the entries reconcile found missing from a cached object.
///
/// Builds the object's tree in a staging directory of its own and moves only
/// the missing entries into the destination, one entry at a time.
fn restore_object(
    with: &Materialization<'_>,
    digest: ContentDigest,
    dataset: &str,
    destination: &Path,
    resolved: &[TreeEntry],
    outcomes: &[Reconciled],
    emit: &dyn Fn(EventPayload),
) -> Result<(), Error> {
    let missing: HashSet<&str> = outcomes
        .iter()
        .filter(|found| found.outcome == ReconcileOutcome::Restored)
        .map(|found| found.path.as_str())
        .collect();

    let staging = staging_beside(destination);
    if staging.exists() {
        std::fs::remove_dir_all(&staging)
            .map_err(|reason| failure(ErrorKind::DestinationForeign, &staging, &reason))?;
    }
    with.platform.create_directories(&staging)?;

    let built = build_into_staging(with, digest, dataset, &staging, resolved, emit);
    let outcome =
        built.and_then(|()| move_missing(with, &staging, destination, resolved, &missing));
    let _ = std::fs::remove_dir_all(&staging);
    outcome?;
    emit(EventPayload::PublishCommit);
    Ok(())
}

fn build_into_staging(
    with: &Materialization<'_>,
    digest: ContentDigest,
    dataset: &str,
    staging: &Path,
    resolved: &[TreeEntry],
    emit: &dyn Fn(EventPayload),
) -> Result<(), Error> {
    let Some(format) = packed_format(with, digest, dataset)? else {
        let Some(first) = resolved.first() else {
            return Ok(());
        };
        return place_object(with, digest, &staging.join(entry_path_str(first)));
    };
    extract_into(
        with,
        digest,
        format,
        dataset,
        staging,
        &Selection::default(),
        emit,
    )
    .map(|_| ())
}

fn move_missing(
    with: &Materialization<'_>,
    staging: &Path,
    destination: &Path,
    resolved: &[TreeEntry],
    missing: &HashSet<&str>,
) -> Result<(), Error> {
    let mut ordered: Vec<&TreeEntry> = resolved
        .iter()
        .filter(|entry| missing.contains(entry_path_str(entry)))
        .collect();
    ordered.sort_by_key(|entry| entry_path_str(entry).matches('/').count());
    for entry in ordered {
        let path = entry_path_str(entry);
        let from = staging.join(path);
        let to = destination.join(path);
        let parent = containing_directory(&to);
        with.platform.create_directories(&parent)?;
        if matches!(entry, TreeEntry::Directory { .. }) {
            with.platform.create_directories(&to)?;
            continue;
        }
        with.platform.publish_file(&from, &to, with.durability)?;
    }
    Ok(())
}

fn entry_path_str(entry: &TreeEntry) -> &str {
    match entry {
        TreeEntry::File { path, .. }
        | TreeEntry::Directory { path }
        | TreeEntry::Symlink { path, .. } => path.as_str(),
    }
}

fn place_object(
    with: &Materialization<'_>,
    digest: ContentDigest,
    into: &Path,
) -> Result<(), Error> {
    let Some(cache) = with.cache else {
        return Err(Error::new(
            ErrorKind::CacheCorrupt,
            "run without --no-cache, because this build streams a remote object through the cache",
        ));
    };
    let object = cache.layout().object(digest);
    with.platform.clone_or_copy(&object, into).map(|_| ())
}

fn object_name(location: &str) -> String {
    let after_scheme = location
        .split_once("://")
        .map_or(location, |(_, rest)| rest);
    let path = after_scheme
        .split(['?', '#'])
        .next()
        .unwrap_or(after_scheme);
    let last = path.rsplit('/').find(|part| !part.is_empty());
    last.map_or_else(|| "object".to_owned(), str::to_owned)
}

/// Reports whether a reference names a location this build fetches over the
/// network.
#[must_use]
pub fn is_remote(reference: &str) -> bool {
    reference.starts_with("http://") || reference.starts_with("https://")
}

/// Returns the name the object at a remote location is materialized under.
#[must_use]
pub fn remote_name(location: &str) -> String {
    object_name(location)
}

fn publish_one_object(
    with: &Materialization<'_>,
    digest: ContentDigest,
    size: u64,
    name: &str,
    destination: &Path,
    selection: &Selection,
    emit: &dyn Fn(EventPayload),
) -> Result<Vec<TreeEntry>, Error> {
    let staging = staging_beside(destination);
    if staging.exists() {
        std::fs::remove_dir_all(&staging)
            .map_err(|reason| failure(ErrorKind::DestinationForeign, &staging, &reason))?;
    }
    with.platform.create_directories(&staging)?;

    let built = match packed_format(with, digest, name)? {
        Some(format) => extract_into(with, digest, format, name, &staging, selection, emit),
        None => place_object(with, digest, &staging.join(name)).and_then(|()| {
            Ok(vec![TreeEntry::File {
                path: EntryPath::new(name).map_err(|reason| {
                    Error::new(ErrorKind::ReferenceUnresolved, reason.to_string())
                })?,
                size,
                mode: Mode::ReadWrite,
                content: digest,
            }])
        }),
    };
    let entries = match built {
        Ok(entries) => entries,
        Err(error) => {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(error);
        }
    };

    {
        let parent = containing_directory(destination);
        with.platform.create_directories(&parent)?;
    }
    if let Err(error) = with
        .platform
        .publish_directory(&staging, destination, with.durability)
    {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(error);
    }
    Ok(entries)
}

/// Returns the archive format an object holds, when it holds one this build
/// extracts.
///
/// Takes what the materialization runs against, the digest of the object, and
/// the name the location gave it. Returns nothing when the name states no
/// archive extension, when the run asked for no extraction, and when there is
/// no cache to read the object from.
///
/// # Errors
///
/// Fails with `archive.unsupported` when the name and the object's own leading
/// bytes disagree about what the object is.
fn packed_format(
    with: &Materialization<'_>,
    digest: ContentDigest,
    name: &str,
) -> Result<Option<ArchiveFormat>, Error> {
    recognized_format(with, digest, name, None)
}

/// Returns the archive format an object holds, honoring a format a manifest
/// declared.
///
/// A declared format is checked against the archive's own leading bytes.
fn recognized_format(
    with: &Materialization<'_>,
    digest: ContentDigest,
    name: &str,
    declared: Option<ArchiveFormat>,
) -> Result<Option<ArchiveFormat>, Error> {
    if !with.extract {
        return Ok(None);
    }
    let Some(cache) = with.cache else {
        return Ok(None);
    };
    if declared.is_none() && fetchloom_archive::format_from_extension(name).is_none() {
        return Ok(None);
    }
    let object = cache.layout().object(digest);
    let mut file = std::fs::File::open(&object)
        .map_err(|reason| failure(ErrorKind::ArchiveUnsupported, &object, &reason))?;
    let mut header = [0_u8; fetchloom_archive::SNIFF_LENGTH];
    let filled = read_up_to(&mut file, &mut header)?;
    fetchloom_archive::recognize(declared, name, &header[..filled])
}

fn read_up_to(file: &mut std::fs::File, into: &mut [u8]) -> Result<usize, Error> {
    let mut filled = 0;
    while filled < into.len() {
        let taken = file.read(&mut into[filled..]).map_err(|reason| {
            Error::new(
                ErrorKind::ArchiveUnsupported,
                format!("the archive's leading bytes could not be read: {reason}"),
            )
        })?;
        if taken == 0 {
            break;
        }
        filled += taken;
    }
    Ok(filled)
}

/// Opens a reader over a cached archive object.
///
/// Takes what the materialization runs against, the digest of the archive, and
/// the format it holds. Returns a reader over the object in the cache, which
/// is where every archive this build reads lives.
///
/// # Errors
///
/// Fails when the run has no cache and when the object cannot be opened.
fn open_archive(
    with: &Materialization<'_>,
    digest: ContentDigest,
    format: ArchiveFormat,
    name: &str,
) -> Result<fetchloom_archive::ArchiveReader<std::fs::File>, Error> {
    let Some(cache) = with.cache else {
        return Err(Error::new(
            ErrorKind::CacheCorrupt,
            "run without --no-cache, because this build extracts an archive out of the cache",
        ));
    };
    let object = cache.layout().object(digest);
    let file = std::fs::File::open(&object)
        .map_err(|reason| failure(ErrorKind::ArchiveUnsupported, &object, &reason))?;
    fetchloom_archive::ArchiveReader::new(file, format, name.to_owned(), Limits::default())
}

/// Extracts a cached archive into a staging directory.
///
/// Takes what the materialization runs against, the digest of the archive, the
/// format it holds, the staging directory, the selection deciding which
/// members land, and where to emit any degradation the reader recorded.
/// Returns the entries that were written.
///
/// # Errors
///
/// Fails with the archive or destination failure the reader or the extraction
/// named, and publishes nothing.
fn extract_into(
    with: &Materialization<'_>,
    digest: ContentDigest,
    format: ArchiveFormat,
    name: &str,
    staging: &Path,
    selection: &Selection,
    emit: &dyn Fn(EventPayload),
) -> Result<Vec<TreeEntry>, Error> {
    let unpacking = Span::start();
    emit(EventPayload::ExtractStart);
    let mut reader = open_archive(with, digest, format, name)?;
    let result = fetchloom_archive::extract(
        &mut reader,
        selection,
        staging,
        Limits::default(),
        with.platform,
        with.work,
    );
    for entry in reader.take_degradations() {
        emit(EventPayload::Degrade {
            requested: entry.requested,
            used: entry.used,
            reason: entry.reason,
        });
    }
    if let Ok(entries) = &result {
        emit(EventPayload::ExtractEnd {
            entries: entries.len() as u64,
            bytes: entries
                .iter()
                .map(|entry| match entry {
                    TreeEntry::File { size, .. } | TreeEntry::Symlink { size, .. } => *size,
                    TreeEntry::Directory { .. } => 0,
                })
                .sum(),
            duration_ms: unpacking.elapsed_ms(),
        });
    }
    result
}

/// Puts a local file into the cache, where the run resolves it as one object.
///
/// Takes what the materialization runs against and the source. Returns nothing
/// when the source is not one file and when there is no cache to hold it.
/// Whether the object is then extracted is decided from its own bytes, not
/// here.
///
/// # Errors
///
/// Fails when the source cannot be read into the cache.
fn object_to_resolve(with: &Materialization<'_>, source: &Path) -> Result<Option<Ingested>, Error> {
    let Some(cache) = with.cache else {
        return Ok(None);
    };
    if !source.is_file() {
        return Ok(None);
    }
    Ok(Some(cache.ingest(source)?))
}

/// Returns the manifest a run that was given no manifest resolved from.
///
/// The synthesized manifest holds the dataset name and, for a reference that
/// names a network location, that location. A local path is never recorded in
/// it.
#[must_use]
pub fn synthesized_manifest(
    dataset: &str,
    reference: &str,
) -> fetchloom_engine::manifest::Manifest {
    let sources = if is_remote(reference) {
        vec![reference.to_owned()]
    } else {
        Vec::new()
    };
    fetchloom_engine::manifest::Manifest {
        name: dataset.to_owned(),
        release: None,
        artifacts: vec![fetchloom_engine::manifest::Artifact {
            id: dataset.to_owned(),
            sources,
            size: None,
            digest: None,
            media_type: None,
            archive: None,
            select: Vec::new(),
            layout: fetchloom_engine::selection::Layout::Keep,
        }],
        license: None,
    }
}

/// Writes the receipt that records what a run materialized.
///
/// Takes the cache the receipt is kept in, the manifest the run resolved from,
/// every artifact it resolved, and what it produced. A receipt is local, so it
/// holds the absolute destination and the sources that were used; it is never
/// read as an authority for identity.
///
/// # Errors
///
/// Fails when the manifest digest cannot be taken and when the receipt cannot
/// be written.
pub fn write_receipt(
    cache: &Cache<NativePlatform>,
    manifest: &fetchloom_engine::manifest::Manifest,
    artifacts: &[ResolvedArtifact],
    result: &RunResult,
    verify: fetchloom_engine::verification::VerificationPolicy,
) -> Result<TrustClass, Error> {
    let manifest_digest = manifest.digest()?;
    let run = run_identity(cache);
    let mut recorded = std::collections::BTreeMap::new();
    let mut weakest = result.trust;
    for artifact in artifacts {
        let key = ArtifactKey::of(manifest_digest, &artifact.id);
        if let Some(origin) = artifact.observed.as_deref() {
            cache.record_witness(
                &key,
                Witness {
                    digest: artifact.digest,
                    machine: cache.token().machine.clone(),
                    origin: origin.to_owned(),
                    run: run.clone(),
                    observed_at: fetchloom_engine::timestamp::Timestamp::now(),
                },
            )?;
        }
        let witnesses = cache.witnesses(&key)?;
        let class = if verify == fetchloom_engine::verification::VerificationPolicy::Never {
            TrustClass::Unverified
        } else {
            classify(artifact.prior, artifact.digest, &witnesses)
        };
        weakest = weakest.max(class);
        recorded.insert(
            artifact.id.clone(),
            fetchloom_engine::receipt::ReceiptArtifact {
                digest: artifact.digest,
                source_used: artifact.source.clone(),
                trust: class,
            },
        );
    }
    cache.write_receipt(&Receipt {
        dataset: result.dataset.clone(),
        manifest: manifest_digest,
        artifacts: recorded,
        tree: Some(result.tree),
        executable: result.executable.clone(),
        fingerprints: fingerprints_of(cache, &result.destination),
        destination: result.destination.clone(),
        accepted_terms: None,
        fetchloom: env!("CARGO_PKG_VERSION").to_owned(),
        completed_at: fetchloom_engine::timestamp::Timestamp::now(),
    })?;
    Ok(weakest)
}

/// Returns what names this run.
fn run_identity(cache: &Cache<NativePlatform>) -> RunId {
    let token = cache.token();
    RunId::new(format!(
        "{}:{}:{}",
        token.boot.as_str(),
        token.pid,
        token.start
    ))
}

/// Returns the fingerprint every file of a destination carries right now.
///
/// A file the platform will not fingerprint is left out.
fn fingerprints_of(
    cache: &Cache<NativePlatform>,
    destination: &Path,
) -> std::collections::BTreeMap<String, RecordedFingerprint> {
    let mut found = std::collections::BTreeMap::new();
    let Ok(walked) = materialize::walk(destination) else {
        return found;
    };
    for file in &walked.files {
        let full = walked.root.join(&file.relative);
        if let Ok(fingerprint) = cache.platform().fingerprint(&full) {
            found.insert(
                file.entry.as_str().to_owned(),
                RecordedFingerprint::new(fingerprint),
            );
        }
    }
    found
}

/// Returns the name a reference's dataset is recorded under.
///
/// Takes the reference the user wrote and the local path it resolved to. A
/// network location is named by its last path component and a local path by its
/// own last component.
#[must_use]
pub fn dataset_name(reference: &str, source: &Path) -> String {
    if is_remote(reference) {
        return remote_name(reference);
    }
    source.file_name().map_or_else(
        || "dataset".to_owned(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// Materializes an object the cache already holds.
///
/// Takes what the materialization runs against, the object a plan resolved,
/// the name it materializes under, and the flags reconcile answers to. Nothing
/// is fetched.
///
/// # Errors
///
/// Fails when the object is absent, when the destination is modified or
/// foreign and neither `--force` nor `--adopt` was given, and when staging
/// cannot be published.
#[expect(
    clippy::too_many_arguments,
    reason = "the selection, force, and adopt flags each name a contract behavior of their own"
)]
pub fn materialize_cached(
    with: &Materialization<'_>,
    digest: ContentDigest,
    size: u64,
    dataset: &str,
    destination: &Path,
    selection: &Selection,
    force: bool,
    adopt: bool,
    source: &SafeUrl,
    observer: &dyn Observer,
    sequence: &Sequence,
) -> Result<RunResult, Error> {
    let emit = |payload: EventPayload| observer.emit(&Event::new(sequence, payload));
    let resolving = Span::start();
    emit(EventPayload::ResolveStart);
    emit(EventPayload::ResolveEnd {
        duration_ms: resolving.elapsed_ms(),
    });
    emit(EventPayload::CacheHit { digest });
    emit(EventPayload::PlanReady);
    let interop = match with.cache {
        Some(cache) => cache.recorded_interop(digest)?,
        None => None,
    };
    materialize_object(
        with,
        digest,
        size,
        dataset,
        destination,
        selection,
        force,
        adopt,
        interop,
        source,
        &Provenance::default(),
        &emit,
    )
}

/// What is known about where an artifact's bytes came from and what was known
/// about them before the run.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Provenance {
    /// The digest supplied before the run, when one was.
    pub prior: Option<ContentDigest>,
    /// The origin that served the bytes, present only when this run moved them.
    pub observed: Option<String>,
}

/// One artifact a manifest named, as this run resolved it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedArtifact {
    /// The name the manifest gave it.
    pub id: String,
    /// The digest its bytes hash to.
    pub digest: ContentDigest,
    /// The interop digest of the same bytes.
    pub interop: fetchloom_engine::digest::InteropDigest,
    /// The length of the object in bytes.
    pub size: u64,
    /// The source that served it, redacted as it was recorded.
    pub source: SafeUrl,
    /// The name the object is known by, which its format is read from.
    pub name: String,
    /// The members the artifact contributes.
    pub selection: Selection,
    /// The format the manifest declared, when it declared one.
    pub declared: Option<ArchiveFormat>,
    /// The digest that was supplied before the run, when one was.
    pub prior: Option<ContentDigest>,
    /// The origin that served the bytes, present only when this run transferred
    /// them in full and verified them as they arrived.
    ///
    /// A cache hit and a local file both leave it absent.
    pub observed: Option<String>,
}

/// What a run against a manifest produced.
pub struct DatasetRun {
    /// Every artifact that resolved, in manifest order, whether or not the run
    /// went on to publish anything.
    ///
    /// A run that failed partway still lists the artifacts that resolved.
    pub resolved: Vec<ResolvedArtifact>,
    /// What the run did, or what stopped it.
    pub outcome: Result<RunResult, Error>,
}

/// Reads the manifest a local reference names, when it names one.
///
/// Takes the path the reference resolved to. Returns nothing when the path does
/// not carry one of the extensions a manifest is written under, which is how a
/// data file is told from a document describing one.
///
/// # Errors
///
/// Fails with `manifest.invalid` when the path carries a manifest extension and
/// does not hold a manifest.
#[must_use]
pub fn manifest_at(source: &Path) -> Option<Result<fetchloom_engine::manifest::Manifest, Error>> {
    let syntax = fetchloom_engine::document::Syntax::of_path(source)?;
    if !source.is_file() {
        return None;
    }
    let read = std::fs::read(source).map_err(|reason| {
        Error::new(
            ErrorKind::ManifestInvalid,
            format!("make {} readable: {reason}", source.display()),
        )
    });
    Some(read.and_then(|bytes| {
        fetchloom_engine::manifest::Manifest::parse(&bytes, syntax, &Limits::default())
    }))
}

/// Materializes every artifact a manifest names into one destination.
///
/// Takes what the materialization runs against, the manifest, the directory a
/// relative source is resolved against, the destination, and what the lock
/// pins. Artifacts are resolved in the order the manifest gives them and every
/// one that verifies stays in the cache. The destination is all or nothing and
/// is published once, after every artifact has resolved.
#[expect(
    clippy::too_many_arguments,
    reason = "the manifest, the lock, and the reconcile flags each name a contract behavior of their own"
)]
pub fn materialize_manifest(
    with: &Materialization<'_>,
    manifest: &fetchloom_engine::manifest::Manifest,
    base: &Path,
    destination: &Path,
    force: bool,
    adopt: bool,
    pinned: Option<&fetchloom_engine::lock::LockedDataset>,
    observer: &dyn Observer,
    sequence: &Sequence,
) -> DatasetRun {
    let emit = |payload: EventPayload| observer.emit(&Event::new(sequence, payload));
    let resolving = Span::start();
    emit(EventPayload::ResolveStart);

    let mut resolved = Vec::new();
    for artifact in &manifest.artifacts {
        match resolve_artifact(with, artifact, base, pinned, observer, sequence) {
            Ok(found) => resolved.push(found),
            Err(error) => {
                return DatasetRun {
                    resolved,
                    outcome: Err(error.with_dataset(manifest.name.clone())),
                };
            }
        }
    }
    emit(EventPayload::ResolveEnd {
        duration_ms: resolving.elapsed_ms(),
    });
    emit(EventPayload::PlanReady);

    let outcome = publish_dataset(
        with,
        &resolved,
        &manifest.name,
        destination,
        force,
        adopt,
        &emit,
    );
    DatasetRun { resolved, outcome }
}

/// Puts one artifact's bytes in the cache and reports what they are.
fn resolve_artifact(
    with: &Materialization<'_>,
    artifact: &fetchloom_engine::manifest::Artifact,
    base: &Path,
    pinned: Option<&fetchloom_engine::lock::LockedDataset>,
    observer: &dyn Observer,
    sequence: &Sequence,
) -> Result<ResolvedArtifact, Error> {
    let emit = |payload: EventPayload| observer.emit(&Event::new(sequence, payload));
    let Some(first) = artifact.sources.first() else {
        return Err(Error::new(
            ErrorKind::ReferenceUnresolved,
            format!(
                "name a source for {}, because an artifact with none resolves to nothing",
                artifact.id
            ),
        )
        .with_artifact(artifact.id.clone()));
    };
    let expected = artifact
        .digest
        .and_then(|claims| claims.blake3)
        .or_else(|| {
            pinned
                .and_then(|entry| entry.artifacts.get(&artifact.id))
                .map(|locked| locked.digest)
        });

    let selection = Selection {
        include: artifact.select.clone(),
        exclude: Vec::new(),
        layout: artifact.layout,
    };
    let name = object_name(first);
    let declared = artifact.archive.as_ref().map(|spec| spec.format);

    if is_remote(first) {
        let moved = transfer_object(with, &artifact.sources, expected, observer, sequence)?;
        return Ok(ResolvedArtifact {
            id: artifact.id.clone(),
            digest: moved.digest,
            interop: moved.interop,
            size: moved.size,
            source: SafeUrl::new(first),
            name,
            selection,
            declared,
            prior: expected,
            observed: moved.observed,
        });
    }

    let path = resolve_source_path(base, first);
    let Some(cache) = with.cache else {
        return Err(Error::new(
            ErrorKind::CacheCorrupt,
            "run without --no-cache, because this build resolves an artifact through the cache",
        )
        .with_artifact(artifact.id.clone()));
    };
    if !path.exists() {
        return Err(Error::new(
            ErrorKind::ReferenceUnresolved,
            format!("check that {} names a path that exists", path.display()),
        )
        .with_artifact(artifact.id.clone()));
    }
    let ingested = cache
        .ingest(&path)
        .map_err(|reason| reason.with_artifact(artifact.id.clone()))?;
    if let Some(expected) = expected
        && expected != ingested.digest
    {
        return Err(Error::new(
            ErrorKind::IntegrityMismatch,
            format!(
                "correct the manifest or replace the bytes, because {} hashes to {} where {} was \
                 stated",
                path.display(),
                ingested.digest,
                expected
            ),
        )
        .with_artifact(artifact.id.clone()));
    }
    if ingested.was_present {
        emit(EventPayload::CacheHit {
            digest: ingested.digest,
        });
    } else {
        emit(EventPayload::CacheMiss {
            digest: ingested.digest,
        });
    }
    Ok(ResolvedArtifact {
        id: artifact.id.clone(),
        digest: ingested.digest,
        interop: ingested.interop,
        size: ingested.size,
        source: SafeUrl::new(&path.to_string_lossy()),
        name,
        selection,
        declared,
        prior: expected,
        observed: None,
    })
}

/// Returns the path a manifest's source names, relative to the manifest.
fn resolve_source_path(base: &Path, source: &str) -> PathBuf {
    let stated = PathBuf::from(source.strip_prefix("file://").unwrap_or(source));
    if stated.is_absolute() {
        stated
    } else {
        base.join(stated)
    }
}

/// Returns what the cache already holds for a reference, which is what a
/// conditional request is built from.
fn prior_from(
    cache: &Cache<NativePlatform>,
) -> impl Fn(&str) -> Option<fetchloom_engine::transfer::Prior> + '_ {
    move |location: &str| {
        let found = cache.resolution(location).ok().flatten()?;
        Some(fetchloom_engine::transfer::Prior {
            digest: found.digest,
            validator: fetchloom_engine::seam::source::Validator {
                etag: found.etag,
                last_modified: found.last_modified,
            },
        })
    }
}

/// Records what a reference resolved to and the validator that came with it.
///
/// A transfer that learned no validator leaves whatever was recorded alone.
fn remember(
    cache: &Cache<NativePlatform>,
    location: &str,
    transferred: &fetchloom_engine::transfer::Transferred,
) -> Result<(), Error> {
    if !transferred.validator.can_be_asked_with() {
        return Ok(());
    }
    cache.record_resolution(
        location,
        &fetchloom_cache::resolution::Resolution {
            digest: transferred.digest,
            etag: transferred.validator.etag.clone(),
            last_modified: transferred.validator.last_modified.clone(),
        },
    )
}

/// What one transfer of an artifact produced.
pub struct Moved {
    /// The digest the bytes hash to.
    pub digest: ContentDigest,
    /// The interop digest of the same bytes.
    pub interop: fetchloom_engine::digest::InteropDigest,
    /// The length of the object in bytes.
    pub size: u64,
    /// The origin that served the bytes, present only when this run moved them.
    pub observed: Option<String>,
}

/// Moves one artifact's bytes from the first source that can serve them.
fn transfer_object(
    with: &Materialization<'_>,
    locations: &[String],
    expected: Option<ContentDigest>,
    observer: &dyn Observer,
    sequence: &Sequence,
) -> Result<Moved, Error> {
    let Some(cache) = with.cache else {
        return Err(Error::new(
            ErrorKind::CacheCorrupt,
            "run without --no-cache, because this build streams a remote object through the cache",
        ));
    };
    let source = HttpSource::new(Limits::default(), Arc::clone(with.work));
    let pause = SleepingPause;
    let limits = Limits::default();
    let degradations = DegradeQueue::new();
    let transfer = Transfer {
        store: cache,
        source: &source,
        pause: &pause,
        limits: &limits,
        degradations: &degradations,
        observer,
        sequence,
    };
    let transferred = transfer.run(expected, &prior_from(cache), locations)?;
    for location in locations {
        remember(cache, location, &transferred)?;
    }
    for entry in source
        .take_degradations()
        .into_iter()
        .chain(degradations.take())
    {
        observer.emit(&Event::new(
            sequence,
            EventPayload::Degrade {
                requested: entry.requested,
                used: entry.used,
                reason: entry.reason,
            },
        ));
    }
    let interop = match transferred.interop {
        Some(interop) => interop,
        None => cache.recorded_interop(transferred.digest)?.ok_or_else(|| {
            Error::new(
                ErrorKind::CacheCorrupt,
                "run cache verify, because the cache holds an object it recorded nothing about",
            )
        })?,
    };
    let moved = transferred.bytes_kept + transferred.bytes_transferred;
    let size = if moved > 0 {
        moved
    } else {
        std::fs::metadata(cache.layout().object(transferred.digest))
            .map(|found| found.len())
            .unwrap_or_default()
    };
    Ok(Moved {
        digest: transferred.digest,
        interop,
        size,
        observed: observation(&transferred, locations),
    })
}

/// Returns the origin that served an artifact's bytes, when this run moved
/// them.
///
/// A run that moved nothing observed nothing about what a source is serving: it
/// read a file it already had. Only a run that carried every byte and hashed
/// them as they arrived has evidence, so only one of those may become a
/// witness.
fn observation(
    transferred: &fetchloom_engine::transfer::Transferred,
    locations: &[String],
) -> Option<String> {
    if transferred.bytes_transferred == 0 {
        return None;
    }
    locations
        .first()
        .map(|location| SafeUrl::new(location).to_string())
}

/// Publishes every resolved artifact into one destination.
fn publish_dataset(
    with: &Materialization<'_>,
    resolved: &[ResolvedArtifact],
    dataset: &str,
    destination: &Path,
    force: bool,
    adopt: bool,
    emit: &dyn Fn(EventPayload),
) -> Result<RunResult, Error> {
    let bytes = resolved.iter().map(|artifact| artifact.size).sum();
    let build = || -> Result<RunResult, Error> {
        let entries = build_dataset_staging(with, resolved, destination, emit)?;
        emit(EventPayload::PublishCommit);
        Ok(RunResult {
            status: "materialized",
            dataset: dataset.to_owned(),
            tree: canonical::tree_digest(&entries),
            destination: destination.to_path_buf(),
            entries: entries.len() as u64,
            bytes,
            work: with.work.taken(),
            trust: provisional_trust(with, None),
            executable: executable_paths(&entries),
            artifact: None,
        })
    };
    if !destination.exists() {
        return build();
    }

    let mut expected = Vec::new();
    for artifact in resolved {
        expected.extend(dataset_entries(with, artifact, emit)?);
    }
    settle(
        with,
        destination,
        &Settlement {
            resolved: &expected,
            force,
            adopt,
            dataset,
            artifact: None,
        },
        emit,
        &build,
        &|_| build().map(|_| ()),
    )
}

/// Returns the entries one resolved artifact contributes, writing nothing.
fn dataset_entries(
    with: &Materialization<'_>,
    artifact: &ResolvedArtifact,
    emit: &dyn Fn(EventPayload),
) -> Result<Vec<TreeEntry>, Error> {
    let Some(format) = recognized_format(with, artifact.digest, &artifact.name, artifact.declared)?
    else {
        return Ok(vec![TreeEntry::File {
            path: EntryPath::new(&artifact.name)
                .map_err(|reason| Error::new(ErrorKind::ReferenceUnresolved, reason.to_string()))?,
            size: artifact.size,
            mode: Mode::ReadWrite,
            content: artifact.digest,
        }]);
    };
    let mut reader = open_archive(with, artifact.digest, format, &artifact.name)?;
    let result = fetchloom_archive::resolve(&mut reader, &artifact.selection, Limits::default());
    for entry in reader.take_degradations() {
        emit(EventPayload::Degrade {
            requested: entry.requested,
            used: entry.used,
            reason: entry.reason,
        });
    }
    result
}

/// Builds every resolved artifact into one staging directory and publishes it.
fn build_dataset_staging(
    with: &Materialization<'_>,
    resolved: &[ResolvedArtifact],
    destination: &Path,
    emit: &dyn Fn(EventPayload),
) -> Result<Vec<TreeEntry>, Error> {
    let staging = staging_beside(destination);
    if staging.exists() {
        std::fs::remove_dir_all(&staging)
            .map_err(|reason| failure(ErrorKind::DestinationForeign, &staging, &reason))?;
    }
    with.platform.create_directories(&staging)?;

    let built = fill_dataset_staging(with, resolved, &staging, emit);
    let entries = match built {
        Ok(entries) => entries,
        Err(error) => {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(error);
        }
    };
    {
        let parent = containing_directory(destination);
        with.platform.create_directories(&parent)?;
    }
    if let Err(error) = with
        .platform
        .publish_directory(&staging, destination, with.durability)
    {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(error);
    }
    Ok(entries)
}

fn fill_dataset_staging(
    with: &Materialization<'_>,
    resolved: &[ResolvedArtifact],
    staging: &Path,
    emit: &dyn Fn(EventPayload),
) -> Result<Vec<TreeEntry>, Error> {
    let mut entries = Vec::new();
    for artifact in resolved {
        if let Some(format) =
            recognized_format(with, artifact.digest, &artifact.name, artifact.declared)?
        {
            entries.extend(extract_into(
                with,
                artifact.digest,
                format,
                &artifact.name,
                staging,
                &artifact.selection,
                emit,
            )?);
        } else {
            {
                let at = staging.join(&artifact.name);
                if at.exists() {
                    return Err(Error::new(
                        ErrorKind::ArchiveCollision,
                        format!(
                            "rename one of them, because two artifacts both land on {}",
                            artifact.name
                        ),
                    ));
                }
                place_object(with, artifact.digest, &at)?;
                entries.push(TreeEntry::File {
                    path: EntryPath::new(&artifact.name).map_err(|reason| {
                        Error::new(ErrorKind::ReferenceUnresolved, reason.to_string())
                    })?,
                    size: artifact.size,
                    mode: Mode::ReadWrite,
                    content: artifact.digest,
                });
            }
        }
    }
    Ok(entries)
}

/// Returns what a run against a single object resolved, in the form the lock
/// and the receipt record it.
///
/// Returns nothing when the run resolved no object, which is what a reference
/// naming a directory does.
#[must_use]
pub fn resolved_object(result: &RunResult, selection: &Selection) -> Vec<ResolvedArtifact> {
    let Some(artifact) = &result.artifact else {
        return Vec::new();
    };
    let Some(interop) = artifact.interop else {
        return Vec::new();
    };
    vec![ResolvedArtifact {
        id: artifact.id.clone(),
        digest: artifact.digest,
        interop,
        size: artifact.size,
        source: artifact.source.clone(),
        name: artifact.id.clone(),
        selection: selection.clone(),
        declared: None,
        prior: artifact.prior,
        observed: artifact.observed.clone(),
    }]
}

/// Returns the trust class a run may claim before its own witness is recorded.
///
/// Takes what the materialization runs against and the object it resolved,
/// when it resolved one. Returns `unverified` when the run was told to check
/// nothing, `verified` when a digest the lock or the manifest supplied before
/// the run matches what was produced, and `tofu` otherwise. A run that records
/// a receipt replaces this with the class its witnesses support, which is the
/// only way `corroborated` is reached.
fn provisional_trust(
    with: &Materialization<'_>,
    artifact: Option<&RecordedArtifact>,
) -> TrustClass {
    if with.verify == fetchloom_engine::verification::VerificationPolicy::Never {
        return TrustClass::Unverified;
    }
    match artifact {
        Some(artifact) if artifact.prior == Some(artifact.digest) => TrustClass::Verified,
        _ => TrustClass::Tofu,
    }
}
