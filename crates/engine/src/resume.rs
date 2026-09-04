//! Which evidence a resumed transfer stands on.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResumeRung {
    Outboard,
    ImmutableIdentity,
    StrongValidator,
    WeakValidator,
    NoValidator,
}

impl ResumeRung {
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
