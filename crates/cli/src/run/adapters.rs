//! Which adapter serves a reference, which credential reaches a host, and what bounds the transfers.

use super::context::Materialization;
use fetchloom_cache::Cache;
use fetchloom_engine::credential::{Credential, Necessity};
use fetchloom_engine::erased::{Adapters, AnySource};
use fetchloom_engine::error::{Error, ErrorKind};
use fetchloom_engine::flights::Flights;
use fetchloom_engine::limits::Limits;
use fetchloom_engine::reference::Host;
use fetchloom_engine::seam::policy::Policy;
use fetchloom_engine::seam::source::Serves;
use fetchloom_engine::work::WorkCounter;
use fetchloom_platform::NativePlatform;
use fetchloom_sources::{HttpSource, ObjectStoreSource};
use std::sync::Arc;

/// Returns the adapters a run dispatches to, asked in the order they are given.
#[must_use]
pub fn adapters_for(work: &Arc<WorkCounter>, limits: &Limits) -> Adapters {
    Adapters::new(vec![
        AnySource::new(fetchloom_sources::HuggingFaceSource::new(
            *limits,
            Arc::clone(work),
        )),
        AnySource::new(fetchloom_sources::ZenodoSource::new(
            *limits,
            Arc::clone(work),
        )),
        AnySource::new(ObjectStoreSource::new(*limits, Arc::clone(work))),
        AnySource::new(HttpSource::new(*limits, Arc::clone(work))),
    ])
}

/// Resolves the credential a transfer to a host may send, without requiring
/// one, because a source that serves the bytes without one needs none.
///
/// # Errors
///
/// Fails when the credential store is present and cannot be read.
pub(super) fn resolve_credential(
    policy: &dyn Policy,
    host: &str,
) -> Result<Option<Credential>, Error> {
    policy.credential(&Host::new(host.to_owned()), Necessity::Optional)
}

/// Turns a source's refusal for want of authorization into the policy failure
/// that names the provider and prints its setup steps.
///
/// # Errors
///
/// Returns the failure it was given, or the policy's required-credential
/// failure when the source refused for want of one.
pub(super) fn required_credential(policy: &dyn Policy, host: &str, failure: Error) -> Error {
    if failure.kind() != ErrorKind::PolicyCredentialMissing {
        return failure;
    }
    match policy.credential(&Host::new(host.to_owned()), Necessity::Required) {
        Err(named) => named,
        Ok(_) => failure,
    }
}

/// What bounds a run's transfers, and whether measurement may move them.
#[derive(Clone, Copy, Debug)]
pub struct Tuning {
    /// The two ceilings no measurement may push a run past.
    pub ceilings: fetchloom_engine::tuning::Ceilings,
    /// Whether a run's decisions may move with what it measures.
    pub adapts: bool,
    /// The ceiling on how fast the run may transfer.
    pub bandwidth: Option<fetchloom_engine::limits::Bandwidth>,
}

impl Tuning {
    /// Returns the controller a run starts a host at: what the cache recorded
    /// for that host, a count that never moves when the run was told not to
    /// adapt, and one transfer at a time for an artifact no host serves.
    #[must_use]
    pub fn controller(
        &self,
        host: &str,
        cache: Option<&Cache<NativePlatform>>,
    ) -> fetchloom_engine::tuning::Controller {
        if host.is_empty() {
            return fetchloom_engine::tuning::Controller::fixed(std::num::NonZeroU32::MIN);
        }
        if !self.adapts {
            return fetchloom_engine::tuning::Controller::fixed(self.ceilings.per_host);
        }
        let recorded = cache
            .and_then(|cache| cache.measurement(host))
            .map(|found| found.concurrency);
        fetchloom_engine::tuning::Controller::start(recorded, self.ceilings.per_host)
    }

    /// Returns the meter a run's transfers share, when a rate was set.
    #[must_use]
    pub fn meter(&self) -> Option<fetchloom_engine::tuning::Meter> {
        self.bandwidth.map(fetchloom_engine::tuning::Meter::new)
    }
}

/// Records what a transfer learned about the host it ran against, so the next
/// run starts where this one finished.
pub(super) fn record_measurement(
    cache: &Cache<NativePlatform>,
    with: &Materialization<'_>,
    host: &str,
    flights: &Flights<'_>,
    moved: u64,
    took: std::time::Duration,
) {
    if !with.tuning.adapts || host.is_empty() {
        return;
    }
    let held = flights.controller(host);
    let permitted = {
        let mut controller = held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        controller.delivered(moved, took);
        controller.permitted()
    };
    let nanos = u64::try_from(took.as_nanos()).unwrap_or(u64::MAX);
    let throughput = moved
        .saturating_mul(1_000_000_000)
        .checked_div(nanos)
        .unwrap_or_default();
    let _ = cache.record_measurement(
        host,
        &fetchloom_engine::tuning::HostMeasurement {
            concurrency: permitted,
            throughput,
            time_to_first_byte_ms: u64::try_from(took.as_millis()).unwrap_or(u64::MAX),
            observed_at: fetchloom_engine::timestamp::Timestamp::now(),
        },
    );
}

/// Returns the resolver a transfer finds each host's own credential through, so
/// that a transfer moving to a second host authenticates against that host.
pub(super) fn credentials_for(
    policy: &dyn Policy,
) -> impl Fn(&str) -> Result<Option<Credential>, Error> + Sync + '_ {
    move |host: &str| resolve_credential(policy, host)
}

/// Returns the hook a transfer offers an optional credential through, which
/// names the provider from the host and lets the policy decide whether the
/// saving is worth interrupting for.
pub(super) fn offers_for(policy: &dyn Policy) -> impl Fn(&str, std::time::Duration) + Sync + '_ {
    move |host: &str, saved: std::time::Duration| {
        let help = fetchloom_sources::help_for(host, Necessity::Optional);
        let _ = policy.offer_credential(&help, saved);
    }
}

/// Returns the host a location names, which is what a measurement, a credential
/// and an in-flight count are all filed under.
#[must_use]
pub fn host_of(location: &str) -> String {
    Host::of_location(location).as_str().to_owned()
}

/// Reports whether an adapter serves the reference, so that a run fetches it
/// rather than reading it as a path on this machine.
#[must_use]
pub fn is_served(adapters: &Adapters, reference: &str) -> bool {
    adapters.serving(reference).is_some()
}

/// Reports whether the adapter serving a reference serves it as a container to
/// be listed rather than as one object.
#[must_use]
pub fn is_container(adapters: &Adapters, reference: &str) -> bool {
    matches!(adapters.serving(reference), Some((_, Serves::Container)))
}

pub(super) fn unserved(reference: &str) -> Error {
    Error::new(
        ErrorKind::ReferenceUnresolved,
        format!(
            "name a location an adapter of this build serves, because nothing here serves {reference}"
        ),
    )
}

/// Names the documented reference form a reference is in, when it is one this
/// build does not resolve.
pub(super) fn unbuilt_form(reference: &str) -> Option<&'static str> {
    if reference.starts_with("blake3:") || reference.starts_with("sha256:") {
        return Some("a content address");
    }
    if let Some((scheme, rest)) = reference.split_once(':')
        && !rest.is_empty()
        && !scheme.is_empty()
        && scheme.chars().all(|letter| letter.is_ascii_lowercase())
        && !std::path::Path::new(reference).exists()
    {
        return Some("a provider identifier");
    }
    if !reference.contains('/')
        && !reference.contains('\\')
        && !reference.contains('.')
        && !std::path::Path::new(reference).exists()
    {
        return Some("a dataset name");
    }
    None
}
