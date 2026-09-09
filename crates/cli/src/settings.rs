//! Resolving every setting across the five precedence levels.

use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::time::Duration;

use fetchloom_engine::limits::{Bandwidth, Limits};

use crate::config::{ConfigFile, Discovered, Origin, Sourced};
use crate::logging::LogLevel;
use crate::surface::{ColorChoice, DisplayMode, DurationArg, GlobalFlags, IoChoice, TransferFlags};
use fetchloom_engine::compression::CompressionChoice;

pub trait Environment: Send + Sync {
    fn get(&self, name: &str) -> Option<String>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ProcessEnvironment;

impl Environment for ProcessEnvironment {
    fn get(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refused {
    pub key: String,
    pub origin: Origin,
    pub next_action: String,
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} from the {} is not a value {} takes: {}",
            self.key, self.origin, self.key, self.next_action
        )
    }
}

impl std::error::Error for Refused {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub offline: Sourced<bool>,
    pub threads: Sourced<Option<NonZeroU32>>,
    pub display: Sourced<DisplayMode>,
    pub cache_dir: Sourced<PathBuf>,
    pub library_dir: Sourced<PathBuf>,
    pub concurrency: Sourced<Option<NonZeroU32>>,
    pub per_host: Sourced<Option<NonZeroU32>>,
    pub bandwidth: Sourced<Option<Bandwidth>>,
    pub(crate) io: Sourced<IoChoice>,
    pub aggressive: Sourced<bool>,
    pub(crate) deterministic_io: Sourced<bool>,
    pub log: Sourced<LogLevel>,
    pub log_clamped: bool,
    pub retries: Sourced<NonZeroU32>,
    pub timeout: Sourced<Duration>,
    pub sources: Sourced<Vec<String>>,
    pub color: Sourced<ColorChoice>,
    pub hints: Sourced<bool>,
    pub compress: Sourced<CompressionChoice>,
}

#[must_use]
pub(crate) fn limits_for(settings: &Settings) -> Limits {
    Limits {
        retry_attempts: settings.retries.value.get(),
        idle_timeout: settings.timeout.value,
        ..Limits::default()
    }
}

/// # Errors
/// `Refused` naming what to set when no per-user directory can be found.
pub fn default_cache_dir(environment: &dyn Environment) -> Result<PathBuf, Refused> {
    #[cfg(windows)]
    {
        if let Some(local) = absolute_from(environment, "LOCALAPPDATA") {
            return Ok(local.join("Fetchloom").join("Cache"));
        }
    }
    #[cfg(unix)]
    {
        if let Some(base) = absolute_from(environment, "XDG_CACHE_HOME") {
            return Ok(base.join("fetchloom"));
        }
        if let Some(home) = absolute_from(environment, "HOME") {
            return Ok(home.join(".cache").join("fetchloom"));
        }
    }
    Err(no_home("cache", CACHE_VARIABLES))
}

/// # Errors
/// `Refused` naming what to set when no per-user directory can be found.
pub fn default_library_dir(environment: &dyn Environment) -> Result<PathBuf, Refused> {
    #[cfg(windows)]
    {
        if let Some(local) = absolute_from(environment, "LOCALAPPDATA") {
            return Ok(local.join("Fetchloom").join("Library"));
        }
    }
    #[cfg(unix)]
    {
        if let Some(base) = absolute_from(environment, "XDG_DATA_HOME") {
            return Ok(base.join("fetchloom").join("library"));
        }
        if let Some(home) = absolute_from(environment, "HOME") {
            return Ok(home
                .join(".local")
                .join("share")
                .join("fetchloom")
                .join("library"));
        }
    }
    Err(no_home("library", LIBRARY_VARIABLES))
}

#[cfg(windows)]
const CACHE_VARIABLES: &str = "LOCALAPPDATA";

#[cfg(unix)]
const CACHE_VARIABLES: &str = "HOME or XDG_CACHE_HOME";

#[cfg(windows)]
const LIBRARY_VARIABLES: &str = "LOCALAPPDATA";

#[cfg(unix)]
const LIBRARY_VARIABLES: &str = "HOME or XDG_DATA_HOME";

/// The base directory specification says a relative value is invalid and must
/// be ignored, and a cache in whatever directory the person was standing in is
/// what honoring one would produce.
fn absolute_from(environment: &dyn Environment, name: &str) -> Option<PathBuf> {
    let stated = PathBuf::from(environment.get(name)?);
    stated.is_absolute().then_some(stated)
}

fn no_home(what: &str, variables: &str) -> Refused {
    Refused {
        key: format!("{what} directory"),
        origin: Origin::Default,
        next_action: format!(
            "set {variables} to an absolute path, or name the {what} directory with a flag, because neither is set to one and a run does not put a {what} in whatever directory it was started from"
        ),
    }
}

fn project_cache_dir(loaded: &crate::config::LoadedConfig) -> Option<PathBuf> {
    let named = loaded.values.cache.as_ref()?.dir.as_ref()?;
    let beside = loaded.path.parent().unwrap_or(Path::new("."));
    Some(without_here(&beside.join(named)))
}

fn project_library_dir(loaded: &crate::config::LoadedConfig) -> Option<PathBuf> {
    let named = loaded.values.library.as_ref()?.dir.as_ref()?;
    let beside = loaded.path.parent().unwrap_or(Path::new("."));
    Some(without_here(&beside.join(named)))
}

fn without_here(path: &Path) -> PathBuf {
    path.components()
        .filter(|part| !matches!(part, std::path::Component::CurDir))
        .collect()
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

fn parse_io(text: &str) -> Option<IoChoice> {
    match text {
        "auto" => Some(IoChoice::Auto),
        "buffered" => Some(IoChoice::Buffered),
        "uncached" => Some(IoChoice::Uncached),
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

fn from_environment<T: std::str::FromStr>(
    environment: &dyn Environment,
    name: &str,
    key: &str,
) -> Result<Option<T>, Refused> {
    let Some(text) = environment.get(name) else {
        return Ok(None);
    };
    text.parse().map(Some).map_err(|_| Refused {
        key: key.to_owned(),
        origin: Origin::Environment,
        next_action: format!("set {name} to a value {key} takes, or unset it"),
    })
}

fn from_files<T, F, P>(
    levels: &Levels<'_>,
    read: F,
    parse: P,
    key: &str,
) -> Result<Option<(T, Origin)>, Refused>
where
    F: Fn(&ConfigFile) -> Option<String>,
    P: Fn(&str) -> Option<T>,
{
    let Some((text, origin)) = levels.pick(read) else {
        return Ok(None);
    };
    match parse(&text) {
        Some(value) => Ok(Some((value, origin))),
        None => Err(Refused {
            key: key.to_owned(),
            origin,
            next_action: format!("write {key} in the {origin} as a value it takes, or remove it"),
        }),
    }
}

/// # Errors
/// `Refused`, naming the setting that was given a value this build does not
/// accept and the level it came from.
pub fn resolve_all(
    flags: &GlobalFlags,
    transfer: &TransferFlags,
    discovered: &Discovered,
    environment: &dyn Environment,
) -> Result<Settings, Refused> {
    let levels = Levels {
        project: discovered.project.as_ref().map(|loaded| &loaded.values),
        user: discovered.user.as_ref().map(|loaded| &loaded.values),
    };

    let offline_variable = match environment.get("FETCHLOOM_OFFLINE") {
        None => None,
        Some(text) => Some(parse_bool(&text).ok_or_else(|| Refused {
            key: "offline".to_owned(),
            origin: Origin::Environment,
            next_action: "set FETCHLOOM_OFFLINE to 1 or 0, or unset it".to_owned(),
        })?),
    };
    let offline = resolve(
        flags.offline.then_some(true),
        offline_variable,
        &levels,
        |file| file.offline,
        false,
    );
    let threads = resolve(
        flags.threads.and_then(NonZeroU32::new).map(Some),
        from_environment::<NonZeroU32>(environment, "FETCHLOOM_THREADS", "threads")?.map(Some),
        &levels,
        |file| file.threads.map(Some),
        None,
    );
    let display = match flags.display {
        Some(mode) => Sourced::new(mode, Origin::CommandLine),
        None => match from_files(
            &levels,
            |file| file.display.clone(),
            parse_display,
            "display",
        )? {
            Some((mode, origin)) => Sourced::new(mode, origin),
            None => Sourced::new(DisplayMode::Plain, Origin::Default),
        },
    };
    let Tuned {
        concurrency,
        per_host,
        bandwidth,
        io,
        aggressive,
        deterministic_io,
    } = resolve_tuning(transfer, &levels, environment)?;

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
        Sourced::new(default_cache_dir(environment)?, Origin::Default)
    };

    let library_dir = resolve_library_dir(flags, discovered, environment)?;

    let (log, log_clamped) = resolve_log(flags, &levels, environment)?;
    let defaults = Limits::default();
    let retries = resolve(
        transfer.retries.and_then(NonZeroU32::new),
        None,
        &levels,
        |file| file.retries,
        NonZeroU32::new(defaults.retry_attempts).unwrap_or(NonZeroU32::MIN),
    );
    let timeout = resolve_timeout(transfer, &levels, defaults)?;
    let Presented {
        sources,
        color,
        hints,
    } = resolve_presentation(flags, &levels)?;
    let compress = resolve_compression(flags, &levels, environment)?;

    Ok(Settings {
        offline,
        threads,
        display,
        cache_dir,
        library_dir,
        concurrency,
        per_host,
        bandwidth,
        io,
        aggressive,
        deterministic_io,
        log,
        log_clamped,
        retries,
        timeout,
        sources,
        color,
        hints,
        compress,
    })
}

fn resolve_color_choice(
    flags: &GlobalFlags,
    levels: &Levels<'_>,
) -> Result<Sourced<ColorChoice>, Refused> {
    if let Some(chosen) = flags.color {
        return Ok(Sourced::new(chosen, Origin::CommandLine));
    }
    match from_files(
        levels,
        |file| file.color.clone(),
        |text| match text {
            "auto" => Some(ColorChoice::Auto),
            "always" => Some(ColorChoice::Always),
            "never" => Some(ColorChoice::Never),
            _ => None,
        },
        "color",
    )? {
        Some((chosen, origin)) => Ok(Sourced::new(chosen, origin)),
        None => Ok(Sourced::new(ColorChoice::Auto, Origin::Default)),
    }
}

struct Tuned {
    concurrency: Sourced<Option<NonZeroU32>>,
    per_host: Sourced<Option<NonZeroU32>>,
    bandwidth: Sourced<Option<Bandwidth>>,
    io: Sourced<IoChoice>,
    aggressive: Sourced<bool>,
    deterministic_io: Sourced<bool>,
}

fn resolve_tuning(
    transfer: &TransferFlags,
    levels: &Levels<'_>,
    environment: &dyn Environment,
) -> Result<Tuned, Refused> {
    let concurrency = resolve(
        transfer.concurrency.and_then(NonZeroU32::new).map(Some),
        from_environment::<NonZeroU32>(environment, "FETCHLOOM_CONCURRENCY", "concurrency")?
            .map(Some),
        levels,
        |file| file.concurrency.map(Some),
        None,
    );
    let per_host = resolve(
        transfer.per_host.and_then(NonZeroU32::new).map(Some),
        from_environment::<NonZeroU32>(environment, "FETCHLOOM_PER_HOST", "per-host")?.map(Some),
        levels,
        |file| file.per_host.map(Some),
        None,
    );
    let bandwidth = if let Some(rate) = transfer.bandwidth {
        Sourced::new(Some(rate.0), Origin::CommandLine)
    } else if let Some(rate) =
        from_environment::<Bandwidth>(environment, "FETCHLOOM_BANDWIDTH", "bandwidth")?
    {
        Sourced::new(Some(rate), Origin::Environment)
    } else if let Some((rate, origin)) = from_files(
        levels,
        |file| file.bandwidth.clone(),
        |text| text.parse::<Bandwidth>().ok(),
        "bandwidth",
    )? {
        Sourced::new(Some(rate), origin)
    } else {
        Sourced::new(None, Origin::Default)
    };
    let io = match transfer.io {
        Some(choice) => Sourced::new(choice, Origin::CommandLine),
        None => match from_files(levels, |file| file.io.clone(), parse_io, "io")? {
            Some((choice, origin)) => Sourced::new(choice, origin),
            None => Sourced::new(IoChoice::Auto, Origin::Default),
        },
    };
    let aggressive = Sourced::new(
        transfer.aggressive,
        if transfer.aggressive {
            Origin::CommandLine
        } else {
            Origin::Default
        },
    );
    let deterministic_io = Sourced::new(
        transfer.deterministic_io,
        if transfer.deterministic_io {
            Origin::CommandLine
        } else {
            Origin::Default
        },
    );
    Ok(Tuned {
        concurrency,
        per_host,
        bandwidth,
        io,
        aggressive,
        deterministic_io,
    })
}

fn resolve_compression(
    flags: &GlobalFlags,
    levels: &Levels<'_>,
    environment: &dyn Environment,
) -> Result<Sourced<CompressionChoice>, Refused> {
    if let Some(chosen) = flags.compress {
        return Ok(Sourced::new(chosen.0, Origin::CommandLine));
    }
    if let Some(chosen) =
        from_environment::<CompressionChoice>(environment, "FETCHLOOM_COMPRESS", "compress")?
    {
        return Ok(Sourced::new(chosen, Origin::Environment));
    }
    if let Some((chosen, origin)) = from_files(
        levels,
        |file| file.compress.clone(),
        |text| text.parse::<CompressionChoice>().ok(),
        "compress",
    )? {
        return Ok(Sourced::new(chosen, origin));
    }
    Ok(Sourced::new(CompressionChoice::Auto, Origin::Default))
}

fn resolve_log(
    flags: &GlobalFlags,
    levels: &Levels<'_>,
    environment: &dyn Environment,
) -> Result<(Sourced<LogLevel>, bool), Refused> {
    if flags.verbose > 0 {
        let (level, clamped) = LogLevel::default().raised(u32::from(flags.verbose));
        return Ok((Sourced::new(level, Origin::CommandLine), clamped));
    }
    if let Some(level) = from_environment::<LogLevel>(environment, "FETCHLOOM_LOG", "log")? {
        return Ok((Sourced::new(level, Origin::Environment), false));
    }
    if let Some((level, origin)) = from_files(
        levels,
        |file| file.log.clone(),
        |text| text.parse::<LogLevel>().ok(),
        "log",
    )? {
        return Ok((Sourced::new(level, origin), false));
    }
    Ok((Sourced::new(LogLevel::default(), Origin::Default), false))
}

fn resolve_timeout(
    transfer: &TransferFlags,
    levels: &Levels<'_>,
    defaults: Limits,
) -> Result<Sourced<Duration>, Refused> {
    if let Some(span) = transfer.timeout {
        return Ok(Sourced::new(span.0, Origin::CommandLine));
    }
    if let Some((span, origin)) = from_files(
        levels,
        |file| file.timeout.clone(),
        |text| text.parse::<DurationArg>().ok().map(|span| span.0),
        "timeout",
    )? {
        return Ok(Sourced::new(span, origin));
    }
    Ok(Sourced::new(defaults.idle_timeout, Origin::Default))
}

struct Presented {
    sources: Sourced<Vec<String>>,
    color: Sourced<ColorChoice>,
    hints: Sourced<bool>,
}

fn resolve_presentation(flags: &GlobalFlags, levels: &Levels<'_>) -> Result<Presented, Refused> {
    let sources = match levels.pick(|file| file.sources.clone()) {
        Some((bases, origin)) => Sourced::new(bases, origin),
        None => Sourced::new(Vec::new(), Origin::Default),
    };
    Ok(Presented {
        sources,
        color: resolve_color_choice(flags, levels)?,
        hints: resolve(
            flags.no_hints.then_some(false),
            None,
            levels,
            |file| file.hints,
            true,
        ),
    })
}

fn resolve_library_dir(
    flags: &GlobalFlags,
    discovered: &Discovered,
    environment: &dyn Environment,
) -> Result<Sourced<PathBuf>, Refused> {
    if let Some(named) = flags.library_dir.clone() {
        return Ok(Sourced::new(named, Origin::CommandLine));
    }
    if let Some(named) = environment.get("FETCHLOOM_LIBRARY_DIR") {
        return Ok(Sourced::new(PathBuf::from(named), Origin::Environment));
    }
    if let Some(named) = discovered.project.as_ref().and_then(project_library_dir) {
        return Ok(Sourced::new(named, Origin::ProjectConfig));
    }
    if let Some(named) = discovered
        .user
        .as_ref()
        .and_then(|loaded| loaded.values.library.as_ref()?.dir.clone())
    {
        return Ok(Sourced::new(named, Origin::UserConfig));
    }
    Ok(Sourced::new(
        default_library_dir(environment)?,
        Origin::Default,
    ))
}
