//! `fetchloom remove`: a dataset out of data.toml and data.lock, and its unchanged files out of the
//! data folder. it downloads nothing.

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

pub(super) async fn remove(context: &Context, args: Remove) -> Result<Report, Error> {
    let Some(name) = args.name else {
        return Err(unnamed(context));
    };
    let project = find(&context.output)?;
    let dry_run = args.dry.dry_run;
    if !dry_run {
        // asked before the project is held, so other runs never wait on the answer
        ask_import(context, &project, &name)?;
    }
    let _held = if dry_run {
        None
    } else {
        Some(hold(context, &project, "remove").await?)
    };
    let (text, manifest) = project.manifest()?;
    known(&manifest, &name, "remove")?;
    unread(&manifest, &name)?;
    let updated = manifest::without_dataset(&text, &name)?;
    let new_manifest = Manifest::parse(&updated)?;
    let mut lock = project.lock()?.unwrap_or_default();
    let imported: Vec<Sha256> = lock
        .get(&name)
        .filter(|_| manifest.datasets[&name].is_import())
        .map(|locked| locked.files.iter().map(|file| file.sha256).collect())
        .unwrap_or_default();
    lock.datasets.retain(|dataset| dataset.name != name);
    lock.derived.retain(|result| result.name != name);
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
            &format!("{name} from data.toml and data.lock"),
        );
        place::tell_preview(&context.output, &new_manifest, &Scope::All, &preview);
        dry_run_done(&context.output, None);
        let outcome = Outcome {
            lock_changed: true,
            pins: Vec::new(),
            placed: preview,
        };
        return Ok(Report::dry(project.root, vec![name], outcome));
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
    context.output.done("Removed", &name);
    context.output.next(&format!(
        "commit {}; after pulling, other clones remove unchanged files with `fetchloom sync`",
        committed(&project)
    ));
    Ok(Report {
        project: project.root,
        datasets: vec![name],
        lock_changed: true,
        placement: Some(placement),
        ..Report::default()
    })
}

/// refuses to remove `name` while a recipe in `manifest` is made from it.
fn unread(manifest: &Manifest, name: &str) -> Result<(), Error> {
    let readers: Vec<&str> = manifest
        .datasets
        .iter()
        .filter(|(_, entry)| {
            entry
                .recipe()
                .is_some_and(|recipe| recipe.from.iter().any(|from| from == name))
        })
        .map(|(name, _)| name.as_str())
        .collect();
    if readers.is_empty() {
        return Ok(());
    }
    let them = crate::units::listed(&readers, "and");
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

/// asks before removing `name` when it is an import: no source can give its files back, so once
/// they leave the data folder only the cache keeps them, until gc frees them. with nobody to ask it
/// refuses, unless `--yes`.
fn ask_import(context: &Context, project: &Project, name: &str) -> Result<(), Error> {
    let Ok((_, manifest)) = project.manifest() else {
        return Ok(());
    };
    if context.yes || !manifest.datasets.get(name).is_some_and(Entry::is_import) {
        return Ok(());
    }
    let why = format!("{name} was imported from files, and no source can give them back");
    let cancel = format!("the removal of {name}");
    let question = format!(
        "{why} once {} and the cache's copy are gone. remove it anyway?",
        crate::plan::folder(&manifest, name)
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
            format!("{why}, so removing it needs a yes"),
        )
        .help(format!(
            "answer yes at a terminal, or run `{}` once you keep its files elsewhere",
            super::project::command(&format!("remove {name} --yes"))
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
