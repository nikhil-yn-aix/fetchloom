//! Configuration files and the five levels a setting can come from.

use std::fmt;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const PROJECT_FILE: &str = "fetchloom.toml";

const USER_FILE: &str = "config.toml";

const CONFIGURATION_DIRECTORY: &str = "fetchloom";

const WINDOWS_CONFIGURATION_DIRECTORY: &str = "Fetchloom";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    CommandLine,
    Environment,
    ProjectConfig,
    UserConfig,
    Default,
}

impl Origin {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::CommandLine => "command line",
            Self::Environment => "environment",
            Self::ProjectConfig => "project config",
            Self::UserConfig => "user config",
            Self::Default => "default",
        }
    }
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Sourced<T> {
    pub value: T,
    pub origin: Origin,
}

impl<T> Sourced<T> {
    #[must_use]
    pub(crate) fn new(value: T, origin: Origin) -> Self {
        Self { value, origin }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub(crate) struct ConfigFile {
    pub(crate) offline: Option<bool>,
    pub(crate) threads: Option<NonZeroU32>,
    pub(crate) display: Option<String>,
    pub(crate) cache: Option<CacheSection>,
    pub(crate) concurrency: Option<NonZeroU32>,
    pub(crate) per_host: Option<NonZeroU32>,
    pub(crate) bandwidth: Option<String>,
    pub(crate) io: Option<String>,
    pub(crate) log: Option<String>,
    pub(crate) retries: Option<NonZeroU32>,
    pub(crate) timeout: Option<String>,
    pub(crate) sources: Option<Vec<String>>,
    pub(crate) color: Option<String>,
    pub(crate) hints: Option<bool>,
    pub(crate) compress: Option<String>,
    pub(crate) library: Option<LibrarySection>,
    pub(crate) datasets: Option<std::collections::BTreeMap<String, crate::project::DatasetEntry>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub(crate) struct LibrarySection {
    pub(crate) dir: Option<PathBuf>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub(crate) struct CacheSection {
    pub(crate) dir: Option<PathBuf>,
}

#[derive(Debug)]
pub enum ConfigError {
    Unreadable {
        path: PathBuf,
        reason: std::io::Error,
    },
    Malformed {
        path: PathBuf,
        reason: Box<toml::de::Error>,
    },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreadable { path, reason } => {
                write!(f, "could not read {}: {reason}", path.display())
            }
            Self::Malformed { path, reason } => {
                write!(f, "{} is not valid configuration: {reason}", path.display())
            }
        }
    }
}

impl std::error::Error for ConfigError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadedConfig {
    pub(crate) path: PathBuf,
    pub(crate) values: ConfigFile,
}

impl LoadedConfig {
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub fn beside(&self) -> &Path {
        self.path.parent().unwrap_or(Path::new("."))
    }

    #[must_use]
    pub fn datasets(&self) -> std::collections::BTreeMap<String, crate::project::DatasetEntry> {
        self.values.datasets.clone().unwrap_or_default()
    }
}

/// # Errors
/// `ConfigError::Unreadable` when the file cannot be read, and
/// `ConfigError::Malformed` when it is not TOML or holds a key this build
/// does not know.
pub fn read(path: &Path) -> Result<LoadedConfig, ConfigError> {
    let text = std::fs::read_to_string(path).map_err(|reason| ConfigError::Unreadable {
        path: path.to_path_buf(),
        reason,
    })?;
    let values = toml::from_str(&text).map_err(|reason| ConfigError::Malformed {
        path: path.to_path_buf(),
        reason: Box::new(reason),
    })?;
    Ok(LoadedConfig {
        path: path.to_path_buf(),
        values,
    })
}

#[must_use]
pub fn find_project_file(start: &Path) -> Option<PathBuf> {
    let mut directory = Some(start);
    while let Some(current) = directory {
        let candidate = current.join(PROJECT_FILE);
        if candidate.is_file() {
            return Some(candidate);
        }
        directory = current.parent();
    }
    None
}

#[must_use]
pub fn user_config_directory() -> Option<PathBuf> {
    if cfg!(windows) {
        std::env::var_os("APPDATA")
            .map(|base| Path::new(&base).join(WINDOWS_CONFIGURATION_DIRECTORY))
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(|base| Path::new(&base).join(CONFIGURATION_DIRECTORY))
            .or_else(|| {
                std::env::var_os("HOME").map(|home| {
                    Path::new(&home)
                        .join(".config")
                        .join(CONFIGURATION_DIRECTORY)
                })
            })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Discovered {
    pub project: Option<LoadedConfig>,
    pub user: Option<LoadedConfig>,
}

/// # Errors
/// The kinds `read` gives, for the file named on the command line or for the
/// project and user files found beside the working directory.
pub fn discover(
    working_directory: &Path,
    named: Option<&Path>,
    disabled: bool,
) -> Result<Discovered, ConfigError> {
    if disabled {
        return Ok(Discovered::default());
    }
    if let Some(named) = named {
        return Ok(Discovered {
            project: Some(read(named)?),
            user: None,
        });
    }
    let project = match find_project_file(working_directory) {
        Some(path) => Some(read(&path)?),
        None => None,
    };
    let user = match user_config_directory().map(|directory| directory.join(USER_FILE)) {
        Some(path) if path.is_file() => Some(read(&path)?),
        _ => None,
    };
    Ok(Discovered { project, user })
}
