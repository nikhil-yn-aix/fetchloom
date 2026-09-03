//! The log level, and the observer that renders events at it.

use std::io::Write;
use std::sync::{Mutex, PoisonError};

use fetchloom_engine::event::{Event, EventPayload};
use fetchloom_engine::seam::observer::Observer;

/// How much of the event stream is rendered to standard error.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogLevel {
    /// Errors and degradations only.
    Error,
    /// Those, plus the run's own start, end, and result.
    #[default]
    Info,
    /// Every event the stream carries.
    Debug,
}

impl LogLevel {
    /// Returns the name this level is written under.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Info => "info",
            Self::Debug => "debug",
        }
    }

    /// Returns the level one step above this one, or nothing when this is
    /// already the highest.
    #[must_use]
    pub fn above(self) -> Option<Self> {
        match self {
            Self::Error => Some(Self::Info),
            Self::Info => Some(Self::Debug),
            Self::Debug => None,
        }
    }

    /// Returns the level reached by raising this one the given number of steps,
    /// and whether the request went past the highest.
    #[must_use]
    pub fn raised(self, steps: u32) -> (Self, bool) {
        let mut level = self;
        for _ in 0..steps {
            match level.above() {
                Some(next) => level = next,
                None => return (level, true),
            }
        }
        (level, false)
    }

    /// Reports whether an event is rendered at this level.
    #[must_use]
    pub fn renders(self, payload: &EventPayload) -> bool {
        match self {
            Self::Debug => true,
            Self::Info => matches!(
                payload,
                EventPayload::Failure { .. }
                    | EventPayload::Degrade { .. }
                    | EventPayload::RunStart
                    | EventPayload::RunEnd { .. }
            ),
            Self::Error => matches!(
                payload,
                EventPayload::Failure { .. } | EventPayload::Degrade { .. }
            ),
        }
    }
}

impl std::fmt::Display for LogLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

impl std::str::FromStr for LogLevel {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text {
            "error" => Ok(Self::Error),
            "info" => Ok(Self::Info),
            "debug" => Ok(Self::Debug),
            _ => Err(format!("{text} is not error, info, or debug")),
        }
    }
}

/// An observer that renders the events a level admits to standard error.
pub struct Log {
    level: LogLevel,
    sink: Mutex<Box<dyn Write + Send>>,
}

impl std::fmt::Debug for Log {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Log")
            .field("level", &self.level)
            .finish_non_exhaustive()
    }
}

impl Log {
    /// Builds the log for a level, writing to standard error.
    #[must_use]
    pub fn new(level: LogLevel) -> Self {
        Self {
            level,
            sink: Mutex::new(Box::new(std::io::stderr())),
        }
    }

    /// Builds the log for a level, writing somewhere a test can read.
    #[must_use]
    pub fn writing(level: LogLevel, sink: Box<dyn Write + Send>) -> Self {
        Self {
            level,
            sink: Mutex::new(sink),
        }
    }
}

impl Observer for Log {
    fn emit(&self, event: &Event) {
        if !self.level.renders(event.payload()) {
            return;
        }
        let Ok(line) = serde_json::to_string(event) else {
            return;
        };
        let mut sink = self.sink.lock().unwrap_or_else(PoisonError::into_inner);
        let _ = writeln!(sink, "{line}");
        let _ = sink.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::{LogLevel, Log};
    use fetchloom_engine::event::{Event, EventPayload, Sequence};
    use fetchloom_engine::seam::observer::Observer;

    #[test]
    fn info_renders_the_run_and_not_a_publication() {
        assert!(LogLevel::Info.renders(&EventPayload::RunStart));
        assert!(!LogLevel::Info.renders(&EventPayload::PublishCommit));
        assert!(LogLevel::Debug.renders(&EventPayload::PublishCommit));
    }

    #[test]
    fn error_renders_a_degradation_and_not_the_run() {
        assert!(LogLevel::Error.renders(&EventPayload::Degrade {
            requested: "a".to_owned(),
            used: "b".to_owned(),
            reason: "c".to_owned(),
        }));
        assert!(!LogLevel::Error.renders(&EventPayload::RunStart));
    }

    #[test]
    fn raising_past_the_highest_reports_the_clamp() {
        assert_eq!(LogLevel::Info.raised(1), (LogLevel::Debug, false));
        assert_eq!(LogLevel::Info.raised(2), (LogLevel::Debug, true));
        assert_eq!(LogLevel::Error.raised(0), (LogLevel::Error, false));
    }

    struct Shared(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for Shared {
        fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
            #[expect(
                clippy::unwrap_used,
                reason = "a poisoned mutex in a test is the assertion"
            )]
            self.0.lock().unwrap().extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_level_renders_only_what_it_admits() {
        let written = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = Log::writing(
            LogLevel::Error,
            Box::new(Shared(std::sync::Arc::clone(&written))),
        );
        let sequence = Sequence::new();
        log.emit(&Event::new(&sequence, EventPayload::RunStart));
        log.emit(&Event::new(
            &sequence,
            EventPayload::Degrade {
                requested: "one".to_owned(),
                used: "two".to_owned(),
                reason: "three".to_owned(),
            },
        ));
        #[expect(
            clippy::unwrap_used,
            reason = "a poisoned mutex in a test is the assertion"
        )]
        let text = String::from_utf8(written.lock().unwrap().clone()).unwrap();
        assert!(text.contains("degrade"), "{text}");
        assert!(!text.contains("run.start"), "{text}");
    }
}
