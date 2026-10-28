//! the project commands: add, remove, sync, lock, status and path. each one reads the project,
//! takes its lock when it changes something, and leaves the order of work to the sync loop.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;

use super::args::{self, Add, Remove};
use super::sync::place::{self, Placement};
use super::sync::{self as engine, Loop, Upgrade};
use super::{Context, fetch, open_cache};
use crate::error::{Error, Kind};
use crate::name;
use crate::net::{Expect, Fetch, Net};
use crate::plan;
use crate::project::lock::{self, Lock};
use crate::project::manifest::{self, Manifest};
use crate::project::{Held, Hold, Project};
use crate::source::{self, Listing, Parsed, Reference};

/// what a project command did, as `--json` prints it.
#[derive(Default, Serialize)]
pub(super) struct Report {
    project: PathBuf,
    /// datasets added, removed or upgraded.
    datasets: Vec<String>,
    lock_changed: bool,
    placement: Option<Placement>,
}

pub(super) async fn add(context: &Context, args: Add) -> Result<Report, Error> {
    let here = std::env::current_dir().map_err(|err| {
        Error::new(
            Kind::LocalIo,
            format!("cannot read the current folder: {err}"),
        )
    })?;
    let project = Project::find(&here).unwrap_or(Project { root: here });
    let _held = hold(context, &project).await?;
    let existed = project.manifest_path().is_file();
    let text = if existed {
        project.manifest()?.0
    } else {
        String::new()
    };
    let manifest = Manifest::parse(&text)?;
    let parsed = Reference::parse(&args.link)?;
    let net = Arc::new(net(context, &manifest)?);
    if let Reference::Url(url) = &parsed.reference {
        net.typed(url);
    }
    let cache = open_cache()?;
    let mut listing = parsed.reference.resolve(&*net).await?;
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
    )?)?;
    let mut entry = alone.datasets["new"].clone();
    entry.lines.clear();
    engine::filter(&parsed.reference.to_string(), &entry, &listing)?;
    let old = project.lock()?;
    if args.name.is_none()
        && let Some(name) = present(&manifest, old.as_ref(), &reference, &listing)
    {
        let work = Loop::new(context, &project, net, cache);
        return again(&work, &manifest, old.as_ref(), &name, &entry, &listing).await;
    }
    if args.name.is_none() {
        learn_name(context, &net, &cache, &mut listing).await?;
    }
    let name = choose_name(args.name.as_deref(), &listing, &parsed, &manifest)?;
    let updated = manifest::with_dataset(&text, &name, &reference, &select, &args.filter.exclude)?;
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
    )
    .await;
    let (lock_changed, placement) =
        synced.inspect_err(|_| restore(&project, existed.then_some(text.as_str())))?;
    if project.ignore_in_git(&new_manifest.data_dir)? {
        context.output.step(
            "Ignored",
            &format!(
                "{}/ in .gitignore, since its files come from data.lock",
                new_manifest.data_dir
            ),
        );
    }
    let folder = plan::folder(&new_manifest, &name);
    context
        .output
        .done(&format!("added {name}. its files are in {folder}"));
    context
        .output
        .hint(&format!("scripts can find it with `fetchloom path {name}`"));
    Ok(Report {
        project: project.root,
        datasets: vec![name],
        lock_changed,
        placement: Some(placement),
    })
}

/// puts data.toml back as it was before `add`, or removes it when `add` created it, so a dataset
/// that could not be fetched never stays behind.
fn restore(project: &Project, before: Option<&str>) {
    let restored = match before {
        Some(text) => project.write(manifest::FILE, text.as_bytes()),
        None => std::fs::remove_file(project.manifest_path())
            .map_err(|err| Error::new(Kind::LocalIo, err.to_string())),
    };
    if let Err(err) = restored {
        tracing::debug!(%err, "could not restore data.toml");
    }
}

/// the dataset in data.toml that already pins what was just resolved, under any spelling of its
/// reference.
fn present(
    manifest: &Manifest,
    old: Option<&Lock>,
    reference: &str,
    listing: &Listing,
) -> Option<String> {
    let resolved = listing.resolved.to_string();
    manifest
        .datasets
        .iter()
        .find(|(name, entry)| {
            entry.reference == reference
                || old
                    .and_then(|old| old.get(name))
                    .is_some_and(|locked| locked.resolved == resolved)
        })
        .map(|(name, _)| name.clone())
}

/// `add` of a dataset data.toml already has. with the same patterns nothing changes, so running
/// the same `add` twice is safe; other patterns need a choice only the person can make.
async fn again(
    work: &Loop<'_>,
    manifest: &Manifest,
    old: Option<&Lock>,
    name: &str,
    entry: &manifest::Entry,
    listing: &Listing,
) -> Result<Report, Error> {
    let existing = &manifest.datasets[name];
    if existing.select != entry.select || existing.exclude != entry.exclude {
        return Err(Error::new(
            Kind::Usage,
            format!(
                "{} is already in data.toml as {name}, with other patterns",
                listing.resolved
            ),
        )
        .help(format!(
            "change the patterns of {name} in data.toml, or pass --as <name> to add it a second time"
        )));
    }
    let (lock_changed, placement) =
        sync_all(work, manifest, old, &Upgrade::Nothing, false, None).await?;
    work.context.output.done(&format!(
        "{name} is already in data.toml. its files are in {}",
        plan::folder(manifest, name)
    ));
    Ok(Report {
        project: work.project.root.clone(),
        datasets: Vec::new(),
        lock_changed,
        placement: Some(placement),
    })
}

pub(super) async fn remove(context: &Context, args: Remove) -> Result<Report, Error> {
    let project = find()?;
    let _held = hold(context, &project).await?;
    let (text, manifest) = project.manifest()?;
    known(&manifest, &args.name)?;
    let updated = manifest::without_dataset(&text, &args.name)?;
    let new_manifest = Manifest::parse(&updated)?;
    let mut lock = project.lock()?.unwrap_or_default();
    lock.datasets.retain(|dataset| dataset.name != args.name);
    project.write(manifest::FILE, updated.as_bytes())?;
    project.write(lock::FILE, lock.render().as_bytes())?;
    let placement = place::Placing::new(&project, &new_manifest, &lock, &open_cache()?, false)?
        .remove_only(&context.output)?;
    context.output.done(&format!("removed {}", args.name));
    Ok(Report {
        project: project.root,
        datasets: vec![args.name],
        lock_changed: true,
        placement: Some(placement),
    })
}

pub(super) async fn sync(context: &Context, args: args::Sync) -> Result<Report, Error> {
    let project = find()?;
    let _held = hold(context, &project).await?;
    let (_, manifest) = project.manifest()?;
    let old = project.lock()?;
    let work = Loop::new(
        context,
        &project,
        Arc::new(net(context, &manifest)?),
        open_cache()?,
    );
    let (lock_changed, placement) = sync_all(
        &work,
        &manifest,
        old.as_ref(),
        &Upgrade::Nothing,
        args.force,
        None,
    )
    .await?;
    if !lock_changed
        && placement.placed + placement.replaced + placement.removed == 0
        && placement.kept.is_empty()
    {
        context
            .output
            .done("everything already matches data.lock. nothing to do");
    } else {
        let datasets = units_datasets(manifest.datasets.len());
        context
            .output
            .done(&format!("synced {datasets} into {}", manifest.data_dir));
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
    let (_, manifest) = project.manifest()?;
    let old = project.lock()?;
    if args.check {
        return check(&project, &manifest, old.as_ref());
    }
    let _held = hold(context, &project).await?;
    let upgrade = match args.upgrade {
        None => Upgrade::Nothing,
        Some(names) if names.is_empty() => Upgrade::All,
        Some(names) => {
            for name in &names {
                known(&manifest, name)?;
            }
            Upgrade::Only(names.into_iter().collect::<BTreeSet<_>>())
        }
    };
    let work = Loop::new(
        context,
        &project,
        Arc::new(net(context, &manifest)?),
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
        context
            .output
            .done("data.lock is up to date with data.toml");
    } else {
        context
            .output
            .done("data.lock was already up to date. nothing to do");
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
    let (_, manifest) = project.manifest()?;
    let (name, file) = match args.target.split_once('/') {
        Some((name, file)) => (name, Some(file.trim_matches('/'))),
        None => (args.target.as_str(), None),
    };
    known(&manifest, name)?;
    if !ready(&project, &manifest, name)? {
        let _held = hold(context, &project).await?;
        let work = Loop::new(
            context,
            &project,
            Arc::new(net(context, &manifest)?),
            open_cache()?,
        );
        sync_all(
            &work,
            &manifest,
            project.lock()?.as_ref(),
            &Upgrade::Nothing,
            false,
            None,
        )
        .await?;
    }
    let folder = plan::folder(&manifest, name);
    let relative = match file.filter(|file| !file.is_empty()) {
        None => folder,
        Some(file) => {
            let lock = project.lock()?.unwrap_or_default();
            let wanted = format!("{folder}/{file}");
            let targets = plan::from_cache(&manifest, &lock, &open_cache()?)?.targets;
            if !targets.iter().any(|target| {
                target.path == wanted || target.path.starts_with(&format!("{wanted}/"))
            }) {
                let inside: Vec<&str> = targets
                    .iter()
                    .filter_map(|target| target.path.strip_prefix(&format!("{folder}/")))
                    .collect();
                let error = Error::new(
                    Kind::Usage,
                    format!("{name} has no file or folder called {file}"),
                );
                return Err(match name::closest(file, inside.iter().copied()) {
                    Some(best) => {
                        error.help(format!("did you mean `fetchloom path {name}/{best}`?"))
                    }
                    None => error.help(format!("`fetchloom status` lists what {name} holds")),
                });
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

/// whether a dataset is locked as data.toml asks and every file of it sits unchanged where it
/// belongs, so `path` can answer without the project lock.
fn ready(project: &Project, manifest: &Manifest, name: &str) -> Result<bool, Error> {
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
    let state = project.state();
    let disk = place::scan(&project.root, &manifest.data_dir);
    let planned = plan::from_cache(manifest, &lock, &open_cache()?)?;
    if planned.unknown.iter().any(|(dataset, _)| *dataset == name) {
        return Ok(false);
    }
    Ok(planned
        .targets
        .iter()
        .filter(|target| target.dataset == name)
        .all(|target| {
            state.files.get(&target.path).is_some_and(|placed| {
                placed.sha256 == target.sha256 && disk.get(&target.path) == Some(&placed.look)
            })
        }))
}

/// the lock, the cache and the data folder brought in line, in that order.
async fn sync_all(
    work: &Loop<'_>,
    manifest: &Manifest,
    old: Option<&Lock>,
    upgrade: &Upgrade,
    force: bool,
    given: Option<(String, Listing)>,
) -> Result<(bool, Placement), Error> {
    place::audit(work.project, manifest, &work.cache)?;
    let empty = Lock::default();
    let lock = work
        .relock(manifest, old.unwrap_or(&empty), upgrade, given)
        .await?;
    let changed = work.write_lock(old, &lock)?;
    let placement = work.place(manifest, &lock, force).await?;
    work.drop_unpacked(&lock);
    // gc keeps what registered projects need, so a failure here only risks a later download
    if let Err(err) = work.cache.register(&work.project.root) {
        tracing::debug!(%err, "could not register the project with the cache");
    }
    Ok((changed, placement))
}

/// the project around the current folder.
pub(super) fn find() -> Result<Project, Error> {
    let here = std::env::current_dir().map_err(|err| {
        Error::new(
            Kind::LocalIo,
            format!("cannot read the current folder: {err}"),
        )
    })?;
    Project::find(&here).ok_or_else(|| {
        Error::new(
            Kind::ProjectInvalid,
            "there is no data.toml in this folder or any folder above it",
        )
        .help("run `fetchloom add <link>` to start a project here")
    })
}

/// takes the project lock, waiting while another fetchloom changes the project.
async fn hold(context: &Context, project: &Project) -> Result<Held, Error> {
    let mut told = false;
    loop {
        match project.hold()? {
            Hold::Held(held) => return Ok(held),
            Hold::Busy if !told => {
                context
                    .output
                    .step("Waiting", "for another fetchloom working in this project");
                told = true;
            }
            Hold::Busy => {}
        }
        tokio::select! {
            () = tokio::time::sleep(Duration::from_millis(200)) => {}
            () = context.stop.asked() => return Err(Error::new(Kind::Interrupted, "stopped while waiting for another fetchloom")),
        }
    }
}

/// refuses a dataset name data.toml does not have, suggesting the closest one.
fn known(manifest: &Manifest, name: &str) -> Result<(), Error> {
    if manifest.datasets.contains_key(name) {
        return Ok(());
    }
    let error = Error::new(
        Kind::Usage,
        format!("data.toml has no dataset called {name}"),
    );
    Err(
        match name::closest(name, manifest.datasets.keys().map(String::as_str)) {
            Some(best) => error.help(format!("did you mean {best}?")),
            None if manifest.datasets.is_empty() => {
                error.help("data.toml has no datasets yet. add one with `fetchloom add <link>`")
            }
            None => error.help(format!(
                "the datasets are {}",
                manifest
                    .datasets
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
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
) -> Result<(), Error> {
    let [remote] = listing.files.as_mut_slice() else {
        return Ok(());
    };
    if !remote.named_by_server {
        return Ok(());
    }
    let (from, divert) = source::fetch_plan(&remote.url);
    let job = fetch::Job {
        fetch: Fetch {
            url: remote.url.clone(),
            from,
            divert,
            expect: Expect {
                sha256: remote.sha256,
                md5: remote.md5,
                size: remote.size,
            },
            revalidate: remote.sha256.is_none() && remote.md5.is_none(),
        },
        label: remote.path.clone(),
        dataset: None,
    };
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

/// the name a new dataset gets: the one asked for, else one made from its file or its title.
fn choose_name(
    asked: Option<&str>,
    listing: &Listing,
    parsed: &Parsed,
    manifest: &Manifest,
) -> Result<String, Error> {
    let name = match asked {
        Some(asked) if name::is_dataset_name(asked) => asked.to_owned(),
        Some(asked) => {
            let error = Error::new(
                Kind::Usage,
                format!("\"{asked}\" is not a valid dataset name"),
            );
            return Err(match name::dataset_name(asked) {
                Some(suggested) => error.help(format!(
                    "use lowercase letters, digits, - and _, such as --as {suggested}"
                )),
                None => error.help("use lowercase letters, digits, - and _"),
            });
        }
        None => {
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
            single
                .and_then(|file| name::dataset_name(&name::folder_for(&file)))
                .or_else(|| listing.title.as_deref().and_then(name::dataset_name))
                .or_else(|| name::dataset_name(&parsed.reference.to_string()))
                .unwrap_or_else(|| "dataset".to_owned())
        }
    };
    if manifest.datasets.contains_key(&name) {
        return Err(Error::new(
            Kind::Usage,
            format!("data.toml already has a dataset called {name}"),
        )
        .help(format!(
            "give this one another name with --as <name>, or run `fetchloom remove {name}` first"
        )));
    }
    Ok(name)
}

/// how data.toml records a reference: the canonical id where a source has one, and the link as
/// pasted for drive, dropbox and plain links, which people recognize by their link.
fn stored_reference(typed: &str, parsed: &Parsed) -> String {
    match parsed.reference {
        Reference::Url(_) | Reference::Drive(_) | Reference::Dropbox(_)
            if typed.contains("://") =>
        {
            typed.trim().to_owned()
        }
        _ => parsed.reference.to_string(),
    }
}

fn units_datasets(count: usize) -> String {
    crate::units::count(count as u64, "dataset", "datasets")
}

/// the network, trusting every host data.toml names, since a person wrote them.
fn net(context: &Context, manifest: &Manifest) -> Result<Net, Error> {
    let net = context.net()?;
    net.trust(manifest.datasets.values().filter_map(|entry| {
        Reference::parse(&entry.reference)
            .ok()?
            .reference
            .written_host()
    }));
    Ok(net)
}
