//! Materializing from a content address the cache already holds.

use super::context::{Materialization, RunResult};
use super::dataset::Provenance;
use super::object::materialize_object;
use fetchloom_engine::digest::ContentDigest;
use fetchloom_engine::error::Error;
use fetchloom_engine::event::{Event, EventPayload, Sequence, Span};
use fetchloom_engine::redact::SafeUrl;
use fetchloom_engine::seam::observer::Observer;
use fetchloom_engine::selection::Selection;
use std::path::Path;

/// Materializes an object the cache already holds.
///
/// # Errors
///
/// Fails when the object is absent, when the destination is modified or foreign
/// and neither `--force` nor `--adopt` was given, and when staging cannot be
/// published.
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
