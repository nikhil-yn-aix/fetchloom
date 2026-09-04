//! Materializing every object a listed container holds.

use super::adapters::{
    credentials_for, host_of, offers_for, record_measurement, required_credential,
    resolve_credential, unserved,
};
use super::context::{Materialization, RunResult};
use super::dataset::{prior_from, provisional_trust, remember};
use super::local::{Settlement, entry_size, settle};
use super::object::place_object;
use super::paths::{container_name, containing_directory, executable_paths, staging_beside};
use super::selection::selected_entries;
use fetchloom_cache::Cache;
use fetchloom_engine::canonical;
use fetchloom_engine::degrade::DegradeQueue;
use fetchloom_engine::digest::ContentDigest;
use fetchloom_engine::erased::AnySource;
use fetchloom_engine::error::{Error, ErrorKind, Surface, filesystem_failure};
use fetchloom_engine::event::{Event, EventPayload, Sequence, Span};
use fetchloom_engine::flights::Flights;
use fetchloom_engine::outcome::RunStatus;
use fetchloom_engine::redact::SafeUrl;
use fetchloom_engine::seam::observer::Observer;
use fetchloom_engine::seam::platform::Platform;
use fetchloom_engine::seam::source::Source;
use fetchloom_engine::selection::Selection;
use fetchloom_engine::transfer::{SleepingPause, Transfer};
use fetchloom_engine::tree::{EntryPath, Mode, TreeEntry};
use fetchloom_platform::NativePlatform;
use std::path::Path;

/// Lists a container, transfers every entry it holds, and materializes them
/// into one destination.
///
/// # Errors
///
/// Fails when the container cannot be listed, when an entry cannot be
/// transferred, and when the destination cannot be published.
#[expect(
    clippy::too_many_arguments,
    reason = "a container materialization is decided by what it runs under, where it reads, where it writes, what it selects, the two flags that govern an existing destination, and both observers"
)]
pub fn materialize_remote_container(
    with: &Materialization<'_>,
    location: &str,
    destination: &Path,
    selection: &Selection,
    force: bool,
    adopt: bool,
    observer: &dyn Observer,
    sequence: &Sequence,
) -> Result<RunResult, Error> {
    let emit = |payload: EventPayload| observer.emit(&Event::new(sequence, payload));
    let dataset = container_name(location);

    let Some(cache) = with.cache else {
        return Err(Error::new(
            ErrorKind::CacheCorrupt,
            "make a directory Fetchloom can write to, because a run streams an object through a store and neither the cache nor a scratch store beside the destination could be opened",
        ));
    };

    let resolving = Span::start();
    emit(EventPayload::ResolveStart);
    let Some((source, _)) = with.adapters.serving(location) else {
        return Err(unserved(location));
    };

    let listing_started = Span::start();
    emit(EventPayload::ListingStart {
        source: SafeUrl::new(location),
    });
    let credential = resolve_credential(with.policy, &host_of(location))?;
    let listing = source.list(location, credential.as_ref())?;
    if listing.skipped > 0 {
        emit(EventPayload::ListingSkipped {
            count: listing.skipped,
        });
    }
    let listed = selected_entries(location, listing.entries, selection)?;
    emit(EventPayload::ListingEnd {
        entries: listed.len() as u64,
        duration_ms: listing_started.elapsed_ms(),
    });

    let placements = transfer_container_entries(
        with, cache, source, location, &listed, observer, sequence, &emit,
    )?;
    emit(EventPayload::ResolveEnd {
        duration_ms: resolving.elapsed_ms(),
    });
    emit(EventPayload::PlanReady);

    let build = || {
        let built = place_container_entries(with, destination, &placements)?;
        let tree = canonical::tree_digest(&built);
        {
            let parent = containing_directory(destination);
            with.platform.create_directories(&parent)?;
        }
        let staging = staging_beside(destination);
        if let Err(error) = with
            .platform
            .publish_directory(&staging, destination, with.durability)
        {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(error);
        }
        for degradation in with.platform.take_degradations() {
            emit(EventPayload::Degrade {
                requested: degradation.requested,
                used: degradation.used,
                reason: degradation.reason,
            });
        }
        emit(EventPayload::PublishCommit);

        Ok(RunResult {
            status: RunStatus::Materialized,
            dataset: dataset.clone(),
            tree,
            destination: destination.to_path_buf(),
            entries: built.len() as u64,
            bytes: built.iter().map(entry_size).sum(),
            work: with.work.taken(),
            trust: provisional_trust(with, None),
            executable: executable_paths(&built),
            artifact: None,
        })
    };
    if !destination.exists() {
        return build();
    }

    let resolved = container_tree(&placements)?;
    settle(
        with,
        destination,
        &Settlement {
            resolved: &resolved,
            force,
            adopt,
            dataset: &dataset,
            artifact: None,
        },
        &emit,
        &build,
        &|_| build().map(|_| ()),
    )
}

/// Returns the tree a container's transferred entries resolve to.
pub(super) fn container_tree(
    placements: &[(String, ContentDigest, u64)],
) -> Result<Vec<TreeEntry>, Error> {
    placements
        .iter()
        .map(|(path, digest, size)| {
            Ok(TreeEntry::File {
                path: EntryPath::new(path).map_err(|reason| {
                    Error::new(ErrorKind::ReferenceUnresolved, reason.to_string())
                })?,
                size: *size,
                mode: Mode::ReadWrite,
                content: *digest,
            })
        })
        .collect()
}

/// Transfers every listed entry of a container into the cache, holding the run
/// inside its global and per-host in-flight bounds.
#[expect(
    clippy::too_many_arguments,
    reason = "each argument names a piece of the transfer a container's entries share"
)]
pub(super) fn transfer_container_entries(
    with: &Materialization<'_>,
    cache: &Cache<NativePlatform>,
    source: &AnySource,
    location: &str,
    listed: &[fetchloom_engine::seam::source::ListingEntry],
    observer: &dyn Observer,
    sequence: &Sequence,
    emit: &(dyn Fn(EventPayload) + Sync),
) -> Result<Vec<(String, ContentDigest, u64)>, Error> {
    let limits = *with.policy.limits();
    let pause = SleepingPause;
    let locations: Vec<String> = listed
        .iter()
        .map(|entry| fetchloom_sources::joined(location, &entry.path))
        .collect();
    let hosts: Vec<String> = locations.iter().map(|one| host_of(one)).collect();
    let flights = Flights::new(with.tuning.ceilings, |host: &str| {
        with.tuning.controller(host, Some(cache))
    });

    let produced = flights.each(&locations, &hosts, &|object_location: &String| {
        let degradations = DegradeQueue::new();
        let host = host_of(object_location);
        let meter = with.tuning.meter();
        let measurement = |location: &str| cache.measurement(&host_of(location));
        let credential = credentials_for(with.policy);
        let offer = offers_for(with.policy);
        let transfer = Transfer {
            store: cache,
            source,
            pause: &pause,
            limits: &limits,
            degradations: &degradations,
            measurement: &measurement,
            observer,
            sequence,
            flights: &flights,
            meter: meter.as_ref(),
            credential: &credential,
            offer: &offer,
        };
        let moving = std::time::Instant::now();
        let transferred = transfer
            .run(
                None,
                &prior_from(cache),
                std::slice::from_ref(object_location),
            )
            .map_err(|failure| required_credential(with.policy, &host, failure))?;
        record_measurement(
            cache,
            with,
            &host,
            &flights,
            transferred.bytes_transferred,
            moving.elapsed(),
        );
        remember(cache, object_location, &transferred)?;
        for degradation in degradations.take() {
            emit(EventPayload::Degrade {
                requested: degradation.requested,
                used: degradation.used,
                reason: degradation.reason,
            });
        }
        let size = if transferred.bytes_kept + transferred.bytes_transferred > 0 {
            transferred.bytes_kept + transferred.bytes_transferred
        } else {
            cache.size_of(transferred.digest).unwrap_or_default()
        };
        Ok((transferred.digest, size))
    });

    for degradation in source.take_degradations() {
        emit(EventPayload::Degrade {
            requested: degradation.requested,
            used: degradation.used,
            reason: degradation.reason,
        });
    }
    if let Some(failure) = produced.failure {
        return Err(failure);
    }
    Ok(listed
        .iter()
        .zip(produced.outputs)
        .map(|(entry, (digest, size))| (entry.path.clone(), digest, size))
        .collect())
}

/// Builds a fresh staging directory holding every transferred entry, at its
/// listed path.
pub(super) fn place_container_entries(
    with: &Materialization<'_>,
    destination: &Path,
    placements: &[(String, ContentDigest, u64)],
) -> Result<Vec<TreeEntry>, Error> {
    let staging = staging_beside(destination);
    if staging.exists() {
        std::fs::remove_dir_all(&staging)
            .map_err(|reason| filesystem_failure(Surface::Destination, &staging, &reason))?;
    }
    with.platform.create_directories(&staging)?;

    let mut built = Vec::with_capacity(placements.len());
    for (path, digest, size) in placements {
        let target = staging.join(path);
        let outcome = with
            .platform
            .create_directories(&containing_directory(&target))
            .and_then(|()| place_object(with, *digest, &target))
            .and_then(|()| {
                EntryPath::new(path).map_err(|reason| {
                    Error::new(ErrorKind::ReferenceUnresolved, reason.to_string())
                })
            });
        match outcome {
            Ok(path) => built.push(TreeEntry::File {
                path,
                size: *size,
                mode: Mode::ReadWrite,
                content: *digest,
            }),
            Err(error) => {
                let _ = std::fs::remove_dir_all(&staging);
                return Err(error);
            }
        }
    }
    Ok(built)
}
