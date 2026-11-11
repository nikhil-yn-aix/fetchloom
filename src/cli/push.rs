//! `fetchloom push`: the files data.lock pins, copied from this machine's cache into a folder
//! mirror as mirrors are read, with the lists of what their archives unpack to, so a teammate's
//! sync, a machine without a network or a reviewer can take them from there. it writes the
//! mirror alone, never the project or the cache, and it never downloads or builds: what the
//! cache lacks is named with what to run first.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;

use serde::Serialize;

use super::Context;
use super::args;
use super::project::{find, known, locked};
use super::sync::Scope;
use crate::cache::{Cache, read_unfollowed};
use crate::digest::Sha256;
use crate::error::{Error, Kind};
use crate::mirror::folder::{self, Fault, Item, Put, Refused, State, Target, There};
use crate::mirror::{Location, OBJECTS, TREES};
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
}

/// where the bytes of a [`Needed`] are on this machine.
#[derive(Clone, Copy)]
enum Source {
    /// a file a source pins, in the cache once `fetchloom sync` fetched it.
    Pinned,
    /// a file of a recipe's result, in the cache once `fetchloom build` made it.
    Result,
    /// the cache's record of what the archive with this SHA-256 unpacks to.
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
}

pub(super) fn run(context: &Context, args: &args::Push) -> Result<Report, Error> {
    let target = target(context, args)?;
    let project = find(&context.output)?;
    let (_, manifest) = project.manifest()?;
    for name in &args.names {
        known(&manifest, name, "push")?;
    }
    let datasets = selected(&manifest, &args.names);
    if datasets.is_empty() {
        context.output.done("Ok", &everything(&datasets));
        let target = std::path::absolute(&target).unwrap_or(target);
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
    let selected = (!args.names.is_empty()).then(|| datasets.clone());
    let planned = plan(
        context, &manifest, &lock, &cache, target, selected, datasets,
    )?;
    if args.dry.dry_run {
        let report = report(&planned, Outcome::DryRun);
        tell_dry(context, &planned, &report);
        return Ok(report);
    }
    ask(context, &planned)?;
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

/// the datasets a push sends: those `names` gives, else every dataset sync places, which is
/// every dataset in data.toml.
fn selected(manifest: &Manifest, names: &[String]) -> Vec<String> {
    if names.is_empty() {
        return manifest.datasets.keys().cloned().collect();
    }
    let unique: BTreeSet<&String> = names.iter().collect();
    unique.into_iter().cloned().collect()
}

/// what pushing `datasets` from `cache` into the folder `target` needs, and what the mirror
/// there holds of it already, looked at once a file.
///
/// # Errors
///
/// a folder that cannot take a push, a mirror that cannot be looked in, and `offline.missing`
/// with what to run when the cache lacks a file the mirror lacks too.
fn plan(
    context: &Context,
    manifest: &Manifest,
    lock: &Lock,
    cache: &Cache,
    target: PathBuf,
    selected: Option<Vec<String>>,
    datasets: Vec<String>,
) -> Result<Planned, Error> {
    let target = std::path::absolute(&target).unwrap_or(target);
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
    let planned = Planned {
        target,
        state,
        selected,
        datasets,
        needed,
        there,
        asks,
    };
    if lacked.is_empty() && unknown.is_empty() {
        return Ok(planned);
    }
    let lacked: Vec<&Needed> = lacked.into_iter().map(|at| &planned.needed[at]).collect();
    let error = missing(&lacked, &unknown, planned.selected.as_deref());
    Err(with_report(error, &report(&planned, Outcome::Stopped)))
}

/// what a push sends to a mirror, and the archives whose files this machine does not know.
struct Wanted {
    /// each file once, then the lists of what their archives unpack to.
    needed: Vec<Needed>,
    /// each archive whose files this machine does not know, as `<dataset>/<archive>`.
    unknown: Vec<String>,
}

/// what pushing `datasets` needs on the mirror. an archive's members are the ones its
/// dataset's members tables keep, as a sync places them, so a member left out is not sent.
fn needed(
    manifest: &Manifest,
    lock: &Lock,
    cache: &Cache,
    datasets: &[String],
) -> Result<Wanted, Error> {
    let mut seen = HashSet::new();
    let (mut files, mut lists, mut unknown) = (Vec::new(), Vec::new(), Vec::new());
    let mut add = |into: &mut Vec<Needed>, item: Item, label: String, dataset: &str, source| {
        if seen.insert((item.folder, item.digest)) {
            into.push(Needed {
                item,
                label,
                dataset: dataset.to_owned(),
                source,
            });
        }
    };
    for name in datasets {
        if let Some(result) = lock.result(name) {
            // a result's files are pinned by data.lock itself, so it needs no list on the mirror
            for member in &result.files {
                let item = object(member.sha256, member.size);
                let label = format!("{name}/{}", member.path);
                add(&mut files, item, label, name, Source::Result);
            }
            continue;
        }
        let Some((locked, filters)) = recipe::source_read(manifest, lock, name) else {
            continue;
        };
        // each archive's list of files, as the record that gave its members holds it
        let records = RefCell::new(Vec::new());
        let members = |file: &LockedFile| {
            let found = cache.unpacked(&file.sha256, file.unpacks_to.as_ref())?;
            if let Some(tree) = file.unpacks_to {
                let size = crate::cache::tree(&found).len() as u64;
                let item = Item {
                    folder: TREES,
                    digest: tree,
                    size,
                };
                records
                    .borrow_mut()
                    .push((item, file.sha256, file.path.clone()));
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
        for target in &planned.targets {
            let item = object(target.sha256, target.size);
            add(&mut files, item, target.path.clone(), name, Source::Pinned);
        }
        unknown.extend(
            planned
                .unknown
                .iter()
                .map(|(_, file)| format!("{name}/{}", file.path)),
        );
        for (item, archive, path) in records.into_inner() {
            let label = format!("{name}/{path}");
            add(&mut lists, item, label, name, Source::List(archive));
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

/// the error for what the cache lacks, `lacked`, and the archives whose files it does not know,
/// `unknown`, with what to run first: a sync for what sources pin, a build for what a recipe
/// makes. `selected` are the datasets the push named.
fn missing(lacked: &[&Needed], unknown: &[String], selected: Option<&[String]>) -> Error {
    let lines: Vec<String> = unknown
        .iter()
        .map(|archive| format!("the files inside {archive}"))
        .chain(lacked.iter().map(|needed| needed.label.clone()))
        .collect();
    let mut error = Error::new(
        Kind::OfflineMissing,
        "this machine's cache lacks files the push needs, and push never downloads",
    );
    for line in lines.iter().take(super::fetch::NAMED) {
        error = error.detail(line.clone());
    }
    if lines.len() > super::fetch::NAMED {
        error = error.detail(format!("and {} more", lines.len() - super::fetch::NAMED));
    }
    let mut help = Vec::new();
    if !unknown.is_empty()
        || lacked
            .iter()
            .any(|needed| matches!(needed.source, Source::Pinned))
    {
        let words: String = selected
            .unwrap_or_default()
            .iter()
            .flat_map(|name| [" ", name.as_str()])
            .collect();
        help.push(format!(
            "run `fetchloom sync{words}` first, then push again"
        ));
    }
    let built: BTreeSet<&str> = lacked
        .iter()
        .filter(|needed| matches!(needed.source, Source::Result))
        .map(|needed| needed.dataset.as_str())
        .collect();
    for name in built {
        help.push(format!("`fetchloom build {name}` makes {name}'s files"));
    }
    error.help(help.join("; "))
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
    let mut puts = copy_files(context, cache, &target, &files)?;
    puts.extend(copy_lists(cache, &target, &lists)?);
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

/// writes `files` from the cache, a few at once within the bound a folder mirror takes, with a
/// bar of the bytes. the first failure stops the rest.
fn copy_files(
    context: &Context,
    cache: &Cache,
    target: &Target,
    files: &[(&Needed, There)],
) -> Result<Vec<Put>, Error> {
    let total = files.iter().map(|(needed, _)| needed.item.size).sum();
    let what = units::count(files.len() as u64, "file", "files");
    let bar = context.output.bar("Pushing", what.clone());
    bar.start(Some(total), 0);
    let ticker = context.output.ticker(
        "Pushing",
        what,
        files.iter().map(|(needed, _)| Some(needed.item.size)),
    );
    let failed = AtomicBool::new(false);
    let threads = context.threads.at_most(folder::COPIES);
    let puts = crate::place::run_all(files, threads, |(needed, there)| {
        if failed.load(Ordering::Relaxed) {
            return None;
        }
        let put = read_unfollowed(&cache.object_path(&needed.item.digest))
            .map_err(Fault::Source)
            .and_then(|mut source| target.put(&needed.item, &mut source, *there));
        bar.advance(needed.item.size);
        ticker.advance(needed.item.size);
        if put.is_err() {
            failed.store(true, Ordering::Relaxed);
        }
        Some(put)
    });
    drop((bar, ticker));
    let mut written = Vec::with_capacity(files.len());
    let mut first = None;
    for ((needed, _), put) in files.iter().zip(puts) {
        match put {
            Some(Ok(put)) => written.push(put),
            Some(Err(fault)) => {
                first.get_or_insert_with(|| failure(fault, needed, target.root()));
            }
            None => {}
        }
    }
    match first {
        Some(error) => Err(error),
        None => Ok(written),
    }
}

/// writes the lists of what archives unpack to, `lists`, from the cache's records of them.
fn copy_lists(
    cache: &Cache,
    target: &Target,
    lists: &[(&Needed, There)],
) -> Result<Vec<Put>, Error> {
    let mut written = Vec::with_capacity(lists.len());
    for (needed, there) in lists {
        let Source::List(archive) = needed.source else {
            continue;
        };
        let Some(members) = cache.unpacked(&archive, Some(&needed.item.digest)) else {
            return Err(Error::new(
                Kind::OfflineMissing,
                format!(
                    "the cache no longer knows the files inside {}",
                    needed.label
                ),
            )
            .help("run `fetchloom sync` first, then push again"));
        };
        let bytes = crate::cache::tree(&members);
        let put = target
            .put(&needed.item, &mut bytes.as_bytes(), *there)
            .map_err(|fault| failure(fault, needed, target.root()))?;
        written.push(put);
    }
    Ok(written)
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
    report
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

/// the lines of a dry run: what a push would write, find there and ask about.
fn tell_dry(context: &Context, planned: &Planned, report: &Report) {
    let output = &context.output;
    let target = planned.target.display();
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
    let nothing = report.copied == 0 && report.trees == 0 && !report.started;
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

    fn asking(dataset: &str, access: Access, from: &[&str]) -> Asking {
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
        let asks = [
            asking("pets", Access::Terms, &[]),
            asking("iris", Access::Unknown, &[]),
            asking("dogs", Access::Unknown, &[]),
            asking("vault", Access::Login, &[]),
            asking("clean", Access::Terms, &["pets"]),
        ];
        assert_eq!(
            reasons(&asks),
            "vault needs an account to read; pets needs accepted terms; the sources of iris and dogs did not say who may read them; clean is made from pets"
        );
        assert_eq!(
            reasons(&[asking("iris", Access::Unknown, &[])]),
            "iris's source did not say who may read it"
        );
        let six = ["a", "b", "c", "d", "e", "f"];
        assert_eq!(named(&six), "a, b, c, d, e and f");
        let eight = ["a", "b", "c", "d", "e", "f", "g", "h"];
        assert_eq!(named(&eight), "a, b, c, d, e and 3 more");
    }
}
