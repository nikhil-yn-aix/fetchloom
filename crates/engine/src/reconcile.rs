//! What a locked run found at each destination entry.

use serde::{Deserialize, Serialize};

/// The state one destination entry was found in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconcileOutcome {
    /// The entry matches the receipt.
    Unchanged,
    /// The entry was missing and was materialized from the cache.
    Restored,
    /// The entry differs from the receipt.
    Modified,
    /// The entry is present and is not in the receipt.
    Foreign,
}
