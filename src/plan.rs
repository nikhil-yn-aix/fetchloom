//! where every locked file lands in the project, checked for clashes before anything is written,
//! with names compared as a disk that ignores case does, so a lock placing here places on windows.

use std::collections::{HashMap, HashSet};

use crate::archive;
use crate::cache::{Cache, Member};
use crate::digest::Sha256;
use crate::error::{Error, Kind};
use crate::glob::{self, Glob};
use crate::name;
use crate::project::lock::archives::{self, Archive, Pinned};
use crate::project::lock::derived::Derived;
use crate::project::lock::{Lock, Locked, LockedFile};
use crate::project::manifest::{self, Manifest, MemberFilter, Members, Source};

/// one file a lock places.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Target<'a> {
    pub(crate) dataset: &'a str,
    /// the path in the project, with `/` between folders.
    pub(crate) path: String,
    pub(crate) sha256: Sha256,
    pub(crate) size: u64,
    /// a writable copy, for tools that write into their inputs.
    pub(crate) copy: bool,
    pub(crate) holder: Holder<'a>,
}

/// what in data.lock holds a target's bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Holder<'a> {
    /// a file a source lists, whose download holds them: the file itself, or the archive it
    /// unpacks from.
    File(&'a LockedFile),
    /// a recipe's result, which only `fetchloom build` makes, so nothing downloads it.
    Result,
}

impl<'a> Target<'a> {
    /// the locked file whose download holds the target's bytes, or none for a recipe's result.
    pub(crate) fn file(&self) -> Option<&'a LockedFile> {
        match self.holder {
            Holder::File(file) => Some(file),
            Holder::Result => None,
        }
    }

    fn unpacked(&self) -> bool {
        self.file().is_some_and(|file| file.unpacks_to.is_some())
    }

    /// the path in its dataset of the archive the target unpacks from, if it is in one.
    fn archive(&self) -> Option<&'a str> {
        self.file()
            .filter(|file| file.unpacks_to.is_some())
            .map(|file| file.path.as_str())
    }
}

/// every file a lock places, and the archives whose files this machine does not know yet.
#[derive(Debug, Default)]
pub(crate) struct Plan<'a> {
    pub(crate) targets: Vec<Target<'a>>,
    /// archives never unpacked on this machine, with their dataset, so the files they hold
    /// cannot be planned until they are.
    pub(crate) unknown: Vec<(&'a str, &'a LockedFile)>,
}

/// the folder a dataset lands in, relative to the project: the data folder itself for
/// `path = "."`.
pub(crate) fn folder(manifest: &Manifest, name: &str) -> String {
    match subfolder(manifest, name) {
        None => manifest.data_dir.clone(),
        Some(inner) => format!("{}/{inner}", manifest.data_dir),
    }
}

/// the folder a dataset lands in below the data folder, or none for `path = "."`.
pub(crate) fn subfolder<'a>(manifest: &'a Manifest, name: &'a str) -> Option<&'a str> {
    match manifest
        .datasets
        .get(name)
        .and_then(|entry| entry.path.as_deref())
    {
        Some(manifest::DATA_FOLDER_ITSELF) => None,
        inner => Some(inner.unwrap_or(name)),
    }
}

/// the plan for `lock`, with each archive standing for the files the cache records it unpacked
/// to.
pub(crate) fn from_cache<'a>(
    manifest: &Manifest,
    lock: &'a Lock,
    cache: &Cache,
) -> Result<Plan<'a>, Error> {
    plan(manifest, lock, &|archive| archive.recorded(cache))
}

/// [`from_cache`] for an add of `added`, whose patterns came from its command line: a clash names
/// those options, since a failed add leaves data.toml without them.
pub(crate) fn from_cache_adding<'a>(
    manifest: &Manifest,
    lock: &'a Lock,
    cache: &Cache,
    added: &[String],
) -> Result<Plan<'a>, Error> {
    let added: HashSet<&str> = added.iter().map(String::as_str).collect();
    let patterns = |dataset: &str| {
        if added.contains(dataset) {
            Patterns::OnCommandLine
        } else {
            Patterns::InDataToml
        }
    };
    planned(
        manifest,
        &lock.datasets,
        &lock.derived,
        &|archive| archive.recorded(cache),
        &patterns,
    )
}

/// every file of `lock`'s fetched datasets as a build reads them, those `place = false` keeps out
/// laid out where they would land. only placed files are checked for clashes.
pub(crate) fn inputs<'a>(
    manifest: &Manifest,
    lock: &'a Lock,
    cache: &Cache,
) -> Result<Plan<'a>, Error> {
    let members = |archive: &Archive<'_>| archive.recorded(cache);
    let mut plan = plan(manifest, lock, &members)?;
    for dataset in &lock.datasets {
        let Some((entry, source)) = manifest
            .datasets
            .get(&dataset.name)
            .filter(|entry| !entry.place)
            .and_then(|entry| Some((entry, entry.source()?)))
        else {
            continue;
        };
        if dataset.drift(entry).is_none() {
            archives_named(&dataset.name, source, dataset)?;
        }
        let folder = folder(manifest, &dataset.name);
        expand(
            &mut plan,
            &dataset.name,
            &folder,
            entry.copy,
            &dataset.files,
            &source.members,
            &members,
        )?;
    }
    Ok(plan)
}

/// refuses, before anything downloads, a clash among the files of `datasets` the cache's records
/// already place. an archive with no record is checked once it is unpacked, before any placing.
pub(crate) fn foresee<'a>(
    manifest: &Manifest,
    datasets: impl IntoIterator<Item = &'a Locked>,
    cache: &Cache,
) -> Result<(), Error> {
    // in data.lock's order, so a clash names its datasets as the later check does
    let mut datasets: Vec<&Locked> = datasets.into_iter().collect();
    datasets.sort_by(|a, b| a.name.cmp(&b.name));
    let members = |archive: &Archive<'_>| archive.recorded(cache);
    planned(manifest, datasets, &[], &members, &|_| Patterns::InDataToml).map(drop)
}

/// every file the lock places for data.toml's datasets, archives as `members` recorded them, or
/// the first clash: datasets may share a folder, never a path. `place = false` places nothing.
pub(crate) fn plan<'a>(
    manifest: &Manifest,
    lock: &'a Lock,
    members: &dyn Fn(&Archive<'_>) -> Option<Vec<Member>>,
) -> Result<Plan<'a>, Error> {
    let patterns = |_: &str| Patterns::InDataToml;
    planned(manifest, &lock.datasets, &lock.derived, members, &patterns)
}

/// [`plan`] for `datasets` and `results`, telling a clash in each dataset as `patterns` says
/// where its patterns were written.
fn planned<'a>(
    manifest: &Manifest,
    datasets: impl IntoIterator<Item = &'a Locked>,
    results: &'a [Derived],
    members: &dyn Fn(&Archive<'_>) -> Option<Vec<Member>>,
    patterns: &dyn Fn(&str) -> Patterns,
) -> Result<Plan<'a>, Error> {
    let span = tracing::debug_span!("plan", files = tracing::field::Empty).entered();
    let mut plan = Plan::default();
    for dataset in datasets {
        // a recipe's files come only from a build, never from a source pin left under its name,
        // and a dataset with `place = false` has no files in the data folder at all
        let Some((entry, source)) = manifest
            .datasets
            .get(&dataset.name)
            .filter(|entry| entry.place)
            .and_then(|entry| Some((entry, entry.source()?)))
        else {
            continue;
        };
        let folder = folder(manifest, &dataset.name);
        // a pin data.toml has moved past is planned as it stands until sync pins again, so its
        // member filters may not fit it yet
        let current = dataset.drift(entry).is_none();
        if current {
            archives_named(&dataset.name, source, dataset)?;
        }
        let (targets, unknown) = (plan.targets.len(), plan.unknown.len());
        let expanded = expand(
            &mut plan,
            &dataset.name,
            &folder,
            entry.copy,
            &dataset.files,
            &source.members,
            members,
        );
        if let Err(error) = expanded {
            if current {
                return Err(error);
            }
            plan.targets.truncate(targets);
            plan.unknown.truncate(unknown);
            let unfiltered = Members::new();
            expand(
                &mut plan,
                &dataset.name,
                &folder,
                entry.copy,
                &dataset.files,
                &unfiltered,
                members,
            )?;
        }
    }
    for result in results {
        // a fetched dataset's files come only from its source, never from a result left under
        // its name
        let Some(entry) = manifest
            .datasets
            .get(&result.name)
            .filter(|entry| entry.recipe().is_some() && entry.place)
        else {
            continue;
        };
        let folder = folder(manifest, &result.name);
        plan.targets
            .extend(result.files.iter().map(|member| Target {
                dataset: &result.name,
                path: name::joined(&folder, &member.path),
                sha256: member.sha256,
                size: member.size,
                copy: entry.copy,
                holder: Holder::Result,
            }));
    }
    check_by(&plan.targets, patterns)?;
    span.record("files", plan.targets.len());
    Ok(plan)
}

/// refuses a members table whose key names or, as a glob, matches no archive the dataset's pin
/// unpacks, as when an upgrade renamed it: the files it left out would come back without a word.
fn archives_named(name: &str, source: &Source, locked: &Locked) -> Result<(), Error> {
    let archives: Vec<&str> = locked
        .files
        .iter()
        .filter(|file| file.unpacks_to.is_some())
        .map(|file| file.path.as_str())
        .collect();
    let Some((archive, filter)) = source.members.unmatched(&archives).next() else {
        return Ok(());
    };
    let error = Error::new(
        Kind::ProjectInvalid,
        format!("members.\"{archive}\" in datasets.{name} names no archive data.lock pins for it"),
    );
    let error = match filter.lines.get("members") {
        Some(line) => error.detail(line.clone()),
        None => error,
    };
    Err(error.help(match archives.first() {
        Some(first) => format!("name an archive by its path in data.lock, such as \"{first}\""),
        None => format!(
            "datasets.{name} has no archive fetchloom unpacks, so remove its members tables"
        ),
    }))
}

/// adds where one dataset's files land in `folder`, an archive standing for the files `members`
/// says it unpacked to that its table in `filters` keeps. each kept file lands where the whole
/// archive puts it, so a filter never moves a file; a split archive stands at its first part.
/// `project.invalid` refuses an archive two glob keys match, an own table whose select matches
/// none of its files or that keeps none, and a glob key's select matching no file of any archive
/// it covers, once this machine knows them all.
pub(crate) fn expand<'a>(
    plan: &mut Plan<'a>,
    dataset: &'a str,
    folder: &str,
    copy: bool,
    files: &'a [LockedFile],
    filters: &Members,
    members: &dyn Fn(&Archive<'_>) -> Option<Vec<Member>>,
) -> Result<(), Error> {
    let pinned = archives::pinned(files);
    let alone = pinned.len() == 1;
    let mut covered: Vec<Covered<'_, 'a>> = Vec::new();
    for each in pinned {
        let archive = match each {
            Pinned::File(file) => {
                plan.targets.push(Target {
                    dataset,
                    path: name::joined(folder, &file.path),
                    sha256: file.sha256,
                    size: file.size,
                    copy,
                    holder: Holder::File(file),
                });
                continue;
            }
            Pinned::Archive(archive) => archive,
        };
        let file = archive.file;
        let target = |path: &str, sha256, size| Target {
            dataset,
            path: name::joined(folder, path),
            sha256,
            size,
            copy,
            holder: Holder::File(file),
        };
        // before the archive's files are known, so two globs stop a run before it downloads
        let filter = filters.of(dataset, &file.path)?;
        let glob = filter
            .filter(|(key, _)| *key != file.path)
            .map(|(key, filter)| Covered::find(&mut covered, key, filter));
        let Some(members) = members(&archive) else {
            if let Some(glob) = glob {
                covered[glob].archives += 1;
                covered[glob].unknown = true;
            }
            plan.unknown.push((dataset, file));
            continue;
        };
        if let (Some((key, filter)), None) = (filter, glob) {
            selected(dataset, key, &file.path, filter, &members)?;
        }
        let before = plan.targets.len();
        for (member, path) in members
            .iter()
            .zip(unpacked_paths(archive.path(), alone, &members))
        {
            if filter.is_none_or(|(_, filter)| filter.keeps(&member.path)) {
                plan.targets.push(target(&path, member.sha256, member.size));
            }
        }
        let kept = plan.targets.len() > before;
        if let Some(glob) = glob {
            covered[glob].add(&file.path, members, kept);
            continue;
        }
        if let Some((key, filter)) = filter
            && !kept
            && !members.is_empty()
        {
            return Err(left_nothing(
                dataset,
                (key, filter),
                archive_name(&file.path),
                &format!(
                    "loosen the patterns, or exclude \"{}\" in datasets.{dataset} to leave the whole archive out",
                    crate::glob::escape(&file.path)
                ),
            ));
        }
    }
    for glob in covered {
        glob.refuse(dataset)?;
    }
    Ok(())
}

/// the refusal of `table`, a key and its table, whose select and exclude leave no file of
/// `what`, with `help`.
fn left_nothing(
    dataset: &str,
    (key, filter): (&str, &MemberFilter),
    what: &str,
    help: &str,
) -> Error {
    let error = Error::new(
        Kind::ProjectInvalid,
        format!(
            "select and exclude in members.\"{key}\" of datasets.{dataset} leave no file of {what}"
        ),
    );
    let line = ["exclude", "select", "members"]
        .iter()
        .find_map(|key| filter.lines.get(key));
    match line {
        Some(line) => error.detail(line.clone()),
        None => error,
    }
    .help(help.to_owned())
}

/// the archives a glob key's table covers in one dataset, which select patterns their files match
/// and whether it keeps a file. shards differ, so a pattern needs a match in one of them, and only
/// a table that keeps no file of any is a mistake.
struct Covered<'m, 'a> {
    key: &'m str,
    filter: &'m MemberFilter,
    archives: u64,
    /// whether an archive it covers is not unpacked on this machine yet, which may hold a match.
    unknown: bool,
    /// for each select pattern, whether a file of an archive matches it.
    matched: Vec<bool>,
    /// whether the table keeps a file of an archive it covers, and whether they hold any.
    kept: bool,
    held: bool,
    /// the first archive it covers whose files are known, with them, for the refusal's help.
    first: Option<(&'a str, Vec<Member>)>,
}

impl<'m, 'a> Covered<'m, 'a> {
    /// the place in `covered` of the glob key `key` with its table `filter`, added when new.
    fn find(covered: &mut Vec<Covered<'m, 'a>>, key: &'m str, filter: &'m MemberFilter) -> usize {
        if let Some(at) = covered.iter().position(|glob| glob.key == key) {
            return at;
        }
        covered.push(Covered {
            key,
            filter,
            archives: 0,
            unknown: false,
            matched: vec![false; filter.select.globs().len()],
            kept: false,
            held: false,
            first: None,
        });
        covered.len() - 1
    }

    /// counts the archive at `archive`, which holds `members`, the patterns they match, and
    /// whether the table `kept` a file of it.
    fn add(&mut self, archive: &'a str, members: Vec<Member>, kept: bool) {
        self.archives += 1;
        self.kept |= kept;
        self.held |= !members.is_empty();
        let paths: HashSet<&str> = members.iter().map(|member| member.path.as_str()).collect();
        for (glob, matched) in self.filter.select.globs().iter().zip(&mut self.matched) {
            *matched = *matched || holds(&members, &paths, glob);
        }
        if self.first.is_none() {
            self.first = Some((archive, members));
        }
    }

    /// refuses, once every archive it covers is known, the first select pattern none of them
    /// matched, or the table when it keeps no file of any of them.
    fn refuse(self, dataset: &str) -> Result<(), Error> {
        let Some((archive, members)) = self.first.filter(|_| !self.unknown) else {
            return Ok(());
        };
        let inside = format!(
            "the {} it covers",
            crate::units::count(self.archives, "archive", "archives")
        );
        if let Some(at) = self.matched.iter().position(|matched| !matched) {
            let unmatched = &self.filter.select.globs()[at];
            return Err(unmatched_select(
                dataset,
                (self.key, self.filter),
                &inside,
                unmatched,
                (archive, &members),
            ));
        }
        if self.held && !self.kept {
            return Err(left_nothing(
                dataset,
                (self.key, self.filter),
                &inside,
                &format!(
                    "loosen the patterns, or exclude \"{}\" in datasets.{dataset} to leave every archive it covers out",
                    self.key
                ),
            ));
        }
        Ok(())
    }
}

/// whether a file among `members`, whose paths are `paths`, matches `glob`: looked up when it
/// names one path, as drop writes one.
fn holds(members: &[Member], paths: &HashSet<&str>, glob: &Glob) -> bool {
    match glob.literal() {
        Some(path) => paths.contains(path.as_str()),
        None => members.iter().any(|member| glob.matches(&member.path)),
    }
}

/// refuses a select pattern of `filter`, the table under `key` of the archive at `archive`, that
/// matches no file the archive holds.
fn selected(
    dataset: &str,
    key: &str,
    archive: &str,
    filter: &MemberFilter,
    members: &[Member],
) -> Result<(), Error> {
    let paths: HashSet<&str> = members.iter().map(|member| member.path.as_str()).collect();
    match filter
        .select
        .globs()
        .iter()
        .find(|glob| !holds(members, &paths, glob))
    {
        Some(unmatched) => Err(unmatched_select(
            dataset,
            (key, filter),
            archive_name(archive),
            unmatched,
            (archive, members),
        )),
        None => Ok(()),
    }
}

/// the refusal of `unmatched`, a select pattern of `table` matching no file `inside` says where,
/// showing a file of `example` or the pattern that would match below the archive's top folder.
fn unmatched_select(
    dataset: &str,
    (key, filter): (&str, &MemberFilter),
    inside: &str,
    unmatched: &Glob,
    (archive, members): (&str, &[Member]),
) -> Error {
    let error = Error::new(
        Kind::ProjectInvalid,
        format!(
            "the select pattern \"{}\" in members.\"{key}\" of datasets.{dataset} matches no file inside {inside}",
            unmatched.as_str()
        ),
    );
    let error = match filter.lines.get("select") {
        Some(line) => error.detail(line.clone()),
        None => error,
    };
    let name = archive_name(archive);
    let topped = common_top(members)
        .map(|top| format!("{top}/{}", unmatched.as_str().trim_start_matches('/')))
        .filter(|topped| {
            Glob::new(topped)
                .is_ok_and(|glob| members.iter().any(|member| glob.matches(&member.path)))
        });
    error.help(match (topped, members.first()) {
        (Some(topped), _) => {
            format!("try \"{topped}\": paths inside an archive start with its top folder")
        }
        (None, Some(example)) => format!("paths inside {name} look like \"{}\"", example.path),
        (None, None) => format!("{name} holds no files"),
    })
}

fn archive_name(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, name)| name)
}

/// a dataset exclude pattern matching files inside an archive, which it leaves where they are,
/// since it chooses among the files the source lists, though its writer likely meant them out.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Unreached {
    pub(crate) dataset: String,
    pub(crate) pattern: String,
    /// the archives the files are inside, by their paths in data.lock, each once.
    pub(crate) archives: Vec<String>,
    /// the files, by their paths in the project.
    pub(crate) paths: Vec<String>,
}

/// the exclude patterns of each dataset a run listed again that match no file its source
/// listed. data.lock keeps no file an exclude left out, so only a listing tells this.
pub(crate) type Unlisted = HashMap<String, Vec<String>>;

/// each exclude of a dataset that matches a planned file inside an archive by its path below the
/// dataset's folder, unless it left out a listed file, as `unlisted` tells, and so did its job. a
/// dataset `unlisted` lacks has none.
pub(crate) fn unreached(
    manifest: &Manifest,
    targets: &[Target],
    unlisted: &Unlisted,
) -> Vec<Unreached> {
    let mut found: Vec<Unreached> = Vec::new();
    let mut by_dataset: HashMap<&str, Option<(String, glob::Set)>> = HashMap::new();
    let inside_archives = targets.iter().filter_map(|target| {
        Some((
            target,
            target.file().filter(|file| file.unpacks_to.is_some())?,
        ))
    });
    for (target, archive) in inside_archives {
        let Some((inside, excluded)) = by_dataset
            .entry(target.dataset)
            .or_insert_with(|| {
                let source = manifest.datasets.get(target.dataset)?.source()?;
                let missed = unlisted.get(target.dataset)?;
                let excluded: Vec<Glob> = source
                    .exclude
                    .iter()
                    .filter(|glob| missed.iter().any(|pattern| pattern == glob.as_str()))
                    .cloned()
                    .collect();
                let inside = format!("{}/", folder(manifest, target.dataset));
                (!excluded.is_empty()).then(|| (inside, glob::Set::new(excluded)))
            })
            .as_ref()
        else {
            continue;
        };
        let Some(glob) = target
            .path
            .strip_prefix(inside.as_str())
            .and_then(|path| excluded.matching(path))
        else {
            continue;
        };
        let at = found
            .iter()
            .position(|seen| seen.dataset == target.dataset && seen.pattern == glob.as_str())
            .unwrap_or_else(|| {
                found.push(Unreached {
                    dataset: target.dataset.to_owned(),
                    pattern: glob.as_str().to_owned(),
                    archives: Vec::new(),
                    paths: Vec::new(),
                });
                found.len() - 1
            });
        let unreached = &mut found[at];
        if !unreached.archives.contains(&archive.path) {
            unreached.archives.push(archive.path.clone());
        }
        unreached.paths.push(target.path.clone());
    }
    found
}

/// where the patterns that decide a dataset's files are set, so a clash says where to change
/// them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Patterns {
    InDataToml,
    OnCommandLine,
}

/// where an archive's files land in its dataset's folder, in `members`' order: a dataset that is
/// one archive gets them without their shared top folder, as nix's fetchzip does; one beside other
/// files keeps a lone top folder or file as `unzip` would, else a folder named after the archive.
pub(crate) fn unpacked_paths(archive: &str, alone: bool, members: &[Member]) -> Vec<String> {
    let (beside, name) = archive.rsplit_once('/').unwrap_or(("", archive));
    let top = common_top(members);
    if alone {
        return match top {
            Some(top) => place_under(beside, members, |path| &path[top.len() + 1..]),
            None => place_under(beside, members, |path| path),
        };
    }
    let lone = top.is_some() || matches!(members, [only] if !only.path.contains('/'));
    let inside = if lone {
        beside.to_owned()
    } else {
        name::joined(beside, archive::stem(name))
    };
    place_under(&inside, members, |path| path)
}

fn place_under(folder: &str, members: &[Member], rest: impl Fn(&str) -> &str) -> Vec<String> {
    members
        .iter()
        .map(|member| name::joined(folder, rest(&member.path)))
        .collect()
}

/// the one top folder every member sits under, when there is one.
fn common_top(members: &[Member]) -> Option<&str> {
    let (top, _) = members.first()?.path.split_once('/')?;
    members
        .iter()
        .all(|member| {
            member
                .path
                .strip_prefix(top)
                .is_some_and(|rest| rest.starts_with('/'))
        })
        .then_some(top)
}

/// how a case-insensitive disk sees a name.
fn key(path: &str) -> String {
    name::fold(path)
}

/// a warning and its help for each dataset whose `path` ends in the name of a file placed straight
/// inside it, as `path = "train.csv"` gives data/train.csv/train.csv: `path` names a folder.
pub(crate) fn file_shaped_paths(manifest: &Manifest, plan: &Plan) -> Vec<(String, String)> {
    let mut warnings = Vec::new();
    for (dataset, entry) in &manifest.datasets {
        let Some(path) = entry
            .path
            .as_deref()
            .filter(|path| *path != manifest::DATA_FOLDER_ITSELF)
        else {
            continue;
        };
        let (parent, name) = path
            .rsplit_once('/')
            .unwrap_or((manifest::DATA_FOLDER_ITSELF, path));
        let inside = format!("{}/", folder(manifest, dataset));
        let named = key(name);
        let Some(file) = plan.targets.iter().find_map(|target| {
            let file = target.path.strip_prefix(&inside)?;
            (target.dataset == dataset && !file.contains('/') && key(file) == named).then_some(file)
        }) else {
            continue;
        };
        let meant = match parent {
            manifest::DATA_FOLDER_ITSELF => format!("{}/{file}", manifest.data_dir),
            parent => format!("{}/{parent}/{file}", manifest.data_dir),
        };
        warnings.push((
            format!("path = \"{path}\" makes a folder named {name}, so the file is {inside}{file}"),
            format!(
                "path names a folder; to put the file at {meant}, set path = \"{parent}\" on datasets.{dataset} in data.toml"
            ),
        ));
    }
    warnings
}

/// the sorted paths below the folder of `locked` that windows or a fat disk cannot store, as
/// [`name::unportable`] finds them among its files and kept archive members, only to be told.
pub(crate) fn unportable(
    manifest: &Manifest,
    locked: &Locked,
    members: &dyn Fn(&Archive<'_>) -> Option<Vec<Member>>,
) -> Vec<String> {
    let Some(source) = manifest
        .datasets
        .get(&locked.name)
        .and_then(manifest::Entry::source)
    else {
        return Vec::new();
    };
    let folder = folder(manifest, &locked.name);
    let mut plan = Plan::default();
    // a members table that no longer fits is refused where the plan is made
    if expand(
        &mut plan,
        &locked.name,
        &folder,
        false,
        &locked.files,
        &source.members,
        members,
    )
    .is_err()
    {
        return Vec::new();
    }
    let inside = format!("{folder}/");
    let mut paths: Vec<String> = plan
        .targets
        .into_iter()
        .filter_map(|target| {
            let path = target.path.strip_prefix(&inside)?;
            name::unportable(path).map(|_| path.to_owned())
        })
        .collect();
    paths.sort_unstable();
    paths
}

/// refuses targets that cannot all be stored: a name this machine cannot store, two files on one
/// path as a disk that ignores case sees it, or a file where a folder must be.
pub(crate) fn check(targets: &[Target], patterns: Patterns) -> Result<(), Error> {
    check_by(targets, &|_| patterns)
}

/// [`check`], with where each dataset's patterns were written as `patterns` says.
fn check_by(targets: &[Target], patterns: &dyn Fn(&str) -> Patterns) -> Result<(), Error> {
    let mut files: HashMap<String, &Target> = HashMap::with_capacity(targets.len());
    let mut folders: HashMap<String, &Target> = HashMap::new();
    for target in targets {
        if let Some(reason) = name::metadata(&target.path) {
            return Err(Error::new(
                Kind::ProjectInvalid,
                format!("{} is refused because {reason}", target.path),
            )
            .help(leave_out("it", target, patterns(target.dataset))));
        }
        if let Some(reason) = name::unstorable(&target.path) {
            return Err(Error::new(
                Kind::ProjectInvalid,
                format!("{} cannot be stored on this machine: {reason}", target.path),
            )
            .help(leave_out("it", target, patterns(target.dataset))));
        }
        let path_key = key(&target.path);
        if let Some(other) = files.insert(path_key.clone(), target) {
            return Err(clash(other, target, targets, patterns));
        }
        let mut parent = path_key.as_str();
        while let Some((folder, _)) = parent.rsplit_once('/') {
            // a folder seen before had its own parents recorded with it
            if folders.contains_key(folder) {
                break;
            }
            folders.insert(folder.to_owned(), target);
            parent = folder;
        }
    }
    let file_keys: HashSet<&String> = files.keys().collect();
    for (folder, inside) in &folders {
        if file_keys.contains(folder) {
            let file = files[folder];
            let message = if file.dataset == inside.dataset {
                format!(
                    "{} is a file, but {} needs a folder there",
                    file.path, inside.path
                )
            } else {
                format!(
                    "dataset {} places a file at {}, but dataset {} needs a folder there for {}",
                    file.dataset, file.path, inside.dataset, inside.path
                )
            };
            let help = fix(file, inside, targets, patterns);
            return Err(Error::new(Kind::ProjectInvalid, message).help(help));
        }
    }
    Ok(())
}

fn clash(
    first: &Target,
    second: &Target,
    targets: &[Target],
    patterns: &dyn Fn(&str) -> Patterns,
) -> Error {
    let datasets = format!("datasets {} and {}", first.dataset, second.dataset);
    let message = match (first.path == second.path, first.dataset == second.dataset) {
        (true, true) => match (first.archive(), second.archive()) {
            (Some(one), Some(other)) if one != other => format!(
                "two files would land on {}: one from {one} and one from {other}",
                first.path
            ),
            _ => format!("two files would land on {}", first.path),
        },
        (true, false) => format!("{datasets} would both place {}", first.path),
        (false, true) => format!(
            "{} and {} would be the same file on a disk that ignores case",
            first.path, second.path
        ),
        (false, false) => format!(
            "{datasets} would place {} and {}, the same file on a disk that ignores case",
            first.path, second.path
        ),
    };
    Error::new(Kind::ProjectInvalid, message).help(fix(first, second, targets, patterns))
}

/// how to resolve a clash of `first` and `second`: a folder of its own for one of two datasets,
/// or within one dataset an archive kept packed or a file left out.
fn fix(
    first: &Target,
    second: &Target,
    targets: &[Target],
    patterns: &dyn Fn(&str) -> Patterns,
) -> String {
    if first.dataset != second.dataset {
        return format!(
            "give datasets.{} or datasets.{} another path in data.toml",
            first.dataset, second.dataset
        );
    }
    let patterns = patterns(first.dataset);
    let unpacked = [first, second].into_iter().find(|target| target.unpacked());
    let listed = [first, second]
        .into_iter()
        .find(|target| !target.unpacked())
        .and_then(Target::file);
    if let (Some(member), Some(listed)) = (unpacked, listed) {
        return beside(member, listed, targets, patterns);
    }
    let left_out = leave_out("one of them", unpacked.unwrap_or(second), patterns);
    match (first.archive(), second.archive()) {
        (Some(one), Some(other)) if one != other => format!(
            "{left_out}, or give each archive a dataset of its own, whose folder keeps its files apart"
        ),
        _ => left_out,
    }
}

/// how to resolve a clash in one dataset between `member`, inside an archive, and `listed` beside
/// it: leave out either, the member only while its archive keeps another file, or keep it packed.
fn beside(member: &Target, listed: &LockedFile, targets: &[Target], patterns: Patterns) -> String {
    let dataset = member.dataset;
    let archive = member.file().map_or("", |file| file.path.as_str());
    let named = format!("the {} beside {archive}", listed.path);
    if patterns == Patterns::OnCommandLine {
        return format!(
            "leave out {named} with {}, or keep {archive} packed with --no-extract",
            glob::exclusion_flag(&listed.path)
        );
    }
    let exclude = toml_edit::Value::from(glob::escape(&listed.path));
    let packed = format!("set extract = false on datasets.{dataset} to keep its archives packed");
    let others = targets
        .iter()
        .any(|target| target.holder == member.holder && target.path != member.path);
    if others {
        format!(
            "leave out {named} with exclude = [{exclude}] in datasets.{dataset}, or the one inside it with exclude in [datasets.{dataset}.members.\"{archive}\"], or {packed}"
        )
    } else {
        format!("leave out {named} with exclude = [{exclude}] in datasets.{dataset}, or {packed}")
    }
}

fn leave_out(what: &str, target: &Target, patterns: Patterns) -> String {
    let archive = match target.holder {
        Holder::Result => {
            return format!(
                "change the step that makes datasets.{} so it writes other names",
                target.dataset
            );
        }
        Holder::File(file) => file.unpacks_to.is_some().then_some(file.path.as_str()),
    };
    match (archive, patterns) {
        (Some(archive), Patterns::InDataToml) => format!(
            "leave {what} out with exclude in [datasets.{dataset}.members.\"{archive}\"] in data.toml, or set extract = false on datasets.{dataset} to keep its archives packed",
            dataset = target.dataset,
        ),
        (Some(_), Patterns::OnCommandLine) => {
            "pass --no-extract to keep archives packed".to_owned()
        }
        (None, Patterns::InDataToml) => format!(
            "leave {what} out with an exclude pattern in datasets.{} in data.toml",
            target.dataset
        ),
        (None, Patterns::OnCommandLine) => format!("leave {what} out with --exclude"),
    }
}

#[cfg(test)]
mod tests;
