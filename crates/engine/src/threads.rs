//! How many threads processor work is allowed to use.

use std::num::NonZeroUsize;

use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetOrigin {
    Detected,
    Requested,
    Clamped,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct ThreadBudget {
    threads: NonZeroUsize,
    detected: NonZeroUsize,
    origin: BudgetOrigin,
}

impl ThreadBudget {
    #[must_use]
    pub fn resolve(detected: NonZeroUsize, requested: Option<NonZeroUsize>) -> Self {
        let (threads, origin) = match requested {
            None => (detected, BudgetOrigin::Detected),
            Some(requested) if requested <= detected => (requested, BudgetOrigin::Requested),
            Some(_) => (detected, BudgetOrigin::Clamped),
        };
        Self {
            threads,
            detected,
            origin,
        }
    }

    #[must_use]
    pub fn threads(self) -> NonZeroUsize {
        self.threads
    }

    #[must_use]
    pub fn detected(self) -> NonZeroUsize {
        self.detected
    }

    #[must_use]
    pub fn origin(self) -> BudgetOrigin {
        self.origin
    }
}
