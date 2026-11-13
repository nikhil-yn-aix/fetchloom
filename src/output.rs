//! what fetchloom prints. stdout carries the answer and nothing else, stderr carries the story.

mod fit;
mod picker;
mod progress;
mod raw;
#[cfg(test)]
pub(crate) mod screen;
mod style;
mod warning;

use std::borrow::Cow;
use std::fmt::Write as _;
use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use anstyle::Style as Paint;
use indicatif::{MultiProgress, ProgressDrawTarget};
use serde::Serialize;

pub(crate) use picker::lead as picker_lead;
pub(crate) use progress::{Bar, Setup, Ticker, Wait};
use style::Tone;
pub(crate) use style::{ColorChoice, Style};
pub(crate) use warning::Warning;

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

/// what interrupts a run: ctrl-c at a terminal, or the SIGTERM a scheduler sends a job it ends.
/// a second of either quits at once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Interrupt {
    CtrlC,
    Terminate,
}

/// the one writer to the terminal, so lines, bars and questions never trample each other.
#[expect(
    clippy::struct_excessive_bools,
    reason = "each is a separate yes or no the run was given"
)]
pub(crate) struct Output {
    style: Style,
    level: Level,
    json: bool,
    bars: Option<MultiProgress>,
    /// lines told while a person answers a question, written once the answer is in. every line goes
    /// out holding it, so a first ctrl-c holding it keeps all others back.
    held: Arc<Mutex<Option<Vec<String>>>>,
    /// taken while a question waits, so two never share the terminal.
    question: Mutex<()>,
    warnings: Mutex<Vec<Warned>>,
    failed: Mutex<Option<Error>>,
    /// off with `FETCHLOOM_NO_HINTS=1`, for people who know the next step already.
    hints: bool,
    /// whether plain progress lines still go to stderr beside `--json`, as `--progress` asks.
    progress: bool,
    /// whether the run was asked to leave data.lock as it is, as `--locked` asks, so no fix may
    /// change it.
    locked: AtomicBool,
    /// set at the first ctrl-c, so every bar says the run is stopping.
    stopping: Arc<AtomicBool>,
    /// whether the interrupt heard last was the SIGTERM a scheduler sends, so the lines told for
    /// it say what a second SIGTERM does rather than ctrl-c.
    terminated: AtomicBool,
    /// the interrupt whose `Stopping` was told first, which the stop it asked names.
    stopped_by: std::sync::OnceLock<Interrupt>,
    /// whether report rows on stdout take color, which stdout decides apart from stderr.
    report_color: bool,
    /// whether the `--json` document is indented for a person, as at a terminal; a program
    /// reading a pipe gets it on one line.
    indented: bool,
    /// every bar made, so a first ctrl-c redraws each at once to say it is stopping.
    drawn: Mutex<Vec<indicatif::WeakProgressBar>>,
    /// how long work that is usually quick goes untold at a terminal.
    untold: Duration,
    /// how long a batch runs before its first plain line, or none when a test turns them off.
    moment: Option<Duration>,
    /// the last closing line, such as `Added iris`, and whether a finishing ctrl-c came after it: a
    /// terminal drops what it had not yet shown at a ctrl-c, so the line is told again at the end.
    closing: Mutex<(Option<String>, bool)>,
    /// every line told, in the order it went out, for tests.
    #[cfg(test)]
    said: Mutex<Vec<String>>,
    /// the width of the terminal stderr goes to, which a line wraps at between words, or none.
    columns: Option<usize>,
}

/// the output while a first ctrl-c is decided and told, from [`Output::alone`]: every other line
/// waits until it is dropped, so a line told by work that sees the stop never comes first.
pub(crate) struct Alone<'a> {
    output: &'a Output,
    held: MutexGuard<'a, Option<Vec<String>>>,
}

impl Alone<'_> {
    /// says that a first ctrl-c or SIGTERM was heard, and what a second one does. every bar says
    /// it is stopping from then on.
    pub(crate) fn stopping(&mut self) {
        let output = self.output;
        output.stopping.store(true, Ordering::Relaxed);
        let by = if output.terminated.load(Ordering::Relaxed) {
            Interrupt::Terminate
        } else {
            Interrupt::CtrlC
        };
        let _ = output.stopped_by.set(by);
        self.heard("Stopping", &format!("({})", output.again(true)));
        // each bar says so now, not at its next tick, which a busy machine can put off
        let drawn = output
            .drawn
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        for bar in drawn.iter().filter_map(indicatif::WeakProgressBar::upgrade) {
            bar.tick();
        }
    }

    /// says that a first ctrl-c or SIGTERM came once the run marked its work as finishing, so it
    /// finishes, and what a second one does. a closing line told before it is told again later.
    pub(crate) fn finishing(&mut self) {
        {
            let mut closing = self
                .output
                .closing
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            closing.1 = closing.0.is_some();
        }
        let again = self.output.again(false);
        self.heard("Finishing", &format!("what it started ({again})"));
    }

    /// says that a first ctrl-c or SIGTERM came once every byte of `count` downloads was in and
    /// the disk only keeps them, so the run finishes rather than download them again next time.
    pub(crate) fn finishing_downloads(&mut self, count: usize) {
        let which = if count == 1 {
            "the download is"
        } else {
            "the downloads are"
        };
        let again = self.output.again(false);
        self.heard("Finishing", &format!("({which} complete; {again})"));
    }

    /// a line told as a ctrl-c is heard, which clears what the terminal echoed of it. a question
    /// waiting for its answer keeps it with the lines it holds.
    fn heard(&mut self, verb: &str, text: &str) {
        let output = self.output;
        if output.tells(Level::Normal) {
            let line = output.prepared(&output.over_echo(&verb_line(
                verb,
                text,
                Tone::Warn,
                output.style.color,
            )));
            match self.held.as_mut() {
                Some(waiting) => waiting.push(line),
                None => output.write_lines(&[line]),
            }
        }
    }
}

/// work told once it lasts, as [`Output::lasting`] makes it, until dropped.
pub(crate) struct Lasting {
    _setup: Setup,
    _ticker: Ticker,
}

/// the document `--json` prints, described in docs/user/reference/json-output.md.
#[derive(Serialize)]
struct Document<'a, T> {
    schema: u32,
    /// none when no command ran, as for `--version` or a command line that could not be read.
    command: Option<&'a str>,
    status: &'static str,
    result: Option<DocumentResult<'a, T>>,
    warnings: Vec<Warned>,
    errors: Vec<Problem<'a>>,
    work: WorkDone,
}

#[derive(Serialize)]
#[serde(untagged)]
enum DocumentResult<'a, T> {
    Complete(&'a T),
    Partial(&'a serde_json::Value),
}

/// a warning as `--json` lists it: its kind, what it says, the paths or names it is about, and
/// what to do, as an error's are.
#[derive(Clone, Debug, Serialize)]
struct Warned {
    kind: &'static str,
    message: String,
    details: Vec<String>,
    help: Option<String>,
}

impl Warned {
    /// the warning as one sentence, as a person reads it.
    #[cfg(test)]
    fn said(&self) -> String {
        match &self.help {
            Some(help) => format!("{}; {help}", self.message),
            None => self.message.clone(),
        }
    }
}

#[derive(Serialize)]
struct Problem<'a> {
    kind: &'static str,
    message: &'a str,
    details: &'a [String],
    help: Option<&'a str>,
    /// the command `help` names, ready to run, for a script or an agent to act on.
    fix: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    failures: Option<&'a [crate::error::FileFailure]>,
}

/// a quoted command fetchloom takes whole, as cli reads it: whether a script may run it with no
/// person to ask, and whether it may write data.lock.
pub(crate) struct Runnable {
    pub(crate) harmless: bool,
    pub(crate) changes_lock: bool,
}

/// the first `fetchloom` command quoted in `help`, without its backticks, when a script may run it
/// as the next step. json-output.md's `errors[].fix` lists what never is: an example, a guess, a
/// person's choice, one of the ways offered with `or`, or under `locked` a write to data.lock.
fn fix(help: &str, locked: bool, judge: fn(&str) -> Option<Runnable>) -> Option<&str> {
    const NOT_A_FIX: [&str; 9] = [
        "as in",
        "such as",
        "for example",
        "e.g.",
        "did you mean",
        "own terminal, run",
        "delete it and run",
        "by hand, then run",
        "errors above, then run",
    ];
    let mut before = String::new();
    let mut offset = 0;
    for (index, part) in help.split('`').enumerate() {
        let at = offset;
        offset += part.len() + 1;
        if index % 2 == 0 {
            before.push_str(part);
            continue;
        }
        let lead = before
            .trim_end_matches([' ', ':', ','])
            .to_ascii_lowercase();
        let example = NOT_A_FIX.iter().any(|marker| lead.ends_with(marker));
        if !example
            && part.starts_with("fetchloom ")
            && !part.contains(['<', '>'])
            && let Some(runnable) = judge(part)
        {
            if !runnable.harmless {
                return None;
            }
            if !locked || !runnable.changes_lock {
                return (!offers_choice(help, at)).then_some(part);
            }
        }
        before.push_str(part);
    }
    None
}

/// whether the clause of `help` holding the quoted command at byte `at` offers a choice of ways:
/// `or` after a comma, at its start, or between two quoted commands. a clause ends at `.`, `:`,
/// `;` or `?`, so an `or` in a reason before it, as "was refused, or has no access", offers none.
fn offers_choice(help: &str, at: usize) -> bool {
    const ENDS: [&str; 4] = [". ", ": ", "; ", "? "];
    // what backticks quote is blanked, byte for byte, so only the prose is read
    let mut prose = String::with_capacity(help.len());
    let mut quoted = false;
    for c in help.chars() {
        if c == '`' {
            quoted = !quoted;
            prose.push(c);
        } else if quoted {
            prose.extend(std::iter::repeat_n('_', c.len_utf8()));
        } else {
            prose.push(c);
        }
    }
    let start = ENDS
        .iter()
        .filter_map(|end| prose[..at].rfind(end).map(|found| found + end.len()))
        .max()
        .unwrap_or(0);
    let end = ENDS
        .iter()
        .filter_map(|end| prose[at..].find(end).map(|found| at + found))
        .min()
        .unwrap_or(prose.len());
    let clause = &prose[start..end];
    clause.contains(", or ") || clause.starts_with("or ") || clause.contains("` or `")
}

#[derive(Serialize)]
struct WorkDone {
    #[serde(flatten)]
    counts: Counts,
    elapsed_ms: u64,
}

/// the widest a list of datasets lets its reference column grow. a longer reference runs past it
/// on its own row, so one long name does not push every other title and fact off the terminal.
pub(crate) const REFERENCE_COLUMN: usize = 48;

impl Output {
    /// `logs` says debug logs share stderr, which bars would garble.
    pub(crate) fn new(style: Style, level: Level, json: bool, logs: bool) -> Output {
        let unread = style::load_palette();
        // indicatif colors through the console crate, which decides on its own unless told
        console::set_colors_enabled_stderr(style.color);
        // a screen reader reads every redraw again, so accessible output tells progress in lines
        let bars =
            (style.interactive && !style.accessible && !json && !logs && level >= Level::Normal)
                .then(|| MultiProgress::with_draw_target(progress::terminal()));
        let output = Output {
            style,
            level,
            json,
            bars,
            held: Arc::default(),
            question: Mutex::new(()),
            warnings: Mutex::new(Vec::new()),
            failed: Mutex::new(None),
            hints: true,
            progress: false,
            locked: AtomicBool::new(false),
            stopping: Arc::default(),
            terminated: AtomicBool::new(false),
            stopped_by: std::sync::OnceLock::new(),
            report_color: Style::stdout_colored(),
            indented: std::io::stdout().is_terminal(),
            drawn: Mutex::new(Vec::new()),
            untold: progress::UNTOLD,
            moment: progress::moment(),
            closing: Mutex::new((None, false)),
            #[cfg(test)]
            said: Mutex::new(Vec::new()),
            columns: None,
        };
        if !unread.is_empty() {
            output.warn_with(
                Warning::SettingsColorsUnread,
                &format!(
                    "FETCHLOOM_COLORS: kept the default for {}",
                    unread.join(", ")
                ),
                "entries look like done=green, work=214 or accent=#d787ff",
            );
        }
        output
    }

    /// a finished step: a right-aligned verb, then the facts.
    pub(crate) fn step(&self, verb: &str, text: &str) {
        if self.tells(Level::Normal) {
            self.line(&verb_line(verb, text, verb_color(verb), self.style.color));
        }
    }

    /// a difference found, in the same rhythm as steps but in the color of its kind.
    pub(crate) fn mark(&self, verb: &str, text: &str) {
        if self.tells(Level::Normal) {
            self.line(&verb_line(verb, text, difference(verb), self.style.color));
        }
    }

    /// detail for `-v`: where things are, which method was used, full hashes.
    pub(crate) fn detail(&self, text: &str) {
        if self.tells(Level::Verbose) {
            self.dim_line(text);
        }
    }

    /// one of the things a step brought, listed dim under it, such as a file `get` fetched.
    pub(crate) fn item(&self, text: &str) {
        if self.tells(Level::Normal) {
            self.dim_line(text);
        }
    }

    /// a dim line under the steps that needs nothing done, such as an account nobody signed in to.
    fn dim_line(&self, text: &str) {
        let dim = paint(self.style.color, Paint::new().dimmed());
        self.line(&format!("{:>11} {dim}{}{dim:#}", "", plain(text)));
    }

    /// a step that matters little, all dimmed, with its verb in the verb column.
    pub(crate) fn faint_step(&self, verb: &str, text: &str) {
        if self.tells(Level::Normal) {
            let dim = paint(self.style.color, Paint::new().dimmed());
            self.line(&format!("{dim}{verb:>11} {}{dim:#}", plain(text)));
        }
    }

    /// [`Output::warned`] under another name.
    #[cfg(test)]
    pub(crate) fn told(&self) -> Vec<String> {
        self.warned()
    }

    /// a warning of `kind`, which `--json` names so a script can tell it from the others.
    pub(crate) fn warn(&self, kind: Warning, text: &str) {
        self.warn_detailed(kind, text, None, std::iter::empty::<&str>());
    }

    /// a warning and what to do about it, the help on a line of its own as an error's is, so
    /// neither runs long. `--json` gives the help a field of its own.
    pub(crate) fn warn_with(&self, kind: Warning, text: &str, help: &str) {
        self.warn_detailed(kind, text, Some(help), std::iter::empty::<&str>());
    }

    /// a warning with the paths or names it is about, which `--json` lists in its `details`. a
    /// person reads them, if at all, in the lines its caller tells under it.
    pub(crate) fn warn_detailed(
        &self,
        kind: Warning,
        text: &str,
        help: Option<&str>,
        details: impl IntoIterator<Item = impl std::fmt::Display>,
    ) {
        self.warnings
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Warned {
                kind: kind.name(),
                message: text.to_owned(),
                details: details
                    .into_iter()
                    .map(|detail| detail.to_string())
                    .collect(),
                help: help.map(str::to_owned),
            });
        // a warning is about the data, as a kept edit is, so -q keeps it as it keeps errors
        if self.tells(Level::Quiet) {
            let yellow = toned(self.style.color, Tone::Warn);
            let said = accented(&plain(text), Paint::new(), self.style.color);
            self.line(&match help {
                Some(help) => format!(
                    "{yellow}warning:{yellow:#} {said}\n{}",
                    help_line(help, self.style.color)
                ),
                None => format!("{yellow}warning:{yellow:#} {said}"),
            });
        }
    }

    /// every line told so far, for tests.
    #[cfg(test)]
    pub(crate) fn said(&self) -> Vec<String> {
        self.said
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// for tests elsewhere: an output whose bars draw on `screen`, as a terminal's would, and
    /// which tells lasting work at once.
    #[cfg(test)]
    pub(crate) fn on_screen(screen: impl indicatif::TermLike + 'static) -> Output {
        let style = Style {
            color: false,
            unicode: false,
            interactive: false,
            watched: false,
            links: false,
            accessible: false,
        };
        let mut output = Output::new(style, Level::Normal, false, false);
        output.bars = Some(MultiProgress::with_draw_target(
            ProgressDrawTarget::term_like(Box::new(screen)),
        ));
        output.untold = Duration::ZERO;
        output
    }

    /// the warnings told so far, each as a person reads it, for tests.
    #[cfg(test)]
    pub(crate) fn warned(&self) -> Vec<String> {
        self.warnings
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(Warned::said)
            .collect()
    }

    /// the closing line of a run that worked.
    pub(crate) fn done(&self, verb: &str, text: &str) {
        if self.tells(Level::Normal) {
            let line = verb_line(verb, text, Tone::Done, self.style.color);
            self.closing
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .0 = Some(line.clone());
            self.line(&line);
        }
    }

    /// tells the closing line once more when a ctrl-c that let the run finish came after it,
    /// since the terminal may have dropped it then.
    pub(crate) fn retell(&self) {
        let closing = self
            .closing
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        if let (Some(line), true) = closing {
            self.line(&line);
        }
    }

    /// wraps each line told on stderr between words at `columns`, the width of the terminal it
    /// goes to, so the terminal never wraps one inside a word.
    pub(crate) fn fitted(mut self, columns: Option<usize>) -> Self {
        self.columns = columns;
        self
    }

    pub(crate) fn without_hints(mut self, off: bool) -> Self {
        self.hints = !off;
        self
    }

    /// keeps the plain progress lines on stderr under `--json`, as `--progress` asks, so a
    /// script that reads the one document still shows a person how a long run goes.
    pub(crate) fn with_progress(mut self, on: bool) -> Self {
        self.progress = on;
        self
    }

    /// marks the run as one asked to leave data.lock as it is, as `--locked` asks, so `--json`
    /// offers no fix that would change it.
    pub(crate) fn leave_lock(&self) {
        self.locked.store(true, Ordering::Relaxed);
    }

    pub(crate) fn hint(&self, text: &str) {
        if self.shows_hints() {
            self.line(&hint_line(text, self.style.color));
        }
    }

    /// whether a hint would print, so work done only to find one can be skipped.
    pub(crate) fn shows_hints(&self) -> bool {
        self.hints && self.tells(Level::Normal)
    }

    /// whether [`Output::detail`] prints, as with `-v`.
    pub(crate) fn shows_details(&self) -> bool {
        self.tells(Level::Verbose)
    }

    /// whether [`Output::item`] prints: not with `-q` or `--json`.
    pub(crate) fn shows_items(&self) -> bool {
        self.tells(Level::Normal)
    }

    /// the step a person usually takes after a run that worked, such as committing data.lock.
    pub(crate) fn next(&self, text: &str) {
        if self.shows_next() {
            let dim = paint(self.style.color, Paint::new().dimmed());
            self.line(&format!(
                "  {dim}next: {}{dim:#}",
                accented(&plain(text), dim, self.style.color)
            ));
        }
    }

    /// whether [`Output::next`] prints: only to a person at a terminal, never with `-q` or
    /// `--json`, or when stdout goes to a pipe or a file.
    pub(crate) fn shows_next(&self) -> bool {
        self.hints && self.for_person()
    }

    /// whether a person reads both streams at a terminal, with the story on.
    fn for_person(&self) -> bool {
        self.style.watched && self.tells(Level::Normal)
    }

    /// free text from a source, such as a title, cut to fit one line of the story.
    pub(crate) fn clip(&self, text: &str, most: usize) -> String {
        units::clip(text, most, self.more())
    }

    /// what ends a text cut short: `…`, or `...` on a terminal that shows only ascii.
    pub(crate) fn more(&self) -> &'static str {
        self.style.more()
    }

    /// what parts the facts on a line: `·`, or `-` on a terminal that shows only ascii.
    pub(crate) fn dot(&self) -> &'static str {
        self.style.dot()
    }

    /// whether the terminal shows unicode symbols, not only ascii.
    pub(crate) fn draws_unicode(&self) -> bool {
        self.style.unicode
    }

    /// the width of the terminal a person watches the story on, or none for a pipe or a file.
    pub(crate) fn width(&self) -> Option<usize> {
        self.style
            .interactive
            .then(|| usize::from(console::Term::stderr().size().1))
    }

    /// the width of stdout when it is a terminal, or none when a script reads it.
    pub(crate) fn answer_width(&self) -> Option<usize> {
        self.style
            .watched
            .then(|| usize::from(console::Term::stdout().size().1))
    }

    /// one match of a search: what to type, padded to `pad`, then its padded title and dim facts,
    /// the facts on a line of their own when `below`. a narrow terminal cuts the end, not the name.
    pub(crate) fn candidate(
        &self,
        typed: &str,
        pad: usize,
        title: &str,
        (facts, below): (&str, bool),
    ) {
        if !self.tells(Level::Normal) {
            return;
        }
        if below {
            self.candidate(typed, pad, title, ("", false));
            let dim = paint(self.style.color, Paint::new().dimmed());
            self.line(&format!("{:>11} {dim}{}{dim:#}", "", plain(facts)));
            return;
        }
        let color = self.style.color;
        let accent = paint(color, accent());
        let dim = paint(color, Paint::new().dimmed());
        let typed = plain(typed);
        let title = plain(title);
        let head = format!("{:>11} {accent}{typed:pad$}{accent:#}", "");
        let head_width = 12 + typed.chars().count().max(pad);
        let width = self.width().unwrap_or(usize::MAX);
        let mut line = if head_width > width {
            format!("    {accent}{typed}{accent:#}")
        } else {
            head
        };
        if head_width <= width {
            let room = width - head_width;
            let named = if title.is_empty() {
                String::new()
            } else {
                format!("  {title}")
            };
            let told = if facts.is_empty() {
                String::new()
            } else {
                format!("  {facts}")
            };
            let mut rest = format!("{named}{told}");
            if console::measure_text_width(&rest) > room {
                rest = console::truncate_str(&rest, room, self.style.more()).into_owned();
            }
            if rest.trim().chars().count() > 3 {
                let split = if rest.starts_with(&named) {
                    named.len()
                } else {
                    rest.len()
                };
                let (named, told) = rest.split_at(split);
                let _ = write!(line, "{named}{dim}{told}{dim:#}");
            }
        }
        self.line(line.trim_end());
    }

    /// the answer on stdout, such as a path a script will read.
    pub(crate) fn answer(&self, text: &str) {
        if !self.json
            && let Err(error) = stdout(|out| writeln!(out, "{text}"))
        {
            self.failed
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get_or_insert(error);
        }
    }

    /// whether a person watches stderr, so a list there is read rather than logged twice.
    pub(crate) fn watched_story(&self) -> bool {
        self.style.interactive
    }

    /// a dim remark for a person at a terminal, never for a script, such as which project a
    /// command found above the current folder.
    pub(crate) fn aside(&self, text: &str) {
        if self.for_person() {
            let dim = paint(self.style.color, Paint::new().dimmed());
            self.line(&format!("  {dim}{}{dim:#}", plain(text)));
        }
    }

    /// an answer the story already showed, printed only when a script reads stdout or `-q`
    /// hid the story.
    pub(crate) fn answer_untold(&self, text: &str) {
        if !self.for_person() {
            self.answer(text);
        }
    }

    /// a failure: the sentence, its supporting lines, and what to do next. a cancel the person
    /// chose, or a stop they asked for, is one line of what was cancelled or where it stopped.
    pub(crate) fn error(&self, error: &Error) {
        if !self.json {
            self.line(&failure_text(error, self.level, self.style.color));
        }
    }

    /// a bar for one download, hidden unless a person is watching the terminal.
    pub(crate) fn bar(&self, verb: &'static str, message: String) -> Bar {
        let Some(bars) = &self.bars else {
            return Bar::hidden();
        };
        let bar = Bar::add(
            bars,
            verb,
            message,
            (self.style.color, self.style.unicode),
            self.stopping.clone(),
        );
        if let Some(weak) = bar.weak() {
            let mut drawn = self.drawn.lock().unwrap_or_else(PoisonError::into_inner);
            drawn.retain(|bar| bar.upgrade().is_some());
            drawn.push(weak);
        }
        bar
    }

    /// a step before a command's own work, as `verb` and `what`, such as `Locking the project`: a
    /// debug line as it begins, and at a terminal a live line once it lasts a second.
    pub(crate) fn setup(&self, verb: &'static str, what: &'static str) -> Setup {
        tracing::debug!(step = %format!("{verb} {what}").to_lowercase(), "setting up");
        self.untold(verb, what.to_owned())
    }

    /// a live line for `verb` and `what` once they last [`Output::untold`], at a terminal.
    fn untold(&self, verb: &'static str, what: String) -> Setup {
        match &self.bars {
            Some(bars) => Setup::after(
                self.untold,
                bars,
                (verb, what),
                (self.style.color, self.style.unicode),
                self.stopping.clone(),
            ),
            None => Setup::quiet(),
        }
    }

    /// work that is usually quick but grows with its size, such as placing many files: a live line
    /// once it lasts a second, or where no bar shows, a long batch's plain lines.
    pub(crate) fn lasting(&self, verb: &'static str, what: String) -> Lasting {
        Lasting {
            _setup: self.untold(verb, what.clone()),
            _ticker: self.ticker(verb, what, [None]),
        }
    }

    /// a wait for another program, which a person may sit through for a while: on a terminal a
    /// live line that counts its seconds until dropped, elsewhere one line, as a log needs.
    pub(crate) fn waiting(&self, verb: &'static str, text: &str) -> Bar {
        if self.bars.is_some() {
            self.bar(verb, text.to_owned())
        } else {
            self.step(verb, text);
            Bar::hidden()
        }
    }

    /// a wait ends when dropped, with elapsed time on a terminal and occasional lines elsewhere.
    pub(crate) fn wait(&self, verb: &'static str, what: String) -> Wait {
        Wait {
            _bar: self.bar(verb, what.clone()),
            _ticker: self.ticker(verb, what, [None]),
        }
    }

    /// plain lines about a batch where no bar shows it, such as `Downloading 3 files`, beside
    /// `--json` too with `--progress`. `sizes` holds each file's size, or none for one not known.
    pub(crate) fn ticker(
        &self,
        verb: &'static str,
        what: String,
        sizes: impl IntoIterator<Item = Option<u64>>,
    ) -> Ticker {
        let told = self.level >= Level::Normal && (!self.json || self.progress);
        let Some(moment) = self.moment.filter(|_| self.bars.is_none() && told) else {
            return Ticker::quiet();
        };
        let known = sizes
            .into_iter()
            .fold((0, 0), |(total, unknown), size| match size {
                Some(size) => (u64::saturating_add(total, size), unknown),
                None => (total, unknown + 1),
            });
        let color = self.style.color;
        let unicode = self.style.unicode;
        let columns = self.columns;
        let held = Arc::clone(&self.held);
        Ticker::start(verb, what, known, moment, move |verb, text| {
            let line = verb_line(verb, text, Tone::Work, color);
            let line = shown(&line, unicode);
            let line = match columns {
                Some(columns) => fit::fit(&line, columns),
                None => line.into_owned(),
            };
            // a question holds the lines told meanwhile, as [`Output::asking`] says
            let mut waiting = held.lock().unwrap_or_else(PoisonError::into_inner);
            match waiting.as_mut() {
                Some(lines) => lines.push(line),
                None => write_line(color, &line),
            }
        })
    }

    /// asks a yes or no question on the terminal. none when nobody can answer, as in CI or a pipe.
    pub(crate) fn confirm(&self, question: &str) -> Result<Option<bool>, Error> {
        self.ask_yes_no(question, false)
    }

    /// whether `--json` asked for one document on stdout.
    pub(crate) fn json(&self) -> bool {
        self.json
    }

    /// whether a person can answer a question here: stderr and stdin are terminals, outside CI,
    /// and `--json` was not asked for. `FETCHLOOM_ACCESSIBLE` asks at a dumb terminal too.
    pub(crate) fn can_ask(&self) -> bool {
        !self.json
            && (self.style.interactive || self.style.accessible)
            && std::io::stdin().is_terminal()
    }

    /// a yes or no question whose enter key means `default`, or none when nobody can answer. an
    /// answer that is neither yes nor no asks it again.
    pub(crate) fn ask_yes_no(&self, question: &str, default: bool) -> Result<Option<bool>, Error> {
        if !self.can_ask() {
            return Ok(None);
        }
        let ask = || {
            let terminal = console::Term::stderr();
            let choices = if default { "[Y/n]" } else { "[y/N]" };
            // wrapped between words at the terminal's width, as the story's lines are
            let asked = format!("{question} {choices}");
            let asked = match self.columns {
                Some(columns) => fit::fit(&asked, columns),
                None => asked,
            };
            loop {
                let _ = terminal.write_str(&format!("{asked} "));
                let mut shown = 0_usize;
                // read ctrl-c as a key on both platforms instead of leaving a blocking read
                // waiting after the signal handler asks network tasks to stop
                let answer = answer(
                    || terminal.read_key_raw(),
                    |edit| {
                        match edit {
                            Edit::Typed(_) => shown += 1,
                            Edit::Erased(_) => shown -= 1,
                        }
                        echo(&terminal, edit, self.style.interactive);
                    },
                    default,
                );
                // enter alone shows what it chose, after the question, which may have wrapped
                let chosen = match answer {
                    Ok(Some(true)) if shown == 0 => "yes",
                    Ok(Some(false)) if shown == 0 => "no",
                    _ => "",
                };
                let _ = terminal.write_line(chosen);
                if !matches!(answer, Ok(None)) {
                    return answer;
                }
                let _ = terminal.write_line("type y or n, or press ctrl-c to cancel");
            }
        };
        self.asking(ask)
    }

    /// a line typed at the terminal stdin is, shown as nothing: a key `--with-token` reads where
    /// no pipe brings it. a person sees `prompt`, then the masked key once enter ends it; `--json`
    /// writes nothing on stderr.
    pub(crate) fn read_hidden(&self, prompt: &str) -> Result<String, Error> {
        raw::question(|| self.hidden(prompt))
    }

    /// [`Output::read_hidden`] at a terminal held for it.
    fn hidden(&self, prompt: &str) -> Result<String, Error> {
        let terminal = console::Term::stderr();
        let tone = paint(self.style.color, accent().bold());
        let label = format!("{tone}{prompt:>11}{tone:#} ");
        if !self.json {
            let _ = terminal.write_str(&label);
        }
        let text = match typed_line(|| terminal.read_key_raw(), |_| {}) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {
                return Err(Error::new(crate::error::Kind::Interrupted, "stopped")
                    .cancelled("the question"));
            }
            Err(err) => return Err(Error::io("could not read from the terminal", &err)),
        };
        if !self.json {
            // keys read one by one bring no line break of their own
            let _ = terminal.write_line("");
            if !text.trim().is_empty() {
                if self.style.interactive && !self.style.accessible {
                    let _ = terminal.clear_last_lines(1);
                }
                let _ = terminal.write_line(&format!("{label}{}", masked(&text)));
            }
        }
        Ok(text)
    }

    /// asks for a line of text, such as a username, or none when nobody can answer. `hidden` shows
    /// nothing until enter, then a masked echo; a blank answer is asked again, twice at most.
    pub(crate) fn ask_text(&self, prompt: &str, hidden: bool) -> Result<Option<String>, Error> {
        if !self.can_ask() {
            return Ok(None);
        }
        let ask = || {
            let terminal = console::Term::stderr();
            let tone = paint(self.style.color, accent().bold());
            let label = format!("{tone}{prompt:>11}{tone:#} ");
            for _ in 0..3 {
                let _ = terminal.write_str(&label);
                let read = typed_line(
                    || terminal.read_key_raw(),
                    |edit| {
                        if !hidden {
                            echo(&terminal, edit, self.style.interactive);
                        }
                    },
                );
                // keys read one by one bring no line break of their own
                let _ = terminal.write_line("");
                // a password may start or end with a space; a visible answer, such as a name,
                // may not
                let text = match read {
                    Ok(text) if hidden => text,
                    Ok(text) => text.trim().to_owned(),
                    Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {
                        return Err(Error::new(crate::error::Kind::Interrupted, "stopped")
                            .cancelled("the question"));
                    }
                    Err(err) => return Err(Error::io("could not read from the terminal", &err)),
                };
                let blank = text.trim().is_empty();
                if hidden && !blank {
                    // a terminal that reads no escape codes keeps the empty line it showed, and so
                    // does a screen reader's, which would read the line again
                    if self.style.interactive && !self.style.accessible {
                        let _ = terminal.clear_last_lines(1);
                    }
                    let _ = terminal.write_line(&format!("{label}{}", masked(&text)));
                }
                if !blank {
                    return Ok(Some(text));
                }
                let paste = if cfg!(windows) {
                    "ctrl+v or a right click"
                } else {
                    "ctrl+shift+v or cmd+v"
                };
                let _ = terminal.write_line(&format!(
                    "{:>11} nothing arrived; paste with {paste}, then press enter",
                    ""
                ));
            }
            Ok(Some(String::new()))
        };
        self.asking(ask)
    }

    /// a line fetchloom wrote itself, such as sign-in steps holding [`Output::link`]s. text from
    /// elsewhere goes through the other methods, which escape control characters.
    pub(crate) fn say(&self, text: &str) {
        if self.tells(Level::Normal) {
            self.line(text);
        }
    }

    /// `url` as a link a person can click where the terminal supports it, always showing the
    /// address itself, so it can be copied anywhere.
    pub(crate) fn link(&self, url: &str) -> String {
        let url = plain(url);
        if self.style.links {
            format!("\u{1b}]8;;{url}\u{1b}\\{url}\u{1b}]8;;\u{1b}\\")
        } else {
            url.into_owned()
        }
    }

    /// asks which option to use, or returns none when nobody can answer. accessible output lists
    /// the options once, numbered, and reads the number typed.
    pub(crate) fn choose(
        &self,
        question: &str,
        options: &[String],
        footer: &str,
    ) -> Result<Option<usize>, Error> {
        if !self.can_ask() {
            return Ok(None);
        }
        let ask = || {
            if self.style.accessible {
                picker::numbered(question, options, footer, self.style)
            } else {
                picker::choose(question, options, footer, self.style)
            }
        };
        self.asking(ask)
    }

    /// the counters at the end of a run, for `-v`.
    pub(crate) fn summary(&self, counts: Counts, elapsed: Duration) {
        self.detail(&work_line(counts, elapsed));
    }

    /// prints the `--json` document for a run, whatever its outcome. fails for an unencodable
    /// result or a failed stdout write, earlier text answers' included.
    pub(crate) fn document<T: Serialize>(
        &self,
        command: Option<&str>,
        outcome: Result<&T, &Error>,
        counts: Counts,
        elapsed: Duration,
        judge: fn(&str) -> Option<Runnable>,
    ) -> Result<(), Error> {
        if !self.json {
            return self
                .failed
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take()
                .map_or(Ok(()), Err);
        }
        let errors = match outcome {
            Ok(_) => Vec::new(),
            Err(error) => vec![Problem {
                kind: error.kind.name(),
                message: &error.message,
                details: &error.details,
                help: error.help.as_deref(),
                fix: error
                    .help
                    .as_deref()
                    .and_then(|help| fix(help, self.locked.load(Ordering::Relaxed), judge)),
                failures: (!error.failures.is_empty()).then_some(&*error.failures),
            }],
        };
        let document = Document {
            schema: 1,
            command,
            status: if outcome.is_ok() { "ok" } else { "error" },
            result: match outcome {
                Ok(result) => Some(DocumentResult::Complete(result)),
                Err(error) => error.result.as_deref().map(DocumentResult::Partial),
            },
            warnings: std::mem::take(
                &mut *self.warnings.lock().unwrap_or_else(PoisonError::into_inner),
            ),
            errors,
            work: WorkDone {
                counts,
                elapsed_ms: u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
            },
        };
        // encoding can fail on a non-unicode path; stdout cannot retract a partial document
        let encoded = if self.indented {
            serde_json::to_vec_pretty(&document)
                .map(|bytes| Pieces::from(escaped_for_terminal(bytes)))
        } else {
            let mut pieces = Pieces::default();
            serde_json::to_writer(&mut pieces, &document).map(|()| pieces)
        };
        let pieces = match encoded {
            Ok(pieces) => pieces,
            Err(cause) => {
                let error = Error::new(
                    crate::error::Kind::LocalIo,
                    format!("could not encode the JSON report: {cause}"),
                )
                .help("use UTF-8 paths for JSON output, or run without --json");
                {
                    let mut warnings = self.warnings.lock().unwrap_or_else(PoisonError::into_inner);
                    let mut previous = document.warnings;
                    previous.append(&mut warnings);
                    *warnings = previous;
                }
                self.document::<()>(command, Err(&error), counts, elapsed, judge)?;
                return Err(error);
            }
        };
        stdout(|out| {
            for slice in pieces.slices() {
                out.write_all(slice)?;
            }
            out.write_all(b"\n")
        })
        .inspect_err(|error| {
            // stdout is unusable, so its failure can only be reported on stderr
            self.line(&error_text(error, self.level, self.style.color));
        })
    }

    fn tells(&self, level: Level) -> bool {
        !self.json && self.level >= level
    }

    /// runs `ask`, a question to a person, with the bars cleared and hidden but free to move, since
    /// suspending them would stall every download's progress until the answer. lines told wait.
    fn asking<R>(&self, ask: impl FnOnce() -> R) -> R {
        let _alone = self.question.lock().unwrap_or_else(PoisonError::into_inner);
        *self.held.lock().unwrap_or_else(PoisonError::into_inner) = Some(Vec::new());
        if let Some(bars) = &self.bars {
            let _ = bars.clear();
            bars.set_draw_target(ProgressDrawTarget::hidden());
        }
        let answer = raw::question(ask);
        if let Some(bars) = &self.bars {
            bars.set_draw_target(progress::terminal());
        }
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        let lines = held.take().unwrap_or_default();
        self.write_lines(&lines);
        answer
    }

    /// the output for a first ctrl-c or SIGTERM to decide and tell what it does while every other
    /// line waits, so no line of the stopping work comes before `Stopping`.
    pub(crate) fn alone(&self) -> Alone<'_> {
        Alone {
            output: self,
            held: self.held.lock().unwrap_or_else(PoisonError::into_inner),
        }
    }

    /// says which interrupt came last, ctrl-c or a SIGTERM, whose second one the lines told
    /// from then on name.
    pub(crate) fn interrupted_by(&self, interrupt: Interrupt) {
        self.terminated
            .store(interrupt == Interrupt::Terminate, Ordering::Relaxed);
    }

    pub(crate) fn stopped_by(&self) -> Option<Interrupt> {
        self.stopped_by.get().copied()
    }

    /// what quits at once after the interrupt heard last, worded for a person to `press` it.
    fn again(&self, press: bool) -> &'static str {
        match (self.terminated.load(Ordering::Relaxed), press) {
            (true, _) => "a second SIGTERM quits at once",
            (false, true) => "press ctrl-c again to quit at once",
            (false, false) => "ctrl-c again to quit at once",
        }
    }

    /// says that the run finishes after all, as when a stop came after its last look for one but
    /// before it marked its work as finishing, and what a second ctrl-c or SIGTERM does.
    pub(crate) fn finishing(&self) {
        self.alone().finishing();
    }

    /// `line`, clearing its own line first at a terminal, which echoes `^C` where its cursor
    /// waits, at the start of a fresh line unless bars are drawn there.
    fn over_echo(&self, line: &str) -> String {
        if self.style.interactive {
            format!("\u{1b}[2K{line}")
        } else {
            line.to_owned()
        }
    }

    /// `line` as it goes to a terminal: from the start of its own line, so a `^C` the terminal
    /// echoed there before it is written over rather than shifting the verb column.
    fn at_start(&self, line: &str) -> String {
        if self.style.interactive {
            format!("\r{line}")
        } else {
            line.to_owned()
        }
    }

    /// ends the story at a second ctrl-c: clears the bars, tells in one line that the run quit
    /// and shows the cursor again, so it leaves the terminal as it found it.
    pub(crate) fn quit(&self) {
        self.abandon();
        if self.tells(Level::Normal) {
            let line = verb_line("Quit", "at once", Tone::Warn, self.style.color);
            // the bars are hidden, and a question may hold its lines back, so this goes out alone
            write_line(
                self.style.color,
                &shown(&self.at_start(&self.over_echo(&line)), self.style.unicode),
            );
        }
    }

    /// puts back the mode a question holds the terminal in, clears the bars and shows the cursor
    /// again, for a run that ends at once, as at a second ctrl-c, so it leaves the terminal as it
    /// found it.
    pub(crate) fn abandon(&self) {
        raw::release();
        if let Some(bars) = &self.bars {
            let _ = bars.clear();
            bars.set_draw_target(ProgressDrawTarget::hidden());
        }
        if self.style.interactive {
            let _ = console::Term::stderr().show_cursor();
        }
    }

    fn line(&self, text: &str) {
        let text = self.prepared(text);
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(waiting) = held.as_mut() {
            waiting.push(text);
            return;
        }
        // still holding `held`, so lines go out in the order they were told
        self.write_lines(&[text]);
    }

    /// `text` as a line goes out: its symbols as the terminal shows them, wrapped to its width,
    /// from the start of the line.
    fn prepared(&self, text: &str) -> String {
        let text = shown(text, self.style.unicode);
        match self.columns {
            Some(columns) => self.at_start(&fit::fit(&text, columns)),
            None => self.at_start(&text),
        }
    }

    /// writes `lines` on stderr below the bars. callers hold `held`, so no other line comes
    /// between.
    fn write_lines(&self, lines: &[String]) {
        if lines.is_empty() {
            return;
        }
        #[cfg(test)]
        self.said
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(lines);
        let write = || {
            for line in lines {
                write_line(self.style.color, line);
            }
        };
        match &self.bars {
            Some(bars) => bars.suspend(write),
            None => write(),
        }
    }
}

impl Output {
    /// one row of a report on stdout, as `mark` draws it on stderr, such as `Missing` and its
    /// subject, so a pipe or a file gets the rows. quiet and `--json` print none.
    pub(crate) fn report(&self, verb: &str, text: &str) {
        let Some(line) = self.report_text(verb, text) else {
            return;
        };
        let write = || self.answer(&line);
        match &self.bars {
            Some(bars) => bars.suspend(write),
            None => write(),
        }
    }

    fn report_text(&self, verb: &str, text: &str) -> Option<String> {
        self.tells(Level::Normal).then(|| {
            shown(
                &verb_line(verb, text, difference(verb), self.report_color),
                self.style.unicode,
            )
            .into_owned()
        })
    }
}

/// the counters of a run in words, as `-v` ends with them.
fn work_line(counts: Counts, elapsed: Duration) -> String {
    let parts = [
        (counts.requests, "request", "requests"),
        (counts.retries, "retry", "retries"),
        (counts.resumes, "resume", "resumes"),
        (
            counts.objects_written,
            "file written to the cache",
            "files written to the cache",
        ),
        (
            counts.downloads_skipped,
            "download skipped",
            "downloads skipped",
        ),
        (
            counts.mirror_objects,
            "file from a mirror",
            "files from mirrors",
        ),
        (
            counts.data_folder_objects,
            "file from the data folder",
            "files from the data folder",
        ),
        (counts.reflinks, "reflink", "reflinks"),
        (counts.hardlinks, "hardlink", "hardlinks"),
        (counts.copies, "copy", "copies"),
    ];
    let mut text: Vec<String> = parts
        .iter()
        .filter(|(n, _, _)| *n > 0)
        .map(|(n, one, many)| units::count(*n, one, many))
        .collect();
    if counts.placed_from_cache > 0 {
        text.push(format!(
            "{} placed from the cache",
            counts.placed_from_cache
        ));
    }
    if counts.bytes_down > 0 {
        text.insert(
            1.min(text.len()),
            format!("{} down", units::size(counts.bytes_down)),
        );
    }
    text.push(format!("{} in all", units::duration(elapsed)));
    text.join(" · ")
}

/// `text` as the terminal can draw it: the few symbols fetchloom uses become ascii where the
/// terminal or locale cannot show unicode.
pub(super) fn shown(text: &str, unicode: bool) -> Cow<'_, str> {
    if unicode || text.is_ascii() {
        return Cow::Borrowed(text);
    }
    Cow::Owned(
        text.replace('\u{b7}', "-")
            .replace('\u{2026}', "...")
            .replace('\u{203a}', ">")
            .replace('\u{2501}', "-"),
    )
}

/// puts the terminal back as a question found it, for a crash, which ends the run before the
/// question lets go of it.
pub(crate) fn release_terminal() {
    raw::release();
}

/// the width of the terminal stderr goes to, which a line told there wraps at between words, or
/// none for a pipe, a file or a coding agent, which reads each line whole.
pub(crate) fn story_columns() -> Option<usize> {
    let terminal =
        std::io::stderr().is_terminal() && !style::by_agent(&|name| std::env::var_os(name));
    terminal
        .then(|| console::Term::stderr().size_checked())
        .flatten()
        .map(|(_, columns)| usize::from(columns))
}

/// prints help with the chosen palette and terminal width. a reader closing its pipe is no failure.
pub(crate) fn help(text: &str, choice: ColorChoice) -> Result<(), Error> {
    let terminal = std::io::stdout().is_terminal();
    let width = if terminal {
        console::Term::stdout()
            .size_checked()
            .map_or(80, |(_, columns)| usize::from(columns))
    } else {
        80
    };
    let wrapped = wrap_help(text.trim_end_matches('\n'), width.clamp(40, 100), terminal);
    let shown = help_text(&wrapped, Style::stdout_color(choice));
    stdout(|out| writeln!(out, "{shown}"))
}

fn stdout(write: impl FnOnce(&mut dyn Write) -> std::io::Result<()>) -> Result<(), Error> {
    write_answer(&mut std::io::stdout().lock(), write)
}

fn write_answer(
    out: &mut dyn Write,
    write: impl FnOnce(&mut dyn Write) -> std::io::Result<()>,
) -> Result<(), Error> {
    match write(out).and_then(|()| out.flush()) {
        Err(error) if error.kind() != std::io::ErrorKind::BrokenPipe => {
            Err(Error::io("could not print the answer", &error))
        }
        _ => Ok(()),
    }
}

/// help as wide as `width`: a table row wraps under its description column, or below its name
/// when the name takes half the width, and prose flush. a command line stays whole to copy, but
/// at a `terminal`, which would wrap it inside a word, it goes on after a shell's continuation
/// mark, its comment below it.
pub(crate) fn wrap_help(text: &str, width: usize, terminal: bool) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut out = Vec::new();
    let mut examples = false;
    let mut below = false;
    for (at, line) in lines.iter().copied().enumerate() {
        if !line.starts_with(' ') {
            examples = line == "examples:";
            let block = lines[at + 1..]
                .iter()
                .copied()
                .take_while(|line| line.starts_with("  "));
            below = examples && fit::comments_below(block, width);
        }
        let command = line.starts_with("  fetchloom ") && !line.trim_start().contains("  ");
        if (examples && line.starts_with("  ")) || command {
            if terminal {
                out.extend(fit::example(line, width, below));
            } else {
                out.push(line.to_owned());
            }
            continue;
        }
        if units::columns(line) <= width {
            out.push(line.to_owned());
            continue;
        }
        // a row: the description starts after the last run of two or more spaces in its first
        // part, as in `  -o, --output <dir>       put the download in <dir>`
        // byte offsets for slicing, display widths for columns, since a name may not be ascii
        let indent = line.len() - line.trim_start().len();
        let gap = line[indent..].find("  ").map(|gap| {
            let after = &line[indent + gap..];
            indent + gap + (after.len() - after.trim_start().len())
        });
        let (head, body, column) = match gap {
            Some(split) if units::columns(&line[..split]) < width / 2 => {
                let (head, body) = line.split_at(split);
                (head.to_owned(), body, units::columns(head))
            }
            // a name too wide to stand beside its description takes a line of its own, and the
            // descriptions below their names share one column, deeper than a long option's name
            Some(split) => {
                out.push(line[..split].trim_end().to_owned());
                let column = (indent + 4).max(10);
                (" ".repeat(column), &line[split..], column)
            }
            None => (line[..indent].to_owned(), &line[indent..], indent),
        };
        let mut current = head;
        let fresh = column;
        for word in body.split(' ').filter(|word| !word.is_empty()) {
            let room = units::columns(&current) + 1 + units::columns(word);
            if units::columns(&current) > fresh && room > width {
                out.push(current);
                current = " ".repeat(column);
            }
            if units::columns(&current) > fresh {
                current.push(' ');
            }
            current.push_str(word);
        }
        out.push(current);
    }
    out.join("\n")
}

/// one repeat of the 2/2 twill fetchloom draws as its mark, a row a string, `#` where the warp
/// crosses over the weft. `python/build_site.py` draws the site's marks from the same rows.
const TWILL: [&str; 4] = ["##..", "#..#", "..##", ".##."];

/// the welcome's mark: the twill three repeats across, beside the name. a terminal cell holds two
/// rows of the twill in half blocks, or one in `#` where the terminal shows only ascii.
fn welcome_mark(unicode: bool) -> [String; 3] {
    let woven = |row: usize, column: usize| {
        TWILL[row % TWILL.len()].as_bytes()[column % TWILL[0].len()] == b'#'
    };
    let mut lines: [String; 3] = Default::default();
    for (index, line) in lines.iter_mut().enumerate() {
        for column in 0..3 * TWILL[0].len() {
            let cell = if unicode {
                match (woven(2 * index, column), woven(2 * index + 1, column)) {
                    (true, true) => '\u{2588}',
                    (true, false) => '\u{2580}',
                    (false, true) => '\u{2584}',
                    (false, false) => ' ',
                }
            } else if woven(index, column) {
                '#'
            } else {
                ' '
            };
            line.push(cell);
        }
        line.truncate(line.trim_end().len());
    }
    lines[1].push_str("   fetchloom");
    lines
}

/// the mark and a blank line, the twill in the accent and the name in bold, when `width` columns
/// can hold it.
pub(crate) fn wordmark(width: usize, color: bool, unicode: bool) -> String {
    let lines = welcome_mark(unicode);
    if lines.iter().any(|line| units::columns(line) >= width) {
        return String::new();
    }
    let tint = paint(color, accent());
    let bold = paint(color, Paint::new().bold());
    let mut drawn = String::new();
    for line in &lines {
        let (twill, name) = line.split_at(line.find("fetchloom").unwrap_or(line.len()));
        let _ = write!(drawn, "{tint}{twill}{tint:#}");
        if !name.is_empty() {
            let _ = write!(drawn, "{bold}{name}{bold:#}");
        }
        drawn.push('\n');
    }
    drawn.push('\n');
    drawn
}

pub(crate) fn help_text(text: &str, color: bool) -> String {
    if !color {
        return text.to_owned();
    }
    let bold = Paint::new().bold();
    let accent = accent();
    let mut commands = false;
    // whether the line before ended in the mark a shell continues a command at
    let mut continued = false;
    text.split('\n')
        .map(|line| {
            if !line.starts_with(' ') {
                commands = line == "commands:";
            }
            if std::mem::replace(&mut continued, line.ends_with(fit::MARK)) {
                let indent = &line[..line.len() - line.trim_start().len()];
                return format!("{indent}{accent}{}{accent:#}", line.trim_start());
            }
            for heading in [
                "usage:",
                "commands:",
                "arguments:",
                "options:",
                "examples:",
                "on disk:",
            ] {
                if let Some(rest) = line.strip_prefix(heading) {
                    let rest = if heading == "usage:" {
                        format!(" {accent}{}{accent:#}", rest.trim_start())
                    } else {
                        accented(rest, Paint::new(), true)
                    };
                    return format!("{bold}{heading}{bold:#}{rest}");
                }
            }
            if let Some(command) = line.strip_prefix("  ") {
                if command.starts_with("fetchloom ") {
                    let (command, description) = command.split_once("  ").unwrap_or((command, ""));
                    let gap = if description.is_empty() { "" } else { "  " };
                    return format!("  {accent}{command}{accent:#}{gap}{description}");
                }
                // an option or an argument: its spelling is what a person types. a description
                // below its name sits deeper, though it may start with < or - too
                let indent = &command[..command.len() - command.trim_start().len()];
                if indent.len() <= 4 && command.trim_start().starts_with(['-', '<']) {
                    let entry = command.trim_start();
                    let (flag, description) = entry.split_once("  ").unwrap_or((entry, ""));
                    let gap = &entry[flag.len()..entry.len() - description.trim_start().len()];
                    return format!(
                        "  {indent}{accent}{flag}{accent:#}{gap}{}",
                        description.trim_start()
                    );
                }
                if commands {
                    let end = command.find(' ').unwrap_or(command.len());
                    return format!("  {accent}{}{accent:#}{}", &command[..end], &command[end..]);
                }
                if let Some((name, analogy)) = command.split_once(", ")
                    && matches!(name, "data.toml" | "data.lock" | "the cache" | "sync")
                {
                    let style = if name == "sync" { accent } else { bold };
                    return format!("  {style}{name}{style:#}, {analogy}");
                }
            }
            for heading in ["fetchloom", "try it", "learn more"] {
                if let Some(rest) = line.strip_prefix(heading) {
                    return format!(
                        "{bold}{heading}{bold:#}{}",
                        accented(rest, Paint::new(), true)
                    );
                }
            }
            accented(line, Paint::new(), true)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn paint(color: bool, style: Paint) -> Paint {
    if color { style } else { Paint::new() }
}

/// the palette: green for what finished, cyan for work under way and anything a person types,
/// yellow for what needs attention, red for what failed.
fn verb_color(verb: &str) -> Tone {
    match verb {
        "Downloading" | "Unpacking" | "Waiting" | "Resolving" | "Checking" | "Searching" => {
            Tone::Work
        }
        "Missing" | "Extra" | "Kept" | "Edited" | "Unlocked" | "Untracked" | "Outdated" => {
            Tone::Warn
        }
        "Failed" | "Damaged" | "Blocked" => Tone::Fail,
        // a dry run's plan is work not yet done
        verb if verb.starts_with("Would ") => Tone::Work,
        // the numbers of a list to choose from
        verb if verb.bytes().all(|byte| byte.is_ascii_digit()) => Tone::Accent,
        _ => Tone::Done,
    }
}

/// the color a difference shows in, the same wherever a command reports it: red where the bytes
/// in place are wrong or an entry stands in a file's way, yellow for every other difference.
fn difference(verb: &str) -> Tone {
    match verb {
        "Damaged" | "Blocked" => Tone::Fail,
        _ => Tone::Warn,
    }
}

fn verb_line(verb: &str, text: &str, tone: Tone, color: bool) -> String {
    let style = toned(color, tone);
    // a dry run's `Would` takes the verb column, its action the start of the line
    if let Some(action) = verb.strip_prefix("Would ") {
        return format!(
            "{style}{:>11} {action}{style:#} {}",
            "Would",
            subject(&plain(text), color)
        );
    }
    format!(
        "{style}{verb:>11}{style:#} {}",
        subject(&plain(text), color)
    )
}

/// what [`Output::error`] writes for `error`.
fn failure_text(error: &Error, level: Level, color: bool) -> String {
    if let Some(what) = &error.cancelled {
        let mut text = verb_line("Cancelled", what, Tone::Warn, color);
        if let Some(help) = &error.help {
            text.push('\n');
            text.push_str(&hint_line(help, color));
        }
        return text;
    }
    if error.kind == crate::error::Kind::Interrupted {
        return stopped_text(error, color);
    }
    error_text(error, level, color)
}

/// a stop the person asked for: a `Stopped` line made from the error's sentence, which every stop
/// starts with "stopped", its supporting lines dim under it, and its help as a hint.
fn stopped_text(error: &Error, color: bool) -> String {
    let style = toned(color, Tone::Warn);
    let dim = paint(color, Paint::new().dimmed());
    let message = plain(&error.message);
    let rest = match message.strip_prefix("stopped") {
        Some(rest) => Cow::Borrowed(rest),
        // a sentence that does not say so still reads, after a colon
        None => Cow::Owned(format!(": {message}")),
    };
    let mut text = format!(
        "{style}{:>11}{style:#}{}",
        "Stopped",
        accented(&rest, Paint::new(), color)
    );
    for line in error.details.iter().flat_map(|line| readable(line, 12)) {
        let _ = write!(
            text,
            "\n{:>11} {dim}{}{dim:#}",
            "",
            accented(&plain(&line), dim, color)
        );
    }
    if let Some(help) = &error.help {
        text.push('\n');
        text.push_str(&hint_line(help, color));
    }
    text
}

fn error_text(error: &Error, level: Level, color: bool) -> String {
    let red = toned(color, Tone::Fail);
    let dim = paint(color, Paint::new().dimmed());
    let mut text = format!(
        "{red}error:{red:#} {}",
        accented(&plain(&error.message), Paint::new(), color)
    );
    for line in error.details.iter().flat_map(|line| readable(line, 2)) {
        let _ = write!(
            text,
            "\n  {dim}{}{dim:#}",
            accented(&plain(&line), dim, color)
        );
    }
    if let Some(help) = &error.help {
        text.push('\n');
        text.push_str(&help_line(help, color));
    }
    if level >= Level::Verbose {
        let _ = write!(text, "\n  {dim}kind: {}{dim:#}", error.kind.name());
    }
    text
}

/// the lines a person reads for the detail `line`, `indent` columns in: two whole SHA-256 or MD5
/// values compared, as `expected sha256 <a>, got <b>`, one under the other with every digit, to
/// check against a published checksum, each label on its own line where 80 columns would wrap a
/// hash. any other line as [`hashes_apart`] shows it.
fn readable(line: &str, indent: usize) -> Vec<Cow<'_, str>> {
    const COLUMNS: usize = 80;
    let pair = [64, 32]
        .into_iter()
        .find_map(|digits| match whole_hashes(line, digits)[..] {
            [(first, a), (second, b)]
                if line[first + a.len()..second].starts_with(", ")
                    && second + b.len() == line.len() =>
            {
                Some((first, a, second, b))
            }
            _ => None,
        });
    if let Some((first, a, second, b)) = pair {
        let labels = [&line[..first], &line[first + a.len() + 2..second]].map(|label| {
            let label = label.trim_end();
            label.strip_suffix(" sha256").unwrap_or(label)
        });
        let width = 1 + labels
            .iter()
            .map(|label| label.chars().count())
            .max()
            .unwrap_or(0);
        let compared = labels.into_iter().zip([a, b]);
        if indent + width + a.len() <= COLUMNS {
            return compared
                .map(|(label, hash)| Cow::Owned(format!("{label:<width$}{hash}")))
                .collect();
        }
        return compared
            .flat_map(|(label, hash)| [format!("{label}:"), format!("  {hash}")])
            .map(Cow::Owned)
            .collect();
    }
    vec![hashes_apart(line)]
}

/// each whole hash of `digits` hex digits on `line`, a word of its own, with where it starts.
fn whole_hashes(line: &str, digits: usize) -> Vec<(usize, &str)> {
    let bytes = line.as_bytes();
    let digit = |byte: &u8| matches!(byte, b'0'..=b'9' | b'a'..=b'f');
    let mut hashes: Vec<(usize, &str)> = Vec::new();
    let mut start = 0;
    while start < bytes.len() {
        let run = bytes[start..].iter().take_while(|byte| digit(byte)).count();
        let end = start + run;
        // a word of its own, never part of a path or a longer name
        let opens = start == 0 || matches!(bytes[start - 1], b' ' | b'(');
        let closes = bytes
            .get(end)
            .is_none_or(|byte| matches!(byte, b' ' | b',' | b';' | b'.' | b')'));
        if run == digits && opens && closes {
            hashes.push((start, &line[start..end]));
        }
        start = end + 1;
    }
    hashes
}

/// `line` with each whole SHA-256 cut to its first 12 digits, or one past where it differs from
/// another on the line, so hashes that differ late still read apart. `--json` keeps them whole.
fn hashes_apart(line: &str) -> Cow<'_, str> {
    const WHOLE: usize = 64;
    const SHORT: usize = 12;
    let hashes = whole_hashes(line, WHOLE);
    if hashes.is_empty() {
        return Cow::Borrowed(line);
    }
    let mut shown = String::with_capacity(line.len());
    let mut at = 0;
    for (start, hash) in &hashes {
        let length = hashes
            .iter()
            .filter(|(other, _)| other != start)
            .map(|(_, other)| {
                hash.bytes()
                    .zip(other.bytes())
                    .take_while(|(a, b)| a == b)
                    .count()
                    + 1
            })
            .fold(SHORT, usize::max)
            .min(WHOLE);
        shown.push_str(&line[at..*start]);
        shown.push_str(&hash[..length]);
        at = start + WHOLE;
    }
    shown.push_str(&line[at..]);
    Cow::Owned(shown)
}

fn hint_line(text: &str, color: bool) -> String {
    let dim = paint(color, Paint::new().dimmed());
    format!("  {dim}hint: {}{dim:#}", accented(&plain(text), dim, color))
}

/// what to do about a warning or an error, on a line of its own.
fn help_line(text: &str, color: bool) -> String {
    let cyan = paint(color, accent().bold());
    format!(
        "  {cyan}help:{cyan:#} {}",
        accented(&plain(text), Paint::new(), color)
    )
}

/// the one color for what a person types, commands and options, wherever they appear.
fn accent() -> Paint {
    tinted(Tone::Accent)
}

fn tinted(tone: Tone) -> Paint {
    Paint::new().fg_color(Some(style::color(tone)))
}

/// bold in the color of `tone`, or plain when color is off.
fn toned(color: bool, tone: Tone) -> Paint {
    paint(color, tinted(tone).bold())
}

/// `text` with each command in backticks in the accent, then back to `around`. the backticks
/// stay, so a command still stands out without color.
fn accented(text: &str, around: Paint, color: bool) -> String {
    let accent = paint(color, accent().bold());
    let mut shown = String::with_capacity(text.len() + 16);
    let mut rest = text;
    while let Some(start) = rest.find('`') {
        let Some(length) = rest[start + 1..].find('`') else {
            break;
        };
        let end = start + length + 2;
        let _ = write!(
            shown,
            "{}{around:#}{accent}{}{accent:#}{around}",
            &rest[..start],
            &rest[start..end]
        );
        rest = &rest[end..];
    }
    shown.push_str(rest);
    shown
}

/// a step's facts with what it is about, such as a file or a dataset, in bold: the text before
/// the first `·`, or the whole text when it is one word. the facts after it are dim.
fn subject(text: &str, color: bool) -> String {
    let bold = paint(color, Paint::new().bold());
    let dim = paint(color, Paint::new().dimmed());
    match text.split_once(" · ") {
        Some((name, facts)) => format!(
            "{bold}{name}{bold:#}{dim} · {}{dim:#}",
            accented(facts, dim, color)
        ),
        None if !text.is_empty() && !text.contains(' ') => format!("{bold}{text}{bold:#}"),
        None => accented(text, Paint::new(), color),
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

/// what a key did to a line typed at a prompt, for showing it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Edit {
    Typed(char),
    Erased(char),
}

/// a line typed at a prompt, read one key at a time from `key` until enter, each change told to
/// `edited`. ctrl-c arrives as a key and cancels it everywhere; read as whole lines, it would be a
/// signal that leaves the read waiting, and echo off on unix after a second ctrl-c.
fn typed_line(
    mut key: impl FnMut() -> std::io::Result<console::Key>,
    mut edited: impl FnMut(Edit),
) -> std::io::Result<String> {
    let mut line = String::new();
    loop {
        match key()? {
            console::Key::Enter => return Ok(line),
            console::Key::CtrlC => return Err(std::io::ErrorKind::Interrupted.into()),
            console::Key::Backspace => {
                if let Some(erased) = line.pop() {
                    edited(Edit::Erased(erased));
                }
            }
            console::Key::Char(typed) if !typed.is_control() => {
                line.push(typed);
                edited(Edit::Typed(typed));
            }
            _ => {}
        }
    }
}

/// one answer to a yes or no question, typed with `key`: y or yes, n or no, or enter for `default`.
/// any other line, as q typed to quit, is none and asked again; a closed terminal answers no.
fn answer(
    key: impl FnMut() -> std::io::Result<console::Key>,
    edited: impl FnMut(Edit),
    default: bool,
) -> Result<Option<bool>, Error> {
    let line = match typed_line(key, edited) {
        Ok(line) => line,
        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {
            return Err(
                Error::new(crate::error::Kind::Interrupted, "stopped").cancelled("the question")
            );
        }
        Err(_) => return Ok(Some(false)),
    };
    match line.trim().to_lowercase().as_str() {
        "" => Ok(Some(default)),
        "y" | "yes" => Ok(Some(true)),
        "n" | "no" => Ok(Some(false)),
        _ => Ok(None),
    }
}

/// shows on `terminal` a change to an answer as it is typed. a terminal that reads no escape
/// codes, as under `TERM=dumb`, has an erased character written over with backspaces.
fn echo(terminal: &console::Term, edit: Edit, escapes: bool) {
    let mut bytes = [0; 4];
    let _ = match edit {
        Edit::Typed(typed) => terminal.write_str(typed.encode_utf8(&mut bytes)),
        Edit::Erased(erased) => {
            let width = console::measure_text_width(erased.encode_utf8(&mut bytes));
            if escapes {
                terminal.clear_chars(width)
            } else {
                let back = "\u{8}".repeat(width);
                terminal.write_str(&format!("{back}{}{back}", " ".repeat(width)))
            }
        }
    };
}

/// a pasted secret shown without giving it away: a long key keeps 4 characters at each end so a
/// person can tell which one arrived, anything shorter shows only as stars.
fn masked(secret: &str) -> String {
    let chars: Vec<char> = secret.chars().collect();
    let count = chars.len();
    let shown = if count >= 24 {
        let start: String = chars[..4].iter().collect();
        let end: String = chars[count - 4..].iter().collect();
        format!("{start}...{end}")
    } else {
        "*".repeat(count.min(12))
    };
    format!(
        "{shown} \u{b7} {}",
        units::count(count as u64, "character", "characters")
    )
}

/// `text` with control characters, bidi controls among them, written as escapes, so text from a
/// server cannot move the cursor, recolor the terminal or reorder its words.
fn plain(text: &str) -> Cow<'_, str> {
    if !text.contains(crate::name::is_control) {
        return Cow::Borrowed(text);
    }
    let mut shown = String::with_capacity(text.len() + 8);
    for c in text.chars() {
        if crate::name::is_control(c) {
            shown.extend(c.escape_default());
        } else {
            shown.push(c);
        }
    }
    Cow::Owned(shown)
}

/// a JSON document in pieces of at most 1 MiB: one growing buffer copies itself at each doubling,
/// and the musl build's allocator keeps freed memory for a second, so the copies add to the peak.
#[derive(Default)]
struct Pieces {
    full: Vec<Vec<u8>>,
    current: Vec<u8>,
}

impl Pieces {
    #[cold]
    fn spill(&mut self, mut bytes: &[u8]) {
        while !bytes.is_empty() {
            let room = self.current.capacity() - self.current.len();
            if room == 0 {
                let size = if self.current.capacity() == 0 {
                    8 << 10
                } else {
                    1 << 20
                };
                let done = std::mem::replace(&mut self.current, Vec::with_capacity(size));
                if !done.is_empty() {
                    self.full.push(done);
                }
                continue;
            }
            let take = room.min(bytes.len());
            self.current.extend_from_slice(&bytes[..take]);
            bytes = &bytes[take..];
        }
    }

    fn slices(&self) -> impl Iterator<Item = &[u8]> {
        self.full
            .iter()
            .chain(std::iter::once(&self.current))
            .map(Vec::as_slice)
    }
}

impl From<Vec<u8>> for Pieces {
    fn from(whole: Vec<u8>) -> Pieces {
        Pieces {
            full: Vec::new(),
            current: whole,
        }
    }
}

impl Write for Pieces {
    #[inline]
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.write_all(bytes)?;
        Ok(bytes.len())
    }

    #[inline]
    fn write_all(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        if bytes.len() <= self.current.capacity() - self.current.len() {
            self.current.extend_from_slice(bytes);
        } else {
            self.spill(bytes);
        }
        Ok(())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// `json` as it goes to a terminal, with c1 and bidi controls, which a terminal may act on or show
/// out of order, written as `\u` escapes, which json holds only in strings and reads back alike.
fn escaped_for_terminal(json: Vec<u8>) -> Vec<u8> {
    let steers = |c: char| matches!(c, '\u{80}'..='\u{9f}') || crate::name::is_bidi_control(c);
    let text = match String::from_utf8(json) {
        Ok(text) if text.contains(steers) => text,
        Ok(text) => return text.into_bytes(),
        Err(err) => return err.into_bytes(),
    };
    let mut escaped = String::with_capacity(text.len() + 16);
    for c in text.chars() {
        if steers(c) {
            let _ = write!(escaped, "\\u{:04x}", u32::from(c));
        } else {
            escaped.push(c);
        }
    }
    escaped.into_bytes()
}

#[cfg(test)]
mod tests;
