//! The one event stream and the two reporters over it: the human story on stderr, and the JSON
//! document on stdout.

use std::fmt::Write as _;
use std::io::{self, Write};
use std::sync::{Mutex, PoisonError};

use fl_model::document::{Document, Status, Work};
use fl_model::event::{Event, Problem};
use fl_model::units::{count, duration, size};
use serde_json::Value;

/// Something that receives what happened during a command, in order.
pub(crate) trait Reporter: Send + Sync {
    /// Takes one event.
    fn emit(&self, event: Event);
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
}

/// The reporter of a run, chosen by `--json`.
pub(crate) enum Output {
    /// Lines on stderr as things happen, the answer on stdout.
    Human(Human),
    /// One document on stdout at the end.
    Json(Json),
}

impl Output {
    /// The reporter that events go to.
    pub(crate) fn reporter(&self) -> &dyn Reporter {
        match self {
            Self::Human(human) => human,
            Self::Json(json) => json,
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
    pub(crate) fn fail(&self, command: &str, problem: Problem, work: &Work) -> io::Result<()> {
        self.reporter().emit(Event::Failed(problem));
        let outcome = Outcome {
            status: Status::Error,
            result: Value::Object(serde_json::Map::new()),
            answer: None,
            sentence: String::new(),
        };
        self.finish(command, outcome, work)
    }
}

/// Writes the human story to stderr.
pub(crate) struct Human {
    loudness: i8,
    hinted: Mutex<bool>,
}

impl Human {
    /// A reporter at `verbose` minus `quiet`: below 0 nothing reaches stderr, from 1 the work is
    /// summed up at the end.
    pub(crate) fn new(verbose: u8, quiet: u8) -> Self {
        let loudness = i8::try_from(i16::from(verbose) - i16::from(quiet)).unwrap_or(i8::MIN);
        Self {
            loudness,
            hinted: Mutex::new(false),
        }
    }

    fn finish(&self, outcome: &Outcome, work: &Work) -> io::Result<()> {
        if self.loudness >= 0 && !outcome.sentence.is_empty() {
            write_err(&format!("{}\n", outcome.sentence));
        }
        if self.loudness >= 1 {
            write_err(&summary(work));
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
}

impl Reporter for Human {
    fn emit(&self, event: Event) {
        if self.loudness < 0 {
            return;
        }
        if let Event::Hint(_) = event {
            let mut hinted = self.hinted.lock().unwrap_or_else(PoisonError::into_inner);
            if *hinted {
                return;
            }
            *hinted = true;
        }
        if let Some(text) = line(&event) {
            write_err(&text);
        }
    }
}

fn write_err(text: &str) {
    let _ignored = io::stderr().lock().write_all(text.as_bytes());
}

fn verb(verb: &str, rest: &str) -> String {
    format!("{verb:>11} {rest}\n")
}

/// The line an event prints at the default loudness, if any.
fn line(event: &Event) -> Option<String> {
    Some(match event {
        Event::Initialized { path } => verb("Created", path),
        Event::Added { dataset, reference } => {
            verb("Added", &format!("{dataset} = {reference} to data.toml"))
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
                let _ = write!(text, " \u{2192} \"{title}\"");
            }
            let _ = write!(text, " \u{b7} {}", count(*files, "file", "files"));
            if *bytes > 0 {
                let _ = write!(text, " \u{b7} {}", size(*bytes));
            }
            if let Some(license) = license {
                let _ = write!(text, " \u{b7} {license}");
            }
            verb("Resolved", &text)
        }
        Event::Fetched {
            dataset,
            files,
            bytes,
            elapsed,
        } => verb(
            "Fetched",
            &format!(
                "{} of {dataset}, {} in {}",
                count(*files, "file", "files"),
                size(*bytes),
                duration(*elapsed)
            ),
        ),
        Event::Locked { files } => verb(
            "Locked",
            &format!("{} in data.lock", count(*files, "file", "files")),
        ),
        Event::Linked {
            path,
            elapsed,
            extra_bytes,
            ..
        } => verb(
            "Linked",
            &format!(
                "{path} in {}, {} extra disk",
                duration(*elapsed),
                size(*extra_bytes)
            ),
        ),
        Event::Unchanged { dataset } => verb("Unchanged", dataset.as_str()),
        Event::Missing { path } => verb("Missing", path),
        Event::Modified { path } => verb("Modified", path),
        Event::Warning(text) => format!("warning: {text}\n"),
        Event::Hint(text) => verb("hint", text),
        Event::Failed(problem) => {
            let mut text = format!("error[{}]: {}\n", problem.kind, problem.message);
            if let Some(help) = &problem.help {
                let _ = writeln!(text, "  help: {help}");
            }
            text
        }
        Event::TransferStarted { .. }
        | Event::TransferAdvanced { .. }
        | Event::TransferFinished { .. } => return None,
    })
}

/// The `-v` summary of the work counters and the time per phase.
fn summary(work: &Work) -> String {
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
    let mut text = verb("Work", &parts.join(" \u{b7} "));
    if !work.phase_ms.is_empty() {
        let phases: Vec<String> = work
            .phase_ms
            .iter()
            .map(|(phase, ms)| format!("{phase} {ms}ms"))
            .collect();
        text.push_str(&verb("Time", &phases.join(" \u{b7} ")));
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

    use fl_model::error::ErrorKind;
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
            line(&event).unwrap(),
            "   Resolved zenodo:3608990 \u{2192} \"DatasetIris\" \u{b7} 1 file \u{b7} 4.1 KiB \u{b7} cc-by-4.0\n"
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
            line(&event).unwrap(),
            "error[project.missing]: no data.toml\n  help: run `fl init`\n"
        );
    }

    #[test]
    fn transfer_progress_prints_nothing() {
        let event = Event::TransferFinished {
            id: 1,
            bytes: 5,
            elapsed: Duration::from_millis(3),
        };
        assert_eq!(line(&event), None);
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
            summary(&work),
            concat!(
                "       Work 2 requests \u{b7} 4.1 KiB down \u{b7} 0 objects ingested \u{b7} 0 B written \u{b7} 1 file linked \u{b7} 1 by hardlink\n",
                "       Time resolve 12ms\n"
            )
        );
    }

    #[test]
    fn loudness_is_verbose_minus_quiet() {
        assert_eq!(Human::new(2, 0).loudness, 2);
        assert_eq!(Human::new(0, 2).loudness, -2);
    }
}
