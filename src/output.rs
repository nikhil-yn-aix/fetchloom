//! what fetchloom prints. stdout carries the answer and nothing else, stderr carries the story.

mod progress;
mod style;

use std::borrow::Cow;
use std::fmt::Write as _;
use std::io::{IsTerminal, Write};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use anstyle::{AnsiColor, Style as Paint};
use indicatif::MultiProgress;
use serde::Serialize;

pub(crate) use progress::{Bar, Ticker, Wait};
pub(crate) use style::{ColorChoice, Style};

use crate::error::Error;
use crate::units;
use crate::work::Counts;

/// how much of the story to tell, from `-q` to `-vv`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Level {
    Quiet,
    Normal,
    Verbose,
    Debug,
}

/// the one writer to the terminal, so lines, bars and questions never trample each other.
pub(crate) struct Output {
    style: Style,
    level: Level,
    json: bool,
    bars: Option<MultiProgress>,
    warnings: Mutex<Vec<String>>,
}

/// the document `--json` prints, described in docs/formats.md.
#[derive(Serialize)]
struct Document<'a, T> {
    schema: u32,
    /// none when no command ran, as for `--version` or a command line that could not be read.
    command: Option<&'a str>,
    status: &'static str,
    result: Option<&'a T>,
    warnings: Vec<String>,
    errors: Vec<Problem<'a>>,
    work: WorkDone,
}

#[derive(Serialize)]
struct Problem<'a> {
    kind: &'static str,
    message: &'a str,
    details: &'a [String],
    help: Option<&'a str>,
}

#[derive(Serialize)]
struct WorkDone {
    #[serde(flatten)]
    counts: Counts,
    elapsed_ms: u64,
}

impl Output {
    /// `logs` says debug logs share stderr, which bars would garble.
    pub(crate) fn new(style: Style, level: Level, json: bool, logs: bool) -> Output {
        // indicatif colors through the console crate, which decides on its own unless told
        console::set_colors_enabled_stderr(style.color);
        let bars = (style.interactive && !json && !logs && level >= Level::Normal)
            .then(MultiProgress::new);
        Output {
            style,
            level,
            json,
            bars,
            warnings: Mutex::new(Vec::new()),
        }
    }

    /// a finished step: a right-aligned verb, then the facts.
    pub(crate) fn step(&self, verb: &str, text: &str) {
        if self.tells(Level::Normal) {
            let green = Paint::new().fg_color(Some(AnsiColor::Green.into())).bold();
            self.line(&format!("{green}{verb:>11}{green:#} {}", plain(text)));
        }
    }

    /// a difference found, in the same rhythm as steps but in the warning color.
    pub(crate) fn mark(&self, verb: &str, text: &str) {
        if self.tells(Level::Normal) {
            let yellow = Paint::new().fg_color(Some(AnsiColor::Yellow.into())).bold();
            self.line(&format!("{yellow}{verb:>11}{yellow:#} {}", plain(text)));
        }
    }

    /// detail for `-v`: where things are, which method was used, full hashes.
    pub(crate) fn detail(&self, text: &str) {
        if self.tells(Level::Verbose) {
            let dim = Paint::new().dimmed();
            self.line(&format!("{:>11} {dim}{}{dim:#}", "", plain(text)));
        }
    }

    pub(crate) fn warn(&self, text: &str) {
        self.warnings
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(text.to_owned());
        if self.tells(Level::Normal) {
            let yellow = Paint::new().fg_color(Some(AnsiColor::Yellow.into())).bold();
            self.line(&format!(
                "{yellow}{}{yellow:#} {}",
                self.style.warned(),
                plain(text)
            ));
        }
    }

    /// the closing line of a run that worked.
    pub(crate) fn done(&self, text: &str) {
        if self.tells(Level::Normal) {
            let green = Paint::new().fg_color(Some(AnsiColor::Green.into())).bold();
            self.line(&format!(
                "{green}{}{green:#} {}",
                self.style.done(),
                plain(text)
            ));
        }
    }

    pub(crate) fn hint(&self, text: &str) {
        if self.tells(Level::Normal) {
            let bold = Paint::new().bold();
            self.line(&format!("  {bold}hint:{bold:#} {}", plain(text)));
        }
    }

    /// the step a person usually takes after a run that worked, such as committing data.lock.
    pub(crate) fn next(&self, text: &str) {
        if self.shows_next() {
            let dim = Paint::new().dimmed();
            self.line(&format!("  {dim}next: {}{dim:#}", plain(text)));
        }
    }

    /// whether [`Output::next`] prints: only to a person at a terminal, never with `-q` or
    /// `--json`, or when stdout goes to a pipe or a file.
    fn shows_next(&self) -> bool {
        self.style.watched && self.tells(Level::Normal)
    }

    /// free text from a source, such as a title, cut to fit one line of the story.
    pub(crate) fn clip(&self, text: &str, most: usize) -> String {
        units::clip(text, most, self.style.more())
    }

    /// the answer on stdout, such as a path a script will read.
    pub(crate) fn answer(&self, text: &str) {
        if !self.json {
            let _ = writeln!(std::io::stdout().lock(), "{text}");
        }
    }

    /// a failure: the sentence, its supporting lines, and what to do next.
    pub(crate) fn error(&self, error: &Error) {
        if self.json {
            return;
        }
        let red = Paint::new().fg_color(Some(AnsiColor::Red.into())).bold();
        let bold = Paint::new().bold();
        let dim = Paint::new().dimmed();
        let mut text = format!(
            "{red}{}{red:#} {}",
            self.style.failed(),
            plain(&error.message)
        );
        for line in &error.details {
            let _ = write!(text, "\n  {}", plain(line));
        }
        if let Some(help) = &error.help {
            let _ = write!(text, "\n  {bold}help:{bold:#} {}", plain(help));
        }
        if self.level >= Level::Verbose {
            let _ = write!(text, "\n  {dim}kind: {}{dim:#}", error.kind.name());
        }
        self.line(&text);
    }

    /// a bar for one download, hidden unless a person is watching the terminal.
    pub(crate) fn bar(&self, message: String) -> Bar {
        match &self.bars {
            Some(bars) => Bar::add(bars, message, self.style.unicode),
            None => Bar::hidden(),
        }
    }

    /// a step that waits on something, such as `resolving zenodo:3608990`: a spinner with the
    /// time it has taken on a terminal, and plain lines off one once it takes more than a
    /// moment. it ends when dropped.
    pub(crate) fn wait(&self, verb: &'static str, what: String) -> Wait {
        let spinner = match &self.bars {
            Some(bars) => Bar::spin(
                bars,
                format!("{} {}", verb.to_lowercase(), plain(&what)),
                self.style.unicode,
            ),
            None => Bar::hidden(),
        };
        Wait {
            _spinner: spinner,
            _ticker: self.ticker(verb, what, [None]),
        }
    }

    /// plain lines about a batch of work where no bar shows it, such as `Downloading 3 files`.
    /// `sizes` holds each file's size, or none for one whose download tells it.
    pub(crate) fn ticker(
        &self,
        verb: &'static str,
        what: String,
        sizes: impl IntoIterator<Item = Option<u64>>,
    ) -> Ticker {
        if self.bars.is_some() || !self.tells(Level::Normal) {
            return Ticker::quiet();
        }
        let known = sizes
            .into_iter()
            .fold((0, 0), |(total, unknown), size| match size {
                Some(size) => (total + size, unknown),
                None => (total, unknown + 1),
            });
        let color = self.style.color;
        Ticker::start(verb, what, known, move |verb, text| {
            let cyan = Paint::new().fg_color(Some(AnsiColor::Cyan.into())).bold();
            write_line(color, &format!("{cyan}{verb:>11}{cyan:#} {}", plain(text)));
        })
    }

    /// asks a yes or no question on the terminal. none when nobody can answer, as in CI or a pipe.
    pub(crate) fn confirm(&self, question: &str) -> Option<bool> {
        if self.json || !self.style.interactive || !std::io::stdin().is_terminal() {
            return None;
        }
        // a failed read, such as ctrl-c at the prompt, counts as no
        let ask = || {
            dialoguer::Confirm::new()
                .with_prompt(question)
                .default(false)
                .interact()
                .unwrap_or(false)
        };
        Some(match &self.bars {
            Some(bars) => bars.suspend(ask),
            None => ask(),
        })
    }

    /// the counters at the end of a run, for `-v`.
    pub(crate) fn summary(&self, counts: Counts, elapsed: Duration) {
        let parts = [
            (counts.requests, "request", "requests"),
            (counts.retries, "retry", "retries"),
            (counts.resumes, "resume", "resumes"),
            (counts.objects_written, "new object", "new objects"),
            (counts.cache_hits, "cache hit", "cache hits"),
            (counts.reflinks, "reflink", "reflinks"),
            (counts.hardlinks, "hardlink", "hardlinks"),
            (counts.copies, "copy", "copies"),
        ];
        let mut text: Vec<String> = parts
            .iter()
            .filter(|(n, _, _)| *n > 0)
            .map(|(n, one, many)| units::count(*n, one, many))
            .collect();
        if counts.bytes_down > 0 {
            text.insert(
                1.min(text.len()),
                format!("{} down", units::size(counts.bytes_down)),
            );
        }
        text.push(format!("{} in all", units::duration(elapsed)));
        self.detail(&text.join(" · "));
    }

    /// prints the `--json` document for a run, whatever its outcome.
    pub(crate) fn document<T: Serialize>(
        &self,
        command: Option<&str>,
        outcome: Result<&T, &Error>,
        counts: Counts,
        elapsed: Duration,
    ) {
        if !self.json {
            return;
        }
        let errors = match outcome {
            Ok(_) => Vec::new(),
            Err(error) => vec![Problem {
                kind: error.kind.name(),
                message: &error.message,
                details: &error.details,
                help: error.help.as_deref(),
            }],
        };
        let document = Document {
            schema: 1,
            command,
            status: if outcome.is_ok() { "ok" } else { "error" },
            result: outcome.ok(),
            warnings: std::mem::take(
                &mut *self.warnings.lock().unwrap_or_else(PoisonError::into_inner),
            ),
            errors,
            work: WorkDone {
                counts,
                elapsed_ms: u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
            },
        };
        let mut out = std::io::stdout().lock();
        if serde_json::to_writer(&mut out, &document).is_ok() {
            let _ = writeln!(out);
        }
    }

    fn tells(&self, level: Level) -> bool {
        !self.json && self.level >= level
    }

    fn line(&self, text: &str) {
        let write = || write_line(self.style.color, text);
        match &self.bars {
            Some(bars) => bars.suspend(write),
            None => write(),
        }
    }
}

/// one line on stderr, with its colors only when they are on.
fn write_line(color: bool, text: &str) {
    let choice = if color {
        anstream::ColorChoice::Always
    } else {
        anstream::ColorChoice::Never
    };
    let mut err = anstream::AutoStream::new(std::io::stderr().lock(), choice);
    let _ = writeln!(err, "{text}");
}

/// `text` with control characters written out as escapes, so text from a server cannot move the
/// cursor or recolor the terminal.
fn plain(text: &str) -> Cow<'_, str> {
    if !text.contains(char::is_control) {
        return Cow::Borrowed(text);
    }
    let mut shown = String::with_capacity(text.len() + 8);
    for c in text.chars() {
        if c.is_control() {
            shown.extend(c.escape_default());
        } else {
            shown.push(c);
        }
    }
    Cow::Owned(shown)
}

#[cfg(test)]
mod tests {
    use super::{Level, Output, Style, plain};

    #[test]
    fn server_text_cannot_steer_the_terminal() {
        assert_eq!(plain("iris.csv · 4.1 KiB"), "iris.csv · 4.1 KiB");
        assert_eq!(plain("\u{1b}[2Jgone"), "\\u{1b}[2Jgone");
        assert_eq!(plain("two\nlines"), "two\\nlines");
    }

    #[test]
    fn the_next_step_is_only_for_a_person_at_a_terminal() {
        let shows = |watched, level, json| {
            let style = Style {
                color: false,
                unicode: true,
                interactive: false,
                watched,
            };
            Output::new(style, level, json, false).shows_next()
        };
        assert!(shows(true, Level::Normal, false));
        assert!(shows(true, Level::Verbose, false));
        assert!(!shows(true, Level::Quiet, false), "-q");
        assert!(!shows(true, Level::Normal, true), "--json");
        assert!(
            !shows(false, Level::Normal, false),
            "stdout or stderr is not a terminal"
        );
    }
}
