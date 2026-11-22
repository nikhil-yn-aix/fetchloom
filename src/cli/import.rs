//! `add <path>`: files on this machine pinned by their bytes, found without following a link and
//! copied into the cache. they are only read, never changed, moved or linked, since a link to the
//! cache's copy would make a person's file read-only.

use std::collections::HashMap;
use std::collections::hash_map::Entry as Slot;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use super::add::{AS, AddProject, free_name, from_here, render_add, sync_add, usable};
use super::args::Add;
use super::project::{Report, dry_run_done};
use super::sync::place::Changes;
use super::sync::place::tell_marked;
use super::sync::{Given, Loop};
use super::{Context, open_cache, peek_cache};
use crate::archive;
use crate::cache::{Cache, Sealed};
use crate::digest::Sha256;
use crate::error::{Error, Kind};
use crate::glob::Glob;
use crate::output::{Output, Warning};
use crate::plan;
use crate::project::Project;
use crate::project::lock::{self, Lock, Locked, LockedFile};
use crate::project::manifest::{self, Manifest, Source};
use crate::source::{self, Access, local};
use crate::stop::Stop;
use crate::volume::is_plain;
use crate::{name, place, units};

mod in_place;

/// `add <path>`: a folder of the project pinned where it is, as an output, or else a folder's
/// files, or one file, imported.
pub(super) async fn path_held(
    context: &Context,
    adding: &mut AddProject,
    args: &Add,
    typed: &str,
    here: &Path,
    starting: bool,
) -> Result<Report, Error> {
    let shown = typed.trim();
    let start = located(shown, here, std::env::home_dir().as_deref())?;
    let data_dir = &adding.manifest.data_dir;
    if let Some(at) = in_place::at_of(&adding.project, data_dir, &start)? {
        let typed = (start.as_path(), shown, at.as_str());
        return in_place::pin_held(context, adding, args, typed, here, starting).await;
    }
    import_held(context, adding, args, (&start, shown), here, starting).await
}

/// `start`, a real path, as a path from the top of `project` with `/` between names, empty for
/// the project's own folder: below the project's folder, or below its data folder `data_dir`
/// reached through the link the project follows there, as a cluster's pipeline writes
/// data/processed onto scratch. none for a path elsewhere.
pub(super) fn in_project(
    project: &Project,
    data_dir: &str,
    start: &Path,
) -> Result<Option<String>, Error> {
    let joined = |below: &Path| -> Option<String> {
        let names: Option<Vec<&str>> = below.iter().map(std::ffi::OsStr::to_str).collect();
        names.map(|names| names.join("/"))
    };
    if let Ok(root) = std::fs::canonicalize(&project.root)
        && let Ok(below) = start.strip_prefix(&root)
    {
        return Ok(joined(below));
    }
    let Some(found) = project.followed_folder(data_dir)? else {
        return Ok(None);
    };
    Ok(start
        .strip_prefix(&found)
        .ok()
        .and_then(joined)
        .map(|below| match below.as_str() {
            "" => data_dir.to_owned(),
            _ => format!("{data_dir}/{below}"),
        }))
}

/// `add <path>`: a folder's files, or one file, at `start`, `shown` as typed, read and pinned by
/// their bytes. with `--as` naming an import data.toml has, they must be the files data.lock
/// pins, and only fill the data folder.
async fn import_held(
    context: &Context,
    adding: &mut AddProject,
    args: &Add,
    (start, shown): (&Path, &str),
    here: &Path,
    starting: bool,
) -> Result<Report, Error> {
    let dry_run = args.dry.dry_run;
    let start = start.to_path_buf();
    let old = adding.old.take();
    let adding = &*adding;
    let project = &adding.project;
    let manifest = &adding.manifest;
    if !dry_run {
        project.check_writable()?;
    }
    let cache = if dry_run {
        peek_cache()?
    } else {
        open_cache()?
    };
    refuse_overlap(
        (&start, shown),
        &project.root,
        &manifest.data_dir,
        cache.root(),
        here,
    )?;
    let waiting = waiting_import(args, &start, old.as_ref());
    let (supplied, source, walked) =
        walk_import(context, manifest, args, (&start, shown), waiting.as_deref())?;
    let pinned = supplied
        .as_ref()
        .and_then(|name| current(old.as_ref(), name, &manifest.datasets[name.as_str()]));
    if let (Some(name), Some(pinned)) = (&supplied, pinned)
        && let Some(difference) = first_difference(&walked.files, &pinned.files)
    {
        return Err(other_files(shown, name, difference));
    }
    let own = match (&supplied, args.name.as_deref()) {
        (Some(name), _) => name.clone(),
        (None, Some(asked)) => asked.to_owned(),
        (None, None) => {
            dataset_name(&start, start.is_file()).unwrap_or_else(|| "dataset".to_owned())
        }
    };
    let retaken = if supplied.is_none() && args.name.is_none() {
        retaken(manifest, old.as_ref(), (&own, shown), &walked.files)?
    } else {
        None
    };
    let net = Arc::new(context.net());
    if dry_run {
        let supplying = supplied.is_some() || retaken.is_some();
        let name = if supplying {
            own
        } else {
            new_name(&context.output, manifest, args, own)?
        };
        let work = Loop::new(context, project, net, cache).adding(&name);
        let found = (&walked, shown, supplying);
        return Ok(preview_import(&work, manifest, found, &name, here));
    }
    room(context, &cache, &walked, shown)?;
    let copied = into_cache(context, &cache, &walked, &|_| Ok(()))?;
    let told = read_line(&walked, &copied, shown);
    // the walk's list of files is read for the last time above; freed here, it stays out of the
    // peak of placing them
    drop(walked);
    let report = if let Some(name) = supplied {
        let work = Loop::new(context, project, net, cache).adding(&name);
        let imported = Imported {
            name: &name,
            source: &source,
            files: copied.files,
        };
        supply(&work, adding, old, imported, (shown, &told), here).await
    } else if args.name.is_none()
        && same_import(manifest, old.as_ref(), &own, &source, &copied.files)
    {
        // the same files imported again under their own name change nothing, as the same link does
        let work = Loop::new(context, project, net, cache).adding(&own);
        already_imported(&work, adding, old, (&own, &told), here).await
    } else {
        if let Some(entry) = retaken {
            let difference = current(old.as_ref(), &own, entry)
                .and_then(|pinned| first_other_bytes(&copied.files, &pinned.files));
            return Err(other_import(manifest, &own, shown, difference));
        }
        let name = new_name(&context.output, manifest, args, own)?;
        let work = Loop::new(context, project, net, cache).adding(&name);
        let imported = Imported {
            name: &name,
            source: &source,
            files: copied.files,
        };
        import_new(&work, adding, old, (imported, &told), here, starting).await
    }?;
    warn_in_git(&context.output, &project.root, (&start, shown));
    Ok(report)
}

/// the import data.toml has under `own`, the name the files `found` in `shown` would take, when
/// they are its pinned files: other paths or sizes are refused before a copy, other bytes after.
fn retaken<'m>(
    manifest: &'m Manifest,
    old: Option<&Lock>,
    (own, shown): (&str, &str),
    found: &[Found],
) -> Result<Option<&'m manifest::Entry>, Error> {
    let Some(entry) = manifest.datasets.get(own).filter(|entry| entry.is_import()) else {
        return Ok(None);
    };
    let difference = current(old, own, entry).map_or_else(
        || Some("data.lock holds no pin of it".to_owned()),
        |pinned| first_difference(found, &pinned.files),
    );
    match difference {
        Some(difference) => Err(other_import(manifest, own, shown, Some(difference))),
        None => Ok(Some(entry)),
    }
}

/// the refusal of the files in `shown`, which differ from those import `name` pins as `difference`
/// says; a numbered name would leave scripts on the old files, so the person chooses.
fn other_import(manifest: &Manifest, name: &str, shown: &str, difference: Option<String>) -> Error {
    let difference =
        difference.unwrap_or_else(|| "data.toml chooses its files with other patterns".to_owned());
    let free = (2..100)
        .map(|number| format!("{name}-{number}"))
        .find(|numbered| !manifest.datasets.contains_key(numbered))
        .unwrap_or_else(|| "<name>".to_owned());
    let path = super::shell_word(shown);
    let keep = super::meant(
        &[shown],
        &[shown, "--as", &free],
        super::project::command(&format!("add {path} --as {free}")),
    );
    Error::new(
        Kind::Usage,
        format!("{name} pins other files than {shown} holds"),
    )
    .detail(difference)
    .help(format!(
        "to pin these instead, run `{}`, then `{}`; to keep both, run `{keep}`",
        super::project::command(&format!("remove {name}")),
        super::project::command(&format!("add {path}"))
    ))
}

/// the name a new import takes: the one `--as` gives, or `own`, with the next number when
/// data.toml has it.
fn new_name(
    output: &Output,
    manifest: &Manifest,
    args: &Add,
    own: String,
) -> Result<String, Error> {
    if args.name.is_some() {
        return Ok(own);
    }
    free_name(
        output,
        own,
        &|name| manifest.datasets.contains_key(name),
        AS,
    )
}

/// warns when `start`, the files an import read, is in the git work tree the project at `project`
/// is in, where `git add .` takes raw data into the repository, or git has it already.
fn warn_in_git(output: &Output, project: &Path, (start, shown): (&Path, &str)) {
    let real = |path: &Path| std::fs::canonicalize(path).ok();
    let Some((top, root)) = Project::repository(project)
        .and_then(real)
        .zip(real(project))
    else {
        return;
    };
    // a path below `folder`, with `/` between its names, as git writes one
    let slashed = |folder: &Path| {
        let names: Option<Vec<&str>> = start
            .strip_prefix(folder)
            .ok()?
            .iter()
            .map(std::ffi::OsStr::to_str)
            .collect();
        names
            .filter(|names| !names.is_empty())
            .map(|names| names.join("/"))
    };
    let Some(inside) = slashed(&top) else {
        return;
    };
    let Some(known) = crate::project::git::known(&top, &[&inside]) else {
        return;
    };
    // .gitignore beside data.toml, or at the top for a folder outside the project
    let below = slashed(&root);
    let (holder, place) = match &below {
        Some(below) => (below.clone(), "the project"),
        None => (inside, "the repository"),
    };
    let mut pattern = format!("/{}", crate::project::gitignore_escaped(&holder));
    if start.is_dir() {
        pattern.push('/');
    }
    let ignore = format!("add {pattern} to .gitignore");
    let (said, help) = if !known.tracked.is_empty() {
        // the told command starts in the project's folder wherever the run did not work in it
        let named = match (crate::suggested::git_root(project), below) {
            (None, _) if !shown.starts_with('~') && !shown.contains(':') => shown.to_owned(),
            (_, Some(below)) => below,
            (_, None) => start.display().to_string(),
        };
        (
            format!("git tracks {shown}, so a commit keeps its files in the repository"),
            format!(
                "run `{}`, then delete it or {ignore}",
                crate::suggested::git(
                    project,
                    &format!("rm -r --cached {}", super::shell_word(&named))
                )
            ),
        )
    } else if !known.untracked.is_empty() {
        (
            format!("{shown} is still in {place}, and git will take it"),
            format!("delete it, or {ignore}"),
        )
    } else {
        return;
    };
    output.warn_with(Warning::GitImportInTree, &said, &help);
}

/// files of a folder copied into the cache, pinned for the dataset `name` as data.toml's `source`
/// would have them.
struct Imported<'a> {
    name: &'a str,
    source: &'a Source,
    files: Vec<lock::LockedFile>,
}

/// the source an import is checked against: data.toml's for `supplied`, an import `--as` names, or
/// else one made from `args`, once `--as` gives a usable name.
fn import_entry(manifest: &Manifest, supplied: Option<&str>, args: &Add) -> Result<Source, Error> {
    if let Some(name) = supplied {
        return supplied_entry(manifest, name, args);
    }
    if let Some(asked) = args.name.as_deref() {
        usable(asked, manifest)?;
    }
    let text = manifest::with_dataset(
        "",
        "new",
        local::IMPORTED,
        &args.filter.select,
        &args.filter.exclude,
        !args.no_extract,
    )?;
    let mut source = Manifest::parse(&text)?.datasets["new"].source.clone();
    source.decompress = args.decompress;
    Ok(source)
}

/// data.lock's pin of `name`, when `old` has one that still answers data.toml's `entry`.
fn current<'a>(
    old: Option<&'a Lock>,
    name: &str,
    entry: &manifest::Entry,
) -> Option<&'a lock::Locked> {
    old.and_then(|lock| lock.get(name))
        .filter(|locked| locked.drift(entry).is_none())
}

/// whether data.toml has `name` as an import with the settings of `source`, pinned in `old` to
/// exactly the files `copied` holds.
fn same_import(
    manifest: &Manifest,
    old: Option<&Lock>,
    name: &str,
    source: &Source,
    copied: &[lock::LockedFile],
) -> bool {
    let Some(existing) = manifest.datasets.get(name) else {
        return false;
    };
    let had = &existing.source;
    let pin = |file: &lock::LockedFile| (file.path.clone(), file.size, file.sha256);
    local::is_imported(&had.reference)
        && had.same_settings(source)
        && current(old, name, existing)
            .is_some_and(|locked| locked.files.iter().map(pin).eq(copied.iter().map(pin)))
}

/// an import whose files data.toml and data.lock pin already: it syncs, and says so.
async fn already_imported(
    work: &Loop<'_>,
    adding: &AddProject,
    old: Option<Lock>,
    (name, told): (&str, &str),
    here: &Path,
) -> Result<Report, Error> {
    let manifest = &adding.manifest;
    let names = [name.to_owned()];
    let outcome = sync_add(work, adding, manifest, None, old, (&names, Vec::new())).await?;
    work.context
        .output
        .step("Imported", &format!("{name} · {told}"));
    tell_marked(&work.context.output, &outcome.placed);
    let folder = from_here(work.project, here, &plan::folder(manifest, name));
    work.context.output.done(
        "Ok",
        &format!("{name} is already in data.toml, its files are in {folder}"),
    );
    Ok(Report {
        project: work.project.root.clone(),
        datasets: Vec::new(),
        lock_changed: outcome.lock_changed,
        placement: Some(outcome.placed),
        ..Report::default()
    })
}

/// adds `imported` to data.toml and pins it in data.lock, then places it, as any add does.
async fn import_new(
    work: &Loop<'_>,
    adding: &AddProject,
    old: Option<Lock>,
    (imported, told): (Imported<'_>, &str),
    here: &Path,
    starting: bool,
) -> Result<Report, Error> {
    let Imported {
        name,
        source,
        files,
    } = imported;
    let text = adding.text.as_deref().unwrap_or_default();
    let mut updated = manifest::with_dataset(
        text,
        name,
        local::IMPORTED,
        &crate::glob::texts_of(&source.select),
        &crate::glob::texts_of(&source.exclude),
        source.extract,
    )?;
    if source.decompress {
        updated = manifest::with_set(&updated, name, "decompress")?;
    }
    let manifest = Manifest::parse(&updated)?;
    let pinned = pinned(name, &manifest.datasets[name].source, files);
    let names = [name.to_owned()];
    let outcome = sync_add(
        work,
        adding,
        &manifest,
        Some(&updated),
        old,
        (&names, vec![Given::Locked(pinned)]),
    )
    .await?;
    // said once the files are in place, so an add that fails never claims them
    work.context
        .output
        .step("Imported", &format!("{name} · {told}"));
    let has_files = outcome.placed.planned.contains(name);
    render_add(
        work,
        &manifest,
        (name, has_files),
        here,
        (starting, &outcome.placed),
        (&[], None),
    );
    Ok(Report {
        project: work.project.root.clone(),
        datasets: names.to_vec(),
        lock_changed: outcome.lock_changed,
        placement: Some(outcome.placed),
        ..Report::default()
    })
}

/// the source of `name`, an import in data.toml that `add <path> --as <name>` supplies. its
/// patterns and its extract and decompress settings stay as data.toml has them.
fn supplied_entry(manifest: &Manifest, name: &str, args: &Add) -> Result<Source, Error> {
    if !args.filter.select.is_empty()
        || !args.filter.exclude.is_empty()
        || args.no_extract
        || args.decompress
    {
        return Err(Error::new(
            Kind::Usage,
            format!("{name} is in data.toml already, so its patterns and unpacking settings come from there"),
        )
        .help(format!(
            "leave out --select, --exclude, --no-extract and --decompress, or change datasets.{name} in data.toml"
        )));
    }
    Ok(manifest.datasets[name].source.clone())
}

/// a supply whose folder, `shown`, holds other files than data.lock pins for `name`.
fn other_files(shown: &str, name: &str, difference: String) -> Error {
    Error::new(
        Kind::LocalIo,
        format!("{shown} does not hold the files data.lock pins for {name}"),
    )
    .detail(difference)
    .help(format!(
        "supply the files {name} was imported from, or run `{}` and add these as a new import",
        super::project::command(&format!("remove {name}"))
    ))
}

/// supplies an import from its files copied into the cache: when data.lock pins them only the data
/// folder changes, and with no pin they are pinned as data.toml asks. other bytes are refused.
async fn supply(
    work: &Loop<'_>,
    adding: &AddProject,
    old: Option<Lock>,
    imported: Imported<'_>,
    (shown, told): (&str, &str),
    here: &Path,
) -> Result<Report, Error> {
    let Imported {
        name,
        source,
        files,
    } = imported;
    let manifest = &adding.manifest;
    let given = match current(old.as_ref(), name, &manifest.datasets[name]) {
        Some(pinned) => {
            if let Some(difference) = first_other_bytes(&files, &pinned.files) {
                return Err(other_files(shown, name, difference));
            }
            Vec::new()
        }
        None => vec![Given::Locked(pinned(name, source, files))],
    };
    let pins = !given.is_empty();
    let output = &work.context.output;
    let names = [name.to_owned()];
    let outcome = sync_add(work, adding, manifest, None, old, (&names, given)).await?;
    // said once the files are in place, so a sync that fails never claims them
    output.step("Supplied", &format!("{name} · {told}"));
    tell_marked(output, &outcome.placed);
    let folder = from_here(work.project, here, &plan::folder(manifest, name));
    if pins {
        output.done(
            "Pinned",
            &format!("{name} in data.lock, its files are in {folder}"),
        );
    } else {
        output.done(
            "Ok",
            &format!("{name} is in data.toml and data.lock as it was, its files are in {folder}"),
        );
    }
    Ok(Report {
        project: work.project.root.clone(),
        datasets: if pins { names.to_vec() } else { Vec::new() },
        lock_changed: outcome.lock_changed,
        placement: Some(outcome.placed),
        ..Report::default()
    })
}

/// `add <path> --dry-run`: what importing the files `walked` found in `shown` as `name`, or
/// supplying them, would change, found without reading a byte of them.
fn preview_import(
    work: &Loop<'_>,
    manifest: &Manifest,
    (walked, shown, supplying): (&Walked, &str, bool),
    name: &str,
    here: &Path,
) -> Report {
    let output = &work.context.output;
    let folder = plan::folder(manifest, name);
    let files = units::count(walked.files.len() as u64, "file", "files");
    let size = units::size(walked.bytes());
    let verb = if supplying {
        "Would supply"
    } else {
        "Would import"
    };
    output.step(
        verb,
        &format!("{name} · {files} · {size} into the cache; {shown} stays unchanged"),
    );
    if !supplying {
        output.step(
            "Would add",
            &format!(
                "{name} to data.toml, its files in {}",
                from_here(work.project, here, &folder)
            ),
        );
    }
    dry_run_done(output, None);
    Report {
        project: work.project.root.clone(),
        datasets: if supplying {
            Vec::new()
        } else {
            vec![name.to_owned()]
        },
        lock_changed: !supplying,
        dry_run: true,
        changes: Some(Changes {
            place: walked
                .files
                .iter()
                .map(|file| format!("{folder}/{}", file.path))
                .collect(),
            ..Changes::default()
        }),
        ..Report::default()
    }
}

/// the name the files at `start` take without `--as`, when `old` has an import of it that waits
/// for its list: those files supply it as `--as` does, since only its list can tell other files
/// from its own, and writing other files' list in its place is refused.
fn waiting_import(args: &Add, start: &Path, old: Option<&Lock>) -> Option<String> {
    let own = args
        .name
        .is_none()
        .then(|| dataset_name(start, start.is_file()))??;
    old?.waits(&own).map(|_| own)
}

/// the import `--as` names, or else `waiting`, that `start`'s files supply, if any, their source
/// and the walk's files. before any copy it says what the walk left out, and may hint keeping
/// numbered tar shards packed.
fn walk_import(
    context: &Context,
    manifest: &Manifest,
    args: &Add,
    (start, shown): (&Path, &str),
    waiting: Option<&str>,
) -> Result<(Option<String>, Source, Walked), Error> {
    let supplied = args
        .name
        .as_deref()
        .or(waiting)
        .filter(|name| {
            manifest
                .datasets
                .get(*name)
                .is_some_and(manifest::Entry::is_import)
        })
        .map(str::to_owned);
    let source = import_entry(manifest, supplied.as_deref(), args)?;
    let walked = told_walk(&context.output, shown, || {
        walk(
            start,
            shown,
            &source,
            context.settings.most_files.value,
            &context.stop,
        )
    })?;
    let output = &context.output;
    tell_left_out(output, shown, &walked);
    if supplied.is_none()
        && source.extract
        && output.shows_hints()
        && let Some(shards) = archive::shards(walked.files.iter().map(|file| file.path.as_str()))
    {
        // a name data.toml has already takes a number once the files are in, so it is not told
        let named = args.name.clone().unwrap_or_else(|| {
            dataset_name(start, start.is_file()).unwrap_or_else(|| "dataset".to_owned())
        });
        let dataset = if args.name.is_none() && manifest.datasets.contains_key(&named) {
            "<name>"
        } else {
            named.as_str()
        };
        let keep = "--no-extract, or extract = false in data.toml";
        output.hint(&shards.hint(shown, keep, Some(dataset)));
    }
    Ok((supplied, source, walked))
}

/// what `walk` gives, told as `Reading <shown>` while it lasts once the command has lasted a
/// moment: a folder of a million files takes seconds to walk.
fn told_walk<T>(output: &Output, shown: &str, walk: impl FnOnce() -> T) -> T {
    let _reading = output.lasting_from_start("Reading", shown.to_owned());
    walk()
}

/// warns about the links and special files below `shown` that an import left out.
fn tell_skipped(output: &Output, shown: &str, skipped: &[String]) {
    let said = match skipped {
        [] => return,
        [one] => format!(
            "left out {one} in {shown}, a link or a special file, which fetchloom never follows or imports"
        ),
        [first, ..] => format!(
            "left out {} in {shown}, such as {first}, which fetchloom never follows or imports",
            units::count(
                skipped.len() as u64,
                "link or special file",
                "links and special files"
            )
        ),
    };
    output.warn_detailed(Warning::ImportSkipped, &said, None, skipped);
}

/// says what an import left out of `walked`: links and special files, and what the system or a
/// tool keeps beside files, such as finder's `.DS_Store` and vim's swap files, and warns of each
/// exclude pattern that left nothing out.
pub(super) fn tell_left_out(output: &Output, shown: &str, walked: &Walked) {
    tell_skipped(output, shown, &walked.skipped);
    for (message, help) in &walked.unmatched {
        output.warn_with(Warning::SourceExcludeUnmatched, message, help);
    }
    let kept_beside = [
        (
            &walked.system,
            "which the system makes",
            "more the system makes",
        ),
        (
            &walked.dropped,
            "a working file of an editor or a tool",
            "more working files of editors and tools",
        ),
    ];
    for (paths, one, many) in kept_beside {
        let said = match paths.as_slice() {
            [] => continue,
            [path] => format!("{path} in {shown}, {one}"),
            [first, rest @ ..] => {
                let more = units::grouped(rest.len() as u64);
                format!("{first} and {more} {many} in {shown}")
            }
        };
        output.step("Skipped", &said);
    }
}

/// the longest path a file may have below the folder, in bytes, as an archive's members may.
const LONGEST_PATH: usize = 4096;

/// the path `typed` names, as [`local::is_path`] reads it: from `here`, from `home` for a leading
/// `~`, or as a `file:` link gives it, with links followed, since the person named it.
pub(super) fn located(typed: &str, here: &Path, home: Option<&Path>) -> Result<PathBuf, Error> {
    let typed = typed.trim();
    let path = if typed
        .get(..5)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("file:"))
    {
        url::Url::parse(typed)
            .ok()
            .and_then(|url| url.to_file_path().ok())
            .ok_or_else(|| {
                Error::new(
                    Kind::Usage,
                    format!(
                        "\"{}\" names no file on this machine",
                        crate::shown::mask_query(typed)
                    ),
                )
                .help(format!(
                    "give the path of the folder or file, as in `{}`",
                    super::project::command("add ./flowers")
                ))
            })?
    } else if let Some(rest) = typed.strip_prefix('~') {
        let rest = match rest.chars().next() {
            None => "",
            Some('/' | '\\') => &rest[1..],
            Some(_) => {
                return Err(Error::new(
                    Kind::Usage,
                    format!("\"{typed}\" names another user's home folder"),
                )
                .help("fetchloom reads ~ only as your own home folder, as in ~/flowers"));
            }
        };
        let home = home.ok_or_else(|| {
            Error::new(
                Kind::Usage,
                "fetchloom cannot tell where your home folder is",
            )
            .help("give the whole path instead of ~")
        })?;
        home.join(rest)
    } else {
        here.join(typed)
    };
    std::fs::canonicalize(&path).map_err(|err| {
        let line = super::TYPED.get().map_or(&[][..], Vec::as_slice);
        if let Some(refused) = refused_name(typed, &path, &err, line, cfg!(windows)) {
            return refused;
        }
        let shown = crate::shown::mask_query(typed);
        if err.kind() != std::io::ErrorKind::NotFound {
            return Error::io(format_args!("could not find {shown}"), &err);
        }
        // a path typed wrong is a mistake on the command line, not a fault of the disk
        let error = Error::new(
            Kind::Usage,
            format!("there is no file or folder \"{shown}\""),
        );
        match near(typed, &path) {
            Some(guess) => {
                let shown = super::meant(
                    &[typed],
                    &[&guess],
                    format!("fetchloom add {}", super::shell_word(&guess)),
                );
                error.help(format!("did you mean `{shown}`?"))
            }
            None if Path::new(typed).is_relative() && !typed.starts_with('~') => {
                error.help(format!(
                    "check the path: fetchloom reads {shown} from {}",
                    units::path(here)
                ))
            }
            None => error.help("check the path"),
        }
    })
}

/// the refusal of `typed`, a path given to add on the command line `line` that leads to `path`,
/// which windows, as `windows` says, could not open with `err` since a name in it holds a
/// character windows takes in none: a typing slip, not a fault of the disk.
fn refused_name(
    typed: &str,
    path: &Path,
    err: &std::io::Error,
    line: &[std::ffi::OsString],
    windows: bool,
) -> Option<Error> {
    if !windows || err.kind() != std::io::ErrorKind::InvalidFilename {
        return None;
    }
    let refused = super::typed::refused_in_name(path)?;
    Some(
        Error::new(
            Kind::Usage,
            format!(
                "there is no file or folder \"{}\": windows takes no {refused} in a name",
                crate::shown::mask_query(typed)
            ),
        )
        .help(super::typed::refused_help(
            refused,
            typed,
            line,
            "check the path",
        )),
    )
}

/// the most names [`near`] reads in one folder; more gets no guess rather than a slow one.
const MOST_NAMES: usize = 10_000;

/// `typed` with its last name changed to the one name in its folder a typo away from it, when
/// exactly one is. `path` is where `typed` leads, and only its folder is read.
pub(super) fn near(typed: &str, path: &Path) -> Option<String> {
    if typed
        .get(..5)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("file:"))
    {
        return None;
    }
    // a `\` where it is no separator, as in `.\flowers` typed on linux, is likelier a `/`
    if cfg!(not(windows))
        && typed.contains('\\')
        && let (Some(folder), Some(last)) = (path.parent(), path.file_name())
        && folder
            .join(last.to_string_lossy().replace('\\', "/"))
            .exists()
    {
        return Some(typed.replace('\\', "/"));
    }
    let kept = typed.trim_end_matches(['/', '\\']);
    let (folder, last) = kept.split_at(kept.rfind(['/', '\\']).map_or(0, |at| at + 1));
    if matches!(last, "" | "." | "..") {
        return None;
    }
    let entries: Vec<std::fs::DirEntry> = std::fs::read_dir(path.parent()?)
        .ok()?
        .take(MOST_NAMES + 1)
        .collect::<Result<_, _>>()
        .ok()?;
    if entries.len() > MOST_NAMES {
        return None;
    }
    let names: Vec<String> = entries
        .into_iter()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();
    let meant = name::sole_closest(last, names.iter().map(String::as_str))?;
    Some(format!("{folder}{meant}{}", &typed[kept.len()..]))
}

/// refuses to import `start`, run from `here`, where it overlaps what fetchloom keeps: a folder
/// holding the project, anything in its data folder `data_dir`, which placing writes, or the cache.
fn refuse_overlap(
    (start, shown): (&Path, &str),
    root: &Path,
    data_dir: &str,
    cache: &Path,
    here: &Path,
) -> Result<(), Error> {
    let real = |path: &Path| std::fs::canonicalize(path).ok();
    let overlap = if real(root).is_some_and(|root| root.starts_with(start)) {
        Some("holds this project")
    } else if real(&root.join(data_dir)).is_some_and(|data| start.starts_with(data)) {
        return Err(in_data_folder((start, shown), root, data_dir, here));
    } else if let Some(cache) = real(cache) {
        if start.starts_with(&cache) {
            Some("is in fetchloom's cache")
        } else if cache.starts_with(start) {
            Some("holds fetchloom's cache")
        } else {
            None
        }
    } else {
        None
    };
    match overlap {
        None => Ok(()),
        Some(overlap) => Err(
            Error::new(Kind::Usage, format!("{shown} {overlap}")).help(format!(
                "name a folder of data outside the project's data folder, as in `{}`",
                super::project::command("add ./raw")
            )),
        ),
    }
}

/// the refusal of `typed`, a word naming a file in the folder `here`, when that is in the data
/// folder of the project there, or of the one an add would start there. a folder there is pinned
/// where it is, as an output, so its word is refused as any folder's is.
pub(super) fn word_in_data_folder(typed: &str, here: &Path) -> Option<Error> {
    let project = Project::find(here).unwrap_or_else(|| Project::at(here.to_path_buf()));
    let data_dir = project
        .manifest()
        .map_or_else(|_| manifest::DATA_DIR.to_owned(), |(_, read)| read.data_dir);
    let start = std::fs::canonicalize(here.join(typed))
        .ok()
        .filter(|start| start.is_file())?;
    let data = std::fs::canonicalize(project.root.join(&data_dir)).ok()?;
    start
        .starts_with(data)
        .then(|| in_data_folder((&start, typed), &project.root, &data_dir, here))
}

/// the refusal of `shown`, `start`, a file inside `data_dir` of the project at `root`, seen from
/// `here`, whose help moves it out and adds it back, or sets the `data_dir` that keeps it as it
/// is. a folder there is pinned where it is, as an output, and never refused here.
fn in_data_folder(
    (start, shown): (&Path, &str),
    root: &Path,
    data_dir: &str,
    here: &Path,
) -> Error {
    let error = Error::new(
        Kind::Usage,
        format!("{shown} is in the data folder, where fetchloom places what data.lock pins"),
    );
    let keep = format!(
        "keep {data_dir}/ as it is by setting data_dir under [project] in data.toml to another folder"
    );
    // both as real paths: windows starts one with `\\?\`, which the current folder lacks, and
    // counting the folders between them needs the one inside the other
    let real = |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let (root, here) = (real(root), real(here));
    let manifest = Project::at(root.clone())
        .manifest()
        .ok()
        .map(|(_, manifest)| manifest);
    if let Some(manifest) = &manifest
        && let Some(owner) = owner(start, &root, manifest)
    {
        return error.help(format!(
            "it is part of {owner} already, so there is nothing to add; `{}` prints where {owner} is",
            super::project::command(&format!("path {owner}"))
        ));
    }
    let named = start
        .file_name()
        .and_then(|own| own.to_str())
        .zip(dataset_name(start, true));
    let Some((own, name)) = named else {
        return error.help(format!("move what you keep there out of it, or {keep}"));
    };
    // the name data.toml has already would make the add take the next free one
    let taken = |name: &str| {
        manifest
            .as_ref()
            .is_some_and(|read| read.datasets.contains_key(name))
    };
    let name = std::iter::once(name.clone())
        .chain((2..100).map(|number| format!("{name}-{number}")))
        .find(|name| !taken(name))
        .unwrap_or(name);
    // a name the project's folder holds already would take the files in rather than be theirs
    let Some(out) = (1..100)
        .map(|number| match number {
            1 => own.to_owned(),
            _ => format!("{own}-{number}"),
        })
        .find(|out| std::fs::symlink_metadata(root.join(out)).is_err())
    else {
        return error.help(format!("move it out of {data_dir}/ first, or {keep}"));
    };
    let up = super::up_to(&root, &here);
    let moved = if up.is_empty() {
        format!("./{out}")
    } else {
        format!("{up}{out}")
    };
    let renamed = if dataset_name(&root.join(&out), true).as_deref() == Some(&name) {
        String::new()
    } else {
        format!(" --as {name}")
    };
    error.help(format!(
        "move it out first, with {}, then run `{}` to place its files in {up}{data_dir}/{name}, or {keep}",
        crate::suggested::moving(shown.trim(), moved.strip_prefix("./").unwrap_or(&moved)),
        super::project::command(&format!("add {}{renamed}", super::shell_word(&moved)))
    ))
}

/// the dataset of `manifest` whose file at `start` data.lock pins: an add of `start` would bring
/// in what that dataset holds already.
fn owner(start: &Path, root: &Path, manifest: &Manifest) -> Option<String> {
    let lock = Project::at(root.to_path_buf()).lock().ok().flatten();
    manifest
        .datasets
        .keys()
        .find(|name| {
            let Ok(folder) = std::fs::canonicalize(root.join(plan::folder(manifest, name))) else {
                return false;
            };
            let Ok(inside) = start.strip_prefix(&folder) else {
                return false;
            };
            let inside = inside.to_string_lossy().replace('\\', "/");
            lock.as_ref()
                .and_then(|lock| lock.get(name))
                .is_some_and(|locked| locked.files.iter().any(|file| file.path == inside))
        })
        .cloned()
}

/// the name a dataset imported from `start` takes without `--as`: the folder's, or the file's
/// without its endings, as the data folder would show it.
pub(super) fn dataset_name(start: &Path, is_file: bool) -> Option<String> {
    let own = start.file_name()?.to_str()?;
    if is_file {
        name::dataset_name(&name::folder_for(own))
    } else {
        name::dataset_name(own)
    }
}

/// a plain file found below the folder an import names.
#[derive(Debug)]
pub(super) struct Found {
    /// where the file goes in the dataset: NFC, with `/` between folders.
    pub(super) path: String,
    /// the file on this machine.
    source: PathBuf,
    pub(super) size: u64,
    /// how it looked when found.
    seen: Seen,
}

/// how a file looked when a walk found it, which a later look must match for the file to be the
/// one read: its size and time, and on unix its inode and the time its metadata last changed,
/// which a rewrite keeping both still moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Seen {
    look: crate::project::state::Look,
    node: (u64, i64, i64),
    writable: bool,
}

impl Seen {
    fn of(metadata: &std::fs::Metadata) -> Seen {
        #[cfg(unix)]
        let node = {
            use std::os::unix::fs::MetadataExt;
            (metadata.ino(), metadata.ctime(), metadata.ctime_nsec())
        };
        #[cfg(not(unix))]
        let node = (0, 0, 0);
        Seen {
            look: crate::project::state::Look::of(metadata),
            node,
            writable: !metadata.permissions().readonly(),
        }
    }
}

/// the files a walk found to import, sorted by path, and the links and special files it left
/// out, by their path.
#[derive(Debug, Default)]
pub(super) struct Walked {
    /// the folder every file is opened below: the folder imported, or the one holding the file.
    root: PathBuf,
    pub(super) files: Vec<Found>,
    pub(super) skipped: Vec<String>,
    /// what the system writes into folders by itself, such as finder's `.DS_Store`, left out.
    pub(super) system: Vec<String>,
    /// what tools keep beside the files they work on, such as vim's swap files, left out.
    pub(super) dropped: Vec<String>,
    /// the warning, and its help, for each exclude pattern that matches no file found.
    unmatched: Vec<(String, String)>,
}

impl Walked {
    pub(super) fn bytes(&self) -> u64 {
        self.files
            .iter()
            .fold(0, |total, file| total.saturating_add(file.size))
    }
}

/// the plain files at `start`, a folder or one file with no link in its path, that `source`'s
/// patterns keep, each checked by name, at most `most` of them. repository metadata is left out
/// unseen, and links and special files are never followed but named in [`Walked::skipped`].
pub(super) fn walk(
    start: &Path,
    shown: &str,
    source: &Source,
    most: usize,
    stop: &Stop,
) -> Result<Walked, Error> {
    let found = std::fs::metadata(start)
        .map_err(|err| Error::io(format_args!("could not read {shown}"), &err))?;
    let mut walked = Walked {
        root: start.to_path_buf(),
        ..Walked::default()
    };
    if found.is_dir() {
        let mut folders = vec![(String::new(), start.to_path_buf())];
        while let Some((folder, path)) = folders.pop() {
            stop.check()?;
            let entries = std::fs::read_dir(&path).map_err(|err| {
                Error::io(format_args!("could not read {}", path.display()), &err)
            })?;
            for listed in entries {
                let listed = listed.map_err(|err| {
                    Error::io(format_args!("could not read {}", path.display()), &err)
                })?;
                let file_name = listed.file_name();
                let Some(file_name) = file_name.to_str() else {
                    return Err(refused(
                        shown,
                        &name::joined(&folder, &file_name.to_string_lossy()),
                        "it is not text every machine can read",
                    ));
                };
                if name::metadata(file_name).is_some() {
                    continue;
                }
                let relative = name::joined(&folder, file_name);
                let Some(relative) = by_name(&mut walked, file_name, relative) else {
                    continue;
                };
                let kind = listed.file_type().map_err(|err| {
                    Error::io(
                        format_args!("could not read {}", listed.path().display()),
                        &err,
                    )
                })?;
                if kind.is_symlink() {
                    walked.skipped.push(relative);
                } else if kind.is_dir() {
                    folders.push((relative, listed.path()));
                } else {
                    let metadata = listed.metadata().map_err(|err| {
                        Error::io(
                            format_args!("could not read {}", listed.path().display()),
                            &err,
                        )
                    })?;
                    if is_plain(&metadata) {
                        walked.files.push(Found {
                            path: crate::name::composed(&relative),
                            source: listed.path(),
                            size: metadata.len(),
                            seen: Seen::of(&metadata),
                        });
                    } else {
                        walked.skipped.push(relative);
                    }
                }
            }
            // a folder of more files than a dataset may list is refused before it is all read
            if walked.files.len() > most {
                return Err(source::too_many(
                    shown,
                    most,
                    "import a smaller folder, or leave files out with --exclude",
                ));
            }
        }
    } else if is_plain(&found) {
        let file_name = start
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| refused(shown, shown, "its name is not text every machine can read"))?;
        walked.files.push(Found {
            path: crate::name::composed(file_name),
            source: start.to_path_buf(),
            size: found.len(),
            seen: Seen::of(&found),
        });
        walked.root = start.parent().unwrap_or(start).to_path_buf();
    } else {
        return Err(Error::new(
            Kind::Usage,
            format!("{shown} is no folder or plain file, so there is nothing to import"),
        )
        .help(format!(
            "name a folder or a file, as in `{}`",
            super::project::command("add ./flowers")
        )));
    }
    walked.skipped.sort_unstable();
    walked.system.sort_unstable();
    walked.dropped.sort_unstable();
    kept(&mut walked, source, shown)?;
    walked.files.sort_unstable_by(|a, b| a.path.cmp(&b.path));
    checked_names(&walked.files, shown)?;
    Ok(walked)
}

/// `relative`, the path of `file_name`, unless its name leaves it out as the system's or a tool's
/// own, which `walked` then notes as such.
fn by_name(walked: &mut Walked, file_name: &str, relative: String) -> Option<String> {
    if name::made_by_system(file_name) {
        walked.system.push(relative);
    } else if name::made_by_tool(file_name) {
        walked.dropped.push(relative);
    } else {
        return Some(relative);
    }
    None
}

/// keeps the files `source`'s select and exclude patterns choose. a select pattern that keeps
/// nothing is a mistake worth stopping for.
fn kept(walked: &mut Walked, source: &Source, shown: &str) -> Result<(), Error> {
    let matches = |globs: &[Glob], path: &str| globs.iter().any(|glob| glob.matches(path));
    if let Some(glob) = source
        .select
        .iter()
        .find(|glob| !walked.files.iter().any(|file| glob.matches(&file.path)))
    {
        return Err(Error::new(
            Kind::Usage,
            format!("--select \"{}\" keeps no file of {shown}", glob.as_str()),
        )
        .help("check the pattern against the paths below the folder, such as \"**/*.csv\""));
    }
    let unpacks = |path: &str| {
        archive::by_name(path) == Some(archive::Packed::Archive) && !matches(&source.exclude, path)
    };
    if !(source.extract && walked.files.iter().any(|file| unpacks(&file.path))) {
        let paths = || walked.files.iter().map(|file| file.path.as_str());
        walked.unmatched = source
            .exclude
            .iter()
            .filter(|glob| !paths().any(|path| glob.matches(path)))
            .map(|glob| {
                let named = format!("--exclude \"{}\"", glob.as_str());
                super::sync::unmatched_exclude(&named, glob, shown, paths)
            })
            .collect();
    }
    walked.files.retain(|file| {
        (source.select.is_empty() || matches(&source.select, &file.path))
            && !matches(&source.exclude, &file.path)
    });
    if walked.files.is_empty() {
        return Err(Error::new(
            Kind::Usage,
            format!("{shown} holds no plain file to import"),
        )
        .help("name a folder that holds files, or loosen --select and --exclude"));
    }
    Ok(())
}

/// refuses a name data.lock could not pin for every machine, as an archive's member is refused,
/// including one that is another's name on disks that ignore case or unicode composition.
fn checked_names(files: &[Found], shown: &str) -> Result<(), Error> {
    let mut seen: HashMap<String, (&str, bool)> = HashMap::new();
    for file in files {
        let path = file.path.as_str();
        if path.len() > LONGEST_PATH {
            let start: String = path.chars().take(60).collect();
            return Err(refused(
                shown,
                &format!("{start}…"),
                &format!(
                    "its path is longer than {}",
                    units::count(LONGEST_PATH as u64, "byte", "bytes")
                ),
            ));
        }
        if let Some(reason) = name::escapes(path) {
            return Err(refused(shown, &path.escape_debug().to_string(), reason));
        }
        if let Some(reason) = name::unstorable(path) {
            return Err(refused(shown, path, &reason));
        }
        let ends = path.match_indices('/').map(|(end, _)| end);
        for (end, is_folder) in ends.map(|end| (end, true)).chain([(path.len(), false)]) {
            let named = &path[..end];
            match seen.entry(name::fold(named)) {
                Slot::Vacant(free) => {
                    free.insert((named, is_folder));
                }
                Slot::Occupied(taken) => {
                    let (other, was_folder) = *taken.get();
                    if other == named && was_folder && is_folder {
                        continue;
                    }
                    let problem = if other != named && name::same_to_hfs(other, named) {
                        format!(
                            "{shown} holds \"{}\" and \"{}\", one name on {}",
                            name::spelled(other),
                            name::spelled(named),
                            name::ON_HFS
                        )
                    } else if other != named {
                        format!(
                            "{shown} holds \"{other}\" and \"{named}\", which are one name on disks that ignore case"
                        )
                    } else if was_folder || is_folder {
                        format!("{shown} holds \"{named}\" both as a file and as a folder")
                    } else {
                        // a folder holds no name twice, so these two differed until composed
                        format!(
                            "{shown} holds two files named \"{named}\", their unicode written two ways, which macos disks see as one name"
                        )
                    };
                    return Err(Error::new(Kind::SourceInvalid, problem)
                        .help("rename one of them, or leave one out with --exclude"));
                }
            }
        }
    }
    Ok(())
}

fn refused(shown: &str, path: &str, reason: &str) -> Error {
    Error::new(
        Kind::SourceInvalid,
        format!("{shown} holds \"{path}\", which fetchloom refuses because {reason}"),
    )
    .help("rename it, or leave it out with --exclude")
}

/// refuses, before any byte is copied, a file too large for the cache's disk, as on FAT32, and,
/// unless the person said yes, more than it has free, which a disk that clones would not need.
pub(super) fn room(
    context: &Context,
    cache: &Cache,
    walked: &Walked,
    shown: &str,
) -> Result<(), Error> {
    crate::space::cache_holds(
        cache.root(),
        walked
            .files
            .iter()
            .map(|file| (file.path.as_str(), file.size)),
    )?;
    if context.yes {
        return Ok(());
    }
    let needed = walked.bytes();
    let free = crate::space::free(cache.root());
    if crate::space::short(needed, free).is_none() {
        return Ok(());
    }
    super::ask_room(
        &context.output,
        &format!("importing {shown}"),
        needed,
        free.unwrap_or_default(),
        cache.root(),
    )
}

/// what reading the folder `shown` did: its files and bytes, copied or cloned into the cache, or
/// held there already, and the folder left where it is, never moved or changed.
fn read_line(walked: &Walked, copied: &Copied, shown: &str) -> String {
    let how = match (copied.new, copied.cloned) {
        (false, _) => "the cache held the bytes already",
        (true, true) => "cloned into the cache",
        (true, false) => "copied into the cache",
    };
    format!(
        "{} · {}, {how}; {shown} stays where it is",
        units::count(walked.files.len() as u64, "file", "files"),
        units::size(walked.bytes())
    )
}

/// the files copied into the cache, in the order found, pinned with no link: no source has them.
pub(super) struct Copied {
    pub(super) files: Vec<LockedFile>,
    /// whether every file is a clone sharing its blocks, which takes no room of its own.
    pub(super) cloned: bool,
    /// whether the cache held none of the files' bytes before.
    pub(super) new: bool,
}

/// copies every file `walked` found into the cache and pins each. none is in the cache under its
/// hash until every one is on disk and `unchanged` passes, given the digests of the copies in the
/// files' order, so a failure, a stop or a change seen meanwhile leaves none of them there.
fn into_cache(
    context: &Context,
    cache: &Cache,
    walked: &Walked,
    unchanged: &dyn Fn(&[Sha256]) -> Result<(), Error>,
) -> Result<Copied, Error> {
    let found = &walked.files;
    // many objects flush together, since one flush each costs far more than writing a small file
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let batch = if found.len() > crate::volume::MOST_FLUSHED_ALONE {
        cache
            .batch()
            .map_err(|err| Error::io("could not open the cache to flush what is imported", &err))?
    } else {
        None
    };
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let flush_each = batch.is_none();
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let flush_each = true;
    let Sealing { sealed, cloned } = sealed(context, cache, walked, flush_each)?;
    context.stop.check()?;
    let copies: Vec<Sha256> = sealed
        .iter()
        .filter_map(|slot| {
            let slot = slot.lock().unwrap_or_else(PoisonError::into_inner);
            slot.as_ref().map(|sealed| sealed.digest)
        })
        .collect();
    unchanged(&copies)?;
    // every file is copied, and the disk still has to keep them before they are in the cache
    let finishing = context.output.short_wait(
        "Finishing",
        units::count(found.len() as u64, "file", "files"),
    );
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if let Some(batch) = batch {
        batch
            .flush()
            .map_err(|err| Error::io("could not flush the imported files to disk", &err))?;
    }
    let published = place::run_all(&sealed, context.threads, |slot| {
        let sealed: Option<Sealed> = slot.lock().unwrap_or_else(PoisonError::into_inner).take();
        sealed.map(|sealed| sealed.publish(cache))
    });
    drop(finishing);
    let mut files = Vec::with_capacity(found.len());
    let mut new = false;
    for (file, stored) in found.iter().zip(published) {
        let stored = stored
            .ok_or_else(|| Error::new(Kind::LocalIo, "importing stopped unexpectedly"))?
            .map_err(|err| {
                Error::io(
                    format_args!("could not put {} in the cache", file.path),
                    &cache.explained(&cache.root().join("objects"), err),
                )
            })?;
        if stored.new {
            context.work.wrote(&stored.digest);
            new = true;
        }
        files.push(LockedFile {
            path: file.path.clone(),
            size: stored.size,
            sha256: stored.digest,
            md5: None,
            url: String::new(),
            unpacks_to: None,
        });
    }
    let digests: Vec<_> = files.iter().map(|file| file.sha256).collect();
    cache
        .mark_imported(&digests)
        .map_err(|err| Error::io("could not record the imported files in the cache", &err))?;
    Ok(Copied { files, cloned, new })
}

/// a copy of each file an import found, sealed in a staging folder of the cache and waiting to be
/// put in place under its hash, in the files' order.
struct Sealing {
    /// each taken once to be put in place.
    sealed: Vec<Mutex<Option<Sealed>>>,
    cloned: bool,
}

/// [`Sealing`] for the files `walked` found. a failure stops the files not begun yet.
fn sealed(
    context: &Context,
    cache: &Cache,
    walked: &Walked,
    flush_each: bool,
) -> Result<Sealing, Error> {
    let found = &walked.files;
    #[cfg(any(unix, windows))]
    let beneath = Beneath::of(&walked.root).map_err(|err| {
        Error::io(
            format_args!("could not read {}", walked.root.display()),
            &err,
        )
    })?;
    // a folder swapped for a link or a junction while the files are read is refused, not read
    // through
    #[cfg(any(unix, windows))]
    let open = |file: &Found| beneath.open(below(&walked.root, file)?);
    #[cfg(not(any(unix, windows)))]
    let open = |file: &Found| {
        below(&walked.root, file)?;
        crate::volume::read_unfollowed(&file.source)
    };
    let staging = cache.staging().map_err(|err| {
        Error::io(
            format_args!(
                "could not make a folder to import into, in {}",
                cache.root().join("tmp").display()
            ),
            &err,
        )
    })?;
    let what = units::count(found.len() as u64, "file", "files");
    let bar = context.output.short_bar("Copying", what.clone());
    bar.start(Some(walked.bytes()), 0);
    let ticker = context
        .output
        .ticker("Copying", what, found.iter().map(|file| Some(file.size)));
    let failed = AtomicBool::new(false);
    let stop = &context.stop;
    let import = |file: &Found| {
        let imported =
            open(file).and_then(|source| cache.import_file(source, &staging, flush_each, stop));
        bar.advance(file.size);
        ticker.advance(file.size);
        imported
    };
    // files that wait for one flush take as many workers as their bytes repay, as hashing does
    // elsewhere, and on linux files flushed one by one take a few, since each waits on the disk;
    // none past a failure starts and every one before it finishes, so the first failure in the
    // files' order is the one told, as when they run in turn
    let imported = if !flush_each {
        let _span = tracing::debug_span!("import_by_bytes", files = found.len()).entered();
        place::run_hashes_until_failure(found, walked.bytes(), context.threads, import)
    } else if cfg!(target_os = "linux") {
        place::run_flushing_until_failure(found, context.threads, import)
    } else {
        place::run_all(found, context.threads, |file| {
            if failed.load(Ordering::Relaxed) {
                return None;
            }
            let imported = import(file);
            if imported.is_err() {
                failed.store(true, Ordering::Relaxed);
            }
            Some(imported)
        })
    };
    drop((bar, ticker));
    let mut sealed = Vec::with_capacity(found.len());
    let mut cloned = true;
    for (file, imported) in found.iter().zip(imported) {
        match imported {
            Some(Ok(crate::cache::Imported {
                sealed: one,
                cloned: shared,
            })) => {
                cloned &= shared;
                sealed.push(Mutex::new(Some(one)));
            }
            Some(Err(err)) if err.kind() == std::io::ErrorKind::Interrupted => {
                return Err(Stop::stopped());
            }
            Some(Err(err)) => {
                return Err(Error::io(
                    format_args!("could not copy {} into the cache", file.source.display()),
                    &err,
                ));
            }
            None => {}
        }
    }
    Ok(Sealing { sealed, cloned })
}

fn below<'f>(root: &Path, file: &'f Found) -> std::io::Result<&'f Path> {
    file.source
        .strip_prefix(root)
        .map_err(|_| std::io::Error::other("a file outside the folder imported"))
}

/// the folder an import reads, open, so each file is opened below it one name at a time and never
/// through a link, wherever a link appears meanwhile.
#[cfg(unix)]
struct Beneath(std::fs::File);

#[cfg(unix)]
impl Beneath {
    fn of(folder: &Path) -> std::io::Result<Beneath> {
        std::fs::File::open(folder).map(Beneath)
    }

    fn open(&self, relative: &Path) -> std::io::Result<std::fs::File> {
        use std::os::fd::AsFd as _;

        use rustix::fs::{Mode, OFlags};

        let mut names = relative.components().peekable();
        let mut folder: Option<std::os::fd::OwnedFd> = None;
        while let Some(component) = names.next() {
            let std::path::Component::Normal(name) = component else {
                return Err(unimportable());
            };
            let at = folder
                .as_ref()
                .map_or_else(|| self.0.as_fd(), |folder| folder.as_fd());
            let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
            if names.peek().is_some() {
                let next = rustix::fs::openat(at, name, flags | OFlags::DIRECTORY, Mode::empty())
                    .map_err(|err| match err {
                    rustix::io::Errno::LOOP | rustix::io::Errno::NOTDIR => unimportable(),
                    err => err.into(),
                })?;
                folder = Some(next);
            } else {
                // a fifo must not block opening before it can be refused
                let file = rustix::fs::openat(at, name, flags | OFlags::NONBLOCK, Mode::empty())
                    .map_err(|err| match err {
                        rustix::io::Errno::LOOP => unimportable(),
                        err => err.into(),
                    })?;
                let file = std::fs::File::from(file);
                return if is_plain(&file.metadata()?) {
                    Ok(file)
                } else {
                    Err(unimportable())
                };
            }
        }
        Err(unimportable())
    }
}

/// the folder an import reads, held, and each folder below it held once a file in it is opened,
/// until the import ends: each is opened as itself, never through a link or a junction, and while
/// held no one renames or replaces it, so no file is reached through one that appears meanwhile.
#[cfg(windows)]
struct Beneath {
    root: PathBuf,
    _held: std::fs::File,
    /// the folders below `root` held so far, by their path below it.
    folders: Mutex<HashMap<PathBuf, std::fs::File>>,
}

#[cfg(windows)]
impl Beneath {
    fn of(folder: &Path) -> std::io::Result<Beneath> {
        Ok(Beneath {
            root: folder.to_path_buf(),
            _held: hold(folder, true)?,
            folders: Mutex::default(),
        })
    }

    fn open(&self, relative: &Path) -> std::io::Result<std::fs::File> {
        let mut names = relative.components().peekable();
        let (mut at, mut inside) = (self.root.clone(), PathBuf::new());
        while let Some(component) = names.next() {
            let std::path::Component::Normal(name) = component else {
                return Err(unimportable());
            };
            at.push(name);
            if names.peek().is_none() {
                return crate::volume::read_unfollowed(&at);
            }
            inside.push(name);
            let mut folders = self.folders.lock().unwrap_or_else(PoisonError::into_inner);
            if !folders.contains_key(&inside) {
                let folder = hold(&at, false)?;
                folders.insert(inside.clone(), folder);
            }
        }
        Err(unimportable())
    }
}

/// `folder` open and held so no one renames or removes it: opened as itself unless `follow`, and
/// refused when it is a link, a junction or no folder.
#[cfg(windows)]
fn hold(folder: &Path, follow: bool) -> std::io::Result<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt as _;
    // listing access records the handle's sharing, which reading attributes alone does not, so
    // leaving out FILE_SHARE_DELETE then refuses a rename or a removal of the folder
    const FILE_LIST_DIRECTORY: u32 = 0x1;
    const FILE_READ_ATTRIBUTES: u32 = 0x80;
    const FILE_SHARE_READ_WRITE: u32 = 0x3;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    let flags = if follow {
        FILE_FLAG_BACKUP_SEMANTICS
    } else {
        FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT
    };
    let held = std::fs::OpenOptions::new()
        .access_mode(FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ_WRITE)
        .custom_flags(flags)
        .open(folder)?;
    let found = held.metadata()?;
    if found.file_type().is_symlink() || !found.is_dir() {
        return Err(unimportable());
    }
    Ok(held)
}

/// the error for a file an import will not read: one reached through a link, or a link or a
/// special file itself.
#[cfg(any(unix, windows))]
fn unimportable() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        "it is reached through a link, or is a link or a special file, which fetchloom never imports",
    )
}

/// the pin of `files` imported for `name`, naming no source to fetch from. relocking learns what
/// its archives unpack to and stamps when it was pinned.
pub(super) fn pinned(name: &str, source: &Source, files: Vec<LockedFile>) -> Locked {
    Locked {
        name: name.to_owned(),
        reference: local::IMPORTED.to_owned(),
        spec: lock::spec(source),
        at: None,
        extract: source.extract,
        decompress: source.decompress,
        web_page: false,
        resolved: local::IMPORTED.to_owned(),
        title: None,
        license: None,
        doi: None,
        access: Access::Unknown,
        timestamp: String::new(),
        files,
    }
}

/// the first way `found` differs from `pinned`, both sorted by path: a file only one has, or
/// another size. none when they may hold the same bytes, which only hashing tells.
fn first_difference(found: &[Found], pinned: &[LockedFile]) -> Option<String> {
    let mut found = found.iter().peekable();
    let mut pinned = pinned.iter().peekable();
    loop {
        return match (found.peek(), pinned.peek()) {
            (None, None) => None,
            (Some(file), None) => Some(format!("{} is not in data.lock", file.path)),
            (None, Some(pin)) => Some(format!("{} is missing", pin.path)),
            (Some(file), Some(pin)) if file.path < pin.path => {
                Some(format!("{} is not in data.lock", file.path))
            }
            (Some(file), Some(pin)) if file.path > pin.path => {
                Some(format!("{} is missing", pin.path))
            }
            (Some(file), Some(pin)) if file.size != pin.size => Some(format!(
                "{} has {}, and data.lock pins {}",
                file.path,
                units::count(file.size, "byte", "bytes"),
                units::count(pin.size, "byte", "bytes")
            )),
            _ => {
                found.next();
                pinned.next();
                continue;
            }
        };
    }
}

/// the first file of `copied` that holds other bytes than `pinned` pins at its path, both sorted
/// by path and alike in paths and sizes.
fn first_other_bytes(copied: &[LockedFile], pinned: &[LockedFile]) -> Option<String> {
    copied
        .iter()
        .zip(pinned)
        .find(|(file, pin)| file.path != pin.path || file.sha256 != pin.sha256)
        .map(|(file, _)| format!("{} holds other bytes than data.lock pins", file.path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::manifest::Manifest;

    /// on windows a path holding a character windows takes in no name is a typing slip, exit 2,
    /// not a fault of the disk; a `"` there is what a backslash before a closing quote leaves, with
    /// the words after it, so the help gives the line meant. elsewhere such a name is a name.
    #[test]
    fn a_name_windows_refuses_is_a_slip_on_the_command_line() {
        let invalid = std::io::Error::from(std::io::ErrorKind::InvalidFilename);
        let root = std::path::absolute("/").unwrap();
        let typed = root.join("lab").join("my data\" --dry-run");
        let typed = typed.to_string_lossy();
        let line: Vec<std::ffi::OsString> = ["add", &*typed].iter().map(Into::into).collect();
        let error = refused_name(&typed, Path::new(&*typed), &invalid, &line, true).unwrap();
        assert_eq!(error.kind, Kind::Usage);
        assert_eq!(
            error.message,
            format!("there is no file or folder \"{typed}\": windows takes no \" in a name")
        );
        let meant =
            crate::suggested::shell_word(&root.join("lab").join("my data").to_string_lossy())
                .into_owned();
        assert_eq!(
            error.help.as_deref(),
            Some(
                format!(
                    "a backslash right before a closing quote, as in \"C:\\my data\\\", keeps the quote in the path, with the words after it: did you mean `fetchloom add {meant} --dry-run`?"
                )
                .as_str()
            )
        );
        let starred = root.join("lab").join("a*b");
        let starred = starred.to_string_lossy();
        let error = refused_name(&starred, Path::new(&*starred), &invalid, &line, true).unwrap();
        assert_eq!(error.help.as_deref(), Some("check the path"));
        assert!(refused_name(&typed, Path::new(&*typed), &invalid, &line, false).is_none());
    }

    /// the current folder spelled otherwise than the project's real path, as windows spells one
    /// without the `\\?\` its real path has, still counts its folders up to the project: a
    /// file is never to be moved onto itself.
    #[cfg(unix)]
    #[test]
    fn a_folder_spelled_another_way_still_counts_its_way_up() {
        let folder = tempfile::tempdir().unwrap();
        let work = folder.path().join("work");
        std::fs::create_dir_all(work.join("data").join("raw")).unwrap();
        std::fs::write(work.join("data.toml"), "").unwrap();
        std::fs::write(work.join("data").join("raw").join("visits.csv"), "id\n").unwrap();
        let alias = folder.path().join("alias");
        std::os::unix::fs::symlink(&work, &alias).unwrap();
        let here = alias.join("data").join("raw");
        let start = std::fs::canonicalize(here.join("visits.csv")).unwrap();
        let error = in_data_folder((&start, "./visits.csv"), &alias, "data", &here);
        let help = error.help.unwrap_or_default();
        assert!(
            help.starts_with("move it out first, with `mv ./visits.csv ../../visits.csv`, then run `fetchloom add ../../visits.csv` to place its files in ../../data/visits,"),
            "{help}"
        );
    }

    fn entry(select: &[&str], exclude: &[&str]) -> Source {
        let quoted = |patterns: &[&str]| {
            patterns
                .iter()
                .map(|pattern| format!("\"{pattern}\""))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let text = format!(
            "[datasets.flowers]\nref = \"local\"\nselect = [{}]\nexclude = [{}]\n",
            quoted(select),
            quoted(exclude)
        );
        Manifest::parse(&text).unwrap().datasets["flowers"]
            .source
            .clone()
    }

    #[test]
    fn a_missing_path_names_the_one_near_name_in_its_folder() {
        let dir = tempfile::tempdir().unwrap();
        let here = dir.path();
        std::fs::create_dir_all(here.join("my data/deeper")).unwrap();
        for file in ["my data/flowers.csv", "iris-a.csv", "iris-b.csv"] {
            std::fs::write(here.join(file), b"x").unwrap();
        }
        let help = |typed: &str| {
            let error = located(typed, here, Some(here)).unwrap_err();
            assert_eq!(error.kind, Kind::Usage, "{typed}");
            assert_eq!(
                error.message,
                format!("there is no file or folder \"{typed}\"")
            );
            error.help.map(String::from)
        };
        let unguessed = |typed: &str| {
            Some(format!(
                "check the path: fetchloom reads {typed} from {}",
                units::path(here)
            ))
        };
        assert_eq!(
            help("./my data/flower.csv").as_deref(),
            Some("did you mean `fetchloom add \"./my data/flowers.csv\"`?")
        );
        assert_eq!(
            help("~/my dta/").as_deref(),
            Some("did you mean `fetchloom add \"~/my data/\"`?")
        );
        assert_eq!(
            help("./iris-c.csv"),
            unguessed("./iris-c.csv"),
            "two names are as near"
        );
        assert_eq!(
            help("./nowhere/flowers.csv"),
            unguessed("./nowhere/flowers.csv"),
            "its folder is missing"
        );
        assert_eq!(
            help("./my data/deeper/flowers.csv"),
            unguessed("./my data/deeper/flowers.csv"),
            "no folder above is read"
        );
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(here.join("gone"), here.join("notes.txt")).unwrap();
            std::fs::write(here.join("notes.tx"), b"x").unwrap();
            assert_eq!(
                help("./notes.txt"),
                unguessed("./notes.txt"),
                "the name is there, as a broken link"
            );
        }
    }

    fn paths(walked: &Walked) -> Vec<&str> {
        walked.files.iter().map(|file| file.path.as_str()).collect()
    }

    #[test]
    fn a_walk_pins_plain_files_by_path_and_leaves_repository_metadata_out() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("flowers");
        std::fs::create_dir_all(root.join("petals/red")).unwrap();
        std::fs::create_dir_all(root.join(".git/objects")).unwrap();
        std::fs::write(root.join("sepals.csv"), b"a,b\n").unwrap();
        std::fs::write(root.join("petals/red/rose.csv"), b"rose\n").unwrap();
        std::fs::write(root.join("petals/notes.txt"), b"notes\n").unwrap();
        std::fs::write(root.join(".git/HEAD"), b"ref\n").unwrap();
        std::fs::write(root.join(".gitignore"), b"*.tmp\n").unwrap();
        let walked = walk(
            &root,
            "./flowers",
            &entry(&[], &[]),
            crate::net::MOST_FILES,
            &Stop::default(),
        )
        .unwrap();
        assert_eq!(
            paths(&walked),
            [
                ".gitignore",
                "petals/notes.txt",
                "petals/red/rose.csv",
                "sepals.csv"
            ]
        );
        assert_eq!(walked.bytes(), 4 + 5 + 6 + 6);
        let csv = walk(
            &root,
            "./flowers",
            &entry(&["**/*.csv"], &["sepals.csv"]),
            crate::net::MOST_FILES,
            &Stop::default(),
        )
        .unwrap();
        assert_eq!(paths(&csv), ["petals/red/rose.csv"]);
        let nothing = walk(
            &root,
            "./flowers",
            &entry(&["*.parquet"], &[]),
            crate::net::MOST_FILES,
            &Stop::default(),
        )
        .unwrap_err();
        assert_eq!(nothing.kind, Kind::Usage);
        assert_eq!(
            nothing.message,
            "--select \"*.parquet\" keeps no file of ./flowers"
        );
        let one = walk(
            &root.join("sepals.csv"),
            "./sepals.csv",
            &entry(&[], &[]),
            crate::net::MOST_FILES,
            &Stop::default(),
        )
        .unwrap();
        assert_eq!(paths(&one), ["sepals.csv"]);
    }

    #[cfg(unix)]
    #[test]
    fn links_inside_an_imported_folder_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), b"keep out\n").unwrap();
        let root = dir.path().join("flowers");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("iris.csv"), b"iris\n").unwrap();
        std::os::unix::fs::symlink(outside.join("secret.txt"), root.join("secret.txt")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("more")).unwrap();
        // rustix makes no fifo on macos, and every unix has mkfifo
        let made = std::process::Command::new("mkfifo")
            .arg(root.join("queue"))
            .status()
            .unwrap();
        assert!(made.success());
        let walked = walk(
            &root,
            "./flowers",
            &entry(&[], &[]),
            crate::net::MOST_FILES,
            &Stop::default(),
        )
        .unwrap();
        assert_eq!(paths(&walked), ["iris.csv"]);
        assert_eq!(walked.skipped, ["more", "queue", "secret.txt"]);
    }

    /// the files left out are named, and a folder of them, as `__MACOSX`, is not read.
    #[test]
    fn files_the_system_makes_are_left_out() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("flowers");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::create_dir_all(root.join("__MACOSX")).unwrap();
        for (path, bytes) in [
            ("iris.csv", &b"iris\n"[..]),
            ("sub/petals.csv", b"petals\n"),
            (".DS_Store", b"finder"),
            ("sub/.DS_Store", b"finder"),
            ("._iris.csv", b"appledouble"),
            ("Thumbs.db", b"thumbs"),
            ("__MACOSX/._iris.csv", b"appledouble"),
        ] {
            std::fs::write(root.join(path), bytes).unwrap();
        }
        let walked = walk(
            &root,
            "./flowers",
            &entry(&[], &[]),
            crate::net::MOST_FILES,
            &Stop::default(),
        )
        .unwrap();
        assert_eq!(paths(&walked), ["iris.csv", "sub/petals.csv"]);
        assert_eq!(
            walked.system,
            [
                ".DS_Store",
                "._iris.csv",
                "Thumbs.db",
                "__MACOSX",
                "sub/.DS_Store"
            ]
        );
        assert_eq!(walked.skipped, Vec::<String>::new());
    }

    /// what an import must leave as it was: a file's bytes, length and modified time, and on
    /// unix its inode and mode, which a hardlink or a sealed copy would change.
    fn look(path: &Path) -> (Vec<u8>, u64, std::time::SystemTime, u64, u32) {
        let metadata = std::fs::symlink_metadata(path).unwrap();
        #[cfg(unix)]
        let (inode, mode) = {
            use std::os::unix::fs::MetadataExt;
            (metadata.ino(), metadata.mode())
        };
        #[cfg(not(unix))]
        let (inode, mode) = (0, 0);
        (
            std::fs::read(path).unwrap(),
            metadata.len(),
            metadata.modified().unwrap(),
            inode,
            mode,
        )
    }

    #[test]
    fn an_import_never_changes_the_original() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("flowers");
        std::fs::create_dir_all(root.join("petals")).unwrap();
        let files = [root.join("iris.csv"), root.join("petals/red.csv")];
        std::fs::write(&files[0], b"iris\n").unwrap();
        std::fs::write(&files[1], b"red\n").unwrap();
        let before = files.each_ref().map(|file| look(file));
        let cache = Cache::open(dir.path().join("cache")).unwrap();
        let context = crate::cli::project::tests::offline_context();
        let walked = walk(
            &root,
            "./flowers",
            &entry(&[], &[]),
            crate::net::MOST_FILES,
            &context.stop,
        )
        .unwrap();
        let copied = into_cache(&context, &cache, &walked, &|_| Ok(())).unwrap();
        let pins: Vec<(&str, u64, crate::digest::Sha256, &str)> = copied
            .files
            .iter()
            .map(|file| {
                (
                    file.path.as_str(),
                    file.size,
                    file.sha256,
                    file.url.as_str(),
                )
            })
            .collect();
        assert_eq!(
            pins,
            [
                ("iris.csv", 5, crate::digest::sha256(b"iris\n"), ""),
                ("petals/red.csv", 4, crate::digest::sha256(b"red\n"), ""),
            ]
        );
        assert_eq!(files.each_ref().map(|file| look(file)), before);
        for (file, pin) in files.iter().zip(&copied.files) {
            let object = cache.object_path(&pin.sha256);
            assert!(
                !same_file::is_same_file(&object, file).unwrap(),
                "{}",
                pin.path
            );
        }
    }

    /// an import that ends within its command's moment, a second at a terminal, draws no bar, so
    /// adding a small folder shows no progress that flashes and goes.
    #[test]
    fn a_quick_import_draws_no_bar() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("flowers");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("iris.csv"), b"iris\n").unwrap();
        let cache = Cache::open(dir.path().join("cache")).unwrap();
        let screen = crate::output::screen::Screen::default();
        let mut context = Context::new(Output::on_screen_after(
            screen.clone(),
            std::time::Duration::from_secs(3600),
        ));
        context.offline = crate::net::Offline::Flag;
        let walked = walk(
            &root,
            "./flowers",
            &entry(&[], &[]),
            crate::net::MOST_FILES,
            &context.stop,
        )
        .unwrap();
        into_cache(&context, &cache, &walked, &|_| Ok(())).unwrap();
        assert_eq!(screen.records(), Vec::<String>::new());
    }

    /// a context whose output draws on `screen` and tells an import's walk, its command's moment
    /// `moment` long.
    fn walking_on(screen: &crate::output::screen::Screen, moment: std::time::Duration) -> Context {
        Context::new(Output::on_screen_after(screen.clone(), moment))
    }

    /// a walk still going once its command has lasted a moment says what it reads, so adding a
    /// folder of a million files shows a line about a second after enter, not seconds of nothing.
    #[test]
    fn a_long_walk_says_what_it_reads() {
        let screen = crate::output::screen::Screen::default();
        let context = walking_on(&screen, std::time::Duration::ZERO);
        let shown = told_walk(&context.output, "./flowers", || {
            screen.shows("Reading ./flowers")
        });
        assert!(shown, "{:?}", screen.records());
    }

    /// a walk that ends within its command's moment draws nothing, then or later, since its line
    /// ends with it.
    #[test]
    fn a_quick_walk_draws_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("flowers");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("iris.csv"), b"iris\n").unwrap();
        let screen = crate::output::screen::Screen::default();
        let context = walking_on(&screen, std::time::Duration::from_secs(3600));
        let walked = told_walk(&context.output, "./flowers", || {
            walk(
                &root,
                "./flowers",
                &entry(&[], &[]),
                crate::net::MOST_FILES,
                &context.stop,
            )
        })
        .unwrap();
        assert_eq!(walked.files.len(), 1);
        assert_eq!(screen.records(), Vec::<String>::new());
    }

    /// a walk that fails or stops clears its line before it returns, so the error that follows
    /// stands alone.
    #[test]
    fn a_walk_that_fails_clears_its_line_first() {
        let screen = crate::output::screen::Screen::default();
        let context = walking_on(&screen, std::time::Duration::ZERO);
        let walked: Result<Walked, Error> = told_walk(&context.output, "./flowers", || {
            assert!(screen.shows("Reading ./flowers"), "{:?}", screen.records());
            Err(crate::stop::Stop::stopped())
        });
        assert!(walked.is_err());
        let records = screen.records();
        let last = records
            .iter()
            .rposition(|record| record.contains("Reading"))
            .unwrap();
        assert!(records[last..].contains(&"clear".to_owned()), "{records:?}");
    }

    /// on windows, a folder of an import swapped for a junction once it was walked, as another
    /// account that can write the folder could, is never read through: the import refuses it, and
    /// the file the junction leads to stays out of the cache.
    #[cfg(windows)]
    #[test]
    fn a_junction_swapped_in_after_the_walk_is_never_read_through() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("flowers");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub").join("bait.csv"), b"what the folder held").unwrap();
        let outside = dir.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("bait.csv"), b"someone's own bytes").unwrap();
        let cache = Cache::open(dir.path().join("cache")).unwrap();
        let context = crate::cli::project::tests::offline_context();
        let walked = walk(
            &root,
            "./flowers",
            &entry(&[], &[]),
            crate::net::MOST_FILES,
            &context.stop,
        )
        .unwrap();
        std::fs::rename(root.join("sub"), dir.path().join("moved")).unwrap();
        if crate::test_links::link_folder(&outside, &root.join("sub")).is_err() {
            return crate::test_skips::skip("junctions", "this disk makes no junctions");
        }
        let copied = into_cache(&context, &cache, &walked, &|_| Ok(()));
        assert!(copied.is_err(), "the junction was read through");
        let theirs = crate::digest::sha256(b"someone's own bytes");
        assert!(cache.find(&theirs, None).is_none());
    }

    /// how a file of `size` bytes no walk read looks.
    fn unseen(size: u64) -> Seen {
        Seen {
            look: crate::project::state::Look { size, modified: 0 },
            node: (0, 0, 0),
            writable: true,
        }
    }

    #[test]
    fn names_are_refused_as_an_archive_refuses_its_members() {
        let found = |paths: &[&str]| -> Vec<Found> {
            paths
                .iter()
                .map(|path| Found {
                    path: (*path).to_owned(),
                    source: PathBuf::new(),
                    size: 0,
                    seen: unseen(0),
                })
                .collect()
        };
        for (paths, message) in [
            (
                &["Iris.csv", "iris.csv"][..],
                "./flowers holds \"Iris.csv\" and \"iris.csv\", which are one name on disks that ignore case",
            ),
            (
                &["Data/a.csv", "data/b.csv"],
                "./flowers holds \"Data\" and \"data\", which are one name on disks that ignore case",
            ),
            (
                &["a\\b.csv"],
                "./flowers holds \"a\\\\b.csv\", which fetchloom refuses because it has a \\, which windows reads as a folder separator",
            ),
            (
                &["bell\u{7}.csv"],
                "./flowers holds \"bell\\u{7}.csv\", which fetchloom refuses because it holds a control character",
            ),
            (
                &["caf\u{e9}.csv", "caf\u{e9}.csv"],
                "./flowers holds two files named \"caf\u{e9}.csv\", their unicode written two ways, which macos disks see as one name",
            ),
            (
                &["a", "a/b.csv"],
                "./flowers holds \"a\" both as a file and as a folder",
            ),
            (
                &["ab.csv", "a\u{200d}b.csv"],
                "./flowers holds \"ab.csv\" and \"a\\u{200d}b.csv\", one name on a mac os extended (hfs+) disk, which ignores the invisible code point they differ in",
            ),
        ] {
            let error = checked_names(&found(paths), "./flowers").unwrap_err();
            assert_eq!(error.kind, Kind::SourceInvalid, "{paths:?}");
            assert_eq!(error.message, message, "{paths:?}");
        }
        assert!(checked_names(&found(&["a/b.csv", "a/c.csv", "b.csv"]), "./flowers").is_ok());
        assert_eq!(crate::name::composed("cafe\u{301}.csv"), "caf\u{e9}.csv");
    }

    #[test]
    fn a_supplied_folder_is_compared_by_path_and_size_before_its_bytes() {
        let pin = |path: &str, size: u64, byte: u8| LockedFile {
            path: path.to_owned(),
            size,
            sha256: crate::digest::sha256(&[byte]),
            md5: None,
            url: String::new(),
            unpacks_to: None,
        };
        let file = |path: &str, size: u64| Found {
            path: path.to_owned(),
            source: PathBuf::new(),
            size,
            seen: unseen(size),
        };
        let pinned = [pin("a.csv", 1, 1), pin("b.csv", 2, 2)];
        assert_eq!(
            first_difference(&[file("a.csv", 1), file("b.csv", 2)], &pinned),
            None
        );
        for (found, difference) in [
            (vec![file("a.csv", 1)], "b.csv is missing"),
            (
                vec![file("a.csv", 1), file("b.csv", 2), file("c.csv", 3)],
                "c.csv is not in data.lock",
            ),
            (
                vec![file("a.csv", 1), file("b.csv", 3)],
                "b.csv has 3 bytes, and data.lock pins 2 bytes",
            ),
            (
                vec![file("a.csv", 1), file("ab.csv", 1), file("b.csv", 2)],
                "ab.csv is not in data.lock",
            ),
        ] {
            assert_eq!(
                first_difference(&found, &pinned).as_deref(),
                Some(difference)
            );
        }
        let copied = [pin("a.csv", 1, 1), pin("b.csv", 2, 9)];
        assert_eq!(
            first_other_bytes(&copied, &pinned).as_deref(),
            Some("b.csv holds other bytes than data.lock pins")
        );
        assert_eq!(first_other_bytes(&pinned, &pinned), None);
    }
}
