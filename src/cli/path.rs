//! `fetchloom path`: where a dataset, or a file or folder in it, is. it answers from data.lock and
//! the data folder when the files are in place, and syncs that dataset alone first when they are
//! not. a recipe's result is placed as data.lock pins it, and never built here.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use serde::Serialize;

use super::args;
use super::project::{
    Run, Waited, find, hold_until, ignore_data, known_with_rest, locked, sync_all,
};
use super::sync::disk::{Survey, is_inside};
use super::sync::place::Seen;
use super::sync::{self, Loop, Scope};
use super::{Context, open_cache};
use crate::cache::Cache;
use crate::digest;
use crate::error::{Error, Kind};
use crate::name;
use crate::plan;
use crate::project::Project;
use crate::project::lock::Lock;
use crate::project::manifest::Manifest;
use crate::project::state::Look;

/// what `path --json` reports, as an object so it can grow.
#[derive(Serialize)]
pub(super) struct Report {
    /// the absolute path of the dataset, or of the file or folder in it.
    path: PathBuf,
}

pub(super) async fn path(context: &Context, args: args::Path) -> Result<Report, Error> {
    let project = find(&context.output)?;
    let before = state_stamp(&project);
    let (_, manifest) = project.manifest()?;
    named(&manifest, &args.target)?;
    let (name, file) = match args.target.split_once('/') {
        Some((name, file)) => (name, Some(file.trim_matches('/'))),
        None => (args.target.as_str(), None),
    };
    let rest = &args.target[name.len()..];
    answerable(&manifest, name, rest)?;
    let lock = pins(&project, &manifest, name)?;
    unbuilt(&manifest, lock.as_ref(), name)?;
    let mut cache = None;
    let scope = Scope::Only(BTreeSet::from([name.to_owned()]));
    let tell_stale = |manifest: &Manifest, lock: Option<&Lock>| {
        if let Some(lock) = lock {
            sync::tell_stale(&context.output, &project.root, manifest, lock, &scope);
        }
    };
    let path = if let Some(path) = ready_path(
        context,
        &project,
        &manifest,
        lock.as_ref(),
        &mut cache,
        name,
        file,
    )? {
        tell_stale(&manifest, lock.as_ref());
        path
    } else {
        drop(lock);
        drop(manifest);
        match hold_path(context, &project, &mut cache, (name, rest), file, before).await? {
            Waited::Ready(path) => {
                tell_stale_again(context, &project, name, &scope)?;
                path
            }
            Waited::Held(held) => {
                let (text, manifest) = project.manifest()?;
                answerable(&manifest, name, rest)?;
                let read = project.lock_and_text()?;
                unbuilt(&manifest, read.as_ref().map(|(lock, _)| lock), name)?;
                if let Some(path) = ready_path(
                    context,
                    &project,
                    &manifest,
                    read.as_ref().map(|(lock, _)| lock),
                    &mut cache,
                    name,
                    file,
                )? {
                    tell_stale(&manifest, read.as_ref().map(|(lock, _)| lock));
                    path
                } else {
                    if args.locked {
                        locked(
                            &project,
                            &manifest,
                            read.as_ref().map(|(lock, _)| lock),
                            "run path without --locked",
                            &scope,
                        )?;
                    }
                    let cache = match cache {
                        Some(cache) => cache,
                        None => open_cache()?,
                    };
                    let work = Loop::new(context, &project, Arc::new(context.net()), cache)
                        .within(scope.clone());
                    let seen = Seen {
                        manifest: digest::sha256(text.as_bytes()),
                        lock: read
                            .as_ref()
                            .map(|(_, text)| digest::sha256(text.as_bytes())),
                    };
                    sync_all(
                        &work,
                        &manifest,
                        read.map(|(lock, _)| lock),
                        None,
                        Run::planned(&seen),
                    )
                    .await?;
                    ignore_data(&work, &manifest)?;
                    drop(held);
                    let lock = project.lock()?.unwrap_or_default();
                    let planned = plan::from_cache(&manifest, &lock, &work.cache)?;
                    planned_path(&project, &manifest, &planned, name, file)?
                }
            }
        }
    };
    context.output.answer(&path.display().to_string());
    context.output.detail(
        "a placed file is a link to its file in the cache folder, or a copy of it; `fetchloom cache` shows the cache",
    );
    Ok(Report { path })
}

/// refuses a target that names no dataset before it is split into a name and a path in it: an
/// empty one, and a path on this machine, which its first `/` would leave with no name at all.
fn named(manifest: &Manifest, target: &str) -> Result<(), Error> {
    let message = if target.is_empty() {
        "\"\" names no dataset".to_owned()
    } else if target.starts_with(['/', '\\']) || std::path::Path::new(target).is_absolute() {
        format!("\"{target}\" is a path on this machine, not a dataset")
    } else {
        return Ok(());
    };
    let example = manifest
        .datasets
        .keys()
        .next()
        .map_or("iris", String::as_str);
    Err(Error::new(Kind::Usage, message).help(format!(
        "name a dataset, with a file or folder in it after a / if you like, as in `{}` or `{}`",
        super::project::command(&format!("path {example}")),
        super::project::command(&format!("path {example}/<file>"))
    )))
}

/// refuses a dataset data.toml does not have, as [`known_with_rest`] does, and one it keeps out
/// of the data folder with `place = false`, which has no folder there. `rest` is what was typed
/// after the name, such as `/iris.csv`.
fn answerable(manifest: &Manifest, name: &str, rest: &str) -> Result<(), Error> {
    known_with_rest(manifest, name, rest, "path")?;
    let entry = &manifest.datasets[name];
    if entry.place {
        return Ok(());
    }
    let error = Error::new(
        Kind::Usage,
        format!(
            "{name} has place = false, so it has no folder in {}/",
            manifest.data_dir
        ),
    );
    let error = match entry.lines.get("place") {
        Some(line) => error.detail(line.clone()),
        None => error,
    };
    Err(error.help(format!(
        "set place = true on datasets.{name} in data.toml, then run `{}`",
        super::project::command(&format!("sync {name}"))
    )))
}

/// data.lock with only the pins of [`needed`] for `name`, or none when the project has none yet.
fn pins(project: &Project, manifest: &Manifest, name: &str) -> Result<Option<Lock>, Error> {
    let needed = needed(manifest, name);
    Ok(project
        .lock_only(&|other| needed.contains(other))?
        .map(|(lock, _)| lock))
}

/// the datasets path reads from data.lock to answer for `name`: it, and those whose folders are
/// its own, hold it or lie in it, so planning them finds every clash with its files, with the
/// datasets the recipes among them read.
fn needed(manifest: &Manifest, name: &str) -> BTreeSet<String> {
    let folder = crate::name::fold(&plan::folder(manifest, name));
    let touching = manifest
        .datasets
        .keys()
        .filter(|other| {
            let theirs = crate::name::fold(&plan::folder(manifest, other));
            is_inside(&theirs, &folder) || is_inside(&folder, &theirs)
        })
        .cloned()
        .collect();
    sync::with_inputs(manifest, touching)
}

/// warns about the recipe `name` as [`sync::tell_stale`] does, once another run made it ready,
/// from data.toml and data.lock as that run left them.
fn tell_stale_again(
    context: &Context,
    project: &Project,
    name: &str,
    scope: &Scope,
) -> Result<(), Error> {
    let (_, manifest) = project.manifest()?;
    if manifest
        .datasets
        .get(name)
        .is_some_and(|entry| entry.recipe().is_some())
    {
        let lock = project.lock()?.unwrap_or_default();
        sync::tell_stale(&context.output, &project.root, &manifest, &lock, scope);
    }
    Ok(())
}

/// refuses a recipe data.lock pins no result of: only `fetchloom build` makes one, so it has no
/// folder in the data folder yet.
fn unbuilt(manifest: &Manifest, lock: Option<&Lock>, name: &str) -> Result<(), Error> {
    if manifest.datasets[name].recipe().is_none()
        || lock.is_some_and(|lock| lock.result(name).is_some())
    {
        return Ok(());
    }
    Err(Error::new(
        Kind::BuildNeeded,
        format!(
            "{name} is made by a recipe, and its result is not in {}/ yet",
            manifest.data_dir
        ),
    )
    .help(format!(
        "run `{}`, which makes it",
        super::project::command(&format!("build {name}"))
    )))
}

/// validates the answer against the same plan used to check readiness.
fn planned_path(
    project: &Project,
    manifest: &Manifest,
    planned: &plan::Plan<'_>,
    name: &str,
    file: Option<&str>,
) -> Result<PathBuf, Error> {
    if !planned.targets.iter().any(|target| target.dataset == name) {
        return Err(Error::new(
            Kind::Usage,
            format!("{name} has no regular files after extraction"),
        )
        .help(format!(
            "set extract = false on {name} in data.toml, then run `{}` to keep the packed archive",
            super::project::command("sync")
        )));
    }
    let folder = plan::folder(manifest, name);
    let relative = match file.filter(|file| !file.is_empty()) {
        None => folder,
        Some(file) => {
            let wanted = format!("{folder}/{file}");
            let prefix = format!("{wanted}/");
            if !planned.targets.iter().any(|target| {
                target.dataset == name
                    && (target.path == wanted || target.path.starts_with(&prefix))
            }) {
                let prefix = format!("{folder}/");
                let inside: Vec<&str> = planned
                    .targets
                    .iter()
                    .filter(|target| target.dataset == name)
                    .filter_map(|target| target.path.strip_prefix(&prefix))
                    .collect();
                return Err(not_in(name, file, &inside));
            }
            wanted
        }
    };
    Ok(project
        .root
        .join(relative.replace('/', std::path::MAIN_SEPARATOR_STR)))
}

/// a path `fetchloom path` did not find in dataset `name`, with the paths `inside` it nearest to
/// what was asked.
fn not_in(name: &str, file: &str, inside: &[&str]) -> Error {
    let error = Error::new(
        Kind::Usage,
        format!("{name} has no file or folder called {file}"),
    );
    let typed = format!("{name}/{file}");
    let shown = |near: &str| {
        let meant = format!("{name}/{near}");
        super::meant(&[&typed], &[&meant], format!("fetchloom path {meant}"))
    };
    if let Some(best) = name::closest(file, inside.iter().copied()) {
        return error.help(format!("did you mean `{}`?", shown(best)));
    }
    let near = name::nearest(file, inside.iter().copied(), 3);
    if near.is_empty() {
        return error.help(format!("{name} holds no files"));
    }
    let listed = crate::units::listed(&near, "and");
    let holds = if inside.len() > near.len() {
        let files = crate::units::count(inside.len() as u64, "file", "files");
        format!("{name} holds {files}, such as {listed}")
    } else {
        format!("{name} holds {listed}")
    };
    error.detail(holds).help(format!(
        "name a path it holds, such as `{}`",
        shown(near[0])
    ))
}

/// a ready answer from one lock snapshot and one plan, without a mutation hold.
fn ready_path(
    context: &Context,
    project: &Project,
    manifest: &Manifest,
    lock: Option<&Lock>,
    cache: &mut Option<Cache>,
    name: &str,
    file: Option<&str>,
) -> Result<Option<PathBuf>, Error> {
    let Some(lock) = lock else {
        return Ok(None);
    };
    // a recipe's pin stands until a build, which alone makes another
    let fresh = match manifest.datasets.get(name) {
        Some(entry) if entry.recipe().is_some() => lock.result(name).is_some(),
        Some(entry) => lock
            .get(name)
            .is_some_and(|locked| locked.drift(entry).is_none()),
        None => false,
    };
    if !fresh {
        return Ok(None);
    }
    let cache = match cache {
        Some(cache) => cache,
        slot @ None => slot.insert(open_cache()?),
    };
    let planned = plan::from_cache(manifest, lock, cache)?;
    if planned.unknown.iter().any(|(dataset, _)| *dataset == name) {
        return Ok(None);
    }
    // reject a known impossible member before a missing file can trigger downloads.
    let answer = planned_path(project, manifest, &planned, name, file)?;
    let survey = Survey::read_within(
        project,
        &manifest.data_dir,
        &[plan::folder(manifest, name)],
        context.threads,
        &context.stop,
    )?;
    let doubted = survey.doubted(&project.root, cache)?;
    let ready = planned
        .targets
        .iter()
        .filter(|target| target.dataset == name)
        .all(|target| {
            let placed = survey.state.files.get(&target.path);
            let ready = !doubted.contains(&target.path)
                && placed.is_some_and(|placed| {
                    placed.sha256 == target.sha256
                        && survey.disk.get(&target.path) == Some(&placed.look)
                        && !survey.edited.contains(&target.path)
                        && (!target.copy || placed.writable)
                });
            if !ready {
                tracing::debug!(
                    path = %target.path,
                    recorded = ?placed.map(|placed| placed.look),
                    found = ?survey.disk.get(&target.path),
                    "not ready, so path syncs"
                );
            }
            ready
        });
    Ok(ready.then_some(answer))
}

fn state_stamp(project: &Project) -> Option<Look> {
    let metadata = std::fs::symlink_metadata(project.own_folder().join("state")).ok()?;
    metadata.is_file().then(|| Look::of(&metadata))
}

async fn hold_path(
    context: &Context,
    project: &Project,
    cache: &mut Option<Cache>,
    (name, rest): (&str, &str),
    file: Option<&str>,
    mut before: Option<Look>,
) -> Result<Waited<PathBuf>, Error> {
    hold_until(context, project, "path", || {
        let current = state_stamp(project);
        if current == before {
            return Ok(None);
        }
        before = current;
        let (_, manifest) = project.manifest()?;
        answerable(&manifest, name, rest)?;
        let lock = pins(project, &manifest, name)?;
        ready_path(
            context,
            project,
            &manifest,
            lock.as_ref(),
            cache,
            name,
            file,
        )
    })
    .await
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::Duration;

    use super::*;
    use crate::cli::project::tests::{pinned, waiting_project};
    use crate::project::state::Synced;
    use crate::project::{lock, manifest};

    fn waiter_state(
        project: &Project,
        stamp: std::time::SystemTime,
    ) -> crate::project::state::State {
        use crate::project::state::{Placed, State};
        let text = "[datasets.a]\nref = 'https://example.org/a'\n";
        project.write(manifest::FILE, text.as_bytes()).unwrap();
        let manifest = Manifest::parse(text).unwrap();
        let digest = digest::sha256(b"data");
        let lock = Lock {
            datasets: vec![pinned(
                "a",
                lock::spec(manifest.datasets["a"].source().unwrap()),
                vec![lock::LockedFile {
                    path: "x".to_owned(),
                    size: 4,
                    sha256: digest,
                    md5: None,
                    url: "https://example.org/a".to_owned(),
                    unpacks_to: None,
                }],
            )],
            ..Lock::default()
        };
        project.write(lock::FILE, lock.render().as_bytes()).unwrap();
        let mut state = State::default();
        state.files.insert(
            "data/a/x".to_owned(),
            Placed {
                dataset: "a".to_owned(),
                sha256: digest,
                method: crate::place::Method::Copy,
                look: Look {
                    size: 4,
                    modified: crate::project::state::nanos(stamp),
                },
                writable: true,
                checked: 1,
                adopted: false,
                racy_intact: std::sync::OnceLock::new(),
            },
        );
        state.synced = Some(Synced {
            manifest: digest::sha256(text.as_bytes()),
            lock: digest::sha256(lock.render().as_bytes()),
            complete: true,
        });
        state
    }

    /// path reads the pins of its dataset, of those whose folders meet its folder, which a clash
    /// could involve, and of what the recipes among them read: never the rest.
    #[test]
    fn path_reads_only_the_pins_its_dataset_needs() {
        let text = concat!(
            "[datasets.a]\nref = 'https://example.org/a'\n\n",
            "[datasets.inner]\nref = 'https://example.org/inner'\npath = 'A/inner'\n\n",
            "[datasets.ab]\nref = 'https://example.org/ab'\n\n",
            "[datasets.raw]\nref = 'https://example.org/raw'\nplace = false\n\n",
            "[datasets.clean]\nfrom = ['raw']\nrun = ['t', '{raw}', '{out}']\n\n",
            "[datasets.tidy]\nfrom = ['clean']\nrun = ['t', '{clean}', '{out}']\n",
        );
        let manifest = Manifest::parse(text).unwrap();
        assert_eq!(needed_in(&manifest, "a"), ["a", "inner"]);
        assert_eq!(needed_in(&manifest, "inner"), ["a", "inner"]);
        assert_eq!(needed_in(&manifest, "ab"), ["ab"]);
        assert_eq!(needed_in(&manifest, "tidy"), ["clean", "raw", "tidy"]);
        let whole = Manifest::parse(&format!(
            "{text}\n[datasets.all]\nref = 'https://example.org/all'\npath = '.'\n"
        ))
        .unwrap();
        assert_eq!(needed_in(&whole, "ab"), ["ab", "all"]);
        assert_eq!(needed_in(&whole, "all").len(), 7);
    }

    fn needed_in(manifest: &Manifest, name: &str) -> Vec<String> {
        needed(manifest, name).into_iter().collect()
    }

    #[tokio::test(start_paused = true)]
    async fn path_waiters_recheck_changed_state_without_claiming_the_owner() {
        let (dir, project, context) = waiting_project();
        let owner = project.own_folder().join("owner");
        let owner_before = std::fs::read(&owner).unwrap();
        let stamp = std::time::UNIX_EPOCH + Duration::from_secs(1_600_000_000);
        let state = waiter_state(&project, stamp);
        let mut cache = Some(Cache::open(dir.path().join("cache")).unwrap());
        let mut waiting = Box::pin(hold_path(
            &context,
            &project,
            &mut cache,
            ("a", "/x"),
            Some("x"),
            None,
        ));
        std::future::poll_fn(|task| {
            assert!(std::future::Future::poll(waiting.as_mut(), task).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        let state_path = project.own_folder().join("state");
        project.write_state(&state).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&state_path)
            .unwrap()
            .set_modified(stamp + Duration::from_secs(4))
            .unwrap();
        tokio::time::advance(Duration::from_millis(200)).await;
        std::future::poll_fn(|task| {
            assert!(
                std::future::Future::poll(waiting.as_mut(), task).is_pending(),
                "state alone cannot vouch for a missing file"
            );
            std::task::Poll::Ready(())
        })
        .await;
        let target = project.root.join("data/a/x");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, b"data").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&target)
            .unwrap()
            .set_modified(stamp)
            .unwrap();
        project.write_state(&state).unwrap();
        // whole even seconds apart, so a clock that keeps two second steps tells the writes apart
        std::fs::File::options()
            .write(true)
            .open(&state_path)
            .unwrap()
            .set_modified(stamp + Duration::from_secs(8))
            .unwrap();
        tokio::time::advance(Duration::from_millis(200)).await;
        // a waiter that never sees the project ready fails here, saying what it saw, rather than
        // waiting for ever
        let waited = tokio::time::timeout(Duration::from_secs(60), waiting).await;
        let look = |path: &Path| std::fs::metadata(path).map(|metadata| Look::of(&metadata));
        let Ok(Ok(Waited::Ready(answer))) = waited else {
            panic!(
                "the waiter never answered: state {:?}, file {:?}, recorded {:?}",
                look(&state_path),
                look(&target),
                state.files.get("data/a/x").map(|placed| placed.look),
            );
        };
        assert_eq!(answer, target);
        assert_eq!(std::fs::read(owner).unwrap(), owner_before);
    }
}
