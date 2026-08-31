//! What a locked run found at each destination entry.

use serde::{Deserialize, Serialize};

use crate::tree::{EntryPath, TreeEntry};

/// The state one destination entry was found in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconcileOutcome {
    /// The entry matches the resolved entry.
    Unchanged,
    /// The entry was missing and was materialized from the cache.
    Restored,
    /// The entry differs from the resolved entry.
    Modified,
    /// The entry is present and is not in the resolved tree.
    Foreign,
}

/// One path and the outcome a comparison against the resolved tree found for
/// it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reconciled {
    /// The path the outcome was decided for.
    pub path: EntryPath,
    /// What the comparison found.
    pub outcome: ReconcileOutcome,
}

/// Compares the tree a run resolved against what a destination currently holds.
#[must_use]
pub fn reconcile(resolved: &[TreeEntry], destination: &[TreeEntry]) -> Vec<Reconciled> {
    let mut found: Vec<Reconciled> = Vec::with_capacity(resolved.len() + destination.len());
    let mut named = std::collections::HashSet::with_capacity(resolved.len());

    for entry in resolved {
        let path = entry.path();
        named.insert(path.as_str());
        let outcome = match destination.iter().find(|holds| holds.path() == path) {
            None => ReconcileOutcome::Restored,
            Some(holds) if holds == entry => ReconcileOutcome::Unchanged,
            Some(_) => ReconcileOutcome::Modified,
        };
        found.push(Reconciled {
            path: path.clone(),
            outcome,
        });
    }

    for entry in destination {
        let path = entry.path();
        if !named.contains(path.as_str()) {
            found.push(Reconciled {
                path: path.clone(),
                outcome: ReconcileOutcome::Foreign,
            });
        }
    }

    found.sort_by(|left, right| left.path.as_bytes().cmp(right.path.as_bytes()));
    found
}
