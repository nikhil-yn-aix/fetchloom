//! What a manifest records about terms, and whether they were asserted.

use serde::{Deserialize, Serialize};

/// What a manifest records about the terms attached to a dataset.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct License {
    /// The identifier the publisher gave for the license.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spdx: Option<String>,
    /// Where the terms can be read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Whether a transfer is refused until acceptance is asserted.
    #[serde(default)]
    pub requires_acceptance: bool,
}

/// Whether the user asserted acceptance of the recorded terms.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Acceptance {
    /// The user asserted acceptance.
    Asserted,
    /// The user did not assert acceptance.
    Withheld,
}
