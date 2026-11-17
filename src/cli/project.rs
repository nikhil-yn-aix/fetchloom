//! `sync` and `lock`, and the steps other project commands share: finding the project, taking its
//! lock and the run that brings data.lock, the cache and the data folder in line. a command takes
//! the lock only when it changes something, and leaves the order of work to the sync loop.

mod conflicts;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tracing::Instrument as _;

use super::args;
use super::sync::disk::{self, Folders, Survey};
use super::sync::place::{self, Changes, Download, Placement, Preview, Seen};
use super::sync::{self as engine, Given, Loop, Scope, Upgrade};
use super::{Context, open_cache, peek_cache};
use crate::digest;
use crate::error::{Error, Kind};
use crate::name;
use crate::output::{Output, Warning};
use crate::plan;
use crate::project::lock::merge;
use crate::project::lock::{self, Drift, Lock};
use crate::project::manifest::{self, Manifest};
use crate::project::state::Synced;
use crate::project::{Held, Hold, Project, git, native};
use crate::source::{Listing, local};
pub(crate) use crate::suggested::command;
use crate::units;

/// what a project command did, or with `--dry-run` would do, as `--json` prints it.
#[derive(Default, Serialize)]
pub(super) struct Report {
    pub(super) project: PathBuf,
    /// for `sync`, the datasets it was asked for: their names, or null for every dataset.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) selected: Option<Scope>,
    /// datasets added, removed or upgraded.
    pub(super) datasets: Vec<String>,
    pub(super) lock_changed: bool,
    pub(super) placement: Option<Placement>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(super) dry_run: bool,
    /// with `--dry-run`, the paths that would change.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) changes: Option<Changes>,
    /// how each dataset came out when `lock` merged both sides of a git merge.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) merge: Option<merge::Summary>,
    /// for `sync`, the datasets of data.lock that wait for their file list, which it warns of.
    #[serde(skip)]
    pub(super) waiting: Vec<lock::Waiting>,
}

impl Report {
    pub(super) fn dry(project: PathBuf, datasets: Vec<String>, outcome: Outcome<Preview>) -> Self {
        Report {
            project,
            selected: None,
            datasets,
            lock_changed: outcome.lock_changed,
            placement: Some(outcome.placed.placement),
            dry_run: true,
            changes: Some(outcome.placed.changes),
            merge: None,
            waiting: Vec::new(),
        }
    }
}

/// ends a run that wrote lists into data.lock.d that git lacks, which `git commit -a` leaves out
/// and a clone then misses; without a terminal it warns, so scripts and `--json` read it too.
pub(super) fn remind_lists(context: &Context) {
    let written = std::mem::take(
        &mut *context
            .lists
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    );
    let Some((root, _)) = written.first() else {
        return;
    };
    let missing = untracked(root, written.iter().map(|(_, part)| part.as_str()));
    let named = match missing.as_slice() {
        [] => return,
        [one] => one.clone(),
        [first, ..] => format!(
            "{} lists data.lock names now, such as {first}",
            units::grouped(missing.len() as u64)
        ),
    };
    let output = &context.output;
    let since = if missing.len() == 1 {
        ", which data.lock names now"
    } else {
        ""
    };
    let add = crate::suggested::git(root, "add data.lock.d");
    if output.shows_next() {
        output.next(&format!(
            "run `{add}` before you commit: git does not have {named}{since}"
        ));
    } else {
        output.warn_detailed(
            Warning::GitLockPartUntracked,
            &format!("git does not have {named}{since}"),
            Some(&format!(
                "run `{add}` before you commit, since `git commit -a` leaves new files out"
            )),
            &missing,
        );
    }
}

/// the parts of `parts`, lists below the project at `root`, that are there and that git does not
/// have, sorted. none when git cannot tell, as with no git or outside a repository.
pub(super) fn untracked<'a>(root: &Path, parts: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut parts: Vec<&str> = parts.filter(|part| native(root, part).is_file()).collect();
    if parts.is_empty() {
        return Vec::new();
    }
    let Some(tracked) = std::env::var_os("PATH")
        .and_then(|path| git::find(Some(&path), root))
        .and_then(|program| git::tracked(&program, root, lock::lists::FOLDER))
    else {
        return Vec::new();
    };
    parts.sort_unstable();
    parts.dedup();
    parts
        .into_iter()
        .filter(|part| !tracked.contains(*part))
        .map(str::to_owned)
        .collect()
}

/// the project files a person commits: data.lock.d too, if any, since a clone without it cannot
/// read data.lock, or once a run removed its last list git has, so the commit takes that away too.
pub(super) fn committed(project: &Project) -> &'static str {
    if names_lists(project) {
        "data.toml, data.lock and data.lock.d"
    } else {
        "data.toml and data.lock"
    }
}

/// whether what to commit names data.lock.d: it is there, or git has lists in it that a run
/// swept with the folder, whose deletion is committed too.
pub(super) fn names_lists(project: &Project) -> bool {
    project.root.join(lock::lists::FOLDER).is_dir()
        || (Project::repository(&project.root).is_some() && lists_gone(&project.root))
}

/// whether git has lists in data.lock.d, which is gone from the project at `root`.
fn lists_gone(root: &Path) -> bool {
    std::env::var_os("PATH")
        .and_then(|path| git::find(Some(&path), root))
        .and_then(|program| git::tracked(&program, root, lock::lists::FOLDER))
        .is_some_and(|tracked| !tracked.is_empty())
}

/// `sync`, failing with `--strict` when the data asked for is not ready once it is done.
pub(super) async fn sync(context: &Context, args: args::Sync) -> Result<Report, Error> {
    let strict = args.strict.then(|| args.names.clone());
    let (report, closing) = sync_as_asked(context, args).await?;
    tell_waiting(
        &context.output,
        &report.waiting,
        report.selected.as_ref().unwrap_or(&Scope::All),
    );
    let ready = match (strict, &report.placement) {
        (Some(names), Some(placement)) => {
            let scope = if names.is_empty() {
                Scope::All
            } else {
                Scope::Only(names.into_iter().collect())
            };
            let unready = super::status::ready::after_sync(&scope, placement, &report.waiting);
            super::status::ready::ready(&unready)
        }
        _ => Ok(()),
    };
    if let Some(closing) = closing {
        closing.tell(&context.output, ready.is_ok());
    }
    ready.map_err(|error| error.with_report(&report))?;
    Ok(report)
}

/// the sync `args` asks for, and its last lines, which a dry run tells itself.
async fn sync_as_asked(
    context: &Context,
    args: args::Sync,
) -> Result<(Report, Option<Closing>), Error> {
    let project = find(context)?;
    let dry_run = args.dry.dry_run;
    // a dry run changes nothing, so like `lock --check` it needs no hold
    let _held = if dry_run {
        None
    } else {
        match hold(context, &project, "sync").await {
            Ok(held) => Some(held),
            // a checkout this user may not write, as a job's read-only mount, still answers a
            // sync that would write nothing
            Err(error) if error.kind == Kind::LocalIo => {
                return match settled_unheld(context, &project, &args)? {
                    Some((report, closing)) => Ok((report, Some(closing))),
                    None => Err(error),
                };
            }
            Err(error) => return Err(error),
        }
    };
    sync_held(context, project, args).await
}

/// [`sync_as_asked`] in `project`, held unless it is a dry run.
async fn sync_held(
    context: &Context,
    project: Project,
    args: args::Sync,
) -> Result<(Report, Option<Closing>), Error> {
    let dry_run = args.dry.dry_run;
    let text = project.manifest_text()?;
    if let Some((plain, read)) = settled_plain(&project, &text, &args) {
        return sync_plain(context, &project, &args, &text, (&plain, read))
            .await
            .map(|(report, closing)| (report, Some(closing)));
    }
    let manifest = Project::parse_manifest(&text)?;
    let scope = selected(&manifest, &args.names, "sync")?;
    let dropped = project.dropped();
    super::drop::tell_left_out(&context.output, &manifest, &scope, &dropped);
    let manifest = super::drop::placed_by(&manifest, &scope, &dropped).into_owned();
    // a named dataset dropped here comes back, which the last sync's word that all was in place
    // left out
    let back = super::drop::names_any(&scope, &dropped);
    if !dry_run
        && !args.locked
        && !args.verify
        && !back
        && let Some((report, closing)) = settled_named(context, &project, &manifest, &text, &scope)?
    {
        return Ok((report, Some(closing)));
    }
    let read = project.lock_and_text()?;
    if args.locked {
        locked(
            &project,
            &manifest,
            read.as_ref().map(|(lock, _)| lock),
            "sync without --locked",
            &scope,
        )?;
    }
    let seen = Seen {
        manifest: digest::sha256(text.as_bytes()),
        lock: read
            .as_ref()
            .map(|(_, text)| digest::sha256(text.as_bytes())),
    };
    let waiting = read
        .as_ref()
        .map(|(lock, _)| waiting_in(lock))
        .unwrap_or_default();
    if dry_run {
        let seen = (!back).then_some(&seen);
        return preview_sync(context, &project, &manifest, read, seen, &args, scope)
            .await
            .map(|report| (Report { waiting, ..report }, None));
    }
    let old = read.map(|(lock, _)| lock);
    let work = Loop::new(context, &project, Arc::new(context.net()), open_cache()?)
        .within(scope)
        .filling()
        .verifying(args.verify);
    let run = Run {
        upgrade: Upgrade::Nothing,
        force: args.force,
        verify: args.verify,
        read: Read::Settled(&seen),
    };
    let Outcome {
        lock_changed,
        pins,
        placed: placement,
    } = waited(
        &context.output,
        &waiting,
        &work.scope,
        sync_all(&work, &manifest, old, None, run).await,
    )?;
    let closing = closing(
        &project,
        &manifest,
        &work.scope,
        lock_changed,
        &placement,
        args.verify,
        &waiting,
    );
    let report = Report {
        selected: Some(work.scope.clone()),
        project: project.root,
        datasets: pins,
        lock_changed,
        placement: Some(placement),
        waiting,
        ..Report::default()
    };
    Ok((report, Some(closing)))
}

/// the datasets of `lock` that wait for their file list.
fn waiting_in(lock: &Lock) -> Vec<lock::Waiting> {
    lock.still_waiting().cloned().collect()
}

/// `result`, having warned of `waiting` in `scope` first when it failed, as the report of a run
/// that ends well does.
fn waited<T>(
    output: &Output,
    waiting: &[lock::Waiting],
    scope: &Scope,
    result: Result<T, Error>,
) -> Result<T, Error> {
    if result.is_err() {
        tell_waiting(output, waiting, scope);
    }
    result
}

/// data.toml's `text` as [`manifest::read_plain`] reads it, with its SHA-256, for a plain sync of
/// every dataset when the last complete sync read the same text; none, so it is parsed, otherwise.
fn settled_plain(
    project: &Project,
    text: &str,
    args: &args::Sync,
) -> Option<(Manifest, digest::Sha256)> {
    if args.dry.dry_run || args.locked || args.verify || !args.names.is_empty() {
        return None;
    }
    let synced = project.synced().filter(|synced| synced.complete)?;
    let read = digest::sha256(text.as_bytes());
    if synced.manifest != read {
        return None;
    }
    manifest::read_plain(text).map(|plain| (plain, read))
}

/// a sync of every dataset from `plain`, data.toml's `text` as [`settled_plain`] read it with its
/// SHA-256: settled from it alone, else run from the text parsed whole and the survey made.
async fn sync_plain(
    context: &Context,
    project: &Project,
    args: &args::Sync,
    text: &str,
    (plain, read): (&Manifest, digest::Sha256),
) -> Result<(Report, Closing), Error> {
    let lock = project.lock_and_text()?;
    let seen = Seen {
        manifest: read,
        lock: lock
            .as_ref()
            .map(|(_, text)| digest::sha256(text.as_bytes())),
    };
    let old = lock.map(|(lock, _)| lock);
    let waiting = old.as_ref().map(waiting_in).unwrap_or_default();
    let work = Loop::new(context, project, Arc::new(context.net()), open_cache()?).filling();
    let mut survey = surveyed(&work, &Folders::of(plain), false)?;
    let dropped = survey.state.dropped.clone();
    super::drop::tell_left_out(&context.output, plain, &Scope::All, &dropped);
    let plain = &*super::drop::placed_by(plain, &Scope::All, &dropped);
    let (whole, outcome) = if let Some(outcome) = settle(&work, plain, &mut survey, &seen)? {
        (None, outcome)
    } else {
        let whole = Project::parse_manifest(text)?;
        let whole = super::drop::placed_by(&whole, &Scope::All, &dropped).into_owned();
        let run = Run {
            upgrade: Upgrade::Nothing,
            force: args.force,
            verify: false,
            read: Read::Settled(&seen),
        };
        let outcome = sync_surveyed(&work, &whole, old, None, run, survey).await;
        (
            Some(whole),
            waited(&context.output, &waiting, &work.scope, outcome)?,
        )
    };
    let manifest = whole.as_ref().unwrap_or(plain);
    let Outcome {
        lock_changed,
        pins,
        placed: placement,
    } = outcome;
    let closing = closing(
        project,
        manifest,
        &work.scope,
        lock_changed,
        &placement,
        false,
        &waiting,
    );
    let report = Report {
        selected: Some(work.scope.clone()),
        project: project.root.clone(),
        datasets: pins,
        lock_changed,
        placement: Some(placement),
        waiting,
        ..Report::default()
    };
    Ok((report, closing))
}

/// the datasets `names` asks `command` for, every one when it names none.
pub(super) fn selected(
    manifest: &Manifest,
    names: &[String],
    command: &str,
) -> Result<Scope, Error> {
    if names.is_empty() {
        return Ok(Scope::All);
    }
    for name in names {
        known(manifest, name, command)?;
    }
    Ok(Scope::Only(names.iter().cloned().collect()))
}

/// a sync of named datasets that would write nothing, costing only what they hold. none when it
/// cannot tell, as when the state must be written again, which needs every record.
fn settled_named(
    context: &Context,
    project: &Project,
    manifest: &Manifest,
    text: &str,
    scope: &Scope,
) -> Result<Option<(Report, Closing)>, Error> {
    let Scope::Only(names) = scope else {
        return Ok(None);
    };
    let Some((lock, lock_text)) = project.lock_only(&|name| names.contains(name))? else {
        return Ok(None);
    };
    let seen = Seen {
        manifest: digest::sha256(text.as_bytes()),
        lock: Some(digest::sha256(lock_text.as_bytes())),
    };
    // a large lock's text would otherwise stay in memory through the survey
    drop(lock_text);
    let work =
        Loop::new(context, project, Arc::new(context.net()), open_cache()?).within(scope.clone());
    refuse_named_waiting(Some(&lock), scope)?;
    let folders: Vec<String> = names
        .iter()
        .map(|name| plan::folder(manifest, name))
        .collect();
    let reading = context.output.setup("Reading", "the data folder");
    let mut survey = Survey::read_within(
        project,
        &Folders::of(manifest),
        &folders,
        context.threads,
        &context.stop,
    )?;
    drop(reading);
    survey.check_doubted(&project.root, &work.cache)?;
    disk::audit(project, &survey, &work.cache)?;
    let Some(placement) = place::untouched(project, manifest, &survey, &seen, true, scope)?
        .filter(|_| !survey.needs_writing)
    else {
        return Ok(None);
    };
    // a sync of named datasets reads only their records, so it notes nothing in the state
    if place::links_copies(project, manifest, &mut survey, &work.cache, scope, false) {
        return Ok(None);
    }
    context
        .stop
        .check()
        .map_err(|error| work.before_placing(error))?;
    context.stop.finish();
    register(&work);
    let waiting = waiting_in(&lock);
    let closing = closing(project, manifest, scope, false, &placement, false, &waiting);
    let report = Report {
        selected: Some(scope.clone()),
        project: project.root.clone(),
        datasets: Vec::new(),
        lock_changed: false,
        placement: Some(placement),
        waiting,
        ..Report::default()
    };
    Ok(Some((report, closing)))
}

/// a sync that would write nothing, found without the project's hold: both files as the last
/// complete sync left them, every file as placed and the ignore rule in place. none otherwise.
fn settled_unheld(
    context: &Context,
    project: &Project,
    args: &args::Sync,
) -> Result<Option<(Report, Closing)>, Error> {
    let (text, manifest) = project.manifest()?;
    let scope = selected(&manifest, &args.names, "sync")?;
    let Some((lock, lock_text)) = project.lock_and_text()? else {
        return Ok(None);
    };
    if args.locked {
        locked(
            project,
            &manifest,
            Some(&lock),
            "sync without --locked",
            &scope,
        )?;
    }
    let seen = Seen {
        manifest: digest::sha256(text.as_bytes()),
        lock: Some(digest::sha256(lock_text.as_bytes())),
    };
    // the cache, unlike the project, may well be writable, and a project listed there needs no
    // write. a cache this user cannot hold either is only read, as a dry run reads it
    let (cache, held) = match open_cache() {
        Ok(cache) => (cache, true),
        Err(_) => (peek_cache()?, false),
    };
    let reading = context.output.setup("Reading", "the data folder");
    let mut survey = Survey::read(
        project,
        &Folders::of(&manifest),
        context.threads,
        &context.stop,
    )?;
    drop(reading);
    // hashing proved files placed too recently to trust by time unchanged, and only asks for the
    // state to be written again, which this project cannot take. an edit it found stays an edit
    survey.needs_writing = false;
    survey.check_doubted(&project.root, &cache)?;
    if args.verify {
        survey.verify(&project.root, &scope, Some(&context.output))?;
    }
    let Some(placement) = place::untouched(project, &manifest, &survey, &seen, false, &scope)?
    else {
        return Ok(None);
    };
    if held && let Err(err) = cache.register(&project.root) {
        tracing::debug!(%err, "could not list a read-only project in the cache");
    }
    // a settled sync follows a complete one, which no dataset waiting for its list allows
    let closing = closing(
        project,
        &manifest,
        &scope,
        false,
        &placement,
        args.verify,
        &[],
    );
    let report = Report {
        project: project.root.clone(),
        selected: Some(scope),
        datasets: Vec::new(),
        lock_changed: false,
        placement: Some(placement),
        waiting: waiting_in(&lock),
        ..Report::default()
    };
    Ok(Some((report, closing)))
}

/// the last lines of a sync: what it found or did, and what to do next.
struct Closing {
    verb: &'static str,
    text: String,
    next: Option<String>,
}

impl Closing {
    /// a closing `Ok` says nothing is amiss, so a strict sync whose data is not `ready` leaves it
    /// untold, and its error says what is not ready instead.
    fn tell(self, output: &Output, ready: bool) {
        if self.verb == "Ok" && !ready {
            return;
        }
        output.done(self.verb, &self.text);
        if let Some(next) = self.next {
            output.next(&next);
        }
    }
}

fn closing(
    project: &Project,
    manifest: &Manifest,
    scope: &Scope,
    lock_changed: bool,
    placement: &Placement,
    verified: bool,
    waiting: &[lock::Waiting],
) -> Closing {
    let waiting = waiting_named(waiting, manifest, scope);
    if !lock_changed
        && placement.placed + placement.replaced + placement.removed == 0
        && placement.kept.is_empty()
        && placement.changed_outputs.is_empty()
        && placement.adopted.is_empty()
        && placement.relinked.is_empty()
    {
        let found = in_place(manifest, scope, verified, &waiting);
        let text = if verified {
            format!("{found}; nothing to do")
        } else {
            format!("{found}, nothing to do")
        };
        return Closing {
            verb: "Ok",
            text,
            next: None,
        };
    }
    let warned = placement.kept.iter().any(|kept| !kept.told)
        || placement
            .changed_outputs
            .iter()
            .any(|changed| !changed.told);
    let left: Vec<&str> = placement
        .changed_outputs
        .iter()
        .map(|changed| changed.name.as_str())
        .collect();
    let kept = (placement.kept.len(), left.as_slice(), warned);
    let (verb, text) = synced(manifest, scope, kept, &waiting);
    // `path` answers only a dataset placed in the data folder, and the line names one this
    // sync just placed
    let first = placement.filled.iter().find(|name| {
        scope.includes(name)
            && manifest
                .datasets
                .get(*name)
                .is_some_and(|entry| entry.place)
    });
    let next = if lock_changed {
        Some(format!(
            "commit {}, so other clones use these pins",
            committed(project)
        ))
    } else {
        first.map(|name| {
            format!(
                "`{}` tells your scripts where {name} is",
                command(&format!("path {name}"))
            )
        })
    };
    Closing { verb, text, next }
}

/// the datasets a sync of every dataset places that wait for their file lists, whose files, any
/// placed before included, it leaves as they are, as their warning says; none for a sync of named
/// datasets.
fn waiting_named(waiting: &[lock::Waiting], manifest: &Manifest, scope: &Scope) -> Vec<String> {
    if *scope != Scope::All {
        return Vec::new();
    }
    waiting
        .iter()
        .map(|waiting| waiting.name.clone())
        .filter(|name| manifest.datasets.get(name).is_some_and(|entry| entry.place))
        .collect()
}

/// the last line of a sync of `scope` that changed something, as its verb and text, with the
/// files it `kept`, the outputs it `left` whole as they changed since they were pinned, and
/// whether a warning above tells of any. the datasets `waiting` for their file lists were left as
/// they were, so they are named apart.
fn synced(
    manifest: &Manifest,
    scope: &Scope,
    (kept, left, warned): (usize, &[&str], bool),
    waiting: &[String],
) -> (&'static str, String) {
    let mut unchanged = Vec::new();
    if kept > 0 {
        let files = ("file left unchanged", "files left unchanged");
        unchanged.push(units::count(kept as u64, files.0, files.1));
    }
    match left {
        [] => {}
        [one] => unchanged.push(format!("{one} left as it is")),
        many => unchanged.push(format!("{} left as they are", units::listed(many, "and"))),
    }
    let see = if warned {
        "; see the warnings above"
    } else {
        ""
    };
    let kept = if unchanged.is_empty() {
        String::new()
    } else {
        format!(", {}{see}", unchanged.join(", "))
    };
    let waits = match waiting {
        [] => String::new(),
        [one] => format!(" · {one} waits for its file list"),
        many => format!(
            " · {} wait for their file lists",
            units::listed(many, "and")
        ),
    };
    let (datasets, others) = match scope {
        Scope::All => {
            let placed = manifest
                .datasets
                .iter()
                .filter(|(name, entry)| entry.place && !waiting.contains(*name))
                .count();
            if placed == 0 {
                let data = &manifest.data_dir;
                return ("Ok", format!("nothing to place in {data}{waits}{kept}"));
            }
            (units::count(placed as u64, "dataset", "datasets"), 0)
        }
        Scope::Only(names) => (
            named(names),
            manifest
                .datasets
                .keys()
                .filter(|name| !names.contains(*name))
                .count(),
        ),
    };
    let mut said = format!("{datasets} into {}", manifest.data_dir);
    if others > 0 {
        let others = units::count(
            others as u64,
            "other dataset left as it was",
            "other datasets left as they were",
        );
        said = format!("{said} · {others}");
    }
    ("Synced", format!("{said}{waits}{kept}"))
}

/// what a sync of `scope` with nothing to do found in place, leaving out the files of the datasets
/// `waiting` for their file lists, which are not.
fn in_place(manifest: &Manifest, scope: &Scope, verified: bool, waiting: &[String]) -> String {
    let placed = |name: &String| manifest.datasets.get(name).is_some_and(|entry| entry.place);
    let files = match scope {
        Scope::All if !manifest.datasets.keys().any(placed) => {
            return format!("nothing to place in {}", manifest.data_dir);
        }
        Scope::All => match waiting {
            [] => "all pinned files".to_owned(),
            [one] => format!("all pinned files but {one}'s"),
            many => format!(
                "all pinned files but those of {}",
                units::listed(many, "and")
            ),
        },
        Scope::Only(names) => format!("the pinned files of {}", named(names)),
    };
    if verified {
        format!("{files} are in place, checked byte for byte")
    } else {
        format!("{files} are in place by size and time")
    }
}

fn named(names: &BTreeSet<String>) -> String {
    match names.iter().next() {
        Some(only) if names.len() == 1 => only.clone(),
        _ => units::count(names.len() as u64, "dataset", "datasets"),
    }
}

/// `sync --dry-run`, given data.lock as `read` and both files as `seen`.
async fn preview_sync(
    context: &Context,
    project: &Project,
    manifest: &Manifest,
    read: Option<(Lock, String)>,
    seen: Option<&Seen>,
    args: &args::Sync,
    scope: Scope,
) -> Result<Report, Error> {
    let old = read.map(|(lock, _)| lock);
    let waits = old.as_ref().map(waiting_in).unwrap_or_default();
    let work = Loop::new(context, project, Arc::new(context.net()), peek_cache()?)
        .within(scope)
        .filling();
    let outcome = preview_all(
        &work,
        manifest,
        old,
        args.force,
        args.verify,
        Vec::new(),
        seen,
    )
    .await?;
    let nothing = !outcome.lock_changed && outcome.placed.changes_nothing();
    let waiting = waiting_named(&waits, manifest, &work.scope);
    dry_run_done(
        &context.output,
        nothing
            .then(|| in_place(manifest, &work.scope, args.verify, &waiting))
            .as_deref(),
    );
    let pins = outcome.pins.clone();
    Ok(Report {
        selected: Some(work.scope.clone()),
        ..Report::dry(project.root.clone(), pins, outcome)
    })
}

pub(super) async fn lock(context: &Context, args: args::Lock) -> Result<Report, Error> {
    let project = find(context)?;
    // a check or a dry run changes nothing, so it needs no hold. anything else reads what it
    // changes only once no other fetchloom can change it
    let _held = if args.check || args.dry.dry_run {
        None
    } else {
        Some(hold(context, &project, "lock").await?)
    };
    // a check or an upgrade reads both files as they are, and so says to merge them first
    if !args.check
        && args.upgrade.is_none()
        && let Some(report) = conflicts::merge(context, &project, &args)?
    {
        return Ok(report);
    }
    let (_, manifest) = project.manifest()?;
    let old = project.lock()?;
    if args.check {
        let report = check(&project, &manifest, old.as_ref(), &Scope::All)?;
        context.output.done("Ok", "data.lock matches data.toml");
        return Ok(report);
    }
    let upgrade = match args.upgrade {
        None => Upgrade::Nothing,
        Some(names) if names.is_empty() => {
            tell_imports_kept(&context.output, &manifest);
            Upgrade::All
        }
        Some(names) => {
            for name in &names {
                known(&manifest, name, "lock --upgrade")?;
                if manifest.datasets[name].is_import() {
                    return Err(local::no_newer(name));
                }
            }
            // a dataset waiting for its list is never resolved
            if let Some(old) = &old {
                old.refuse_waiting(names.iter().map(String::as_str))?;
            }
            Upgrade::Only(names.into_iter().collect::<BTreeSet<_>>())
        }
    };
    if args.dry.dry_run {
        return preview_lock(context, &project, &manifest, old, &upgrade).await;
    }
    let work = Loop::new(context, &project, Arc::new(context.net()), open_cache()?);
    let (lock, before) = work
        .relock(&manifest, old, &upgrade, Vec::new())
        .await
        .map_err(|error| without_private_lfs(error, &manifest, &upgrade))?;
    // a stop asked while the datasets were resolved leaves data.lock as it was
    context.stop.check()?;
    context.stop.finish();
    tell_waiting(&context.output, &waiting_in(&lock), &Scope::All);
    let changed = work.write_lock(&before, &lock)?.is_some();
    work.drop_unpacked(&lock);
    engine::tell_upgrades(
        &context.output,
        &before,
        &lock,
        false,
        &std::collections::HashSet::new(),
    );
    work.tell_versions_kept(&manifest, &before, &lock, &upgrade);
    let pins = engine::repinned(&before, &lock);
    if changed {
        context.output.done("Locked", "data.toml into data.lock");
        context.output.next(&format!(
            "run `{}` to update {}/, then commit {}",
            command("sync"),
            manifest.data_dir,
            committed(&project)
        ));
    } else {
        // an upgrade asks no source of a dataset waiting for its list
        let unasked: Vec<&str> = lock
            .still_waiting()
            .map(|waiting| waiting.name.as_str())
            .filter(|name| upgrade.includes(name))
            .collect();
        let text = match unasked.as_slice() {
            [] => "data.lock is already up to date, nothing to do".to_owned(),
            [one] => format!(
                "data.lock is already up to date, but {one} waits for its file list and was not asked"
            ),
            many => format!(
                "data.lock is already up to date, but {} wait for their file lists and were not asked",
                units::listed(many, "and")
            ),
        };
        context.output.done("Ok", &text);
    }
    Ok(Report {
        project: project.root,
        datasets: pins,
        lock_changed: changed,
        placement: None,
        ..Report::default()
    })
}

/// `error` from relocking, whose downloads of new files git-lfs keeps for a private repository
/// failed: its help names the patterns that leave them out of their dataset in data.toml and the
/// command to run again, as an add names its `--exclude`.
fn without_private_lfs(mut error: Error, manifest: &Manifest, upgrade: &Upgrade) -> Error {
    let refused = |help: &str| help.starts_with(crate::source::PRIVATE_LFS);
    let files: Vec<(&str, &str)> = error
        .failures
        .iter()
        .filter(|failure| failure.help.as_deref().is_some_and(refused))
        .filter_map(|failure| {
            manifest.datasets.keys().find_map(|name| {
                let path = failure
                    .file
                    .strip_prefix(name.as_str())?
                    .strip_prefix('/')?;
                Some((name.as_str(), path))
            })
        })
        .collect();
    let Some(&(name, _)) = files.first() else {
        return error;
    };
    let paths: Vec<&str> = files
        .iter()
        .filter(|(dataset, _)| *dataset == name)
        .map(|(_, path)| *path)
        .collect();
    let patterns: Vec<String> = paths
        .iter()
        .map(|path| format!("\"{}\"", crate::glob::escape(path)))
        .collect();
    let (verb, again) = match upgrade {
        Upgrade::Nothing => ("pin", "lock".to_owned()),
        _ => ("upgrade", format!("lock --upgrade {name}")),
    };
    let help = format!(
        "git-lfs keeps {}, and {}. to {verb} {name} without {}, add {} to exclude in datasets.{name} in data.toml, then run `{}` again",
        units::some_of(&paths, 5),
        crate::source::PRIVATE_LFS,
        if paths.len() == 1 { "it" } else { "them" },
        units::listed(&patterns, "and"),
        command(&again)
    );
    let prefix = format!("{name}/");
    for failure in &mut error.failures {
        if failure.file.starts_with(&prefix) && failure.help.as_deref().is_some_and(refused) {
            failure.help = Some(help.clone());
        }
    }
    for detail in &mut error.details {
        if detail.strip_prefix("help: ").is_some_and(refused) {
            *detail = format!("help: {help}");
        }
    }
    if error.help.as_deref().is_some_and(refused) {
        error = error.help(help);
    }
    error
}

fn tell_imports_kept(output: &Output, manifest: &Manifest) {
    let imported: Vec<&str> = manifest
        .datasets
        .iter()
        .filter(|(_, entry)| entry.is_import())
        .map(|(name, _)| name.as_str())
        .collect();
    if imported.is_empty() {
        return;
    }
    let names = units::listed(&imported, "and");
    output.step(
        "Skipped",
        &format!("{names}, imported from files, which no source has a newer version of"),
    );
}

/// `lock --dry-run`, which never downloads, unpacks or writes. a file no checksum pins keeps its
/// old pin and is listed as a download, since only its bytes can tell whether it changed.
async fn preview_lock(
    context: &Context,
    project: &Project,
    manifest: &Manifest,
    old: Option<Lock>,
    upgrade: &Upgrade,
) -> Result<Report, Error> {
    let work = Loop::new(context, project, Arc::new(context.net()), peek_cache()?);
    let dry = work.relock_dry(manifest, old, upgrade, Vec::new()).await?;
    let output = &context.output;
    tell_waiting(output, &waiting_in(&dry.lock), &Scope::All);
    let changed = !dry.before.holds(&dry.lock);
    if changed {
        engine::tell_lock(
            output,
            &dry.before,
            &dry.lock,
            engine::Verbs::WOULD,
            None,
            &engine::sizeless(&dry.jobs),
        );
    }
    engine::tell_upgrades(output, &dry.before, &dry.lock, true, &dry.guessed);
    let download: Vec<Download> = dry
        .jobs
        .iter()
        .map(|job| Download {
            path: super::fetch::foreseen(job),
            size: job.fetch.expect.size,
        })
        .collect();
    let to_download = super::fetch::planned(
        context,
        dry.jobs
            .iter()
            .map(|job| (super::fetch::host_of(job), super::fetch::size_of(job))),
    );
    super::fetch::tell_would_fetch(output, &download, &to_download);
    for archive in &dry.unpack {
        output.step("Would open", archive);
    }
    if !matches!(upgrade, Upgrade::Nothing) && dry.jobs.iter().any(|job| job.fetch.revalidate) {
        output.hint("whether a file no checksum pins has changed shows only once it is downloaded");
    }
    let nothing = !changed && download.is_empty() && dry.unpack.is_empty();
    dry_run_done(output, nothing.then_some("data.lock is already up to date"));
    Ok(Report {
        project: project.root.clone(),
        selected: None,
        datasets: engine::repinned(&dry.before, &dry.lock),
        lock_changed: changed,
        placement: None,
        dry_run: true,
        changes: Some(Changes {
            download,
            unpack: dry.unpack,
            to_download: Some(to_download),
            ..Changes::default()
        }),
        merge: None,
        waiting: Vec::new(),
    })
}

/// the help for a project without data.lock, such as a clone whose owner never committed it. no
/// source lists an import's files, so `lock` pins the rest only once each import of `manifest` is
/// imported again.
pub(super) fn unpinned(manifest: &Manifest) -> String {
    let imports: Vec<&str> = manifest
        .datasets
        .iter()
        .filter(|(_, entry)| entry.is_import())
        .map(|(name, _)| name.as_str())
        .collect();
    let lock = format!(
        "run `{}` to pin what the sources serve today",
        command("lock")
    );
    let again = match imports.as_slice() {
        [] => return format!("ask whoever shares the project to commit data.lock, or {lock}"),
        [one] => format!(
            "import {one}'s files again with `{}` where they are",
            command(&format!("add <folder> --as {one}"))
        ),
        _ => format!(
            "import each one's files again with `{}` where they are",
            command("add <folder> --as <name>")
        ),
    };
    if imports.len() < manifest.datasets.len() {
        format!("ask whoever shares the project to commit data.lock, or {again}, then {lock}")
    } else {
        format!("ask whoever shares the project to commit data.lock, or {again}")
    }
}

/// what `--locked` asks: data.lock is there and up to date with data.toml for `scope`. `instead`
/// is the help's other way out, such as "sync without --locked".
pub(super) fn locked(
    project: &Project,
    manifest: &Manifest,
    lock: Option<&Lock>,
    instead: &str,
    scope: &Scope,
) -> Result<(), Error> {
    let Some(lock) = lock else {
        return Err(Error::new(
            Kind::Outdated,
            "there is no data.lock, so nothing pins the files' bytes yet",
        )
        .help(unpinned(manifest)));
    };
    let help = format!(
        "run `{}` and commit data.lock, or {instead}",
        command("lock")
    );
    check(project, manifest, Some(lock), scope)
        .map(drop)
        .map_err(|error| match error.kind {
            Kind::Outdated => error.help(help),
            _ => error,
        })?;
    // filling in what only a source's digests pin changes data.lock, which --locked forbids
    let stated: Vec<&lock::Stated> = lock
        .stated
        .iter()
        .filter(|stated| scope.includes(stated.name()))
        .collect();
    match lock::stated_error(&stated) {
        Some(error) => Err(Error {
            kind: Kind::Outdated,
            message: format!("data.lock would change: {}", error.message),
            ..error
        }),
        None => Ok(()),
    }
}

/// `lock --check`, without network or writing.
fn check(
    project: &Project,
    manifest: &Manifest,
    old: Option<&Lock>,
    scope: &Scope,
) -> Result<Report, Error> {
    let empty = Lock::default();
    let old = old.unwrap_or(&empty);
    let mut stale: Vec<String> = Vec::new();
    for (name, entry) in &manifest.datasets {
        if !scope.includes(name) {
            continue;
        }
        // a pin by its source's digests counts as locked until data.toml changes, and so does
        // one waiting for its list, whose missing list is told of below
        let stated = old.stated(name).map(|stated| &stated.pin);
        let waiting = old.waits(name).map(lock::Waiting::pin);
        match old
            .get(name)
            .or(stated)
            .or(waiting)
            .map(|locked| locked.drift(entry))
        {
            None => stale.push(format!("{name} is not in data.lock yet")),
            Some(Some(Drift::Spec)) => stale.push(format!("{name} changed in data.toml")),
            Some(Some(Drift::Extract)) if stated.is_some() => {
                stale.push(format!("{name} changed in data.toml"));
            }
            Some(Some(Drift::Extract)) => stale.push(format!(
                "{name} changed extract in data.toml; `{}` needs no network for it while its archives are cached",
                super::project::command("lock")
            )),
            Some(Some(Drift::At)) => stale.push(format!(
                "{name} changed at in data.toml; `{}` needs no network for it",
                super::project::command("lock")
            )),
            Some(None) => {}
        }
    }
    let names = old.datasets.iter().map(|dataset| &dataset.name);
    let stated = old.stated.iter().map(|stated| &stated.pin.name);
    let waiting = old.still_waiting().map(|waiting| &waiting.name);
    for name in names.chain(stated).chain(waiting) {
        if scope.includes(name) && !manifest.datasets.contains_key(name) {
            stale.push(format!("{name} is in data.lock but no longer in data.toml"));
        }
    }
    if !stale.is_empty() {
        let mut error = Error::new(Kind::Outdated, "data.lock is out of date with data.toml");
        for line in stale {
            error = error.detail(line);
        }
        return Err(error.help(format!("run `{}` and commit data.lock", command("lock"))));
    }
    // its pin stands, but nothing here can place or check it until its list is back
    if let Some(waiting) = old
        .still_waiting()
        .find(|waiting| scope.includes(&waiting.name))
    {
        return Err(waiting.error());
    }
    if let Some(error) = mispinned(project, manifest, old, scope) {
        return Err(error);
    }
    Ok(Report {
        project: project.root.clone(),
        ..Report::default()
    })
}

/// a pin in `scope` whose size is not that of its SHA-256's bytes, as a placed file that still
/// hashes to it shows; only a file the state records at another size is read.
fn mispinned(project: &Project, manifest: &Manifest, lock: &Lock, scope: &Scope) -> Option<Error> {
    let (state, _) = project.state();
    if state.files.is_empty() {
        return None;
    }
    for locked in lock
        .datasets
        .iter()
        .filter(|locked| scope.includes(&locked.name))
    {
        let folder = plan::folder(manifest, &locked.name);
        for file in locked.files.iter().filter(|file| file.unpacks_to.is_none()) {
            let path = format!("{folder}/{}", file.path);
            let Some(placed) = state.files.get(&path) else {
                continue;
            };
            if let Some(actual) = place::placed_other_size(
                &native(&project.root, &path),
                placed,
                &file.sha256,
                file.size,
            ) {
                let label = format!("{}/{}", locked.name, file.path);
                return Some(place::mispinned(&label, file.size, actual));
            }
        }
    }
    None
}

/// data.toml and data.lock as a run read them, which the state records as the run leaves them.
#[derive(Clone, Copy)]
pub(super) enum Read<'a> {
    /// a data folder still as a complete sync left them needs no plan, as `sync` finds.
    Settled(&'a Seen),
    /// the run plans even so, as `add` and `path` must, since they answer from what planning
    /// learns, such as what an archive holds.
    Planned(&'a Seen),
}

/// what a run did, or with `--dry-run` would do, to data.lock and the data folder.
#[derive(Debug)]
pub(super) struct Outcome<T> {
    pub(super) lock_changed: bool,
    /// the datasets newly pinned, pinned to other data or files, or no longer pinned.
    pub(super) pins: Vec<String>,
    pub(super) placed: T,
}

/// the choices a command makes for one [`sync_all`].
pub(super) struct Run<'a> {
    /// which datasets resolve their newest version.
    pub(super) upgrade: Upgrade,
    /// whether edited files and files fetchloom did not place are replaced.
    pub(super) force: bool,
    /// whether every placed file is hashed, so changes that kept their size and time show.
    pub(super) verify: bool,
    pub(super) read: Read<'a>,
}

impl<'a> Run<'a> {
    pub(super) fn planned(seen: &'a Seen) -> Self {
        Run {
            upgrade: Upgrade::Nothing,
            force: false,
            verify: false,
            read: Read::Planned(seen),
        }
    }
}

/// brings the lock, the cache and the data folder in line, in that order, leaving the state to
/// record both files as read and as left. `given` is a listing already resolved for one dataset.
pub(super) async fn sync_all(
    work: &Loop<'_>,
    manifest: &Manifest,
    old: Option<Lock>,
    given: Option<(String, Listing)>,
    run: Run<'_>,
) -> Result<Outcome<Placement>, Error> {
    refuse_named_waiting(old.as_ref(), &work.scope)?;
    let survey = surveyed(work, &Folders::of(manifest), run.verify)?;
    sync_surveyed(work, manifest, old, given, run, survey).await
}

/// the data folder as a sync starts from it: read, its doubted links checked, and with `verify`
/// every file hashed.
fn surveyed(work: &Loop<'_>, folders: &Folders, verify: bool) -> Result<Survey, Error> {
    let reading = work.context.output.setup("Reading", "the data folder");
    let mut survey = Survey::read(
        work.project,
        folders,
        work.context.threads,
        &work.context.stop,
    )?;
    drop(reading);
    let restored = place::restore_aside(work.project, &folders.data_dir, &mut survey, false);
    place::restored_aside(&work.context.output, &restored, false);
    survey.check_doubted(&work.project.root, &work.cache)?;
    if verify {
        survey.verify(&work.project.root, &work.scope, Some(&work.context.output))?;
    }
    disk::audit(work.project, &survey, &work.cache)?;
    Ok(survey)
}

/// a sync with nothing to do, finished: none unless [`place::untouched`] finds it settled and no
/// copy of a cached file waits to be linked to it.
fn settle(
    work: &Loop<'_>,
    manifest: &Manifest,
    survey: &mut Survey,
    seen: &Seen,
) -> Result<Option<Outcome<Placement>>, Error> {
    let Some(placement) =
        place::untouched(work.project, manifest, survey, seen, true, &work.scope)?
    else {
        return Ok(None);
    };
    if place::links_copies(
        work.project,
        manifest,
        survey,
        &work.cache,
        &work.scope,
        true,
    ) {
        return Ok(None);
    }
    // files hashing found unchanged settle with a state written now; a project folder
    // that takes no write fails no run that changed nothing
    if survey.needs_writing
        && let Err(error) = work.project.write_state(&survey.state)
    {
        tracing::debug!(error = %error, "could not settle the state");
    }
    // nothing is left to do, so a stop asked from here on, as by ctrl-c while the
    // last lines print, leaves the sync done
    work.context
        .stop
        .check()
        .map_err(|error| work.before_placing(error))?;
    work.context.stop.finish();
    register(work);
    Ok(Some(Outcome {
        lock_changed: false,
        pins: Vec::new(),
        placed: placement,
    }))
}

/// [`sync_all`] from `survey`, read as it reads one.
async fn sync_surveyed(
    work: &Loop<'_>,
    manifest: &Manifest,
    old: Option<Lock>,
    given: Option<(String, Listing)>,
    run: Run<'_>,
    mut survey: Survey,
) -> Result<Outcome<Placement>, Error> {
    let Run {
        upgrade,
        force,
        verify: _,
        read,
    } = run;
    let named = work.named();
    super::drop::brought_back(&mut survey, &named);
    let left_out = super::drop::placed_naming(manifest, &work.scope, &named, &survey.state.dropped);
    let manifest = &*left_out;
    let seen = match read {
        Read::Settled(seen) => {
            if let Some(outcome) = settle(work, manifest, &mut survey, seen)? {
                return Ok(outcome);
            }
            seen
        }
        Read::Planned(seen) => seen,
    };
    work.placing_next();
    let (lock, before) = work
        .relock(
            manifest,
            old,
            &upgrade,
            given
                .into_iter()
                .map(|(name, listing)| Given::Listing(name, listing))
                .collect(),
        )
        .instrument(tracing::debug_span!("relock"))
        .await
        .map_err(|error| work.before_placing(error))?;
    work.tell_versions_kept(manifest, &before, &lock, &upgrade);
    // a lock whose files cannot all land is refused before it is written, so data.lock never
    // pins what no sync can place. archives not unpacked here yet are checked once they are
    let plan = plan::from_cache(manifest, &lock, &work.cache)?;
    let written = work.write_lock(&before, &lock)?;
    let changed = written.is_some();
    let pins = engine::repinned(&before, &lock);
    let written = match &written {
        Some(text) => Some(digest::sha256(text.as_bytes())),
        None => seen.lock,
    };
    let synced = written.map(|lock| Synced {
        manifest: seen.manifest,
        lock,
        complete: false,
    });
    let refetched = refetched(&survey, &work.scope);
    let placement = work
        .place(manifest, &lock, plan, force, survey, synced)
        .await
        .map_err(|error| refetched.iter().cloned().fold(error, Error::detail))?;
    work.context.stop.finish();
    work.drop_unpacked(&lock);
    register(work);
    Ok(Outcome {
        lock_changed: changed,
        pins,
        placed: placement,
    })
}

/// error lines naming files of `scope` linked to a changed cache copy, why a sync that stops
/// before placing fetches their bytes again; its error alone may name only the download.
pub(super) fn refetched(survey: &Survey, scope: &Scope) -> Vec<String> {
    let mut paths: Vec<&String> = survey
        .elsewhere
        .iter()
        .filter(|path| {
            survey
                .state
                .files
                .get(*path)
                .is_some_and(|placed| scope.includes(&placed.dataset))
        })
        .collect();
    paths.sort_unstable();
    let mut said: Vec<String> = paths
        .iter()
        .take(5)
        .map(|path| {
            format!(
                "{path} is linked to the cache's copy, which was changed, so its pinned bytes must be fetched again"
            )
        })
        .collect();
    if paths.len() > 5 {
        said.push(format!(
            "and {} more files linked to changed cache copies",
            units::grouped((paths.len() - 5) as u64)
        ));
    }
    said
}

/// what [`sync_all`] would do, told as it is found. it changes nothing: no download, unpacking,
/// lock, state or registration, and cached objects are read without being sealed or marked. given
/// `seen`, a settled project needs neither the cache nor the network, as in [`sync_all`].
pub(super) async fn preview_all(
    work: &Loop<'_>,
    manifest: &Manifest,
    old: Option<Lock>,
    force: bool,
    verify: bool,
    given: Vec<Given>,
    seen: Option<&Seen>,
) -> Result<Outcome<Preview>, Error> {
    refuse_named_waiting(old.as_ref(), &work.scope)?;
    let reading = work.context.output.setup("Reading", "the data folder");
    let mut survey = Survey::read(
        work.project,
        &Folders::of(manifest),
        work.context.threads,
        &work.context.stop,
    )?;
    drop(reading);
    let restored = place::restore_aside(work.project, &manifest.data_dir, &mut survey, true);
    place::restored_aside(&work.context.output, &restored, true);
    survey.check_doubted(&work.project.root, &work.cache)?;
    if verify {
        survey.verify(&work.project.root, &work.scope, Some(&work.context.output))?;
    }
    let damaged = work.damaged_objects(&survey)?;
    if let Some(seen) = seen
        && let Some(placement) =
            place::untouched(work.project, manifest, &survey, seen, false, &work.scope)?
    {
        return Ok(Outcome {
            lock_changed: false,
            pins: Vec::new(),
            placed: Preview {
                placement,
                ..Preview::default()
            },
        });
    }
    let engine::DryRelock {
        lock,
        before,
        jobs,
        guessed,
        ..
    } = work
        .relock_dry(manifest, old, &Upgrade::Nothing, given)
        .await?;
    let changed = !before.holds(&lock);
    let output = &work.context.output;
    if changed {
        engine::tell_lock(
            output,
            &before,
            &lock,
            engine::Verbs::WOULD,
            None,
            &engine::sizeless(&jobs),
        );
    }
    let preview = work
        .preview(manifest, &lock, force, survey, jobs, &damaged, &guessed)
        .await?;
    if let Some(total) = &preview.changes.to_download {
        super::fetch::tell_would_fetch(output, &preview.changes.download, total);
    }
    for archive in &preview.changes.unpack {
        output.step("Would open", archive);
    }
    if !preview.changes.unpack.is_empty() {
        output.hint("an archive's files are known once it is unpacked, so they are not counted");
    }
    place::tell_preview(output, manifest, &work.scope, &preview);
    Ok(Outcome {
        lock_changed: changed,
        pins: engine::repinned(&before, &lock),
        placed: preview,
    })
}

/// refuses a run of named datasets that names one waiting for its file list, before anything
/// else, since the run has nothing to place for it.
fn refuse_named_waiting(lock: Option<&Lock>, scope: &Scope) -> Result<(), Error> {
    if let (Scope::Only(names), Some(lock)) = (scope, lock) {
        lock.refuse_waiting(names.iter().map(String::as_str))?;
    }
    Ok(())
}

/// warns of each dataset of `waiting`, those that wait for their file list, in `scope`, with the
/// part that is not there and how to bring it back.
pub(super) fn tell_waiting(output: &Output, waiting: &[lock::Waiting], scope: &Scope) {
    for waiting in waiting {
        if let Scope::Only(names) = scope
            && !names.contains(&waiting.name)
        {
            continue;
        }
        let error = waiting.error();
        output.warn_detailed(
            Warning::LockListMissing,
            &format!("{}, so {} waits for it", error.message, waiting.name),
            error.help.as_deref(),
            [&waiting.part],
        );
    }
}

/// the last line of every dry run, with what it found when there is nothing to do.
pub(super) fn dry_run_done(output: &Output, nothing_to_do: Option<&str>) {
    if let Some(found) = nothing_to_do {
        output.done("Ok", &format!("{found}; dry run, nothing was changed"));
    } else {
        output.done("Ok", "dry run, nothing was changed");
        output.next("run the same command without --dry-run to make these changes");
    }
}

/// a failure only warns: gc keeps what registered projects need, so it risks a later download.
pub(super) fn register(work: &Loop<'_>) {
    if let Err(err) = work.cache.register(&work.project.root) {
        work.context.output.warn_with(
            Warning::CacheUnregistered,
            &format!(
                "could not add this project to the cache's list of projects in {}: {err}. `fetchloom cache gc` may then delete files this project needs",
                work.cache.root().display()
            ),
            "check the cache folder's permissions, then run the command again",
        );
    }
}

/// the folders a project's data folder may be a link into, as the run's settings list them.
pub(super) fn data_links(context: &Context) -> crate::project::DataLinks {
    let listed = &context.settings.data_links;
    crate::project::DataLinks {
        folders: listed.value.clone(),
        from_variable: listed.origin == crate::settings::Origin::Environment,
    }
}

/// the project around the current folder.
pub(super) fn find(context: &Context) -> Result<Project, Error> {
    let output = &context.output;
    let here =
        std::env::current_dir().map_err(|err| Error::io("cannot read the current folder", &err))?;
    let mut project = Project::find(&here).ok_or_else(|| match Project::repository(&here) {
        Some(repository) => {
            let error = Error::new(
                Kind::ProjectInvalid,
                format!(
                    "there is no data.toml in this folder or any folder above it inside the git repository at {}",
                    repository.display()
                ),
            );
            let below = projects_below(&here);
            let Some(first) = below.first() else {
                return error.help(format!("run `{}` to start a project here, or give -C the folder of a project outside this repository, as in `fetchloom -C <folder> <command>`", command("add <link>")));
            };
            // after -C, the folder below it is named whole, as the run's own folder is
            let run = super::typed::in_folder(first).map_or_else(
                || {
                    let folder = here.join(first);
                    format!(
                        "fetchloom -C {} <command>",
                        crate::suggested::shell_folder(&folder.to_string_lossy())
                    )
                },
                crate::suggested::written,
            );
            let folders: Vec<String> = below.iter().map(|folder| format!("{folder}/")).collect();
            if let [one] = folders.as_slice() {
                error.help(format!("there is one in {one}: run `{run}`"))
            } else {
                let listed = units::listed(&folders, "and");
                error.help(format!("there is one in each of {listed}: give -C its folder, as in `{run}`"))
            }
        }
        None => Error::new(
            Kind::ProjectInvalid,
            "there is no data.toml in this folder or any folder above it",
        )
        .help(format!("run `{}` to start a project here", command("add <link>"))),
    })?;
    tracing::debug!(root = %project.root.display(), "found data.toml");
    if project.root != here {
        output.aside(&format!(
            "using the project in {}",
            units::path(&project.root)
        ));
    }
    project.data_links = data_links(context);
    Ok(project)
}

/// the folders below `here` that hold a data.toml, as paths from it with `/` between folders:
/// those at the nearest depth that has any, up to three folders down. hidden folders and links
/// are passed by, and a large tree is read only in part.
fn projects_below(here: &Path) -> Vec<String> {
    const DEPTH: usize = 3;
    const MOST_FOLDERS: usize = 2_000;
    let mut level = vec![(here.to_path_buf(), String::new())];
    let mut read = 0;
    for _ in 0..DEPTH {
        let mut found = Vec::new();
        let mut next = Vec::new();
        for (folder, shown) in level {
            read += 1;
            if read > MOST_FOLDERS {
                break;
            }
            let Ok(entries) = std::fs::read_dir(&folder) else {
                continue;
            };
            for entry in entries.flatten() {
                let Ok(name) = entry.file_name().into_string() else {
                    continue;
                };
                // a name a shell would need quoted, as one with a backtick, never reaches a help
                let plain = matches!(super::shell_word(&name), std::borrow::Cow::Borrowed(_));
                if name.starts_with('.')
                    || !plain
                    || !entry.file_type().is_ok_and(|kind| kind.is_dir())
                {
                    continue;
                }
                let inner = if shown.is_empty() {
                    name
                } else {
                    format!("{shown}/{name}")
                };
                if entry.path().join(manifest::FILE).is_file() {
                    found.push(inner);
                } else {
                    next.push((entry.path(), inner));
                }
            }
        }
        if !found.is_empty() || next.is_empty() {
            found.sort();
            return found;
        }
        level = next;
    }
    Vec::new()
}

pub(super) enum Waited<T> {
    Held(Held),
    Ready(T),
}

/// takes the project lock, waiting while another fetchloom changes the project.
pub(super) async fn hold(
    context: &Context,
    project: &Project,
    command: &str,
) -> Result<Held, Error> {
    match hold_until(context, project, command, || {
        Ok(None::<std::convert::Infallible>)
    })
    .await?
    {
        Waited::Held(held) => Ok(held),
        Waited::Ready(never) => match never {},
    }
}

pub(super) async fn hold_until<T>(
    context: &Context,
    project: &Project,
    command: &str,
    mut ready: impl FnMut() -> Result<Option<T>, Error>,
) -> Result<Waited<T>, Error> {
    let mut told = false;
    let mut told_owner = false;
    let mut setup = Some(context.output.setup("Locking", "the project"));
    // counts its seconds on a terminal until the hold is taken
    let mut _waiting = None;
    loop {
        if context.stop.is_asked() {
            return Err(Error::new(
                Kind::Interrupted,
                "stopped while waiting for another fetchloom",
            ));
        }
        if told && let Some(answer) = ready()? {
            return Ok(Waited::Ready(answer));
        }
        match project.hold(command)? {
            Hold::Held(mut held) => {
                if let Some(path) = held.removed_unread() {
                    context.output.warn(
                        Warning::ProjectOwnerRemoved,
                        &format!(
                            "removed {}, which no fetchloom wrote, as a cloned project can hold, and went on",
                            path.display()
                        ),
                    );
                }
                if project.finish_pending()? {
                    context.output.step(
                        "Finished",
                        "data.lock, which a stopped command left behind data.toml",
                    );
                }
                return Ok(Waited::Held(held));
            }
            Hold::Busy(holder) if !told => {
                // the line about the other fetchloom says what the wait is
                drop(setup.take());
                let who = holder.as_deref().unwrap_or("another fetchloom");
                _waiting = Some(
                    context
                        .output
                        .waiting("Waiting", &format!("for {who} working in this project")),
                );
                told = true;
            }
            Hold::Owner(message) if !told_owner => {
                drop(setup.take());
                _waiting = Some(context.output.waiting("Waiting", &message));
                told = true;
                told_owner = true;
            }
            Hold::Busy(_) | Hold::Owner(_) => {}
        }
        tokio::select! {
            () = tokio::time::sleep(Duration::from_millis(200)) => {}
            () = context.stop.asked() => return Err(Error::new(Kind::Interrupted, "stopped while waiting for another fetchloom")),
        }
    }
}

/// refuses a name data.toml does not have, suggesting the closest as `command` would take it, as
/// `path` in `fetchloom path iris`; a link typed as a name is shown with its secrets masked.
pub(super) fn known(manifest: &Manifest, name: &str, command: &str) -> Result<(), Error> {
    known_with_rest(manifest, name, "", command)
}

/// [`known`] for a name typed with `rest` after it, such as `/iris.csv` in `irs/iris.csv`,
/// which a guess at the name keeps.
pub(super) fn known_with_rest(
    manifest: &Manifest,
    name: &str,
    rest: &str,
    command: &str,
) -> Result<(), Error> {
    if manifest.datasets.contains_key(name) {
        return Ok(());
    }
    let error = Error::new(
        Kind::Usage,
        format!(
            "data.toml has no dataset called \"{}\"",
            crate::shown::mask_reference(name)
        ),
    );
    let typed = format!("{name}{rest}");
    Err(
        match name::closest(name, manifest.datasets.keys().map(String::as_str)) {
            Some(best) => {
                let shown = super::meant(
                    &[&typed],
                    &[&format!("{best}{rest}")],
                    format!(
                        "fetchloom {command} {best}{}",
                        crate::shown::mask_reference(rest)
                    ),
                );
                error.help(format!("did you mean `{shown}`?"))
            }
            None if manifest.datasets.is_empty() => error.help(format!(
                "data.toml has no datasets yet. add one with `{}`",
                self::command("add <link>")
            )),
            None => {
                let names: Vec<&str> = manifest.datasets.keys().map(String::as_str).collect();
                let shown = super::meant(
                    &[&typed],
                    &names[..1],
                    format!("fetchloom {command} {}", names[0]),
                );
                error.help(format!("name one of {}, as in `{shown}`", names.join(", ")))
            }
        },
    )
}

#[cfg(test)]
pub(crate) mod tests {
    /// a folder whose name a shell would need quoted is passed by, so no clone's folder name
    /// steers the help, or the `fix` taken from it.
    #[test]
    fn projects_below_pass_by_names_a_shell_would_need_quoted() {
        let root = tempfile::tempdir().unwrap();
        for name in ["analysis", "x`fetchloom remove y`", "two words"] {
            let folder = root.path().join(name);
            std::fs::create_dir_all(&folder).unwrap();
            std::fs::write(folder.join("data.toml"), "").unwrap();
        }
        assert_eq!(super::projects_below(root.path()), ["analysis"]);
    }

    use super::*;
    use crate::cache::Cache;
    use crate::output::{Level, Output, Style};
    use crate::project::manifest;
    use crate::source::Reference;
    use crate::source::recorded::block_on;

    pub(crate) fn waiting_project() -> (tempfile::TempDir, Project, Context) {
        let dir = tempfile::tempdir().unwrap();
        let project = Project::at(dir.path().to_path_buf());
        std::fs::create_dir(project.own_folder()).unwrap();
        let os = if cfg!(windows) { "linux" } else { "windows" };
        let owner = serde_json::json!({
            "os": os,
            "pid": 42,
            "started": "2026-10-30T12:00:00Z",
            "command": "sync",
        });
        std::fs::write(
            project.own_folder().join("owner"),
            serde_json::to_vec(&owner).unwrap(),
        )
        .unwrap();
        let style = Style {
            color: false,
            unicode: false,
            interactive: false,
            watched: false,
            links: false,
            accessible: false,
        };
        let context = Context::new(Output::new(style, Level::Quiet, false, false));
        (dir, project, context)
    }

    #[test]
    fn an_owner_wait_stops_on_ctrl_c() {
        let (_dir, project, context) = waiting_project();
        let path = project.own_folder().join("owner");
        let before = std::fs::read(&path).unwrap();
        block_on(async {
            let mut waiting = Box::pin(hold(&context, &project, "remove"));
            std::future::poll_fn(|task| {
                assert!(std::future::Future::poll(waiting.as_mut(), task).is_pending());
                std::task::Poll::Ready(())
            })
            .await;
            context.stop.ask();
            let Err(error) = tokio::time::timeout(Duration::from_secs(2), &mut waiting)
                .await
                .unwrap()
            else {
                panic!("a stopped wait must not acquire the project")
            };
            assert_eq!(error.kind, Kind::Interrupted);
            assert_eq!(error.message, "stopped while waiting for another fetchloom");
        });
        assert_eq!(std::fs::read(&path).unwrap(), before);
        std::fs::remove_file(&path).unwrap();
        let Err(error) = block_on(hold(&context, &project, "remove")) else {
            panic!("a pending stop must not claim a free project")
        };
        assert_eq!(error.kind, Kind::Interrupted);
        assert!(!path.exists());
    }

    #[test]
    fn an_owner_wait_resumes_when_the_record_is_removed() {
        let (_dir, project, context) = waiting_project();
        let path = project.own_folder().join("owner");
        block_on(async {
            let mut waiting = Box::pin(hold(&context, &project, "remove"));
            std::future::poll_fn(|task| {
                assert!(std::future::Future::poll(waiting.as_mut(), task).is_pending());
                std::task::Poll::Ready(())
            })
            .await;
            std::fs::remove_file(&path).unwrap();
            let held = tokio::time::timeout(Duration::from_secs(2), &mut waiting)
                .await
                .unwrap()
                .unwrap();
            let owner: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            assert_eq!(owner["command"], "remove");
            drop(held);
        });
        assert!(!path.exists());
    }

    pub(crate) fn offline_context() -> Context {
        let style = Style {
            color: false,
            unicode: false,
            interactive: false,
            watched: false,
            links: false,
            accessible: false,
        };
        let mut context = Context::new(Output::new(style, Level::Quiet, false, false));
        context.offline = crate::net::Offline::Flag;
        context
    }

    /// a file a source states the hash of, so pinning it needs no download.
    pub(crate) fn stated(path: &str, url: &str, bytes: &[u8]) -> crate::source::Remote {
        crate::source::Remote {
            path: path.to_owned(),
            size: Some(bytes.len() as u64),
            sha256: Some(digest::sha256(bytes)),
            md5: None,
            url: url::Url::parse(url).unwrap(),
            named_by_server: false,
            etag: None,
            blob: None,
        }
    }

    pub(crate) fn listed(url: &str, files: Vec<crate::source::Remote>) -> Listing {
        Listing {
            within: Vec::new(),
            access: crate::source::Access::Unknown,
            resolved: Reference::Url(url::Url::parse(url).unwrap()),
            title: None,
            license: None,
            doi: None,
            files,
            unavailable: Vec::new(),
            warnings: Vec::new(),
            rounded: None,
        }
    }

    pub(crate) fn pinned(name: &str, spec: String, files: Vec<lock::LockedFile>) -> lock::Locked {
        lock::Locked {
            name: name.to_owned(),
            reference: format!("https://example.org/{name}"),
            spec,
            at: None,
            extract: true,
            decompress: false,
            web_page: false,
            resolved: format!("https://example.org/{name}"),
            title: None,
            license: None,
            doi: None,
            access: crate::source::Access::Unknown,
            timestamp: "2026-01-01T00:00:00Z".to_owned(),
            files,
        }
    }

    /// data.toml and data.lock as a test pretends to have read them.
    fn unread() -> Seen {
        Seen {
            manifest: digest::sha256(b"data.toml"),
            lock: None,
        }
    }

    pub(crate) fn pinned_file(path: &str, url: &str, bytes: &[u8]) -> lock::LockedFile {
        lock::LockedFile {
            path: path.to_owned(),
            size: bytes.len() as u64,
            sha256: digest::sha256(bytes),
            md5: None,
            url: url.to_owned(),
            unpacks_to: None,
        }
    }

    #[test]
    fn a_lock_that_cannot_be_placed_is_not_written() {
        let dir = tempfile::tempdir().unwrap();
        let project = Project::at(dir.path().to_path_buf());
        let context = offline_context();
        let cache = crate::cache::Cache::open(dir.path().join("cache")).unwrap();
        let manifest = Manifest::parse(
            "[datasets.a]\nref = \"https://example.org/a\"\npath = \"shared\"\n\n[datasets.b]\nref = \"https://example.org/b\"\npath = \"shared\"\n",
        )
        .unwrap();
        let a = pinned(
            "a",
            lock::spec(&manifest.datasets["a"].source),
            vec![pinned_file("README", "https://example.org/a", b"a")],
        );
        let old = Lock {
            datasets: vec![a],
            ..Lock::default()
        };
        std::fs::write(project.root.join(lock::FILE), old.render()).unwrap();
        let work = Loop::new(&context, &project, Arc::new(context.net()), cache);
        let given = (
            "b".to_owned(),
            listed(
                "https://example.org/b",
                vec![stated("README", "https://example.org/b", b"b")],
            ),
        );

        let error = block_on(sync_all(
            &work,
            &manifest,
            Some(old.clone()),
            Some(given),
            Run::planned(&unread()),
        ))
        .unwrap_err();

        assert_eq!(
            error.message, "datasets a and b would both place data/shared/README",
            "{error:?}"
        );
        assert_eq!(
            std::fs::read_to_string(project.root.join(lock::FILE)).unwrap(),
            old.render(),
            "data.lock changed though nothing could be placed"
        );
    }

    /// `b` pinned with a README and `a` listing only `remote`, both in the data folder itself: the
    /// error a sync meets first, with every download failing, and whether data.lock stayed.
    fn sync_beside_a_readme(remote: crate::source::Remote) -> (Error, bool) {
        let dir = tempfile::tempdir().unwrap();
        let project = Project::at(dir.path().to_path_buf());
        let context = offline_context();
        let cache = crate::cache::Cache::open(dir.path().join("cache")).unwrap();
        let manifest = Manifest::parse(
            "[datasets.a]\nref = \"https://example.org/a\"\npath = \".\"\n\n[datasets.b]\nref = \"https://example.org/b\"\npath = \".\"\n",
        )
        .unwrap();
        let b = pinned(
            "b",
            lock::spec(&manifest.datasets["b"].source),
            vec![pinned_file("README", "https://example.org/b", b"b")],
        );
        let old = Lock {
            datasets: vec![b],
            ..Lock::default()
        };
        std::fs::write(project.root.join(lock::FILE), old.render()).unwrap();
        let work = Loop::new(&context, &project, Arc::new(context.net()), cache);
        let given = (
            "a".to_owned(),
            listed("https://example.org/a", vec![remote]),
        );
        let error = block_on(sync_all(
            &work,
            &manifest,
            Some(old.clone()),
            Some(given),
            Run::planned(&unread()),
        ))
        .unwrap_err();
        let kept = std::fs::read_to_string(project.root.join(lock::FILE)).unwrap() == old.render();
        (error, kept)
    }

    /// the datasets are named in data.lock's order, as the check after the downloads names them.
    #[test]
    fn a_clash_the_listing_shows_is_found_before_any_download() {
        let mut listed = stated("README", "https://example.org/a/README", b"a");
        listed.sha256 = None;
        // the MD5 of a, as zenodo and S3 list it
        listed.md5 = crate::digest::Md5::from_hex("0cc175b9c0f1b6a831c399e269772661");

        let (error, kept) = sync_beside_a_readme(listed);

        assert_eq!(error.kind, Kind::ProjectInvalid, "{error:?}");
        assert_eq!(
            error.message,
            "datasets a and b would both place data/README"
        );
        assert!(kept, "data.lock changed though nothing could be placed");
    }

    /// before the download, the name is only a guess from the link.
    #[test]
    fn a_name_the_server_gives_is_checked_once_downloaded() {
        let mut guessed = stated("README", "https://example.org/a/README", b"a");
        guessed.sha256 = None;
        guessed.size = None;
        guessed.named_by_server = true;

        let (error, kept) = sync_beside_a_readme(guessed);

        assert_eq!(error.kind, Kind::OfflineMissing, "{error:?}");
        assert!(kept);
    }

    /// kaggle may send a competition file as a zip of itself, pinned as `<name>.zip`; with a real
    /// `<name>.zip` beside it, pinning both on one path would leave data.lock unreadable.
    #[test]
    fn two_files_on_one_path_are_refused_before_data_lock_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let project = Project::at(dir.path().to_path_buf());
        let context = offline_context();
        let cache = crate::cache::Cache::open(dir.path().join("cache")).unwrap();
        let manifest = Manifest::parse(
            "[datasets.comp]\nref = \"https://example.org/comp\"\nextract = false\n",
        )
        .unwrap();
        let download = "https://api.kaggle.com/v1/competitions/data/download/titanic";
        let wrapped = format!("{download}/train.csv");
        let zipped = format!("{download}/train.csv.zip");
        let old = Lock {
            datasets: vec![pinned(
                "comp",
                "sha256:an older entry".to_owned(),
                vec![pinned_file("train.csv.zip", &wrapped, b"wrapped")],
            )],
            ..Lock::default()
        };
        std::fs::write(project.root.join(lock::FILE), old.render()).unwrap();
        let work = Loop::new(&context, &project, Arc::new(context.net()), cache);
        let mut unstated = stated("train.csv", &wrapped, b"wrapped");
        unstated.sha256 = None;
        let given = (
            "comp".to_owned(),
            listed(
                "https://example.org/comp",
                vec![unstated, stated("train.csv.zip", &zipped, b"a real zip")],
            ),
        );

        let error = block_on(sync_all(
            &work,
            &manifest,
            Some(old.clone()),
            Some(given),
            Run::planned(&unread()),
        ))
        .unwrap_err();

        assert_eq!(error.kind, Kind::ProjectInvalid);
        assert!(
            error.message.contains("comp")
                && error.message.contains("train.csv.zip")
                && error.message.contains("pinned"),
            "{error:?}"
        );
        assert_eq!(
            std::fs::read_to_string(project.root.join(lock::FILE)).unwrap(),
            old.render(),
            "data.lock lists a path twice"
        );
        assert!(Lock::parse(&old.render()).is_ok());
    }

    /// an unknown name may be a link given where a dataset's name belongs.
    #[test]
    fn an_unknown_name_never_shows_a_secret() {
        let manifest =
            Manifest::parse("[datasets.iris]\nref = \"https://example.org/iris.csv\"\n").unwrap();

        let error = known(
            &manifest,
            "https://example.org/x?token=AUDIT_CANARY",
            "sync",
        )
        .unwrap_err();

        let shown = format!("{} {:?} {:?}", error.message, error.details, error.help);
        assert!(!shown.contains("AUDIT_CANARY"), "{shown}");
        assert!(shown.contains("token=***"), "{shown}");
    }

    /// ctrl-c while the last lines print leaves the sync done; a stop before ends it as stopped.
    #[test]
    fn a_stop_once_a_settled_sync_found_nothing_to_do_leaves_it_done() {
        let dir = tempfile::tempdir().unwrap();
        let project = Project::at(dir.path().join("project"));
        std::fs::create_dir(&project.root).unwrap();
        project.write(manifest::FILE, b"").unwrap();
        let manifest = Manifest::parse("").unwrap();
        let cache = Cache::open(dir.path().join("cache")).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _entered = runtime.enter();
        // a first sync writes data.lock and the state that vouch for the project
        let first = offline_context();
        let unlocked = Seen {
            manifest: digest::sha256(b""),
            lock: None,
        };
        let work = Loop::new(&first, &project, Arc::new(first.net()), cache.clone());
        let synced = sync_all(&work, &manifest, None, None, Run::planned(&unlocked));
        assert!(super::super::block_on(&first, synced).is_ok());
        let (lock, text) = project.lock_and_text().unwrap().unwrap();
        let seen = Seen {
            manifest: digest::sha256(b""),
            lock: Some(digest::sha256(text.as_bytes())),
        };
        let survey = Survey::read(
            &project,
            &Folders::of(&manifest),
            first.threads,
            &first.stop,
        )
        .unwrap();
        assert!(
            place::untouched(&project, &manifest, &survey, &seen, false, &Scope::All)
                .unwrap()
                .is_some(),
            "the project is not settled"
        );
        let mut stopped = Vec::new();
        for late in [true, false] {
            let context = offline_context();
            let work = Loop::new(&context, &project, Arc::new(context.net()), cache.clone());
            let synced = super::super::block_on(&context, async {
                if !late {
                    context.stop.ask();
                }
                let synced = sync_all(
                    &work,
                    &manifest,
                    Some(lock.clone()),
                    None,
                    Run {
                        read: Read::Settled(&seen),
                        ..Run::planned(&seen)
                    },
                )
                .await;
                context.stop.ask();
                synced.map(|_| ())
            });
            stopped.push(synced.err().map(|error| error.kind));
        }
        assert_eq!(stopped, [None, Some(Kind::Interrupted)]);
    }

    /// a settled project whose data.toml is as `add` writes it syncs without parsing it, and once
    /// data.lock changed parses it once, from its own cache, never the one other runs hold.
    #[test]
    fn a_settled_sync_of_a_plain_data_toml_parses_none() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("project");
        let project = Project::at(root.clone());
        std::fs::create_dir(&root).unwrap();
        let text = "[datasets.iris]\nref = \"https://example.org/iris\"\n";
        project.write(manifest::FILE, text.as_bytes()).unwrap();
        let manifest = Manifest::parse(text).unwrap();
        let cache = Cache::open(dir.path().join("cache")).unwrap();
        let bytes = b"5.1,3.5,1.4,0.2\n";
        crate::cache::tests::store(&cache, bytes);
        let iris = pinned(
            "iris",
            lock::spec(&manifest.datasets["iris"].source),
            vec![pinned_file("iris.csv", "https://example.org/iris", bytes)],
        );
        let lock = Lock {
            datasets: vec![iris],
            ..Lock::default()
        };
        let locked = lock.render();
        project.write(lock::FILE, locked.as_bytes()).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _entered = runtime.enter();
        let first = offline_context();
        let seen = Seen {
            manifest: digest::sha256(text.as_bytes()),
            lock: Some(digest::sha256(locked.as_bytes())),
        };
        let work = Loop::new(&first, &project, Arc::new(first.net()), cache);
        let synced = sync_all(&work, &manifest, Some(lock), None, Run::planned(&seen));
        super::super::block_on(&first, synced).unwrap();
        let own = Cache::open(dir.path().join("own")).unwrap();
        crate::cache::tests::store(&own, bytes);
        super::super::tests::CACHE.set(Some(own.root().to_path_buf()));
        let args = args::Sync {
            names: Vec::new(),
            force: false,
            locked: false,
            dry: args::DryRun { dry_run: false },
            verify: false,
            strict: false,
        };
        let sync = || {
            let context = offline_context();
            let before = manifest::tests::reads();
            let project = Project::at(root.clone());
            let synced = sync_held(&context, project, args.clone());
            let (report, _) = super::super::block_on(&context, synced).unwrap();
            let placement = report.placement.unwrap();
            let reads = manifest::tests::reads() - before;
            (placement.unchanged, placement.placed, reads)
        };
        assert_eq!(sync(), (1, 0, 0));
        project
            .write(lock::FILE, format!("{locked}\n").as_bytes())
            .unwrap();
        assert_eq!(sync(), (1, 0, 1));
        assert_eq!(sync(), (1, 0, 0));
        assert_eq!(
            own.registered_projects(&crate::stop::Stop::default())
                .unwrap(),
            [root]
        );
    }

    /// a sync of every dataset counts only those it placed, not those a drop left out.
    #[test]
    fn a_sync_that_places_no_dataset_says_so() {
        let raw = "[datasets.raw]\nref = \"zenodo:1\"\n";
        let iris = "[datasets.iris]\nref = \"zenodo:2\"\n";
        for (tables, kept, said) in [
            (&[raw, iris][..], 0, ("Synced", "1 dataset into data")),
            (&[raw], 0, ("Ok", "nothing to place in data")),
            (
                &[raw],
                1,
                (
                    "Ok",
                    "nothing to place in data, 1 file left unchanged; see the warnings above",
                ),
            ),
        ] {
            let mut manifest = Manifest::parse(&tables.join("\n")).unwrap();
            super::super::drop::left_out(&mut manifest, &BTreeSet::from(["raw".to_owned()]));
            let (verb, text) = synced(&manifest, &Scope::All, (kept, &[], kept > 0), &[]);
            assert_eq!((verb, text.as_str()), said, "{tables:?}");
        }
    }
}
