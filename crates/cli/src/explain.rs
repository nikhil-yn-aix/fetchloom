//! Reporting every effective setting and the level that supplied it.

use crate::config::Discovered;
use crate::settings::Settings;

/// One setting as `explain` reports it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Explained {
    /// The setting's name.
    pub key: String,
    /// Its effective value.
    pub value: String,
    /// The level that supplied it.
    pub origin: String,
}

/// Renders one setting's value for reporting.
trait Describe {
    fn describe(&self) -> String;
}

impl Describe for bool {
    fn describe(&self) -> String {
        self.to_string()
    }
}

impl Describe for u32 {
    fn describe(&self) -> String {
        self.to_string()
    }
}

impl Describe for std::path::PathBuf {
    fn describe(&self) -> String {
        self.display().to_string()
    }
}

impl Describe for crate::surface::DisplayMode {
    fn describe(&self) -> String {
        match self {
            Self::Plain => "plain".to_owned(),
            Self::Live => "live".to_owned(),
            Self::None => "none".to_owned(),
        }
    }
}

impl<T: Describe> Describe for Option<T> {
    fn describe(&self) -> String {
        self.as_ref()
            .map_or_else(|| "?".to_owned(), Describe::describe)
    }
}

/// Reports every effective setting with the level that supplied it.
#[must_use]
pub fn rows(settings: &Settings, measured: u32) -> Vec<Explained> {
    macro_rules! row {
        ($name:literal, $field:ident) => {
            Explained {
                key: $name.to_owned(),
                value: Describe::describe(&settings.$field.value),
                origin: settings.$field.origin.to_string(),
            }
        };
    }
    vec![
        row!("offline", offline),
        match settings.threads.value {
            Some(requested) => Explained {
                key: "threads".to_owned(),
                value: requested.to_string(),
                origin: settings.threads.origin.to_string(),
            },
            None => Explained {
                key: "threads".to_owned(),
                value: measured.to_string(),
                origin: "measured".to_owned(),
            },
        },
        row!("display", display),
        row!("cache.dir", cache_dir),
    ]
}

/// Describes where the configuration files came from.
#[must_use]
pub fn file_lines(discovered: &Discovered) -> Vec<String> {
    let project = discovered.project.as_ref().map_or_else(
        || "project config: none found".to_owned(),
        |loaded| format!("project config: {}", loaded.path.display()),
    );
    let user = discovered.user.as_ref().map_or_else(
        || "user config: none found".to_owned(),
        |loaded| format!("user config: {}", loaded.path.display()),
    );
    vec![project, user]
}

/// What one config level supplied.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct ConfigFile {
    /// The level's name.
    pub level: String,
    /// The path that was found, when one was.
    pub path: Option<String>,
}

/// Everything `explain` reports about a run's configuration.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Report {
    /// Where the configuration files were found, or that none were.
    pub files: Vec<ConfigFile>,
    /// Every effective setting.
    pub settings: Vec<Explained>,
}

/// Describes where the configuration files came from, as values.
#[must_use]
pub fn files(discovered: &Discovered) -> Vec<ConfigFile> {
    vec![
        ConfigFile {
            level: "project".to_owned(),
            path: discovered
                .project
                .as_ref()
                .map(|loaded| loaded.path.display().to_string()),
        },
        ConfigFile {
            level: "user".to_owned(),
            path: discovered
                .user
                .as_ref()
                .map(|loaded| loaded.path.display().to_string()),
        },
    ]
}
