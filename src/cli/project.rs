//! the project commands: add, remove, sync, lock, status and path. each one reads the project,
//! takes its lock when it changes something, and leaves the order of work to the sync loop.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tracing::Instrument as _;

use super::args::{self, Add, Remove};
use super::sync::disk::{self, Survey};
use super::sync::place::{self, Placement, Seen};
use super::sync::{self as engine, Loop, Upgrade};
use super::{Context, fetch, open_cache};
use crate::digest;
use crate::error::{Error, Kind};
use crate::name;
use crate::net::Net;
use crate::plan;
use crate::project::lock::{self, Lock};
use crate::project::manifest::{self, Manifest};
use crate::project::state::Synced;
use crate::project::{Held, Hold, Project};
use crate::source::{Listing, Parsed, Reference};

/// what a project command did, as `--json` prints it.
#[derive(Default, Serialize)]
pub(super) struct Report {
    project: PathBuf,
    /// datasets added, removed or upgraded.
    datasets: Vec<String>,
    lock_changed: bool,
    placement: Option<Placement>,
}

pub(super) async fn add(context: &Context, mut args: Add) -> Result<Report, Error> {
    let here =
        std::env::current_dir().map_err(|err| Error::io("cannot read the current folder", &err))?;
    let project = Project::find(&here).unwrap_or(Project { root: here });
    let _held = hold(context, &project, "add").await?;
    let existed = project.manifest_path().is_file();
    let text = if existed {
        project.manifest()?.0
    } else {
        String::new()
    };
    let manifest = Manifest::parse(&text)?;
    let net = Arc::new(net(context, &manifest));
    if crate::source::search::is_search(&args.link) {
        args.link = super::search::pick(context, net.as_ref(), &args.link, "add").await?;
    }
    let parsed = Reference::parse(&args.link)?;
    engine::tell_raw(&context.output, &args.link, &parsed.reference);
    parsed.reference.check_shareable()?;
    // no answer from the source changes whether the name --as gives is usable
    if let Some(asked) = args.name.as_deref() {
        usable(asked, &manifest)?;
    }
    if let Reference::Url(url) = &parsed.reference {
        net.typed(url);
    }
    let cache = open_cache()?;
    let mut listing = engine::resolve(&context.output, &parsed.reference, &net).await?;
    engine::tell_resolved(&context.output, &listing);
    let select = if args.filter.select.is_empty() {
        parsed.file.iter().cloned().collect()
    } else {
        args.filter.select.clone()
    };
    let reference = stored_reference(&args.link, &parsed);
    // patterns from the command line are checked before any byte moves, and no line of
    // data.toml is to blame for them
    let alone = Manifest::parse(&manifest::with_dataset(
        "",
        "new",
        &reference,
        &select,
        &args.filter.exclude,
        !args.no_extract,
    )?)?;
    let mut entry = alone.datasets["new"].clone();
    entry.lines.clear();
    engine::filter(&parsed.reference.to_string(), &entry, &listing)?;
    let old = project.lock()?;
    if args.name.is_none()
        && let Some(name) = present(&manifest, old.as_ref(), &entry, &listing)
    {
        let work = Loop::new(context, &project, net, cache);
        return again(&work, &manifest, old.as_ref(), &name, &entry, &listing).await;
    }
    if args.name.is_none() {
        learn_name(context, &net, &cache, &mut listing, !args.no_extract).await?;
    }
    let name = match args.name {
        Some(asked) => asked,
        None => choose_name(&listing, &parsed, &manifest)?,
    };
    let updated = manifest::with_dataset(
        &text,
        &name,
        &reference,
        &select,
        &args.filter.exclude,
        !args.no_extract,
    )?;
    let new_manifest = Manifest::parse(&updated)?;
    project.write(manifest::FILE, updated.as_bytes())?;
    let work = Loop::new(context, &project, net, cache);
    let synced = sync_all(
        &work,
        &new_manifest,
        old.as_ref(),
        &Upgrade::Nothing,
        false,
        Some((name.clone(), listing)),
        None,
    )
    .await;
    let (lock_changed, placement) = synced
        .map_err(|error| restore(&project, existed.then_some(text.as_str()), &name, error))?;
    if project.ignore_in_git(&new_manifest.data_dir)? {
        context.output.step(
            "Ignored",
            &format!(
                "{}/ in .gitignore, since its files come from data.lock",
                new_manifest.data_dir
            ),
        );
    }
    tell_added(&work, &new_manifest, &name)?;
    Ok(Report {
        project: project.root,
        datasets: vec![name],
        lock_changed,
        placement: Some(placement),
    })
}

/// puts data.toml back as it was before `add`, or removes it when `add` created it, so a dataset
/// that could not be fetched never stays behind.
fn restore(project: &Project, before: Option<&str>, name: &str, mut failure: Error) -> Error {
    let restored = match before {
        Some(text) => project.write(manifest::FILE, text.as_bytes()),
        None => std::fs::remove_file(project.manifest_path())
            .map_err(|err| Error::io("could not remove data.toml", &err)),
    };
    if let Err(err) = restored {
        failure = failure.detail(format!("could not restore data.toml: {err}"));
        if let Some(help) = err.help {
            failure = failure.detail(help);
        }
        failure = match before {
            Some(text) => failure
                .detail("restore data.toml from this backup text (decode it as a JSON string):")
                .detail(serde_json::Value::String(text.to_owned()).to_string()),
            None => failure.detail(format!(
                "remove the {name} dataset entry from data.toml, or remove data.toml if it holds nothing else"
            )),
        };
    }
    failure
}

/// the dataset in data.toml that already pins what was just resolved, under any spelling of its
/// reference.
fn present(
    manifest: &Manifest,
    old: Option<&Lock>,
    wanted: &manifest::Entry,
    listing: &Listing,
) -> Option<String> {
    let resolved = listing.resolved.to_string();
    manifest
        .datasets
        .iter()
        .filter(|(name, entry)| {
            entry.reference == wanted.reference
                || old
                    .and_then(|old| old.get(name))
                    .is_some_and(|locked| locked.resolved == resolved)
        })
        .min_by_key(|(_, entry)| {
            (
                entry.select != wanted.select
                    || entry.exclude != wanted.exclude
                    || entry.extract != wanted.extract,
                entry.reference != wanted.reference,
            )
        })
        .map(|(name, _)| name.clone())
}

/// `add` of a dataset data.toml already has. with the same settings nothing changes, so running
/// the same `add` twice is safe; other settings need a choice only the person can make.
async fn again(
    work: &Loop<'_>,
    manifest: &Manifest,
    old: Option<&Lock>,
    name: &str,
    entry: &manifest::Entry,
    listing: &Listing,
) -> Result<Report, Error> {
    let existing = &manifest.datasets[name];
    let differs = if existing.select != entry.select || existing.exclude != entry.exclude {
        Some(("other patterns", "the patterns of"))
    } else if existing.extract != entry.extract {
        Some(("another extract setting", "extract on"))
    } else {
        None
    };
    if let Some((differs, setting)) = differs {
        return Err(Error::new(
            Kind::Usage,
            format!(
                "{} is already in data.toml as {name}, with {differs}",
                listing.resolved
            ),
        )
        .help(format!(
            "change {setting} {name} in data.toml, or pass --as <name> to add it a second time"
        )));
    }
    let (lock_changed, placement) =
        sync_all(work, manifest, old, &Upgrade::Nothing, false, None, None).await?;
    let contents = if has_files(work, manifest, name)? {
        format!("its files are in {}", plan::folder(manifest, name))
    } else {
        "its archives contain no regular files".to_owned()
    };
    work.context
        .output
        .done("Ok", &format!("{name} is already in data.toml, {contents}"));
    Ok(Report {
        project: work.project.root.clone(),
        datasets: Vec::new(),
        lock_changed,
        placement: Some(placement),
    })
}

fn tell_added(work: &Loop<'_>, manifest: &Manifest, name: &str) -> Result<(), Error> {
    if has_files(work, manifest, name)? {
        let folder = plan::folder(manifest, name);
        work.context
            .output
            .done("Added", &format!("{name}, its files are in {folder}"));
        work.context
            .output
            .hint(&format!("scripts can find it with `fetchloom path {name}`"));
    } else {
        work.context.output.done(
            "Added",
            &format!("{name}, its archives contain no regular files"),
        );
    }
    work.context
        .output
        .next("commit data.toml and data.lock; anyone who clones runs `fetchloom sync`");
    Ok(())
}

fn has_files(work: &Loop<'_>, manifest: &Manifest, name: &str) -> Result<bool, Error> {
    let lock = work.project.lock()?.unwrap_or_default();
    Ok(plan::from_cache(manifest, &lock, &work.cache)?
        .targets
        .iter()
        .any(|target| target.dataset == name))
}

pub(super) async fn remove(context: &Context, args: Remove) -> Result<Report, Error> {
    let project = find()?;
    let _held = hold(context, &project, "remove").await?;
    let (text, manifest) = project.manifest()?;
    known(&manifest, &args.name, "remove")?;
    let updated = manifest::without_dataset(&text, &args.name)?;
    let new_manifest = Manifest::parse(&updated)?;
    let mut lock = project.lock()?.unwrap_or_default();
    lock.datasets.retain(|dataset| dataset.name != args.name);
    let cache = open_cache()?;
    let survey = Survey::read(&project, &new_manifest.data_dir, context.threads);
    let placing = place::Placing::new(&project, &new_manifest, &lock, &cache, false, survey)?;
    project.write(manifest::FILE, updated.as_bytes())?;
    project.write(lock::FILE, lock.render().as_bytes())?;
    let placement = placing.remove_only(&context.output).map_err(|error| {
        error.detail("data.toml and data.lock were updated; after fixing the error, `fetchloom sync` finishes the removal")
    })?;
    context.output.done("Removed", &args.name);
    context.output.next(
        "commit data.toml and data.lock; after pulling, other clones remove unchanged files with `fetchloom sync`",
    );
    Ok(Report {
        project: project.root,
        datasets: vec![args.name],
        lock_changed: true,
        placement: Some(placement),
    })
}

pub(super) async fn sync(context: &Context, args: args::Sync) -> Result<Report, Error> {
    let project = find()?;
    let _held = hold(context, &project, "sync").await?;
    let (text, manifest) = project.manifest()?;
    if args.locked {
        check(&project, &manifest, project.lock()?.as_ref()).map_err(|error| {
            error.help("run `fetchloom lock` and commit data.lock, or sync without --locked")
        })?;
    }
    let read = project.lock_and_text()?;
    let seen = Seen {
        manifest: digest::sha256(text.as_bytes()),
        lock: read
            .as_ref()
            .map(|(_, text)| digest::sha256(text.as_bytes())),
    };
    let old = read.map(|(lock, _)| lock);
    let work = Loop::new(
        context,
        &project,
        Arc::new(net(context, &manifest)),
        open_cache()?,
    );
    let (lock_changed, placement) = sync_all(
        &work,
        &manifest,
        old.as_ref(),
        &Upgrade::Nothing,
        args.force,
        None,
        Some(&seen),
    )
    .await?;
    if !lock_changed
        && placement.placed + placement.replaced + placement.removed == 0
        && placement.kept.is_empty()
    {
        context
            .output
            .done("Ok", "all pinned files are in place, nothing to do");
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
            context
                .output
                .next("commit data.toml and data.lock, so other clones use these pins");
        } else if let Some(name) = manifest.datasets.keys().next()
            && placement.placed + placement.replaced > 0
        {
            context.output.next(&format!(
                "`fetchloom path {name}` tells your scripts where {name} is"
            ));
        }
    }
    Ok(Report {
        project: project.root,
        lock_changed,
        placement: Some(placement),
        ..Report::default()
    })
}

pub(super) async fn lock(context: &Context, args: args::Lock) -> Result<Report, Error> {
    let project = find()?;
    // a check changes nothing, so it needs no hold. anything else reads what it changes only
    // once no other fetchloom can change it
    let _held = if args.check {
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
    let work = Loop::new(
        context,
        &project,
        Arc::new(net(context, &manifest)),
        open_cache()?,
    );
    let empty = Lock::default();
    let lock = work
        .relock(&manifest, old.as_ref().unwrap_or(&empty), &upgrade, None)
        .await?;
    let changed = work.write_lock(old.as_ref(), &lock)?;
    work.drop_unpacked(&lock);
    let moved = engine::tell_upgrades(&context.output, old.as_ref().unwrap_or(&empty), &lock);
    if changed {
        context.output.done("Locked", "data.toml into data.lock");
        context.output.next(&format!(
            "run `fetchloom sync` to update {}/, then commit data.toml and data.lock",
            manifest.data_dir
        ));
    } else {
        context
            .output
            .done("Ok", "data.lock is already up to date, nothing to do");
    }
    Ok(Report {
        project: project.root,
        datasets: moved,
        lock_changed: changed,
        placement: None,
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

pub(super) async fn path(context: &Context, args: args::Path) -> Result<PathBuf, Error> {
    let project = find()?;
    let (_, mut manifest) = project.manifest()?;
    let (name, file) = match args.target.split_once('/') {
        Some((name, file)) => (name, Some(file.trim_matches('/'))),
        None => (args.target.as_str(), None),
    };
    known(&manifest, name, "path")?;
    check_member(&project, &manifest, name, file)?;
    if !ready(context, &project, &manifest, name)? {
        let _held = hold(context, &project, "path").await?;
        // many jobs on a cluster may ask at once: the first syncs, and the rest find its work
        // done once they hold the lock, so they answer without syncing in turn
        manifest = project.manifest()?.1;
        known(&manifest, name, "path")?;
        check_member(&project, &manifest, name, file)?;
        if !ready(context, &project, &manifest, name)? {
            let work = Loop::new(
                context,
                &project,
                Arc::new(net(context, &manifest)),
                open_cache()?,
            );
            sync_all(
                &work,
                &manifest,
                project.lock()?.as_ref(),
                &Upgrade::Nothing,
                false,
                None,
                None,
            )
            .await?;
        }
    }
    let lock = project.lock()?.unwrap_or_default();
    let targets = plan::from_cache(&manifest, &lock, &open_cache()?)?.targets;
    if !targets.iter().any(|target| target.dataset == name) {
        return Err(Error::new(
            Kind::Usage,
            format!("{name} has no regular files after extraction"),
        )
        .help(format!(
            "set extract = false on {name} in data.toml, then run `fetchloom sync` to keep the packed archive"
        )));
    }
    let folder = plan::folder(&manifest, name);
    let relative = match file.filter(|file| !file.is_empty()) {
        None => folder,
        Some(file) => {
            let wanted = format!("{folder}/{file}");
            if !targets.iter().any(|target| {
                target.path == wanted || target.path.starts_with(&format!("{wanted}/"))
            }) {
                let inside: Vec<&str> = targets
                    .iter()
                    .filter_map(|target| target.path.strip_prefix(&format!("{folder}/")))
                    .collect();
                return Err(not_in(name, file, &inside));
            }
            wanted
        }
    };
    let path = project
        .root
        .join(relative.replace('/', std::path::MAIN_SEPARATOR_STR));
    context.output.answer(&path.display().to_string());
    Ok(path)
}

/// rejects a member only when the current lock and cached trees prove it cannot exist.
fn check_member(
    project: &Project,
    manifest: &Manifest,
    name: &str,
    file: Option<&str>,
) -> Result<(), Error> {
    let Some(file) = file.filter(|file| !file.is_empty()) else {
        return Ok(());
    };
    let Some(lock) = project.lock()? else {
        return Ok(());
    };
    let Some(locked) = lock.get(name) else {
        return Ok(());
    };
    if manifest
        .datasets
        .get(name)
        .is_none_or(|entry| locked.spec != lock::spec(entry))
    {
        return Ok(());
    }
    let planned = plan::from_cache(manifest, &lock, &open_cache()?)?;
    if planned.unknown.iter().any(|(dataset, _)| *dataset == name) {
        return Ok(());
    }
    let folder = plan::folder(manifest, name);
    let wanted = format!("{folder}/{file}");
    if planned.targets.iter().any(|target| {
        target.dataset == name
            && (target.path == wanted || target.path.starts_with(&format!("{wanted}/")))
    }) {
        return Ok(());
    }
    let inside: Vec<&str> = planned
        .targets
        .iter()
        .filter(|target| target.dataset == name)
        .filter_map(|target| target.path.strip_prefix(&format!("{folder}/")))
        .collect();
    // a dataset with no files at all gets the fuller answer that explains why
    if inside.is_empty() {
        return Ok(());
    }
    Err(not_in(name, file, &inside))
}

/// a path `fetchloom path` did not find in dataset `name`, with the paths `inside` it nearest to
/// what was asked.
fn not_in(name: &str, file: &str, inside: &[&str]) -> Error {
    let error = Error::new(
        Kind::Usage,
        format!("{name} has no file or folder called {file}"),
    );
    if let Some(best) = name::closest(file, inside.iter().copied()) {
        return error.help(format!("did you mean `fetchloom path {name}/{best}`?"));
    }
    let near = name::nearest(file, inside.iter().copied(), 3);
    let shown = match near.as_slice() {
        [] => return error.help(format!("{name} holds no files")),
        [only] => (*only).to_owned(),
        [first @ .., last] => format!("{} and {last}", first.join(", ")),
    };
    let holds = if inside.len() > near.len() {
        let files = crate::units::count(inside.len() as u64, "file", "files");
        format!("{name} holds {files}, such as {shown}")
    } else {
        format!("{name} holds {shown}")
    };
    error.detail(holds).help(format!(
        "name a path it holds, such as `fetchloom path {name}/{}`",
        near[0]
    ))
}

/// whether a dataset is locked as data.toml asks and every file of it sits unchanged where it
/// belongs, so `path` can answer without the project lock.
fn ready(
    context: &Context,
    project: &Project,
    manifest: &Manifest,
    name: &str,
) -> Result<bool, Error> {
    let Some(lock) = project.lock()? else {
        return Ok(false);
    };
    let fresh = lock.get(name).is_some_and(|locked| {
        manifest
            .datasets
            .get(name)
            .is_some_and(|entry| locked.spec == lock::spec(entry))
    });
    if !fresh {
        return Ok(false);
    }
    let survey = Survey::read(project, &manifest.data_dir, context.threads);
    let cache = open_cache()?;
    // a file a mark puts in doubt waits for a sync, which hashes it once and records it
    let doubted = survey.doubted(&cache);
    let planned = plan::from_cache(manifest, &lock, &cache)?;
    if planned.unknown.iter().any(|(dataset, _)| *dataset == name) {
        return Ok(false);
    }
    Ok(planned
        .targets
        .iter()
        .filter(|target| target.dataset == name)
        .all(|target| {
            !doubted.contains(&target.path)
                && survey.state.files.get(&target.path).is_some_and(|placed| {
                    placed.sha256 == target.sha256
                        && survey.disk.get(&target.path) == Some(&placed.look)
                        && !survey.edited.contains(&target.path)
                        && (!target.copy || placed.writable)
                })
        }))
}

/// the lock, the cache and the data folder brought in line, in that order. given data.toml and
/// data.lock as `seen` before the run, a data folder still as a complete sync left them needs no
/// plan, and the state records them as this run leaves them.
async fn sync_all(
    work: &Loop<'_>,
    manifest: &Manifest,
    old: Option<&Lock>,
    upgrade: &Upgrade,
    force: bool,
    given: Option<(String, Listing)>,
    seen: Option<&Seen>,
) -> Result<(bool, Placement), Error> {
    let mut survey = Survey::read(work.project, &manifest.data_dir, work.context.threads);
    survey.look_again(&work.project.root, &work.cache, force)?;
    disk::audit(work.project, &survey, &work.cache)?;
    if let Some(seen) = seen
        && let Some(placement) = place::untouched(work.project, manifest, &survey, seen)?
    {
        register(work);
        return Ok((false, placement));
    }
    let empty = Lock::default();
    let lock = work
        .relock(manifest, old.unwrap_or(&empty), upgrade, given)
        .instrument(tracing::debug_span!("relock"))
        .await?;
    let changed = work.write_lock(old, &lock)?;
    let synced = seen.and_then(|seen| {
        let written = if changed {
            digest::sha256(lock.render().as_bytes())
        } else {
            seen.lock?
        };
        Some(Synced {
            manifest: seen.manifest,
            lock: written,
            complete: false,
        })
    });
    let placement = work.place(manifest, &lock, force, survey, synced).await?;
    work.drop_unpacked(&lock);
    register(work);
    Ok((changed, placement))
}

/// registers the project with the cache. gc keeps what registered projects need, so a failure
/// here only risks a later download.
fn register(work: &Loop<'_>) {
    if let Err(err) = work.cache.register(&work.project.root) {
        work.context.output.warn(&format!(
            "could not register this project with {}: {err}. gc may not keep this project's files; check the cache folder's permissions and run the command again",
            work.cache.root().display()
        ));
    }
}

/// the project around the current folder.
pub(super) fn find() -> Result<Project, Error> {
    let here =
        std::env::current_dir().map_err(|err| Error::io("cannot read the current folder", &err))?;
    Project::find(&here).ok_or_else(|| {
        Error::new(
            Kind::ProjectInvalid,
            "there is no data.toml in this folder or any folder above it",
        )
        .help("run `fetchloom add <link>` to start a project here")
    })
}

/// takes the project lock, waiting while another fetchloom changes the project.
async fn hold(context: &Context, project: &Project, command: &str) -> Result<Held, Error> {
    let mut told = false;
    let mut told_owner = false;
    loop {
        if context.stop.is_asked() {
            return Err(Error::new(
                Kind::Interrupted,
                "stopped while waiting for another fetchloom",
            ));
        }
        match project.hold(command)? {
            Hold::Held(held) => return Ok(held),
            Hold::Busy if !told => {
                context
                    .output
                    .step("Waiting", "for another fetchloom working in this project");
                told = true;
            }
            Hold::Owner(message) if !told_owner => {
                context.output.step("Waiting", &message);
                told = true;
                told_owner = true;
            }
            Hold::Busy | Hold::Owner(_) => {}
        }
        tokio::select! {
            () = tokio::time::sleep(Duration::from_millis(200)) => {}
            () = context.stop.asked() => return Err(Error::new(Kind::Interrupted, "stopped while waiting for another fetchloom")),
        }
    }
}

/// refuses a dataset name data.toml does not have, suggesting the closest one as the `command`
/// that names it, such as `path` in `fetchloom path iris`.
fn known(manifest: &Manifest, name: &str, command: &str) -> Result<(), Error> {
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

/// downloads a lone file whose name only the server knows, so the dataset can be named after it.
async fn learn_name(
    context: &Context,
    net: &Arc<Net>,
    cache: &crate::cache::Cache,
    listing: &mut Listing,
    extract: bool,
) -> Result<(), Error> {
    let [remote] = listing.files.as_mut_slice() else {
        return Ok(());
    };
    if !remote.named_by_server {
        return Ok(());
    }

    let job = fetch::listed_job(remote, &remote.path, extract);
    let started = std::time::Instant::now();
    let fetched = fetch::all(context, net, cache, vec![job]).await?;
    if let Some(fetched) = fetched.first() {
        if let Some(given) = fetched.name.as_deref().and_then(name::sanitize) {
            remote.path = given;
        }
        if fetched.origin == crate::net::Origin::Network {
            let elapsed = crate::units::duration(started.elapsed());
            context.output.step(
                "Downloaded",
                &format!(
                    "{} · {} in {elapsed}",
                    remote.path,
                    crate::units::size(fetched.size)
                ),
            );
        }
        remote.sha256 = Some(fetched.digest);
        remote.size = Some(fetched.size);
        remote.named_by_server = false;
    }
    Ok(())
}

/// refuses a name that is not a valid dataset name or that data.toml already has, as `--as`
/// gives it.
fn usable(asked: &str, manifest: &Manifest) -> Result<(), Error> {
    if !name::is_dataset_name(asked) {
        let error = Error::new(
            Kind::Usage,
            format!("\"{asked}\" is not a valid dataset name"),
        );
        return Err(match name::dataset_name(asked) {
            Some(suggested) => error.help(format!(
                "use lowercase letters, digits, - and _, such as --as {suggested}"
            )),
            None => error.help("pass --as a name of lowercase letters, digits, - and _"),
        });
    }
    unused(asked, manifest)
}

/// the name a new dataset gets without `--as`: one made from its file or its title.
fn choose_name(listing: &Listing, parsed: &Parsed, manifest: &Manifest) -> Result<String, Error> {
    let single = match (listing.files.as_slice(), &parsed.file) {
        ([only], _) => Some(
            only.path
                .rsplit('/')
                .next()
                .unwrap_or(&only.path)
                .to_owned(),
        ),
        (_, Some(file)) => Some(file.rsplit('/').next().unwrap_or(file).to_owned()),
        _ => None,
    };
    let name = single
        .and_then(|file| name::dataset_name(&name::folder_for(&file)))
        .or_else(|| listing.title.as_deref().and_then(name::title_name))
        .or_else(|| name::reference_name(&parsed.reference.to_string()))
        .unwrap_or_else(|| "dataset".to_owned());
    // a clash with --as is refused, but a name fetchloom chose can simply take the next number
    if !manifest.datasets.contains_key(&name) {
        return Ok(name);
    }
    (2..100)
        .map(|number| format!("{name}-{number}"))
        .find(|numbered| !manifest.datasets.contains_key(numbered))
        .map_or_else(|| unused(&name, manifest).map(|()| name), Ok)
}

/// refuses a name data.toml already has.
fn unused(name: &str, manifest: &Manifest) -> Result<(), Error> {
    if manifest.datasets.contains_key(name) {
        return Err(Error::new(
            Kind::Usage,
            format!("data.toml already has a dataset called {name}"),
        )
        .help(format!(
            "give this one another name with --as <name>, or run `fetchloom remove {name}` first"
        )));
    }
    Ok(())
}
/// how data.toml records a reference: the canonical id where a source has one, and the link as
/// pasted for drive and dropbox, and the raw endpoint for plain file links.
fn stored_reference(typed: &str, parsed: &Parsed) -> String {
    match parsed.reference {
        Reference::Drive(_) | Reference::Dropbox(_) if typed.contains("://") => {
            typed.trim().to_owned()
        }
        _ => parsed.reference.to_string(),
    }
}

/// the network, trusting every host data.toml names, since a person wrote them.
fn net(context: &Context, manifest: &Manifest) -> Net {
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
mod tests {
    #[test]
    fn raw_references_are_written_instead_of_repository_pages() {
        let typed = "https://github.com/org/repo/blob/main/iris.csv";
        let parsed = crate::source::Reference::parse(typed).unwrap();
        assert_eq!(
            super::stored_reference(typed, &parsed),
            "https://raw.githubusercontent.com/org/repo/main/iris.csv"
        );
    }

    use super::*;
    use crate::output::{Level, Output, Style};
    use crate::source::recorded::block_on;

    fn waiting_project() -> (tempfile::TempDir, Project, Context) {
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
            assert!(
                tokio::time::timeout(Duration::from_millis(20), &mut waiting)
                    .await
                    .is_err()
            );
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
            assert!(
                tokio::time::timeout(Duration::from_millis(20), &mut waiting)
                    .await
                    .is_err()
            );
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
}
