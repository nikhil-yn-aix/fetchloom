//! `fetchloom push`: the files data.lock pins, copied from this machine's cache into a folder
//! mirror as mirrors are read, with the lists of what their archives unpack to, so a teammate's
//! sync, a machine without a network or a reviewer can take them from there. it writes the
//! mirror, and the cache to take in a file it lacks: from the data folder when that holds the
//! pinned bytes, else downloaded as a sync downloads it, the mirrors asked first. it never
//! changes the project or builds: a recipe's result the cache lacks is named with the build
//! that makes it.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::SystemTime;

use serde::Serialize;

use super::Context;
use super::args;
use super::fetch::data_folder::DataFolder;
use super::project::{find, known, locked};
use super::sync::{Loop, Scope, place};
use crate::cache::{Cache, read_unfollowed};
use crate::digest::Sha256;
use crate::error::{Error, Kind};
use crate::mirror::folder::{self, Failed, Fault, Item, Put, Refused, State, Target, There};
use crate::mirror::{Location, OBJECTS, TREES};
use crate::project::Project;
use crate::project::lock::archives::Archive;
use crate::project::lock::{Lock, LockedFile};
use crate::project::manifest::Manifest;
use crate::source::Access;
use crate::stop::Stop;
use crate::{plan, recipe, units};

/// what a push did, or with `--dry-run` would do, as `--json` prints it.
#[derive(Serialize)]
pub(super) struct Report {
    /// the mirror's folder.
    target: PathBuf,
    /// whether the push started a mirror in that folder.
    started: bool,
    /// the datasets named, or none when the push named none.
    selected: Option<Vec<String>>,
    /// every dataset pushed.
    datasets: Vec<String>,
    /// the files those datasets need on the mirror, each once, and their bytes.
    objects: u64,
    bytes: u64,
    /// the files written, and their bytes.
    copied: u64,
    copied_bytes: u64,
    /// the files found there after one look each, and left as they were.
    present: u64,
    present_bytes: u64,
    /// the files of `copied` written in place of a damaged copy.
    replaced: u64,
    /// the lists of what archives unpack to that were written.
    trees: u64,
    /// the datasets a push asks about before it copies their files.
    asks: Vec<Ask>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    dry_run: bool,
    /// with `--dry-run`, what would be written.
    #[serde(skip_serializing_if = "Option::is_none")]
    changes: Option<Changes>,
}

/// a dataset a push asks about first, with what its source said of who may read it.
#[derive(Serialize)]
struct Ask {
    dataset: String,
    access: &'static str,
}

#[derive(Serialize)]
struct Changes {
    copy: Vec<Copied>,
    trees: Vec<Copied>,
    /// the pinned files a push would download first, since neither the cache nor the data
    /// folder holds what it needs of them.
    download: Vec<Fetch>,
    /// the archives among `download`, as `<dataset>/<path>`, which are unpacked once they are
    /// in. the files of one this machine never unpacked are not counted.
    unpack: Vec<String>,
}

/// a pinned file a push downloads first when the cache lacks what it brings: the needed file
/// itself, or the archive it is inside.
#[derive(Clone, Serialize)]
struct Fetch {
    /// `<dataset>/<path>`, as data.lock names it.
    path: String,
    size: u64,
    /// whether it is an archive, which is unpacked once it is in.
    #[serde(skip)]
    archive: bool,
}

/// a file a push would write: the first file that needs it, or the archive whose list of files
/// it is, as `<dataset>/<path>`.
#[derive(Serialize)]
struct Copied {
    path: String,
    sha256: Sha256,
    size: u64,
}

/// a file a push needs on the mirror, and where it comes from on this machine.
struct Needed {
    item: Item,
    /// `<dataset>/<path>` of the first file that needs it, or of the archive whose list it is.
    label: String,
    /// the dataset that first needs it.
    dataset: String,
    source: Source,
    /// for a file a source pins, the file whose download brings it.
    fetch: Option<Fetch>,
}

/// where the bytes of a [`Needed`] are on this machine.
#[derive(Clone, Copy)]
enum Source {
    /// a file a source pins, which a push downloads when the cache and the data folder lack it.
    Pinned,
    /// a file of a recipe's result, in the cache once `fetchloom build` made it.
    Result,
    /// the cache's record of what an archive unpacks to, by the record's key: the archive's
    /// SHA-256, or a split archive's parts key.
    List(Sha256),
}

/// a dataset a push asks about first, and why: what its source said of who may read it, or for a
/// recipe's result the strictest of what the datasets it is made from said, with their names.
struct Asking {
    dataset: String,
    access: Access,
    from: Vec<String>,
}

/// what a push found before writing anything.
struct Planned {
    target: PathBuf,
    state: State,
    selected: Option<Vec<String>>,
    datasets: Vec<String>,
    needed: Vec<Needed>,
    /// what the mirror holds at the name of each of `needed`.
    there: Vec<There>,
    asks: Vec<Asking>,
    /// where in `needed` the files are that the mirror and the cache lack.
    lacked: Vec<usize>,
    /// each archive whose files this machine does not know, with its dataset.
    unknown: Vec<(String, Fetch)>,
    /// where in `needed` the files are that the cache lacks and the data folder looks to hold,
    /// which the push takes into the cache before it writes.
    taking: Vec<usize>,
    /// the data folder `taking` comes from.
    folder: DataFolder,
}

pub(super) fn run(context: &Context, args: &args::Push) -> Result<Report, Error> {
    let target = target(context, args)?;
    let project = find(&context.output)?;
    let (_, manifest) = project.manifest()?;
    for name in &args.names {
        known(&manifest, name, "push")?;
    }
    let datasets = selected(&manifest, &args.names, args.inputs);
    if datasets.is_empty() {
        context.output.done("Ok", &everything(&datasets));
        let target = reached(target);
        return Ok(Report::empty(target, args.dry.dry_run));
    }
    let lock = project.lock()?;
    let scope = Scope::Only(datasets.iter().cloned().collect());
    locked(
        &project,
        &manifest,
        lock.as_ref(),
        "name only datasets data.lock pins",
        &scope,
    )?;
    let lock = lock.unwrap_or_default();
    let cache = if args.dry.dry_run {
        super::peek_cache()?
    } else {
        super::open_cache()?
    };
    let selected = (!args.names.is_empty()).then(|| selected(&manifest, &args.names, false));
    let mut planned = plan(
        context, &manifest, &lock, &cache, target, selected, datasets,
    )?;
    let dry_run = args.dry.dry_run;
    settle(
        context,
        &project,
        &manifest,
        &lock,
        &cache,
        &mut planned,
        dry_run,
    )?;
    if dry_run {
        let report = report(&planned, Outcome::DryRun);
        tell_dry(context, &planned, &report);
        return Ok(report);
    }
    if !planned.lacked.is_empty() || !planned.unknown.is_empty() {
        fetch(
            context,
            &project,
            &manifest,
            &lock,
            &cache,
            &fetching(&planned),
        )
        .map_err(|error| with_report(error, &report(&planned, Outcome::Stopped)))?;
        // what the archives hold is known now, so the push plans again
        planned = plan(
            context,
            &manifest,
            &lock,
            &cache,
            planned.target.clone(),
            planned.selected.clone(),
            planned.datasets.clone(),
        )?;
        settle(
            context,
            &project,
            &manifest,
            &lock,
            &cache,
            &mut planned,
            false,
        )?;
        if !planned.lacked.is_empty() || !planned.unknown.is_empty() {
            let lacked: Vec<&Needed> = planned
                .lacked
                .iter()
                .map(|at| &planned.needed[*at])
                .collect();
            let error = missing(&lacked, &planned.unknown);
            return Err(with_report(error, &report(&planned, Outcome::Stopped)));
        }
    }
    ask(context, &planned)?;
    take(context, &project, &manifest, &lock, &cache, &planned)?;
    let (started, puts) = write(context, &cache, &planned)?;
    let report = report(&planned, Outcome::Written(started, &puts));
    tell(context, &planned, &report);
    Ok(report)
}

/// the folder to push to, as `--to` names it, else the `push_to` setting.
///
/// # Errors
///
/// `usage` when neither names one, or `--to` names a web address.
fn target(context: &Context, args: &args::Push) -> Result<PathBuf, Error> {
    let Some(to) = &args.to else {
        return context.settings.push_to.value.clone().ok_or_else(|| {
            Error::new(Kind::Usage, "push needs a folder to copy the files into").help(
                "give --to <folder>, or set push_to in the settings file to a mirror's folder",
            )
        });
    };
    // a web address is text, so a name that is not is a folder
    let Some(text) = to.to_str() else {
        return Ok(to.clone());
    };
    let shown = crate::net::mask_query(text);
    match Location::parse(text, None) {
        Ok(Location::Folder(folder)) => Ok(folder),
        Ok(Location::Web(_)) => Err(Error::new(
            Kind::Usage,
            format!("--to {shown} is a web address, and push writes to folders"),
        )
        .help("give --to a folder, and copy that folder to a server or a bucket with a tool such as rclone")),
        Err(refusal) => Err(Error::new(
            Kind::Usage,
            format!("--to {shown} is no folder: {}", refusal.reason),
        )
        .help(refusal.instead)),
    }
}

/// `target` as an absolute path without `.` or `..`: the folder the system reaches through it.
/// unix keeps `..` in an absolute path, and a link before it makes that path reach another folder
/// than the same path without both, so the part up to the last `..` is resolved as the system
/// resolves it.
fn reached(target: PathBuf) -> PathBuf {
    let absolute = std::path::absolute(&target).unwrap_or(target);
    let parts: Vec<Component<'_>> = absolute.components().collect();
    let Some(last) = parts.iter().rposition(|part| *part == Component::ParentDir) else {
        return absolute;
    };
    match std::fs::canonicalize(parts[..=last].iter().collect::<PathBuf>()) {
        Ok(real) => parts[last + 1..]
            .iter()
            .fold(real, |path, part| path.join(part)),
        Err(_) => absolute,
    }
}

/// the datasets a push sends: those `names` gives, else every dataset sync places. with
/// `inputs`, also what those are made from, all the way down, and with no names what every
/// recipe is made from.
fn selected(manifest: &Manifest, names: &[String], inputs: bool) -> Vec<String> {
    let roots: Vec<&str> = if names.is_empty() {
        manifest
            .datasets
            .iter()
            .filter(|(_, entry)| entry.place || (inputs && entry.recipe().is_some()))
            .map(|(name, _)| name.as_str())
            .collect()
    } else {
        names.iter().map(String::as_str).collect()
    };
    let chosen: BTreeSet<&str> = if inputs {
        recipe::closure(&manifest.datasets, &roots)
            .into_iter()
            .collect()
    } else {
        roots.into_iter().collect()
    };
    chosen.into_iter().map(str::to_owned).collect()
}

/// what pushing `datasets` from `cache` into the folder `target` needs, what the mirror there
/// holds of it already, looked at once a file, and what neither the mirror nor the cache holds.
///
/// # Errors
///
/// a folder that cannot take a push, and a mirror that cannot be looked in.
fn plan(
    context: &Context,
    manifest: &Manifest,
    lock: &Lock,
    cache: &Cache,
    target: PathBuf,
    selected: Option<Vec<String>>,
    datasets: Vec<String>,
) -> Result<Planned, Error> {
    let target = reached(target);
    let Wanted { needed, unknown } = needed(manifest, lock, cache, &datasets)?;
    let state = folder::look(&target).map_err(|refused| refusal(&target, refused))?;
    let items: Vec<Item> = needed.iter().map(|needed| needed.item).collect();
    let there = folder::survey(&target, state, &items, context.threads).map_err(|err| {
        Error::io(
            format_args!("could not look in the mirror {}", target.display()),
            &err,
        )
    })?;
    let copying: BTreeSet<&str> = needed
        .iter()
        .zip(&there)
        .filter(|(_, there)| **there != There::Present)
        .map(|(needed, _)| needed.dataset.as_str())
        .collect();
    let asks = asking(manifest, lock, &datasets)
        .into_iter()
        .filter(|ask| copying.contains(ask.dataset.as_str()))
        .collect();
    let lacked = lacking(context, cache, &needed, &there);
    Ok(Planned {
        target,
        state,
        selected,
        datasets,
        needed,
        there,
        asks,
        lacked,
        unknown,
        taking: Vec::new(),
        folder: DataFolder::default(),
    })
}

/// moves from `planned.lacked` to `planned.taking` the files the data folder holds, which the
/// push takes into the cache once it may write. a `dry_run` counts only what the push would take:
/// a file the project's state vouches for, unread, or one that hashes to its pin. a push counts
/// any file the state does not say holds other bytes that has the pinned size, unread, since
/// taking it reads and checks it. what is left, and the archives whose files are not known, a
/// push downloads first.
///
/// # Errors
///
/// `offline.missing` with the build to run, when the cache lacks a file of a recipe's result,
/// which no download brings; `project.invalid` for a pin no bytes can have; and a stop.
fn settle(
    context: &Context,
    project: &Project,
    manifest: &Manifest,
    lock: &Lock,
    cache: &Cache,
    planned: &mut Planned,
    dry_run: bool,
) -> Result<(), Error> {
    if !planned.lacked.is_empty() {
        planned.folder = DataFolder::planned(project, manifest, lock, cache);
        let wanted: Vec<crate::mirror::Wanted> = planned
            .lacked
            .iter()
            .map(|at| wanted(&planned.needed[*at]))
            .collect();
        let held = if dry_run {
            planned.folder.holds(context, &wanted)
        } else {
            planned.folder.may_hold(context, &wanted)
        };
        context.stop.check()?;
        let (taking, lacked): (Vec<_>, Vec<_>) =
            planned.lacked.iter().zip(held).partition(|(_, held)| *held);
        planned.taking = taking.into_iter().map(|(at, _)| *at).collect();
        planned.lacked = lacked.into_iter().map(|(at, _)| *at).collect();
    }
    let lacked: Vec<&Needed> = planned
        .lacked
        .iter()
        .map(|at| &planned.needed[*at])
        .collect();
    match mispinned(cache, lock, &lacked).or_else(|| unbuilt(&lacked)) {
        Some(error) => Err(with_report(error, &report(planned, Outcome::Stopped))),
        None => Ok(()),
    }
}

/// the datasets whose files `planned` lacks or whose archives' files it does not know, which a
/// push downloads first.
fn fetching(planned: &Planned) -> BTreeSet<&str> {
    planned
        .lacked
        .iter()
        .map(|at| &planned.needed[*at])
        .filter(|needed| matches!(needed.source, Source::Pinned))
        .map(|needed| needed.dataset.as_str())
        .chain(planned.unknown.iter().map(|(dataset, _)| dataset.as_str()))
        .collect()
}

/// downloads into the cache what a push lacks of the fetched datasets `names`, as a sync
/// downloads it: the data folder and the mirrors are asked first, each file is checked against
/// its pin, and an archive whose files are not known is unpacked. each download is told.
///
/// # Errors
///
/// offline, or in mirror-only mode, what no mirror holds, with what to do on the next push; a
/// download that fails; and a stop.
fn fetch(
    context: &Context,
    project: &Project,
    manifest: &Manifest,
    lock: &Lock,
    cache: &Cache,
    names: &BTreeSet<&str>,
) -> Result<(), Error> {
    if names.is_empty() {
        return Ok(());
    }
    let runtime = tokio::runtime::Handle::try_current()
        .map_err(|err| Error::new(Kind::LocalIo, format!("cannot download: {err}")))?;
    let work = Loop::new(context, project, Arc::new(context.net()), cache.clone()).readying("push");
    runtime.block_on(work.ready(manifest, lock, names))
}

/// the error for a file of `lacked` that `lock` pins at another size than the bytes its SHA-256
/// names, which the cache holds: no download could bring other bytes, so none is tried.
fn mispinned(cache: &Cache, lock: &Lock, lacked: &[&Needed]) -> Option<Error> {
    lacked
        .iter()
        .filter(|needed| matches!(needed.source, Source::Pinned))
        .find_map(|needed| {
            let (digest, size) = (needed.item.digest, needed.item.size);
            let actual = place::cached_other_size(cache, &digest, size)?;
            let file = lock.get(&needed.dataset)?.files.iter().find(|file| {
                file.unpacks_to.is_none() && file.sha256 == digest && file.size == size
            })?;
            let label = format!("{}/{}", needed.dataset, file.path);
            Some(place::mispinned(&label, size, actual))
        })
}

/// takes into the cache the files `planned.taking` names, from the data folder, each only when
/// its bytes there hash to its pin. one the data folder held other bytes of after all is
/// downloaded instead, as any file the cache lacks.
///
/// # Errors
///
/// a failure to write the cache or to download, a stop, and `offline.missing` naming the build
/// for a file of a recipe's result the data folder held other bytes of.
fn take(
    context: &Context,
    project: &Project,
    manifest: &Manifest,
    lock: &Lock,
    cache: &Cache,
    planned: &Planned,
) -> Result<(), Error> {
    if planned.taking.is_empty() {
        return Ok(());
    }
    let wanted = planned
        .taking
        .iter()
        .map(|at| wanted(&planned.needed[*at]))
        .collect();
    let rest = planned.folder.take(context, cache, wanted)?;
    if rest.is_empty() {
        return Ok(());
    }
    let lacked: Vec<&Needed> = planned
        .taking
        .iter()
        .map(|at| &planned.needed[*at])
        .filter(|needed| rest.iter().any(|want| want.digest == needed.item.digest))
        .collect();
    if let Some(error) = unbuilt(&lacked) {
        return Err(with_report(error, &report(planned, Outcome::Stopped)));
    }
    let names = lacked
        .iter()
        .map(|needed| needed.dataset.as_str())
        .collect();
    fetch(context, project, manifest, lock, cache, &names)
        .map_err(|error| with_report(error, &report(planned, Outcome::Stopped)))
}

/// `needed` as the data folder is asked for it.
fn wanted(needed: &Needed) -> crate::mirror::Wanted {
    crate::mirror::Wanted {
        digest: needed.item.digest,
        size: needed.item.size,
        label: needed.label.clone(),
    }
}

/// what a push sends to a mirror, and the archives whose files this machine does not know.
struct Wanted {
    /// each file once, then the lists of what their archives unpack to.
    needed: Vec<Needed>,
    /// each archive whose files this machine does not know, with its dataset.
    unknown: Vec<(String, Fetch)>,
}

/// what pushing `datasets` needs on the mirror. an archive's members are the ones its
/// dataset's members tables keep, as a sync places them, so a member left out is not sent.
fn needed(
    manifest: &Manifest,
    lock: &Lock,
    cache: &Cache,
    datasets: &[String],
) -> Result<Wanted, Error> {
    let mut seen = HashMap::new();
    let (mut files, mut lists, mut unknown) = (Vec::new(), Vec::new(), Vec::new());
    let mut add =
        |into: &mut Vec<Needed>, item: Item, label: String, dataset: &str, source, fetch| {
            let Some(&at) = seen.get(&(item.folder, item.digest)) else {
                seen.insert((item.folder, item.digest), into.len());
                into.push(Needed {
                    item,
                    label,
                    dataset: dataset.to_owned(),
                    source,
                    fetch,
                });
                return;
            };
            // bytes a fetched dataset pins come with its download, though a result holds them too
            let first: &mut Needed = &mut into[at];
            if matches!(first.source, Source::Result) && matches!(source, Source::Pinned) {
                first.source = source;
                dataset.clone_into(&mut first.dataset);
                first.fetch = fetch;
            }
        };
    for name in datasets {
        if let Some(result) = lock.result(name) {
            // a result's files are pinned by data.lock itself, so it needs no list on the mirror
            for member in &result.files {
                let item = object(member.sha256, member.size);
                let label = format!("{name}/{}", member.path);
                add(&mut files, item, label, name, Source::Result, None);
            }
            continue;
        }
        let Some((locked, filters)) = recipe::source_read(manifest, lock, name) else {
            continue;
        };
        // each archive's list of files, as the record that gave its members holds it
        let records = RefCell::new(Vec::new());
        let members = |archive: &Archive<'_>| {
            let found = archive.recorded(cache)?;
            if let Some(tree) = archive.file.unpacks_to {
                let size = crate::cache::tree(&found).len() as u64;
                let item = Item {
                    folder: TREES,
                    digest: tree,
                    size,
                };
                records
                    .borrow_mut()
                    .push((item, archive.key(), archive.file.path.clone()));
            }
            Some(found)
        };
        let mut planned = plan::Plan::default();
        plan::expand(
            &mut planned,
            name,
            name,
            false,
            &locked.files,
            filters,
            &members,
        )?;
        let fetch = |file: &LockedFile| Fetch {
            path: format!("{name}/{}", file.path),
            size: file.size,
            archive: file.unpacks_to.is_some(),
        };
        for target in &planned.targets {
            let item = object(target.sha256, target.size);
            let brings = target.file().map(fetch);
            add(
                &mut files,
                item,
                target.path.clone(),
                name,
                Source::Pinned,
                brings,
            );
        }
        unknown.extend(
            planned
                .unknown
                .iter()
                .map(|(_, file)| (name.clone(), fetch(file))),
        );
        for (item, archive, path) in records.into_inner() {
            let label = format!("{name}/{path}");
            add(&mut lists, item, label, name, Source::List(archive), None);
        }
    }
    files.extend(lists);
    Ok(Wanted {
        needed: files,
        unknown,
    })
}

fn object(digest: Sha256, size: u64) -> Item {
    Item {
        folder: OBJECTS,
        digest,
        size,
    }
}

/// where in `needed` the files are that the mirror lacks, as `there` says, and the cache lacks
/// too, looked up a few at once.
fn lacking(context: &Context, cache: &Cache, needed: &[Needed], there: &[There]) -> Vec<usize> {
    let looked: Vec<(usize, &Needed)> = needed
        .iter()
        .enumerate()
        .zip(there)
        .filter(|((_, needed), there)| {
            **there != There::Present && !matches!(needed.source, Source::List(_))
        })
        .map(|(at, _)| at)
        .collect();
    let found = crate::place::run_all(&looked, context.threads, |(_, needed)| {
        cache
            .find(&needed.item.digest, Some(needed.item.size))
            .is_some()
    });
    looked
        .iter()
        .zip(found)
        .filter(|(_, found)| !found)
        .map(|((at, _), _)| *at)
        .collect()
}

/// the error for the files of recipes' results among `lacked`, which no download brings, with
/// the build that makes each, which only a person may run.
fn unbuilt(lacked: &[&Needed]) -> Option<Error> {
    let made: Vec<&str> = lacked
        .iter()
        .filter(|needed| matches!(needed.source, Source::Result))
        .map(|needed| needed.label.as_str())
        .collect();
    if made.is_empty() {
        return None;
    }
    let error = Error::new(
        Kind::OfflineMissing,
        "this machine's cache lacks files the push needs, which only a build makes",
    );
    let built: BTreeSet<&str> = lacked
        .iter()
        .filter(|needed| matches!(needed.source, Source::Result))
        .map(|needed| needed.dataset.as_str())
        .collect();
    let help: Vec<String> = built
        .iter()
        .map(|name| format!("`fetchloom build {name}` makes {name}'s files"))
        .collect();
    Some(named_lines(error, &made).help(help.join("; ")))
}

/// the error for files a push still lacks once it fetched them, `lacked`, and the archives whose
/// files it still does not know, `unknown`, as when something took them from the cache
/// meanwhile: the next push fetches them again.
fn missing(lacked: &[&Needed], unknown: &[(String, Fetch)]) -> Error {
    let lines: Vec<String> = unknown
        .iter()
        .map(|(_, archive)| format!("the files inside {}", archive.path))
        .chain(lacked.iter().map(|needed| needed.label.clone()))
        .collect();
    let error = Error::new(
        Kind::OfflineMissing,
        "this machine's cache lacks files the push needs",
    );
    let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
    named_lines(error, &lines).help("run the push again, which downloads them")
}

/// `error` with the first few of `lines` as details, and how many more there are.
fn named_lines(mut error: Error, lines: &[&str]) -> Error {
    for line in lines.iter().take(super::fetch::NAMED) {
        error = error.detail((*line).to_owned());
    }
    if lines.len() > super::fetch::NAMED {
        error = error.detail(format!("and {} more", lines.len() - super::fetch::NAMED));
    }
    error
}

/// the datasets of `datasets` that ask before they are pushed: one fetched from a source that
/// wanted an account or accepted terms or did not say, and a recipe's result made from one.
/// `access` is a hint from the source, never a licence check.
fn asking(manifest: &Manifest, lock: &Lock, datasets: &[String]) -> Vec<Asking> {
    datasets
        .iter()
        .filter_map(|name| {
            if let Some(locked) = lock.get(name) {
                return (locked.access != Access::Public).then(|| Asking {
                    dataset: name.clone(),
                    access: locked.access,
                    from: Vec::new(),
                });
            }
            // anyone who reads a result reads what it was made from
            let from: Vec<(&str, Access)> = recipe::closure(&manifest.datasets, &[name.as_str()])
                .into_iter()
                .filter_map(|input| lock.get(input).map(|locked| (input, locked.access)))
                .filter(|(_, access)| *access != Access::Public)
                .collect();
            let access = from
                .iter()
                .map(|(_, access)| *access)
                .max_by_key(|access| strictness(*access))?;
            Some(Asking {
                dataset: name.clone(),
                access,
                from: from
                    .into_iter()
                    .map(|(input, _)| input.to_owned())
                    .collect(),
            })
        })
        .collect()
}

/// how much an access asks of a reader: an account most, then accepted terms, then a source
/// that did not say.
fn strictness(access: Access) -> u8 {
    match access {
        Access::Login => 3,
        Access::Terms => 2,
        Access::Unknown => 1,
        Access::Public => 0,
    }
}

/// asks before copying the files of datasets whose sources wanted an account or accepted terms,
/// or did not say, since anyone who can read the mirror gets them without either. `--yes`
/// answers; without a person to ask, the push stops before writing, with no command to run, as
/// only a person may agree.
///
/// # Errors
///
/// `unconfirmed` when nobody said yes.
fn ask(context: &Context, planned: &Planned) -> Result<(), Error> {
    if planned.asks.is_empty() || context.yes {
        return Ok(());
    }
    let (it, its) = if planned.asks.len() == 1 {
        ("it", "its")
    } else {
        ("them", "their")
    };
    let why = reasons(&planned.asks);
    let target = planned.target.display();
    let names: Vec<&str> = planned
        .asks
        .iter()
        .map(|ask| ask.dataset.as_str())
        .collect();
    let cancel = format!("the push to {target}");
    let question = format!(
        "{why}. anyone who can read {target} can take {its} files from there. push {it} anyway?"
    );
    match context
        .output
        .confirm(&question)
        .map_err(|error| error.cancelling(&cancel))?
    {
        Some(true) => Ok(()),
        Some(false) => Err(Error::new(
            Kind::Unconfirmed,
            format!("did not push {}", named(&names)),
        )
        .cancelled(cancel)
        .unchanged()),
        None => {
            let error = Error::new(
                Kind::Unconfirmed,
                format!("{why}, so pushing {it} needs a yes"),
            );
            let help = format!(
                "answer yes at a terminal, or add --yes once everyone who can read {target} may have {it}"
            );
            Err(with_report(
                error.help(help),
                &report(planned, Outcome::Stopped),
            ))
        }
    }
}

/// why `asks` ask first, as a person reads it: `iris needs accepted terms`.
fn reasons(asks: &[Asking]) -> String {
    let alike = |access: Access| -> Vec<&str> {
        asks.iter()
            .filter(|ask| ask.access == access && ask.from.is_empty())
            .map(|ask| ask.dataset.as_str())
            .collect()
    };
    let needs = |names: &[&str], what: &str| {
        let verb = if names.len() == 1 { "needs" } else { "need" };
        format!("{} {verb} {what}", named(names))
    };
    let mut reasons = Vec::new();
    let login = alike(Access::Login);
    if !login.is_empty() {
        reasons.push(needs(&login, "an account to read"));
    }
    let terms = alike(Access::Terms);
    if !terms.is_empty() {
        reasons.push(needs(&terms, "accepted terms"));
    }
    match alike(Access::Unknown).as_slice() {
        [] => {}
        [one] => reasons.push(format!("{one}'s source did not say who may read it")),
        many => reasons.push(format!(
            "the sources of {} did not say who may read them",
            named(many)
        )),
    }
    for ask in asks.iter().filter(|ask| !ask.from.is_empty()) {
        let from: Vec<&str> = ask.from.iter().map(String::as_str).collect();
        reasons.push(format!("{} is made from {}", ask.dataset, named(&from)));
    }
    reasons.join("; ")
}

/// writes what `planned` found the mirror lacks: its files first, a few at once, then the lists
/// of what archives unpack to, so a reader never finds a list before the files it names. says
/// whether it started the mirror, and what writing each file gave, in the order of
/// `planned.needed` without what was there.
///
/// # Errors
///
/// the first file that could not be written; those written before it keep their names.
fn write(context: &Context, cache: &Cache, planned: &Planned) -> Result<(bool, Vec<Put>), Error> {
    let root = &planned.target;
    let (mut target, started) =
        Target::open(root, Arc::clone(&context.stop)).map_err(|refused| refusal(root, refused))?;
    if started {
        context
            .output
            .step("Started", &format!("a mirror in {}", root.display()));
    }
    let copy: Vec<(&Needed, There)> = planned
        .needed
        .iter()
        .zip(&planned.there)
        .filter(|(_, there)| **there != There::Present)
        .map(|(needed, there)| (needed, *there))
        .collect();
    let items: Vec<Item> = copy.iter().map(|(needed, _)| needed.item).collect();
    target.prepare(&items).map_err(|err| {
        Error::io(
            format_args!("could not make folders in the mirror {}", root.display()),
            &err,
        )
    })?;
    target.sweep(SystemTime::now());
    let (files, lists): (Vec<_>, Vec<_>) = copy
        .into_iter()
        .partition(|(needed, _)| !matches!(needed.source, Source::List(_)));
    let mut puts = put_files(context, cache, &target, &files)?;
    puts.extend(put_lists(context, cache, &target, &lists)?);
    let written: Vec<Item> = files
        .iter()
        .chain(&lists)
        .zip(&puts)
        .filter(|(_, put)| **put != Put::Present)
        .map(|((needed, _), _)| needed.item)
        .collect();
    if started || !written.is_empty() {
        target.flush(&written).map_err(|err| {
            Error::io(
                format_args!(
                    "pushed to {}, but could not make sure the files stay after a power cut",
                    root.display()
                ),
                &err,
            )
        })?;
    }
    context.stop.finish();
    Ok((started, puts))
}

/// writes `files` from the cache as [`Target::put_all`] does, with a bar of the bytes written,
/// then, once they are on the disk, one of the bytes read back and named in its place.
fn put_files(
    context: &Context,
    cache: &Cache,
    target: &Target,
    files: &[(&Needed, There)],
) -> Result<Vec<Put>, Error> {
    let total = files.iter().map(|(needed, _)| needed.item.size).sum();
    let what = units::count(files.len() as u64, "file", "files");
    let output = &context.output;
    let stage = |verb| {
        let bar = output.bar(verb, what.clone());
        bar.start(Some(total), 0);
        let sizes = files.iter().map(|(needed, _)| Some(needed.item.size));
        (bar, output.ticker(verb, what.clone(), sizes))
    };
    let writing = Mutex::new(Some(stage("Pushing")));
    let checking = OnceLock::new();
    let wrote = |size| {
        if let Some((bar, ticker)) = &*writing.lock().unwrap_or_else(PoisonError::into_inner) {
            bar.advance(size);
            ticker.advance(size);
        }
    };
    let checked = |size| {
        let (bar, ticker) = checking.get_or_init(|| {
            drop(
                writing
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .take(),
            );
            stage("Checking")
        });
        bar.advance(size);
        ticker.advance(size);
    };
    let items: Vec<(Item, There)> = files
        .iter()
        .map(|(needed, there)| (needed.item, *there))
        .collect();
    let source = |item: &Item| read_unfollowed(&cache.object_path(&item.digest));
    let put = target.put_all(&items, context.threads, &source, &wrote, &checked);
    drop((writing, checking));
    put.map_err(|failed| failed_put(failed, files, target.root()))
}

/// writes the lists of what archives unpack to, `lists`, rendered from the cache's records of
/// them, as [`Target::put_all`] does.
fn put_lists(
    context: &Context,
    cache: &Cache,
    target: &Target,
    lists: &[(&Needed, There)],
) -> Result<Vec<Put>, Error> {
    let archives: HashMap<Sha256, Sha256> = lists
        .iter()
        .filter_map(|(needed, _)| match needed.source {
            Source::List(archive) => Some((needed.item.digest, archive)),
            Source::Pinned | Source::Result => None,
        })
        .collect();
    let items: Vec<(Item, There)> = lists
        .iter()
        .map(|(needed, there)| (needed.item, *there))
        .collect();
    let source = |item: &Item| {
        let members = archives
            .get(&item.digest)
            .and_then(|archive| cache.unpacked(archive, Some(&item.digest)))
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "the cache no longer records what the archive unpacks to",
                )
            })?;
        Ok(std::io::Cursor::new(
            crate::cache::tree(&members).into_bytes(),
        ))
    };
    target
        .put_all(&items, context.threads, &source, &|_| {}, &|_| {})
        .map_err(|failed| failed_list(failed, lists, target.root()))
}

/// the error for `failed`, from writing the lists `lists` into the mirror at `root`. a list the
/// cache stopped recording after the push planned is missing, as before a push fetched it.
fn failed_list(failed: Failed, lists: &[(&Needed, There)], root: &Path) -> Error {
    match failed {
        Failed::File(at, Fault::Source(_)) => Error::new(
            Kind::OfflineMissing,
            format!(
                "the cache no longer knows the files inside {}",
                lists[at].0.label
            ),
        )
        .help("run the push again, which fetches what it holds"),
        failed => failed_put(failed, lists, root),
    }
}

/// the error for `failed`, from writing `files` into the mirror at `root`.
fn failed_put(failed: Failed, files: &[(&Needed, There)], root: &Path) -> Error {
    match failed {
        Failed::File(at, fault) => failure(fault, files[at].0, root),
        Failed::Flush(err) if err.kind() == std::io::ErrorKind::Interrupted => Stop::stopped(),
        Failed::Flush(err) => Error::io(
            format_args!(
                "could not put the files pushed to {} on its disk",
                root.display()
            ),
            &err,
        ),
    }
}

/// the error for `fault`, from writing `needed` into the mirror at `root`.
fn failure(fault: Fault, needed: &Needed, root: &Path) -> Error {
    let label = &needed.label;
    match fault {
        Fault::Mirror(err) if err.kind() == std::io::ErrorKind::Interrupted => Stop::stopped(),
        Fault::Mirror(err) => Error::io(
            format_args!("could not push {label} to {}", root.display()),
            &err,
        ),
        Fault::Source(err) => Error::io(
            format_args!("could not read the cache's copy of {label}"),
            &err,
        ),
        Fault::OtherBytes => Error::new(
            Kind::IntegrityMismatch,
            format!("the cache's copy of {label} does not hold its pinned bytes, so it was not pushed"),
        )
        .help("run `fetchloom cache verify`, which notes it, then `fetchloom sync` to fetch it again"),
        Fault::ReadBack => Error::new(
            Kind::IntegrityMismatch,
            format!(
                "{label}, read back from {}, holds other bytes than were written, so it was not given its name",
                root.display()
            ),
        )
        .help("check the disk that holds the mirror, then push again"),
        Fault::Replaced => Error::new(
            Kind::LocalIo,
            format!(
                "something replaced {label} in the mirror {} while push wrote it, so it was not given its name",
                root.display()
            ),
        )
        .help("stop whatever else writes into that folder, such as a sync client, a backup tool or antivirus, then push again"),
    }
}

/// the error for a folder that cannot take a push.
fn refusal(root: &Path, refused: Refused) -> Error {
    let shown = root.display();
    match refused {
        Refused::NotEmpty => Error::new(
            Kind::LocalNotEmpty,
            format!("{shown} holds files, but no fetchloom-mirror file says it is a mirror"),
        )
        .help("name an empty folder, or the top folder of a mirror, which holds its fetchloom-mirror file, and check that its disk is mounted"),
        Refused::NoParent => Error::new(
            Kind::LocalIo,
            format!("neither {shown} nor the folder above it is there"),
        )
        .help("check the folder's name, and that its disk is mounted"),
        Refused::NotAFolder => Error::new(Kind::LocalIo, format!("{shown} is not a folder"))
            .help("name a folder for the mirror"),
        Refused::Failed(err) => Error::io(format_args!("could not use the mirror {shown}"), &err),
    }
}

/// `error` with what the push found, so `--json` still says it.
fn with_report(error: Error, report: &Report) -> Error {
    match serde_json::to_value(report) {
        Ok(value) => error.with_result(value),
        Err(_) => error,
    }
}

/// how a push that `planned` ended.
#[derive(Clone, Copy)]
enum Outcome<'a> {
    /// it wrote: whether it started the mirror, and what writing each file not there gave, in
    /// order, every file before every list.
    Written(bool, &'a [Put]),
    /// it stopped before writing, as when the cache lacks a file or nobody said yes.
    Stopped,
    DryRun,
}

/// what `planned` comes to, as it ended: what it wrote, or what it would have written.
fn report(planned: &Planned, outcome: Outcome<'_>) -> Report {
    let dry_run = matches!(outcome, Outcome::DryRun);
    let mut report = Report::empty(planned.target.clone(), dry_run);
    let (started, puts) = match outcome {
        Outcome::Written(started, puts) => (started, Some(puts)),
        Outcome::Stopped => (false, None),
        Outcome::DryRun => (planned.state == State::New, None),
    };
    report.started = started;
    report.selected.clone_from(&planned.selected);
    report.datasets.clone_from(&planned.datasets);
    report.asks = planned
        .asks
        .iter()
        .map(|ask| Ask {
            dataset: ask.dataset.clone(),
            access: ask.access.word(),
        })
        .collect();
    let mut puts = puts.map(<[Put]>::iter);
    for (needed, there) in planned.needed.iter().zip(&planned.there) {
        let put = match (there, &mut puts) {
            (There::Present, _) => Some(Put::Present),
            (_, Some(puts)) => puts.next().copied(),
            (There::Damaged, None) => Some(Put::Replaced),
            (There::Absent, None) => Some(Put::Copied),
        };
        report.tally(needed, put);
    }
    if let Some(changes) = &mut report.changes {
        (changes.download, changes.unpack) = downloads(planned);
    }
    report
}

/// what a push would download first: the file that brings each it lacks, each once, and the
/// archives among them, which are unpacked once they are in.
fn downloads(planned: &Planned) -> (Vec<Fetch>, Vec<String>) {
    let mut seen = HashSet::new();
    let lacking = planned
        .lacked
        .iter()
        .filter_map(|at| planned.needed[*at].fetch.as_ref());
    let download: Vec<Fetch> = lacking
        .chain(planned.unknown.iter().map(|(_, archive)| archive))
        .filter(|fetch| seen.insert(fetch.path.as_str()))
        .cloned()
        .collect();
    let unpack = download
        .iter()
        .filter(|fetch| fetch.archive)
        .map(|fetch| fetch.path.clone())
        .collect();
    (download, unpack)
}

impl Report {
    /// a report of nothing pushed yet into `target`.
    fn empty(target: PathBuf, dry_run: bool) -> Report {
        Report {
            target,
            started: false,
            selected: None,
            datasets: Vec::new(),
            objects: 0,
            bytes: 0,
            copied: 0,
            copied_bytes: 0,
            present: 0,
            present_bytes: 0,
            replaced: 0,
            trees: 0,
            asks: Vec::new(),
            dry_run,
            changes: dry_run.then(|| Changes {
                copy: Vec::new(),
                trees: Vec::new(),
                download: Vec::new(),
                unpack: Vec::new(),
            }),
        }
    }

    /// counts `needed`, which writing it gave `put`, or which was not written.
    fn tally(&mut self, needed: &Needed, put: Option<Put>) {
        let size = needed.item.size;
        if let Source::List(_) = needed.source {
            if matches!(put, Some(Put::Copied | Put::Replaced)) {
                self.trees += 1;
                if let Some(changes) = &mut self.changes {
                    changes.trees.push(copied(needed));
                }
            }
            return;
        }
        self.objects += 1;
        self.bytes += size;
        match put {
            Some(Put::Present) => {
                self.present += 1;
                self.present_bytes += size;
            }
            Some(put @ (Put::Copied | Put::Replaced)) => {
                self.copied += 1;
                self.copied_bytes += size;
                self.replaced += u64::from(put == Put::Replaced);
                if let Some(changes) = &mut self.changes {
                    changes.copy.push(copied(needed));
                }
            }
            None => {}
        }
    }
}

fn copied(needed: &Needed) -> Copied {
    Copied {
        path: needed.label.clone(),
        sha256: needed.item.digest,
        size: needed.item.size,
    }
}

/// the lines of a push that wrote: what it copied, what it replaced, what it found there.
fn tell(context: &Context, planned: &Planned, report: &Report) {
    let output = &context.output;
    let target = planned.target.display();
    if report.copied > 0 {
        output.step(
            "Pushed",
            &format!(
                "{} \u{b7} {} to {target}",
                units::count(report.copied, "file", "files"),
                units::size(report.copied_bytes)
            ),
        );
    }
    if report.trees > 0 {
        output.detail(&format!(
            "and what {}",
            units::count(report.trees, "archive holds", "archives hold")
        ));
    }
    if report.replaced > 0 {
        output.mark(
            "Replaced",
            &format!(
                "{} on the mirror",
                units::count(report.replaced, "damaged copy", "damaged copies")
            ),
        );
    }
    if report.present > 0 {
        output.step(
            "Skipped",
            &format!(
                "{} already there \u{b7} {}",
                units::count(report.present, "file", "files"),
                units::size(report.present_bytes)
            ),
        );
    }
    output.done("Ok", &everything(&planned.datasets));
    let mirrors = toml_edit::Value::from(target.to_string());
    output.next(&format!(
        "teammates add mirrors = [{mirrors}] to their settings file"
    ));
}

/// the lines of a dry run: what a push would take from the data folder, write, find there and
/// ask about.
fn tell_dry(context: &Context, planned: &Planned, report: &Report) {
    let output = &context.output;
    let target = planned.target.display();
    if !planned.taking.is_empty() {
        let bytes = planned
            .taking
            .iter()
            .map(|at| planned.needed[*at].item.size)
            .sum();
        let files = planned.taking.len() as u64;
        planned.folder.tell(output, "Would copy", files, bytes);
    }
    let (download, unpack): (&[Fetch], &[String]) = match &report.changes {
        Some(changes) => (&changes.download, &changes.unpack),
        None => (&[], &[]),
    };
    for fetch in download {
        output.step(
            "Would fetch",
            &format!("{} \u{b7} {}", fetch.path, units::size(fetch.size)),
        );
    }
    for archive in unpack {
        output.step("Would open", archive);
    }
    if !planned.unknown.is_empty() {
        output.hint("an archive's files are known once it is unpacked, so they are not counted");
    }
    if report.started {
        output.step("Would start", &format!("a mirror in {target}"));
    }
    if report.copied > 0 {
        output.step(
            "Would push",
            &format!(
                "{} \u{b7} {} to {target}",
                units::count(report.copied, "file", "files"),
                units::size(report.copied_bytes)
            ),
        );
    }
    if report.replaced > 0 {
        output.mark(
            "Damaged",
            &format!(
                "{} on the mirror, which a push replaces",
                units::count(report.replaced, "copy", "copies")
            ),
        );
    }
    if report.present > 0 {
        output.step(
            "Would skip",
            &format!(
                "{} already there \u{b7} {}",
                units::count(report.present, "file", "files"),
                units::size(report.present_bytes)
            ),
        );
    }
    if !planned.asks.is_empty() && !context.yes {
        output.step("Would ask", &format!("first: {}", reasons(&planned.asks)));
    }
    let nothing = report.copied == 0 && report.trees == 0 && !report.started && download.is_empty();
    let everything = everything(&planned.datasets);
    super::project::dry_run_done(output, nothing.then_some(everything.as_str()));
}

/// the closing line of a push of `datasets`: every file they need is on the mirror.
fn everything(datasets: &[String]) -> String {
    let names: Vec<&str> = datasets.iter().map(String::as_str).collect();
    match names.as_slice() {
        [] => "data.toml has no datasets yet, so there is nothing to push".to_owned(),
        [one] => format!("every file {one} needs is on the mirror"),
        few if few.len() <= 3 => format!("every file {} need is on the mirror", listed(few)),
        many => format!(
            "every file the {} datasets need is on the mirror",
            many.len()
        ),
    }
}

/// the names of datasets as a person reads them in a sentence: the first few, then how many
/// more.
fn named(names: &[&str]) -> String {
    const FEW: usize = 5;
    if names.len() <= FEW + 1 {
        return listed(names);
    }
    format!("{} and {} more", names[..FEW].join(", "), names.len() - FEW)
}

/// `words` as a person reads a list: `a`, `a and b`, `a, b and c`.
fn listed(words: &[&str]) -> String {
    match words.split_last() {
        None => String::new(),
        Some((last, [])) => (*last).to_owned(),
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::Member;
    use crate::project::lock::derived::Derived;
    use crate::project::lock::{Locked, LockedFile};

    /// raw and vault fetched, clean made from both, report from clean, and other fetched alone.
    fn manifest() -> Manifest {
        Manifest::parse(
            r#"
[datasets.raw]
ref = "zenodo:1"

[datasets.vault]
ref = "zenodo:2"

[datasets.other]
ref = "zenodo:3"

[datasets.clean]
from = ["raw", "vault"]
run = ["python3", "clean.py", "{raw}", "{vault}", "{out}"]

[datasets.report]
from = ["clean"]
run = ["python3", "report.py", "{clean}", "{out}"]
"#,
        )
        .unwrap()
    }

    fn locked(name: &str, access: Access) -> Locked {
        Locked {
            name: name.to_owned(),
            reference: "zenodo:1".to_owned(),
            spec: "sha256:00".to_owned(),
            extract: true,
            resolved: "zenodo:1@1".to_owned(),
            title: None,
            license: None,
            doi: None,
            access,
            timestamp: "2026-11-10T00:00:00Z".to_owned(),
            files: Vec::<LockedFile>::new(),
        }
    }

    fn member(path: &str, bytes: &[u8]) -> Member {
        Member {
            path: path.to_owned(),
            size: bytes.len() as u64,
            sha256: crate::digest::sha256(bytes),
        }
    }

    fn names(selected: &[String]) -> Vec<&str> {
        selected.iter().map(String::as_str).collect()
    }

    /// `--inputs` adds what the named recipes are made from, all the way down, and nothing else.
    #[test]
    fn inputs_add_what_the_named_recipes_are_made_from() {
        let manifest = manifest();
        let report = ["report".to_owned()];
        assert_eq!(names(&selected(&manifest, &report, false)), ["report"]);
        assert_eq!(
            names(&selected(&manifest, &report, true)),
            ["clean", "raw", "report", "vault"]
        );
        let other = ["other".to_owned()];
        assert_eq!(names(&selected(&manifest, &other, true)), ["other"]);
        assert_eq!(selected(&manifest, &[], true).len(), 5);
    }

    /// a recipe's result goes by the files data.lock pins for it, and the cache lacking one says
    /// to build it, which only a person may run.
    #[test]
    fn a_result_goes_by_the_files_data_lock_pins_for_it() {
        let files = vec![member("a.csv", b"a"), member("b.csv", b"b")];
        let lock = Lock {
            datasets: Vec::new(),
            derived: vec![Derived {
                name: "clean".to_owned(),
                from: Vec::new(),
                run: vec!["python3".to_owned()],
                code: Vec::new(),
                env: std::collections::BTreeMap::new(),
                key: crate::digest::sha256(b"key"),
                timestamp: "2026-11-10T00:00:00Z".to_owned(),
                tree: crate::cache::tree_hash(&files),
                files,
            }],
        };
        let folder = tempfile::tempdir().unwrap();
        let cache = Cache::open(folder.path().join("cache")).unwrap();
        let Wanted { needed, unknown } =
            needed(&manifest(), &lock, &cache, &["clean".to_owned()]).unwrap();
        assert!(unknown.is_empty());
        let labels: Vec<&str> = needed.iter().map(|needed| needed.label.as_str()).collect();
        assert_eq!(labels, ["clean/a.csv", "clean/b.csv"]);
        assert!(
            needed
                .iter()
                .all(|needed| needed.item.folder == OBJECTS
                    && matches!(needed.source, Source::Result))
        );
        let lacked: Vec<&Needed> = needed.iter().collect();
        let error = unbuilt(&lacked).unwrap();
        assert_eq!(
            error.help.as_deref(),
            Some("`fetchloom build clean` makes clean's files")
        );
    }

    /// a result asks first when anything it is made from does, as strictly as the strictest.
    #[test]
    fn a_result_asks_as_strictly_as_what_it_is_made_from() {
        let manifest = manifest();
        let lock = Lock {
            // sorted by name, as data.lock keeps them
            datasets: vec![
                locked("other", Access::Public),
                locked("raw", Access::Terms),
                locked("vault", Access::Login),
            ],
            derived: Vec::new(),
        };
        let asks = asking(&manifest, &lock, &["report".to_owned(), "other".to_owned()]);
        assert_eq!(asks.len(), 1);
        assert_eq!(asks[0].dataset, "report");
        assert_eq!(asks[0].access, Access::Login);
        assert_eq!(asks[0].from, ["raw", "vault"]);
        let public = Lock {
            datasets: vec![
                locked("raw", Access::Public),
                locked("vault", Access::Public),
            ],
            derived: Vec::new(),
        };
        assert!(asking(&manifest, &public, &["report".to_owned()]).is_empty());
    }

    /// a list the cache stopped recording after the push planned is missing, with what to run,
    /// not a failure of the disk: the next push fetches it again.
    #[test]
    fn a_list_the_cache_lost_is_missing_with_what_to_run() {
        let digest = crate::digest::sha256(b"images.zip");
        let needed = Needed {
            item: Item {
                folder: TREES,
                digest: crate::digest::sha256(b"its list"),
                size: 8,
            },
            label: "pets/images.zip".into(),
            dataset: "pets".into(),
            source: Source::List(digest),
            fetch: None,
        };
        let lost = std::io::Error::new(std::io::ErrorKind::NotFound, "no record");
        let error = failed_list(
            Failed::File(0, Fault::Source(lost)),
            &[(&needed, There::Absent)],
            Path::new("mirror"),
        );
        assert_eq!(error.kind, Kind::OfflineMissing);
        assert_eq!(
            error.message,
            "the cache no longer knows the files inside pets/images.zip"
        );
        assert_eq!(
            error.help.as_deref(),
            Some("run the push again, which fetches what it holds")
        );
    }

    /// a file push read back other bytes from points at the disk, `integrity.mismatch`; one
    /// something else put in its place in the mirror is a local problem, `local.io`, with what to
    /// stop, since no disk fault caused it.
    #[test]
    fn a_replaced_file_is_told_apart_from_bytes_the_disk_changed() {
        let needed = Needed {
            item: object(crate::digest::sha256(b"small file\n"), 11),
            label: "many/d00/f00".into(),
            dataset: "many".into(),
            source: Source::Pinned,
            fetch: None,
        };
        let root = Path::new("/mnt/lab/fetchloom");
        let changed = failure(Fault::ReadBack, &needed, root);
        assert_eq!(
            (changed.kind, changed.kind.exit_code()),
            (Kind::IntegrityMismatch, 30)
        );
        let replaced = failure(Fault::Replaced, &needed, root);
        assert_eq!(
            (replaced.kind, replaced.kind.exit_code()),
            (Kind::LocalIo, 50)
        );
        assert_eq!(
            replaced.message,
            "something replaced many/d00/f00 in the mirror /mnt/lab/fetchloom while push wrote it, so it was not given its name"
        );
        assert_eq!(
            replaced.help.as_deref(),
            Some(
                "stop whatever else writes into that folder, such as a sync client, a backup tool or antivirus, then push again"
            )
        );
    }

    fn asks(dataset: &str, access: Access, from: &[&str]) -> Asking {
        Asking {
            dataset: dataset.to_owned(),
            access,
            from: from.iter().map(|name| (*name).to_owned()).collect(),
        }
    }

    /// the question says why each dataset asks, grouped as a person reads it, and a result by
    /// what it is made from.
    #[test]
    fn the_question_says_why_each_dataset_asks() {
        let told = [
            asks("pets", Access::Terms, &[]),
            asks("iris", Access::Unknown, &[]),
            asks("dogs", Access::Unknown, &[]),
            asks("vault", Access::Login, &[]),
            asks("clean", Access::Terms, &["pets"]),
        ];
        assert_eq!(
            reasons(&told),
            "vault needs an account to read; pets needs accepted terms; the sources of iris and dogs did not say who may read them; clean is made from pets"
        );
        assert_eq!(
            reasons(&[asks("iris", Access::Unknown, &[])]),
            "iris's source did not say who may read it"
        );
        let six = ["a", "b", "c", "d", "e", "f"];
        assert_eq!(named(&six), "a, b, c, d, e and f");
        let eight = ["a", "b", "c", "d", "e", "f", "g", "h"];
        assert_eq!(named(&eight), "a, b, c, d, e and 3 more");
    }
}
