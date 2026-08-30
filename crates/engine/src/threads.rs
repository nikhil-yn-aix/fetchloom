//! How many threads processor work is allowed to use.

use std::num::NonZeroUsize;

use serde::Serialize;

/// Where a thread budget's value came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetOrigin {
    /// The platform's own answer, after affinity, container, and job limits.
    Detected,
    /// A user ceiling at or below the detected value.
    Requested,
    /// A user ceiling above the detected value, clamped to it.
    Clamped,
}

/// The number of threads processor work may use, and why it is that number.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct ThreadBudget {
    threads: NonZeroUsize,
    detected: NonZeroUsize,
    origin: BudgetOrigin,
}

impl ThreadBudget {
    /// Resolves a budget from what the platform detected and what the user
    /// asked for.
    ///
    /// Takes the detected count and an optional user ceiling. Returns the
    /// smaller of the two, together with the origin of the value.
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

    /// Returns the number of threads processor work may use.
    #[must_use]
    pub fn threads(self) -> NonZeroUsize {
        self.threads
    }

    /// Returns the count the platform detected.
    #[must_use]
    pub fn detected(self) -> NonZeroUsize {
        self.detected
    }

    /// Returns where the value came from.
    #[must_use]
    pub fn origin(self) -> BudgetOrigin {
        self.origin
    }
}
