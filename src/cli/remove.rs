//! `fetchloom remove`: a dataset out of data.toml and data.lock, and its unchanged files out of the
//! data folder. it downloads nothing.

use std::collections::HashSet;

use super::args::Remove;
use super::project::{Outcome, Report, committed, dry_run_done, find, hold, known};
use super::sync::disk::{Folders, Survey};
use super::sync::{Scope, place};
use super::{Context, open_cache, peek_cache};
use crate::digest::Sha256;
use crate::error::{Error, Kind};
use crate::output::{Output, Warning};
use crate::plan;
use crate::project::lock;
use crate::project::manifest::{self, Entry, Manifest};
use crate::stop::Stop;

pub(super) async fn remove(context: &Context, args: Remove) -> Result<Report, Error> {
    if args.names.is_empty() {
        return Err(unnamed(context));
    }
    let names = distinct(args.names);
    let project = find(context)?;
    let dry_run = args.dry.dry_run;
    if !dry_run && let Ok((_, manifest)) = project.manifest() {
        // the names first, so no yes is asked for a removal a typo then stops; asked before the
        // project is held, so other runs never wait on the answer
        named(&manifest, &names)?;
        ask_import(context, &manifest, &names)?;
    }
    let _held = if dry_run {
        None
    } else {
        Some(hold(context, &project, "remove").await?)
    };
    let (text, mut kept) = project.manifest()?;
    named(&kept, &names)?;
    // one edit of data.toml for every name, which a stop between names ends with nothing written
    let updated = manifest::without_datasets(&text, &names, |at| {
        stop_for_tests(&context.stop, at);
        context.stop.check()
    })?;
    // what stays is planned from the entries read, whose lines are those data.toml holds until
    // the removal writes it
    let gone = kept.take(&names);
    let mut lock = project.lock()?.unwrap_or_default();
    let imported: Vec<Sha256> = names
        .iter()
        .filter(|name| gone.datasets[name.as_str()].is_import())
        .filter_map(|name| lock.get(name))
        .flat_map(|locked| locked.files.iter().map(|file| file.sha256))
        .collect();
    let going: HashSet<&str> = names.iter().map(String::as_str).collect();
    lock.datasets
        .retain(|dataset| !going.contains(dataset.name.as_str()));
    lock.stated
        .retain(|stated| !names.iter().any(|name| name == stated.name()));
    // one waiting for its file list goes too, as a removal needs nothing of its list
    lock.waiting
        .retain(|waiting| !going.contains(waiting.name.as_str()));
    let listed = crate::units::listed(&names, "and");
    let cache = if dry_run {
        peek_cache()?
    } else {
        open_cache()?
    };
    let stop = &context.stop;
    let folders = Folders::of(&kept);
    let survey = Survey::read(&project, &folders, context.threads, stop)?;
    let unrecorded = unrecorded(&survey, &gone, &names);
    let mut placing =
        place::Placing::new(&project, &kept, &lock, &cache, false, &Scope::All, survey)?;
    placing.unmark_removed(unlisted_outputs(&gone, &folders));
    if dry_run {
        let outcome = previewed(context, &placing, &kept, &listed, &unrecorded);
        return Ok(Report::dry(project.root, names, outcome));
    }
    // a stop asked while the data folder was read leaves the project as it was; once the files
    // change, the removal runs to its end
    context.stop.check()?;
    // gc then asks before it frees these files, even if they reached the cache from data/
    cache
        .mark_imported(&imported)
        .map_err(|err| Error::io("could not record the imported files in the cache", &err))?;
    context.stop.finish();
    project.write(manifest::FILE, updated.as_bytes())?;
    context.wrote_lists(&project.root, project.write_lists(&lock)?);
    project.write(lock::FILE, lock.render().as_bytes())?;
    project.sweep_lists(&lock);
    let placement = placing.remove_only(&context.output).map_err(|error| {
        error
            .finished_by(&super::project::command("sync"), "the removal")
            .detail(format!(
                "data.toml and data.lock already leave out {listed}"
            ))
    })?;
    tell_unrecorded(&context.output, &unrecorded, false);
    context.output.done("Removed", &listed);
    context.output.next(&format!(
        "commit {}; after pulling, other clones remove unchanged files with `fetchloom sync`",
        committed(&project)
    ));
    Ok(Report {
        project: project.root,
        datasets: names,
        lock_changed: true,
        placement: Some(placement),
        ..Report::default()
    })
}

/// the folders of the outputs of `gone` that none of `folders`, those the removal lists, holds.
fn unlisted_outputs(gone: &Manifest, folders: &Folders) -> Vec<String> {
    gone.datasets
        .values()
        .filter_map(|entry| entry.at.clone())
        .filter(|at| folders.holding(at).is_none())
        .collect()
}

/// what the removal of `listed` would change, told as the removal tells it.
fn previewed(
    context: &Context,
    placing: &place::Placing<'_>,
    manifest: &Manifest,
    listed: &str,
    unrecorded: &[(String, Vec<String>)],
) -> Outcome<place::Preview> {
    let preview = placing.preview_removal();
    context.output.step(
        "Would remove",
        &format!("{listed} from data.toml and data.lock"),
    );
    place::tell_preview(&context.output, manifest, &Scope::All, &preview);
    tell_unrecorded(&context.output, unrecorded, true);
    dry_run_done(&context.output, None);
    Outcome {
        lock_changed: true,
        pins: Vec::new(),
        placed: preview,
    }
}

/// the files below the folders of `names` when the state records nothing for this project folder,
/// as in a copy or a clone not yet synced, by folder. removal cannot tell which of them fetchloom
/// placed, so it deletes none. a dataset placed in the data folder itself names no folder of its own.
fn unrecorded(
    survey: &Survey,
    manifest: &Manifest,
    names: &[String],
) -> Vec<(String, Vec<String>)> {
    if survey.state.was_read() {
        return Vec::new();
    }
    names
        .iter()
        .map(|name| plan::folder(manifest, name))
        .filter(|folder| *folder != manifest.data_dir)
        .filter_map(|folder| {
            let below = format!("{folder}/");
            let mut files: Vec<String> = survey
                .disk
                .keys()
                .filter(|path| path.starts_with(&below))
                .filter(|path| {
                    !crate::place::is_copy_temporary(path.rsplit('/').next().unwrap_or(path))
                })
                .filter(|path| !survey.made_by_system(path))
                .cloned()
                .collect();
            files.sort_unstable();
            (!files.is_empty()).then_some((folder, files))
        })
        .collect()
}

/// one line for the folders [`unrecorded`] found, which the removal leaves as they are. `-v`
/// names each file.
fn tell_unrecorded(output: &Output, unrecorded: &[(String, Vec<String>)], dry_run: bool) {
    if unrecorded.is_empty() {
        return;
    }
    let folders: Vec<&str> = unrecorded
        .iter()
        .map(|(folder, _)| folder.as_str())
        .collect();
    let files: Vec<&String> = unrecorded.iter().flat_map(|(_, files)| files).collect();
    let (keep, its) = match (folders.len(), dry_run) {
        (1, false) => ("keeps", "its"),
        (1, true) => ("would keep", "its"),
        (_, false) => ("keep", "their"),
        (_, true) => ("would keep", "their"),
    };
    output.warn_detailed(
        Warning::PlaceLeftBehind,
        &format!(
            "{} {keep} {its} {}, since fetchloom cannot tell which files it placed in this project folder",
            crate::units::listed(&folders, "and"),
            crate::units::count(files.len() as u64, "file", "files")
        ),
        None,
        &files,
    );
    for path in files {
        output.detail(path);
    }
}

/// `asked` without the names given twice, in the order given.
fn distinct(asked: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::with_capacity(asked.len());
    asked
        .into_iter()
        .filter(|name| seen.insert(name.clone()))
        .collect()
}

/// refuses the first of `names` data.toml does not have; a path in the data folder is answered
/// with the dataset it names or with `exclude`, and a name near none, given with others, with the
/// command line that leaves it out.
fn named(manifest: &Manifest, names: &[String]) -> Result<(), Error> {
    for name in names {
        known(manifest, name, "remove").map_err(|error| match exclusion(manifest, name) {
            Some(help) => error.help(help),
            None if names.len() > 1
                && crate::name::closest(name, manifest.datasets.keys().map(String::as_str))
                    .is_none() =>
            {
                let others: Vec<&str> = names
                    .iter()
                    .map(String::as_str)
                    .filter(|other| other != name)
                    .collect();
                let instead = format!("fetchloom remove {}", others.join(" "));
                error.help(format!(
                    "leave it out, as in `{}`",
                    super::meant(&[name], &[], instead)
                ))
            }
            None => error,
        })?;
    }
    Ok(())
}

/// the help for a path given where remove takes a dataset's name: a dataset's own folder names
/// the dataset, and `exclude` leaves out files below it.
fn exclusion(manifest: &Manifest, typed: &str) -> Option<String> {
    let path = typed.trim_start_matches("./").trim_end_matches('/');
    let command = super::project::command;
    // the form `path` takes, `iris/Iris.csv`, names a file of a dataset too
    if let Some((name, rest)) = path.split_once('/')
        && manifest.datasets.contains_key(name)
        && !rest.is_empty()
    {
        let file = format!("{}/{rest}", plan::folder(manifest, name));
        return Some(format!(
            "exclude leaves out files by their paths, as in `{}`",
            command(&format!("exclude {}", crate::suggested::shell_word(&file)))
        ));
    }
    path.strip_prefix(&format!("{}/", manifest.data_dir))?;
    if let Some(name) = manifest
        .datasets
        .keys()
        .find(|name| plan::folder(manifest, name) == path)
    {
        return Some(format!(
            "did you mean `{}`?",
            command(&format!("remove {name}"))
        ));
    }
    Some(format!(
        "exclude leaves out files by their paths, as in `{}`",
        command(&format!("exclude {}", crate::suggested::shell_word(typed)))
    ))
}

/// a debug build's tests ask `stop` before the name at the index `FETCHLOOM_TEST_STOP_AFTER`
/// gives, as a ctrl-c then would.
#[cfg(debug_assertions)]
fn stop_for_tests(stop: &Stop, at: usize) {
    if std::env::var("FETCHLOOM_TEST_STOP_AFTER")
        .ok()
        .and_then(|after| after.parse::<usize>().ok())
        == Some(at)
    {
        stop.ask();
    }
}

#[cfg(not(debug_assertions))]
fn stop_for_tests(_: &Stop, _: usize) {}

/// asks before removing the imports among `names`, whose files no source can give back once gc
/// frees them from the cache; with nobody to ask it refuses, unless `--yes`.
fn ask_import(context: &Context, manifest: &Manifest, names: &[String]) -> Result<(), Error> {
    if context.yes {
        return Ok(());
    }
    let imports: Vec<&str> = names
        .iter()
        .filter(|name| {
            manifest
                .datasets
                .get(name.as_str())
                .is_some_and(Entry::is_import)
        })
        .map(String::as_str)
        .collect();
    if imports.is_empty() {
        return Ok(());
    }
    let name = crate::units::listed(&imports, "and");
    let (was, it, its) = if imports.len() == 1 {
        ("was", "it", "its")
    } else {
        ("were", "them", "their")
    };
    let folders: Vec<String> = imports
        .iter()
        .map(|import| crate::plan::folder(manifest, import))
        .collect();
    let why = format!("{name} {was} imported from files, and no source can give them back");
    let cancel = format!("the removal of {name}");
    let question = format!(
        "{why} once {} and the cache's copy are gone. remove {it} anyway?",
        crate::units::listed(&folders, "and")
    );
    match context
        .output
        .confirm(&question)
        .map_err(|error| error.cancelling(&cancel))?
    {
        Some(true) => Ok(()),
        Some(false) => Err(
            Error::new(Kind::Unconfirmed, format!("did not remove {name}"))
                .cancelled(cancel)
                .unchanged(),
        ),
        None => Err(Error::new(
            Kind::Unconfirmed,
            format!("{why}, so removing {it} needs a yes"),
        )
        .help(format!(
            "answer yes at a terminal, or run `{}` once you keep {its} files elsewhere",
            super::project::command(&format!("remove {} --yes", names.join(" ")))
        ))),
    }
}

/// the error for `remove` given no name, listing the datasets, since only a person knows which.
fn unnamed(context: &Context) -> Error {
    const NAMED: usize = 10;
    let error = Error::new(Kind::Usage, "remove needs <name>");
    let Some((_, manifest)) = find(context)
        .ok()
        .and_then(|project| project.manifest().ok())
    else {
        return error.help("`fetchloom remove --help` says what it takes");
    };
    let names: Vec<&str> = manifest.datasets.keys().map(String::as_str).collect();
    let Some(first) = names.first() else {
        return error.help("data.toml has no datasets yet, so there is nothing to remove");
    };
    let more = match names.len().saturating_sub(NAMED) {
        0 => String::new(),
        more => format!(" and {} more", crate::units::grouped(more as u64)),
    };
    error
        .detail(format!(
            "datasets: {}{more}",
            names[..names.len().min(NAMED)].join(", ")
        ))
        .help(format!(
            "name one of them, as in `{}`",
            super::project::command(&format!("remove {first}"))
        ))
}
