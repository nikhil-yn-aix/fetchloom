//! `sync` and `lock`, and the steps the other project commands share with them: finding the
//! project, taking its lock and the run that brings data.lock, the cache and the data folder in
//! line. each command reads the project, takes its lock when it changes something, and leaves the
//! order of work to the sync loop.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tracing::Instrument as _;

use super::args;
use super::sync::disk::{self, Survey};
use super::sync::place::{self, Changes, Download, Placement, Preview, Seen};
use super::sync::{self as engine, Loop, Upgrade};
use super::{Context, open_cache, peek_cache};
use crate::digest;
use crate::error::{Error, Kind};
use crate::name;
use crate::net::Net;
use crate::output::Output;
use crate::plan;
use crate::project::lock::{self, Lock};
use crate::project::manifest::Manifest;
use crate::project::state::Synced;
use crate::project::{Held, Hold, Project};
use crate::source::{Listing, Reference};
use crate::units;

/// what a project command did, or with `--dry-run` would do, as `--json` prints it.
#[derive(Default, Serialize)]
pub(super) struct Report {
    pub(super) project: PathBuf,
    /// datasets added, removed or upgraded.
    pub(super) datasets: Vec<String>,
    pub(super) lock_changed: bool,
    pub(super) placement: Option<Placement>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(super) dry_run: bool,
    /// with `--dry-run`, the paths that would change.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) changes: Option<Changes>,
}

impl Report {
    /// what a dry run found: whether data.lock would change and what placing would do.
    pub(super) fn dry(project: PathBuf, datasets: Vec<String>, outcome: Outcome<Preview>) -> Self {
        Report {
            project,
            datasets,
            lock_changed: outcome.lock_changed,
            placement: Some(outcome.placed.placement),
            dry_run: true,
            changes: Some(outcome.placed.changes),
        }
    }
}

pub(super) fn preview_ignore(work: &Loop<'_>, manifest: &Manifest) -> Result<(), Error> {
    if work.project.ignored_in_git(&manifest.data_dir)?.is_some() {
        work.context.output.step(
            "Would add",
            &format!(
                "{}/ to .gitignore, since its files come from data.lock",
                manifest.data_dir
            ),
        );
    }
    Ok(())
}

pub(super) fn ignore_data(work: &Loop<'_>, manifest: &Manifest) -> Result<(), Error> {
    if work.project.ignore_in_git(&manifest.data_dir)? {
        work.context.output.step(
            "Ignored",
            &format!(
                "{}/ in .gitignore, since its files come from data.lock",
                manifest.data_dir
            ),
        );
    }
    Ok(())
}

/// the project files a person commits: data.lock.d as well when a big dataset keeps its file
/// list there, since a clone without it cannot read data.lock.
pub(super) fn committed(project: &Project) -> &'static str {
    if project.root.join(lock::lists::FOLDER).is_dir() {
        "data.toml, data.lock and data.lock.d"
    } else {
        "data.toml and data.lock"
    }
}

pub(super) async fn sync(context: &Context, args: args::Sync) -> Result<Report, Error> {
    let project = find(&context.output)?;
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
                    Some(report) => Ok(report),
                    None => Err(error),
                };
            }
            Err(error) => return Err(error),
        }
    };
    let (text, manifest) = project.manifest()?;
    let read = project.lock_and_text()?;
    if args.locked {
        let Some((lock, _)) = &read else {
            return Err(Error::new(Kind::Outdated, "there is no data.lock yet")
                .help("run `fetchloom lock` and commit data.lock, or sync without --locked"));
        };
        check(&project, &manifest, Some(lock)).map_err(|error| {
            error.help("run `fetchloom lock` and commit data.lock, or sync without --locked")
        })?;
    }
    let seen = Seen {
        manifest: digest::sha256(text.as_bytes()),
        lock: read
            .as_ref()
            .map(|(_, text)| digest::sha256(text.as_bytes())),
    };
    if dry_run {
        return preview_sync(context, &project, &manifest, read, &seen, &args).await;
    }
    let old = read.map(|(lock, _)| lock);
    let work = Loop::new(
        context,
        &project,
        Arc::new(net(context, &manifest)),
        open_cache()?,
    );
    let Outcome {
        lock_changed,
        pins,
        placed: placement,
    } = sync_all(
        &work,
        &manifest,
        old,
        &Upgrade::Nothing,
        args.force,
        args.verify,
        None,
        Read::Settled(&seen),
    )
    .await?;
    // a project that became a git repository after its add gets its ignore rule now
    ignore_data(&work, &manifest)?;
    tell_synced(
        context,
        &project,
        &manifest,
        lock_changed,
        &placement,
        args.verify,
    );
    Ok(Report {
        project: project.root,
        datasets: pins,
        lock_changed,
        placement: Some(placement),
        ..Report::default()
    })
}

/// a sync that would write nothing, found without the project's hold: data.toml and data.lock
/// are byte for byte as the last complete sync left them, every file still looks as placed, no
/// doubt is open, and the ignore rule is in place. none when a sync would write anything, which
/// needs the hold.
fn settled_unheld(
    context: &Context,
    project: &Project,
    args: &args::Sync,
) -> Result<Option<Report>, Error> {
    let (text, manifest) = project.manifest()?;
    let Some((lock, lock_text)) = project.lock_and_text()? else {
        return Ok(None);
    };
    if args.locked {
        check(project, &manifest, Some(&lock)).map_err(|error| {
            error.help("run `fetchloom lock` and commit data.lock, or sync without --locked")
        })?;
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
    let mut survey = Survey::read(project, &manifest.data_dir, context.threads);
    // hashing proved the files placed too recently to trust by time unchanged, and only asks
    // for the state to be written again, which this project cannot be. an edit hashing found
    // stays an edit
    survey.needs_writing = false;
    survey.check_doubted(&project.root, &cache)?;
    if args.verify {
        survey.verify(&project.root)?;
    }
    let Some(placement) = place::untouched(project, &manifest, &survey, &seen, false)? else {
        return Ok(None);
    };
    if project.ignored_in_git(&manifest.data_dir)?.is_some() {
        return Ok(None);
    }
    if held && let Err(err) = cache.register(&project.root) {
        tracing::debug!(%err, "could not list a read-only project in the cache");
    }
    tell_synced(context, project, &manifest, false, &placement, args.verify);
    Ok(Some(Report {
        project: project.root.clone(),
        datasets: Vec::new(),
        lock_changed: false,
        placement: Some(placement),
        ..Report::default()
    }))
}

/// the last lines of a sync: nothing to do, or what it synced and what to do next.
fn tell_synced(
    context: &Context,
    project: &Project,
    manifest: &Manifest,
    lock_changed: bool,
    placement: &Placement,
    verified: bool,
) {
    if !lock_changed
        && placement.placed + placement.replaced + placement.removed == 0
        && placement.kept.is_empty()
    {
        context.output.done(
            "Ok",
            if verified {
                "all pinned files are in place, checked byte for byte; nothing to do"
            } else {
                "all pinned files are in place by size and time, nothing to do"
            },
        );
    } else {
        let datasets = crate::units::count(manifest.datasets.len() as u64, "dataset", "datasets");
        if placement.kept.is_empty() {
            context
                .output
                .done("Synced", &format!("{datasets} into {}", manifest.data_dir));
        } else {
            let kept = crate::units::count(
                placement.kept.len() as u64,
                "file left unchanged",
                "files left unchanged",
            );
            context.output.done(
                "Synced",
                &format!(
                    "{datasets} into {}, {kept}; see the warnings above",
                    manifest.data_dir
                ),
            );
        }
        if lock_changed {
            context.output.next(&format!(
                "commit {}, so other clones use these pins",
                committed(project)
            ));
        } else if let Some(name) = manifest.datasets.keys().next()
            && placement.placed + placement.replaced > 0
        {
            context.output.next(&format!(
                "`fetchloom path {name}` tells your scripts where {name} is"
            ));
        }
    }
}

/// `sync --dry-run`: what syncing would change, given data.lock as `read` and both files as
/// `seen`.
async fn preview_sync(
    context: &Context,
    project: &Project,
    manifest: &Manifest,
    read: Option<(Lock, String)>,
    seen: &Seen,
    args: &args::Sync,
) -> Result<Report, Error> {
    let old = read.map(|(lock, _)| lock);
    let work = Loop::new(
        context,
        project,
        Arc::new(net(context, manifest)),
        peek_cache()?,
    );
    let outcome = preview_all(
        &work,
        manifest,
        old,
        args.force,
        args.verify,
        None,
        Some(seen),
    )
    .await?;
    preview_ignore(&work, manifest)?;
    let nothing = !outcome.lock_changed && outcome.placed.changes_nothing();
    dry_run_done(
        &context.output,
        nothing.then_some(if args.verify {
            "all pinned files are in place, checked byte for byte"
        } else {
            "all pinned files are in place by size and time"
        }),
    );
    let pins = outcome.pins.clone();
    Ok(Report::dry(project.root.clone(), pins, outcome))
}

pub(super) async fn lock(context: &Context, args: args::Lock) -> Result<Report, Error> {
    let project = find(&context.output)?;
    // a check or a dry run changes nothing, so it needs no hold. anything else reads what it
    // changes only once no other fetchloom can change it
    let _held = if args.check || args.dry.dry_run {
        None
    } else {
        Some(hold(context, &project, "lock").await?)
    };
    let (_, manifest) = project.manifest()?;
    let old = project.lock()?;
    if args.check {
        return check(&project, &manifest, old.as_ref());
    }
    let upgrade = match args.upgrade {
        None => Upgrade::Nothing,
        Some(names) if names.is_empty() => Upgrade::All,
        Some(names) => {
            for name in &names {
                known(&manifest, name, "lock --upgrade")?;
            }
            Upgrade::Only(names.into_iter().collect::<BTreeSet<_>>())
        }
    };
    if args.dry.dry_run {
        return preview_lock(context, &project, &manifest, old, &upgrade).await;
    }
    let work = Loop::new(
        context,
        &project,
        Arc::new(net(context, &manifest)),
        open_cache()?,
    );
    let (lock, before) = work.relock(&manifest, old, &upgrade, None).await?;
    // a stop asked while the datasets were resolved leaves data.lock as it was
    context.stop.check()?;
    context.stop.finish();
    let changed = work.write_lock(&before, &lock)?.is_some();
    work.drop_unpacked(&lock);
    engine::tell_upgrades(
        &context.output,
        &before,
        &lock,
        "Upgraded",
        &std::collections::HashSet::new(),
    );
    let pins = engine::repinned(&before, &lock);
    if changed {
        context.output.done("Locked", "data.toml into data.lock");
        context.output.next(&format!(
            "run `fetchloom sync` to update {}/, then commit {}",
            manifest.data_dir,
            committed(&project)
        ));
    } else {
        context
            .output
            .done("Ok", "data.lock is already up to date, nothing to do");
    }
    Ok(Report {
        project: project.root,
        datasets: pins,
        lock_changed: changed,
        placement: None,
        ..Report::default()
    })
}

/// `lock --dry-run`: what locking would change, found without downloading, unpacking or
/// writing. a file no checksum pins is planned with the pin it had and listed as a download,
/// since only its bytes can tell whether it changed.
async fn preview_lock(
    context: &Context,
    project: &Project,
    manifest: &Manifest,
    old: Option<Lock>,
    upgrade: &Upgrade,
) -> Result<Report, Error> {
    let work = Loop::new(
        context,
        project,
        Arc::new(net(context, manifest)),
        peek_cache()?,
    );
    let dry = work.relock_dry(manifest, old, upgrade, None).await?;
    let output = &context.output;
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
    engine::tell_upgrades(
        output,
        &dry.before,
        &dry.lock,
        "Would upgrade",
        &dry.guessed,
    );
    let download: Vec<Download> = dry
        .jobs
        .iter()
        .map(|job| Download {
            path: job.label.clone(),
            size: job.fetch.expect.size,
        })
        .collect();
    for download in &download {
        let size = download
            .size
            .map_or_else(|| "size unknown".to_owned(), units::size);
        output.step("Would fetch", &format!("{} · {size}", download.path));
    }
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
        datasets: engine::repinned(&dry.before, &dry.lock),
        lock_changed: changed,
        placement: None,
        dry_run: true,
        changes: Some(Changes {
            download,
            unpack: dry.unpack,
            ..Changes::default()
        }),
    })
}

/// `lock --check`: compares without the network and without writing.
fn check(project: &Project, manifest: &Manifest, old: Option<&Lock>) -> Result<Report, Error> {
    let empty = Lock::default();
    let old = old.unwrap_or(&empty);
    let mut stale: Vec<String> = Vec::new();
    for (name, entry) in &manifest.datasets {
        match old.get(name) {
            None => stale.push(format!("{name} is not in data.lock yet")),
            Some(locked) if locked.spec != lock::spec(entry) => {
                stale.push(format!("{name} changed in data.toml"));
            }
            Some(_) => {}
        }
    }
    for dataset in &old.datasets {
        if !manifest.datasets.contains_key(&dataset.name) {
            stale.push(format!(
                "{} is in data.lock but no longer in data.toml",
                dataset.name
            ));
        }
    }
    if stale.is_empty() {
        return Ok(Report {
            project: project.root.clone(),
            ..Report::default()
        });
    }
    let mut error = Error::new(Kind::Outdated, "data.lock is out of date with data.toml");
    for line in stale {
        error = error.detail(line);
    }
    Err(error.help("run `fetchloom lock` and commit data.lock"))
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
    /// the datasets whose pins changed: pinned for the first time, pinned to other data or files,
    /// or no longer pinned.
    pub(super) pins: Vec<String>,
    /// what placing did, or would do.
    pub(super) placed: T,
}

/// the lock, the cache and the data folder brought in line, in that order, and the state left
/// recording data.toml and data.lock as `read` and as this run leaves them.
#[expect(
    clippy::too_many_arguments,
    reason = "each is one independent choice a command makes for the run"
)]
pub(super) async fn sync_all(
    work: &Loop<'_>,
    manifest: &Manifest,
    old: Option<Lock>,
    upgrade: &Upgrade,
    force: bool,
    verify: bool,
    given: Option<(String, Listing)>,
    read: Read<'_>,
) -> Result<Outcome<Placement>, Error> {
    let mut survey = Survey::read(work.project, &manifest.data_dir, work.context.threads);
    survey.check_doubted(&work.project.root, &work.cache)?;
    if verify {
        survey.verify(&work.project.root)?;
    }
    disk::audit(work.project, &survey, &work.cache)?;
    let seen = match read {
        Read::Settled(seen) => {
            if let Some(placement) = place::untouched(work.project, manifest, &survey, seen, true)?
            {
                // nothing is left to do, so a stop asked from here on, as by ctrl-c while the
                // last lines print, leaves the sync done
                work.context
                    .stop
                    .check()
                    .map_err(|error| work.before_placing(error))?;
                work.context.stop.finish();
                register(work);
                return Ok(Outcome {
                    lock_changed: false,
                    pins: Vec::new(),
                    placed: placement,
                });
            }
            seen
        }
        Read::Planned(seen) => seen,
    };
    let (lock, before) = work
        .relock(manifest, old, upgrade, given)
        .instrument(tracing::debug_span!("relock"))
        .await
        .map_err(|error| work.before_placing(error))?;
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
    let placement = work
        .place(manifest, &lock, plan, force, survey, synced)
        .await?;
    work.context.stop.finish();
    work.drop_unpacked(&lock);
    register(work);
    Ok(Outcome {
        lock_changed: changed,
        pins,
        placed: placement,
    })
}

/// what [`sync_all`] would do, told as it is found: the lock it would write when that changes,
/// the downloads and unpacking it would need, then what placing would change. it stops before
/// anything [`sync_all`] changes: no download, unpacking, lock, state or registration, and cached
/// objects are read without being sealed or marked. given `seen`, it takes the settled-sync
/// shortcut as [`sync_all`] does, so a settled project needs neither the cache nor the network.
pub(super) async fn preview_all(
    work: &Loop<'_>,
    manifest: &Manifest,
    old: Option<Lock>,
    force: bool,
    verify: bool,
    given: Option<(String, Listing)>,
    seen: Option<&Seen>,
) -> Result<Outcome<Preview>, Error> {
    let mut survey = Survey::read(work.project, &manifest.data_dir, work.context.threads);
    survey.check_doubted(&work.project.root, &work.cache)?;
    if verify {
        survey.verify(&work.project.root)?;
    }
    let damaged = work.damaged_objects(&survey)?;
    if let Some(seen) = seen
        && let Some(placement) = place::untouched(work.project, manifest, &survey, seen, false)?
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
        lock, before, jobs, ..
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
    let preview = work.preview(manifest, &lock, force, survey, jobs, &damaged)?;
    for download in &preview.changes.download {
        let size = download
            .size
            .map_or_else(|| "size unknown".to_owned(), units::size);
        output.step("Would fetch", &format!("{} · {size}", download.path));
    }
    for archive in &preview.changes.unpack {
        output.step("Would open", archive);
    }
    if !preview.changes.unpack.is_empty() {
        output.hint("an archive's files are known once it is unpacked, so they are not counted");
    }
    place::tell_preview(output, manifest, &preview);
    Ok(Outcome {
        lock_changed: changed,
        pins: engine::repinned(&before, &lock),
        placed: preview,
    })
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

/// registers the project with the cache. gc keeps what registered projects need, so a failure
/// here only risks a later download.
pub(super) fn register(work: &Loop<'_>) {
    if let Err(err) = work.cache.register(&work.project.root) {
        work.context.output.warn_with(
            &format!(
                "could not add this project to the cache's list of projects in {}: {err}. `fetchloom cache gc` may then delete files this project needs",
                work.cache.root().display()
            ),
            "check the cache folder's permissions, then run the command again",
        );
    }
}

/// the project around the current folder. a person working in a folder below it is told which
/// data.toml the command uses.
pub(super) fn find(output: &Output) -> Result<Project, Error> {
    let here =
        std::env::current_dir().map_err(|err| Error::io("cannot read the current folder", &err))?;
    let project = Project::find(&here).ok_or_else(|| match Project::repository(&here) {
        Some(repository) => Error::new(
            Kind::ProjectInvalid,
            format!(
                "there is no data.toml in this folder or any folder above it inside the git repository at {}",
                repository.display()
            ),
        )
        .help("run `fetchloom add <link>` to start a project here, or give -C the folder of a project outside this repository, as in `fetchloom -C <folder> <command>`"),
        None => Error::new(
            Kind::ProjectInvalid,
            "there is no data.toml in this folder or any folder above it",
        )
        .help("run `fetchloom add <link>` to start a project here"),
    })?;
    tracing::debug!(root = %project.root.display(), "found data.toml");
    if project.root != here {
        output.aside(&format!("using the project in {}", project.root.display()));
    }
    Ok(project)
}

/// takes the project lock, waiting while another fetchloom changes the project.
pub(super) enum Waited<T> {
    Held(Held),
    Ready(T),
}

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
            Hold::Held(held) => return Ok(Waited::Held(held)),
            Hold::Busy(holder) if !told => {
                let who = holder.as_deref().unwrap_or("another fetchloom");
                context
                    .output
                    .step("Waiting", &format!("for {who} working in this project"));
                told = true;
            }
            Hold::Owner(message) if !told_owner => {
                context.output.step("Waiting", &message);
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

/// refuses a dataset name data.toml does not have, suggesting the closest one as the `command`
/// that names it, such as `path` in `fetchloom path iris`.
pub(super) fn known(manifest: &Manifest, name: &str, command: &str) -> Result<(), Error> {
    if manifest.datasets.contains_key(name) {
        return Ok(());
    }
    let error = Error::new(
        Kind::Usage,
        format!("data.toml has no dataset called {name}"),
    );
    Err(
        match name::closest(name, manifest.datasets.keys().map(String::as_str)) {
            Some(best) => error.help(format!("did you mean `fetchloom {command} {best}`?")),
            None if manifest.datasets.is_empty() => {
                error.help("data.toml has no datasets yet. add one with `fetchloom add <link>`")
            }
            None => error.help(format!(
                "name one of {}, as in `fetchloom {command} {}`",
                manifest
                    .datasets
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", "),
                manifest.datasets.keys().next().map_or("", String::as_str)
            )),
        },
    )
}

/// the network, trusting every host data.toml names, since a person wrote them.
pub(super) fn net(context: &Context, manifest: &Manifest) -> Net {
    let net = context.net();
    net.trust(manifest.datasets.values().filter_map(|entry| {
        Reference::parse(&entry.reference)
            .ok()?
            .reference
            .written_host()
    }));
    net
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::cache::Cache;
    use crate::output::{Level, Output, Style};
    use crate::project::manifest;
    use crate::source::recorded::block_on;

    pub(crate) fn waiting_project() -> (tempfile::TempDir, Project, Context) {
        let dir = tempfile::tempdir().unwrap();
        let project = Project {
            root: dir.path().to_path_buf(),
        };
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
        }
    }

    pub(crate) fn listed(url: &str, files: Vec<crate::source::Remote>) -> Listing {
        Listing {
            within: Vec::new(),
            resolved: Reference::Url(url::Url::parse(url).unwrap()),
            title: None,
            license: None,
            doi: None,
            files,
            unavailable: Vec::new(),
            warnings: Vec::new(),
        }
    }

    pub(crate) fn pinned(name: &str, spec: String, files: Vec<lock::LockedFile>) -> lock::Locked {
        lock::Locked {
            name: name.to_owned(),
            reference: format!("https://example.org/{name}"),
            spec,
            resolved: format!("https://example.org/{name}"),
            title: None,
            license: None,
            doi: None,
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
        let project = Project {
            root: dir.path().to_path_buf(),
        };
        let context = offline_context();
        let cache = crate::cache::Cache::open(dir.path().join("cache")).unwrap();
        let manifest = Manifest::parse(
            "[datasets.a]\nref = \"https://example.org/a\"\npath = \"shared\"\n\n[datasets.b]\nref = \"https://example.org/b\"\npath = \"shared/inner\"\n",
        )
        .unwrap();
        let a = pinned(
            "a",
            lock::spec(&manifest.datasets["a"]),
            vec![pinned_file("a", "https://example.org/a", b"a")],
        );
        let old = Lock { datasets: vec![a] };
        std::fs::write(project.root.join(lock::FILE), old.render()).unwrap();
        let work = Loop::new(&context, &project, Arc::new(context.net()), cache);
        let given = (
            "b".to_owned(),
            listed(
                "https://example.org/b",
                vec![stated("b", "https://example.org/b", b"b")],
            ),
        );

        let error = block_on(sync_all(
            &work,
            &manifest,
            Some(old.clone()),
            &Upgrade::Nothing,
            false,
            false,
            Some(given),
            Read::Planned(&unread()),
        ))
        .unwrap_err();

        assert!(error.message.contains("would share files"), "{error:?}");
        assert_eq!(
            std::fs::read_to_string(project.root.join(lock::FILE)).unwrap(),
            old.render(),
            "data.lock changed though nothing could be placed"
        );
    }

    /// kaggle may send a competition file as a zip of itself, which data.lock then pins as
    /// `<name>.zip`. a listing that also holds a real `<name>.zip` must not pin both on one path,
    /// or no later run could read data.lock.
    #[test]
    fn two_files_on_one_path_are_refused_before_data_lock_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let project = Project {
            root: dir.path().to_path_buf(),
        };
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
            &Upgrade::Nothing,
            false,
            false,
            Some(given),
            Read::Planned(&unread()),
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

    /// a sync that finds its project settled has nothing left to do, so a stop asked once it
    /// found so, as by ctrl-c while it prints its last lines, leaves it done, while one asked
    /// before ends it as stopped.
    #[test]
    fn a_stop_once_a_settled_sync_found_nothing_to_do_leaves_it_done() {
        let dir = tempfile::tempdir().unwrap();
        let project = Project {
            root: dir.path().join("project"),
        };
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
        let synced = sync_all(
            &work,
            &manifest,
            None,
            &Upgrade::Nothing,
            false,
            false,
            None,
            Read::Planned(&unlocked),
        );
        assert!(super::super::block_on(&first, synced).is_ok());
        let (lock, text) = project.lock_and_text().unwrap().unwrap();
        let seen = Seen {
            manifest: digest::sha256(b""),
            lock: Some(digest::sha256(text.as_bytes())),
        };
        let survey = Survey::read(&project, &manifest.data_dir, first.threads);
        assert!(
            place::untouched(&project, &manifest, &survey, &seen, false)
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
                    &Upgrade::Nothing,
                    false,
                    false,
                    None,
                    Read::Settled(&seen),
                )
                .await;
                context.stop.ask();
                synced.map(|_| ())
            });
            stopped.push(synced.err().map(|error| error.kind));
        }
        assert_eq!(stopped, [None, Some(Kind::Interrupted)]);
    }
}
