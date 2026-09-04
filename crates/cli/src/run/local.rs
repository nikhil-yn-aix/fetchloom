//! Materializing from a path that is already on this filesystem.

use super::context::{Materialization, RecordedArtifact, RunResult};
use super::dataset::{Provenance, provisional_trust};
use super::object::materialize_object;
use super::paths::{containing_directory, executable_paths, staging_beside, temp_beside};
use super::selection::apply_selection;
use super::verify::{destination_entries, hash_files, report_unread_modes};
use crate::materialize;
use fetchloom_cache::Cache;
use fetchloom_cache::ingest::Ingested;
use fetchloom_engine::canonical;
use fetchloom_engine::digest::ContentDigest;
use fetchloom_engine::error::{Error, ErrorKind, Layer, Surface, filesystem_failure};
use fetchloom_engine::event::{Event, EventPayload, Sequence, Span};
use fetchloom_engine::hashing;
use fetchloom_engine::outcome::RunStatus;
use fetchloom_engine::pool::Processor;
use fetchloom_engine::reconcile::{ReconcileOutcome, Reconciled, reconcile};
use fetchloom_engine::redact::SafeUrl;
use fetchloom_engine::seam::observer::Observer;
use fetchloom_engine::seam::platform::Platform;
use fetchloom_engine::selection::Selection;
use fetchloom_engine::tree::{EntryPath, Mode, TreeEntry};
use fetchloom_engine::work::WorkCounter;
use fetchloom_platform::NativePlatform;
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// Materializes a local source tree into a destination.
///
/// # Errors
///
/// Fails when the source cannot be read, when the selection matches nothing or
/// a layout leaves a member with no path or a collision, when a destination
/// entry is modified or foreign and neither `force` nor `adopt` was given, and
/// when staging cannot be published.
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

pub(super) fn materialize_fresh(
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
            .map_err(|reason| filesystem_failure(Surface::Destination, &staging, &reason))?;
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
        status: RunStatus::Materialized,
        dataset: dataset.to_owned(),
        tree,
        destination: destination.to_path_buf(),
        entries: entries.len() as u64,
        bytes: entries.iter().map(entry_size).sum(),
        work: with.work.taken(),
        trust: provisional_trust(with, None),
        executable: executable_paths(&entries),
        artifact: None,
    })
}

pub(super) fn entry_size(entry: &TreeEntry) -> u64 {
    match entry {
        TreeEntry::File { size, .. } | TreeEntry::Symlink { size, .. } => *size,
        TreeEntry::Directory { .. } => 0,
    }
}

/// Returns a destination's entries with every mode taken from the resolved
/// tree.
pub(super) fn with_resolved_modes(resolved: &[TreeEntry], found: Vec<TreeEntry>) -> Vec<TreeEntry> {
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
pub(super) struct Settlement<'a> {
    /// The tree the run resolved, whatever the source was.
    pub(super) resolved: &'a [TreeEntry],
    /// Whether the run may overwrite a modified entry and remove a foreign one.
    pub(super) force: bool,
    /// Whether the run accepts the destination as it stands.
    pub(super) adopt: bool,
    /// What the run calls the dataset.
    pub(super) dataset: &'a str,
    /// The object the run resolved, when it resolved one.
    pub(super) artifact: Option<RecordedArtifact>,
}

/// Decides one run against an existing destination.
///
/// # Errors
///
/// Fails with `destination.modified` or `destination.foreign` naming every path
/// when neither `--force` nor `--adopt` was given, and with whatever rebuilding
/// or restoring fails with.
pub(super) fn settle(
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
            status: RunStatus::Unchanged,
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
            status: RunStatus::Adopted,
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
        status: RunStatus::Restored,
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

pub(super) fn reconcile_existing(
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

pub(super) fn restore_missing(
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

pub(super) fn fill_staging(
    with: &Materialization<'_>,
    staging: &Path,
    walked: &materialize::Walked,
    emit: &dyn Fn(EventPayload),
) -> Result<Vec<TreeEntry>, Error> {
    let source = walked.root.as_path();
    let moving = Span::start();
    emit(EventPayload::TransferStart {
        source: fetchloom_engine::redact::SafeUrl::new(&source.to_string_lossy()),
        host: fetchloom_engine::reference::Host::new(""),
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
        host: fetchloom_engine::reference::Host::new(""),
        bytes: copied,
        duration_ms: moving.elapsed_ms(),
    });
    Ok(entries)
}

/// Places one file at its target, through the cache when one is open.
///
/// # Errors
///
/// Fails when the source cannot be read or the target cannot be written.
pub(super) fn place_file(
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
            &mut with
                .digester
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            processor,
            with.work,
            from,
            to,
        ),
        Some(held) => match through_cache(
            &mut with
                .digester
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
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
                    &mut with
                        .digester
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner),
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
pub(super) fn through_cache(
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

/// Puts a local file into the cache, where the run resolves it as one object.
///
/// # Errors
///
/// Fails when the source cannot be read into the cache.
pub(super) fn object_to_resolve(
    with: &Materialization<'_>,
    source: &Path,
) -> Result<Option<Ingested>, Error> {
    let Some(cache) = with.cache else {
        return Ok(None);
    };
    if !source.is_file() {
        return Ok(None);
    }
    Ok(Some(cache.ingest(source)?))
}
