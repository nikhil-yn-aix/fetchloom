//! `fetchloom drop`: files left out of their dataset for good, by patterns in data.toml, from
//! data.lock for files a source lists, and from the data folder when unchanged. it asks no server.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

use serde::Serialize;

use super::args::Drop;
use super::project::{committed, dry_run_done, find, hold};
use super::sync::disk::Survey;
use super::sync::place::{self, Changes, Placement};
use super::sync::{Scope, without_files};
use super::{Context, open_cache, peek_cache};
use crate::cache::Cache;
use crate::error::{Error, Kind};
use crate::glob::{self, Glob};
use crate::name;
use crate::plan::{self, Plan, Target};
use crate::project::lock::archives::{self, Archive, Pinned};
use crate::project::lock::{self, Lock, Locked, LockedFile};
use crate::project::manifest::{self, Manifest, Narrowing};
use crate::recipe;
use crate::units;

/// what `drop` did, or with `--dry-run` would do, as `--json` prints it.
#[derive(Serialize)]
pub(super) struct Report {
    project: PathBuf,
    /// each file left out, by its path in the project.
    dropped: Vec<Left>,
    copied: Vec<Copied>,
    lock_changed: bool,
    placement: Option<Placement>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    dry_run: bool,
    /// with `--dry-run`, the paths that would change.
    #[serde(skip_serializing_if = "Option::is_none")]
    changes: Option<Changes>,
}

/// one file left out, with the archive it is inside, if any, and the pattern written for it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct Left {
    path: String,
    dataset: String,
    archive: Option<String>,
    pattern: String,
}

/// a members table written for `archive` as a copy of the glob key `from`'s, which covered it: a
/// table under the archive's own path wins over a glob, so the copy keeps what the glob chose.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct Copied {
    dataset: String,
    archive: String,
    from: String,
}

/// the glob key whose table an archive's own table starts as a copy of, and the select patterns
/// of it that match no file of the archive, which the copy leaves out.
struct FromGlob {
    key: String,
    unmatched: Vec<String>,
}

/// the planned files one path names: the file at it, or every file under a folder.
struct Asked<'p, 'a> {
    /// the path in the project, with `/` between folders, as spelled in the plan when it names a
    /// file.
    path: String,
    folder: bool,
    targets: Vec<&'p Target<'a>>,
}

/// where the files a dataset keeps of one archive land, by their paths in the project, with
/// each one's path inside the archive.
type Inside = HashMap<String, String>;

/// what drop writes for one dataset.
#[derive(Default)]
struct Edit<'a> {
    /// patterns for the dataset's own exclude, which leave out files its source lists.
    exclude: BTreeSet<String>,
    /// the files those leave out, by their paths in data.lock: a file, or a whole archive.
    leaving: BTreeSet<&'a str>,
    /// patterns for the members table of each archive, by its path in data.lock.
    members: BTreeMap<&'a str, BTreeSet<String>>,
    /// the glob key's table each archive in `members` without a table of its own starts from.
    copied: BTreeMap<&'a str, FromGlob>,
    /// the files left out, as `--json` lists them.
    left: Vec<Left>,
}

/// planning's work, a step for each planned file it takes and each listed path it compares with
/// a pattern, looking for a stop every [`CHUNK`] steps and between its parts.
struct Pace<'p> {
    steps: usize,
    poll: &'p dyn Fn() -> Result<(), Error>,
}

/// the steps planning takes between two looks for a stop, a few milliseconds of work.
const CHUNK: usize = 4096;

impl<'p> Pace<'p> {
    fn new(poll: &'p dyn Fn() -> Result<(), Error>) -> Pace<'p> {
        Pace { steps: 0, poll }
    }

    fn step(&mut self) -> Result<(), Error> {
        self.steps += 1;
        if self.steps.is_multiple_of(CHUNK) {
            (self.poll)()
        } else {
            Ok(())
        }
    }

    fn look(&self) -> Result<(), Error> {
        (self.poll)()
    }
}

pub(super) async fn run(context: &Context, args: Drop) -> Result<Report, Error> {
    let project = find(&context.output)?;
    let dry_run = args.dry.dry_run;
    let _held = if dry_run {
        None
    } else {
        Some(hold(context, &project, "drop").await?)
    };
    let (text, manifest) = project.manifest()?;
    let lock = project.lock()?.unwrap_or_default();
    let cache = if dry_run {
        peek_cache()?
    } else {
        open_cache()?
    };
    let here =
        std::env::current_dir().map_err(|err| Error::io("cannot read the current folder", &err))?;
    let paths = args
        .paths
        .iter()
        .map(|path| in_data_folder(&project.root, &here, &manifest.data_dir, path))
        .collect::<Result<Vec<_>, _>>()?;
    let poll = || unchanged_by_a_stop(&context.stop);
    let mut pace = Pace::new(&poll);
    let progress = context.output.wait("Planning", planning(&paths));
    pace.look()?;
    // a dataset with `place = false` is planned where its files would land, so they drop too
    let planned = plan::inputs(&manifest, &lock, &cache)?;
    let asked = asked(
        &manifest,
        &lock,
        &cache,
        &planned,
        &paths,
        &args.paths,
        &pace,
    )?;
    let edits = edits(&manifest, &lock, &cache, &asked, &mut pace)?;
    refuse_what_breaks(&manifest, &lock, &planned, &cache, &edits, &pace)?;
    let (updated, new_lock) = written(&text, &manifest, &lock, &edits, &pace)?;
    let new_manifest = Manifest::parse(&updated)?;
    let lock_changed = edits.values().any(|edit| !edit.leaving.is_empty());
    let mut dropped: Vec<Left> = edits.values().flat_map(|edit| edit.left.clone()).collect();
    dropped.sort_by(|a, b| a.path.cmp(&b.path));
    let copied = copies(&edits);
    let scope = Scope::Only(edits.keys().map(|name| (*name).to_owned()).collect());
    pace.look()?;
    let survey = Survey::read(
        &project,
        &new_manifest.data_dir,
        context.threads,
        &context.stop,
    )?;
    pace.look()?;
    let placing = place::Placing::new(
        &project,
        &new_manifest,
        &new_lock,
        &cache,
        false,
        &scope,
        survey,
    )?;
    pace.look()?;
    let report = |placement, changes| Report {
        project: project.root.clone(),
        dropped: dropped.clone(),
        copied: copied.clone(),
        lock_changed,
        placement: Some(placement),
        dry_run,
        changes,
    };
    if dry_run {
        let preview = placing.preview_removal();
        pace.look()?;
        context.stop.finish();
        drop(progress);
        tell(&context.output, &edits, true);
        place::tell_preview(&context.output, &new_manifest, &scope, &preview);
        dry_run_done(&context.output, None);
        return Ok(report(preview.placement, Some(preview.changes)));
    }
    // the last look for a stop: once the files change, the drop runs to its end
    pace.look()?;
    context.stop.finish();
    drop(progress);
    write(
        context,
        &project,
        &updated,
        lock_changed.then_some(&new_lock),
    )?;
    tell(&context.output, &edits, false);
    let placement = placing
        .remove_only(&context.output)
        .map_err(|error| written_first(error, lock_changed))?;
    tell_next(context, &project, &new_manifest, &edits, lock_changed);
    Ok(report(placement, None))
}

fn written_first(error: Error, lock_changed: bool) -> Error {
    let written = if lock_changed {
        "data.toml and data.lock were"
    } else {
        "data.toml was"
    };
    error.detail(format!(
        "{written} updated; after fixing the error, `{}` removes the files they leave out",
        super::project::command("sync")
    ))
}

fn copies(edits: &BTreeMap<&str, Edit>) -> Vec<Copied> {
    edits
        .iter()
        .flat_map(|(dataset, edit)| {
            edit.copied.iter().map(|(archive, from)| Copied {
                dataset: (*dataset).to_owned(),
                archive: (*archive).to_owned(),
                from: from.key.clone(),
            })
        })
        .collect()
}

/// a stop asked while drop plans, which writes nothing, so the stop says that nothing changed.
fn unchanged_by_a_stop(stop: &crate::stop::Stop) -> Result<(), Error> {
    stop.check().map_err(|mut error| {
        error.message = format!("{}, so nothing changed", error.message);
        error
    })
}

fn write(
    context: &Context,
    project: &crate::project::Project,
    updated: &str,
    lock: Option<&Lock>,
) -> Result<(), Error> {
    project.write(manifest::FILE, updated.as_bytes())?;
    if let Some(lock) = lock {
        context.wrote_lists(&project.root, project.write_lists(lock)?);
        project.write(lock::FILE, lock.render().as_bytes())?;
        project.sweep_lists(lock);
    }
    Ok(())
}

/// the closing `next:` line: build what data.lock pins as made from the files before, or commit.
fn tell_next(
    context: &Context,
    project: &crate::project::Project,
    manifest: &Manifest,
    edits: &BTreeMap<&str, Edit>,
    lock_changed: bool,
) {
    let datasets: Vec<&str> = edits.keys().copied().collect();
    context
        .output
        .next(&match built_again(manifest, &datasets).as_slice() {
            [] => {
                let changed = if lock_changed {
                    committed(project)
                } else {
                    "data.toml"
                };
                format!("commit {changed}; other clones drop them at their next `fetchloom sync`")
            }
            recipes => format!(
                "run `{}`, then commit data.toml, data.lock and data.lock.d",
                super::project::command(&format!("build {}", recipes.join(" ")))
            ),
        });
}

/// the recipes to build once `changed` datasets hold other files: each made from one of them,
/// directly or not, apart from those another of these is made from, which its build makes too.
fn built_again<'m>(manifest: &'m Manifest, changed: &[&str]) -> Vec<&'m str> {
    let made_from: BTreeMap<&str, Vec<&str>> = manifest
        .datasets
        .iter()
        .filter(|(name, entry)| entry.recipe().is_some() && !changed.contains(&name.as_str()))
        .map(|(name, _)| {
            let needs = recipe::closure(&manifest.datasets, &[name.as_str()]);
            (name.as_str(), needs)
        })
        .filter(|(_, needs)| needs.iter().any(|need| changed.contains(need)))
        .collect();
    made_from
        .keys()
        .copied()
        .filter(|name| {
            !made_from
                .iter()
                .any(|(other, needs)| other != name && needs.contains(name))
        })
        .collect()
}

fn planning(paths: &[String]) -> String {
    match paths {
        [one] => format!("the drop of {one}"),
        many => format!(
            "the drop of {}",
            units::count(many.len() as u64, "path", "paths")
        ),
    }
}

/// `arg`, relative to `here` and maybe naming a file already deleted, as a project path with `/`
/// between folders, read without following links. it fails as `usage` outside the data folder.
fn in_data_folder(root: &Path, here: &Path, data_dir: &str, arg: &Path) -> Result<String, Error> {
    let outside = || {
        Error::new(
            Kind::Usage,
            format!(
                "{} is not inside the data folder, {data_dir}/",
                arg.display()
            ),
        )
        .help(format!(
            "drop leaves out files data.lock places in {data_dir}/; delete other files yourself"
        ))
    };
    let joined = here.join(arg);
    let below = joined.strip_prefix(root).map_err(|_| outside())?;
    let mut names: Vec<&str> = Vec::new();
    for component in below.components() {
        match component {
            Component::Normal(part) => names.push(part.to_str().ok_or_else(outside)?),
            Component::ParentDir => {
                names.pop().ok_or_else(outside)?;
            }
            Component::CurDir => {}
            Component::RootDir | Component::Prefix(_) => return Err(outside()),
        }
    }
    let path = names.join("/");
    let data = name::fold(data_dir);
    let folded = name::fold(&path);
    if folded == data || folded.starts_with(&format!("{data}/")) {
        Ok(path)
    } else {
        Err(outside())
    }
}

/// the planned files each of `paths` names, compared as a disk that ignores case compares names;
/// it fails as `usage` for a path that names none, saying why, or that names a recipe's file.
fn asked<'p, 'a>(
    manifest: &Manifest,
    lock: &Lock,
    cache: &Cache,
    planned: &'p Plan<'a>,
    paths: &[String],
    typed: &[PathBuf],
    pace: &Pace,
) -> Result<Vec<Asked<'p, 'a>>, Error> {
    let mut sorted: Vec<(String, &'p Target<'a>)> = planned
        .targets
        .iter()
        .map(|target| (name::fold(&target.path), target))
        .collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    let mut asked = Vec::with_capacity(paths.len());
    for (path, typed) in paths.iter().zip(typed) {
        pace.look()?;
        let found = named(&sorted, path);
        if found.targets.is_empty() {
            return Err(unplanned(
                manifest, lock, cache, planned, &sorted, path, typed,
            ));
        }
        if let Some(made) = found.targets.iter().find(|target| {
            manifest
                .datasets
                .get(target.dataset)
                .is_some_and(|entry| entry.recipe().is_some())
        }) {
            return Err(made_by_recipe(path, made.dataset));
        }
        asked.push(found);
    }
    Ok(asked)
}

/// the planned files `path` names among `sorted`, the targets by their folded paths: the file at
/// it, or every file under it.
fn named<'p, 'a>(sorted: &[(String, &'p Target<'a>)], path: &str) -> Asked<'p, 'a> {
    let folded = name::fold(path);
    let at = sorted.partition_point(|(key, _)| *key < folded);
    if let Some((key, target)) = sorted.get(at)
        && *key == folded
    {
        return Asked {
            path: target.path.clone(),
            folder: false,
            targets: vec![target],
        };
    }
    let below = format!("{folded}/");
    let start = sorted.partition_point(|(key, _)| *key < below);
    Asked {
        path: path.to_owned(),
        folder: true,
        targets: sorted[start..]
            .iter()
            .take_while(|(key, _)| key.starts_with(&below))
            .map(|(_, target)| *target)
            .collect(),
    }
}

fn made_by_recipe(path: &str, dataset: &str) -> Error {
    Error::new(
        Kind::Usage,
        format!("{path} belongs to {dataset}, which a recipe makes"),
    )
    .help("choose its files inside the recipe's step instead")
}

/// why `path`, `typed` as given, names no planned file of `sorted`, the targets by folded path.
fn unplanned(
    manifest: &Manifest,
    lock: &Lock,
    cache: &Cache,
    planned: &Plan,
    sorted: &[(String, &Target)],
    path: &str,
    typed: &Path,
) -> Error {
    let within = |dataset: &str| {
        let folder = name::fold(&plan::folder(manifest, dataset));
        let path = name::fold(path);
        path == folder || path.starts_with(&format!("{folder}/"))
    };
    if let Some(name) = manifest
        .datasets
        .iter()
        .find(|(name, entry)| entry.recipe().is_some() && within(name))
        .map(|(name, _)| name)
    {
        return made_by_recipe(path, name);
    }
    if let Some((_, archive)) = planned.unknown.iter().find(|(dataset, _)| within(dataset)) {
        let archive = archive_name(&archive.path);
        return Error::new(
            Kind::Usage,
            format!("this machine never unpacked {archive}, so fetchloom cannot tell whether it holds {path}"),
        )
        .help(format!(
            "run `{}` first, so fetchloom knows what {archive} holds",
            super::project::command("sync")
        ));
    }
    if let Some(by) = left_out(manifest, lock, cache, path) {
        return Error::new(Kind::Usage, format!("{path} is left out already, {by}")).help(format!(
            "to bring it back, delete that pattern from data.toml and run `{}`",
            super::project::command("sync")
        ));
    }
    let owners: Vec<&str> = manifest
        .datasets
        .keys()
        .filter(|name| within(name))
        .map(String::as_str)
        .collect();
    let owners = if owners.is_empty() {
        "any dataset".to_owned()
    } else {
        units::listed(&owners, "or")
    };
    let error = Error::new(Kind::Usage, format!("{path} names no file of {owners}"));
    if let Some(wild) = path.chars().find(|c| matches!(c, '*' | '?' | '[')) {
        return error
            .detail(format!(
                "drop takes paths, not patterns, so {wild} is part of the name it looks for"
            ))
            .help(exclude_help(manifest, lock, cache, sorted, path));
    }
    if let Some(meant) = near(sorted, path) {
        // a path in the project is a command only from the project's folder
        let help = match (typed.to_str(), retyped(typed, path, &meant)) {
            (Some(typed), Some(retyped)) => format!(
                "did you mean `{}`?",
                super::meant(
                    &[typed],
                    &[&retyped],
                    format!("fetchloom drop {}", super::shell_word(&retyped)),
                )
            ),
            _ => format!("did you mean {meant}?"),
        };
        return error.help(help);
    }
    error.help(
        "check the path; a file of your own needs no drop: delete it yourself if you do not need it",
    )
}

/// the planned file or folder `path` likely meant, as planning spells it: one name changed to the
/// closest planned there, below the deepest folder of `path` holding planned files.
fn near(sorted: &[(String, &Target)], path: &str) -> Option<String> {
    let key = name::fold(path);
    let names: Vec<&str> = key.split('/').collect();
    let under = |folder: &str| {
        let below = format!("{folder}/");
        let start = sorted.partition_point(|(key, _)| *key < below);
        sorted[start..]
            .iter()
            .take_while(move |(key, _)| key.starts_with(&below))
    };
    let depth = (1..names.len())
        .rev()
        .find(|depth| under(&names[..*depth].join("/")).next().is_some())?;
    let folder = names[..depth].join("/");
    let mut there: Vec<&str> = under(&folder)
        .filter_map(|(key, _)| key[folder.len() + 1..].split('/').next())
        .collect();
    there.dedup();
    let mut meant = names.clone();
    meant[depth] = name::closest(names[depth], there)?;
    let found = named(sorted, &meant.join("/"));
    let target = found.targets.first()?;
    Some(
        target
            .path
            .split('/')
            .take(names.len())
            .collect::<Vec<_>>()
            .join("/"),
    )
}

/// `typed` with its names from the first `meant` changes written as in `meant`, so a suggestion
/// keeps the folder it was typed from; none when they do not end `typed`, as after a `..`.
fn retyped(typed: &Path, path: &str, meant: &str) -> Option<String> {
    let typed = typed.to_str()?;
    let names: Vec<&str> = path.split('/').collect();
    let changed: Vec<&str> = meant.split('/').collect();
    let first = names
        .iter()
        .zip(&changed)
        .position(|(name, change)| name != change)?;
    let separators: &[char] = if cfg!(windows) { &['/', '\\'] } else { &['/'] };
    let mut start = typed.trim_end_matches(separators).len();
    for (at, name) in names.iter().enumerate().skip(first).rev() {
        let rest = typed[..start].strip_suffix(name)?;
        if !(rest.is_empty() || rest.ends_with(separators)) {
            return None;
        }
        start = rest.len();
        if at > first {
            start = typed[..start].strip_suffix(separators)?.len();
        }
    }
    Some(format!("{}{}", &typed[..start], changed[first..].join("/")))
}

/// the help for `path`, a pattern given where drop takes a path: the exclude to write and where,
/// when its folder holds planned files of one dataset from its source's list or one archive.
fn exclude_help(
    manifest: &Manifest,
    lock: &Lock,
    cache: &Cache,
    sorted: &[(String, &Target)],
    path: &str,
) -> String {
    let sync = super::project::command("sync");
    let anywhere = format!(
        "to leave out every file a pattern matches, add it to exclude in data.toml, or to the archive's members table for files inside an archive, by hand, then run `{sync}`"
    );
    let Some((folder, last)) = path.rsplit_once('/') else {
        return anywhere;
    };
    let found = named(sorted, folder);
    let Some((dataset, first)) = found
        .targets
        .first()
        .and_then(|target| Some((target.dataset, target.file()?)))
        .filter(|_| found.folder)
    else {
        return anywhere;
    };
    let alike = |file: &LockedFile| match first.unpacks_to {
        Some(_) => file.path == first.path,
        None => file.unpacks_to.is_none(),
    };
    let one_origin = found
        .targets
        .iter()
        .all(|target| target.dataset == dataset && target.file().is_some_and(alike));
    let Some(locked) = lock.get(dataset).filter(|_| one_origin) else {
        return anywhere;
    };
    let inside = match first.unpacks_to {
        Some(_) => match unpacked(manifest, locked, first, cache) {
            Ok(inside) => Some(inside),
            Err(_) => return anywhere,
        },
        None => None,
    };
    let chosen: Vec<(&str, &str)> = found
        .targets
        .iter()
        .filter_map(|target| {
            let origin = match &inside {
                Some(inside) => inside.get(&target.path)?.as_str(),
                None => target.file()?.path.as_str(),
            };
            Some((target.path.as_str(), origin))
        })
        .collect();
    let pattern = match shared_prefix(folder, &chosen) {
        Some(prefix) => format!("{}/{last}", glob::escape(&prefix)),
        None => last.to_owned(),
    };
    let table = match inside {
        Some(_) => {
            // the table that covers the archive, since a table of its own would win over a glob
            let key = manifest
                .datasets
                .get(dataset)
                .and_then(|entry| entry.source())
                .and_then(|source| source.members.of(dataset, &first.path).ok().flatten())
                .map_or(first.path.as_str(), |(key, _)| key);
            format!("members.\"{key}\" of datasets.{dataset}")
        }
        None => format!("datasets.{dataset}"),
    };
    format!(
        "to leave out every file a pattern matches, add \"{pattern}\" to exclude in {table} in data.toml by hand, then run `{sync}`"
    )
}

/// the pattern in data.toml that leaves out the file at `path`, as a few words naming it.
fn left_out(manifest: &Manifest, lock: &Lock, cache: &Cache, path: &str) -> Option<String> {
    let asked = name::fold(path);
    for locked in &lock.datasets {
        let name = &locked.name;
        let Some(source) = manifest.datasets.get(name).and_then(|entry| entry.source()) else {
            continue;
        };
        let folder = plan::folder(manifest, name);
        let pinned = archives::pinned(&locked.files);
        let alone = pinned.len() == 1;
        for archive in pinned.iter().filter_map(|pinned| match pinned {
            Pinned::Archive(archive) => Some(archive),
            Pinned::File(_) => None,
        }) {
            let (Some((key, filter)), Some(members)) = (
                source.members.of(name, &archive.file.path).ok().flatten(),
                archive.recorded(cache),
            ) else {
                continue;
            };
            let found = members
                .iter()
                .zip(plan::unpacked_paths(archive.path(), alone, &members))
                .find(|(_, placed)| name::fold(&format!("{folder}/{placed}")) == asked);
            if let Some((member, _)) = found.filter(|(member, _)| !filter.keeps(&member.path)) {
                let table = format!("members.\"{key}\" of datasets.{name}");
                return Some(match filter.exclude.matching(&member.path) {
                    Some(glob) => format!("by exclude \"{}\" in {table}", glob.as_str()),
                    None => format!("since select in {table} does not keep it"),
                });
            }
        }
        if !asked.starts_with(&format!("{}/", name::fold(&folder))) {
            continue;
        }
        let relative: Vec<&str> = path.split('/').skip(folder.split('/').count()).collect();
        let relative = relative.join("/");
        if let Some(glob) = source.exclude.iter().find(|glob| glob.matches(&relative)) {
            return Some(format!(
                "by exclude \"{}\" in datasets.{name}",
                glob.as_str()
            ));
        }
    }
    None
}

fn archive_name(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, name)| name)
}

/// what to write for each dataset `asked` names files of.
fn edits<'a>(
    manifest: &Manifest,
    lock: &'a Lock,
    cache: &Cache,
    asked: &[Asked<'_, 'a>],
    pace: &mut Pace,
) -> Result<BTreeMap<&'a str, Edit<'a>>, Error> {
    let mut archives: HashMap<(&str, &str), Inside> = HashMap::new();
    let mut edits: BTreeMap<&'a str, Edit<'a>> = BTreeMap::new();
    let mut seen: HashSet<&str> = HashSet::new();
    // a folder claims its files before a path inside it does, so overlapping paths still fold
    let mut ordered: Vec<&Asked<'_, 'a>> = asked.iter().collect();
    ordered.sort_by_key(|request| request.path.matches('/').count());
    for request in ordered {
        // the files a dataset's source lists are matched against its whole list once for each
        // path, and the files inside an archive against what that archive keeps
        let mut listed: BTreeMap<&'a str, Vec<(&Target<'a>, &'a LockedFile)>> = BTreeMap::new();
        let mut packed: BTreeMap<(&'a str, &'a str), Vec<(&Target<'a>, &'a LockedFile)>> =
            BTreeMap::new();
        // `asked` refuses a recipe's files, so each target comes from a file a source lists
        for (target, file) in request
            .targets
            .iter()
            .filter_map(|target| Some((*target, target.file()?)))
        {
            pace.step()?;
            if !seen.insert(target.path.as_str()) {
                continue;
            }
            if file.unpacks_to.is_none() {
                listed
                    .entry(target.dataset)
                    .or_default()
                    .push((target, file));
            } else {
                packed
                    .entry((target.dataset, file.path.as_str()))
                    .or_default()
                    .push((target, file));
            }
        }
        for (dataset, targets) in listed {
            let locked = lock.get(dataset).ok_or_else(|| missing_pin(dataset))?;
            let edit = edits.entry(dataset).or_default();
            let chosen: Vec<(&str, &str)> = targets
                .iter()
                .map(|(target, file)| (target.path.as_str(), file.path.as_str()))
                .collect();
            let all = locked.files.iter().map(|file| file.path.as_str());
            let patterns = written_for(request, &chosen, all, pace)?;
            record(edit, dataset, None, &chosen, &patterns);
            edit.leaving
                .extend(targets.iter().map(|(_, file)| file.path.as_str()));
        }
        for ((dataset, origin), targets) in packed {
            let locked = lock.get(dataset).ok_or_else(|| missing_pin(dataset))?;
            let edit = edits.entry(dataset).or_default();
            let inside: &Inside = match archives.entry((dataset, origin)) {
                std::collections::hash_map::Entry::Occupied(found) => found.into_mut(),
                std::collections::hash_map::Entry::Vacant(slot) => {
                    slot.insert(unpacked(manifest, locked, targets[0].1, cache)?)
                }
            };
            let chosen: Vec<(&str, &str)> = targets
                .iter()
                .filter_map(|(target, _)| {
                    let member = inside.get(&target.path)?;
                    Some((target.path.as_str(), member.as_str()))
                })
                .collect();
            let kept = inside.values().map(String::as_str);
            let patterns = written_for(request, &chosen, kept, pace)?;
            record(edit, dataset, Some(origin), &chosen, &patterns);
            edit.members
                .entry(origin)
                .or_default()
                .extend(patterns.into_iter().map(|(_, pattern)| pattern));
        }
    }
    for (dataset, edit) in &mut edits {
        let locked = lock.get(dataset).ok_or_else(|| missing_pin(dataset))?;
        whole_archives(locked, edit, &archives);
        if let Some(source) = manifest
            .datasets
            .get(*dataset)
            .and_then(|entry| entry.source())
        {
            edit.copied = from_globs(source, locked, &edit.members, cache)?;
        }
    }
    Ok(edits)
}

/// the glob key's table each archive in `members` without a table of its own starts from, since a
/// pattern added to the glob's table would reach every archive it covers.
fn from_globs<'a>(
    source: &manifest::Source,
    locked: &Locked,
    members: &BTreeMap<&'a str, BTreeSet<String>>,
    cache: &Cache,
) -> Result<BTreeMap<&'a str, FromGlob>, Error> {
    let mut copied = BTreeMap::new();
    for archive in members.keys() {
        let Some((key, filter)) = source
            .members
            .of(&locked.name, archive)?
            .filter(|(key, _)| key != archive)
        else {
            continue;
        };
        let holds = locked
            .files
            .iter()
            .find(|file| file.path == *archive)
            .and_then(|file| archives::archive(&locked.files, file).recorded(cache))
            .unwrap_or_default();
        let unmatched = filter
            .select
            .globs()
            .iter()
            .filter(|glob| !holds.iter().any(|member| glob.matches(&member.path)))
            .map(|glob| glob.as_str().to_owned())
            .collect();
        let key = key.to_owned();
        copied.insert(*archive, FromGlob { key, unmatched });
    }
    Ok(copied)
}

/// the error for a planned file whose dataset data.lock does not pin, which planning rules out.
fn missing_pin(dataset: &str) -> Error {
    Error::new(
        Kind::ProjectInvalid,
        format!("data.lock does not pin {dataset}"),
    )
    .help(format!(
        "run `{}`, then drop again",
        super::project::command("sync")
    ))
}

/// the [`Inside`] of `locked`'s archive `file`, from what this machine recorded unpacking it to.
fn unpacked(
    manifest: &Manifest,
    locked: &Locked,
    file: &LockedFile,
    cache: &Cache,
) -> Result<Inside, Error> {
    let archive = archives::archive(&locked.files, file);
    let members = archive.recorded(cache).ok_or_else(|| {
        Error::new(
            Kind::LocalIo,
            format!("the cache lost what {} unpacked to", file.path),
        )
        .help(format!(
            "run `{}`, then drop again",
            super::project::command("sync")
        ))
    })?;
    let filter = match manifest
        .datasets
        .get(&locked.name)
        .and_then(|entry| entry.source())
    {
        Some(source) => source.members.of(&locked.name, &file.path)?,
        None => None,
    };
    let folder = plan::folder(manifest, &locked.name);
    let alone = archives::pinned(&locked.files).len() == 1;
    Ok(members
        .iter()
        .zip(plan::unpacked_paths(archive.path(), alone, &members))
        .filter(|(member, _)| filter.is_none_or(|(_, filter)| filter.keeps(&member.path)))
        .map(|(member, path)| (format!("{folder}/{path}"), member.path.clone()))
        .collect())
}

/// the patterns that leave out `chosen`, pairs of project and origin paths: `<folder>/**` when it
/// matches exactly those among `all` the origin keeps, else one escaped path each.
fn written_for<'c>(
    request: &Asked,
    chosen: &[(&'c str, &'c str)],
    all: impl Iterator<Item = &'c str>,
    pace: &mut Pace,
) -> Result<Vec<(Vec<&'c str>, String)>, Error> {
    if request.folder
        && let Some(prefix) = shared_prefix(&request.path, chosen)
    {
        let pattern = format!("{}/**", glob::escape(&prefix));
        let wanted: HashSet<&str> = chosen.iter().map(|(_, origin)| *origin).collect();
        if let Ok(glob) = Glob::new(&pattern) {
            let mut matched: HashSet<&str> = HashSet::new();
            for path in all {
                pace.step()?;
                // every path the pattern matches starts with its folder, so the rest skip the match
                if path.starts_with(prefix.as_str()) && glob.matches(path) {
                    matched.insert(path);
                }
            }
            if matched == wanted {
                return Ok(vec![(
                    chosen.iter().map(|(placed, _)| *placed).collect(),
                    pattern,
                )]);
            }
        }
    }
    Ok(chosen
        .iter()
        .map(|(placed, origin)| (vec![*placed], glob::escape(origin)))
        .collect())
}

/// the origin's folder that the project's `folder` stands for: each chosen origin path less the
/// part below `folder`, when that is the same for all and not empty.
fn shared_prefix(folder: &str, chosen: &[(&str, &str)]) -> Option<String> {
    let depth = folder.split('/').count();
    let mut shared: Option<&str> = None;
    for (placed, origin) in chosen {
        let below: Vec<&str> = placed.split('/').skip(depth).collect();
        let prefix = origin
            .strip_suffix(below.join("/").as_str())?
            .strip_suffix('/')?;
        if prefix.is_empty() || shared.is_some_and(|shared| shared != prefix) {
            return None;
        }
        shared = Some(prefix);
    }
    shared.map(str::to_owned)
}

fn record(
    edit: &mut Edit,
    dataset: &str,
    archive: Option<&str>,
    chosen: &[(&str, &str)],
    patterns: &[(Vec<&str>, String)],
) {
    let mut written: HashMap<&str, &str> = HashMap::new();
    for (placed, pattern) in patterns {
        for path in placed {
            written.entry(path).or_insert(pattern);
        }
    }
    for (placed, origin) in chosen {
        let pattern = written
            .get(placed)
            .map_or_else(|| glob::escape(origin), |pattern| (*pattern).to_owned());
        if archive.is_none() {
            edit.exclude.insert(pattern.clone());
        }
        edit.left.push(Left {
            path: (*placed).to_owned(),
            dataset: dataset.to_owned(),
            archive: archive.map(str::to_owned),
            pattern,
        });
    }
}

/// leaves out an archive whose every kept file was asked for, every part of it, in the dataset's
/// exclude, since a members table may not leave its archive no file.
fn whole_archives<'a>(
    locked: &'a Locked,
    edit: &mut Edit<'a>,
    archives: &HashMap<(&str, &str), Inside>,
) {
    let dataset = locked.name.as_str();
    let mut left_of: HashMap<&str, HashSet<&str>> = HashMap::new();
    for left in &edit.left {
        if let Some(archive) = left.archive.as_deref() {
            left_of
                .entry(archive)
                .or_default()
                .insert(left.path.as_str());
        }
    }
    let whole: Vec<&str> = edit
        .members
        .keys()
        .copied()
        .filter(|archive| {
            let left = left_of.get(archive);
            archives.get(&(dataset, *archive)).is_some_and(|inside| {
                inside
                    .keys()
                    .all(|placed| left.is_some_and(|left| left.contains(placed.as_str())))
            })
        })
        .collect();
    for archive in whole {
        edit.members.remove(archive);
        let pattern = glob::escape(archive);
        let files = locked
            .files
            .iter()
            .find(|file| file.path == archive)
            .map(|file| archives::archive(&locked.files, file).files());
        for file in files.into_iter().flatten() {
            edit.exclude.insert(glob::escape(&file.path));
            edit.leaving.insert(&file.path);
        }
        for left in &mut edit.left {
            if left.archive.as_deref() == Some(archive) {
                left.pattern.clone_from(&pattern);
            }
        }
    }
}

/// refuses a drop that would leave a dataset no file, or move files it keeps. data.lock can drop a
/// listed file offline only while it pins what data.toml asks.
fn refuse_what_breaks(
    manifest: &Manifest,
    lock: &Lock,
    planned: &Plan,
    cache: &Cache,
    edits: &BTreeMap<&str, Edit>,
    pace: &Pace,
) -> Result<(), Error> {
    let mut planned_files: HashMap<&str, usize> = HashMap::new();
    for target in &planned.targets {
        *planned_files.entry(target.dataset).or_default() += 1;
    }
    let unknown: HashSet<&str> = planned.unknown.iter().map(|(name, _)| *name).collect();
    for (dataset, edit) in edits {
        pace.look()?;
        let kept = planned_files.get(dataset).copied().unwrap_or_default();
        if edit.left.len() >= kept && !unknown.contains(dataset) {
            return Err(Error::new(
                Kind::Usage,
                format!("that is every file of {dataset}, and a dataset needs one"),
            )
            .help(format!(
                "to take the whole dataset out of the project, run `{}`",
                super::project::command(&format!("remove {dataset}"))
            )));
        }
        if edit.leaving.is_empty() {
            continue;
        }
        let locked = lock.get(dataset).ok_or_else(|| missing_pin(dataset))?;
        if let Some(entry) = manifest.datasets.get(*dataset)
            && locked.drift(entry).is_some()
        {
            return Err(Error::new(
                Kind::Outdated,
                format!("data.toml changed {dataset} since data.lock was written, so drop cannot leave files out of its pin"),
            )
            .help(format!(
                "run `{}` to bring data.lock in line, then drop again",
                super::project::command("sync")
            )));
        }
        let pinned = archives::pinned(&locked.files);
        let left: Vec<&Pinned> = pinned
            .iter()
            .filter(|pinned| {
                let file = match pinned {
                    Pinned::File(file) => file,
                    Pinned::Archive(archive) => archive.file,
                };
                !edit.leaving.contains(file.path.as_str())
            })
            .collect();
        if let [Pinned::Archive(only)] = left.as_slice()
            && pinned.len() > 1
            && moves_alone(only, cache)
        {
            let only = &only.file.path;
            return Err(Error::new(
                Kind::Usage,
                format!(
                    "that would leave {only} the only file of {dataset}, and its files would move, since a dataset that is one archive places them straight in its folder"
                ),
            )
            .help(format!(
                "keep the files, or leave them out with exclude in datasets.{dataset} in data.toml and run `{}`, which moves the files of {only}",
                super::project::command("sync")
            )));
        }
    }
    Ok(())
}

/// whether the files of `archive` land elsewhere once it is the only archive of its dataset, as
/// when nothing records what it holds.
fn moves_alone(archive: &Archive<'_>, cache: &Cache) -> bool {
    archive.recorded(cache).is_none_or(|members| {
        plan::unpacked_paths(archive.path(), true, &members)
            != plan::unpacked_paths(archive.path(), false, &members)
    })
}

fn written(
    text: &str,
    manifest: &Manifest,
    lock: &Lock,
    edits: &BTreeMap<&str, Edit>,
    pace: &Pace,
) -> Result<(String, Lock), Error> {
    let updated = narrowed(text, manifest, lock, edits, pace)?;
    lock_written(&updated, lock, edits).map(|new_lock| (updated, new_lock))
}

/// data.toml's new text once `edits` are written, read and written once.
fn narrowed(
    text: &str,
    manifest: &Manifest,
    lock: &Lock,
    edits: &BTreeMap<&str, Edit>,
    pace: &Pace,
) -> Result<String, Error> {
    let mut changes = Vec::new();
    for (dataset, edit) in edits {
        pace.look()?;
        for (archive, patterns) in &edit.members {
            if let Some(from) = edit.copied.get(archive) {
                changes.push(Narrowing::MembersCopied {
                    name: dataset,
                    from: &from.key,
                    archive,
                    unmatched: &from.unmatched,
                });
            }
            changes.push(Narrowing::Exclusions {
                name: dataset,
                archive: Some(archive),
                patterns: patterns.iter().cloned().collect(),
            });
        }
        // a table that matches no archive left would be refused, as a whole archive's own is
        if let Some(source) = manifest
            .datasets
            .get(*dataset)
            .and_then(|entry| entry.source())
            && let Some(locked) = lock.get(dataset)
            && !edit.leaving.is_empty()
        {
            let kept: Vec<&str> = locked
                .files
                .iter()
                .filter(|file| file.unpacks_to.is_some())
                .map(|file| file.path.as_str())
                .filter(|path| !edit.leaving.contains(path))
                .collect();
            for (key, _) in source.members.unmatched(&kept) {
                changes.push(Narrowing::WithoutMembers {
                    name: dataset,
                    archive: key.to_owned(),
                });
            }
        }
        if !edit.exclude.is_empty() {
            changes.push(Narrowing::Exclusions {
                name: dataset,
                archive: None,
                patterns: edit.exclude.iter().cloned().collect(),
            });
        }
    }
    manifest::narrowed(text, &changes)
}

fn lock_written(updated: &str, lock: &Lock, edits: &BTreeMap<&str, Edit>) -> Result<Lock, Error> {
    let new_manifest = Manifest::parse(updated)?;
    let now = crate::units::timestamp(std::time::SystemTime::now());
    let mut new_lock = lock.clone();
    for locked in &mut new_lock.datasets {
        let Some(edit) = edits.get(locked.name.as_str()) else {
            continue;
        };
        let Some(source) = new_manifest
            .datasets
            .get(&locked.name)
            .and_then(|entry| entry.source())
        else {
            continue;
        };
        if !edit.leaving.is_empty() {
            *locked = without_files(
                locked,
                source,
                &edit.leaving.iter().copied().collect(),
                &now,
            );
        }
    }
    Ok(new_lock)
}

fn tell(output: &crate::output::Output, edits: &BTreeMap<&str, Edit>, dry_run: bool) {
    let verb = if dry_run { "Would exclude" } else { "Excluded" };
    for (dataset, edit) in edits {
        let mut groups: BTreeMap<Option<&str>, Vec<&Left>> = BTreeMap::new();
        for left in &edit.left {
            let whole = left
                .archive
                .as_deref()
                .filter(|archive| edit.members.contains_key(archive));
            groups.entry(whole).or_default().push(left);
        }
        for (archive, left) in groups {
            let (of, written) = match archive {
                Some(archive) => (archive_name(archive), "data.toml"),
                None => (*dataset, "data.toml and data.lock"),
            };
            output.step(
                verb,
                &format!(
                    "{} of {of} in {written}",
                    units::count(left.len() as u64, "file", "files")
                ),
            );
            for left in left {
                output.detail(&left.path);
            }
            if let Some(from) = archive
                .and_then(|archive| edit.copied.get(archive))
                .map(|from| &from.key)
            {
                output.hint(&if dry_run {
                    format!("{of} would have its own members table, copied from members.\"{from}\", so later changes to that table would not reach it")
                } else {
                    format!("{of} now has its own members table, copied from members.\"{from}\", so later changes to that table do not reach it")
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use super::*;

    /// the import tiny, `folders` folders of `each` files, with an empty cache of its own.
    fn tiny(folders: usize, each: usize) -> (Manifest, Lock, tempfile::TempDir, Cache) {
        let manifest = Manifest::parse("[datasets.tiny]\nref = \"local\"\n").unwrap();
        let source = manifest.datasets["tiny"].source().unwrap();
        let files = (0..folders * each)
            .map(|n| {
                let path = format!("d{:03}/f{n:05}.txt", n / each);
                LockedFile {
                    sha256: crate::digest::sha256(path.as_bytes()),
                    path,
                    size: 1,
                    md5: None,
                    url: String::new(),
                    unpacks_to: None,
                }
            })
            .collect();
        let lock = Lock {
            datasets: vec![Locked {
                name: "tiny".to_owned(),
                reference: "local".to_owned(),
                spec: lock::spec(source),
                extract: source.extract,
                web_page: false,
                resolved: "local".to_owned(),
                title: None,
                license: None,
                doi: None,
                access: crate::source::Access::Unknown,
                timestamp: "2026-01-01T00:00:00Z".to_owned(),
                files,
            }],
            derived: Vec::new(),
        };
        let folder = tempfile::tempdir().unwrap();
        let cache = Cache::open(folder.path().join("cache")).unwrap();
        (manifest, lock, folder, cache)
    }

    /// `paths` as a person types them from the project's folder.
    fn typed(paths: &[String]) -> Vec<PathBuf> {
        paths.iter().map(PathBuf::from).collect()
    }

    /// a folder of listed files becomes one pattern.
    #[test]
    fn a_folder_reads_its_dataset_list_once() {
        let (manifest, lock, _folder, cache) = tiny(10, 100);
        let planned = plan::inputs(&manifest, &lock, &cache).unwrap();
        let paths = ["data/tiny/d003".to_owned(), "data/tiny/d007".to_owned()];
        let go_on = || Ok(());
        let mut pace = Pace::new(&go_on);
        let asked = asked(
            &manifest,
            &lock,
            &cache,
            &planned,
            &paths,
            &typed(&paths),
            &pace,
        )
        .unwrap();
        let edits = edits(&manifest, &lock, &cache, &asked, &mut pace).unwrap();
        assert_eq!(
            edits["tiny"].exclude,
            BTreeSet::from(["d003/**".to_owned(), "d007/**".to_owned()])
        );
        assert_eq!(edits["tiny"].left.len(), 200);
        assert!(pace.steps <= 2 * (1_000 + 100), "{}", pace.steps);
    }

    /// a recipe another rebuilt recipe is made from builds along with it, and recipes made from
    /// other datasets stay out.
    #[test]
    fn a_drop_names_the_builds_that_make_every_result_again() {
        let manifest = Manifest::parse(concat!(
            "[datasets.lab]\nref = \"local\"\n\n",
            "[datasets.mnist]\nref = \"local\"\n\n",
            "[datasets.processed]\nfrom = [\"lab\", \"mnist\"]\nrun = [\"prep\", \"{lab}\", \"{mnist}\", \"{out}\"]\n\n",
            "[datasets.features]\nfrom = [\"processed\"]\nrun = [\"feat\", \"{processed}\", \"{out}\"]\n\n",
            "[datasets.thumbs]\nfrom = [\"lab\"]\nrun = [\"thumb\", \"{lab}\", \"{out}\"]\n\n",
            "[datasets.digits]\nfrom = [\"mnist\"]\nrun = [\"digit\", \"{mnist}\", \"{out}\"]\n",
        ))
        .unwrap();
        assert_eq!(built_again(&manifest, &["lab"]), ["features", "thumbs"]);
        assert_eq!(built_again(&manifest, &["mnist"]), ["digits", "features"]);
        assert_eq!(
            built_again(&manifest, &["lab", "mnist"]),
            ["digits", "features", "thumbs"]
        );
        let alone = Manifest::parse("[datasets.lab]\nref = \"local\"\n").unwrap();
        assert_eq!(built_again(&alone, &["lab"]), Vec::<&str>::new());
    }

    /// any one name of the path may be off, and the suggestion keeps the path as typed up to it.
    #[test]
    fn a_typo_is_found_one_name_away_and_kept_as_typed() {
        let (manifest, lock, _folder, cache) = tiny(3, 2);
        let planned = plan::inputs(&manifest, &lock, &cache).unwrap();
        let mut sorted: Vec<(String, &Target)> = planned
            .targets
            .iter()
            .map(|target| (name::fold(&target.path), target))
            .collect();
        sorted.sort_by(|a, b| a.0.cmp(&b.0));
        for (typed, meant) in [
            (
                "data/tiny/d001/f0003.txt",
                Some("data/tiny/d001/f00003.txt"),
            ),
            ("data/tiyn/d002", Some("data/tiny/d002")),
            (
                "data/tiny/D001/F0003.TXT",
                Some("data/tiny/d001/f00003.txt"),
            ),
            ("data/tiny/d001/f00003.txt/x", None),
            ("data/tiny/zzz", None),
        ] {
            assert_eq!(near(&sorted, typed).as_deref(), meant, "{typed}");
        }
        let path = "data/pest/images/x.png";
        let meant = "data/pets/images/x.png";
        for (typed, retyped_as) in [
            ("data/pest/images/x.png", Some("data/pets/images/x.png")),
            (
                "../data/pest/images/x.png/",
                Some("../data/pets/images/x.png"),
            ),
            ("pest/images/x.png", Some("pets/images/x.png")),
            ("pest/images/../images/x.png", None),
        ] {
            let retyped_as = retyped_as.map(str::to_owned);
            assert_eq!(
                retyped(Path::new(typed), path, meant),
                retyped_as,
                "{typed}"
            );
        }
    }

    /// planning looks for a stop every chunk of work, and the look that finds one is the last.
    #[test]
    fn planning_ends_at_the_first_look_after_a_stop() {
        let (manifest, lock, _folder, cache) = tiny(4, 1_000);
        let planned = plan::inputs(&manifest, &lock, &cache).unwrap();
        let paths = ["data/tiny/d001", "data/tiny/d002", "data/tiny/d003"].map(str::to_owned);
        let stop = crate::stop::Stop::default();
        let looks = std::cell::Cell::new(0);
        let poll = || {
            looks.set(looks.get() + 1);
            // a look for each path, then the first inside the work on the first folder
            if looks.get() == 4 {
                stop.ask();
            }
            stop.check()
        };
        let mut pace = Pace::new(&poll);
        let asked = asked(
            &manifest,
            &lock,
            &cache,
            &planned,
            &paths,
            &typed(&paths),
            &pace,
        )
        .unwrap();
        assert_eq!(looks.get(), 3);
        let Err(stopped) = edits(&manifest, &lock, &cache, &asked, &mut pace) else {
            panic!("planning went on after the stop");
        };
        assert_eq!(stopped.kind, Kind::Interrupted);
        assert_eq!((looks.get(), pace.steps), (4, CHUNK));
    }

    #[test]
    fn a_path_is_read_from_the_current_folder_inside_the_data_folder() {
        let root = Path::new("/home/me/study");
        let here = root.join("notes");
        let read = |arg: &str| in_data_folder(root, &here, "data", Path::new(arg));
        assert_eq!(
            read("../data/pets/images/x.png").unwrap(),
            "data/pets/images/x.png"
        );
        assert_eq!(
            read("/home/me/study/data/./pets/../pets/x.png").unwrap(),
            "data/pets/x.png"
        );
        for outside in [
            "x.png",
            "../../other/data/x.png",
            "../dataset/x",
            "/elsewhere/x",
        ] {
            let error = read(outside).unwrap_err();
            assert_eq!(error.kind, Kind::Usage, "{outside}");
            assert!(
                error
                    .message
                    .contains("is not inside the data folder, data/")
            );
        }
    }

    #[test]
    fn a_folder_stands_for_one_prefix_only_where_every_file_shares_it() {
        let chosen = [
            ("data/pets/images/dog/a.png", "pets-v1/images/dog/a.png"),
            (
                "data/pets/images/dog/sub/b.png",
                "pets-v1/images/dog/sub/b.png",
            ),
        ];
        assert_eq!(
            shared_prefix("data/pets/images/dog", &chosen).as_deref(),
            Some("pets-v1/images/dog")
        );
        assert_eq!(
            shared_prefix("data/pets", &[("data/pets/a.png", "a.png")]),
            None,
            "the whole of an origin has no folder of its own"
        );
        let renamed = [
            ("data/x/raw/a.csv", "raw/a.csv"),
            ("data/x/raw/b.csv", "other/b.csv"),
        ];
        assert_eq!(shared_prefix("data/x/raw", &renamed), None);
    }

    /// the pin of `name`, read from `manifest`, holding `files`, each `(path, archive)`.
    fn pinned(manifest: &Manifest, name: &str, files: &[(&str, bool)]) -> Locked {
        let source = manifest.datasets[name].source().unwrap();
        Locked {
            name: name.to_owned(),
            reference: source.reference.clone(),
            spec: lock::spec(source),
            extract: source.extract,
            web_page: false,
            resolved: source.reference.clone(),
            title: None,
            license: None,
            doi: None,
            access: crate::source::Access::Unknown,
            timestamp: "2026-01-01T00:00:00Z".to_owned(),
            files: files
                .iter()
                .map(|(path, archive)| LockedFile {
                    sha256: crate::digest::sha256(path.as_bytes()),
                    path: (*path).to_owned(),
                    size: 1,
                    md5: None,
                    url: String::new(),
                    unpacks_to: archive.then(|| crate::digest::sha256(b"tree")),
                })
                .collect(),
        }
    }

    /// what drop leaves out of one dataset: `exclude` in its own table, the same paths leaving
    /// data.lock, and `members`, patterns for an archive's table.
    fn leaves<'a>(exclude: &[&'a str], members: &[(&'a str, &str)]) -> Edit<'a> {
        let mut by_archive: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
        for (archive, pattern) in members {
            by_archive
                .entry(archive)
                .or_default()
                .insert((*pattern).to_owned());
        }
        Edit {
            exclude: exclude.iter().map(|path| (*path).to_owned()).collect(),
            leaving: exclude.iter().copied().collect(),
            members: by_archive,
            copied: BTreeMap::new(),
            left: Vec::new(),
        }
    }

    const ACROSS: &str = concat!(
        "[project]\ndata_dir = \"data\"\n\n",
        "# raw recordings\n[datasets.alpha]\nref = \"zenodo:1\"\nexclude = \"*.tmp\"  # scratch\n\n",
        "[datasets.beta]\nref = \"zenodo:2\"\n\n",
        "# the glob that picks parts\n[datasets.beta.members.\"*.zip\"]\nselect = [\"a/*\", \"b/*\"]\nexclude = [\"*.log\"]\n\n",
        "[datasets.gamma]\nref = \"zenodo:3\"\n\n",
        "[datasets.gamma.members.\"old.zip\"]\nexclude = [\"junk/*\"]\n\n",
        "# left alone\n[datasets.untouched]\nref = \"zenodo:4\"\n",
    );

    /// an archive a glob covered gets its own table copied from it, one that leaves whole takes its
    /// own table with it, and every comment and the layout around them stay.
    #[test]
    fn a_drop_across_datasets_writes_each_dataset_in_place() {
        let manifest = Manifest::parse(ACROSS).unwrap();
        let lock = Lock {
            datasets: vec![
                pinned(
                    &manifest,
                    "alpha",
                    &[("x.txt", false), ("y.txt", false), ("z.csv", false)],
                ),
                pinned(
                    &manifest,
                    "beta",
                    &[("parts.zip", true), ("other.zip", true)],
                ),
                pinned(&manifest, "gamma", &[("old.zip", true), ("new.zip", true)]),
                pinned(&manifest, "untouched", &[("only.bin", false)]),
            ],
            derived: Vec::new(),
        };
        let mut beta = leaves(&[], &[("parts.zip", "a/one.txt")]);
        beta.copied.insert(
            "parts.zip",
            FromGlob {
                key: "*.zip".to_owned(),
                unmatched: vec!["b/*".to_owned()],
            },
        );
        let edits = BTreeMap::from([
            ("alpha", leaves(&["x.txt", "y.txt"], &[])),
            ("beta", beta),
            ("gamma", leaves(&["old.zip"], &[])),
        ]);
        let go_on = || Ok(());
        let (text, new_lock) =
            written(ACROSS, &manifest, &lock, &edits, &Pace::new(&go_on)).unwrap();
        assert_eq!(
            text,
            concat!(
                "[project]\ndata_dir = \"data\"\n\n",
                "# raw recordings\n[datasets.alpha]\nref = \"zenodo:1\"\nexclude = [\n  \"*.tmp\",\n  \"x.txt\",\n  \"y.txt\",\n]  # scratch\n\n",
                "[datasets.beta]\nref = \"zenodo:2\"\n\n",
                "# the glob that picks parts\n[datasets.beta.members.\"*.zip\"]\nselect = [\"a/*\", \"b/*\"]\nexclude = [\"*.log\"]\n\n",
                "[datasets.beta.members.\"parts.zip\"]\nselect = [\"a/*\"]\nexclude = [\n  \"*.log\",\n  \"a/one.txt\",\n]\n\n",
                "[datasets.gamma]\nref = \"zenodo:3\"\nexclude = [\"old.zip\"]\n\n",
                "# left alone\n[datasets.untouched]\nref = \"zenodo:4\"\n",
            )
        );
        let files = |name: &str| -> Vec<String> {
            new_lock
                .get(name)
                .unwrap()
                .files
                .iter()
                .map(|file| file.path.clone())
                .collect()
        };
        assert_eq!(files("alpha"), ["z.csv"]);
        assert_eq!(files("beta"), ["parts.zip", "other.zip"]);
        assert_eq!(files("gamma"), ["new.zip"]);
        assert_eq!(files("untouched"), ["only.bin"]);
    }

    #[test]
    fn a_drop_across_many_datasets_reads_data_toml_as_often_as_across_few() {
        let reads = |count: usize| {
            let mut text = String::new();
            for at in 0..count {
                writeln!(text, "[datasets.d{at:04}]\nref = \"zenodo:{at}\"\n").unwrap();
            }
            let manifest = Manifest::parse(&text).unwrap();
            let names: Vec<String> = (0..count).map(|at| format!("d{at:04}")).collect();
            let lock = Lock {
                datasets: names
                    .iter()
                    .map(|name| {
                        pinned(
                            &manifest,
                            name,
                            &[("keep.bin", false), ("remove.bin", false)],
                        )
                    })
                    .collect(),
                derived: Vec::new(),
            };
            let edits: BTreeMap<&str, Edit> = names
                .iter()
                .map(|name| (name.as_str(), leaves(&["remove.bin"], &[])))
                .collect();
            let go_on = || Ok(());
            let before = manifest::tests::reads();
            let (text, _) = written(&text, &manifest, &lock, &edits, &Pace::new(&go_on)).unwrap();
            let reads = manifest::tests::reads() - before;
            assert_eq!(text.matches("exclude = [\"remove.bin\"]").count(), count);
            reads
        };
        assert_eq!(
            reads(60),
            reads(3),
            "data.toml is read again for each dataset"
        );
    }

    /// names come in name order, and a dataset whose archive was never unpacked here is not
    /// counted out.
    #[test]
    fn a_drop_across_datasets_refuses_the_first_left_with_no_file() {
        let manifest = Manifest::parse(concat!(
            "[datasets.a]\nref = \"local\"\n\n[datasets.b]\nref = \"local\"\n\n",
            "[datasets.c]\nref = \"local\"\n\n[datasets.d]\nref = \"local\"\n",
        ))
        .unwrap();
        let lock = Lock {
            datasets: vec![
                pinned(
                    &manifest,
                    "a",
                    &[("1.txt", false), ("2.txt", false), ("3.txt", false)],
                ),
                pinned(&manifest, "b", &[("only.txt", false)]),
                pinned(&manifest, "c", &[("1.txt", false), ("2.txt", false)]),
                pinned(&manifest, "d", &[("whole.zip", true)]),
            ],
            derived: Vec::new(),
        };
        let folder = tempfile::tempdir().unwrap();
        let cache = Cache::open(folder.path().join("cache")).unwrap();
        let planned = plan::inputs(&manifest, &lock, &cache).unwrap();
        let left = |dataset: &str, paths: &[&str]| -> Vec<Left> {
            paths
                .iter()
                .map(|path| Left {
                    path: format!("data/{dataset}/{path}"),
                    dataset: dataset.to_owned(),
                    archive: None,
                    pattern: (*path).to_owned(),
                })
                .collect()
        };
        let edit = |dataset: &str, paths: &[&'static str]| {
            let mut edit = leaves(paths, &[]);
            edit.left = left(dataset, paths);
            edit
        };
        let go_on = || Ok(());
        let pace = Pace::new(&go_on);
        let refused = |edits: &BTreeMap<&str, Edit>| {
            refuse_what_breaks(&manifest, &lock, &planned, &cache, edits, &pace)
                .err()
                .map(|error| error.message)
        };
        let mut edits = BTreeMap::from([
            ("a", edit("a", &["1.txt"])),
            ("c", edit("c", &["1.txt", "2.txt"])),
            ("d", edit("d", &["whole.zip"])),
        ]);
        assert_eq!(
            refused(&edits).as_deref(),
            Some("that is every file of c, and a dataset needs one")
        );
        edits.insert("b", edit("b", &["only.txt"]));
        assert_eq!(
            refused(&edits).as_deref(),
            Some("that is every file of b, and a dataset needs one")
        );
        edits.remove("b");
        edits.insert("c", edit("c", &["2.txt"]));
        assert_eq!(refused(&edits), None);
    }
}
