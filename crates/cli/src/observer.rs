//! Where the event stream goes, and the one renderer that consumes it.

use std::io::Write;
use std::path::Path;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use fetchloom_engine::event::{Event, EventPayload};
use fetchloom_engine::seam::observer::Observer;

use crate::surface::DisplayMode;

/// How often the aggregated progress line is redrawn.
pub const REDRAW_INTERVAL: Duration = Duration::from_millis(100);

/// An observer that writes the event stream as newline-delimited JSON.
pub struct EventStream {
    sink: Mutex<Box<dyn Write + Send>>,
}

impl EventStream {
    /// Opens an event stream at a path, or on standard output for `-`.
    ///
    /// # Errors
    ///
    /// Fails when the path cannot be opened for writing.
    pub fn open(target: &str) -> std::io::Result<Self> {
        let sink: Box<dyn Write + Send> = if target == "-" {
            Box::new(std::io::stdout())
        } else {
            Box::new(std::fs::File::create(Path::new(target))?)
        };
        Ok(Self {
            sink: Mutex::new(sink),
        })
    }
}

impl std::fmt::Debug for EventStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventStream").finish_non_exhaustive()
    }
}

impl Observer for EventStream {
    fn emit(&self, event: &Event) {
        let Ok(line) = serde_json::to_string(event) else {
            return;
        };
        let mut sink = self.sink.lock().unwrap_or_else(PoisonError::into_inner);
        let _ = writeln!(sink, "{line}");
        let _ = sink.flush();
    }
}

#[derive(Debug, Default)]
struct Progress {
    bytes: u64,
    entries: u64,
    last_drawn: Option<Instant>,
    drawn_width: usize,
}

/// The one renderer for the whole run.
#[derive(Debug)]
pub struct Renderer {
    mode: DisplayMode,
    animate: bool,
    progress: Mutex<Progress>,
}

impl Renderer {
    /// Builds the renderer for a display mode.
    #[must_use]
    pub fn new(mode: DisplayMode, animate: bool) -> Self {
        Self {
            mode,
            animate,
            progress: Mutex::new(Progress::default()),
        }
    }

    fn draw(&self, progress: &mut Progress, force: bool) {
        if self.mode != DisplayMode::Plain {
            return;
        }
        let now = Instant::now();
        if !force && !self.animate {
            return;
        }
        if !force
            && progress
                .last_drawn
                .is_some_and(|last| now.duration_since(last) < REDRAW_INTERVAL)
        {
            return;
        }
        progress.last_drawn = Some(now);

        let line = format!(
            "{} entries  {}",
            progress.entries,
            human_bytes(progress.bytes)
        );
        let mut stderr = std::io::stderr().lock();
        let padding = progress.drawn_width.saturating_sub(line.len());
        progress.drawn_width = line.len();
        if self.animate {
            let _ = write!(stderr, "\r{line}{:padding$}", "");
        } else {
            let _ = writeln!(stderr, "{line}");
        }
        let _ = stderr.flush();
    }

    fn finish_line(&self) {
        if self.mode == DisplayMode::Plain && self.animate {
            let mut stderr = std::io::stderr().lock();
            let _ = writeln!(stderr);
            let _ = stderr.flush();
        }
    }
}

impl Observer for Renderer {
    fn emit(&self, event: &Event) {
        if self.mode == DisplayMode::None {
            return;
        }
        let mut progress = self.progress.lock().unwrap_or_else(PoisonError::into_inner);
        match event.payload() {
            EventPayload::TransferProgress { bytes } => {
                progress.bytes = *bytes;
                self.draw(&mut progress, false);
            }
            EventPayload::ExtractEnd { entries, bytes, .. } => {
                progress.entries = *entries;
                progress.bytes = *bytes;
                self.draw(&mut progress, true);
            }
            EventPayload::TransferEnd { bytes, .. } => {
                progress.bytes = *bytes;
                self.draw(&mut progress, true);
            }
            EventPayload::RunEnd { .. } => {
                drop(progress);
                self.finish_line();
            }
            _ => {}
        }
    }
}

/// An observer that forwards every event to several observers.
pub struct Fanout {
    observers: Vec<Box<dyn Observer>>,
}

impl std::fmt::Debug for Fanout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Fanout")
            .field("observers", &self.observers.len())
            .finish()
    }
}

impl Fanout {
    /// Collects observers that every event is forwarded to, in order.
    #[must_use]
    pub fn new(observers: Vec<Box<dyn Observer>>) -> Self {
        Self { observers }
    }
}

impl Observer for Fanout {
    fn emit(&self, event: &Event) {
        for observer in &self.observers {
            observer.emit(event);
        }
    }
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    #[expect(
        clippy::cast_precision_loss,
        reason = "a byte count is shown to one decimal place, where the loss is invisible"
    )]
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[0])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}
