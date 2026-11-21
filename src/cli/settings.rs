//! `fetchloom settings`: every setting a run would use, with its value and where it came from,
//! what fetchloom read about this machine, and where the settings file is. it changes nothing;
//! `settings set` and `settings unset` change one key of the settings file.

use std::fmt::Write as _;

use serde::Serialize;
use serde_json::{Value, json};

use super::{Context, args};
use crate::error::{Error, Kind};
use crate::mirror::Mirrors;
use crate::net::Splitting;
use crate::output::{Output, Warning};
use crate::settings::edit::Stops;
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
    /// whether only its variable sets it, never the settings file.
    #[serde(skip)]
    only_variable: bool,
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
    /// the usual settings file, there but unread, since `FETCHLOOM_SETTINGS` names another.
    passed_over: Option<String>,
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
    output.label("Machine", &about.join(" \u{b7} "));
    let file = &context.settings.file;
    match &file.path {
        Some(path) if let Some(by) = file.bypassed => output.label(
            "Settings",
            &format!("{} (bypassed by {by})", units::path(path)),
        ),
        Some(path) if let Some(usual) = &file.passed_over => output.label(
            "Settings",
            &format!(
                "{}, which FETCHLOOM_SETTINGS names, so {} is not read",
                units::path(path),
                units::path(usual)
            ),
        ),
        Some(path) if file.found => output.label("Settings", &units::path(path)),
        Some(path) => output.label("Settings", &format!("{} (no file yet)", units::path(path))),
        None => output.label("Settings", "no folder for a settings file"),
    }
    output.hint("`fetchloom settings set <key> <value>` saves a setting for every run, and `fetchloom settings unset <key>` takes it out; a FETCHLOOM_* variable wins where it is set");
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
            passed_over: file
                .passed_over
                .as_ref()
                .map(|path| path.display().to_string()),
        },
    }
}

/// what `settings set` or `settings unset` changed, as `--json` reports it.
#[derive(Serialize)]
pub(super) struct Edited {
    key: String,
    /// the settings file.
    file: String,
    /// the key's value in the settings file before and after, null where it held none, each text
    /// masked where it may carry a login or a key.
    before: Value,
    after: Value,
    /// whether the settings file changed, which it does not when it held the value already.
    changed: bool,
    /// whether the change made the settings file.
    created: bool,
    /// the variable that gives the key its value in this run, over the settings file.
    overridden_by: Option<&'static str>,
}

/// `settings set` or `settings unset`, which `--no-config`, given as `no_config`, or
/// `FETCHLOOM_NO_CONFIG` refuses, since the settings file is to be left alone then. it reads the
/// settings file itself, so a file with a mistake in it can be fixed this way.
pub(super) fn change(
    context: &Context,
    action: &args::SettingsAction,
    no_config: bool,
    var: crate::machine::Var<'_>,
) -> Result<Edited, Error> {
    let (command, key) = match action {
        args::SettingsAction::Set(set) => ("settings set", &set.key),
        args::SettingsAction::Unset(unset) => ("settings unset", &unset.key),
    };
    if let Some(by) = super::no_config(no_config, var("FETCHLOOM_NO_CONFIG").as_deref())? {
        let help = if by == "--no-config" {
            "run it again without --no-config".to_owned()
        } else {
            format!("unset {by}, then run it again")
        };
        return Err(Error::new(
            Kind::Usage,
            format!("`fetchloom {command}` changes the settings file, which {by} leaves alone"),
        )
        .help(help));
    }
    let changed = match action {
        args::SettingsAction::Set(set) => {
            let here = std::env::current_dir().ok();
            let sets_it = set.key == "mirrors_only";
            let usable = |settings: &crate::settings::Settings| usable(settings, sets_it);
            crate::settings::edit::set(var, key, &set.values, here.as_deref(), &usable)?
        }
        args::SettingsAction::Unset(_) => {
            crate::settings::edit::unset(var, key, &|settings| usable(settings, false))?
        }
    };
    let overridden_by = crate::settings::edit::overridden(var, &changed.key);
    tell_changed(&context.output, &changed, overridden_by);
    if changed.written {
        for folder in &changed.written_in {
            tell_in_git(&context.output, &changed.key, folder);
        }
    }
    if changed.key == "cache" && changed.written && overridden_by.is_none() {
        tell_old_cache(context, &changed);
    }
    Ok(Edited {
        file: changed.path.display().to_string(),
        before: changed.before.as_ref().map_or(Value::Null, json_of),
        after: changed.after.as_ref().map_or(Value::Null, json_of),
        changed: changed.written,
        created: changed.created,
        key: changed.key,
        overridden_by,
    })
}

/// what every run checks of its settings once each is read: that the cache folder the settings
/// file names is one a run can open, which stops the runs that open the cache, and that
/// `mirrors_only` has a mirror to ask, where no `--mirror` gives one, which stops every run. a
/// change that `sets_it` is told to keep a mirror first.
fn usable(settings: &crate::settings::Settings, sets_it: bool) -> Vec<Stops> {
    let mut stops = Vec::new();
    if let Some(folder) = settings.file.cache()
        && let Some(why) = crate::cache::refused(&folder)
    {
        let error = Error::new(
            Kind::Usage,
            format!("cache cannot take the folder {}: {why}", folder.display()),
        );
        let error = if folder.is_dir() && crate::error::file_in_the_way(&folder).is_none() {
            let inside = folder.join("fetchloom-cache");
            error.help(format!(
                "choose an empty folder or a new one, as in `fetchloom settings set cache {}`",
                crate::suggested::shell_folder(&inside.to_string_lossy())
            ))
        } else {
            error.help(
                "choose an empty folder or a new one with `fetchloom settings set cache <folder>`",
            )
        };
        stops.push(Stops {
            error,
            runs: "every run that opens the cache",
        });
    }
    let help = if sets_it {
        "keep a mirror in `mirrors` first, as `fetchloom settings set mirrors /mnt/lab/fetchloom` saves one"
    } else {
        "keep a mirror in `mirrors`, as `fetchloom settings set mirrors /mnt/lab/fetchloom` saves one, or run `fetchloom settings unset mirrors_only` first"
    };
    if let Err(error) = super::fetch::mirrors::read(&[], settings, None) {
        stops.push(Stops {
            error: error.help(help),
            runs: "every run",
        });
    }
    stops
}

/// warns when `folder`, which the setting `key` names for every project on this machine, or a
/// push wrote to for `key` "push", is in a git work tree that would take what fetchloom writes
/// there, as a relative folder given inside a project lands.
pub(super) fn tell_in_git(output: &Output, key: &str, folder: &std::path::Path) {
    let Some(top) = crate::project::git::taking(folder) else {
        return;
    };
    let (what, help) = if key == "cache" {
        (
            format!(
                "the cache folder {} is in the git repository at {}, so `git add .` there takes every file the cache holds, for every project on this machine",
                units::path(folder),
                units::path(&top)
            ),
            "choose a folder outside it with `fetchloom settings set cache <folder>`, or run `fetchloom settings unset cache` for the usual one".to_owned(),
        )
    } else {
        (
            format!(
                "the mirror folder {} is in the git repository at {}, so `git add .` there takes every file pushed to it",
                units::path(folder),
                units::path(&top)
            ),
            if key == "push" {
                format!(
                    "push to a folder outside any repository, on a disk your teammates reach, as in `{}`",
                    crate::suggested::command("push --to /mnt/lab/fetchloom")
                )
            } else {
                format!(
                    "keep a mirror outside any repository, on a disk your teammates reach, as in `fetchloom settings set {key} /mnt/lab/fetchloom`"
                )
            },
        )
    };
    output.warn_with(Warning::GitFolderInTree, &what, &help);
}

/// says what `changed` did: the key's value before and after, and in which file, then, when a
/// variable gives the key its value in this run, that the variable wins.
fn tell_changed(
    output: &Output,
    changed: &crate::settings::edit::Changed,
    overridden_by: Option<&str>,
) {
    let key = &changed.key;
    let file = units::path(&changed.path);
    let said = |value: Option<&toml_edit::Value>| value.map_or_else(|| "not set".to_owned(), shown);
    for folder in &changed.missing {
        let shown = units::path(folder);
        if key == "push_to" {
            output.warn_with(
                Warning::SettingsMissingFolder,
                &format!("the folder {shown} does not exist yet, so the first push makes it"),
                "check its name before you push",
            );
        } else if key == "reuse" {
            output.warn_with(
                Warning::SettingsMissingFolder,
                &format!(
                    "the folder {shown} does not exist, so runs read nothing there until it does"
                ),
                "check its name, or mount the disk that holds it",
            );
        } else if key == "data_links" {
            output.warn_with(
                Warning::SettingsMissingFolder,
                &format!(
                    "the folder {shown} does not exist, so no data folder links into it until it does"
                ),
                "check its name, or mount the disk that holds it",
            );
        } else {
            output.warn_with(
                Warning::SettingsMissingFolder,
                &format!("the mirror folder {shown} does not exist, so runs skip it until it does"),
                &format!(
                    "check its name, or fill it with `fetchloom push --to {}`",
                    crate::suggested::shell_word(&folder.to_string_lossy())
                ),
            );
        }
    }
    if let Some(broken) = &changed.broken {
        let said = format!(
            "{}, so {} still stops there",
            broken.error.message, broken.runs
        );
        match &broken.error.help {
            Some(help) => output.warn_with(Warning::SettingsBroken, &said, help),
            None => output.warn(Warning::SettingsBroken, &said),
        }
    }
    if let Some(variable) = overridden_by {
        let help = if changed.after.is_some() {
            format!("unset {variable} where the saved value should hold")
        } else {
            format!("unset {variable} too where the default should hold")
        };
        output.warn_with(
            Warning::SettingsOverridden,
            &format!(
                "{variable} is set here, and a variable wins over the settings file, so runs where it is set keep its value for {key}"
            ),
            &help,
        );
    }
    match (changed.written, &changed.after) {
        (true, after) => output.done(
            if after.is_some() { "Saved" } else { "Removed" },
            &format!(
                "{key} \u{b7} {} {} {} \u{b7} {file}",
                said(changed.before.as_ref()),
                output.arrow(),
                said(after.as_ref())
            ),
        ),
        (false, Some(after)) => output.done(
            "Ok",
            &format!(
                "{file} holds {key} = {} already, so nothing changed",
                shown(after)
            ),
        ),
        (false, None) => output.done(
            "Ok",
            &format!("{file} does not set {key}, so nothing changed"),
        ),
    }
}

/// after the cache moved, that the one before keeps what it cached, since fetchloom moves no
/// cache, and what deleting it frees or loses: data/ holds its own links or copies, a link there
/// keeps its bytes, and no source gives back bytes imported from files. a folder that is no cache,
/// as a person's own a setting named by mistake, or the cache in use named another way, is not
/// told.
fn tell_old_cache(context: &Context, changed: &crate::settings::edit::Changed) {
    // a relative folder is taken from where runs find the settings file, as runs take it
    let folder = |value: Option<&toml_edit::Value>| {
        value
            .and_then(toml_edit::Value::as_str)
            .map(|folder| {
                let folder = std::path::PathBuf::from(folder);
                match changed.found_at.parent() {
                    Some(parent) if folder.is_relative() => parent.join(folder),
                    _ => folder,
                }
            })
            .or_else(|| crate::cache::Cache::locate(&|name| std::env::var_os(name), None))
    };
    let Some(before) =
        folder(changed.before.as_ref()).filter(|before| crate::cache::is_cache(before))
    else {
        return;
    };
    let after = folder(changed.after.as_ref());
    // a link to it, a path through `..` or other letter case names the cache runs use now
    if after.as_deref().is_some_and(|after| {
        after == before || same_file::is_same_file(after, &before).unwrap_or(false)
    }) {
        return;
    }
    let cache = crate::cache::Cache::unopened(before.clone()).ok();
    let imported = cache
        .as_ref()
        .map(crate::cache::Cache::imported)
        .unwrap_or_default();
    let held = cache
        .and_then(|cache| cache.scan_objects(context.threads, &context.stop).ok())
        .filter(|scan| scan.unreadable.is_empty())
        .map(|scan| {
            scan.objects
                .iter()
                .fold(Held::default(), |mut held, object| {
                    held.bytes += object.size;
                    if object.has_other_names() {
                        held.linked += object.size;
                    } else if imported.contains_key(&object.digest) {
                        held.imported += object.size;
                    }
                    held
                })
        });
    // a folder no run has made yet can take the old cache whole
    let moved_to = after
        .filter(|after| std::fs::symlink_metadata(after).is_err())
        .map(|after| units::path(&after));
    let told = Told {
        held,
        imports: !imported.is_empty(),
        moved_to: moved_to.as_deref(),
    };
    if let Some(hint) = old_cache(&units::path(&before), &told) {
        context.output.hint(&hint);
    }
}

/// what an old cache's files hold, in bytes: all of them, those a name outside the cache shares,
/// as a project's hardlink does, and those imported from files that no other name holds.
#[derive(Clone, Copy, Debug, Default)]
struct Held {
    bytes: u64,
    linked: u64,
    imported: u64,
}

/// what [`old_cache`] tells of a cache a move left: what it holds, when its files could be
/// counted, whether it holds bytes imported from files at all, and the folder it could be moved
/// to whole, where none is yet.
struct Told<'a> {
    held: Option<Held>,
    imports: bool,
    moved_to: Option<&'a str>,
}

/// what the cache `shown` a move left keeps and what deleting it frees, from `told`. a clone in
/// data/ shares blocks no name counts, so it frees up to that; bytes imported from files that no
/// name outside the cache holds would be lost, since no source gives them back.
fn old_cache(shown: &str, told: &Told<'_>) -> Option<String> {
    let kept = "since fetchloom moves no cache";
    let working = "the data/ folders placed from it keep working without it";
    let lost = "which no source gives back, so deleting it would lose those bytes unless a data/ folder holds a copy";
    let instead = told.moved_to.map_or_else(String::new, |to| {
        format!(": move it to {to} instead, before a run makes a new cache there")
    });
    Some(match told.held {
        None if told.imports => format!(
            "{shown} keeps what it cached, {kept}; some of it was imported from files, {lost}{instead}"
        ),
        None => format!(
            "{shown} keeps what it cached, {kept}; {working}, though a file there linked to it keeps its bytes when it is deleted"
        ),
        Some(Held { bytes: 0, .. }) => return None,
        Some(Held {
            bytes, imported, ..
        }) if imported > 0 => format!(
            "{shown} keeps the {} it cached, {kept}; {} of it was imported from files, {lost}{instead}",
            units::size(bytes),
            units::size(imported)
        ),
        Some(Held {
            bytes, linked: 0, ..
        }) => format!(
            "{shown} keeps the {} it cached, {kept}; {working}, so deleting it frees up to that",
            units::size(bytes)
        ),
        Some(Held { bytes, linked, .. }) if linked >= bytes => format!(
            "{shown} keeps the {} it cached, {kept}; all of it is linked outside the cache too, so deleting it would free nothing",
            units::size(bytes)
        ),
        Some(Held { bytes, linked, .. }) => format!(
            "{shown} keeps the {} it cached, {kept}; {} of it is linked outside the cache too, so deleting it frees up to {}, and {working}",
            units::size(bytes),
            units::size(linked),
            units::size(bytes - linked)
        ),
    })
}

/// a value of the settings file as a person reads it: text as it is, a list's items separated by
/// commas, each masked where it may carry a login or a key.
fn shown(value: &toml_edit::Value) -> String {
    match value {
        toml_edit::Value::String(text) => crate::shown::mask_typed(text.value()),
        toml_edit::Value::Array(items) if items.is_empty() => "none".to_owned(),
        toml_edit::Value::Array(items) => items.iter().map(shown).collect::<Vec<_>>().join(", "),
        other => other.clone().decorated("", "").to_string(),
    }
}

/// a value of the settings file as `--json` gives it, text masked as [`shown`] masks it.
fn json_of(value: &toml_edit::Value) -> Value {
    match value {
        toml_edit::Value::String(text) => json!(crate::shown::mask_typed(text.value())),
        toml_edit::Value::Integer(number) => json!(number.value()),
        toml_edit::Value::Boolean(on) => json!(on.value()),
        toml_edit::Value::Array(items) => Value::Array(items.iter().map(json_of).collect()),
        other => json!(other.clone().decorated("", "").to_string()),
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
    output.label(
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
pub(super) fn warn_unread(output: &Output, var: crate::machine::Var<'_>) {
    for (name, help) in DISPLAY_VARIABLES {
        if let Err(error) = crate::machine::switched(name, var(name).as_deref()) {
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
        only_variable: false,
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
        only_variable: false,
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
    shown.push(switch("mirrors_only", settings.mirrors_only));
    shown.push(switch("web_archives", settings.web_archives));
    let reuse = settings.reuse.value.iter();
    let reuse = reuse.map(|folder| folder.path.display().to_string());
    shown.push(list("reuse", reuse, vec![settings.reuse.origin]));
    let links = settings.data_links.value.iter();
    let links = links.map(|folder| folder.display().to_string());
    shown.push(list("data_links", links, vec![settings.data_links.origin]));
    let push_to = settings.push_to.value.as_ref();
    shown.push(Shown {
        name: "push_to",
        value: push_to.map_or(Value::Null, |target| json!(target.to_string())),
        origin: vec![settings.push_to.origin],
        variable: crate::settings::variable("push_to"),
        said: push_to.map_or_else(|| "none".to_owned(), ToString::to_string),
        only_variable: false,
    });
    shown.push(optional_size("cache_budget", settings.cache_budget));
    shown.push(rate(settings.rate));
    shown.extend(variables(context));
    shown
}

/// the settings only a variable sets: threads, split and accessible.
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
        only_variable: true,
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
        only_variable: true,
    });
    let accessible = crate::machine::is_on(&var, "FETCHLOOM_ACCESSIBLE");
    shown.push(Shown {
        name: "accessible",
        value: json!(accessible),
        origin: vec![from_env("FETCHLOOM_ACCESSIBLE")],
        variable: "FETCHLOOM_ACCESSIBLE",
        said: accessible.to_string(),
        only_variable: true,
    });
    shown
}

/// a setting that is true or false.
fn switch(name: &'static str, set: Set<bool>) -> Shown {
    Shown {
        name,
        value: json!(set.value),
        origin: vec![set.origin],
        variable: crate::settings::variable(name),
        said: set.value.to_string(),
        only_variable: false,
    }
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
        only_variable: false,
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
        only_variable: false,
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
        only_variable: false,
    }
}

/// a size that may be unset, which then sets no limit.
fn optional_size(name: &'static str, set: Set<Option<u64>>) -> Shown {
    Shown {
        name,
        value: set.value.map_or(Value::Null, |bytes| json!(bytes)),
        origin: vec![set.origin],
        variable: crate::settings::variable(name),
        said: set.value.map_or_else(|| "no limit".to_owned(), units::size),
        only_variable: false,
    }
}

/// the rate cap in bytes a second, readable as a speed.
fn rate(set: Set<Option<u64>>) -> Shown {
    Shown {
        name: "rate",
        value: set.value.map_or(Value::Null, |rate| json!(rate)),
        origin: vec![set.origin],
        variable: crate::settings::variable("rate"),
        said: set.value.map_or_else(
            || "no limit".to_owned(),
            |rate| format!("{}/s", units::size(rate)),
        ),
        only_variable: false,
    }
}

fn level(set: Set<crate::settings::Level>) -> Shown {
    Shown {
        name: "resources",
        value: json!(set.value.name()),
        origin: vec![set.origin],
        variable: crate::settings::variable("resources"),
        said: set.value.name().to_owned(),
        only_variable: false,
    }
}

/// the widest a value may make its column. a longer one, such as a deep cache folder, runs past
/// it, its origin and variable on a line of their own, so the other rows keep their columns.
const VALUE: usize = 24;

/// the settings in aligned columns, a line each, for a person or `grep`. a terminal `width` shows
/// the cache from `~`, breaks after a long value, and when narrow drops variables, then origins.
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
            // nobody set a setting only its variable sets, and the settings file cannot
            let unset = setting.only_variable
                && setting
                    .origin
                    .iter()
                    .all(|origin| matches!(origin, Origin::Default | Origin::Machine));
            let origin = if unset {
                "variable only".to_owned()
            } else {
                setting
                    .origin
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            [
                setting.name.to_owned(),
                said,
                origin,
                setting.variable.to_owned(),
            ]
        })
        .collect();
    let widest = |column: usize, most: usize| {
        rows.iter()
            .map(|row| units::columns(&row[column]))
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
    if width.is_some_and(|width| widths[0] + widths[1] + widths[2] + 4 > width) {
        // too narrow for a value and its origin side by side, so each origin goes on a line of
        // its own, under its value
        return rows
            .iter()
            .map(|row| {
                let pad = units::columns(&row[0]) + 2;
                format!("{}  {}\n{:pad$}{}", row[0], row[1], "", row[2])
            })
            .collect::<Vec<_>>()
            .join("\n");
    }
    rows.iter()
        .map(|row| {
            let mut line = format!("{:<name$}  ", row[0], name = widths[0]);
            let value = units::columns(&row[1]);
            if value > widths[1] && width.is_none() {
                // a pipe gets each row on one line, so `grep` finds its origin
                let _ = write!(line, "{}  ", row[1]);
            } else if value > widths[1] {
                // the origin and variable go under their columns, on a line of their own
                let _ = write!(
                    line,
                    "{}\n{:<pad$}",
                    row[1],
                    "",
                    pad = widths[0] + widths[1] + 4
                );
            } else {
                let _ = write!(line, "{}{:pad$}  ", row[1], "", pad = widths[1] - value);
            }
            if variables {
                let pad = widths[2] - units::columns(&row[2]);
                let _ = write!(line, "{}{:pad$}  {}", row[2], "", row[3]);
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

    /// a terminal too narrow for every column leaves out the variable column.
    #[test]
    fn a_long_value_keeps_the_other_rows_in_their_columns() {
        let row = |name: &'static str, said: &str, variable: &'static str| Shown {
            name,
            value: json!(said),
            origin: vec![Origin::Default],
            variable,
            said: said.to_owned(),
            only_variable: false,
        };
        let cache = "/scratch/a-very-long-group-name/someone/fetchloom-cache";
        let shown = [
            row("cache", cache, "FETCHLOOM_CACHE"),
            row("jobs", "8", "FETCHLOOM_JOBS"),
        ];
        assert_eq!(
            table(&shown, Some(120)),
            format!(
                "cache  {cache}\n          default  FETCHLOOM_CACHE\njobs   8  default  FETCHLOOM_JOBS"
            )
        );
        assert_eq!(
            table(&shown, Some(30)),
            format!("cache  {cache}\n          default\njobs   8  default")
        );
        let shown = [
            row("cache", "/data/鸢尾花", "FETCHLOOM_CACHE"),
            row("jobs", "8", "FETCHLOOM_JOBS"),
        ];
        let told = table(&shown, None);
        let starts: Vec<usize> = told
            .lines()
            .map(|line| console::measure_text_width(&line[..line.find("default").unwrap()]))
            .collect();
        assert_eq!(
            starts[0], starts[1],
            "a wide character takes two columns:\n{told}"
        );
    }

    /// a setting only its variable sets says so where nobody set it, and a pipe keeps each row on
    /// one line, a long value with it, so `grep` finds its origin.
    #[test]
    fn a_variable_only_setting_says_so_and_a_pipe_keeps_each_row_whole() {
        let cache = "/scratch/a-very-long-group-name/someone/fetchloom-cache";
        let shown = [
            Shown {
                name: "cache",
                value: json!(cache),
                origin: vec![Origin::Environment],
                variable: "FETCHLOOM_CACHE",
                said: cache.to_owned(),
                only_variable: false,
            },
            Shown {
                name: "threads",
                value: json!(24),
                origin: vec![Origin::Machine],
                variable: "FETCHLOOM_THREADS",
                said: "24".to_owned(),
                only_variable: true,
            },
            Shown {
                name: "rate",
                value: json!(1024),
                origin: vec![Origin::Environment],
                variable: "FETCHLOOM_RATE",
                said: "1.0 KiB/s".to_owned(),
                only_variable: false,
            },
        ];
        assert_eq!(
            table(&shown, None),
            format!(
                "cache    {cache}  environment    FETCHLOOM_CACHE\n\
                 threads  24         variable only  FETCHLOOM_THREADS\n\
                 rate     1.0 KiB/s  environment    FETCHLOOM_RATE"
            )
        );
    }

    /// a terminal too narrow for a value and its origin side by side gets each origin under its
    /// value, so every line fits, as list leaves out columns to fit.
    #[test]
    fn a_narrow_terminal_gets_each_origin_under_its_value() {
        let row = |name: &'static str, said: &str, origin: Origin| Shown {
            name,
            value: json!(said),
            origin: vec![origin],
            variable: "FETCHLOOM_SPLIT",
            said: said.to_owned(),
            only_variable: false,
        };
        let shown = [
            row("azure_containers", "none", Origin::Default),
            row("split", "when one stream is slow", Origin::Default),
            row("most_files", "2,097,152", Origin::Machine),
        ];
        let told = table(&shown, Some(40));
        assert_eq!(
            told,
            "azure_containers  none\n                  default\n\
             split  when one stream is slow\n       default\n\
             most_files  2,097,152\n            this machine"
        );
        assert!(
            told.lines().all(|line| units::columns(line) <= 40),
            "{told}"
        );
    }

    /// `--json` keeps the plain number.
    #[test]
    fn counts_read_with_their_thousands_apart() {
        let files = count("archive_files", 10_000_000_u64, Origin::Default);
        assert_eq!(files.said, "10,000,000");
        assert_eq!(files.value, json!(10_000_000));
        assert_eq!(count("jobs", 8_usize, Origin::Default).said, "8");
    }

    /// deleting a moved cache whose files data/ folders link to frees nothing, and one that holds
    /// no files is not told about.
    #[test]
    fn the_old_cache_says_what_deleting_it_frees() {
        let old = "~/.cache/fetchloom";
        let told = |bytes, linked, imported| Told {
            held: Some(Held {
                bytes,
                linked,
                imported,
            }),
            imports: imported > 0,
            moved_to: None,
        };
        assert_eq!(
            old_cache(old, &told(17_613, 17_613, 0)).as_deref(),
            Some(
                "~/.cache/fetchloom keeps the 17.2 KiB it cached, since fetchloom moves no cache; all of it is linked outside the cache too, so deleting it would free nothing"
            )
        );
        assert_eq!(
            old_cache(old, &told(17_613, 0, 0)).as_deref(),
            Some(
                "~/.cache/fetchloom keeps the 17.2 KiB it cached, since fetchloom moves no cache; the data/ folders placed from it keep working without it, so deleting it frees up to that"
            )
        );
        assert_eq!(old_cache(old, &told(0, 0, 0)), None);
        assert_eq!(
            old_cache(old, &told(17_613, 0, 1_024)).as_deref(),
            Some(
                "~/.cache/fetchloom keeps the 17.2 KiB it cached, since fetchloom moves no cache; 1.0 KiB of it was imported from files, which no source gives back, so deleting it would lose those bytes unless a data/ folder holds a copy"
            )
        );
        let uncounted = Told {
            held: None,
            imports: true,
            moved_to: Some("/scratch/fetchloom"),
        };
        assert_eq!(
            old_cache(old, &uncounted).as_deref(),
            Some(
                "~/.cache/fetchloom keeps what it cached, since fetchloom moves no cache; some of it was imported from files, which no source gives back, so deleting it would lose those bytes unless a data/ folder holds a copy: move it to /scratch/fetchloom instead, before a run makes a new cache there"
            )
        );
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

    /// mirrors `--mirror` names come first, then those of the setting it did not name, each once.
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
