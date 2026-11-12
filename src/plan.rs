//! where every locked file lands in the project, checked for clashes before anything is written.
//! names are compared the way a case-insensitive disk compares them, so a lock that places
//! cleanly here places cleanly on windows too.

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
    /// what in data.lock holds these bytes.
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

    /// whether the target is a file an archive unpacks to.
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

/// [`from_cache`] for an add of the datasets `added`, whose patterns came from its command line,
/// so a clash among their files names the add's options: a failed add leaves data.toml without
/// them.
pub(crate) fn from_cache_adding<'a>(
    manifest: &Manifest,
    lock: &'a Lock,
    cache: &Cache,
    added: &[String],
) -> Result<Plan<'a>, Error> {
    let patterns = |dataset: &str| {
        if added.iter().any(|name| name == dataset) {
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

/// every file of `lock`'s fetched datasets as a build reads them: [`from_cache`]'s, then those of
/// datasets `place = false` keeps out of the data folder, laid out where they would land. only
/// the placed ones are checked for clashes, since only they reach the data folder.
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

/// refuses, before anything is downloaded, a clash among the files of `datasets` whose place is
/// already known, as the cache records what each archive unpacked to. an archive with no record
/// is planned once it is unpacked, and checked then, before anything is placed.
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

/// every file the lock places for the datasets data.toml lists, or the first clash: the files of
/// each source, an archive replaced by the files `members` says it unpacked to, and the result
/// of each recipe as data.lock pins it. a dataset with `place = false` places nothing. datasets
/// may share a folder, but no two files may share a path.
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
                path: join(&folder, &member.path),
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
/// says it unpacked to that its table in `filters` keeps. where each kept file lands is decided
/// from every file the archive holds, so a filter never moves another file. a split archive
/// stands at its first part, and its other parts land nowhere.
///
/// # Errors
///
/// `project.invalid` for an archive two glob keys match; for an archive's own table whose select
/// matches no file of it, or that leaves it no file; and for a glob key's select pattern that
/// matches no file of any archive the key covers, once this machine knows them all.
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
                    path: join(folder, &file.path),
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
            path: join(folder, path),
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
        if let Some(glob) = glob {
            covered[glob].add(&file.path, members);
            continue;
        }
        if let Some((key, filter)) = filter
            && plan.targets.len() == before
            && !members.is_empty()
        {
            let error = Error::new(
                Kind::ProjectInvalid,
                format!(
                    "select and exclude in members.\"{key}\" of datasets.{dataset} leave no file of {}",
                    archive_name(&file.path)
                ),
            );
            let line = ["exclude", "select", "members"]
                .iter()
                .find_map(|key| filter.lines.get(key));
            return Err(match line {
                Some(line) => error.detail(line.clone()),
                None => error,
            }
            .help(format!(
                "loosen the patterns, or exclude \"{}\" in datasets.{dataset} to leave the whole archive out",
                crate::glob::escape(&file.path)
            )));
        }
    }
    for glob in covered {
        glob.refuse_unmatched(dataset)?;
    }
    Ok(())
}

/// the archives a glob key's table covers in one dataset, and which of its select patterns a
/// file of them matches: shards may differ, so a pattern needs a match in one of them, not in
/// each, and a shard it leaves no file places none.
struct Covered<'m, 'a> {
    key: &'m str,
    filter: &'m MemberFilter,
    archives: u64,
    /// whether an archive it covers is not unpacked on this machine yet, which may hold a match.
    unknown: bool,
    /// for each select pattern, whether a file of an archive matches it.
    matched: Vec<bool>,
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
            first: None,
        });
        covered.len() - 1
    }

    /// counts the archive at `archive`, which holds `members`, and the patterns they match.
    fn add(&mut self, archive: &'a str, members: Vec<Member>) {
        self.archives += 1;
        let paths: HashSet<&str> = members.iter().map(|member| member.path.as_str()).collect();
        for (glob, matched) in self.filter.select.globs().iter().zip(&mut self.matched) {
            *matched = *matched || holds(&members, &paths, glob);
        }
        if self.first.is_none() {
            self.first = Some((archive, members));
        }
    }

    /// refuses the first select pattern no archive matched, once every archive is known.
    fn refuse_unmatched(self, dataset: &str) -> Result<(), Error> {
        let Some(at) = self.matched.iter().position(|matched| !matched) else {
            return Ok(());
        };
        let Some((archive, members)) = self.first.filter(|_| !self.unknown) else {
            return Ok(());
        };
        let inside = format!(
            "the {} it covers",
            crate::units::count(self.archives, "archive", "archives")
        );
        let unmatched = &self.filter.select.globs()[at];
        Err(unmatched_select(
            dataset,
            (self.key, self.filter),
            &inside,
            unmatched,
            (archive, &members),
        ))
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

/// the refusal of `unmatched`, a select pattern of `table`, a key and its table, that matches no
/// file `inside` says where, showing a file of `example`, an archive and what it holds, or the
/// pattern that would match there once the archive's top folder leads it.
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

/// the last part of an archive's path, its file name.
fn archive_name(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, name)| name)
}

/// a dataset exclude pattern that matches files inside an archive. a dataset's own patterns
/// choose among the files its source lists, so it leaves those files where they are, though a
/// person who wrote it most likely meant to leave them out.
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

/// each exclude pattern of a dataset that matches a planned file inside an archive by the
/// file's path below the dataset's folder, the path a person sees, and matched no file the
/// dataset's source listed, as `unlisted` says. a pattern that left out a listed file did its
/// job, as `train.csv` beside `train.csv.zip` does, so a dataset `unlisted` lacks has none.
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

/// where an archive's files land inside its dataset's folder, in the order of `members`. a
/// dataset that is one archive gets the archive's files straight in its folder, without the one
/// top folder they may all share, as nix's fetchzip does. an archive beside other files unpacks
/// where it sits: a lone top folder or file comes out as it is, as `unzip` would make it, and
/// anything else goes into a folder named after the archive.
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
        join(beside, archive::stem(name))
    };
    place_under(&inside, members, |path| path)
}

fn place_under(folder: &str, members: &[Member], rest: impl Fn(&str) -> &str) -> Vec<String> {
    members
        .iter()
        .map(|member| join(folder, rest(&member.path)))
        .collect()
}

fn join(folder: &str, path: &str) -> String {
    if folder.is_empty() {
        path.to_owned()
    } else {
        format!("{folder}/{path}")
    }
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

/// a warning and its help for each dataset whose `path` ends in the name of a file placed
/// straight inside it, as `path = "train.csv"` puts train.csv at data/train.csv/train.csv: a
/// person who wrote that most likely meant the file's own place, since `path` names a folder.
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

/// the paths below the folder of `locked`, a dataset data.toml lists, that windows or a fat disk
/// cannot store, as [`name::unportable`] finds them, sorted: its own files and the files its
/// archives unpack to, as `members` records them and its members tables keep them. this machine
/// may store them fine, so they are only told.
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
/// path as a case-insensitive disk sees it, or a file where a folder must be. datasets may share
/// folders, so targets of two datasets clash only where their files do.
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

/// how to resolve a clash between `first` and `second`, two of `targets`: give one of two
/// datasets a folder of its own, or within one dataset keep an archive involved packed, or leave
/// a file out.
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

/// how to resolve a clash in one dataset between `member`, a file inside one of its archives, and
/// `listed`, a file its source lists beside that archive: leave out the listed one, or the member
/// while its archive keeps another file, or keep the archives packed.
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
mod tests {
    use super::*;
    use crate::digest;
    use crate::project::lock::{Locked, LockedFile};

    fn manifest(text: &str) -> Manifest {
        Manifest::parse(text).unwrap()
    }

    fn lock(datasets: &[(&str, &[&str])]) -> Lock {
        Lock {
            datasets: datasets
                .iter()
                .map(|(name, paths)| Locked {
                    name: (*name).to_owned(),
                    reference: "https://example.org/x".to_owned(),
                    spec: String::new(),
                    extract: true,
                    resolved: String::new(),
                    title: None,
                    license: None,
                    doi: None,
                    access: crate::source::Access::Unknown,
                    timestamp: String::new(),
                    files: paths
                        .iter()
                        .map(|path| LockedFile {
                            path: (*path).to_owned(),
                            size: 1,
                            sha256: digest::sha256(path.as_bytes()),
                            md5: None,
                            url: String::new(),
                            unpacks_to: None,
                        })
                        .collect(),
                })
                .collect(),
            ..Lock::default()
        }
    }

    const TWO: &str = "[datasets.a]\nref = \"x\"\n[datasets.b]\nref = \"y\"\npath = \"elsewhere/b\"\ncopy = true\n";
    const ONE: &str = "[datasets.a]\nref = \"x\"\n";

    /// the paths a lock places when no archive was unpacked yet.
    fn targets<'a>(manifest: &Manifest, lock: &'a Lock) -> Result<Vec<Target<'a>>, Error> {
        plan(manifest, lock, &|_| None).map(|plan| plan.targets)
    }

    fn member(path: &str) -> Member {
        Member {
            path: path.to_owned(),
            size: 1,
            sha256: digest::sha256(path.as_bytes()),
        }
    }

    /// where dataset `a` places its files when `archive` unpacks to `members`, beside `others`.
    fn unpacked(archive: &str, members: &[&str], others: &[&str]) -> Vec<String> {
        let mut locked = lock(&[("a", others)]);
        let mut file = locked.datasets[0]
            .files
            .first()
            .cloned()
            .unwrap_or(LockedFile {
                path: String::new(),
                size: 1,
                sha256: digest::sha256(b"archive"),
                md5: None,
                url: String::new(),
                unpacks_to: None,
            });
        file.path = archive.to_owned();
        file.sha256 = digest::sha256(archive.as_bytes());
        file.unpacks_to = Some(digest::sha256(b"tree"));
        locked.datasets[0].files.push(file);
        let members: Vec<Member> = members.iter().map(|path| member(path)).collect();
        let planned = plan(&manifest(ONE), &locked, &|unpacking| {
            (unpacking.file.path == archive).then(|| members.clone())
        })
        .unwrap();
        let mut paths: Vec<String> = planned
            .targets
            .into_iter()
            .filter(|target| target.file().is_some_and(|file| file.path == archive))
            .map(|target| target.path)
            .collect();
        paths.sort();
        paths
    }

    /// data.toml `text` with `datasets` pinned as it asks, each archive among them one that
    /// unpacks.
    fn pinned(text: &str, datasets: &[(&str, &[&str])]) -> (Manifest, Lock) {
        let manifest = manifest(text);
        let mut locked = lock(datasets);
        for dataset in &mut locked.datasets {
            if let Some(source) = manifest.datasets[&dataset.name].source() {
                dataset.spec = crate::project::lock::spec(source);
            }
            for file in &mut dataset.files {
                if archive::by_name(&file.path) == Some(archive::Packed::Archive) {
                    file.unpacks_to = Some(digest::sha256(file.path.as_bytes()));
                }
            }
        }
        (manifest, locked)
    }

    /// where `lock` places its files when each archive unpacks to the members `archives` names
    /// for its path, in the order planned.
    fn placed_from(
        manifest: &Manifest,
        lock: &Lock,
        archives: &[(&str, &[&str])],
    ) -> Result<Vec<String>, Error> {
        let members = |unpacking: &Archive<'_>| {
            archives
                .iter()
                .find(|(archive, _)| *archive == unpacking.file.path)
                .map(|(_, paths)| paths.iter().map(|path| member(path)).collect())
        };
        Ok(plan(manifest, lock, &members)?
            .targets
            .into_iter()
            .map(|target| target.path)
            .collect())
    }

    /// a split archive lands as the archive whole would, by its name before the counter, while
    /// its parts land nowhere; one that stays packed lands as its parts.
    #[test]
    fn a_split_archive_lands_as_the_whole_would() {
        let parts: &[&str] = &["scans.zip.001", "scans.zip.002"];
        let members: &[&str] = &["top/a.csv", "top/b.csv", "c.csv"];
        let with_readme = [&["README.md"][..], parts].concat();
        let (manifest, mut beside) = pinned(ONE, &[("a", &with_readme)]);
        beside.datasets[0].files[1].unpacks_to = Some(digest::sha256(b"tree"));
        assert_eq!(
            placed_from(&manifest, &beside, &[("scans.zip.001", members)]).unwrap(),
            [
                "data/a/README.md",
                "data/a/scans/top/a.csv",
                "data/a/scans/top/b.csv",
                "data/a/scans/c.csv"
            ]
        );
        let unknown = plan(&manifest, &beside, &|_| None).unwrap();
        assert_eq!(unknown.targets.len(), 1, "README.md alone");
        assert_eq!(
            unknown.unknown,
            [("a", &beside.datasets[0].files[1])],
            "never unpacked here, by its first part"
        );
        let (manifest, mut alone) = pinned(ONE, &[("a", parts)]);
        alone.datasets[0].files[0].unpacks_to = Some(digest::sha256(b"tree"));
        assert_eq!(
            placed_from(&manifest, &alone, &[("scans.zip.001", &members[..2])]).unwrap(),
            ["data/a/a.csv", "data/a/b.csv"],
            "alone, without the folder its files share"
        );
        let (manifest, packed) = pinned(ONE, &[("a", parts)]);
        assert_eq!(
            placed_from(&manifest, &packed, &[]).unwrap(),
            ["data/a/scans.zip.001", "data/a/scans.zip.002"]
        );
    }

    /// a filter decides which members are placed, never where: a kept file lands where it would
    /// without the filter, though the files left show no folder they all share.
    #[test]
    fn member_filters_leave_other_members_where_they_were() {
        let members: &[&str] = &["top/a.csv", "top/b.csv", "other.txt"];
        let (plain, locked) = pinned(ONE, &[("a", &["x.zip"])]);
        let everything = placed_from(&plain, &locked, &[("x.zip", members)]).unwrap();
        assert_eq!(
            everything,
            ["data/a/top/a.csv", "data/a/top/b.csv", "data/a/other.txt"]
        );
        let (filtered, locked) = pinned(
            &format!("{ONE}[datasets.a.members.\"x.zip\"]\nexclude = \"other.txt\"\n"),
            &[("a", &["x.zip"])],
        );
        assert_eq!(
            placed_from(&filtered, &locked, &[("x.zip", members)]).unwrap(),
            ["data/a/top/a.csv", "data/a/top/b.csv"]
        );
        let (selected, locked) = pinned(
            &format!(
                "{ONE}[datasets.a.members.\"x.zip\"]\nselect = \"top/*.csv\"\nexclude = \"top/a.csv\"\n"
            ),
            &[("a", &["x.zip"])],
        );
        assert_eq!(
            placed_from(&selected, &locked, &[("x.zip", members)]).unwrap(),
            ["data/a/top/b.csv"]
        );
    }

    /// a name windows or a fat disk cannot store is found among a dataset's own files and the
    /// files its archives unpack to, as they land below its folder, but not in a member its
    /// table leaves out.
    #[test]
    fn names_windows_cannot_store_are_found_in_files_and_archives() {
        let (manifest, locked) = pinned(
            &format!("{ONE}[datasets.a.members.\"x.zip\"]\nexclude = \"top/left*\"\n"),
            &[("a", &["ok.csv", "what?.csv", "x.zip"])],
        );
        let inside: &[&str] = &[
            "top/con.txt",
            "top/dot.",
            "top/fine.txt",
            "top/left|out.txt",
        ];
        let members = |unpacking: &Archive<'_>| {
            (unpacking.file.path == "x.zip")
                .then(|| inside.iter().map(|path| member(path)).collect())
        };
        assert_eq!(
            unportable(&manifest, &locked.datasets[0], &members),
            ["top/con.txt", "top/dot.", "what?.csv"],
            "a lone top folder comes out as it is"
        );
        let (manifest, locked) = pinned(ONE, &[("a", &["ok.csv", "con-data.txt"])]);
        assert_eq!(
            unportable(&manifest, &locked.datasets[0], &|_| None),
            Vec::<String>::new()
        );
    }

    /// two archives of one dataset may hold the same path; a table names one archive.
    #[test]
    fn a_member_exclude_reaches_only_its_own_archive() {
        let both: &[&str] = &["images/x.png", "labels.csv"];
        let (manifest, locked) = pinned(
            &format!("{ONE}[datasets.a.members.\"train.zip\"]\nexclude = [\"images/x.png\"]\n"),
            &[("a", &["test.zip", "train.zip"])],
        );
        assert_eq!(
            placed_from(
                &manifest,
                &locked,
                &[("train.zip", both), ("test.zip", both)]
            )
            .unwrap(),
            [
                "data/a/test/images/x.png",
                "data/a/test/labels.csv",
                "data/a/train/labels.csv"
            ]
        );
    }

    /// once an upgrade renames the archive, a table under its old name must follow it rather
    /// than let the files it left out return. a pin data.toml has moved past is planned as it
    /// stands until sync pins again.
    #[test]
    fn a_members_table_for_an_archive_the_lock_does_not_pin_is_refused() {
        let text = format!("{ONE}\n[datasets.a.members.\"pets.zip\"]\nexclude = \"x\"\n");
        let (manifest, locked) = pinned(&text, &[("a", &["README", "pets-v2.zip"])]);
        let error = placed_from(&manifest, &locked, &[("pets-v2.zip", &["x", "y"])]).unwrap_err();
        assert_eq!(error.kind, Kind::ProjectInvalid);
        assert_eq!(
            error.message,
            "members.\"pets.zip\" in datasets.a names no archive data.lock pins for it"
        );
        assert_eq!(
            error.details,
            ["data.toml:4   [datasets.a.members.\"pets.zip\"]"]
        );
        assert_eq!(
            error.help.as_deref(),
            Some("name an archive by its path in data.lock, such as \"pets-v2.zip\"")
        );
        let (manifest, mut stale) = pinned(&text, &[("a", &["README", "pets-v2.zip"])]);
        stale.datasets[0].spec = "sha256:older".to_owned();
        assert_eq!(
            placed_from(&manifest, &stale, &[("pets-v2.zip", &["x", "y"])]).unwrap(),
            ["data/a/README", "data/a/pets-v2/x", "data/a/pets-v2/y"]
        );
        let glob = format!("{ONE}\n[datasets.a.members.\"pets-v1*.zip\"]\nexclude = \"x\"\n");
        let (manifest, locked) = pinned(&glob, &[("a", &["README", "pets-v2.zip"])]);
        let error = placed_from(&manifest, &locked, &[("pets-v2.zip", &["x", "y"])]).unwrap_err();
        assert_eq!(
            error.message,
            "members.\"pets-v1*.zip\" in datasets.a names no archive data.lock pins for it",
            "a glob that matches no archive is refused as such a name is"
        );
    }

    /// three webdataset shards beside a readme, and the files of one sample each holds.
    const SHARDS: [&str; 4] = [
        "README",
        "shard-000000.tar",
        "shard-000001.tar",
        "shard-000002.tar",
    ];
    const SAMPLE: [&str; 3] = ["s.json", "s.txt", "s.wav"];

    /// each shard of [`SHARDS`] unpacking to [`SAMPLE`], and the archives of `more` to theirs.
    fn shards<'a>(more: &[(&'a str, &'a [&'a str])]) -> Vec<(&'a str, &'a [&'a str])> {
        SHARDS[1..]
            .iter()
            .map(|shard| (*shard, &SAMPLE[..]))
            .chain(more.iter().copied())
            .collect()
    }

    /// a table under a glob key chooses the files of each archive whose path in the dataset it
    /// matches, as a table under that path would, and an archive's own table wins over it. `*`
    /// stays inside one folder, so a shard in a folder of the dataset is not matched.
    #[test]
    fn a_glob_key_chooses_the_files_of_every_archive_it_matches() {
        let paths = [&SHARDS[..], &["train/shard-000003.tar"]].concat();
        let (manifest, locked) = pinned(
            &format!(
                "{ONE}[datasets.a.members.\"shard-*.tar\"]\nselect = \"*.wav\"\n[datasets.a.members.\"shard-000001.tar\"]\nselect = \"*.txt\"\n"
            ),
            &[("a", &paths)],
        );
        assert_eq!(
            placed_from(
                &manifest,
                &locked,
                &shards(&[("train/shard-000003.tar", &SAMPLE[..])])
            )
            .unwrap(),
            [
                "data/a/README",
                "data/a/shard-000000/s.wav",
                "data/a/shard-000001/s.txt",
                "data/a/shard-000002/s.wav",
                "data/a/train/shard-000003/s.json",
                "data/a/train/shard-000003/s.txt",
                "data/a/train/shard-000003/s.wav",
            ]
        );
    }

    /// two glob keys matching one archive leave no one table for it, so planning stops, naming
    /// both and the archive, unless the archive has a table of its own, which wins over both.
    /// gc and status, which cannot stop, take no table for it and keep every file.
    #[test]
    fn two_glob_keys_matching_one_archive_are_refused() {
        let text = format!(
            "{ONE}[datasets.a.members.\"shard-*.tar\"]\nselect = \"*.wav\"\n[datasets.a.members.\"*-000001.tar\"]\nselect = \"*.txt\"\n"
        );
        let (manifest, locked) = pinned(&text, &[("a", &SHARDS)]);
        let error = placed_from(&manifest, &locked, &shards(&[])).unwrap_err();
        assert_eq!(error.kind, Kind::ProjectInvalid);
        assert_eq!(
            error.message,
            "members.\"shard-*.tar\" and members.\"*-000001.tar\" in datasets.a both match shard-000001.tar"
        );
        assert_eq!(
            error.details,
            [
                "data.toml:3   [datasets.a.members.\"shard-*.tar\"]",
                "data.toml:5   [datasets.a.members.\"*-000001.tar\"]"
            ]
        );
        assert_eq!(
            error.help.as_deref(),
            Some(
                "change one key so only one matches it, or give it a table of its own, as [datasets.a.members.\"shard-000001.tar\"], which wins over both"
            )
        );
        let members = &manifest.datasets["a"].source().unwrap().members;
        assert!(members.get("shard-000001.tar").is_none());
        let (manifest, locked) = pinned(
            &format!("{text}[datasets.a.members.\"shard-000001.tar\"]\nexclude = \"s.json\"\n"),
            &[("a", &SHARDS)],
        );
        assert_eq!(
            placed_from(&manifest, &locked, &shards(&[])).unwrap(),
            [
                "data/a/README",
                "data/a/shard-000000/s.wav",
                "data/a/shard-000001/s.txt",
                "data/a/shard-000001/s.wav",
                "data/a/shard-000002/s.wav",
            ]
        );
    }

    /// three shards of one dataset that differ: only the second holds a `.flac`, and the third
    /// has no `.txt`.
    fn uneven() -> Vec<(&'static str, &'static [&'static str])> {
        vec![
            ("shard-000000.tar", &["s.json", "s.txt", "s.wav"]),
            ("shard-000001.tar", &["s.flac", "s.json", "s.txt", "s.wav"]),
            ("shard-000002.tar", &["s.json", "s.wav"]),
        ]
    }

    /// a glob key's select patterns each need a file in some archive the key covers, not in
    /// every one: shards that differ place what each holds, and one left no file places none.
    #[test]
    fn a_glob_tables_select_places_each_kind_where_it_is() {
        let (manifest, locked) = pinned(
            &format!(
                "{ONE}[datasets.a.members.\"shard-*.tar\"]\nselect = [\"*.flac\", \"*.txt\"]\n"
            ),
            &[("a", &SHARDS)],
        );
        assert_eq!(
            placed_from(&manifest, &locked, &uneven()).unwrap(),
            [
                "data/a/README",
                "data/a/shard-000000/s.txt",
                "data/a/shard-000001/s.flac",
                "data/a/shard-000001/s.txt",
            ]
        );
    }

    /// a glob key's select pattern that matches a file in no archive the key covers, as a typo,
    /// is refused naming the key and the pattern, once every archive it covers is known here.
    #[test]
    fn a_glob_tables_select_that_matches_in_no_archive_is_refused() {
        let (manifest, locked) = pinned(
            &format!(
                "{ONE}[datasets.a.members.\"shard-*.tar\"]\nselect = [\"*.flca\", \"*.txt\"]\n"
            ),
            &[("a", &SHARDS)],
        );
        let error = placed_from(&manifest, &locked, &uneven()).unwrap_err();
        assert_eq!(error.kind, Kind::ProjectInvalid);
        assert_eq!(
            error.message,
            "the select pattern \"*.flca\" in members.\"shard-*.tar\" of datasets.a matches no file inside the 3 archives it covers"
        );
        assert_eq!(
            error.details,
            ["data.toml:4   select = [\"*.flca\", \"*.txt\"]"]
        );
        assert_eq!(
            error.help.as_deref(),
            Some("paths inside shard-000000.tar look like \"s.json\"")
        );
        let unknown: Vec<_> = uneven()
            .into_iter()
            .filter(|(archive, _)| *archive != "shard-000001.tar")
            .collect();
        assert!(
            placed_from(&manifest, &locked, &unknown).is_ok(),
            "a shard not unpacked here yet may hold it"
        );
    }

    #[test]
    fn an_unmatched_member_select_suggests_the_top_folder() {
        let members: &[&str] = &["pets-v1/images/cat.png", "pets-v1/labels.csv"];
        for (select, help) in [
            (
                "images/cat.png",
                "try \"pets-v1/images/cat.png\": paths inside an archive start with its top folder",
            ),
            (
                "pictures/*.png",
                "paths inside pets.zip look like \"pets-v1/images/cat.png\"",
            ),
        ] {
            let (manifest, locked) = pinned(
                &format!("{ONE}[datasets.a.members.\"pets.zip\"]\nselect = [\"{select}\"]\n"),
                &[("a", &["pets.zip"])],
            );
            let error = placed_from(&manifest, &locked, &[("pets.zip", members)]).unwrap_err();
            assert_eq!(error.kind, Kind::ProjectInvalid);
            assert_eq!(
                error.message,
                format!(
                    "the select pattern \"{select}\" in members.\"pets.zip\" of datasets.a matches no file inside pets.zip"
                )
            );
            assert_eq!(
                error.details,
                [format!("data.toml:4   select = [\"{select}\"]")]
            );
            assert_eq!(error.help.as_deref(), Some(help));
        }
    }

    #[test]
    fn member_filters_that_leave_an_archive_no_file_are_refused() {
        let (manifest, locked) = pinned(
            &format!("{ONE}[datasets.a.members.\"pets.zip\"]\nexclude = [\"**\"]\n"),
            &[("a", &["README", "pets.zip"])],
        );
        let error = placed_from(&manifest, &locked, &[("pets.zip", &["a/x", "a/y"])]).unwrap_err();
        assert_eq!(error.kind, Kind::ProjectInvalid);
        assert_eq!(
            error.message,
            "select and exclude in members.\"pets.zip\" of datasets.a leave no file of pets.zip"
        );
        assert_eq!(error.details, ["data.toml:4   exclude = [\"**\"]"]);
        assert_eq!(
            error.help.as_deref(),
            Some(
                "loosen the patterns, or exclude \"pets.zip\" in datasets.a to leave the whole archive out"
            )
        );
    }

    /// a dataset exclude chooses among the files its source lists, so one written against a
    /// file inside an archive leaves it where it is; it is found by the path the file lands at
    /// below the dataset's folder, which is what a person writes. one that left out a file the
    /// listing named, as `images/x.png` here, did its job.
    #[test]
    fn an_exclude_naming_files_inside_an_archive_is_found() {
        let (manifest, locked) = pinned(
            &format!("{ONE}exclude = [\"images/x.png\", \"labels.txt\", \"images/*.jpg\"]\n"),
            &[("a", &["pets.zip"])],
        );
        let members: &[&str] = &[
            "pets-v1/images/x.png",
            "pets-v1/images/y.jpg",
            "pets-v1/images/z.jpg",
            "pets-v1/labels.csv",
        ];
        let paths = |archive: &str| -> Option<Vec<Member>> {
            (archive == "pets.zip").then(|| members.iter().map(|path| member(path)).collect())
        };
        let planned = plan(&manifest, &locked, &|archive| paths(&archive.file.path)).unwrap();
        let missed = vec!["labels.txt".to_owned(), "images/*.jpg".to_owned()];
        let unlisted = Unlisted::from([("a".to_owned(), missed)]);
        assert_eq!(
            unreached(&manifest, &planned.targets, &unlisted),
            [Unreached {
                dataset: "a".to_owned(),
                pattern: "images/*.jpg".to_owned(),
                archives: vec!["pets.zip".to_owned()],
                paths: vec![
                    "data/a/images/y.jpg".to_owned(),
                    "data/a/images/z.jpg".to_owned()
                ],
            },]
        );
        let (plain, locked) = pinned(ONE, &[("a", &["pets.zip"])]);
        let planned = plan(&plain, &locked, &|archive| paths(&archive.file.path)).unwrap();
        assert_eq!(unreached(&plain, &planned.targets, &unlisted), []);
    }

    /// data.lock keeps no file an exclude left out, so a plan of it alone cannot tell an exclude
    /// that left out a listed `train.csv` beside `train.csv.zip` from one that missed.
    #[test]
    fn a_lock_alone_never_calls_an_exclude_unreached() {
        let (manifest, locked) = pinned(
            &format!("{ONE}exclude = [\"train.csv\"]\n"),
            &[("a", &["test.csv", "train.csv.zip"])],
        );
        let planned = plan(&manifest, &locked, &|archive| {
            (archive.file.path == "train.csv.zip").then(|| vec![member("train.csv")])
        })
        .unwrap();
        assert_eq!(unreached(&manifest, &planned.targets, &Unlisted::new()), []);
    }

    #[test]
    fn files_land_in_their_dataset_folder() {
        let locked = lock(&[("a", &["x.csv", "sub/y.csv"]), ("b", &["z.csv"])]);
        let targets = targets(&manifest(TWO), &locked).unwrap();
        let paths: Vec<&str> = targets.iter().map(|target| target.path.as_str()).collect();
        assert_eq!(
            paths,
            ["data/a/x.csv", "data/a/sub/y.csv", "data/elsewhere/b/z.csv"]
        );
        assert!(targets[2].copy && !targets[0].copy);
    }

    #[test]
    fn a_dataset_at_the_root_lands_in_the_data_folder() {
        let root = manifest("[datasets.a]\nref = \"x\"\npath = \".\"\n");
        assert_eq!(folder(&root, "a"), "data");
        let locked = lock(&[("a", &["train.csv", "raw/x.csv"])]);
        let paths: Vec<String> = targets(&root, &locked)
            .unwrap()
            .into_iter()
            .map(|target| target.path)
            .collect();
        assert_eq!(paths, ["data/train.csv", "data/raw/x.csv"]);
    }

    #[test]
    fn datasets_share_a_folder_when_no_file_clashes() {
        let shared = manifest(
            "[datasets.a]\nref = \"w\"\npath = \"splits\"\n[datasets.b]\nref = \"x\"\npath = \"splits\"\n[datasets.c]\nref = \"y\"\npath = \"splits/raw\"\n[datasets.d]\nref = \"z\"\npath = \".\"\n",
        );
        let locked = lock(&[
            ("a", &["train.csv"]),
            ("b", &["test.csv", "raw/labels.csv"]),
            ("c", &["one.png"]),
            ("d", &["README"]),
        ]);
        let paths: Vec<String> = targets(&shared, &locked)
            .unwrap()
            .into_iter()
            .map(|target| target.path)
            .collect();
        assert_eq!(
            paths,
            [
                "data/splits/train.csv",
                "data/splits/test.csv",
                "data/splits/raw/labels.csv",
                "data/splits/raw/one.png",
                "data/README"
            ]
        );
    }

    /// two datasets' files on one path clash even with the same bytes, since each placed file
    /// has one dataset that owns it.
    #[test]
    fn a_clash_between_datasets_names_both_and_the_path() {
        let flat = manifest(
            "[datasets.a]\nref = \"x\"\npath = \".\"\n[datasets.b]\nref = \"y\"\npath = \".\"\n",
        );
        for (a, b, said) in [
            (
                "README",
                "README",
                "datasets a and b would both place data/README",
            ),
            (
                "README",
                "readme",
                "datasets a and b would place data/README and data/readme, the same file on a disk that ignores case",
            ),
            (
                "raw",
                "raw/x.csv",
                "dataset a places a file at data/raw, but dataset b needs a folder there for data/raw/x.csv",
            ),
        ] {
            let error = targets(&flat, &lock(&[("a", &[a]), ("b", &[b])])).unwrap_err();
            assert_eq!(error.kind, Kind::ProjectInvalid);
            assert_eq!(error.message, said);
            assert_eq!(
                error.help.as_deref(),
                Some("give datasets.a or datasets.b another path in data.toml")
            );
        }
    }

    #[test]
    fn a_dataset_that_is_one_archive_gets_its_files_without_their_shared_top_folder() {
        assert_eq!(
            unpacked("MNIST.zip", &["mnist/train.csv", "mnist/test/a.csv"], &[]),
            ["data/a/test/a.csv", "data/a/train.csv"]
        );
        assert_eq!(
            unpacked("v2/data.tar.gz", &["a.csv", "b/c.csv"], &[]),
            ["data/a/v2/a.csv", "data/a/v2/b/c.csv"]
        );
        assert_eq!(
            unpacked("big.csv.zip", &["big.csv"], &[]),
            ["data/a/big.csv"]
        );
    }

    #[test]
    fn an_archive_beside_other_files_unpacks_where_it_sits() {
        assert_eq!(
            unpacked("images.zip", &["imgs/1.png", "imgs/2.png"], &["labels.csv"]),
            ["data/a/imgs/1.png", "data/a/imgs/2.png"],
            "a lone top folder comes out as it is"
        );
        assert_eq!(
            unpacked("big.csv.zip", &["big.csv"], &["README.md"]),
            ["data/a/big.csv"],
            "so does a lone file"
        );
        assert_eq!(
            unpacked("raw/train.tar.zst", &["1.png", "2.png"], &["README.md"]),
            ["data/a/raw/train/1.png", "data/a/raw/train/2.png"],
            "loose files get a folder named after the archive"
        );
    }

    #[test]
    fn an_archive_not_unpacked_here_is_reported_not_planned() {
        let mut locked = lock(&[("a", &["x.zip", "y.csv"])]);
        locked.datasets[0].files[0].unpacks_to = Some(digest::sha256(b"tree"));
        let planned = plan(&manifest(ONE), &locked, &|_| None).unwrap();
        assert_eq!(planned.targets.len(), 1);
        assert_eq!(planned.unknown.len(), 1);
        assert_eq!(planned.unknown[0].1.path, "x.zip");
    }

    /// a file beside an archive that holds one of the same name: either can be left out, the
    /// archive's only when the members table leaves it another file, or the archive kept packed.
    /// two archives of one dataset that unpack a file to one path are both named with that path,
    /// and the remedies keep leaving one out and keeping them packed, beside a dataset for each.
    #[test]
    fn a_clash_between_two_archives_names_both() {
        let (manifest, locked) = pinned(ONE, &[("a", &["fold-000000.tar", "fold-000001.tar"])]);
        let same: &[&str] = &["train/0000.txt"];
        let shards = [("fold-000000.tar", same), ("fold-000001.tar", same)];
        let error = placed_from(&manifest, &locked, &shards).unwrap_err();
        assert_eq!(
            error.message,
            "two files would land on data/a/train/0000.txt: one from fold-000000.tar and one from fold-000001.tar"
        );
        assert_eq!(
            error.help.as_deref(),
            Some(
                "leave one of them out with exclude in [datasets.a.members.\"fold-000000.tar\"] in data.toml, or set extract = false on datasets.a to keep its archives packed, or give each archive a dataset of its own, whose folder keeps its files apart"
            )
        );
    }

    #[test]
    fn a_clash_with_an_archives_file_suggests_leaving_either_out_or_keeping_it_packed() {
        for (listed, members, help) in [
            (
                "notes.txt",
                vec![member("notes.txt")],
                "leave out the notes.txt beside x.zip with exclude = [\"notes.txt\"] in datasets.a, or set extract = false on datasets.a to keep its archives packed",
            ),
            (
                "t/notes.txt",
                vec![member("t/notes.txt"), member("t/more.txt")],
                "leave out the t/notes.txt beside x.zip with exclude = [\"t/notes.txt\"] in datasets.a, or the one inside it with exclude in [datasets.a.members.\"x.zip\"], or set extract = false on datasets.a to keep its archives packed",
            ),
        ] {
            let mut locked = lock(&[("a", &[listed, "x.zip"])]);
            locked.datasets[0].files[1].unpacks_to = Some(digest::sha256(b"tree"));
            let err = plan(&manifest(ONE), &locked, &|archive| {
                (archive.file.path == "x.zip").then(|| members.clone())
            })
            .unwrap_err();
            let clash = format!("two files would land on data/a/{listed}");
            assert!(err.message.contains(&clash), "{}", err.message);
            assert_eq!(err.help.as_deref(), Some(help));
        }
    }

    /// a dataset data.toml makes with a recipe places nothing from a source pin data.lock still
    /// has under its name, since only a build makes its files.
    #[test]
    fn a_recipe_places_nothing_from_an_old_source_pin() {
        let made = manifest(
            "[datasets.a]\nref = \"x\"\n\n[datasets.b]\nfrom = [\"a\"]\nrun = [\"t\", \"{a}\", \"{out}\"]\n",
        );
        let locked = lock(&[("a", &["x.csv"]), ("b", &["y.csv"])]);
        let paths: Vec<String> = targets(&made, &locked)
            .unwrap()
            .into_iter()
            .map(|target| target.path)
            .collect();
        assert_eq!(paths, ["data/a/x.csv"]);
    }

    #[test]
    fn a_locked_dataset_missing_from_data_toml_places_nothing() {
        let only_a = manifest(ONE);
        assert_eq!(
            targets(&only_a, &lock(&[("a", &["x"]), ("gone", &["y"])]))
                .unwrap()
                .len(),
            1
        );
    }

    /// a dataset with `place = false`, fetched or made, has no target: nothing of it is
    /// downloaded, unpacked or placed, and its files clash with no other dataset's.
    #[test]
    fn an_unplaced_dataset_has_no_targets() {
        let (unplaced, mut locked) = pinned(
            "[datasets.raw]\nref = \"x\"\npath = \".\"\nplace = false\n\n[datasets.other]\nref = \"y\"\npath = \".\"\n\n[datasets.clean]\nfrom = [\"raw\"]\nrun = [\"t\", \"{raw}\", \"{out}\"]\nplace = false\n",
            &[("raw", &["a.csv", "x.zip"]), ("other", &["a.csv"])],
        );
        let files = vec![member("c.csv")];
        locked.derived.push(Derived {
            name: "clean".to_owned(),
            from: Vec::new(),
            run: Vec::new(),
            code: Vec::new(),
            env: std::collections::BTreeMap::new(),
            key: digest::sha256(b"key"),
            timestamp: String::new(),
            tree: digest::sha256(b"tree"),
            files,
        });
        let planned = plan(&unplaced, &locked, &|_| None).unwrap();
        let paths: Vec<&str> = planned
            .targets
            .iter()
            .map(|target| target.path.as_str())
            .collect();
        assert_eq!(paths, ["data/a.csv"]);
        assert_eq!(planned.targets[0].dataset, "other");
        assert!(planned.unknown.is_empty(), "{:?}", planned.unknown);
    }

    #[test]
    fn a_refused_archive_member_suggests_keeping_the_archive_packed() {
        let mut locked = lock(&[("a", &["data.zip"])]);
        locked.datasets[0].files[0].unpacks_to = Some(digest::sha256(b"tree"));
        let members = vec![member("safe.csv"), member(".git/config")];
        let mut planned = Plan::default();
        expand(
            &mut planned,
            "a",
            "",
            false,
            &locked.datasets[0].files,
            &Members::new(),
            &|_| Some(members.clone()),
        )
        .unwrap();
        for (patterns, expected) in [
            (Patterns::InDataToml, "extract = false"),
            (Patterns::OnCommandLine, "--no-extract"),
        ] {
            let error = check(&planned.targets, patterns).unwrap_err();
            assert!(error.help.unwrap().contains(expected));
        }
    }

    #[test]
    fn metadata_targets_are_refused() {
        for path in [
            ".git/hooks/x",
            ".hg/store/x",
            ".svn/entries",
            ".fetchloom/state",
            "data/a/.GIT/hooks/x",
            "nested/.git. /hooks/x",
            "GIT~1/hooks/x",
            "nested/.git:stream/hooks/x",
        ] {
            let locked = lock(&[("a", &[path])]);
            let mut planned = Plan::default();
            expand(
                &mut planned,
                "a",
                "",
                false,
                &locked.datasets[0].files,
                &Members::new(),
                &|_| None,
            )
            .unwrap();
            for patterns in [Patterns::InDataToml, Patterns::OnCommandLine] {
                let result = check(&planned.targets, patterns);
                assert!(result.is_err(), "accepted metadata target: {path}");
                let error = result.unwrap_err();
                assert_eq!(error.kind, Kind::ProjectInvalid);
                assert!(error.message.contains(path), "{}", error.message);
                assert!(error.message.contains("metadata"), "{}", error.message);
                assert!(error.help.is_some());
            }
        }
    }

    #[test]
    fn metadata_archive_targets_are_refused() {
        let manifest = manifest(ONE);
        let mut locked = lock(&[("a", &["x.zip"])]);
        locked.datasets[0].files[0].unpacks_to = Some(digest::sha256(b"tree"));
        let members = vec![member(".git/hooks/x"), member("readme.txt")];
        let result = plan(&manifest, &locked, &|_| Some(members.clone()));
        assert!(result.is_err(), "accepted metadata from archive members");
        let error = result.unwrap_err();
        assert!(error.message.contains(".git/hooks/x"), "{}", error.message);
        assert!(error.message.contains("metadata"), "{}", error.message);
    }

    #[test]
    fn clashes_are_refused_before_anything_is_written() {
        let one = manifest(ONE);
        assert!(
            targets(&one, &lock(&[("a", &["Readme.md", "README.md"])])).is_err(),
            "case-insensitive disks see one file"
        );
        assert!(
            targets(&one, &lock(&[("a", &["\u{3c2}.txt", "\u{3c3}.txt"])])).is_err(),
            "windows upcases both sigmas to one letter"
        );
        assert!(
            targets(&one, &lock(&[("a", &["caf\u{e9}.csv", "cafe\u{301}.csv"])])).is_err(),
            "two unicode spellings of one name count as one"
        );
        assert!(
            targets(&one, &lock(&[("a", &["x", "x/y"])])).is_err(),
            "a file where a folder must be"
        );
        assert_eq!(
            targets(&one, &lock(&[("a", &["2020-01-01T10:00:00.csv"])])).is_err(),
            cfg!(windows)
        );
    }

    /// `path` names a folder, so one that ends in the name of a file placed straight inside it
    /// most likely meant that file's own place.
    #[test]
    fn a_path_named_like_its_file_is_found() {
        let flagged = |path: &str, files: &[&str]| {
            let text = format!("[datasets.train]\nref = \"x\"\npath = \"{path}\"\n");
            let manifest = manifest(&text);
            let locked = lock(&[("train", files)]);
            file_shaped_paths(&manifest, &plan(&manifest, &locked, &|_| None).unwrap())
        };
        assert_eq!(
            flagged("train.csv", &["train.csv", "README"]),
            [(
                "path = \"train.csv\" makes a folder named train.csv, so the file is data/train.csv/train.csv".to_owned(),
                "path names a folder; to put the file at data/train.csv, set path = \".\" on datasets.train in data.toml".to_owned()
            )]
        );
        assert_eq!(
            flagged("raw/Train.CSV", &["train.csv"]),
            [(
                "path = \"raw/Train.CSV\" makes a folder named Train.CSV, so the file is data/raw/Train.CSV/train.csv".to_owned(),
                "path names a folder; to put the file at data/raw/train.csv, set path = \"raw\" on datasets.train in data.toml".to_owned()
            )],
            "a disk that ignores case"
        );
        for (path, files) in [
            ("train.csv", &["sub/train.csv"][..]),
            ("splits", &["train.csv"][..]),
            (".", &["train.csv"][..]),
        ] {
            assert_eq!(flagged(path, files), [], "{path} with {files:?}");
        }
        let unnamed = manifest("[datasets.iris]\nref = \"x\"\n");
        let locked = lock(&[("iris", &["iris"])]);
        let planned = plan(&unnamed, &locked, &|_| None).unwrap();
        assert_eq!(file_shaped_paths(&unnamed, &planned), []);
    }

    #[test]
    fn a_top_folder_is_shared_only_by_every_member() {
        let members = |paths: &[&str]| paths.iter().map(|path| member(path)).collect::<Vec<_>>();
        assert_eq!(
            common_top(&members(&["data/a.csv", "data/sub/b.csv"])),
            Some("data")
        );
        assert_eq!(common_top(&members(&["data/a.csv", "b.csv"])), None);
        assert_eq!(common_top(&members(&["data/a.csv", "data2/b.csv"])), None);
        assert_eq!(common_top(&members(&["a.csv"])), None);
        assert_eq!(common_top(&[]), None);
    }

    #[test]
    fn caseless_sharp_s_paths_are_refused() {
        let locked = lock(&[("a", &["ß.csv", "ẞ.csv"])]);
        assert!(targets(&manifest(ONE), &locked).is_err());
    }
}
