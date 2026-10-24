//! Diagnostics for the `fl` process: the stderr log, the JSON lines log file, the Chrome trace file
//! and the message printed when the program panics.

use std::fs::{self, File};
use std::io::{self, IsTerminal, Write};
use std::panic::PanicHookInfo;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;

use fl_model::error::ErrorKind;

use crate::failure::ISSUES;
use tracing::Subscriber;
use tracing::level_filters::LevelFilter;
use tracing_chrome::{ChromeLayerBuilder, FlushGuard};
use tracing_subscriber::Layer;
use tracing_subscriber::filter::{ParseError, Targets};
use tracing_subscriber::fmt;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::registry::Registry;

const OWN_TARGETS: [&str; 5] = [
    "fl_cli",
    "fl_sources",
    "fl_transfer",
    "fl_store",
    "fl_model",
];

/// What the user asked to observe: the repeat count of `-v`, the files to write and the filter
/// taken from `FETCHLOOM_LOG`.
pub(crate) struct Options {
    pub(crate) verbose: u8,
    pub(crate) log_file: Option<PathBuf>,
    pub(crate) trace_file: Option<PathBuf>,
    pub(crate) filter: Option<String>,
}

/// Keeps the trace file writer alive. Dropping it writes the closing bracket of the trace file.
pub(crate) struct Guard {
    _chrome: Option<FlushGuard>,
}

/// Why diagnostics could not be set up.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ObserveError {
    /// A log or trace file could not be created.
    #[error("cannot create `{}`: {source}", path.display())]
    File {
        /// The file that could not be created.
        path: PathBuf,
        /// The failure of the file system.
        source: io::Error,
    },
    /// The `FETCHLOOM_LOG` filter does not parse.
    #[error("`{filter}` is not a valid log filter: {source}")]
    Filter {
        /// The text that was refused.
        filter: String,
        /// The parser failure.
        source: ParseError,
    },
    /// Another subscriber already receives the events of this process.
    #[error("diagnostics were already installed in this process")]
    AlreadyInstalled,
}

impl ObserveError {
    /// The error kind that decides the exit code.
    pub(crate) fn kind(&self) -> ErrorKind {
        match self {
            Self::File { .. } => ErrorKind::LocalIo,
            Self::Filter { .. } | Self::AlreadyInstalled => ErrorKind::Usage,
        }
    }
}

/// Builds the subscriber for `options` and makes it the global one for the process.
pub(crate) fn install(options: &Options) -> Result<Guard, ObserveError> {
    let (subscriber, guard) = subscriber(options)?;
    tracing::subscriber::set_global_default(subscriber)
        .map_err(|_| ObserveError::AlreadyInstalled)?;
    Ok(guard)
}

/// Builds the subscriber for `options` without installing it anywhere.
pub(crate) fn subscriber(
    options: &Options,
) -> Result<(impl Subscriber + Send + Sync, Guard), ObserveError> {
    let filter = options.filter.as_deref();
    let stderr_filter = stderr_targets(options.verbose, filter)?;
    let log_filter = filter
        .map(parse_filter)
        .transpose()?
        .unwrap_or_else(|| own_targets(LevelFilter::DEBUG, LevelFilter::OFF));
    let log_writer = options
        .log_file
        .as_deref()
        .map(create_file)
        .transpose()?
        .map(Arc::new);
    let trace_writer = options.trace_file.as_deref().map(create_file).transpose()?;

    let stderr_layer = stderr_filter.map(|targets| {
        fmt::layer()
            .with_writer(io::stderr)
            .with_ansi(io::stderr().is_terminal())
            .with_filter(targets)
    });
    let log_layer = log_writer.map(|file| {
        fmt::layer()
            .json()
            .with_ansi(false)
            .with_writer(file)
            .with_filter(log_filter)
    });
    let (chrome_layer, chrome_guard) = match trace_writer {
        Some(file) => {
            let (layer, guard) = ChromeLayerBuilder::new()
                .writer(file)
                .include_args(true)
                .build();
            (
                Some(layer.with_filter(own_targets(LevelFilter::TRACE, LevelFilter::DEBUG))),
                Some(guard),
            )
        }
        None => (None, None),
    };

    let subscriber = Registry::default()
        .with(stderr_layer)
        .with(log_layer)
        .with(chrome_layer);
    Ok((
        subscriber,
        Guard {
            _chrome: chrome_guard,
        },
    ))
}

/// The filter for the stderr log: none below verbosity 2, otherwise `filter` when given, else
/// debug (trace from 3) for the `fl` crates and a quieter level for the rest.
pub(crate) fn stderr_targets(
    verbose: u8,
    filter: Option<&str>,
) -> Result<Option<Targets>, ObserveError> {
    if verbose < 2 {
        return Ok(None);
    }
    if let Some(text) = filter {
        return parse_filter(text).map(Some);
    }
    let targets = if verbose >= 3 {
        own_targets(LevelFilter::TRACE, LevelFilter::INFO)
    } else {
        own_targets(LevelFilter::DEBUG, LevelFilter::WARN)
    };
    Ok(Some(targets))
}

fn own_targets(own: LevelFilter, rest: LevelFilter) -> Targets {
    Targets::new()
        .with_targets(OWN_TARGETS.iter().map(|target| (*target, own)))
        .with_default(rest)
}

fn parse_filter(text: &str) -> Result<Targets, ObserveError> {
    Targets::from_str(text).map_err(|source| ObserveError::Filter {
        filter: text.to_owned(),
        source,
    })
}

fn create_file(path: &Path) -> Result<File, ObserveError> {
    path.parent()
        .map_or(Ok(()), fs::create_dir_all)
        .and_then(|()| File::create(path))
        .map_err(|source| ObserveError::File {
            path: path.to_path_buf(),
            source,
        })
}

/// Replaces the panic hook with one that tells the user the program stopped on an internal
/// error, where, and where to report it. The previous hook runs first when `RUST_BACKTRACE` is
/// set to something other than `0`.
pub(crate) fn panic_hook(log_file: Option<PathBuf>) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if backtrace_requested(std::env::var_os("RUST_BACKTRACE").as_deref()) {
            previous(info);
        }
        let text = told(info, log_file.as_deref());
        let _ignored = io::stderr().write_all(text.as_bytes());
    }));
}

fn backtrace_requested(value: Option<&std::ffi::OsStr>) -> bool {
    value.is_some_and(|value| value != "0")
}

fn told(info: &PanicHookInfo<'_>, log: Option<&Path>) -> String {
    apology(
        env!("CARGO_PKG_VERSION"),
        &message_of(info),
        &location_of(info),
        log,
    )
}

fn message_of(info: &PanicHookInfo<'_>) -> String {
    let payload = info.payload();
    payload
        .downcast_ref::<&str>()
        .map(|text| (*text).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown".to_owned())
}

fn location_of(info: &PanicHookInfo<'_>) -> String {
    info.location().map_or_else(
        || "unknown".to_owned(),
        |place| format!("{}:{}", place.file(), place.line()),
    )
}

fn apology(version: &str, message: &str, location: &str, log: Option<&Path>) -> String {
    let log_line = log.map_or_else(String::new, |path| format!("  log: {}\n", path.display()));
    format!(
        "fl {version} stopped on an internal error, sorry.\n  what: {message} at {location}\n{log_line}  report it at {ISSUES}\n"
    )
}

#[cfg(test)]
mod tests {
    use std::error::Error;
    use std::thread;

    use serde_json::Value;
    use tracing::dispatcher::{self, Dispatch};

    use super::*;

    type TestResult = Result<(), Box<dyn Error>>;

    #[test]
    fn apology_without_a_log_has_three_lines() {
        let text = apology("1.2.3", "boom", "src/a.rs:7", None);
        assert_eq!(
            text,
            format!(
                "fl 1.2.3 stopped on an internal error, sorry.\n  what: boom at src/a.rs:7\n  report it at {ISSUES}\n"
            )
        );
    }

    #[test]
    fn apology_with_a_log_names_it_before_the_report_line() {
        let text = apology("1.2.3", "boom", "src/a.rs:7", Some(Path::new("run.log")));
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[2], "  log: run.log");
        assert!(lines[3].starts_with("  report it at "));
    }

    #[test]
    fn a_panic_is_told_with_its_message_and_place() {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let kept = Arc::clone(&seen);
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            kept.lock().unwrap().push(told(info, None));
        }));
        let _ = std::panic::catch_unwind(|| panic!("boom"));
        let _ = std::panic::catch_unwind(|| panic!("boom {}", 7));
        let _ = std::panic::catch_unwind(|| std::panic::panic_any(3_u8));
        std::panic::set_hook(previous);
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 3);
        assert!(seen[0].contains("what: boom at "), "{}", seen[0]);
        assert!(seen[1].contains("what: boom 7 at "), "{}", seen[1]);
        assert!(seen[2].contains("what: unknown at "), "{}", seen[2]);
        assert!(seen[0].contains("observe.rs:"), "{}", seen[0]);
    }

    #[test]
    fn a_backtrace_is_asked_for_by_any_value_but_zero() {
        assert!(!backtrace_requested(None));
        assert!(!backtrace_requested(Some("0".as_ref())));
        assert!(backtrace_requested(Some("1".as_ref())));
        assert!(backtrace_requested(Some("full".as_ref())));
    }

    #[test]
    fn a_log_file_gets_the_folders_it_needs() -> TestResult {
        let dir = tempfile::tempdir()?;
        let nested = dir.path().join("logs").join("run.log");
        create_file(&nested)?;
        assert!(nested.is_file());
        Ok(())
    }

    #[test]
    fn stderr_is_off_below_verbosity_two() -> TestResult {
        assert!(stderr_targets(0, None)?.is_none());
        assert!(stderr_targets(1, Some("trace"))?.is_none());
        Ok(())
    }

    #[test]
    fn verbosity_picks_the_levels_for_own_and_other_targets() -> TestResult {
        let two = stderr_targets(2, None)?.ok_or("no filter at 2")?;
        assert!(two.would_enable("fl_store", &tracing::Level::DEBUG));
        assert!(!two.would_enable("fl_store", &tracing::Level::TRACE));
        assert!(two.would_enable("hyper", &tracing::Level::WARN));
        assert!(!two.would_enable("hyper", &tracing::Level::INFO));
        let three = stderr_targets(3, None)?.ok_or("no filter at 3")?;
        assert!(three.would_enable("fl_cli", &tracing::Level::TRACE));
        assert!(three.would_enable("hyper", &tracing::Level::INFO));
        assert!(!three.would_enable("hyper", &tracing::Level::DEBUG));
        Ok(())
    }

    #[test]
    fn a_filter_replaces_the_defaults() -> TestResult {
        let targets = stderr_targets(2, Some("hyper=trace"))?.ok_or("no filter")?;
        assert!(targets.would_enable("hyper", &tracing::Level::TRACE));
        assert!(!targets.would_enable("fl_store", &tracing::Level::ERROR));
        Ok(())
    }

    #[test]
    fn a_bad_filter_is_a_usage_error() {
        let err = stderr_targets(2, Some("fl_store=loud")).err();
        assert_eq!(err.as_ref().map(ObserveError::kind), Some(ErrorKind::Usage));
    }

    #[test]
    fn kinds_follow_the_failure() {
        let file = ObserveError::File {
            path: PathBuf::from("x"),
            source: io::ErrorKind::NotFound.into(),
        };
        assert_eq!(file.kind(), ErrorKind::LocalIo);
        assert_eq!(ObserveError::AlreadyInstalled.kind(), ErrorKind::Usage);
        assert!(file.to_string().contains("cannot create `x`"));
    }

    #[test]
    fn a_file_that_cannot_be_created_is_a_local_io_error() -> TestResult {
        let dir = tempfile::tempdir()?;
        let blocker = dir.path().join("blocker");
        fs::write(&blocker, b"")?;
        let options = Options {
            verbose: 0,
            log_file: Some(blocker.join("run.log")),
            trace_file: None,
            filter: None,
        };
        let err = subscriber(&options).err();
        assert_eq!(
            err.as_ref().map(ObserveError::kind),
            Some(ErrorKind::LocalIo)
        );
        Ok(())
    }

    #[test]
    fn spans_on_two_threads_reach_the_trace_and_log_files() -> TestResult {
        let dir = tempfile::tempdir()?;
        let trace = dir.path().join("nested").join("trace.json");
        let log = dir.path().join("logs").join("run.log");
        let options = Options {
            verbose: 0,
            log_file: Some(log.clone()),
            trace_file: Some(trace.clone()),
            filter: None,
        };
        let (subscriber, guard) = subscriber(&options)?;
        let dispatch = Dispatch::new(subscriber);
        thread::scope(|scope| -> TestResult {
            let mut handles = Vec::new();
            for name in ["worker-a", "worker-b"] {
                let dispatch = dispatch.clone();
                handles.push(thread::Builder::new().name(name.to_owned()).spawn_scoped(
                    scope,
                    move || {
                        dispatcher::with_default(&dispatch, || {
                            let span = tracing::info_span!("fetch", job = name);
                            let _entered = span.enter();
                            tracing::debug!(bytes = 5, "chunk done");
                        });
                    },
                )?);
            }
            for handle in handles {
                handle.join().map_err(|_| "worker panicked")?;
            }
            Ok(())
        })?;
        drop(dispatch);
        drop(guard);

        let events: Value = serde_json::from_str(&fs::read_to_string(&trace)?)?;
        let events = events.as_array().ok_or("trace is not an array")?;
        let mut tids: Vec<u64> = events
            .iter()
            .filter(|event| event["ph"] == "B")
            .filter_map(|event| event["tid"].as_u64())
            .collect();
        tids.sort_unstable();
        tids.dedup();
        assert_eq!(tids.len(), 2);
        assert!(events.iter().any(|event| event["name"] == "thread_name"
            && event["ph"] == "M"
            && event["args"]["name"] == "worker-a"));

        let lines = fs::read_to_string(&log)?;
        let parsed: Vec<Value> = lines
            .lines()
            .map(serde_json::from_str)
            .collect::<Result<_, _>>()?;
        assert_eq!(parsed.len(), 2);
        let mut jobs: Vec<&str> = parsed
            .iter()
            .filter_map(|line| line["span"]["job"].as_str())
            .collect();
        jobs.sort_unstable();
        assert_eq!(jobs, ["worker-a", "worker-b"]);
        Ok(())
    }
}
