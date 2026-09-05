//! What a user names on the command line.

use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Host(String);

impl Host {
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Host {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Host {
    #[must_use]
    pub fn of_location(location: &str) -> Self {
        let Some(after) = location.split_once("://") else {
            return Self(String::new());
        };
        let authority = after.1.split(['/', '?', '#']).next().unwrap_or_default();
        let host = authority.rsplit('@').next().unwrap_or_default();
        if let Some(literal) = host.strip_prefix('[') {
            return Self(
                literal
                    .split_once(']')
                    .map_or(literal, |(address, _)| address)
                    .to_owned(),
            );
        }
        Self(host.split(':').next().unwrap_or_default().to_owned())
    }
}
