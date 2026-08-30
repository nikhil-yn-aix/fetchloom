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
/// Takes the lock file, the dataset a run is about to resolve, and whether the
/// run may only do what the lock already states.
///
/// # Errors
///
/// Fails with `policy.trust_refused` when the run is locked and the lock states
/// nothing about this dataset. Fails when the lock cannot be read.
pub fn pinned(path: &Path, dataset: &str, locked: bool) -> Result<Option<LockedDataset>, Error> {
    let held = Lock::read(path, &Limits::default())?;
    let entry = held.datasets.get(dataset).cloned();
    if locked && entry.is_none() {
        return Err(Error::new(
            ErrorKind::PolicyTrustRefused,
            format!(
                "run once without --locked to record what {dataset} resolves to, because {} pins \
                 nothing for it and a locked run never accepts a first use",
                path.display()
            ),
        )
        .with_dataset(dataset.to_owned()));
    }
    Ok(entry)
}

/// Returns what a run resolved, in the form a lock pins it.
///
/// Takes the manifest the run resolved from, every artifact that verified, and
/// the tree the run materialized when it materialized one. Returns nothing when
/// the run verified no object. A run that failed part way pins what it verified
/// and no tree.
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
/// Takes the lock file, the dataset name, what the lock pins, what the run
/// resolved, and whether the run was locked. A locked run writes nothing.
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
