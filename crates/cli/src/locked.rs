//! Reading the lock a run is held to, and writing the one it produced.

use std::collections::BTreeMap;
use std::path::Path;

use fetchloom_engine::digest::TreeDigest;
use fetchloom_engine::error::{Error, ErrorKind};
use fetchloom_engine::event::{Event, EventPayload, Sequence};
use fetchloom_engine::limits::Limits;
use fetchloom_engine::lock::{Lock, LockedArtifact, LockedDataset};
use fetchloom_engine::manifest::Manifest;
use fetchloom_engine::seam::observer::Observer;

use crate::run::ResolvedArtifact;

/// Returns what the lock pins for a dataset.
///
/// # Errors
///
/// Fails with `policy.trust_refused` when the run is locked and the lock states
/// nothing about this dataset. Fails when the lock cannot be read.
pub fn pinned(
    path: &Path,
    dataset: &str,
    requires: Option<Requirement>,
) -> Result<Option<LockedDataset>, Error> {
    let held = Lock::read(path, &Limits::default())?;
    let entry = held.datasets.get(dataset).cloned();
    if let Some(requirement) = requires
        && entry.is_none()
    {
        return Err(Error::new(
            ErrorKind::PolicyTrustRefused,
            format!(
                "{}, because {} pins nothing for {dataset} and {}",
                requirement.remedy(),
                path.display(),
                requirement.because()
            ),
        )
        .with_dataset(dataset.to_owned()));
    }
    Ok(entry)
}

/// Why a run needs the lock to already pin a dataset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Requirement {
    /// The run was given `--locked`.
    LockedRun,
    /// The run is a plan, which states resolved digests and moves no bytes to
    /// learn one.
    Plan,
}

impl Requirement {
    /// Returns what the reader should do next.
    fn remedy(self) -> &'static str {
        match self {
            Self::LockedRun => "run once without --locked to record what it resolves to",
            Self::Plan => "run get once to record what it resolves to",
        }
    }

    /// Returns why the lock had to pin it already.
    fn because(self) -> &'static str {
        match self {
            Self::LockedRun => "a locked run never accepts a first use",
            Self::Plan => "a plan states resolved digests and moves no bytes to learn one",
        }
    }
}

/// Returns what a run resolved, in the form a lock pins it.
///
/// # Errors
///
/// Fails when the manifest digest cannot be taken.
pub fn resolved(
    manifest: &Manifest,
    artifacts: &[ResolvedArtifact],
    tree: Option<TreeDigest>,
) -> Result<Option<LockedDataset>, Error> {
    if artifacts.is_empty() {
        return Ok(None);
    }
    let mut pinned = BTreeMap::new();
    for artifact in artifacts {
        pinned.insert(
            artifact.id.clone(),
            LockedArtifact {
                digest: artifact.digest,
                interop: artifact.interop,
                size: artifact.size,
                select: artifact.selection.include.clone(),
                layout: artifact.selection.layout,
            },
        );
    }
    Ok(Some(LockedDataset {
        manifest: manifest.digest()?,
        release: manifest.release.clone(),
        artifacts: pinned,
        tree,
    }))
}

/// Holds a locked run to the lock, or records what an unlocked run resolved.
///
/// # Errors
///
/// Fails with whatever the comparison found, and when the lock cannot be
/// written.
pub fn settle(
    path: &Path,
    dataset: &str,
    pinned: Option<&LockedDataset>,
    resolved: Option<&LockedDataset>,
    locked: bool,
    observer: &dyn Observer,
    sequence: &Sequence,
) -> Result<(), Error> {
    let Some(resolved) = resolved else {
        observer.emit(&Event::new(
            sequence,
            EventPayload::Degrade {
                requested: format!("a lock pinning what {dataset} resolves to"),
                used: "no lock entry at all".to_owned(),
                reason: "this reference names a directory, which resolves to no object, and a \
                         lock pins objects rather than trees"
                    .to_owned(),
            },
        ));
        return Ok(());
    };
    if locked {
        return match pinned {
            Some(pinned) => pinned.check(resolved),
            None => Ok(()),
        };
    }
    let mut held = Lock::read(path, &Limits::default())?;
    held.datasets.insert(dataset.to_owned(), resolved.clone());
    held.write(path)
}
