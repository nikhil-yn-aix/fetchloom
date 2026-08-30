//! Which evidence a resumed transfer stands on.

use serde::{Deserialize, Serialize};

/// The rung of the resume ladder a transfer used.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResumeRung {
    /// An outboard tree is known for the expected digest. The bytes on disk are
    /// verified by range and the transfer resumes from the first bad or
    /// missing chunk.
    Outboard,
    /// The source exposes an immutable content address or version identity.
    ImmutableIdentity,
    /// A strong validator is unchanged.
    StrongValidator,
    /// Only a weak validator is available. A mismatch quarantines.
    WeakValidator,
    /// No validator is available. The transfer restarts from zero.
    NoValidator,
}

impl ResumeRung {
    /// Returns the number this rung is reported as.
    #[must_use]
    pub fn number(self) -> u8 {
        match self {
            Self::Outboard => 1,
            Self::ImmutableIdentity => 2,
            Self::StrongValidator => 3,
            Self::WeakValidator => 4,
            Self::NoValidator => 5,
        }
    }
}
