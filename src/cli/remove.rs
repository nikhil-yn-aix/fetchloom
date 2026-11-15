//! `fetchloom remove`: a dataset out of data.toml and data.lock, and its unchanged files out of the
//! data folder. it downloads nothing.

use std::collections::{HashMap, HashSet};

use super::args::Remove;
use super::project::{Outcome, Report, committed, dry_run_done, find, hold, known};
use super::sync::disk::Survey;
use super::sync::{Scope, place};
use super::{Context, open_cache, peek_cache};
use crate::digest::Sha256;
use crate::error::{Error, Kind};
use crate::project::Project;
use crate::project::lock;
use crate::project::manifest::{self, Entry, Manifest};
use crate::stop::Stop;

pub(super) async fn remove(context: &Context, args: Remove) -> Result<Report, Error> {
    if args.names.is_empty() {
        return Err(unnamed(context));
    }
    let names = distinct(args.names);
    let project = find(&context.output)?;
    let dry_run = args.dry.dry_run;
    if !dry_run {
        // asked before the project is held, so other runs never wait on the answer
        ask_import(context, &project, &names)?;
    }
    let _held = if dry_run {
        None
    } else {
        Some(hold(context, &project, "remove").await?)
    };
    let (text, manifest) = project.manifest()?;
    for name in &names {
        known(&manifest, name, "remove")?;
    }
    unread(&manifest, &names)?;
    // one edit of data.toml for every name, which a stop between names ends with nothing written
    let updated = manifest::without_datasets(&text, &names, |at| {
        stop_for_tests(&context.stop, at);
        context.stop.check()
    })?;
    let new_manifest = Manifest::parse(&updated)?;
    let mut lock = project.lock()?.unwrap_or_default();
    let imported: Vec<Sha256> = names
        .iter()
        .filter(|name| manifest.datasets[name.as_str()].is_import())
        .filter_map(|name| lock.get(name))
        .flat_map(|locked| locked.files.iter().map(|file| file.sha256))
        .collect();
    let going: HashSet<&str> = names.iter().map(String::as_str).collect();
    lock.datasets
        .retain(|dataset| !going.contains(dataset.name.as_str()));
    lock.derived
        .retain(|result| !going.contains(result.name.as_str()));
    lock.stated
        .retain(|stated| !names.iter().any(|name| name == stated.name()));
    let listed = crate::units::listed(&names, "and");
    let cache = if dry_run {
        peek_cache()?
    } else {
        open_cache()?
    };
    let stop = &context.stop;
    let survey = Survey::read(&project, &new_manifest.data_dir, context.threads, stop)?;
    let placing = place::Placing::new(
        &project,
        &new_manifest,
        &lock,
        &cache,
        false,
        &Scope::All,
        survey,
    )?;
    if dry_run {
        let preview = placing.preview_removal();
        context.output.step(
            "Would remove",
            &format!("{listed} from data.toml and data.lock"),
        );
        place::tell_preview(&context.output, &new_manifest, &Scope::All, &preview);
        dry_run_done(&context.output, None);
        let outcome = Outcome {
            lock_changed: true,
            pins: Vec::new(),
            placed: preview,
        };
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
        error.detail(format!(
            "data.toml and data.lock were updated; after fixing the error, `{}` finishes the removal",
            super::project::command("sync")
        ))
    })?;
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

/// `asked` without the names given twice, in the order given.
fn distinct(asked: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::with_capacity(asked.len());
    asked
        .into_iter()
        .filter(|name| seen.insert(name.clone()))
        .collect()
}

/// refuses to remove the first of `removed` that a recipe in `manifest` is made from, unless that
/// recipe is removed too.
fn unread(manifest: &Manifest, removed: &[String]) -> Result<(), Error> {
    let going: HashSet<&str> = removed.iter().map(String::as_str).collect();
    let mut readers: HashMap<&str, Vec<&str>> = HashMap::new();
    for (reader, entry) in &manifest.datasets {
        let Some(recipe) = entry.recipe() else {
            continue;
        };
        if going.contains(reader.as_str()) {
            continue;
        }
        for from in &recipe.from {
            let of = readers.entry(from.as_str()).or_default();
            if of.last() != Some(&reader.as_str()) {
                of.push(reader.as_str());
            }
        }
    }
    let Some((name, readers)) = removed
        .iter()
        .find_map(|name| Some((name, readers.get(name.as_str())?)))
    else {
        return Ok(());
    };
    let them = crate::units::listed(readers, "and");
    let (are, their) = if readers.len() == 1 {
        ("is", "its")
    } else {
        ("are", "their")
    };
    Err(
        Error::new(Kind::Usage, format!("{them} {are} made from {name}")).help(format!(
            "remove {them} first, or take {name} out of {their} from in data.toml"
        )),
    )
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
fn ask_import(context: &Context, project: &Project, names: &[String]) -> Result<(), Error> {
    if context.yes {
        return Ok(());
    }
    let Ok((_, manifest)) = project.manifest() else {
        return Ok(());
    };
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
        .map(|import| crate::plan::folder(&manifest, import))
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
    let Some((_, manifest)) = find(&context.output)
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
