//! what fetchloom prints. stdout carries the answer and nothing else, stderr carries the story.

mod picker;
mod progress;
mod style;

use std::borrow::Cow;
use std::fmt::Write as _;
use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use anstyle::Style as Paint;
use indicatif::{MultiProgress, ProgressDrawTarget};
use serde::Serialize;

pub(crate) use progress::{Bar, Setup, Ticker, Wait};
use style::Tone;
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
#[expect(
    clippy::struct_excessive_bools,
    reason = "each is a separate yes or no the run was given"
)]
pub(crate) struct Output {
    style: Style,
    level: Level,
    json: bool,
    bars: Option<MultiProgress>,
    /// lines told while a person answers a question, written once the answer is in.
    held: Mutex<Option<Vec<String>>>,
    /// taken while a question waits, so two never share the terminal.
    question: Mutex<()>,
    warnings: Mutex<Vec<String>>,
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
    /// whether report rows on stdout take color, which stdout decides apart from stderr.
    report_color: bool,
    /// every bar made, so a first ctrl-c redraws each at once to say it is stopping.
    drawn: Mutex<Vec<indicatif::WeakProgressBar>>,
    /// how long work that is usually quick goes untold at a terminal.
    untold: Duration,
    /// the last closing line told, such as `Added iris`, and whether a ctrl-c that let the run
    /// finish came after it. a terminal drops what a program wrote but it had not yet shown when
    /// a ctrl-c comes, so that line is told once more at the end.
    closing: Mutex<(Option<String>, bool)>,
    /// every line told, for tests.
    #[cfg(test)]
    said: Mutex<Vec<String>>,
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
    warnings: Vec<String>,
    errors: Vec<Problem<'a>>,
    work: WorkDone,
}

#[derive(Serialize)]
#[serde(untagged)]
enum DocumentResult<'a, T> {
    Complete(&'a T),
    Partial(&'a serde_json::Value),
}

#[derive(Serialize)]
struct Problem<'a> {
    kind: &'static str,
    message: &'a str,
    details: &'a [String],
    help: Option<&'a str>,
    /// the command `help` names, ready to run, for a script or an agent to act on
    fix: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    failures: Option<&'a [crate::error::FileFailure]>,
}

/// the first `fetchloom` command quoted in `help`, without its backticks. an example ("as in"),
/// a guess ("did you mean") or a command meant for a person's own terminal is no fix: an agent
/// running it would act on a dataset nobody named or ask the same question again. a command
/// that first needs a broken file deleted, a file edited by hand or the errors listed above fixed
/// is not ready to run either, and a build runs the project's code, which only a person may agree
/// to. neither is taking other bytes than data.lock pins, which a help offers only "if the new
/// bytes are intended". in a run that is `locked`, a command that may write data.lock is no fix
/// either, since a script running it would undo `--locked`.
fn fix(help: &str, locked: bool) -> Option<&str> {
    const NOT_A_FIX: [&str; 10] = [
        "as in",
        "such as",
        "for example",
        "e.g.",
        "did you mean",
        "own terminal, run",
        "delete it and run",
        "by hand, then run",
        "errors above, then run",
        "intended, run",
    ];
    let mut before = String::new();
    for (at, part) in help.split('`').enumerate() {
        if at % 2 == 0 {
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
            && crate::cli::runs(part)
            && !crate::cli::runs_code(part)
            && (!locked || !crate::cli::changes_lock(part))
        {
            return Some(part);
        }
        before.push_str(part);
    }
    None
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
        let bars = (style.interactive && !json && !logs && level >= Level::Normal)
            .then(|| MultiProgress::with_draw_target(progress::terminal()));
        let output = Output {
            style,
            level,
            json,
            bars,
            held: Mutex::new(None),
            question: Mutex::new(()),
            warnings: Mutex::new(Vec::new()),
            failed: Mutex::new(None),
            hints: true,
            progress: false,
            locked: AtomicBool::new(false),
            stopping: Arc::default(),
            report_color: Style::stdout_colored(),
            drawn: Mutex::new(Vec::new()),
            untold: progress::UNTOLD,
            closing: Mutex::new((None, false)),
            #[cfg(test)]
            said: Mutex::new(Vec::new()),
        };
        if !unread.is_empty() {
            output.warn_with(
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

    /// a difference found, in the same rhythm as steps but in the warning color.
    pub(crate) fn mark(&self, verb: &str, text: &str) {
        if self.tells(Level::Normal) {
            self.line(&verb_line(verb, text, Tone::Warn, self.style.color));
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

    /// the warnings told so far, for tests.
    #[cfg(test)]
    pub(crate) fn told(&self) -> Vec<String> {
        self.warnings
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub(crate) fn warn(&self, text: &str) {
        self.warnings
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(text.to_owned());
        // a warning is about the data, as a kept edit is, so -q keeps it as it keeps errors
        if self.tells(Level::Quiet) {
            let yellow = toned(self.style.color, Tone::Warn);
            self.line(&format!(
                "{yellow}warning:{yellow:#} {}",
                accented(&plain(text), Paint::new(), self.style.color)
            ));
        }
    }

    /// a warning and what to do about it, the help on a line of its own as an error's is, so
    /// neither runs long. `--json` and the python package get them as one sentence.
    pub(crate) fn warn_with(&self, text: &str, help: &str) {
        self.warnings
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(format!("{text}; {help}"));
        if self.tells(Level::Quiet) {
            let yellow = toned(self.style.color, Tone::Warn);
            self.line(&format!(
                "{yellow}warning:{yellow:#} {}\n{}",
                accented(&plain(text), Paint::new(), self.style.color),
                help_line(help, self.style.color)
            ));
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

    /// the warnings told so far, as `--json` lists them.
    #[cfg(test)]
    pub(crate) fn warned(&self) -> Vec<String> {
        self.warnings
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
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
        units::clip(text, most, self.style.more())
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

    /// one numbered choice: what to type, padded to `pad`, then its title and dim facts, both
    /// already padded into columns, the facts on a line of their own under what to type when
    /// `below` says so. what is cut on a narrow terminal is the end, never what to type.
    pub(crate) fn candidate(
        &self,
        number: usize,
        typed: &str,
        pad: usize,
        title: &str,
        (facts, below): (&str, bool),
    ) {
        if !self.tells(Level::Normal) {
            return;
        }
        if below {
            self.candidate(number, typed, pad, title, ("", false));
            let dim = paint(self.style.color, Paint::new().dimmed());
            self.line(&format!("{:>11} {dim}{}{dim:#}", "", plain(facts)));
            return;
        }
        let color = self.style.color;
        let cyan = paint(color, accent().bold());
        let accent = paint(color, accent());
        let dim = paint(color, Paint::new().dimmed());
        let typed = plain(typed);
        let title = plain(title);
        let head = format!("{cyan}{number:>11}{cyan:#} {accent}{typed:pad$}{accent:#}");
        let head_width = 12 + typed.chars().count().max(pad);
        let width = self.width().unwrap_or(usize::MAX);
        let mut line = if head_width > width {
            format!("{cyan}{number:>3}{cyan:#} {accent}{typed}{accent:#}")
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
    /// chose is no failure, so it is one plain line of what was cancelled, then the help as a
    /// hint, and neither is a stop they asked for, which is one plain line of where it stopped.
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

    /// a step a command takes before its own work, as `verb` and `what`, such as `Locking the
    /// project`: a debug line as it begins, and at a terminal a live line once it lasts a second,
    /// for a slow disk or a network filesystem that makes such a step take seconds.
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

    /// work that is usually quick but grows with its size, such as placing many files: at a
    /// terminal a live line once it lasts a second, and where no bar shows, the plain lines a
    /// long batch tells, which `--progress` keeps beside `--json`.
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

    /// plain lines about a batch of work where no bar shows it, such as `Downloading 3 files`,
    /// also beside `--json` when `--progress` asks. `sizes` holds each file's size, or none for
    /// one whose download tells it.
    pub(crate) fn ticker(
        &self,
        verb: &'static str,
        what: String,
        sizes: impl IntoIterator<Item = Option<u64>>,
    ) -> Ticker {
        let told = self.level >= Level::Normal && (!self.json || self.progress);
        if self.bars.is_some() || !told {
            return Ticker::quiet();
        }
        // tests turn off these lines, since how fast a machine runs decides when they come.
        // release builds never read it
        #[cfg(debug_assertions)]
        if std::env::var_os("FETCHLOOM_TEST_TICKER").is_some_and(|value| value == "off") {
            return Ticker::quiet();
        }
        let known = sizes
            .into_iter()
            .fold((0, 0), |(total, unknown), size| match size {
                Some(size) => (u64::saturating_add(total, size), unknown),
                None => (total, unknown + 1),
            });
        let color = self.style.color;
        let unicode = self.style.unicode;
        Ticker::start(verb, what, known, move |verb, text| {
            write_line(
                color,
                &shown(&verb_line(verb, text, Tone::Work, color), unicode),
            );
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
    /// and `--json` was not asked for.
    pub(crate) fn can_ask(&self) -> bool {
        !self.json && self.style.interactive && std::io::stdin().is_terminal()
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
            loop {
                let _ = terminal.write_str(&format!("{question} {choices} "));
                let mut shown = 0_usize;
                // read ctrl-c as a key on both platforms instead of leaving a blocking read
                // waiting after the signal handler asks network tasks to stop.
                let answer = answer(
                    || terminal.read_key_raw(),
                    |edit| {
                        match edit {
                            Edit::Typed(_) => shown += 1,
                            Edit::Erased(_) => shown -= 1,
                        }
                        echo(&terminal, edit);
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

    /// asks for a line of text, such as a username, or returns none when nobody can answer.
    /// with `hidden`, nothing typed or pasted shows until enter, then a masked echo confirms it
    /// arrived. a blank answer is asked again, twice at most. ctrl-c cancels the question.
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
                            echo(&terminal, edit);
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
                    let _ = terminal.clear_last_lines(1);
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

    /// asks which option to use, or returns none when nobody can answer.
    pub(crate) fn choose(
        &self,
        question: &str,
        options: &[String],
        footer: &str,
    ) -> Result<Option<usize>, Error> {
        if !self.can_ask() {
            return Ok(None);
        }
        let ask = || picker::choose(question, options, footer, self.style);
        self.asking(ask)
    }

    /// the counters at the end of a run, for `-v`.
    pub(crate) fn summary(&self, counts: Counts, elapsed: Duration) {
        self.detail(&work_line(counts, elapsed));
    }

    /// prints the `--json` document for a run, whatever its outcome.
    ///
    /// # Errors
    /// reports an unencodable result or a failed stdout write, including earlier text answers.
    pub(crate) fn document<T: Serialize>(
        &self,
        command: Option<&str>,
        outcome: Result<&T, &Error>,
        counts: Counts,
        elapsed: Duration,
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
                    .and_then(|help| fix(help, self.locked.load(Ordering::Relaxed))),
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
        // encoding can fail on a non-unicode path; stdout cannot retract a partial document.
        let bytes = match serde_json::to_vec(&document) {
            Ok(bytes) => bytes,
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
                self.document::<()>(command, Err(&error), counts, elapsed)?;
                return Err(error);
            }
        };
        stdout(|out| {
            out.write_all(&bytes)?;
            out.write_all(b"\n")
        })
        .inspect_err(|error| {
            // stdout is unusable, so its failure can only be reported on stderr.
            self.line(&error_text(error, self.level, self.style.color));
        })
    }

    fn tells(&self, level: Level) -> bool {
        !self.json && self.level >= level
    }

    /// runs `ask`, a question to a person, with the bars cleared and hidden but free to move.
    /// holding their lock until a person answers, as suspending them does, would stop every
    /// download that reports progress meanwhile. lines told meanwhile wait for the answer.
    fn asking<R>(&self, ask: impl FnOnce() -> R) -> R {
        let Some(bars) = &self.bars else {
            return ask();
        };
        let _alone = self.question.lock().unwrap_or_else(PoisonError::into_inner);
        *self.held.lock().unwrap_or_else(PoisonError::into_inner) = Some(Vec::new());
        let _ = bars.clear();
        bars.set_draw_target(ProgressDrawTarget::hidden());
        let answer = ask();
        bars.set_draw_target(progress::terminal());
        let held = self
            .held
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
            .unwrap_or_default();
        bars.suspend(|| {
            for line in &held {
                write_line(self.style.color, line);
            }
        });
        answer
    }

    /// says that a first ctrl-c was heard, and what a second one does. every bar says it is
    /// stopping from then on.
    pub(crate) fn stopping(&self) {
        self.stopping.store(true, Ordering::Relaxed);
        self.heard("Stopping", "(press ctrl-c again to quit at once)");
        // each bar says so now, not at its next tick, which a busy machine can put off
        let drawn = self
            .drawn
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        for bar in drawn.iter().filter_map(indicatif::WeakProgressBar::upgrade) {
            bar.tick();
        }
    }

    /// says that a first ctrl-c was heard once the run had marked its work as finishing, so it
    /// finishes, and what a second one does. a closing line told before it is told again at the
    /// end.
    pub(crate) fn finishing(&self) {
        {
            let mut closing = self.closing.lock().unwrap_or_else(PoisonError::into_inner);
            closing.1 = closing.0.is_some();
        }
        self.heard(
            "Finishing",
            "what it started (ctrl-c again to quit at once)",
        );
    }

    /// says that a first ctrl-c came once every byte of `count` downloads was in and the disk
    /// only keeps them, so the run finishes rather than download them again next time.
    pub(crate) fn finishing_downloads(&self, count: usize) {
        let which = if count == 1 {
            "the download is"
        } else {
            "the downloads are"
        };
        self.heard(
            "Finishing",
            &format!("({which} complete; ctrl-c again to quit at once)"),
        );
    }

    /// a line told as a ctrl-c is heard, which clears what the terminal echoed of it.
    fn heard(&self, verb: &str, text: &str) {
        if self.tells(Level::Normal) {
            self.line(&self.over_echo(&verb_line(verb, text, Tone::Warn, self.style.color)));
        }
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

    /// clears the bars and shows the cursor again, for a run that ends at once, as at a second
    /// ctrl-c, so it leaves the terminal as it found it.
    pub(crate) fn abandon(&self) {
        if let Some(bars) = &self.bars {
            let _ = bars.clear();
            bars.set_draw_target(ProgressDrawTarget::hidden());
        }
        if self.style.interactive {
            let _ = console::Term::stderr().show_cursor();
        }
    }

    fn line(&self, text: &str) {
        let text = self.at_start(&shown(text, self.style.unicode));
        #[cfg(test)]
        self.said
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(text.clone());
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(waiting) = held.as_mut() {
            waiting.push(text);
            return;
        }
        let write = || write_line(self.style.color, &text);
        match &self.bars {
            Some(bars) => bars.suspend(write),
            None => write(),
        }
    }
}

impl Output {
    /// one row of a report on stdout, as `mark` draws it on stderr: a verb in its column, such
    /// as `Missing`, then its subject, so a pipe or a file gets the rows while the story stays on
    /// stderr. quiet and `--json` print none.
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
                &verb_line(verb, text, Tone::Warn, self.report_color),
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

/// prints help with the chosen palette and terminal width.
///
/// # Errors
/// returns stdout failures other than a reader closing its pipe.
pub(crate) fn help(text: &str, choice: ColorChoice) -> Result<(), Error> {
    let width = if std::io::stdout().is_terminal() {
        console::Term::stdout()
            .size_checked()
            .map_or(80, |(_, columns)| usize::from(columns))
    } else {
        80
    };
    let wrapped = wrap_help(text.trim_end_matches('\n'), width.clamp(40, 100));
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

/// help as wide as `width`: a row of a table wraps under its own description column, prose
/// wraps flush, and the command lines under `examples:` never wrap, so each can be copied whole.
pub(crate) fn wrap_help(text: &str, width: usize) -> String {
    let mut out = Vec::new();
    let mut examples = false;
    for line in text.split('\n') {
        if !line.starts_with(' ') {
            examples = line == "examples:";
        }
        if line.chars().count() <= width || (examples && line.starts_with("  ")) {
            out.push(line.to_owned());
            continue;
        }
        // a row: the description starts after the last run of two or more spaces in its first
        // part, as in `  -o, --output <dir>       put the download in <dir>`
        // byte offsets for slicing, character counts for columns, since a name may not be ascii
        let indent = line.len() - line.trim_start().len();
        let split = line[indent..]
            .find("  ")
            .map(|gap| {
                let after = &line[indent + gap..];
                indent + gap + (after.len() - after.trim_start().len())
            })
            .filter(|split| line[..*split].chars().count() < width / 2)
            .unwrap_or(indent);
        let (head, body) = line.split_at(split);
        let column = head.chars().count();
        let mut current = head.to_owned();
        let fresh = column;
        for word in body.split(' ').filter(|word| !word.is_empty()) {
            let room = current.chars().count() + 1 + word.chars().count();
            if current.chars().count() > fresh && room > width {
                out.push(current);
                current = " ".repeat(column);
            }
            if current.chars().count() > fresh {
                current.push(' ');
            }
            current.push_str(word);
        }
        out.push(current);
    }
    out.join("\n")
}
/// the name drawn large, 98 columns wide, for the welcome on a wide terminal.
const WORDMARK: &str = r"
 ________ _______  _________  ________  ___  ___  ___       ________  ________  _____ ______
|\  _____\\  ___ \|\___   ___\\   ____\|\  \|\  \|\  \     |\   __  \|\   __  \|\   _ \  _   \
\ \  \__/\ \   __/\|___ \  \_\ \  \___|\ \  \\\  \ \  \    \ \  \|\  \ \  \|\  \ \  \\\__\ \  \
 \ \   __\\ \  \_|/__  \ \  \ \ \  \    \ \   __  \ \  \    \ \  \\\  \ \  \\\  \ \  \\|__| \  \
  \ \  \_| \ \  \_|\ \  \ \  \ \ \  \____\ \  \ \  \ \  \____\ \  \\\  \ \  \\\  \ \  \    \ \  \
   \ \__\   \ \_______\  \ \__\ \ \_______\ \__\ \__\ \_______\ \_______\ \_______\ \__\    \ \__\
    \|__|    \|_______|   \|__|  \|_______|\|__|\|__|\|_______|\|_______|\|_______|\|__|     \|__|
";

/// the wordmark and a blank line, in the accent, when `width` columns can hold it.
pub(crate) fn wordmark(width: usize, color: bool) -> String {
    let longest = WORDMARK.lines().map(str::len).max().unwrap_or(0);
    if width <= longest {
        return String::new();
    }
    let tint = paint(color, accent());
    WORDMARK
        .trim_matches('\n')
        .lines()
        .map(|line| format!("{tint}{line}{tint:#}\n"))
        .chain(std::iter::once(String::from("\n")))
        .collect()
}

pub(crate) fn help_text(text: &str, color: bool) -> String {
    if !color {
        return text.to_owned();
    }
    let bold = Paint::new().bold();
    let accent = accent();
    let mut commands = false;
    text.split('\n')
        .map(|line| {
            if !line.starts_with(' ') {
                commands = line == "commands:";
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
                // an option or an argument: its spelling is what a person types
                if command.trim_start().starts_with(['-', '<']) {
                    let indent = &command[..command.len() - command.trim_start().len()];
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

/// the palette: green for what finished, cyan for work under way and for anything a person
/// types, yellow for what needs attention, red for what failed. a line's subject is bold and its
/// facts are dim, so color always means the same thing.
fn verb_color(verb: &str) -> Tone {
    match verb {
        "Downloading" | "Unpacking" | "Waiting" | "Resolving" | "Checking" | "Searching" => {
            Tone::Work
        }
        "Missing" | "Extra" | "Kept" | "Edited" | "Unlocked" | "Untracked" | "Outdated" => {
            Tone::Warn
        }
        "Failed" => Tone::Fail,
        // a dry run's plan is work not yet done
        verb if verb.starts_with("Would ") => Tone::Work,
        // the numbers of a list to choose from
        verb if verb.bytes().all(|byte| byte.is_ascii_digit()) => Tone::Accent,
        _ => Tone::Done,
    }
}

fn verb_line(verb: &str, text: &str, tone: Tone, color: bool) -> String {
    let style = toned(color, tone);
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
    for line in &error.details {
        let _ = write!(
            text,
            "\n{:>11} {dim}{}{dim:#}",
            "",
            accented(&plain(&hashes_apart(line)), dim, color)
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
    for line in &error.details {
        let _ = write!(
            text,
            "\n  {dim}{}{dim:#}",
            accented(&plain(&hashes_apart(line)), dim, color)
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

/// `line` as a person reads it: each whole SHA-256 on it, a word of 64 hex digits, cut to its
/// first 12 digits, or to one past the first digit where it differs from another hash on the
/// line, so two hashes that differ late still read apart. `--json` gives them whole.
fn hashes_apart(line: &str) -> Cow<'_, str> {
    const WHOLE: usize = 64;
    const SHORT: usize = 12;
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
        if run == WHOLE && opens && closes {
            hashes.push((start, &line[start..end]));
        }
        start = end + 1;
    }
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

/// the one color for what a person types, commands and options, wherever they appear. the stream
/// drops it, like every color, when color is off.
fn accent() -> Paint {
    tinted(Tone::Accent)
}

/// text in the color of `tone`.
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

/// a line typed at a prompt, read one key at a time from `key` until enter, with each change
/// told to `edited`. ctrl-c arrives as a key, so it cancels the line on every system: a terminal
/// reading whole lines turns it into a signal, which only asks the work to stop while the read
/// goes on waiting, and leaves echo off on unix if a second ctrl-c ends the run.
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

/// one answer to a yes or no question, a line typed with `key` and each change told to
/// `edited`: y or yes, n or no in any case, or enter alone for `default`. any other line, such as
/// q typed to quit, is none, so the question is asked again rather than taking the default.
/// ctrl-c cancels the question, and a terminal that closed answers no.
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

/// shows on `terminal` a change to an answer as it is typed.
fn echo(terminal: &console::Term, edit: Edit) {
    let mut bytes = [0; 4];
    let _ = match edit {
        Edit::Typed(typed) => terminal.write_str(typed.encode_utf8(&mut bytes)),
        Edit::Erased(erased) => {
            terminal.clear_chars(console::measure_text_width(erased.encode_utf8(&mut bytes)))
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
    use std::io::Write;
    use std::time::Duration;

    use anstyle::Style as Paint;

    use super::{Level, Output, Style, accented, masked, plain, subject, write_answer};
    use crate::work::Counts;

    #[test]
    fn answer_writes_and_flushes_propagate_failures_except_broken_pipes() {
        struct Sink {
            kind: std::io::ErrorKind,
            fail_write: bool,
            flushed: bool,
        }
        impl Write for Sink {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if self.fail_write {
                    Err(self.kind.into())
                } else {
                    Ok(bytes.len())
                }
            }

            fn flush(&mut self) -> std::io::Result<()> {
                self.flushed = true;
                Err(self.kind.into())
            }
        }
        for kind in [
            std::io::ErrorKind::BrokenPipe,
            std::io::ErrorKind::StorageFull,
        ] {
            for fail_write in [false, true] {
                let mut sink = Sink {
                    kind,
                    fail_write,
                    flushed: false,
                };
                let result = write_answer(&mut sink, |out| out.write_all(b"answer\n"));
                assert_eq!(sink.flushed, !fail_write);
                if kind == std::io::ErrorKind::BrokenPipe {
                    assert!(result.is_ok());
                } else {
                    assert_eq!(result.unwrap_err().kind, crate::error::Kind::LocalIo);
                }
            }
        }
        let mut bytes = Vec::new();
        write_answer(&mut bytes, |out| out.write_all(b"answer\n")).unwrap();
        assert_eq!(bytes, b"answer\n");
    }

    #[test]
    fn help_wraps_rows_under_their_column_and_keeps_examples_whole() {
        let text = "options:\n  -o, --output <dir>  put the download in <dir> instead of a new folder named after it\n\nexamples:\n  fetchloom completions bash > ~/.local/share/bash-completion/completions/fetchloom";
        let wrapped = super::wrap_help(text, 60);
        assert_eq!(
            wrapped,
            "options:\n  -o, --output <dir>  put the download in <dir> instead of a\n                      new folder named after it\n\nexamples:\n  fetchloom completions bash > ~/.local/share/bash-completion/completions/fetchloom"
        );
        // prose wraps flush, one-letter words keep their spaces, and nothing is lost
        let prose = "a b c d e f g h i j k l m n o p q r s t u v w x y z a b c d e f g h";
        let wrapped = super::wrap_help(prose, 20);
        assert!(
            wrapped.lines().all(|line| line.chars().count() <= 20),
            "{wrapped}"
        );
        assert_eq!(
            wrapped.split_whitespace().collect::<Vec<_>>().join(" "),
            prose
        );
        // a name that is not ascii still lines its description up by characters
        let row = "  --données  une description assez longue pour passer à la ligne suivante";
        let wrapped = super::wrap_help(row, 40);
        let column = wrapped.lines().next().unwrap().find("une").unwrap();
        let first = &wrapped.lines().next().unwrap()[..column];
        assert!(
            wrapped.lines().skip(1).all(|line| {
                line.chars().take_while(|c| *c == ' ').count() == first.chars().count()
            }),
            "{wrapped}"
        );
    }

    /// a word wider than the help's width, such as a link, goes on a line of its own, whole.
    #[test]
    fn help_keeps_a_word_wider_than_the_terminal_whole() {
        let link = "https://huggingface.co/datasets/ILSVRC/imagenet-1k/resolve/main/train.tar";
        let text = format!("request access and accept the terms at {link} while signed in");
        let wrapped = super::wrap_help(&text, 40);
        assert!(wrapped.lines().any(|line| line == link), "{wrapped}");
        assert_eq!(
            wrapped.split_whitespace().collect::<Vec<_>>(),
            text.split_whitespace().collect::<Vec<_>>()
        );
    }

    /// fetchloom never wraps an error: each line of it is written whole, and a terminal narrower
    /// than a line wraps it where its last column falls, even inside a word.
    #[test]
    fn an_error_is_written_one_line_for_each_part() {
        let said = "huggingface.co says: Access to dataset ILSVRC/imagenet-1k is restricted. You must have access to it and be authenticated to access it. Please log in.";
        let help = "request access and accept the terms at https://huggingface.co/datasets/ILSVRC/imagenet-1k while signed in, then sign in here with `fetchloom auth login hf`";
        let error = crate::error::Error::new(
            crate::error::Kind::AuthRequired,
            "hf:datasets/ILSVRC/imagenet-1k requires permission to download its files",
        )
        .detail(said)
        .help(help);
        let shown = super::error_text(&error, Level::Normal, false);
        assert_eq!(
            shown.lines().collect::<Vec<_>>(),
            [
                "error: hf:datasets/ILSVRC/imagenet-1k requires permission to download its files",
                &format!("  {said}"),
                &format!("  help: {help}"),
            ]
        );
    }

    /// a stop the person asked for is no failure, as a cancel they chose is not: a `Stopped`
    /// line in the warning color made from the error's sentence, its supporting lines dim under
    /// it and its help as a hint, never `error:`, even with -v.
    #[test]
    fn a_stop_is_a_plain_line_not_an_error() {
        use crate::error::{Error, Kind};
        let kept = "the 18.3 MiB downloaded so far is kept, and the next run continues from there";
        let stop = Error::new(Kind::Interrupted, "stopped while talking to 127.0.0.1")
            .detail(kept)
            .help("run `fetchloom sync` again");
        assert_eq!(
            super::failure_text(&stop, Level::Verbose, false)
                .lines()
                .collect::<Vec<_>>(),
            [
                "    Stopped while talking to 127.0.0.1",
                &format!("            {kept}"),
                "  hint: run `fetchloom sync` again",
            ]
        );
        for (message, shown) in [
            ("stopped", "    Stopped"),
            (
                "stopped, and nothing was added",
                "    Stopped, and nothing was added",
            ),
            ("the list closed", "    Stopped: the list closed"),
        ] {
            let stop = Error::new(Kind::Interrupted, message);
            assert_eq!(super::failure_text(&stop, Level::Normal, false), shown);
        }
        let warn = super::toned(true, super::Tone::Warn);
        let dim = Paint::new().dimmed();
        assert_eq!(
            super::failure_text(&stop, Level::Normal, true)
                .lines()
                .take(2)
                .collect::<Vec<_>>(),
            [
                format!("{warn}    Stopped{warn:#} while talking to 127.0.0.1"),
                format!("            {dim}{kept}{dim:#}"),
            ]
        );
        // a cancel at a question, which ctrl-c ends as interrupted too, still says what it cancelled
        let cancel = Error::new(Kind::Interrupted, "stopped").cancelled("the question");
        assert_eq!(
            super::failure_text(&cancel, Level::Normal, false),
            "  Cancelled the question"
        );
    }

    #[test]
    fn a_pasted_secret_shows_only_its_ends_and_length() {
        assert_eq!(masked("q"), "* \u{b7} 1 character");
        assert_eq!(
            masked("KGAT_0123456789abcdef0123456789abcd"),
            "KGAT...abcd \u{b7} 35 characters"
        );
        assert_eq!(masked("hunter2"), "******* \u{b7} 7 characters");
        assert_eq!(masked(&"p".repeat(20)), "************ \u{b7} 20 characters");
    }

    #[test]
    fn final_lines_share_the_verb_column_without_status_symbols() {
        for (verb, text, expected) in [
            (
                "Added",
                "iris, its files are in data/iris",
                "      Added iris, its files are in data/iris",
            ),
            (
                "Fetched",
                "iris.csv into iris",
                "    Fetched iris.csv into iris",
            ),
            (
                "Synced",
                "1,000 datasets into data",
                "     Synced 1,000 datasets into data",
            ),
            (
                "Ok",
                "everything matches data.lock",
                "         Ok everything matches data.lock",
            ),
        ] {
            let shown = super::verb_line(verb, text, super::Tone::Done, false);
            assert_eq!(shown, expected);
            assert!(!shown.contains('\u{1b}'));
        }
        assert_eq!(
            super::verb_line(
                "Added",
                "iris, its files are in data/iris",
                super::Tone::Done,
                true,
            ),
            "\u{1b}[1m\u{1b}[32m      Added\u{1b}[0m iris, its files are in data/iris"
        );
    }

    #[test]
    fn a_fix_is_the_first_runnable_command_in_the_help() {
        assert_eq!(
            super::fix(
                "sign in with `fetchloom auth login hf`, then run the same command again",
                false
            ),
            Some("fetchloom auth login hf")
        );
        assert_eq!(
            super::fix(
                "run `fetchloom add <link>` to start, or `fetchloom search iris`",
                false
            ),
            Some("fetchloom search iris")
        );
        assert_eq!(
            super::fix("add --yes to download it without being asked", false),
            None
        );
        assert_eq!(
            super::fix("quote it, as in `\"fashion mnist\"`", false),
            None
        );
        assert_eq!(
            super::fix(
                "use a link or an id, as in `fetchloom add zenodo:3608990`",
                false
            ),
            None
        );
        assert_eq!(
            super::fix("did you mean `fetchloom remove iris`?", false),
            None
        );
        // a command named without what it needs is no fix a script could run
        assert_eq!(
            super::fix(
                "ask its owner for a public link to pass to `fetchloom add` or `fetchloom get`",
                false
            ),
            None
        );
        assert_eq!(
            super::fix(
                "in your own terminal, run `fetchloom auth login hf`. a script can pipe the key in, as in `fetchloom auth login hf --with-token`",
                false
            ),
            None
        );
        assert_eq!(
            super::fix(
                "delete git's conflict lines by hand, then run `fetchloom lock`",
                false
            ),
            None
        );
        assert_eq!(
            super::fix(
                "downloaded files stay in the cache. fix the errors above, then run `fetchloom sync` again",
                false
            ),
            None,
            "files that failed on their own fail again until someone fixes them"
        );
        assert_eq!(
            super::fix(
                "run `fetchloom build clean`, then commit data.lock and data.lock.d",
                false
            ),
            None,
            "a build runs the project's code"
        );
        assert_eq!(
            super::fix("run `fetchloom build -n clean` to see what it runs", false),
            Some("fetchloom build -n clean")
        );
        assert_eq!(
            super::fix(
                "to keep the pinned bytes, someone who has them runs `fetchloom push --to <folder>`, then you run `fetchloom sync --mirror <folder>`. if the new bytes are intended, run `fetchloom lock --upgrade iris`",
                false
            ),
            None,
            "only a person may take other bytes than data.lock pins"
        );
        // a run asked to leave data.lock as it is gets no fix that writes it, and keeps one that
        // does not
        for (help, fix) in [
            ("run `fetchloom lock` and commit data.lock", None),
            ("run `fetchloom sync iris`", None),
            (
                "run `fetchloom lock --check` to see what moved",
                Some("fetchloom lock --check"),
            ),
            (
                "run `fetchloom sync --locked` again later",
                Some("fetchloom sync --locked"),
            ),
        ] {
            assert_eq!(super::fix(help, true), fix, "{help}");
        }
        assert_eq!(
            super::fix("run `fetchloom lock` and commit data.lock", false),
            Some("fetchloom lock")
        );
    }

    /// a report row is a verb line for stdout, colored as stdout decides apart from stderr, and
    /// quiet or `--json` print none.
    #[test]
    fn report_rows_follow_stdout_alone() {
        let style = |color| Style {
            color,
            unicode: true,
            interactive: false,
            watched: false,
            links: false,
        };
        let mut colored_story = Output::new(style(true), Level::Normal, false, false);
        colored_story.report_color = false;
        assert_eq!(
            colored_story
                .report_text("Missing", "data/iris/iris.csv")
                .as_deref(),
            Some("    Missing data/iris/iris.csv")
        );
        let mut plain_story = Output::new(style(false), Level::Normal, false, false);
        plain_story.report_color = true;
        let row = plain_story
            .report_text("Missing", "data/iris/iris.csv")
            .unwrap_or_default();
        assert!(row.contains("\u{1b}["), "{row:?}");
        assert_eq!(
            console::strip_ansi_codes(&row),
            "    Missing data/iris/iris.csv"
        );
        // a row is a difference whatever its verb, as a mark is, never a finished step
        let damaged = plain_story
            .report_text("Damaged", "data/iris/iris.csv")
            .unwrap_or_default();
        assert_eq!(damaged.replace("Damaged", "Missing"), row);
        for (level, json) in [(Level::Quiet, false), (Level::Normal, true)] {
            let silent = Output::new(style(false), level, json, false);
            assert_eq!(silent.report_text("Missing", "data/iris/iris.csv"), None);
            silent.report("Missing", "data/iris/iris.csv");
        }
    }

    /// the `-v` counters say what each counts: a placement from bytes the cache held before is
    /// not a download that did not happen, and a file the run wrote to the cache, as a one-off
    /// `get -o` does before moving it out, is not said to be in it still.
    #[test]
    fn work_counters_say_what_happened_in_the_run() {
        let second = Duration::from_secs(2);
        let reused = Counts {
            requests: 1,
            bytes_down: 50_000_000,
            hardlinks: 1,
            placed_from_cache: 1,
            ..Counts::default()
        };
        assert_eq!(
            super::work_line(reused, second),
            "1 request · 47.7 MiB down · 1 hardlink · 1 placed from the cache · 2.0 s in all"
        );
        let one_off = Counts {
            requests: 1,
            bytes_down: 4551,
            objects_written: 1,
            copies: 1,
            ..Counts::default()
        };
        assert_eq!(
            super::work_line(one_off, second),
            "1 request · 4.4 KiB down · 1 file written to the cache · 1 copy · 2.0 s in all"
        );
    }

    /// two hashes on a line read apart wherever they differ, at least 12 digits each; a lone
    /// hash takes 12, and hex digits in a path or a longer word stay as they are.
    #[test]
    fn hashes_on_a_line_read_apart_however_late_they_differ() {
        let hash = "e54cc4d2ce1bff65d32ca60b3e4b802e06bde1d7e7caf6f796f6bf7370e863b1";
        let at = |index: usize| {
            let mut other = hash.to_owned();
            other.replace_range(
                index..=index,
                if &hash[index..=index] == "0" {
                    "1"
                } else {
                    "0"
                },
            );
            other
        };
        let (late, early) = (at(40), at(3));
        assert_eq!(
            super::hashes_apart(&format!("expected sha256 {hash}, got {late}")),
            format!("expected sha256 {}, got {}", &hash[..41], &late[..41])
        );
        assert_eq!(
            super::hashes_apart(&format!("expected sha256 {hash}, got {early}")),
            format!("expected sha256 {}, got {}", &hash[..12], &early[..12])
        );
        assert_eq!(
            super::hashes_apart(&format!("got {hash}.")),
            format!("got {}.", &hash[..12])
        );
        for untouched in [
            format!("could not read /cache/builds/{hash}/lock"),
            format!("sha256:{hash}"),
            format!("{hash}0 is no hash"),
            "expected 32 bytes, got 15".to_owned(),
        ] {
            assert_eq!(super::hashes_apart(&untouched), untouched);
        }
    }

    #[test]
    fn the_wordmark_shows_only_where_it_fits() {
        assert_eq!(super::wordmark(98, false), "");
        let shown = super::wordmark(100, false);
        assert_eq!(shown.lines().count(), 8);
        assert!(shown.ends_with("\\|__|\n\n"));
        assert!(!shown.contains('\u{1b}'));
        assert!(super::wordmark(100, true).contains('\u{1b}'));
    }

    #[test]
    fn step_colors_follow_the_work_being_reported() {
        use super::Tone::{Accent, Done, Fail, Warn, Work};

        for (verbs, expected) in [
            (
                &["Downloaded", "Unpacked", "Locked", "Placed", "Removed"][..],
                Done,
            ),
            (
                &[
                    "Downloading",
                    "Unpacking",
                    "Waiting",
                    "Searching",
                    "Would place",
                    "Would fetch",
                    "Would remove",
                ][..],
                Work,
            ),
            (&["1", "12"][..], Accent),
            (
                &[
                    "Missing",
                    "Extra",
                    "Kept",
                    "Edited",
                    "Untracked",
                    "Outdated",
                ][..],
                Warn,
            ),
            (&["Failed"][..], Fail),
        ] {
            for verb in verbs {
                assert_eq!(super::verb_color(verb), expected, "{verb}");
            }
        }
    }

    #[test]
    fn error_and_help_are_plain_when_color_is_off() {
        let error = crate::error::Error::new(crate::error::Kind::OfflineMissing, "iris is missing")
            .detail("iris.csv is not in the cache")
            .help("run `fetchloom sync` again");
        let shown = super::error_text(&error, Level::Normal, false);
        assert_eq!(
            shown,
            "error: iris is missing\n  iris.csv is not in the cache\n  help: run `fetchloom sync` again"
        );
        assert!(!shown.contains('\u{1b}'));
        let verbose = super::error_text(&error, Level::Verbose, false);
        assert!(verbose.ends_with("\n  kind: offline.missing"));
        assert!(!verbose.contains('\u{1b}'));
    }

    #[test]
    fn error_and_help_color_the_labels_and_dim_the_details() {
        let error = crate::error::Error::new(crate::error::Kind::OfflineMissing, "iris is missing")
            .detail("iris.csv is not in the cache")
            .help("run `fetchloom sync` again");
        assert_eq!(
            super::error_text(&error, Level::Normal, true),
            concat!(
                "\u{1b}[1m\u{1b}[31merror:\u{1b}[0m iris is missing",
                "\n  \u{1b}[2miris.csv is not in the cache\u{1b}[0m",
                "\n  \u{1b}[1m\u{1b}[36mhelp:\u{1b}[0m run ",
                "\u{1b}[1m\u{1b}[36m`fetchloom sync`\u{1b}[0m again",
            )
        );
    }

    #[test]
    fn hints_are_dim_with_commands_in_bold_cyan() {
        let text = "try `fetchloom path iris` next";
        assert_eq!(
            super::hint_line(text, false),
            "  hint: try `fetchloom path iris` next"
        );
        assert_eq!(
            super::hint_line(text, true),
            concat!(
                "  \u{1b}[2mhint: try \u{1b}[0m\u{1b}[1m\u{1b}[36m",
                "`fetchloom path iris`\u{1b}[0m\u{1b}[2m next\u{1b}[0m",
            )
        );
    }

    #[test]
    fn help_styles_headings_and_commands_without_changing_plain_text() {
        let text = concat!(
            "usage: fetchloom get [options] <link>\n\ncommands:\n",
            "  get  fetch once\n\nexamples:\n  fetchloom get zenodo:1\n",
            "\n  data.toml, the shopping list: which datasets you want\n",
        );
        assert_eq!(super::help_text(text, false), text);
        let shown = super::help_text(text, true);
        assert!(shown.contains("\u{1b}[1musage:\u{1b}[0m"));
        assert!(shown.contains("\u{1b}[1mcommands:\u{1b}[0m"));
        assert!(shown.contains("\u{1b}[36mfetchloom get zenodo:1\u{1b}[0m"));
        assert!(shown.contains("  \u{1b}[36mget\u{1b}[0m  fetch once"));
        assert!(shown.contains("data.toml\u{1b}[0m, the shopping list: which datasets you want"));
    }

    #[test]
    fn the_first_ctrl_c_at_a_question_is_interrupted() {
        for key in [
            Ok(console::Key::CtrlC),
            Err(std::io::ErrorKind::Interrupted.into()),
        ] {
            let mut keys = std::iter::once(key);
            let error = super::answer(|| keys.next().unwrap(), |_| {}, false).unwrap_err();
            assert_eq!(error.kind, crate::error::Kind::Interrupted);
            assert_eq!(error.kind.exit_code(), 130);
            assert_eq!(error.message, "stopped");
            assert_eq!(error.cancelled.as_deref(), Some("the question"));
        }
    }

    /// y or yes and n or no answer a question in any case, and enter alone takes its default.
    /// anything else, such as q typed to quit, is asked again, so it never saves or downloads
    /// anything as a yes would.
    #[test]
    fn an_answer_other_than_yes_or_no_is_asked_again_not_taken_as_the_default() {
        for default in [true, false] {
            for (typed, said) in [
                ("y", Some(true)),
                ("Yes", Some(true)),
                (" yes ", Some(true)),
                ("n", Some(false)),
                ("NO", Some(false)),
                ("", Some(default)),
                ("q", None),
                ("quit", None),
                ("yeah", None),
                ("y n", None),
            ] {
                let mut keys = typed
                    .chars()
                    .map(console::Key::Char)
                    .chain([console::Key::Enter])
                    .map(Ok);
                let read = super::answer(|| keys.next().unwrap(), |_| {}, default);
                assert_eq!(read.unwrap(), said, "{typed:?}, {default} by default");
            }
        }
    }

    /// a terminal that closed before an answer came, as at the end of its input, answers no.
    #[test]
    fn a_question_whose_terminal_closed_is_answered_no() {
        let closed = || Err(std::io::ErrorKind::UnexpectedEof.into());
        assert_eq!(super::answer(closed, |_| {}, true).unwrap(), Some(false));
    }

    #[test]
    fn commands_in_backticks_take_the_accent_and_keep_their_backticks() {
        let dim = Paint::new().dimmed();
        assert_eq!(
            accented("run `fetchloom sync` again", Paint::new(), true),
            "run \u{1b}[1m\u{1b}[36m`fetchloom sync`\u{1b}[0m again"
        );
        assert_eq!(
            accented("commit it; then `fetchloom sync`", dim, true),
            "commit it; then \u{1b}[0m\u{1b}[1m\u{1b}[36m`fetchloom sync`\u{1b}[0m\u{1b}[2m"
        );
        assert_eq!(
            accented("an odd ` mark", Paint::new(), true),
            "an odd ` mark"
        );
    }

    #[test]
    fn a_step_names_its_subject_in_bold() {
        assert_eq!(
            subject("iris · 1 file in data.lock", true),
            "\u{1b}[1miris\u{1b}[0m\u{1b}[2m · 1 file in data.lock\u{1b}[0m"
        );
        assert_eq!(
            subject("data/iris/iris.csv", true),
            "\u{1b}[1mdata/iris/iris.csv\u{1b}[0m"
        );
        assert_eq!(
            subject("for another fetchloom working in this project", true),
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
        let style = |watched| Style {
            color: false,
            unicode: true,
            interactive: false,
            watched,
            links: false,
        };
        let shows =
            |watched, level, json| Output::new(style(watched), level, json, false).shows_next();
        assert!(shows(true, Level::Normal, false));
        assert!(
            !Output::new(style(true), Level::Normal, false, false)
                .without_hints(true)
                .shows_next(),
            "FETCHLOOM_NO_HINTS=1"
        );
        assert!(shows(true, Level::Verbose, false));
        assert!(!shows(true, Level::Quiet, false), "-q");
        assert!(!shows(true, Level::Normal, true), "--json");
        assert!(
            !shows(false, Level::Normal, false),
            "stdout or stderr is not a terminal"
        );
    }

    #[test]
    fn bars_keep_moving_while_a_question_waits() {
        let style = Style {
            color: false,
            unicode: false,
            interactive: true,
            watched: true,
            links: false,
        };
        let output = Output::new(style, Level::Normal, false, false);
        let bars = output.bars.as_ref().unwrap();
        let bar = bars.add(indicatif::ProgressBar::new(1000));
        let moved = output.asking(|| {
            let (sender, receiver) = std::sync::mpsc::channel();
            let other = bar.clone();
            std::thread::spawn(move || {
                for _ in 0..100 {
                    other.inc(1);
                    other.tick();
                }
                let _ = sender.send(());
            });
            receiver
                .recv_timeout(std::time::Duration::from_secs(5))
                .is_ok()
        });
        assert!(moved, "a download stood still while the question waited");
        assert_eq!(bar.position(), 100);
    }

    #[test]
    fn a_typed_line_ends_at_enter_and_ctrl_c_cancels_it() {
        use console::Key::{Backspace, Char, CtrlC, Enter, Tab};

        use super::Edit::{Erased, Typed};
        fn keys(
            keys: Vec<std::io::Result<console::Key>>,
        ) -> impl FnMut() -> std::io::Result<console::Key> {
            let mut keys = keys.into_iter();
            move || keys.next().unwrap_or(Ok(Enter))
        }
        let mut edits = Vec::new();
        let line = super::typed_line(
            keys(vec![
                Ok(Backspace),
                Ok(Char('a')),
                Ok(Char('b')),
                Ok(Backspace),
                Ok(Char('é')),
                Ok(Char(' ')),
                Ok(Tab),
                Ok(Char('\u{4}')),
                Ok(Enter),
                Ok(Char('x')),
            ]),
            |edit| edits.push(edit),
        )
        .unwrap();
        assert_eq!(line, "aé ");
        assert_eq!(
            edits,
            [Typed('a'), Typed('b'), Erased('b'), Typed('é'), Typed(' ')]
        );
        for stop in [Ok(CtrlC), Err(std::io::ErrorKind::Interrupted.into())] {
            let error = super::typed_line(keys(vec![Ok(Char('s')), stop]), |_| {}).unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
        }
        let error = super::typed_line(
            keys(vec![Err(std::io::ErrorKind::UnexpectedEof.into())]),
            |_| {},
        )
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::UnexpectedEof);
    }

    /// a terminal that records what bars draw on it.
    #[derive(Debug, Default, Clone)]
    struct Screen(std::sync::Arc<std::sync::Mutex<Vec<String>>>);

    impl Screen {
        fn record(&self, what: String) {
            self.0.lock().unwrap().push(what);
        }
    }

    impl indicatif::TermLike for Screen {
        fn width(&self) -> u16 {
            80
        }
        fn move_cursor_up(&self, n: usize) -> std::io::Result<()> {
            self.record(format!("up {n}"));
            Ok(())
        }
        fn move_cursor_down(&self, n: usize) -> std::io::Result<()> {
            self.record(format!("down {n}"));
            Ok(())
        }
        fn move_cursor_right(&self, n: usize) -> std::io::Result<()> {
            self.record(format!("right {n}"));
            Ok(())
        }
        fn move_cursor_left(&self, n: usize) -> std::io::Result<()> {
            self.record(format!("left {n}"));
            Ok(())
        }
        fn write_line(&self, s: &str) -> std::io::Result<()> {
            self.record(format!("line {s}"));
            Ok(())
        }
        fn write_str(&self, s: &str) -> std::io::Result<()> {
            self.record(format!("text {s}"));
            Ok(())
        }
        fn clear_line(&self) -> std::io::Result<()> {
            self.record("clear".to_owned());
            Ok(())
        }
        fn flush(&self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn telling(interactive: bool) -> Output {
        let style = Style {
            color: false,
            unicode: false,
            interactive,
            watched: interactive,
            links: false,
        };
        let mut output = Output::new(style, Level::Normal, false, false);
        output.bars = None;
        output
    }

    /// a terminal drops what a program wrote but had not yet shown when a ctrl-c comes, so a
    /// closing line told before a ctrl-c that lets the run finish is told again at the end, and
    /// one told after it, or with no ctrl-c, only once.
    #[test]
    fn a_closing_line_before_a_finishing_ctrl_c_is_told_again() {
        let added = "      Added iris, its files are in data/iris";
        let finishing = "  Finishing what it started (ctrl-c again to quit at once)";
        let late = telling(false);
        late.done("Added", "iris, its files are in data/iris");
        late.finishing();
        late.retell();
        assert_eq!(late.said(), [added, finishing, added]);
        let early = telling(false);
        early.finishing();
        early.done("Added", "iris, its files are in data/iris");
        early.retell();
        assert_eq!(early.said(), [finishing, added]);
        let calm = telling(false);
        calm.done("Added", "iris, its files are in data/iris");
        calm.retell();
        assert_eq!(calm.said(), [added]);
    }

    /// a terminal echoes ctrl-c as `^C` at the start of the empty line where its cursor waits, so
    /// the line told then clears it first, and every line starts from the start of its own line,
    /// writing over an echo the command's next line once followed, so the verb column stays; a
    /// pipe gets the lines alone.
    #[test]
    fn a_line_told_at_a_ctrl_c_clears_the_echo() {
        let stopping = "   Stopping (press ctrl-c again to quit at once)";
        let terminal = telling(true);
        terminal.stopping();
        terminal.finishing_downloads(1);
        terminal.done("Added", "late, its files are in data/late");
        assert_eq!(
            terminal.said(),
            [
                format!("\r\u{1b}[2K{stopping}"),
                "\r\u{1b}[2K  Finishing (the download is complete; ctrl-c again to quit at once)"
                    .to_owned(),
                "\r      Added late, its files are in data/late".to_owned(),
            ]
        );
        let piped = telling(false);
        piped.stopping();
        assert_eq!(piped.said(), [stopping]);
    }

    /// a wait for another program counts its seconds in a live line on a terminal, where a fixed
    /// line once stood still for as long as the wait, and is one plain line in a log or a pipe.
    #[test]
    fn a_wait_for_another_program_counts_on_a_terminal() {
        let text = "for `fetchloom add` (pid 7) working in this project";
        let mut terminal = telling(false);
        terminal.bars = Some(indicatif::MultiProgress::with_draw_target(
            indicatif::ProgressDrawTarget::term_like(Box::new(Screen::default())),
        ));
        let live = terminal.waiting("Waiting", text);
        assert_eq!(live.told().as_deref(), Some(text));
        assert!(terminal.said().is_empty(), "{:?}", terminal.said());
        let piped = telling(false);
        let none = piped.waiting("Waiting", text);
        assert_eq!(none.told(), None);
        assert_eq!(piped.said(), [format!("    Waiting {text}")]);
    }

    /// work that grows with its size, such as placing many files, gets a live line at a
    /// terminal once it lasts, where it once went untold for seconds after `Imported`.
    #[test]
    fn lasting_work_is_told_once_it_lasts() {
        let mut terminal = telling(false);
        let screen = Screen::default();
        terminal.bars = Some(indicatif::MultiProgress::with_draw_target(
            indicatif::ProgressDrawTarget::term_like(Box::new(screen.clone())),
        ));
        terminal.untold = Duration::ZERO;
        let placing = terminal.lasting("Placing", "20,000 files".to_owned());
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while !screen
            .0
            .lock()
            .unwrap()
            .iter()
            .any(|drawn| drawn.contains("Placing 20,000 files"))
        {
            assert!(std::time::Instant::now() < deadline, "no line");
            std::thread::sleep(Duration::from_millis(10));
        }
        drop(placing);
    }

    #[test]
    fn a_run_that_ends_at_once_clears_its_bars_first() {
        let style = Style {
            color: false,
            unicode: false,
            interactive: false,
            watched: false,
            links: false,
        };
        let mut output = Output::new(style, Level::Normal, false, false);
        let screen = Screen::default();
        output.bars = Some(indicatif::MultiProgress::with_draw_target(
            indicatif::ProgressDrawTarget::term_like(Box::new(screen.clone())),
        ));
        let bars = output.bars.as_ref().unwrap();
        let bar = bars.add(indicatif::ProgressBar::new(10));
        bar.set_message("iris.csv");
        bar.tick();
        assert!(!bars.is_hidden());
        let drawn = screen.0.lock().unwrap().len();
        assert!(drawn > 0);

        output.abandon();

        let after = screen.0.lock().unwrap()[drawn..].to_vec();
        assert!(after.contains(&"clear".to_owned()), "{after:?}");
        assert!(bars.is_hidden());
        bar.inc(5);
        bar.tick();
        assert_eq!(screen.0.lock().unwrap().len(), drawn + after.len());
    }
}
