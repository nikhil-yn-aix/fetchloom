//! Which adapter serves a reference, which credential reaches a host, and what bounds the transfers.

use super::context::Materialization;
use fetchloom_cache::Cache;
use fetchloom_engine::adapters::{Adapters, AnySource};
use fetchloom_engine::credential::{Credential, Necessity};
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

#[must_use]
pub fn adapters_for(work: &Arc<WorkCounter>, limits: &Limits) -> Adapters {
    Adapters::new(
        vec![
            AnySource::new(fetchloom_sources::HuggingFaceSource::new(
                *limits,
                Arc::clone(work),
            )),
            AnySource::new(fetchloom_sources::ZenodoSource::new(
                *limits,
                Arc::clone(work),
            )),
        ]
        .into_iter()
        .chain(
            fetchloom_sources::Provider::ALL
                .into_iter()
                .map(|provider| {
                    AnySource::new(fetchloom_sources::described(
                        provider,
                        *limits,
                        Arc::clone(work),
                    ))
                }),
        )
        .chain([
            AnySource::new(fetchloom_sources::FtpSource::new(*limits, Arc::clone(work))),
            AnySource::new(ObjectStoreSource::new(*limits, Arc::clone(work))),
            AnySource::new(HttpSource::new(*limits, Arc::clone(work))),
        ])
        .collect(),
    )
}

pub(super) fn resolve_credential(
    policy: &dyn Policy,
    host: &str,
) -> Result<Option<Credential>, Error> {
    policy.credential(&Host::new(host.to_owned()), Necessity::Optional)
}

pub(super) fn required_credential(policy: &dyn Policy, host: &str, failure: Error) -> Error {
    if failure.kind() != ErrorKind::PolicyCredentialMissing {
        return failure;
    }
    match policy.credential(&Host::new(host.to_owned()), Necessity::Required) {
        Err(named) => named,
        Ok(_) => failure,
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Tuning {
    pub ceilings: fetchloom_engine::tuning::Ceilings,
    pub adapts: bool,
    pub bandwidth: Option<fetchloom_engine::limits::Bandwidth>,
}

impl Tuning {
    #[must_use]
    pub(crate) fn controller(
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

    #[must_use]
    pub fn meter(&self) -> Option<fetchloom_engine::tuning::Meter> {
        self.bandwidth.map(fetchloom_engine::tuning::Meter::new)
    }
}

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

pub(crate) fn credential_for(policy: &dyn Policy, host: &str) -> Result<Option<Credential>, Error> {
    resolve_credential(policy, host)
}

pub(super) fn credentials_for(
    policy: &dyn Policy,
) -> impl Fn(&str) -> Result<Option<Credential>, Error> + Sync + '_ {
    move |host: &str| resolve_credential(policy, host)
}

pub(super) fn offers_for(policy: &dyn Policy) -> impl Fn(&str, std::time::Duration) + Sync + '_ {
    move |host: &str, saved: std::time::Duration| {
        let help = fetchloom_sources::help_for(host, Necessity::Optional);
        let _ = policy.offer_credential(&help, saved);
    }
}

#[must_use]
pub(crate) fn host_of(location: &str) -> String {
    Host::of_location(location).as_str().to_owned()
}

#[must_use]
pub(crate) fn is_served(adapters: &Adapters, reference: &str) -> bool {
    adapters.serving(reference).is_some()
}

#[must_use]
pub(crate) fn is_container(adapters: &Adapters, reference: &str) -> bool {
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
