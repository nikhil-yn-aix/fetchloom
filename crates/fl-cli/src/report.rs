//! The one event stream and the two reporters over it: the human story on stderr, and the JSON
//! document on stdout.

use std::fmt::Write as _;
use std::io::{self, Write};
use std::sync::{Arc, Mutex, PoisonError};

use fl_model::document::{Document, Status, Work};
use fl_model::error::ErrorKind;
use fl_model::event::{Event, Problem};
use fl_model::units::{count, duration, size};
use serde_json::Value;

use crate::failure::{Failure, Snippet};
use crate::progress::Bars;
use crate::term::{Style, Tone};

/// Something that receives what happened during a command, in order.
pub(crate) trait Reporter: Send + Sync {
    /// Takes one event.
    fn emit(&self, event: Event);

    /// Runs `work` with nothing drawn on the terminal, such as while a question waits.
    fn suspend(&self, work: &mut dyn FnMut()) {
        work();
    }
}

/// How a command ended, for the reporter that closes the run.
pub(crate) struct Outcome {
    /// Everything asked for was done, part of it, or nothing.
    pub(crate) status: Status,
    /// The command's own result, the `result` of the JSON document.
    pub(crate) result: Value,
    /// The one line stdout carries, such as a path.
    pub(crate) answer: Option<String>,
    /// The sentence that says what happened.
    pub(crate) sentence: String,
    /// The kind whose exit code ends a run that did part of its work.
    pub(crate) exit: Option<ErrorKind>,
}

impl Outcome {
    /// A command that did all it was asked: its `result` and the `sentence` saying so.
    pub(crate) fn ok(result: Value, sentence: String) -> Self {
        Self {
            status: Status::Ok,
            result,
            answer: None,
            sentence,
            exit: None,
        }
    }
}

/// The reporter of a run, chosen by `--json`.
pub(crate) enum Output {
    /// Lines on stderr as things happen, the answer on stdout.
    Human(Arc<Human>),
    /// One document on stdout at the end.
    Json(Arc<Json>),
}

impl Output {
    /// The reporter that events go to.
    pub(crate) fn reporter(&self) -> &dyn Reporter {
        match self {
            Self::Human(human) => human.as_ref(),
            Self::Json(json) => json.as_ref(),
        }
    }

    /// The reporter that events go to, for work that outlives a borrow.
    pub(crate) fn shared(&self) -> Arc<dyn Reporter> {
        match self {
            Self::Human(human) => Arc::clone(human) as Arc<dyn Reporter>,
            Self::Json(json) => Arc::clone(json) as Arc<dyn Reporter>,
        }
    }

    /// Ends the run: the answer, the closing sentence and, at `-v`, the work done; or the
    /// document.
    pub(crate) fn finish(&self, command: &str, outcome: Outcome, work: &Work) -> io::Result<()> {
        match self {
            Self::Human(human) => human.finish(&outcome, work),
            Self::Json(json) => json.finish(command, outcome, work),
        }
    }

    /// Ends a run that failed before it had an outcome.
    pub(crate) fn fail(&self, command: &str, failure: Failure, work: &Work) -> io::Result<()> {
        match self {
            Self::Human(human) => {
                human.failed(&failure);
                human.finish(&Outcome::failed(), work)
            }
            Self::Json(json) => {
                json.emit(Event::Failed(failure.0));
                json.finish(command, Outcome::failed(), work)
            }
        }
    }
}

impl Outcome {
    fn failed() -> Self {
        Self {
            status: Status::Error,
            result: Value::Object(serde_json::Map::new()),
            answer: None,
            sentence: String::new(),
            exit: None,
        }
    }
}

/// Writes the human story to stderr.
pub(crate) struct Human {
    loudness: i8,
    style: Style,
    bars: Option<Bars>,
    hint: Mutex<Option<String>>,
}

impl Human {
    /// A reporter at `verbose` minus `quiet` in `style`: below 0 nothing reaches stderr, from 1
    /// the work is summed up at the end. `bars` draws progress, when there is a terminal to
    /// draw on.
    pub(crate) fn new(verbose: u8, quiet: u8, style: Style, bars: Option<Bars>) -> Self {
        let loudness = i8::try_from(i16::from(verbose) - i16::from(quiet)).unwrap_or(i8::MIN);
        Self {
            loudness,
            style,
            bars,
            hint: Mutex::new(None),
        }
    }

    fn write(&self, text: &str) {
        match &self.bars {
            Some(bars) => bars.println(text),
            None => write_err(self.style, text),
        }
    }

    fn finish(&self, outcome: &Outcome, work: &Work) -> io::Result<()> {
        if let Some(bars) = &self.bars {
            bars.clear();
        }
        if self.loudness >= 0 && !outcome.sentence.is_empty() {
            let (mark, tone) = match outcome.status {
                Status::Ok => (self.style.check(), Tone::Done),
                Status::Partial | Status::Error => (self.style.cross(), Tone::Warn),
            };
            self.write(&format!(
                "{} {}\n",
                self.style.paint(tone, mark),
                outcome.sentence
            ));
        }
        let hint = self
            .hint
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(hint) = hint.filter(|_| self.loudness >= 0 && outcome.status == Status::Ok) {
            self.write(&self.style.verb("hint", Tone::Dim, &hint));
        }
        if self.loudness >= 1 {
            self.write(&summary(self.style, work));
        }
        if self.loudness >= -1
            && let Some(answer) = &outcome.answer
        {
            let mut out = io::stdout().lock();
            writeln!(out, "{answer}")?;
            out.flush()?;
        }
        Ok(())
    }

    fn failed(&self, failure: &Failure) {
        if self.loudness < 0 {
            return;
        }
        if let Some(bars) = &self.bars {
            bars.clear();
        }
        let style = self.style;
        let problem = &failure.0;
        let mut text = format!(
            "{} {}\n",
            style.paint(
                Tone::Error,
                &format!("{} error[{}]:", style.cross(), problem.kind)
            ),
            problem.message
        );
        if let Some(snippet) = &failure.1 {
            text.push_str(&show(style, snippet));
        }
        if let Some(help) = &problem.help {
            let _ = writeln!(text, "  {} {help}", style.paint(Tone::Done, "help:"));
        }
        self.write(&text);
    }
}

impl Reporter for Human {
    fn suspend(&self, work: &mut dyn FnMut()) {
        match &self.bars {
            Some(bars) => bars.suspend(work),
            None => work(),
        }
    }

    fn emit(&self, event: Event) {
        if self.loudness < 0 {
            return;
        }
        let bars = self.bars.as_ref();
        match &event {
            Event::Detail { label, text } => {
                if self.loudness >= 1 {
                    self.write(&self.style.verb(label, Tone::Dim, text));
                }
                return;
            }
            Event::Hint(text) => {
                self.hint
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .get_or_insert_with(|| text.clone());
                return;
            }
            Event::Waiting(text) => {
                if let Some(bars) = bars {
                    bars.wait(text);
                }
                return;
            }
            Event::TransferStarted { id, host, bytes } => {
                if let Some(bars) = bars {
                    bars.start(*id, host, *bytes);
                }
                return;
            }
            Event::TransferAdvanced { id, bytes } => {
                if let Some(bars) = bars {
                    bars.advance(*id, *bytes);
                }
                return;
            }
            Event::TransferFinished { id, .. } => {
                if let Some(bars) = bars {
                    bars.finish(*id);
                }
                return;
            }
            _ => {}
        }
        if let Some(bars) = bars {
            bars.settle();
        }
        if let Some(text) = line(self.style, &event) {
            self.write(&text);
        }
    }
}

fn write_err(style: Style, text: &str) {
    let choice = if style.color() {
        anstream::ColorChoice::Always
    } else {
        anstream::ColorChoice::Never
    };
    let mut err = anstream::AutoStream::new(io::stderr().lock(), choice);
    let _ignored = err.write_all(text.as_bytes());
}

/// The lines that show `snippet` under an error: the file and line, the line itself, and a
/// mark under the bytes at fault.
fn show(style: Style, snippet: &Snippet) -> String {
    let text = &snippet.text;
    let start = snippet.span.start.min(text.len());
    let line_start = text[..start].rfind('\n').map_or(0, |at| at + 1);
    let line_end = text[start..].find('\n').map_or(text.len(), |at| start + at);
    let end = snippet.span.end.clamp(start, line_end);
    let number = text[..start].matches('\n').count() + 1;
    let line = text[line_start..line_end].trim_end_matches('\r');
    let pad = " ".repeat(number.to_string().len());
    let column = text[line_start..start].chars().count();
    let width = text[start..end].chars().count().max(1);
    let gutter = style.paint(Tone::Dim, style.gutter());
    format!(
        "{pad} {} {}:{number}\n{} {gutter} {line}\n{pad} {gutter} {}{}\n",
        style.paint(Tone::Dim, style.corner()),
        snippet.file,
        style.paint(Tone::Dim, &number.to_string()),
        " ".repeat(column),
        style.paint(Tone::Error, &"^".repeat(width)),
    )
}

/// The line an event prints at the default loudness, if any.
fn line(style: Style, event: &Event) -> Option<String> {
    let done = |word: &str, rest: &str| style.verb(word, Tone::Done, rest);
    let dot = style.dot();
    Some(match event {
        Event::Initialized { path } => done("Created", path),
        Event::Added { dataset, reference } => {
            done("Added", &format!("{dataset} = {reference} to data.toml"))
        }
        Event::Resolved {
            resolved,
            title,
            files,
            bytes,
            license,
            ..
        } => {
            let mut text = resolved.to_string();
            if let Some(title) = title {
                let _ = write!(text, " {} \"{title}\"", style.arrow());
            }
            let _ = write!(text, " {dot} {}", count(*files, "file", "files"));
            if *bytes > 0 {
                let _ = write!(text, " {dot} {}", size(*bytes));
            }
            if let Some(license) = license {
                let _ = write!(text, " {dot} {license}");
            }
            done("Resolved", &text)
        }
        Event::Fetched {
            dataset,
            files,
            bytes,
            elapsed,
        } => done(
            "Downloaded",
            &format!(
                "{} of {dataset}, {} in {}",
                count(*files, "file", "files"),
                size(*bytes),
                duration(*elapsed)
            ),
        ),
        Event::Locked { files } => done(
            "Locked",
            &format!("{} in data.lock", count(*files, "file", "files")),
        ),
        Event::Linked {
            path,
            elapsed,
            extra_bytes,
            ..
        } => done(
            "Linked",
            &format!(
                "{path} in {}, {} extra disk",
                duration(*elapsed),
                size(*extra_bytes)
            ),
        ),
        Event::Unchanged { dataset } => done("Unchanged", dataset.as_str()),
        Event::Missing { path } => style.verb("Missing", Tone::Warn, path),
        Event::Modified { path } => style.verb("Modified", Tone::Warn, path),
        Event::Warning(text) => format!("{} {text}\n", style.paint(Tone::Warn, "warning:")),
        Event::Note { label, text } => done(label, text),
        Event::Failed(problem) => {
            let mut text = format!(
                "{} {}\n",
                style.paint(Tone::Error, &format!("error[{}]:", problem.kind)),
                problem.message
            );
            if let Some(help) = &problem.help {
                let _ = writeln!(text, "  {} {help}", style.paint(Tone::Done, "help:"));
            }
            text
        }
        Event::Detail { .. }
        | Event::Hint(_)
        | Event::Waiting(_)
        | Event::TransferStarted { .. }
        | Event::TransferAdvanced { .. }
        | Event::TransferFinished { .. } => return None,
    })
}

/// The `-v` summary of the work counters and the time per phase.
fn summary(style: Style, work: &Work) -> String {
    let mut parts = vec![
        count(work.requests, "request", "requests").to_string(),
        format!("{} down", size(work.bytes_down)),
        count(work.objects_ingested, "object ingested", "objects ingested").to_string(),
        format!("{} written", size(work.bytes_written)),
        count(work.files_linked, "file linked", "files linked").to_string(),
    ];
    let links = &work.links;
    for (mode, n) in [
        ("reflink", links.reflink),
        ("hardlink", links.hardlink),
        ("symlink", links.symlink),
        ("copy", links.copy),
    ] {
        if n > 0 {
            parts.push(format!("{n} by {mode}"));
        }
    }
    if work.bytes_copied > 0 {
        parts.push(format!("{} copied", size(work.bytes_copied)));
    }
    if work.files_rehashed > 0 {
        parts.push(count(work.files_rehashed, "file rehashed", "files rehashed").to_string());
    }
    for (tier, hits) in &work.tier_hits {
        parts.push(format!(
            "{} in the {tier} tier",
            count(*hits, "hit", "hits")
        ));
    }
    if work.retries > 0 {
        parts.push(count(work.retries, "retry", "retries").to_string());
    }
    if work.resumes > 0 {
        parts.push(count(work.resumes, "resume", "resumes").to_string());
    }
    let separator = format!(" {} ", style.dot());
    let mut text = style.verb("Work", Tone::Dim, &parts.join(&separator));
    if !work.phase_ms.is_empty() {
        let phases: Vec<String> = work
            .phase_ms
            .iter()
            .map(|(phase, ms)| format!("{phase} {ms}ms"))
            .collect();
        text.push_str(&style.verb("Time", Tone::Dim, &phases.join(&separator)));
    }
    text
}

/// Collects warnings and errors, and prints the document at the end.
#[derive(Default)]
pub(crate) struct Json {
    raised: Mutex<(Vec<String>, Vec<Problem>)>,
}

impl Json {
    fn finish(&self, command: &str, outcome: Outcome, work: &Work) -> io::Result<()> {
        let (warnings, errors) =
            std::mem::take(&mut *self.raised.lock().unwrap_or_else(PoisonError::into_inner));
        let mut document = Document::new(command, outcome.status, outcome.result);
        document.warnings = warnings;
        document.errors = errors;
        document.work = work.clone();
        let mut out = io::stdout().lock();
        serde_json::to_writer(&mut out, &document)?;
        writeln!(out)?;
        out.flush()
    }
}

impl Reporter for Json {
    fn emit(&self, event: Event) {
        let mut raised = self.raised.lock().unwrap_or_else(PoisonError::into_inner);
        match event {
            Event::Warning(text) => raised.0.push(text),
            Event::Failed(problem) => raised.1.push(problem),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use fl_model::name::Name;

    use super::*;

    #[test]
    fn a_resolved_dataset_names_title_files_size_and_license() {
        let event = Event::Resolved {
            dataset: Name::new("iris").unwrap(),
            resolved: "zenodo:3608990".parse().unwrap(),
            title: Some("DatasetIris".to_owned()),
            files: 1,
            bytes: 4177,
            license: Some("cc-by-4.0".to_owned()),
        };
        assert_eq!(
            line(Style::UNICODE, &event).unwrap(),
            "   Resolved zenodo:3608990 \u{2192} \"DatasetIris\" \u{b7} 1 file \u{b7} 4.1 KiB \u{b7} cc-by-4.0\n"
        );
        assert_eq!(
            line(Style::PLAIN, &event).unwrap(),
            "   Resolved zenodo:3608990 -> \"DatasetIris\" - 1 file - 4.1 KiB - cc-by-4.0\n"
        );
    }

    #[test]
    fn an_error_prints_its_kind_and_help() {
        let event = Event::Failed(Problem {
            kind: ErrorKind::ProjectMissing,
            message: "no data.toml".to_owned(),
            help: Some("run `fl init`".to_owned()),
            subject: None,
        });
        assert_eq!(
            line(Style::PLAIN, &event).unwrap(),
            "error[project.missing]: no data.toml\n  help: run `fl init`\n"
        );
    }

    #[test]
    fn waits_details_and_transfers_print_no_line() {
        let events = [
            Event::TransferFinished {
                id: 1,
                bytes: 5,
                elapsed: Duration::from_millis(3),
            },
            Event::Waiting("Resolving".to_owned()),
            Event::Detail {
                label: "Setting".to_owned(),
                text: "jobs = 8".to_owned(),
            },
        ];
        for event in events {
            assert_eq!(line(Style::PLAIN, &event), None);
        }
    }

    #[test]
    fn the_summary_lists_counters_links_and_phases() {
        let mut work = Work {
            requests: 2,
            bytes_down: 4177,
            files_linked: 1,
            ..Work::default()
        };
        work.links.hardlink = 1;
        work.phase_ms.insert("resolve".to_owned(), 12);
        assert_eq!(
            summary(Style::UNICODE, &work),
            concat!(
                "       Work 2 requests \u{b7} 4.1 KiB down \u{b7} 0 objects ingested \u{b7} 0 B written \u{b7} 1 file linked \u{b7} 1 by hardlink\n",
                "       Time resolve 12ms\n"
            )
        );
    }

    #[test]
    fn loudness_is_verbose_minus_quiet() {
        assert_eq!(Human::new(2, 0, Style::PLAIN, None).loudness, 2);
        assert_eq!(Human::new(0, 2, Style::PLAIN, None).loudness, -2);
    }

    #[test]
    fn a_snippet_points_at_the_bytes_at_fault_on_their_line() {
        let text =
            "[project]\nname = \"p\"\n\n[datasets.eeg]\nref = \"zenodo:1\"\nselct = [\"*.edf\"]\n";
        let at = text.find("selct").unwrap();
        let snippet = Snippet {
            file: "data.toml".to_owned(),
            text: text.to_owned(),
            span: at..at + 5,
        };
        assert_eq!(
            show(Style::UNICODE, &snippet),
            "  \u{250c}\u{2500} data.toml:6\n6 \u{2502} selct = [\"*.edf\"]\n  \u{2502} ^^^^^\n"
        );
        let wide = Snippet {
            text: "a = 1\r\nb = \u{e9}\u{e9}x\r\n".to_owned(),
            span: 15..16,
            ..snippet
        };
        assert_eq!(
            show(Style::PLAIN, &wide),
            "  -- data.toml:2\n2 | b = \u{e9}\u{e9}x\n  |       ^\n"
        );
    }
}
