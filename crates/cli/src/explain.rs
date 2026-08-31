//! Reporting every effective setting and the level that supplied it.

use crate::config::Discovered;
use crate::settings::Settings;

/// What a run measured for a setting no level supplied.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Measurement {
    /// What the measurement found.
    pub found: String,
    /// When it was taken.
    pub taken: String,
}

/// One setting as `explain` reports it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Explained {
    /// The setting's name.
    pub key: String,
    /// Its effective value.
    pub value: String,
    /// The level that supplied it, or `measured`.
    pub origin: String,
    /// What was measured, when the origin is `measured`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub measurement: Option<Measurement>,
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

impl Describe for std::num::NonZeroU32 {
    fn describe(&self) -> String {
        self.to_string()
    }
}

impl Describe for fetchloom_engine::limits::Bandwidth {
    fn describe(&self) -> String {
        format!("{} bytes per second", self.bytes_per_second())
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

impl Describe for crate::surface::IoChoice {
    fn describe(&self) -> String {
        match self {
            Self::Auto => "auto".to_owned(),
            Self::Buffered => "buffered".to_owned(),
            Self::Uncached => "uncached".to_owned(),
        }
    }
}

impl<T: Describe> Describe for Option<T> {
    fn describe(&self) -> String {
        self.as_ref()
            .map_or_else(|| "?".to_owned(), Describe::describe)
    }
}

/// What this machine measured for the settings no level supplies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Measured {
    /// The threads the platform detected, after every limit.
    pub threads: u32,
    /// The transfers in flight a run would use across every host.
    pub concurrency: u32,
    /// The transfers in flight a run would use for one host.
    pub per_host: u32,
    /// What the per-host measurement cache holds, host by host.
    pub recorded: Vec<String>,
}

/// Reports every effective setting with the level that supplied it.
#[must_use]
pub fn rows(settings: &Settings, measured: &Measured) -> Vec<Explained> {
    macro_rules! row {
        ($name:literal, $field:ident) => {
            Explained {
                key: $name.to_owned(),
                value: Describe::describe(&settings.$field.value),
                origin: settings.$field.origin.to_string(),
                measurement: None,
            }
        };
    }
    /// Reports a ceiling as the level that named it, or as what was measured.
    fn ceiling(
        key: &str,
        supplied: crate::config::Sourced<Option<std::num::NonZeroU32>>,
        found: u32,
        taken: &str,
    ) -> Explained {
        match supplied.value {
            Some(requested) => Explained {
                key: key.to_owned(),
                value: requested.to_string(),
                origin: supplied.origin.to_string(),
                measurement: None,
            },
            None => Explained {
                key: key.to_owned(),
                value: found.to_string(),
                origin: "measured".to_owned(),
                measurement: Some(Measurement {
                    found: found.to_string(),
                    taken: taken.to_owned(),
                }),
            },
        }
    }
    vec![
        row!("offline", offline),
        threads_row(settings, measured),
        row!("display", display),
        row!("cache.dir", cache_dir),
        ceiling(
            "concurrency",
            settings.concurrency,
            measured.concurrency,
            "this run, from the thread budget and the politeness ceiling",
        ),
        ceiling(
            "per-host",
            settings.per_host,
            measured.per_host,
            &per_host_taken(measured),
        ),
        row!("bandwidth", bandwidth),
        row!("io", io),
        row!("aggressive", aggressive),
        row!("deterministic-io", deterministic_io),
    ]
}

/// Describes when the per-host measurement was taken, host by host.
fn per_host_taken(measured: &Measured) -> String {
    if measured.recorded.is_empty() {
        return "this run, from the politeness ceiling, because no host has a recorded measurement"
            .to_owned();
    }
    format!("recorded per host: {}", measured.recorded.join(", "))
}

/// Reports the thread ceiling, saying when a requested one was clamped.
fn threads_row(settings: &Settings, measured: &Measured) -> Explained {
    let Some(requested) = settings.threads.value else {
        return Explained {
            key: "threads".to_owned(),
            value: measured.threads.to_string(),
            origin: "measured".to_owned(),
            measurement: Some(Measurement {
                found: measured.threads.to_string(),
                taken: "this run, from the platform".to_owned(),
            }),
        };
    };
    if requested.get() > measured.threads {
        return Explained {
            key: "threads".to_owned(),
            value: measured.threads.to_string(),
            origin: format!(
                "{}, clamped from {requested} to the {} this machine detected",
                settings.threads.origin, measured.threads
            ),
            measurement: Some(Measurement {
                found: measured.threads.to_string(),
                taken: "this run, from the platform".to_owned(),
            }),
        };
    }
    Explained {
        key: "threads".to_owned(),
        value: requested.to_string(),
        origin: settings.threads.origin.to_string(),
        measurement: None,
    }
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
