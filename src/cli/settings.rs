//! `fetchloom settings`: every setting a run would use, with its value and where it came from,
//! what fetchloom read about this machine, and where the settings file is. it changes nothing.

use std::fmt::Write as _;

use serde::Serialize;
use serde_json::{Value, json};

use super::Context;
use crate::mirror::Mirrors;
use crate::net::Splitting;
use crate::output::{Output, Warning};
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
    /// where the value came from: one origin, or for mirrors `--mirror` names before the
    /// setting's, the command line and then the setting's origin.
    #[serde(serialize_with = "named")]
    origin: Vec<Origin>,
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
    bypassed: bool,
}

pub(super) fn run(context: &Context) -> Report {
    let shown = rows(context);
    let machine = context.settings.machine;
    let output = &context.output;
    output.answer(&table(&shown, output.answer_width()));
    let mut about = vec![units::count(
        machine.processors as u64,
        "processor",
        "processors",
    )];
    if let Some(memory) = machine.memory {
        about.push(format!("{} of memory available", units::size(memory)));
    }
    if let Some(open) = machine.open_files {
        about.push(format!("{} open files at most", units::grouped(open)));
    }
    output.step("Machine", &about.join(" \u{b7} "));
    let file = &context.settings.file;
    match &file.path {
        Some(path) if let Some(by) = file.bypassed => output.step(
            "Settings",
            &format!("{} (bypassed by {by})", units::path(path)),
        ),
        Some(path) if file.found => output.step("Settings", &units::path(path)),
        Some(path) => output.step("Settings", &format!("{} (no file yet)", units::path(path))),
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
            bypassed: file.bypassed.is_some(),
        },
    }
}

/// tells, at `-v`, what a run spends as it starts: downloads, workers, the memory budget and hash
/// lanes, each with what holds it there when that is not fetchloom's own default.
pub(super) fn tell_spending(context: &Context) {
    let output = &context.output;
    if !output.shows_details() {
        return;
    }
    let spending = context
        .settings
        .spending(context.threads, crate::machine::processors_online());
    output.step(
        "Resources",
        &format!(
            "{}: {} at once, {} to one host, {}, {} for buffers, {}",
            spending.level,
            units::count(spending.jobs as u64, "download", "downloads"),
            spending.host_jobs,
            units::count(spending.workers as u64, "local worker", "local workers"),
            units::size(spending.memory),
            units::count(spending.lanes as u64, "hash lane", "hash lanes"),
        ),
    );
    for reason in &spending.reasons {
        output.detail(reason);
    }
}

/// warns about each of `names` that is one typo from a variable fetchloom reads, naming that
/// variable and never the value, which may be a secret.
pub(super) fn warn_misnamed(output: &Output, names: impl IntoIterator<Item = std::ffi::OsString>) {
    for (name, meant) in crate::settings::misnamed(names) {
        output.warn_detailed(
            Warning::SettingsUnknownVariable,
            &format!("{name} is set, but fetchloom reads no such variable, so it changes nothing"),
            Some(&format!("did you mean {meant}?")),
            [&name],
        );
    }
}

/// the variables that change only what a person sees, each with the help for a value that is
/// neither true nor false, which leaves it off with a warning rather than stopping the run.
const DISPLAY_VARIABLES: [(&str, &str); 2] = [
    (
        "FETCHLOOM_ACCESSIBLE",
        "set it to true for plain lines a screen reader reads well, or unset it",
    ),
    (
        "FETCHLOOM_NO_HINTS",
        "set it to true to drop the hint: and next: lines, or unset it",
    ),
];

/// warns about each of [`DISPLAY_VARIABLES`] that holds neither true nor false.
pub(super) fn warn_unread(output: &Output, var: crate::settings::Var<'_>) {
    for (name, help) in DISPLAY_VARIABLES {
        if let Err(error) = crate::settings::switched(name, var(name).as_deref()) {
            output.warn_with(
                Warning::SettingsSwitchUnread,
                &format!("{}, so it stays off", error.message),
                help,
            );
        }
    }
}

/// every setting the run uses, in the order the table shows them.
fn rows(context: &Context) -> Vec<Shown> {
    let settings = &context.settings;
    let var = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty());
    let mut shown = Vec::new();
    let cache = super::cache_folder();
    let build_floor = floor(settings, cache.as_deref());
    shown.push(level(settings.resources));
    shown.push(Shown {
        name: "cache",
        value: cache
            .as_ref()
            .map_or(Value::Null, |path| json!(path.display().to_string())),
        origin: vec![if var("FETCHLOOM_CACHE").is_some() {
            Origin::Environment
        } else if settings.file.cache().is_some() {
            Origin::File
        } else {
            Origin::Default
        }],
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
        origin: vec![settings.timeout.origin],
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
    shown.push(count(
        "archive_files",
        settings.archive_files.value,
        settings.archive_files.origin,
    ));
    shown.push(size("archive_size", settings.archive_size));
    shown.push(count(
        "archive_ratio",
        settings.archive_ratio.value,
        settings.archive_ratio.origin,
    ));
    shown.push(list(
        "allowed_origins",
        settings
            .allowed_origins
            .value
            .iter()
            .map(url::Origin::ascii_serialization),
        vec![settings.allowed_origins.origin],
    ));
    shown.push(targets("s3_buckets", &settings.s3_buckets));
    shown.push(targets("gcs_buckets", &settings.gcs_buckets));
    shown.push(targets("azure_containers", &settings.azure_containers));
    shown.push(targets("drive_folders", &settings.drive_folders));
    shown.push(mirrors(&context.mirrors.mirrors, settings.mirrors.origin));
    shown.push(Shown {
        name: "mirrors_only",
        value: json!(settings.mirrors_only.value),
        origin: vec![settings.mirrors_only.origin],
        variable: crate::settings::variable("mirrors_only"),
        said: settings.mirrors_only.value.to_string(),
    });
    let push_to = settings.push_to.value.as_ref();
    shown.push(Shown {
        name: "push_to",
        value: push_to.map_or(Value::Null, |folder| json!(folder.display().to_string())),
        origin: vec![settings.push_to.origin],
        variable: crate::settings::variable("push_to"),
        said: push_to.map_or_else(|| "none".to_owned(), |folder| folder.display().to_string()),
    });
    shown.push(build_floor);
    shown.extend(variables(context));
    shown
}

/// `build_floor`: as set, else the larger of 1 GiB and 1% of the disk holding `cache`, sized from
/// this machine when that disk's share is the larger.
fn floor(settings: &crate::settings::Settings, cache: Option<&std::path::Path>) -> Shown {
    let set = settings.build_floor;
    let total = cache
        .filter(|cache| cache.exists())
        .and_then(crate::space::total);
    let value = crate::space::floor(set.value, total);
    let origin = match set.value {
        Some(_) => set.origin,
        None if value > crate::space::floor(None, None) => Origin::Machine,
        None => Origin::Default,
    };
    Shown {
        name: "build_floor",
        value: json!(value),
        origin: vec![origin],
        variable: crate::settings::variable("build_floor"),
        said: units::size(value),
    }
}

/// the settings only a variable sets: threads, rate, split and accessible.
fn variables(context: &Context) -> Vec<Shown> {
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
    let threads = context.threads.limit(settings.machine.processors.max(1));
    shown.push(Shown {
        name: "threads",
        value: json!(threads),
        origin: vec![match settings.threads {
            Some(level) => level.origin,
            None if context.threads.is_limited() => Origin::Environment,
            None => Origin::Machine,
        }],
        variable: "FETCHLOOM_THREADS",
        said: threads.to_string(),
    });
    shown.push(Shown {
        name: "rate",
        value: context
            .rate
            .as_ref()
            .map_or(Value::Null, |rate| json!(rate.per_second())),
        origin: vec![from_env("FETCHLOOM_RATE")],
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
        origin: vec![from_env("FETCHLOOM_SPLIT")],
        variable: "FETCHLOOM_SPLIT",
        said: said.to_owned(),
    });
    let accessible = crate::settings::is_on(&var, "FETCHLOOM_ACCESSIBLE");
    shown.push(Shown {
        name: "accessible",
        value: json!(accessible),
        origin: vec![from_env("FETCHLOOM_ACCESSIBLE")],
        variable: "FETCHLOOM_ACCESSIBLE",
        said: accessible.to_string(),
    });
    shown
}

fn count<N>(name: &'static str, value: N, origin: Origin) -> Shown
where
    N: Into<Value> + TryInto<u64> + Copy,
{
    Shown {
        name,
        value: value.into(),
        origin: vec![origin],
        variable: crate::settings::variable(name),
        said: value.try_into().map(units::grouped).unwrap_or_default(),
    }
}

/// a list setting of what a credential may go to, each item as its text.
fn targets<T: ToString>(name: &'static str, set: &Set<Vec<T>>) -> Shown {
    list(
        name,
        set.value.iter().map(ToString::to_string),
        vec![set.origin],
    )
}

/// a setting that holds a list, shown as its items separated by commas, or none.
fn list(name: &'static str, items: impl Iterator<Item = String>, origin: Vec<Origin>) -> Shown {
    let items: Vec<String> = items.collect();
    Shown {
        name,
        said: if items.is_empty() {
            "none".to_owned()
        } else {
            items.join(", ")
        },
        value: json!(items),
        origin,
        variable: crate::settings::variable(name),
    }
}

/// the mirrors a run asks, in order: those `--mirror` names, then those of the setting it did not
/// name, from `setting`.
fn mirrors(asked: &Mirrors, setting: Origin) -> Shown {
    let mut origin = Vec::new();
    if asked.flagged > 0 {
        origin.push(Origin::Flag);
    }
    if asked.flagged == 0 || asked.list.len() > asked.flagged {
        origin.push(setting);
    }
    list(
        "mirrors",
        asked.list.iter().map(ToString::to_string),
        origin,
    )
}

/// origins as `--json` names them, separated by commas.
fn named<S: serde::Serializer>(origins: &[Origin], serializer: S) -> Result<S::Ok, S::Error> {
    let names: Vec<&str> = origins.iter().map(|origin| origin.name()).collect();
    serializer.serialize_str(&names.join(","))
}

fn size(name: &'static str, set: Set<u64>) -> Shown {
    Shown {
        name,
        value: json!(set.value),
        origin: vec![set.origin],
        variable: crate::settings::variable(name),
        said: units::size(set.value),
    }
}

fn level(set: Set<crate::settings::Level>) -> Shown {
    Shown {
        name: "resources",
        value: json!(set.value.name()),
        origin: vec![set.origin],
        variable: crate::settings::variable("resources"),
        said: set.value.name().to_owned(),
    }
}

/// the widest a value may make its column. a longer one, such as a deep cache folder, runs past
/// it, its origin and variable on a line of their own, so the other rows keep their columns.
const VALUE: usize = 24;

/// the settings as aligned columns, one per line, for a person or for `grep`. a terminal `width`
/// shows a home path from `~`, and too narrow leaves out the variable, `FETCHLOOM_` and the name.
fn table(shown: &[Shown], width: Option<usize>) -> String {
    let rows: Vec<[String; 4]> = shown
        .iter()
        .map(|setting| {
            let said = match (&setting.value, width) {
                (Value::String(path), Some(_)) if setting.name == "cache" => {
                    units::path(std::path::Path::new(path))
                }
                _ => setting.said.clone(),
            };
            [
                setting.name.to_owned(),
                said,
                setting
                    .origin
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", "),
                setting.variable.to_owned(),
            ]
        })
        .collect();
    let widest = |column: usize, most: usize| {
        rows.iter()
            .map(|row| row[column].chars().count())
            .filter(|count| *count <= most)
            .max()
            .unwrap_or_default()
    };
    let widths = [
        widest(0, usize::MAX),
        widest(1, VALUE),
        widest(2, usize::MAX),
    ];
    let full = widths.iter().map(|width| width + 2).sum::<usize>() + widest(3, usize::MAX);
    let variables = width.is_none_or(|width| full <= width);
    rows.iter()
        .map(|row| {
            let mut line = format!("{:<name$}  ", row[0], name = widths[0]);
            let value = row[1].chars().count();
            if value > widths[1] {
                // the origin and variable go under their columns, on a line of their own
                let _ = write!(
                    line,
                    "{}\n{:<pad$}",
                    row[1],
                    "",
                    pad = widths[0] + widths[1] + 4
                );
            } else {
                let _ = write!(line, "{:<width$}  ", row[1], width = widths[1]);
            }
            if variables {
                let _ = write!(line, "{:<width$}  {}", row[2], row[3], width = widths[2]);
            } else {
                line.push_str(&row[2]);
            }
            line.trim_end().to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::json;

    use super::*;
    use crate::cli::fetch::mirrors::Mirrored;
    use crate::mirror::Location;
    use crate::output::{Level, Style};

    fn folder(path: &str) -> Location {
        Location::Folder(PathBuf::from(path))
    }

    /// and a terminal too narrow for every column leaves out the variable.
    #[test]
    fn a_long_value_keeps_the_other_rows_in_their_columns() {
        let row = |name: &'static str, said: &str, variable: &'static str| Shown {
            name,
            value: json!(said),
            origin: vec![Origin::Default],
            variable,
            said: said.to_owned(),
        };
        let cache = "/scratch/a-very-long-group-name/someone/fetchloom-cache";
        let shown = [
            row("cache", cache, "FETCHLOOM_CACHE"),
            row("jobs", "8", "FETCHLOOM_JOBS"),
        ];
        assert_eq!(
            table(&shown, None),
            format!(
                "cache  {cache}\n          default  FETCHLOOM_CACHE\njobs   8  default  FETCHLOOM_JOBS"
            )
        );
        assert_eq!(
            table(&shown, Some(30)),
            format!("cache  {cache}\n          default\njobs   8  default")
        );
    }

    /// while `--json` keeps the number.
    #[test]
    fn counts_read_with_their_thousands_apart() {
        let files = count("archive_files", 10_000_000_u64, Origin::Default);
        assert_eq!(files.said, "10,000,000");
        assert_eq!(files.value, json!(10_000_000));
        assert_eq!(count("jobs", 8_usize, Origin::Default).said, "8");
    }

    /// a run given `flagged` with `--mirror`, whose mirrors setting lists `listed` from `origin`.
    fn context(flagged: Vec<Location>, listed: Vec<Location>, origin: Origin) -> Context {
        let mut context = Context::new(Output::new(
            Style {
                color: false,
                unicode: false,
                interactive: false,
                watched: false,
                links: false,
                accessible: false,
            },
            Level::Quiet,
            false,
            false,
        ));
        context.mirrors = Mirrored::new(Mirrors::new(flagged, &listed, false));
        context.settings.mirrors = Set {
            value: listed,
            origin,
        };
        context
    }

    /// those `--mirror` names first, then those of the setting it did not name, each once.
    #[test]
    fn the_mirrors_row_shows_what_a_run_asks_and_where_each_part_came_from() {
        for (flagged, listed, origin, value, json, said) in [
            (
                vec![],
                vec![folder("/mnt/lab")],
                Origin::File,
                json!(["/mnt/lab"]),
                "file",
                "settings file",
            ),
            (
                vec![folder("/usb")],
                vec![],
                Origin::Default,
                json!(["/usb"]),
                "flag",
                "command line",
            ),
            (
                vec![folder("/usb")],
                vec![folder("/mnt/lab")],
                Origin::Environment,
                json!(["/usb", "/mnt/lab"]),
                "flag,environment",
                "command line, environment",
            ),
            (
                vec![folder("/mnt/lab")],
                vec![folder("/mnt/lab"), folder("/usb")],
                Origin::File,
                json!(["/mnt/lab", "/usb"]),
                "flag,file",
                "command line, settings file",
            ),
            (
                vec![folder("/usb")],
                vec![folder("/usb")],
                Origin::File,
                json!(["/usb"]),
                "flag",
                "command line",
            ),
        ] {
            let shown = rows(&context(flagged, listed, origin));
            let row = shown.iter().find(|row| row.name == "mirrors").unwrap();
            let document = serde_json::to_value(row).unwrap();
            assert_eq!(document["value"], value, "{said}");
            assert_eq!(document["origin"], json, "{said}");
            let line = table(std::slice::from_ref(row), None);
            let items: Vec<&str> = value
                .as_array()
                .unwrap()
                .iter()
                .filter_map(serde_json::Value::as_str)
                .collect();
            assert_eq!(
                line,
                format!("mirrors  {}  {said}  FETCHLOOM_MIRRORS", items.join(", "))
            );
        }
    }
}
