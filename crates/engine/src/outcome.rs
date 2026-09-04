//! Process exit codes and the layer each one reports.

use crate::error::Layer;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ExitCode {
    Success,
    Usage,
    Resolution,
    Network,
    Integrity,
    Policy,
    Resource,
    Destination,
    Archive,
    Cache,
    Cancelled,
}

impl ExitCode {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Materialized,
    Unchanged,
    Restored,
    Adopted,
}
