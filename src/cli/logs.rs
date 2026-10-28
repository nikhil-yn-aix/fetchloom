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
