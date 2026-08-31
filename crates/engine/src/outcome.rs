//! Process exit codes and the layer each one reports.

use crate::error::Layer;

/// The code a run exits with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ExitCode {
    /// The run succeeded.
    Success,
    /// The command line was wrong.
    Usage,
    /// Resolution failed.
    Resolution,
    /// Network failure after retries.
    Network,
    /// Integrity mismatch.
    Integrity,
    /// Policy blocked the run.
    Policy,
    /// Insufficient disk or a resource limit was exceeded.
    Resource,
    /// Destination conflict.
    Destination,
    /// Unsafe archive content.
    Archive,
    /// Cache or lock contention failure.
    Cache,
    /// The run was cancelled.
    Cancelled,
}

impl ExitCode {
    /// Returns the number this code is reported to the shell as.
    #[must_use]
    pub fn code(self) -> i32 {
        match self {
            Self::Success => 0,
            Self::Usage => 2,
            Self::Resolution => 10,
            Self::Network => 20,
            Self::Integrity => 30,
            Self::Policy => 40,
            Self::Resource => 50,
            Self::Destination => 60,
            Self::Archive => 70,
            Self::Cache => 80,
            Self::Cancelled => 130,
        }
    }
}

impl From<Layer> for ExitCode {
    fn from(layer: Layer) -> Self {
        match layer {
            Layer::Resolve => Self::Resolution,
            Layer::Transfer => Self::Network,
            Layer::Verify => Self::Integrity,
            Layer::Extract => Self::Archive,
            Layer::Materialize => Self::Destination,
            Layer::Cache => Self::Cache,
            Layer::Policy => Self::Policy,
            Layer::Resource => Self::Resource,
        }
    }
}

/// What a run did to its destination.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    /// The destination did not exist and every selected entry was published.
    Materialized,
    /// The destination held every selected entry already, and nothing was
    /// written.
    Unchanged,
    /// The destination was missing entries and only those were written.
    Restored,
    /// `--adopt` was given, nothing was written, and the reported tree is the
    /// one the destination holds.
    Adopted,
}
