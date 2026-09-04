//! Reporting every effective setting and the level that supplied it.

use crate::config::Discovered;
use crate::settings::Settings;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Measurement {
    pub found: String,
    pub taken: String,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Explained {
    pub key: String,
    pub value: String,
    pub origin: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub measurement: Option<Measurement>,
}

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

impl Describe for crate::surface::ColorChoice {
    fn describe(&self) -> String {
        match self {
            Self::Auto => "auto".to_owned(),
            Self::Always => "always".to_owned(),
            Self::Never => "never".to_owned(),
        }
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

impl Describe for std::time::Duration {
    fn describe(&self) -> String {
        if self.subsec_millis() == 0 {
            format!("{}s", self.as_secs())
        } else {
            format!("{}ms", self.as_millis())
        }
    }
}

impl Describe for Vec<String> {
    fn describe(&self) -> String {
        if self.is_empty() {
            "none configured".to_owned()
        } else {
            self.join(", ")
        }
    }
}

impl<T: Describe> Describe for Option<T> {
    fn describe(&self) -> String {
        self.as_ref()
            .map_or_else(|| "?".to_owned(), Describe::describe)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Measured {
    pub threads: u32,
    pub concurrency: u32,
    pub per_host: u32,
    pub recorded: Vec<String>,
}

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
        log_row(settings),
        row!("retries", retries),
        row!("timeout", timeout),
        row!("sources", sources),
        row!("color", color),
        row!("hints", hints),
    ]
}

fn log_row(settings: &Settings) -> Explained {
    Explained {
        key: "log".to_owned(),
        value: settings.log.value.label().to_owned(),
        origin: if settings.log_clamped {
            format!(
                "{}, clamped to the highest level",
                settings.log.origin.label()
            )
        } else {
            settings.log.origin.to_string()
        },
        measurement: None,
    }
}

fn per_host_taken(measured: &Measured) -> String {
    if measured.recorded.is_empty() {
        return "this run, from the politeness ceiling, because no host has a recorded measurement"
            .to_owned();
    }
    format!("recorded per host: {}", measured.recorded.join(", "))
}

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

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct ConfigFile {
    pub level: String,
    pub path: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Report {
    pub files: Vec<ConfigFile>,
    pub settings: Vec<Explained>,
}

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
