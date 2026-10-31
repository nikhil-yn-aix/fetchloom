//! debug logs and timelines. `-vv` or `FETCHLOOM_LOG` print logs on stderr, and
//! `FETCHLOOM_TRACE=run.json` writes a timeline of the run that perfetto opens.

use std::ffi::OsString;
use std::path::PathBuf;

use tracing_chrome::{ChromeLayerBuilder, FlushGuard};
use tracing_subscriber::Layer as _;
use tracing_subscriber::filter::{LevelFilter, Targets};
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;

use crate::output::Level;

/// keeps the timeline open until the run ends.
pub(super) struct Logs {
    on_stderr: bool,
    _timeline: Option<FlushGuard>,
}

impl Logs {
    /// whether logs share stderr with the story.
    pub(super) fn on_stderr(&self) -> bool {
        self.on_stderr
    }
}

pub(super) fn start(level: Level, color: bool, var: &dyn Fn(&str) -> Option<OsString>) -> Logs {
    let filter = var("FETCHLOOM_LOG")
        .and_then(|value| value.to_str()?.parse::<Targets>().ok())
        .or_else(|| {
            (level >= Level::Debug)
                .then(|| Targets::new().with_target("fetchloom", LevelFilter::DEBUG))
        });
    let timeline = var("FETCHLOOM_TRACE")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    if filter.is_none() && timeline.is_none() {
        return Logs {
            on_stderr: false,
            _timeline: None,
        };
    }
    let on_stderr = filter.is_some();
    let logs = filter.map(|filter| {
        tracing_subscriber::fmt::layer()
            .with_writer(std::io::stderr)
            .with_ansi(color)
            .without_time()
            .with_filter(filter)
    });
    let (timeline, guard) = match timeline {
        Some(path) => {
            let (layer, guard) = ChromeLayerBuilder::new()
                .file(path)
                .include_args(true)
                .build();
            let ours = Targets::new().with_target("fetchloom", LevelFilter::TRACE);
            (Some(layer.with_filter(ours)), Some(guard))
        }
        None => (None, None),
    };
    let _ = tracing_subscriber::registry()
        .with(logs)
        .with(timeline)
        .try_init();
    Logs {
        on_stderr,
        _timeline: guard,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_empty_or_invalid_settings_do_not_enable_logging() {
        for log in [None, Some("fetchloom=not-a-level")] {
            let logs = start(Level::Normal, false, &|name| match name {
                "FETCHLOOM_LOG" => log.map(OsString::from),
                "FETCHLOOM_TRACE" => Some(OsString::new()),
                _ => None,
            });
            assert!(!logs.on_stderr());
        }
    }

    #[test]
    fn configured_logs_filter_stderr_and_flush_a_readable_timeline() {
        const CHILD: &str = "FETCHLOOM_TEST_LOG_TIMELINE";
        const MODE: &str = "FETCHLOOM_TEST_LOG_MODE";
        if let Some(path) = std::env::var_os(CHILD) {
            let mode = std::env::var(MODE).unwrap();
            let level = if mode == "debug" {
                Level::Debug
            } else {
                Level::Normal
            };
            let logs = start(level, false, &|name| match name {
                "FETCHLOOM_TRACE" => Some(path.clone()),
                "FETCHLOOM_LOG" if mode == "debug" => Some("fetchloom=not-a-level".into()),
                "FETCHLOOM_LOG" if mode == "explicit" => Some("fetchloom::coverage=info".into()),
                _ => None,
            });
            assert_eq!(logs.on_stderr(), mode != "timeline");
            {
                let span =
                    tracing::info_span!(target: "fetchloom::coverage", "listing", dataset = "iris");
                let _entered = span.enter();
                tracing::info!(target: "fetchloom::coverage", count = 2, "listed dataset");
                tracing::debug!(target: "fetchloom::coverage", "debug-only");
                tracing::info!(target: "other_crate", "outside-noise");
            }
            drop(logs);
            return;
        }
        // Each child owns its global subscriber; tests in this process never replace it.
        let dir = tempfile::tempdir().unwrap();
        for mode in ["timeline", "debug", "explicit"] {
            let path = dir.path().join(format!("{mode}.json"));
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "cli::logs::tests::configured_logs_filter_stderr_and_flush_a_readable_timeline",
                ])
                .env(CHILD, &path)
                .env(MODE, mode)
                .output()
                .unwrap();
            let stderr = String::from_utf8(output.stderr).unwrap();
            assert!(output.status.success(), "{mode}: {stderr}");
            assert_eq!(
                stderr.contains("listed dataset"),
                mode != "timeline",
                "{stderr}"
            );
            assert_eq!(stderr.contains("debug-only"), mode == "debug", "{stderr}");
            assert!(!stderr.contains("outside-noise"), "{stderr}");
            assert!(!stderr.contains('\u{1b}'), "color was disabled: {stderr}");
            let trace: serde_json::Value =
                serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
            let events = trace
                .as_array()
                .or_else(|| {
                    trace
                        .get("traceEvents")
                        .and_then(serde_json::Value::as_array)
                })
                .unwrap();
            assert!(events.iter().any(|event| event["name"] == "listing"));
            let contents = trace.to_string();
            assert!(
                contents.contains("iris"),
                "span arguments were not recorded"
            );
            assert!(!contents.contains("outside-noise"));
        }
    }
}
