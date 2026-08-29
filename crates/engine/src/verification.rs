//! What a cache hit is checked against before it is reused.

use serde::{Deserialize, Serialize};

/// The check applied to an object already present in the cache.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum VerificationPolicy {
    /// Reread and rehash the whole object.
    Always,
    /// Trust the object when its recorded filesystem fingerprint matches.
    #[default]
    Fingerprint,
    /// Trust the object unconditionally, which makes the result unverified.
    Never,
}
