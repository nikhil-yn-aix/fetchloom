//! The machine configuration, `config.toml`: settings of one machine, user level or system
//! level, never shared through git.

use std::ops::Range;

use serde::Deserialize;

use crate::error::ErrorKind;
use crate::manifest::Link;
use crate::toml_error::explain_error;

/// A `config.toml` as read. Every key is optional, so layers can be merged key by key.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineConfig {
    /// The `[store]` table.
    #[serde(default)]
    pub store: StoreConfig,
    /// The `[network]` table.
    #[serde(default)]
    pub network: NetworkConfig,
}

/// The `[store]` table.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoreConfig {
    /// The local tier's directory, where a leading `~` stands for the home directory.
    pub local: Option<String>,
    /// How files are placed in projects.
    pub link: Option<Link>,
}

/// The `[network]` table.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkConfig {
    /// Transfers running at once.
    pub jobs: Option<usize>,
    /// Transfers running at once against one host.
    pub per_host: Option<usize>,
    /// The longest silence tolerated on a connection, in seconds.
    pub timeout: Option<u64>,
}

impl MachineConfig {
    /// Reads `config.toml` text. A leading byte order mark is accepted.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] for invalid TOML, an unknown key (naming the closest valid one),
    /// or a value of the wrong form.
    pub fn from_toml(text: &str) -> Result<Self, ConfigError> {
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        toml::from_str(text).map_err(|err| {
            let explained = explain_error(&err);
            ConfigError {
                message: explained.message,
                span: explained.span,
                help: explained.help,
            }
        })
    }
}

/// Why a `config.toml` could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct ConfigError {
    /// What is wrong, in one sentence.
    pub message: String,
    /// The byte range of the file it points at, when known.
    pub span: Option<Range<usize>>,
    /// What may fix it, such as the closest valid key.
    pub help: Option<String>,
}

impl ConfigError {
    /// The kind this error is reported as.
    #[must_use]
    pub const fn kind(&self) -> ErrorKind {
        ErrorKind::LocalInvalidConfig
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_documented_key_is_read() {
        let config = MachineConfig::from_toml(
            "\u{feff}[store]\nlocal = \"~/cache\"\nlink = \"copy\"\n[network]\njobs = 2\nper_host = 1\ntimeout = 9\n",
        )
        .unwrap();
        assert_eq!(config.store.local.as_deref(), Some("~/cache"));
        assert_eq!(config.store.link, Some(Link::Copy));
        assert_eq!(config.network.jobs, Some(2));
        assert_eq!(config.network.per_host, Some(1));
        assert_eq!(config.network.timeout, Some(9));
        assert_eq!(
            MachineConfig::from_toml("").unwrap(),
            MachineConfig::default()
        );
    }

    #[test]
    fn an_unknown_key_names_the_closest_and_points_at_it() {
        let text = "[network]\njbos = 2\n";
        let err = MachineConfig::from_toml(text).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::LocalInvalidConfig);
        assert_eq!(err.message, "unknown key `jbos`");
        assert_eq!(err.help.as_deref(), Some("the closest valid key is `jobs`"));
        assert_eq!(&text[err.span.unwrap()], "jbos");
    }
}
