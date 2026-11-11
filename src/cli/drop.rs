//! `fetchloom drop`: files left out of their dataset for good, such as bad samples. it writes the
//! patterns that leave them out into data.toml, rewrites data.lock for files a source lists, and
//! deletes the unchanged files from the data folder. it asks no server.

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
use crate::project::manifest::{self, Manifest};
use crate::units;

/// what `drop` did, or with `--dry-run` would do, as `--json` prints it.
#[derive(Serialize)]
pub(super) struct Report {
    project: PathBuf,
    /// each file left out, by its path in the project.
    dropped: Vec<Left>,
    lock_changed: bool,
    placement: Option<Placement>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    dry_run: bool,
    /// with `--dry-run`, the paths that would change.
    #[serde(skip_serializing_if = "Option::is_none")]
    changes: Option<Changes>,
}

/// one file left out: its dataset, the archive it is inside, if any, and the pattern written
/// for it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct Left {
    path: String,
    dataset: String,
    archive: Option<String>,
    pattern: String,
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
    /// the files left out, as `--json` lists them.
    left: Vec<Left>,
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
    // a dataset with `place = false` is planned where its files would land, so they drop too
    let planned = plan::inputs(&manifest, &lock, &cache)?;
    let asked = asked(&manifest, &lock, &cache, &planned, &paths)?;
    let edits = edits(&manifest, &lock, &cache, &asked)?;
    refuse_what_breaks(&manifest, &lock, &planned, &cache, &edits)?;
    let (updated, new_lock) = written(&text, &lock, &edits)?;
    let new_manifest = Manifest::parse(&updated)?;
    let lock_changed = edits.values().any(|edit| !edit.leaving.is_empty());
    let mut dropped: Vec<Left> = edits.values().flat_map(|edit| edit.left.clone()).collect();
    dropped.sort_by(|a, b| a.path.cmp(&b.path));
    let scope = Scope::Only(edits.keys().map(|name| (*name).to_owned()).collect());
    let survey = Survey::read(&project, &new_manifest.data_dir, context.threads);
    let placing = place::Placing::new(
        &project,
        &new_manifest,
        &new_lock,
        &cache,
        false,
        &scope,
        survey,
    )?;
    let verb = if dry_run { "Would exclude" } else { "Excluded" };
    tell(&context.output, &edits, verb);
    let report = |placement, changes| Report {
        project: project.root.clone(),
        dropped: dropped.clone(),
        lock_changed,
        placement: Some(placement),
        dry_run,
        changes,
    };
    if dry_run {
        let preview = placing.preview_removal();
        place::tell_preview(&context.output, &new_manifest, &scope, &preview);
        dry_run_done(&context.output, None);
        return Ok(report(preview.placement, Some(preview.changes)));
    }
    // a stop asked while the data folder was read leaves the project as it was; once the files
    // change, the drop runs to its end
    context.stop.check()?;
    context.stop.finish();
    project.write(manifest::FILE, updated.as_bytes())?;
    if lock_changed {
        context.wrote_lists(&project.root, project.write_lists(&new_lock)?);
        project.write(lock::FILE, new_lock.render().as_bytes())?;
        project.sweep_lists(&new_lock);
    }
    let placement = placing.remove_only(&context.output).map_err(|error| {
        let written = if lock_changed {
            "data.toml and data.lock were"
        } else {
            "data.toml was"
        };
        error.detail(format!("{written} updated; after fixing the error, `fetchloom sync` removes the files they leave out"))
    })?;
    let changed = if lock_changed {
        committed(&project)
    } else {
        "data.toml"
    };
    context.output.next(&format!(
        "commit {changed}; other clones drop them at their next `fetchloom sync`"
    ));
    Ok(report(placement, None))
}

/// `arg`, a path relative to `here` that may name a file already deleted, as a path in the
/// project with `/` between folders, read without following links.
///
/// # Errors
///
/// `usage` for a path outside the data folder, which holds every file drop can leave out.
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

/// the planned files each of `paths` names: the file at it, or every file under it, compared
/// the way a disk that ignores case compares names.
///
/// # Errors
///
/// `usage` for a path that names no planned file: a file of a recipe's result, one an archive
/// this machine never unpacked may hold, one a pattern leaves out already, or any other.
fn asked<'p, 'a>(
    manifest: &Manifest,
    lock: &Lock,
    cache: &Cache,
    planned: &'p Plan<'a>,
    paths: &[String],
) -> Result<Vec<Asked<'p, 'a>>, Error> {
    let mut sorted: Vec<(String, &'p Target<'a>)> = planned
        .targets
        .iter()
        .map(|target| (name::fold(&target.path), target))
        .collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    let mut asked = Vec::with_capacity(paths.len());
    for path in paths {
        let found = named(&sorted, path);
        if found.targets.is_empty() {
            return Err(unplanned(manifest, lock, cache, planned, path));
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

/// why `path`, in the data folder, names no planned file.
fn unplanned(manifest: &Manifest, lock: &Lock, cache: &Cache, planned: &Plan, path: &str) -> Error {
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
            "run `fetchloom sync` first, so fetchloom knows what {archive} holds"
        ));
    }
    if let Some(by) = left_out(manifest, lock, cache, path) {
        return Error::new(Kind::Usage, format!("{path} is left out already, {by}"))
            .help("to bring it back, delete that pattern from data.toml and run `fetchloom sync`");
    }
    let owners: Vec<&str> = manifest
        .datasets
        .keys()
        .filter(|name| within(name))
        .map(String::as_str)
        .collect();
    let owners = match owners.split_last() {
        None => "any dataset".to_owned(),
        Some((last, [])) => (*last).to_owned(),
        Some((last, rest)) => format!("{} or {last}", rest.join(", ")),
    };
    Error::new(Kind::Usage, format!("{path} names no file of {owners}")).help(
        "check the path; a file of your own needs no drop: delete it yourself if you do not need it",
    )
}

/// the pattern in data.toml that leaves out the file at `path`, in the data folder, as a few
/// words naming it: an exclude or select of a members table, for a file an archive holds, or the
/// dataset's own exclude, for a file its source lists.
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
            let (Some(filter), Some(members)) = (
                source.members.get(&archive.file.path),
                archive.recorded(cache),
            ) else {
                continue;
            };
            let found = members
                .iter()
                .zip(plan::unpacked_paths(archive.path(), alone, &members))
                .find(|(_, placed)| name::fold(&format!("{folder}/{placed}")) == asked);
            if let Some((member, _)) = found.filter(|(member, _)| !filter.keeps(&member.path)) {
                let table = format!("members.\"{}\" of datasets.{name}", archive.file.path);
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

/// the last part of a path, its file name.
fn archive_name(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, name)| name)
}

/// what to write for each dataset `asked` names files of: an escaped path, or `<folder>/**` for
/// a folder whose files it matches exactly, in the dataset's exclude for a file its source
/// lists and in the archive's members table for a file inside an archive. a request for every
/// file an archive keeps leaves out the archive itself.
fn edits<'a>(
    manifest: &Manifest,
    lock: &'a Lock,
    cache: &Cache,
    asked: &[Asked<'_, 'a>],
) -> Result<BTreeMap<&'a str, Edit<'a>>, Error> {
    let mut archives: HashMap<(&str, &str), Inside> = HashMap::new();
    let mut edits: BTreeMap<&'a str, Edit<'a>> = BTreeMap::new();
    let mut seen: HashSet<&str> = HashSet::new();
    for request in asked {
        let mut by_origin: BTreeMap<(&'a str, &'a str), Vec<(&Target<'a>, &'a LockedFile)>> =
            BTreeMap::new();
        // `asked` refuses a recipe's files, so each target comes from a file a source lists
        let listed = request
            .targets
            .iter()
            .filter_map(|target| Some((*target, target.file()?)));
        for (target, file) in listed {
            if seen.insert(target.path.as_str()) {
                by_origin
                    .entry((target.dataset, file.path.as_str()))
                    .or_default()
                    .push((target, file));
            }
        }
        for ((dataset, origin), targets) in by_origin {
            let locked = lock.get(dataset).ok_or_else(|| missing_pin(dataset))?;
            let edit = edits.entry(dataset).or_default();
            if targets[0].1.unpacks_to.is_none() {
                let chosen: Vec<(&str, &str)> = targets
                    .iter()
                    .map(|(target, file)| (target.path.as_str(), file.path.as_str()))
                    .collect();
                let listed = locked.files.iter().map(|file| file.path.as_str());
                let patterns = written_for(request, &chosen, listed);
                record(edit, dataset, None, &chosen, &patterns);
                edit.leaving
                    .extend(targets.iter().map(|(_, file)| file.path.as_str()));
                continue;
            }
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
            let patterns = written_for(request, &chosen, kept);
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
    }
    Ok(edits)
}

/// the error for a planned file whose dataset data.lock does not pin, which planning rules out.
fn missing_pin(dataset: &str) -> Error {
    Error::new(
        Kind::ProjectInvalid,
        format!("data.lock does not pin {dataset}"),
    )
    .help("run `fetchloom sync`, then drop again")
}

/// where the files `locked`'s archive `file` keeps land, with their paths inside it, from what
/// this machine recorded unpacking it to, as planning places them.
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
        .help("run `fetchloom sync`, then drop again")
    })?;
    let filter = manifest
        .datasets
        .get(&locked.name)
        .and_then(|entry| entry.source())
        .and_then(|source| source.members.get(&file.path));
    let folder = plan::folder(manifest, &locked.name);
    let alone = archives::pinned(&locked.files).len() == 1;
    Ok(members
        .iter()
        .zip(plan::unpacked_paths(archive.path(), alone, &members))
        .filter(|(member, _)| filter.is_none_or(|filter| filter.keeps(&member.path)))
        .map(|(member, path)| (format!("{folder}/{path}"), member.path.clone()))
        .collect())
}

/// the patterns that leave out `chosen`, each a path in the project with its path in its origin:
/// `<folder>/**` when `request` is a folder and that pattern matches exactly the chosen paths
/// among `all` the origin keeps, else one escaped path each. each pattern comes with the paths it
/// was written for.
fn written_for<'c>(
    request: &Asked,
    chosen: &[(&'c str, &'c str)],
    all: impl Iterator<Item = &'c str>,
) -> Vec<(Vec<&'c str>, String)> {
    if request.folder
        && let Some(prefix) = shared_prefix(&request.path, chosen)
    {
        let pattern = format!("{}/**", glob::escape(&prefix));
        let wanted: HashSet<&str> = chosen.iter().map(|(_, origin)| *origin).collect();
        if let Ok(glob) = Glob::new(&pattern) {
            let matched: HashSet<&str> = all.filter(|path| glob.matches(path)).collect();
            if matched == wanted {
                return vec![(chosen.iter().map(|(placed, _)| *placed).collect(), pattern)];
            }
        }
    }
    chosen
        .iter()
        .map(|(placed, origin)| (vec![*placed], glob::escape(origin)))
        .collect()
}

/// the folder in the origin's own paths that `folder`, in the project, stands for: what is left
/// of each chosen origin path once the part below `folder` is cut from its end, when that is the
/// same for every one and not empty.
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

/// adds what `patterns` leave out to `edit`'s record for `--json`.
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

/// turns a request for every file an archive keeps into leaving out the archive itself, every
/// part of a split one, in the dataset's exclude, since a members table may not leave its
/// archive no file.
fn whole_archives<'a>(
    locked: &'a Locked,
    edit: &mut Edit<'a>,
    archives: &HashMap<(&str, &str), Inside>,
) {
    let dataset = locked.name.as_str();
    let whole: Vec<&str> = edit
        .members
        .keys()
        .copied()
        .filter(|archive| {
            let left: HashSet<&str> = edit
                .left
                .iter()
                .filter(|left| left.archive.as_deref() == Some(*archive))
                .map(|left| left.path.as_str())
                .collect();
            archives
                .get(&(dataset, *archive))
                .is_some_and(|inside| inside.keys().all(|placed| left.contains(placed.as_str())))
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

/// refuses a drop that would leave a dataset no file, or move files it keeps: a dataset of more
/// than one file left with one archive places that archive's files without their top folder.
/// a file a source lists leaves data.lock offline only while data.lock pins what data.toml asks.
fn refuse_what_breaks(
    manifest: &Manifest,
    lock: &Lock,
    planned: &Plan,
    cache: &Cache,
    edits: &BTreeMap<&str, Edit>,
) -> Result<(), Error> {
    for (dataset, edit) in edits {
        let kept = planned
            .targets
            .iter()
            .filter(|target| target.dataset == *dataset)
            .count();
        let unknown = planned.unknown.iter().any(|(name, _)| name == dataset);
        if edit.left.len() >= kept && !unknown {
            return Err(Error::new(
                Kind::Usage,
                format!("that is every file of {dataset}, and a dataset needs one"),
            )
            .help(format!(
                "to take the whole dataset out of the project, run `fetchloom remove {dataset}`"
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
            .help("run `fetchloom sync` to bring data.lock in line, then drop again"));
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
                "keep the files, or leave them out with exclude in datasets.{dataset} in data.toml and run `fetchloom sync`, which moves the files of {only}"
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

/// data.toml's and data.lock's new content once `edits` are written.
fn written(text: &str, lock: &Lock, edits: &BTreeMap<&str, Edit>) -> Result<(String, Lock), Error> {
    let mut updated = text.to_owned();
    for (dataset, edit) in edits {
        for (archive, patterns) in &edit.members {
            let patterns: Vec<String> = patterns.iter().cloned().collect();
            updated = manifest::with_exclusions(&updated, dataset, Some(archive), &patterns)?;
        }
        for archive in &edit.leaving {
            updated = manifest::without_members(&updated, dataset, archive)?;
        }
        if !edit.exclude.is_empty() {
            let patterns: Vec<String> = edit.exclude.iter().cloned().collect();
            updated = manifest::with_exclusions(&updated, dataset, None, &patterns)?;
        }
    }
    let manifest = Manifest::parse(&updated)?;
    let now = lock::timestamp(std::time::SystemTime::now());
    let mut new_lock = lock.clone();
    for locked in &mut new_lock.datasets {
        let Some(edit) = edits.get(locked.name.as_str()) else {
            continue;
        };
        let Some(source) = manifest
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
    Ok((updated, new_lock))
}

/// a line for each dataset and archive drop leaves files out of, with `-v` naming each file.
fn tell(output: &crate::output::Output, edits: &BTreeMap<&str, Edit>, verb: &str) {
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
