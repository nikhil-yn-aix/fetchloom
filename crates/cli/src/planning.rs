//! Turning a lock into a plan, and a plan back into a run.

use std::path::Path;

use fetchloom_cache::Cache;
use fetchloom_engine::digest::ContentDigest;
use fetchloom_engine::error::{Error, ErrorKind};
use fetchloom_engine::limits::Limits;
use fetchloom_engine::lock::LockedDataset;
use fetchloom_engine::plan::{Plan, PlanArtifact, PlanDisk, PlanNetwork, VolumeRequirement};
use fetchloom_engine::redact::SafeUrl;
use fetchloom_engine::reference::Host;
use fetchloom_engine::seam::store::Store;
use fetchloom_engine::trust::TrustClass;
use fetchloom_platform::NativePlatform;

/// The field a plan reports as unknown when no source states the length an
/// artifact expands to.
const EXPANDED: &str = "expanded";

/// The staging requirement, which is the expanded length and is unknown with
/// it.
const STAGING: &str = "staging";

/// The destination requirement, which is the expanded length and is unknown
/// with it.
const DESTINATION: &str = "destination";

/// The cost, which no source in this build states.
const COST: &str = "cost";

/// Builds the plan a reference resolves to, moving no bytes.
///
/// # Errors
///
/// Fails with `policy.trust_refused` when the lock pins nothing for the
/// dataset.
pub fn build(
    pinned: Option<&LockedDataset>,
    dataset: &str,
    reference: &str,
    destination: &Path,
    cache: Option<&Cache<NativePlatform>>,
) -> Result<Plan, Error> {
    let Some(pinned) = pinned else {
        return Err(Error::new(
            ErrorKind::PolicyTrustRefused,
            format!(
                "run get once to record what {dataset} resolves to, because a plan states \
                 resolved digests and the lock pins none for it"
            ),
        )
        .with_dataset(dataset.to_owned()));
    };

    let mut artifacts = Vec::new();
    let mut required = false;
    for (id, locked) in &pinned.artifacts {
        let cached = match cache {
            Some(cache) => cache.contains(locked.digest)?,
            None => false,
        };
        required = required || !cached;
        artifacts.push(PlanArtifact {
            id: id.clone(),
            digest: locked.digest,
            size: locked.size,
            expanded: None,
            cached,
            source: SafeUrl::new(reference),
            select: locked.select.clone(),
            layout: locked.layout,
            cost: None,
        });
    }

    let bytes: u64 = artifacts
        .iter()
        .filter(|artifact| !artifact.cached)
        .map(|artifact| artifact.size)
        .sum();
    let cache_volume = cache.map_or_else(
        || volume_of(destination),
        |cache| volume_of(cache.layout().root()),
    );
    Ok(Plan {
        dataset: dataset.to_owned(),
        release: pinned.release.clone(),
        network: PlanNetwork {
            hosts: host_of(reference).into_iter().collect(),
            required,
        },
        artifacts,
        trust: TrustClass::Verified,
        credentials: Vec::new(),
        terms: Vec::new(),
        disk: PlanDisk {
            partial: VolumeRequirement {
                volume: cache_volume.clone(),
                bytes: Some(bytes),
            },
            cache: VolumeRequirement {
                volume: cache_volume.clone(),
                bytes: Some(bytes),
            },
            staging: VolumeRequirement {
                volume: cache_volume,
                bytes: None,
            },
            destination: VolumeRequirement {
                volume: volume_of(destination),
                bytes: None,
            },
        },
        destination: destination.to_path_buf(),
        conflicts: conflicts(destination),
        unknown: vec![
            EXPANDED.to_owned(),
            STAGING.to_owned(),
            DESTINATION.to_owned(),
            COST.to_owned(),
        ],
    })
}

/// Reads a plan from a file.
///
/// # Errors
///
/// Fails when the file cannot be read and when it does not parse.
pub fn read(path: &Path, limits: &Limits) -> Result<Plan, Error> {
    let bytes = std::fs::read(path).map_err(|reason| {
        Error::new(
            ErrorKind::ReferenceUnresolved,
            format!("make {} readable: {reason}", path.display()),
        )
    })?;
    let syntax = fetchloom_engine::document::Syntax::of_path(path)
        .unwrap_or(fetchloom_engine::document::Syntax::Yaml);
    Plan::parse(&bytes, syntax, limits)
}

/// Returns the digest a plan's one artifact records.
///
/// # Errors
///
/// Fails when the plan names no artifact.
pub fn only_artifact(plan: &Plan) -> Result<&PlanArtifact, Error> {
    plan.artifacts.first().ok_or_else(|| {
        Error::new(
            ErrorKind::ReferenceUnresolved,
            "plan a reference that resolves to something, because this plan names no artifact",
        )
    })
}

/// Returns the digest a plan pins for its one artifact.
#[must_use]
pub fn pinned_digest(plan: &Plan) -> Option<ContentDigest> {
    plan.artifacts.first().map(|artifact| artifact.digest)
}

fn conflicts(destination: &Path) -> Vec<String> {
    if destination.exists() {
        vec![destination.display().to_string()]
    } else {
        Vec::new()
    }
}

/// Returns the name of the volume a path sits on.
fn volume_of(path: &Path) -> String {
    let mut components = path.components();
    match components.next() {
        Some(std::path::Component::Prefix(prefix)) => {
            prefix.as_os_str().to_string_lossy().into_owned()
        }
        Some(std::path::Component::RootDir) => "/".to_owned(),
        _ => ".".to_owned(),
    }
}

fn host_of(reference: &str) -> Option<Host> {
    let after = reference.split_once("://")?.1;
    let authority = after.split(['/', '?', '#']).next()?;
    let host = authority.rsplit('@').next()?;
    let host = host.split(':').next()?;
    if host.is_empty() {
        None
    } else {
        Some(Host::new(host))
    }
}
