//! What a cache hit is checked against before it is reused.

use serde::{Deserialize, Serialize};

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum VerificationPolicy {
    Always,
    #[default]
    Fingerprint,
    Never,
}
