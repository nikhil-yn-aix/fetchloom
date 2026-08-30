//! Resolving every setting across the five precedence levels.

use std::path::{Path, PathBuf};

use crate::config::{ConfigFile, Discovered, Origin, Sourced};
use crate::surface::{DisplayMode, GlobalFlags};

/// Somewhere a value can be read from.
pub trait Environment: Send + Sync {
    /// Returns the value of a variable, or nothing when it is unset.
    fn get(&self, name: &str) -> Option<String>;
}

/// The real process environment.
#[derive(Clone, Copy, Debug, Default)]
pub struct ProcessEnvironment;

impl Environment for ProcessEnvironment {
    fn get(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }
}

/// Every effective setting, each carrying the level that supplied it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    /// Whether all network activity is forbidden.
    pub offline: Sourced<bool>,
    /// The ceiling on threads used for processor work.
    pub threads: Sourced<Option<u32>>,
    /// The progress presentation requested.
    pub display: Sourced<DisplayMode>,
    /// Where the cache is.
    pub cache_dir: Sourced<PathBuf>,
}

/// Returns the cache directory this platform puts a cache in by default.
///
/// Takes somewhere to read environment variables from. Returns the location
/// contracts.md names for this platform, and the working directory when the
/// platform names no home, which is the only place always writable.
#[must_use]
pub fn default_cache_dir(environment: &dyn Environment) -> PathBuf {
    #[cfg(windows)]
    {
        if let Some(local) = environment.get("LOCALAPPDATA") {
            return PathBuf::from(local).join("Fetchloom").join("Cache");
        }
    }
    #[cfg(unix)]
    {
        if let Some(base) = environment.get("XDG_CACHE_HOME") {
            return PathBuf::from(base).join("fetchloom");
        }
        if let Some(home) = environment.get("HOME") {
            return PathBuf::from(home).join(".cache").join("fetchloom");
        }
    }
    PathBuf::from(".fetchloom-cache")
}

/// Resolves the cache directory a project file names, against the directory
/// that file is in.
///
/// A project file names a cache beside itself, so its value is read against the
/// file rather than against the working directory.
fn project_cache_dir(loaded: &crate::config::LoadedConfig) -> Option<PathBuf> {
    let named = loaded.values.cache.as_ref()?.dir.as_ref()?;
    let beside = loaded.path.parent().unwrap_or(Path::new("."));
    Some(beside.join(named))
}

fn parse_bool(text: &str) -> Option<bool> {
    match text {
        "1" | "true" => Some(true),
        "0" | "false" => Some(false),
        _ => None,
    }
}

fn parse_display(text: &str) -> Option<DisplayMode> {
    match text {
        "plain" => Some(DisplayMode::Plain),
        "live" => Some(DisplayMode::Live),
        "none" => Some(DisplayMode::None),
        _ => None,
    }
}

struct Levels<'a> {
    project: Option<&'a ConfigFile>,
    user: Option<&'a ConfigFile>,
}

impl Levels<'_> {
    fn pick<T, F>(&self, read: F) -> Option<(T, Origin)>
    where
        F: Fn(&ConfigFile) -> Option<T>,
    {
        if let Some(value) = self.project.and_then(&read) {
            return Some((value, Origin::ProjectConfig));
        }
        if let Some(value) = self.user.and_then(&read) {
            return Some((value, Origin::UserConfig));
        }
        None
    }
}

fn resolve<T, F>(
    command_line: Option<T>,
    environment: Option<T>,
    levels: &Levels<'_>,
    from_file: F,
    fallback: T,
) -> Sourced<T>
where
    F: Fn(&ConfigFile) -> Option<T>,
{
    if let Some(value) = command_line {
        return Sourced::new(value, Origin::CommandLine);
    }
    if let Some(value) = environment {
        return Sourced::new(value, Origin::Environment);
    }
    if let Some((value, origin)) = levels.pick(from_file) {
        return Sourced::new(value, origin);
    }
    Sourced::new(fallback, Origin::Default)
}

/// Resolves every setting across the five precedence levels.
///
/// Takes the parsed global flags, the configuration files that were found, and
/// somewhere to read environment variables from. Returns each setting with the
/// level that supplied it, highest first: command line, environment, project
/// configuration, user configuration, then the built-in default.
#[must_use]
pub fn resolve_all(
    flags: &GlobalFlags,
    discovered: &Discovered,
    environment: &dyn Environment,
) -> Settings {
    let levels = Levels {
        project: discovered.project.as_ref().map(|loaded| &loaded.values),
        user: discovered.user.as_ref().map(|loaded| &loaded.values),
    };

    let offline = resolve(
        flags.offline.then_some(true),
        environment
            .get("FETCHLOOM_OFFLINE")
            .as_deref()
            .and_then(parse_bool),
        &levels,
        |file| file.offline,
        false,
    );
    let threads = resolve(
        flags.threads.map(Some),
        environment
            .get("FETCHLOOM_THREADS")
            .and_then(|text| text.parse().ok())
            .map(Some),
        &levels,
        |file| file.threads.map(Some),
        None,
    );
    let display = resolve(
        flags.display,
        None,
        &levels,
        |file| file.display.as_deref().and_then(parse_display),
        DisplayMode::Plain,
    );

    let cache_dir = if let Some(named) = flags.cache_dir.clone() {
        Sourced::new(named, Origin::CommandLine)
    } else if let Some(named) = environment.get("FETCHLOOM_CACHE_DIR") {
        Sourced::new(PathBuf::from(named), Origin::Environment)
    } else if let Some(named) = discovered.project.as_ref().and_then(project_cache_dir) {
        Sourced::new(named, Origin::ProjectConfig)
    } else if let Some(named) = discovered
        .user
        .as_ref()
        .and_then(|loaded| loaded.values.cache.as_ref()?.dir.clone())
    {
        Sourced::new(named, Origin::UserConfig)
    } else {
        Sourced::new(default_cache_dir(environment), Origin::Default)
    };

    Settings {
        offline,
        threads,
        display,
        cache_dir,
    }
}
