//! Shared helpers for the command line contract tests.

#![expect(
    dead_code,
    reason = "test helpers, where each test binary uses a different part"
)]

use std::collections::HashMap;

use fetchloom_cli::settings::Environment;

/// An environment a test writes, so no test reads the real process environment.
#[derive(Default)]
pub struct FakeEnvironment(HashMap<String, String>);

impl FakeEnvironment {
    /// Builds an environment holding exactly these names.
    pub fn with(pairs: &[(&str, &str)]) -> Self {
        Self(
            pairs
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                .collect(),
        )
    }
}

impl Environment for FakeEnvironment {
    fn get(&self, name: &str) -> Option<String> {
        self.0.get(name).cloned()
    }
}
