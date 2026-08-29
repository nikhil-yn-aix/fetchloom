//! Which members of an artifact a run takes, and where they land.

use serde::{Deserialize, Serialize};

/// A pattern matched against a canonical member path.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Glob(String);

impl Glob {
    /// Builds a pattern from its text.
    #[must_use]
    pub fn new(pattern: impl Into<String>) -> Self {
        Self(pattern.into())
    }

    /// Returns the pattern text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// How member paths are rewritten on the way to the destination.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Layout {
    /// Preserve archive paths.
    #[default]
    Keep,
    /// Drop the first n path components.
    Flatten(u32),
}

/// The include and exclude patterns that make selection part of identity.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Selection {
    /// Member paths to include. An empty list means every member.
    pub include: Vec<Glob>,
    /// Member paths to remove, applied after every include.
    pub exclude: Vec<Glob>,
    /// How the selected paths are rewritten.
    pub layout: Layout,
}
