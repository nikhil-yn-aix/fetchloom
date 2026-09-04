//! Configuration files and the five levels a setting can come from.

use std::fmt;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const PROJECT_FILE: &str = "fetchloom.toml";

pub const USER_FILE: &str = "config.toml";

pub const CONFIGURATION_DIRECTORY: &str = "fetchloom";

pub const WINDOWS_CONFIGURATION_DIRECTORY: &str = "Fetchloom";

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
    pub fn new(value: T, origin: Origin) -> Self {
        Self { value, origin }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct ConfigFile {
    pub offline: Option<bool>,
    pub threads: Option<NonZeroU32>,
    pub display: Option<String>,
    pub cache: Option<CacheSection>,
    pub concurrency: Option<NonZeroU32>,
    pub per_host: Option<NonZeroU32>,
    pub bandwidth: Option<String>,
    pub io: Option<String>,
    pub log: Option<String>,
    pub retries: Option<NonZeroU32>,
    pub timeout: Option<String>,
    pub sources: Option<Vec<String>>,
    pub color: Option<String>,
    pub hints: Option<bool>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct CacheSection {
    pub dir: Option<PathBuf>,
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
    pub path: PathBuf,
    pub values: ConfigFile,
}

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
