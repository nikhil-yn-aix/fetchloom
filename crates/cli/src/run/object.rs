//! Placing one transferred object into its destination.

use super::archive::{extract_into, open_archive, packed_format};
use super::context::{Materialization, RecordedArtifact, RunResult};
use super::dataset::{Provenance, provisional_trust};
use super::local::{Settlement, settle};
use super::paths::{containing_directory, entry_path_str, executable_paths, staging_beside};
use super::remote::publish_one_object;
use fetchloom_engine::canonical;
use fetchloom_engine::digest::ContentDigest;
use fetchloom_engine::error::{Error, ErrorKind, Surface, filesystem_failure};
use fetchloom_engine::event::EventPayload;
use fetchloom_engine::limits::Limits;
use fetchloom_engine::outcome::RunStatus;
use fetchloom_engine::reconcile::{ReconcileOutcome, Reconciled};
use fetchloom_engine::redact::SafeUrl;
use fetchloom_engine::seam::platform::Platform;
use fetchloom_engine::selection::Selection;
use fetchloom_engine::tree::{EntryPath, Mode, TreeEntry};
use std::collections::HashSet;
use std::path::Path;

/// Materializes one cached object into a destination, reconciling when the
/// destination already exists.
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
pub(super) fn materialize_object(
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
            status: RunStatus::Materialized,
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
pub(super) fn object_tree(
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
pub(super) fn restore_object(
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
            .map_err(|reason| filesystem_failure(Surface::Destination, &staging, &reason))?;
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

pub(super) fn build_into_staging(
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

pub(super) fn move_missing(
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

pub(super) fn place_object(
    with: &Materialization<'_>,
    digest: ContentDigest,
    into: &Path,
) -> Result<(), Error> {
    let Some(cache) = with.cache else {
        return Err(Error::new(
            ErrorKind::CacheCorrupt,
            "make a directory Fetchloom can write to, because a run streams an object through a store and neither the cache nor a scratch store beside the destination could be opened",
        ));
    };
    cache.place_object(digest, into)
}
