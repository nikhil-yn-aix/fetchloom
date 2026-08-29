//! How far a write is pushed before an object is published.

use serde::{Deserialize, Serialize};

/// The flushing a publication performs before its rename.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum DurabilityTier {
    /// Flush the file and its containing directory to the device.
    Strict,
    /// Flush the file.
    #[default]
    Normal,
    /// Flush nothing and rely on the atomic rename alone.
    Fast,
}
