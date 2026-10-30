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
            self.line(&format!(
                "{green}{verb:>11}{green:#} {}",
                subject(&plain(text))
            ));
        }
    }

    /// a difference found, in the same rhythm as steps but in the warning color.
    pub(crate) fn mark(&self, verb: &str, text: &str) {
        if self.tells(Level::Normal) {
            let yellow = Paint::new().fg_color(Some(AnsiColor::Yellow.into())).bold();
            self.line(&format!(
                "{yellow}{verb:>11}{yellow:#} {}",
                subject(&plain(text))
            ));
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
                accented(&plain(text), Paint::new())
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
                accented(&plain(text), Paint::new())
            ));
        }
    }

    pub(crate) fn hint(&self, text: &str) {
        if self.tells(Level::Normal) {
            let bold = Paint::new().bold();
            self.line(&format!(
                "  {bold}hint:{bold:#} {}",
                accented(&plain(text), Paint::new())
            ));
        }
    }

    /// the step a person usually takes after a run that worked, such as committing data.lock.
    pub(crate) fn next(&self, text: &str) {
        if self.shows_next() {
            let dim = Paint::new().dimmed();
            self.line(&format!(
                "  {dim}next: {}{dim:#}",
                accented(&plain(text), dim)
            ));
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
            accented(&plain(&error.message), Paint::new())
        );
        for line in &error.details {
            let _ = write!(text, "\n  {dim}{}{dim:#}", accented(&plain(line), dim));
        }
        if let Some(help) = &error.help {
            let _ = write!(
                text,
                "\n  {bold}help:{bold:#} {}",
                accented(&plain(help), Paint::new())
            );
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
    pub(crate) fn confirm(&self, question: &str) -> Result<Option<bool>, Error> {
        if self.json || !self.style.interactive || !std::io::stdin().is_terminal() {
            return Ok(None);
        }
        let ask = || {
            let terminal = console::Term::stderr();
            let _ = terminal.write_str(&format!("{question} [y/N] "));
            // read ctrl-c as a key on both platforms instead of leaving a blocking read
            // waiting after the signal handler asks network tasks to stop.
            let answer = loop {
                match confirmation_key(terminal.read_key_raw()) {
                    Ok(None) => {}
                    answer => break answer,
                }
            };
            let _ = terminal.clear_line();
            if let Ok(Some(yes)) = &answer {
                let selected = if *yes { "yes" } else { "no" };
                let _ = terminal.write_line(&format!("{question} {selected}"));
            }
            answer
        };
        match &self.bars {
            Some(bars) => bars.suspend(ask),
            None => ask(),
        }
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

/// the one color for names and commands, wherever they appear. the stream drops it, like every
/// color, when color is off.
const ACCENT: Paint = Paint::new().fg_color(Some(anstyle::Color::Ansi(AnsiColor::Cyan)));

/// `text` with each command in backticks in the accent, then back to `around`. the backticks
/// stay, so a command still stands out without color.
fn accented(text: &str, around: Paint) -> String {
    let mut shown = String::with_capacity(text.len() + 16);
    let mut rest = text;
    while let Some(start) = rest.find('`') {
        let Some(length) = rest[start + 1..].find('`') else {
            break;
        };
        let end = start + length + 2;
        let _ = write!(
            shown,
            "{}{ACCENT}{}{ACCENT:#}{around}",
            &rest[..start],
            &rest[start..end]
        );
        rest = &rest[end..];
    }
    shown.push_str(rest);
    shown
}

/// a step's facts with what it is about, such as a file or a dataset, in the accent: the text
/// before the first `·`, or the whole text when it is one word.
fn subject(text: &str) -> String {
    match text.split_once(" · ") {
        Some((name, facts)) => format!("{ACCENT}{name}{ACCENT:#} · {facts}"),
        None if !text.is_empty() && !text.contains(' ') => format!("{ACCENT}{text}{ACCENT:#}"),
        None => accented(text, Paint::new()),
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

fn confirmation_key(key: std::io::Result<console::Key>) -> Result<Option<bool>, Error> {
    let key = match key {
        Ok(key) => key,
        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => console::Key::CtrlC,
        Err(_) => return Ok(Some(false)),
    };
    match key {
        console::Key::CtrlC => Err(Error::new(crate::error::Kind::Interrupted, "stopped")),
        console::Key::Char('y' | 'Y') => Ok(Some(true)),
        console::Key::Char('n' | 'N') | console::Key::Enter => Ok(Some(false)),
        _ => Ok(None),
    }
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
    use anstyle::Style as Paint;

    use super::{Level, Output, Style, accented, plain, subject};

    #[test]
    fn the_first_ctrl_c_at_a_question_is_interrupted() {
        for key in [
            Ok(console::Key::CtrlC),
            Err(std::io::ErrorKind::Interrupted.into()),
        ] {
            let error = super::confirmation_key(key).unwrap_err();
            assert_eq!(error.kind, crate::error::Kind::Interrupted);
            assert_eq!(error.kind.exit_code(), 130);
            assert_eq!(error.message, "stopped");
        }
    }

    #[test]
    fn a_question_keeps_yes_no_and_default_answers() {
        for (key, answer) in [
            (console::Key::Char('y'), Some(true)),
            (console::Key::Char('Y'), Some(true)),
            (console::Key::Char('n'), Some(false)),
            (console::Key::Char('N'), Some(false)),
            (console::Key::Enter, Some(false)),
            (console::Key::Unknown, None),
        ] {
            assert_eq!(super::confirmation_key(Ok(key)).unwrap(), answer);
        }
        assert_eq!(
            super::confirmation_key(Err(std::io::ErrorKind::UnexpectedEof.into())).unwrap(),
            Some(false)
        );
    }

    #[test]
    fn commands_in_backticks_take_the_accent_and_keep_their_backticks() {
        let dim = Paint::new().dimmed();
        assert_eq!(
            accented("run `fetchloom sync` again", Paint::new()),
            "run \u{1b}[36m`fetchloom sync`\u{1b}[0m again"
        );
        assert_eq!(
            accented("commit it; then `fetchloom sync`", dim),
            "commit it; then \u{1b}[36m`fetchloom sync`\u{1b}[0m\u{1b}[2m"
        );
        assert_eq!(accented("an odd ` mark", Paint::new()), "an odd ` mark");
    }

    #[test]
    fn a_step_names_its_subject_in_the_accent() {
        assert_eq!(
            subject("iris · 1 file in data.lock"),
            "\u{1b}[36miris\u{1b}[0m · 1 file in data.lock"
        );
        assert_eq!(
            subject("data/iris/iris.csv"),
            "\u{1b}[36mdata/iris/iris.csv\u{1b}[0m"
        );
        assert_eq!(
            subject("for another fetchloom working in this project"),
            "for another fetchloom working in this project"
        );
    }

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
