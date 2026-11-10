//! `fetchloom settings`: every setting a run would use, with its value and where it came from,
//! what fetchloom read about this machine, and where the settings file is. it changes nothing.

use std::fmt::Write as _;

use serde::Serialize;
use serde_json::{Value, json};

use super::Context;
use crate::net::Splitting;
use crate::settings::{Origin, Set};
use crate::units;

#[derive(Serialize)]
pub(super) struct Report {
    settings: Vec<Shown>,
    machine: Machine,
    file: File,
}

/// one setting as `--json` reports it.
#[derive(Serialize)]
struct Shown {
    name: &'static str,
    /// a number for counts, bytes and seconds, text for a folder or a mode, null for no limit.
    value: Value,
    origin: Origin,
    variable: &'static str,
    /// the value as a person reads it.
    #[serde(skip)]
    said: String,
}

#[derive(Serialize)]
struct Machine {
    processors: usize,
    /// bytes a run may use, none when the system does not say.
    memory: Option<u64>,
    /// files the process may hold open, none where the system sets no limit.
    open_files: Option<u64>,
}

#[derive(Serialize)]
struct File {
    path: Option<String>,
    found: bool,
}

pub(super) fn run(context: &Context) -> Report {
    let shown = rows(context);
    let machine = context.settings.machine;
    let output = &context.output;
    output.answer(&table(&shown));
    let mut about = vec![units::count(
        machine.processors as u64,
        "processor",
        "processors",
    )];
    if let Some(memory) = machine.memory {
        about.push(format!("{} of memory available", units::size(memory)));
    }
    if let Some(open) = machine.open_files {
        about.push(format!("{open} open files at most"));
    }
    output.step("Machine", &about.join(" \u{b7} "));
    let file = &context.settings.file;
    match &file.path {
        Some(path) if file.found => output.step("Settings", &path.display().to_string()),
        Some(path) => output.step("Settings", &format!("{} (no file yet)", path.display())),
        None => output.step("Settings", "no folder for a settings file"),
    }
    output.hint("a FETCHLOOM_* variable changes a setting where it is set; the settings file keeps it for every run");
    Report {
        settings: shown,
        machine: Machine {
            processors: machine.processors,
            memory: machine.memory,
            open_files: machine.open_files,
        },
        file: File {
            path: file.path.as_ref().map(|path| path.display().to_string()),
            found: file.found,
        },
    }
}

/// every setting the run uses, in the order the table shows them.
fn rows(context: &Context) -> Vec<Shown> {
    let settings = &context.settings;
    let var = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty());
    let from_env = |name: &str| {
        if var(name).is_some() {
            Origin::Environment
        } else {
            Origin::Default
        }
    };
    let mut shown = Vec::new();
    let cache = super::cache_folder();
    shown.push(Shown {
        name: "cache",
        value: cache
            .as_ref()
            .map_or(Value::Null, |path| json!(path.display().to_string())),
        origin: if var("FETCHLOOM_CACHE").is_some() {
            Origin::Environment
        } else if settings.file.cache().is_some() {
            Origin::File
        } else {
            Origin::Default
        },
        variable: "FETCHLOOM_CACHE",
        said: cache.map_or_else(
            || "none: set FETCHLOOM_CACHE".to_owned(),
            |path| path.display().to_string(),
        ),
    });
    shown.push(count("jobs", settings.jobs.value, settings.jobs.origin));
    shown.push(count(
        "host_jobs",
        settings.host_jobs.value,
        settings.host_jobs.origin,
    ));
    shown.push(count(
        "retries",
        settings.retries.value,
        settings.retries.origin,
    ));
    shown.push(Shown {
        name: "timeout",
        value: json!(settings.timeout.value.as_secs()),
        origin: settings.timeout.origin,
        variable: "FETCHLOOM_TIMEOUT",
        said: format!("{} s", settings.timeout.value.as_secs()),
    });
    shown.push(size("ask_above", settings.ask_above));
    shown.push(size("memory", settings.memory));
    shown.push(count(
        "most_files",
        settings.most_files.value,
        settings.most_files.origin,
    ));
    let threads = context.threads.limit(settings.machine.processors.max(1));
    shown.push(Shown {
        name: "threads",
        value: json!(threads),
        origin: if context.threads.is_limited() {
            Origin::Environment
        } else {
            Origin::Machine
        },
        variable: "FETCHLOOM_THREADS",
        said: threads.to_string(),
    });
    shown.push(Shown {
        name: "rate",
        value: context
            .rate
            .as_ref()
            .map_or(Value::Null, |rate| json!(rate.per_second())),
        origin: from_env("FETCHLOOM_RATE"),
        variable: "FETCHLOOM_RATE",
        said: context.rate.as_ref().map_or_else(
            || "no limit".to_owned(),
            |rate| format!("{}/s", units::size(rate.per_second())),
        ),
    });
    let (mode, said) = match context.split {
        Splitting::Never => ("never", "never"),
        Splitting::WhenSlow => ("when_slow", "when one stream is slow"),
        Splitting::Always => ("always", "always"),
    };
    shown.push(Shown {
        name: "split",
        value: json!(mode),
        origin: from_env("FETCHLOOM_SPLIT"),
        variable: "FETCHLOOM_SPLIT",
        said: said.to_owned(),
    });
    shown
}

fn count(name: &'static str, value: impl Into<Value> + ToString + Copy, origin: Origin) -> Shown {
    Shown {
        name,
        value: value.into(),
        origin,
        variable: crate::settings::variable(name),
        said: value.to_string(),
    }
}

fn size(name: &'static str, set: Set<u64>) -> Shown {
    Shown {
        name,
        value: json!(set.value),
        origin: set.origin,
        variable: crate::settings::variable(name),
        said: units::size(set.value),
    }
}

/// the settings in aligned columns: name, value, where it came from, and the variable that sets
/// it.
fn table(shown: &[Shown]) -> String {
    let rows: Vec<[String; 4]> = shown
        .iter()
        .map(|setting| {
            [
                setting.name.to_owned(),
                setting.said.clone(),
                setting.origin.to_string(),
                setting.variable.to_owned(),
            ]
        })
        .collect();
    let widths: Vec<usize> = (0..3)
        .map(|column| {
            rows.iter()
                .map(|row| row[column].chars().count())
                .max()
                .unwrap_or_default()
        })
        .collect();
    rows.iter()
        .map(|row| {
            let mut line = String::new();
            for (cell, width) in row.iter().zip(&widths) {
                let _ = write!(line, "{cell:<width$}  ");
            }
            line.push_str(&row[3]);
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}
