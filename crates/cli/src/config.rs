//! Configuration files and the five levels a setting can come from.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The name of a project configuration file.
pub const PROJECT_FILE: &str = "fetchloom.toml";

/// The name of a user configuration file inside the configuration location.
pub const USER_FILE: &str = "config.toml";

/// The directory name Fetchloom uses inside the platform's configuration
/// location.
pub const CONFIGURATION_DIRECTORY: &str = "fetchloom";

/// The directory name Fetchloom uses on Windows, where the convention is
/// capitalized.
pub const WINDOWS_CONFIGURATION_DIRECTORY: &str = "Fetchloom";

/// Which of the five precedence levels supplied a value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// The command line, which wins over every other level.
    CommandLine,
    /// An environment variable.
    Environment,
    /// The project configuration file.
    ProjectConfig,
    /// The user configuration file.
    UserConfig,
    /// The value built into the binary.
    Default,
}

impl Origin {
    /// Returns the name this origin is reported with.
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

/// A value together with the level that supplied it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Sourced<T> {
    /// The effective value.
    pub value: T,
    /// The level that supplied it.
    pub origin: Origin,
}

impl<T> Sourced<T> {
    /// Pairs a value with the level that supplied it.
    #[must_use]
    pub fn new(value: T, origin: Origin) -> Self {
        Self { value, origin }
    }
}

/// Everything a configuration file may set.
///
/// Only settings this build acts on are accepted. Unknown keys, and the `x-`
/// prefix, are an error.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct ConfigFile {
    /// Forbid all network activity.
    pub offline: Option<bool>,
    /// Ceiling on threads used for processor work.
    pub threads: Option<u32>,
    /// Progress presentation.
    pub display: Option<String>,
    /// Where the cache is.
    pub cache: Option<CacheSection>,
}

/// Everything the cache table of a configuration file may set.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct CacheSection {
    /// Where the cache is. Relative in a project file.
    pub dir: Option<PathBuf>,
}

/// Why a configuration file could not be used.
#[derive(Debug)]
pub enum ConfigError {
    /// The named file could not be read.
    Unreadable {
        /// The file that could not be read.
        path: PathBuf,
        /// What the platform reported.
        reason: std::io::Error,
    },
    /// The file was not valid configuration.
    Malformed {
        /// The file that did not parse.
        path: PathBuf,
        /// What the parser reported.
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

/// A configuration file and where it was found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadedConfig {
    /// The file that was read.
    pub path: PathBuf,
    /// What it set.
    pub values: ConfigFile,
}

/// Reads one configuration file.
///
/// Takes the path of a file that is expected to exist. Returns what it set.
/// Fails when the file cannot be read and when it sets a key that is not
/// defined, the reserved prefix among them.
///
/// # Errors
///
/// Returns the path and the reason.
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

/// Searches a directory and then each parent for a project configuration file.
///
/// Takes the directory to start from. Returns the first file found, and nothing
/// when the filesystem root is reached without one.
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

/// Returns the platform's configuration location for Fetchloom.
///
/// This is the configuration location and never the cache location; the two are
/// separate directories. Returns nothing when the platform's own variable is
/// unset and no home directory is known.
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

/// The two configuration files a run read, in precedence order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Discovered {
    /// The project file, when one was found.
    pub project: Option<LoadedConfig>,
    /// The user file, when one was found.
    pub user: Option<LoadedConfig>,
}

/// Finds and reads the configuration files a run should use.
///
/// Takes the directory to search from, an explicitly named file, and whether
/// configuration files are disabled entirely. A named file disables the search
/// and becomes the project level. Disabling configuration returns nothing at
/// either level.
///
/// # Errors
///
/// Fails when a file that was found cannot be read or does not parse.
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
