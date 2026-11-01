//! what fetchloom prints. stdout carries the answer and nothing else, stderr carries the story.

mod picker;
mod progress;
mod style;

use std::borrow::Cow;
use std::fmt::Write as _;
use std::io::{IsTerminal, Write};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use anstyle::Style as Paint;
use indicatif::MultiProgress;
use serde::Serialize;

pub(crate) use progress::{Bar, Ticker, Wait};
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
pub(crate) struct Output {
    style: Style,
    level: Level,
    json: bool,
    bars: Option<MultiProgress>,
    warnings: Mutex<Vec<String>>,
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

/// the first `fetchloom` command quoted in `help`, without its backticks.
fn fix(help: &str) -> Option<&str> {
    help.split('`')
        .skip(1)
        .step_by(2)
        .find(|quoted| quoted.starts_with("fetchloom ") && !quoted.contains(['<', '>']))
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
        let unread = style::load_palette();
        // indicatif colors through the console crate, which decides on its own unless told
        console::set_colors_enabled_stderr(style.color);
        let bars = (style.interactive && !json && !logs && level >= Level::Normal)
            .then(MultiProgress::new);
        let output = Output {
            style,
            level,
            json,
            bars,
            warnings: Mutex::new(Vec::new()),
        };
        if !unread.is_empty() {
            output.warn(&format!(
                "FETCHLOOM_COLORS: kept the default for {}; entries look like done=green, work=214 or accent=#d787ff",
                unread.join(", ")
            ));
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
            let dim = paint(self.style.color, Paint::new().dimmed());
            self.line(&format!("{:>11} {dim}{}{dim:#}", "", plain(text)));
        }
    }

    pub(crate) fn warn(&self, text: &str) {
        self.warnings
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(text.to_owned());
        if self.tells(Level::Normal) {
            let yellow = toned(self.style.color, Tone::Warn);
            self.line(&format!(
                "{yellow}warning:{yellow:#} {}",
                accented(&plain(text), Paint::new(), self.style.color)
            ));
        }
    }

    /// the closing line of a run that worked.
    pub(crate) fn done(&self, verb: &str, text: &str) {
        if self.tells(Level::Normal) {
            self.line(&verb_line(verb, text, Tone::Done, self.style.color));
        }
    }

    pub(crate) fn hint(&self, text: &str) {
        if self.tells(Level::Normal) {
            self.line(&hint_line(text, self.style.color));
        }
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
    fn shows_next(&self) -> bool {
        self.style.watched && self.tells(Level::Normal)
    }

    /// free text from a source, such as a title, cut to fit one line of the story.
    pub(crate) fn clip(&self, text: &str, most: usize) -> String {
        units::clip(text, most, self.style.more())
    }

    /// the width of the terminal a person watches the story on, or none for a pipe or a file.
    fn width(&self) -> Option<usize> {
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

    /// one numbered choice: what to type, padded to `pad`, then its title and dim facts. on a
    /// narrow terminal the facts, then the title, give way, and what to type never does.
    pub(crate) fn candidate(
        &self,
        number: usize,
        typed: &str,
        pad: usize,
        title: &str,
        facts: &str,
    ) {
        if !self.tells(Level::Normal) {
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
            let mut rest = String::new();
            if !title.is_empty() {
                let _ = write!(rest, "  {title}");
            }
            if !facts.is_empty() {
                let gap = if title.is_empty() { "  " } else { " \u{b7} " };
                let _ = write!(rest, "{gap}{facts}");
            }
            if rest.chars().count() > room {
                rest = console::truncate_str(&rest, room, self.style.more()).into_owned();
            }
            if rest.trim().chars().count() > 3 {
                let split = if title.is_empty() {
                    0
                } else {
                    rest.find(" \u{b7} ").unwrap_or(rest.len())
                };
                let (named, told) = rest.split_at(split);
                let _ = write!(line, "{named}{dim}{told}{dim:#}");
            }
        }
        self.line(line.trim_end());
    }

    /// whether `--json` asked for one document on stdout instead of text.
    pub(crate) fn is_json(&self) -> bool {
        self.json
    }

    /// the answer on stdout, such as a path a script will read.
    pub(crate) fn answer(&self, text: &str) {
        if !self.json {
            let _ = writeln!(std::io::stdout().lock(), "{text}");
        }
    }

    /// whether a person watches stderr, so a list there is read rather than logged twice.
    pub(crate) fn watched_story(&self) -> bool {
        self.style.interactive
    }

    /// a dim remark for a person at a terminal, never for a script, such as which project a
    /// command found above the current folder.
    pub(crate) fn aside(&self, text: &str) {
        if self.shows_next() {
            let dim = paint(self.style.color, Paint::new().dimmed());
            self.line(&format!("  {dim}{}{dim:#}", plain(text)));
        }
    }

    /// an answer the story already showed, printed only when a script reads stdout or `-q`
    /// hid the story.
    pub(crate) fn answer_untold(&self, text: &str) {
        if !self.shows_next() {
            self.answer(text);
        }
    }

    /// a failure: the sentence, its supporting lines, and what to do next.
    pub(crate) fn error(&self, error: &Error) {
        if self.json {
            return;
        }
        self.line(&error_text(error, self.level, self.style.color));
    }

    /// a bar for one download, hidden unless a person is watching the terminal.
    pub(crate) fn bar(&self, verb: &'static str, message: String) -> Bar {
        match &self.bars {
            Some(bars) => Bar::add(bars, verb, message, self.style.color, self.style.unicode),
            None => Bar::hidden(),
        }
    }

    /// a wait ends when dropped, with elapsed time on a terminal and occasional lines elsewhere.
    pub(crate) fn wait(&self, verb: &'static str, what: String) -> Wait {
        Wait {
            _bar: self.bar(verb, what.clone()),
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

    /// a yes or no question whose enter key means `default`, or none when nobody can answer.
    pub(crate) fn ask_yes_no(&self, question: &str, default: bool) -> Result<Option<bool>, Error> {
        if !self.can_ask() {
            return Ok(None);
        }
        let ask = || {
            let terminal = console::Term::stderr();
            let choices = if default { "[Y/n]" } else { "[y/N]" };
            let _ = terminal.write_str(&format!("{question} {choices} "));
            // read ctrl-c as a key on both platforms instead of leaving a blocking read
            // waiting after the signal handler asks network tasks to stop.
            let answer = loop {
                match confirmation_key(terminal.read_key_raw(), default) {
                    Ok(None) => {}
                    answer => break answer,
                }
            };
            // the question may have wrapped, so the answer goes after it rather than over it
            let selected = match &answer {
                Ok(Some(true)) => "yes",
                Ok(Some(false)) => "no",
                _ => "",
            };
            let _ = terminal.write_line(selected);
            answer
        };
        match &self.bars {
            Some(bars) => bars.suspend(ask),
            None => ask(),
        }
    }

    /// asks for a line of text, such as a username, or returns none when nobody can answer.
    /// with `hidden`, nothing typed or pasted shows until enter, then a masked echo confirms it
    /// arrived. a blank answer is asked again, twice at most.
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
                let read = if hidden {
                    terminal.read_secure_line()
                } else {
                    terminal.read_line()
                };
                let text = match read {
                    Ok(text) => text.trim().to_owned(),
                    Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {
                        return Err(Error::new(crate::error::Kind::Interrupted, "stopped"));
                    }
                    Err(err) => return Err(Error::io("could not read from the terminal", &err)),
                };
                if hidden && !text.is_empty() {
                    let _ = terminal.clear_last_lines(1);
                    let _ = terminal.write_line(&format!("{label}{}", masked(&text)));
                }
                if !text.is_empty() {
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
        match &self.bars {
            Some(bars) => bars.suspend(ask),
            None => ask(),
        }
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
    ) -> Result<Option<usize>, Error> {
        if self.json || !self.style.interactive || !std::io::stdin().is_terminal() {
            return Ok(None);
        }
        let ask = || picker::choose(question, options, self.style);
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
                fix: error.help.as_deref().and_then(fix),
                failures: (!error.failures.is_empty()).then_some(error.failures.as_slice()),
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
        let mut out = std::io::stdout().lock();
        if serde_json::to_writer(&mut out, &document).is_ok() {
            let _ = writeln!(out);
        }
    }

    fn tells(&self, level: Level) -> bool {
        !self.json && self.level >= level
    }

    fn line(&self, text: &str) {
        let text = shown(text, self.style.unicode);
        let write = || write_line(self.style.color, &text);
        match &self.bars {
            Some(bars) => bars.suspend(write),
            None => write(),
        }
    }
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

pub(crate) fn help(text: &str, choice: ColorChoice) {
    let shown = help_text(text.trim_end_matches('\n'), Style::stdout_color(choice));
    let _ = writeln!(std::io::stdout().lock(), "{shown}");
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

fn error_text(error: &Error, level: Level, color: bool) -> String {
    let red = toned(color, Tone::Fail);
    let cyan = paint(color, accent().bold());
    let dim = paint(color, Paint::new().dimmed());
    let mut text = format!(
        "{red}error:{red:#} {}",
        accented(&plain(&error.message), Paint::new(), color)
    );
    for line in &error.details {
        let _ = write!(
            text,
            "\n  {dim}{}{dim:#}",
            accented(&plain(line), dim, color)
        );
    }
    if let Some(help) = &error.help {
        let _ = write!(
            text,
            "\n  {cyan}help:{cyan:#} {}",
            accented(&plain(help), Paint::new(), color)
        );
    }
    if level >= Level::Verbose {
        let _ = write!(text, "\n  {dim}kind: {}{dim:#}", error.kind.name());
    }
    text
}

fn hint_line(text: &str, color: bool) -> String {
    let dim = paint(color, Paint::new().dimmed());
    format!("  {dim}hint: {}{dim:#}", accented(&plain(text), dim, color))
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

fn confirmation_key(
    key: std::io::Result<console::Key>,
    default: bool,
) -> Result<Option<bool>, Error> {
    let key = match key {
        Ok(key) => key,
        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => console::Key::CtrlC,
        Err(_) => return Ok(Some(false)),
    };
    match key {
        console::Key::CtrlC => Err(Error::new(crate::error::Kind::Interrupted, "stopped")),
        console::Key::Char('y' | 'Y') => Ok(Some(true)),
        console::Key::Char('n' | 'N') => Ok(Some(false)),
        console::Key::Enter => Ok(Some(default)),
        _ => Ok(None),
    }
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
    format!("{shown} \u{b7} {count} characters")
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

    use super::{Level, Output, Style, accented, masked, plain, subject};

    #[test]
    fn a_pasted_secret_shows_only_its_ends_and_length() {
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
            super::fix("sign in with `fetchloom auth login hf`, then run the same command again"),
            Some("fetchloom auth login hf")
        );
        assert_eq!(
            super::fix("run `fetchloom add <link>` to start, or `fetchloom search iris`"),
            Some("fetchloom search iris")
        );
        assert_eq!(super::fix("add --yes to approve this download"), None);
        assert_eq!(super::fix("quote it, as in `\"fashion mnist\"`"), None);
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
                &["Downloading", "Unpacking", "Waiting", "Searching"][..],
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
            let error = super::confirmation_key(key, false).unwrap_err();
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
            assert_eq!(super::confirmation_key(Ok(key), false).unwrap(), answer);
        }
        assert_eq!(
            super::confirmation_key(Err(std::io::ErrorKind::UnexpectedEof.into()), false).unwrap(),
            Some(false)
        );
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
        let shows = |watched, level, json| {
            let style = Style {
                color: false,
                unicode: true,
                interactive: false,
                watched,
                links: false,
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
