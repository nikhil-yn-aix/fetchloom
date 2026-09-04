//! What a locked run found at each destination entry.

use serde::{Deserialize, Serialize};

use crate::tree::{EntryPath, TreeEntry};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconcileOutcome {
    Unchanged,
    Restored,
    Modified,
    Foreign,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reconciled {
    pub path: EntryPath,
    pub outcome: ReconcileOutcome,
}

#[must_use]
pub fn reconcile(resolved: &[TreeEntry], destination: &[TreeEntry]) -> Vec<Reconciled> {
    let mut found: Vec<Reconciled> = Vec::with_capacity(resolved.len() + destination.len());
    let mut named = std::collections::HashSet::with_capacity(resolved.len());
    let mut held: std::collections::HashMap<&str, &TreeEntry> =
        std::collections::HashMap::with_capacity(destination.len());
    for entry in destination {
        held.entry(entry.path().as_str()).or_insert(entry);
    }

    for entry in resolved {
        let path = entry.path();
        named.insert(path.as_str());
        let outcome = match held.get(path.as_str()).copied() {
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

    found.sort_by(|left, right| {
        crate::canonical::compare(left.path.as_bytes(), right.path.as_bytes())
    });
    found
}
