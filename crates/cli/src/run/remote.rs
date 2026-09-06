//! Materializing one object named by a location.

use super::adapters::{
    credentials_for, host_of, offers_for, record_measurement, required_credential,
    resolve_credential, unserved,
};
use super::archive::{extract_into, packed_format};
use super::container::transfer_container_entries;
use super::context::{Materialization, Moved, RunResult};
use super::dataset::{Provenance, prior_from, remember};
use super::object::{materialize_object, place_object};
use super::paths::{containing_directory, object_name, staging_beside};
use super::selection::selected_entries;
use fetchloom_engine::degrade::DegradeQueue;
use fetchloom_engine::digest::ContentDigest;

use fetchloom_engine::error::{Error, ErrorKind, Surface, filesystem_failure};
use fetchloom_engine::event::{Event, EventPayload, Sequence, Span};
use fetchloom_engine::flights::Flights;
use fetchloom_engine::redact::SafeUrl;
use fetchloom_engine::seam::observer::Observer;
use fetchloom_engine::seam::platform::Platform;
use fetchloom_engine::seam::source::Source;
use fetchloom_engine::selection::Selection;
use fetchloom_engine::transfer::{SleepingPause, Transfer};
use fetchloom_engine::tree::{EntryPath, Mode, TreeEntry};
use std::path::Path;

#[expect(
    clippy::too_many_arguments,
    reason = "the selection, force, and adopt flags each name a contract behavior of their own"
)]
/// # Errors
/// Whatever probing, transferring, verifying or publishing the object
/// reports: the `network.*` kinds, `integrity.mismatch` against a pinned
/// digest, `destination.modified` for a destination this run may not replace,
/// and the destination's own filesystem kinds.
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
    let Some((source, _)) = with.adapters.serving(location) else {
        return Err(unserved(location));
    };
    let pause = SleepingPause;
    let limits = *with.policy.limits();
    emit(EventPayload::ResolveEnd {
        duration_ms: resolving.elapsed_ms(),
    });
    emit(EventPayload::PlanReady);

    let Some(cache) = with.cache else {
        return Err(Error::new(
            ErrorKind::CacheCorrupt,
            "make a directory Fetchloom can write to, because a run streams an object through a store and neither the cache nor a scratch store beside the destination could be opened",
        ));
    };
    let degradations = DegradeQueue::new();
    let host = host_of(location);
    let flights = Flights::new(with.tuning.ceilings, |host: &str| {
        with.tuning.controller(host, Some(cache))
    });
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
        .run(pinned, &prior_from(cache), &[location.to_owned()])
        .map_err(|failure| required_credential(with.policy, &host, failure))?;
    record_measurement(
        cache,
        with,
        &host,
        &flights,
        transferred.bytes_transferred,
        moving.elapsed(),
    );
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
        cache.size_of(transferred.digest).unwrap_or_default()
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
            observed: observation(&transferred),
        },
        &emit,
    )
}

pub(crate) fn infer_remote(
    with: &Materialization<'_>,
    location: &str,
    observer: &dyn Observer,
    sequence: &Sequence,
) -> Result<Vec<(String, ContentDigest, u64)>, Error> {
    let emit = |payload: EventPayload| observer.emit(&Event::new(sequence, payload));
    let Some(cache) = with.cache else {
        return Err(Error::new(
            ErrorKind::CacheCorrupt,
            "make a directory Fetchloom can write to, because inference streams every object it reads through a store",
        ));
    };
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
    let listed = selected_entries(location, listing.entries, &Selection::default())?;
    emit(EventPayload::ListingEnd {
        entries: listed.len() as u64,
        duration_ms: listing_started.elapsed_ms(),
    });
    transfer_container_entries(
        with, cache, source, location, &listed, observer, sequence, &emit,
    )
}

pub(super) fn publish_one_object(
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
            .map_err(|reason| filesystem_failure(Surface::Destination, &staging, &reason))?;
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

pub(super) fn transfer_object(
    with: &Materialization<'_>,
    source: &fetchloom_engine::erased::Adapters,
    flights: &Flights<'_>,
    locations: &[String],
    expected: Option<ContentDigest>,
    observer: &dyn Observer,
    sequence: &Sequence,
) -> Result<Moved, Error> {
    let Some(cache) = with.cache else {
        return Err(Error::new(
            ErrorKind::CacheCorrupt,
            "make a directory Fetchloom can write to, because a run streams an object through a store and neither the cache nor a scratch store beside the destination could be opened",
        ));
    };
    let pause = SleepingPause;
    let limits = *with.policy.limits();
    let degradations = DegradeQueue::new();
    let host = locations
        .first()
        .map(|first| host_of(first))
        .unwrap_or_default();
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
        flights,
        credential: &credential,
        offer: &offer,
        meter: meter.as_ref(),
    };
    let moving = std::time::Instant::now();
    let transferred = transfer
        .run(expected, &prior_from(cache), locations)
        .map_err(|failure| required_credential(with.policy, &host, failure))?;
    record_measurement(
        cache,
        with,
        &host,
        flights,
        transferred.bytes_transferred,
        moving.elapsed(),
    );
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
        cache.size_of(transferred.digest).unwrap_or_default()
    };
    Ok(Moved {
        digest: transferred.digest,
        interop,
        size,
        observed: observation(&transferred),
        chosen: transferred.chosen.clone(),
    })
}

pub(super) fn observation(transferred: &fetchloom_engine::transfer::Transferred) -> Option<String> {
    if transferred.bytes_transferred == 0 {
        return None;
    }
    Some(transferred.served.to_string())
}
