//! `fetchloom path`: where a dataset, or a file or folder in it, is. it answers from data.lock and
//! the data folder when the files are in place, and syncs first when they are not.

use std::path::PathBuf;
use std::sync::Arc;

use serde::Serialize;

use super::args;
use super::project::{Read, Waited, find, hold_until, ignore_data, known, locked, sync_all};
use super::sync::disk::Survey;
use super::sync::place::Seen;
use super::sync::{Loop, Upgrade};
use super::{Context, open_cache};
use crate::cache::Cache;
use crate::digest;
use crate::error::{Error, Kind};
use crate::name;
use crate::plan;
use crate::project::Project;
use crate::project::lock::{self, Lock};
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
    let (name, file) = match args.target.split_once('/') {
        Some((name, file)) => (name, Some(file.trim_matches('/'))),
        None => (args.target.as_str(), None),
    };
    known(&manifest, name, "path")?;
    let lock = project.lock()?;
    let mut cache = None;
    let path = if let Some(path) = ready_path(
        context,
        &project,
        &manifest,
        lock.as_ref(),
        &mut cache,
        name,
        file,
    )? {
        path
    } else {
        drop(lock);
        drop(manifest);
        match hold_path(context, &project, &mut cache, name, file, before).await? {
            Waited::Ready(path) => path,
            Waited::Held(held) => {
                let (text, manifest) = project.manifest()?;
                known(&manifest, name, "path")?;
                let read = project.lock_and_text()?;
                if let Some(path) = ready_path(
                    context,
                    &project,
                    &manifest,
                    read.as_ref().map(|(lock, _)| lock),
                    &mut cache,
                    name,
                    file,
                )? {
                    path
                } else {
                    if args.locked {
                        locked(
                            &project,
                            &manifest,
                            read.as_ref().map(|(lock, _)| lock),
                            "run path without --locked",
                        )?;
                    }
                    let cache = match cache {
                        Some(cache) => cache,
                        None => open_cache()?,
                    };
                    let work = Loop::new(context, &project, Arc::new(context.net()), cache);
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
                        &Upgrade::Nothing,
                        false,
                        false,
                        None,
                        Read::Planned(&seen),
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
            "set extract = false on {name} in data.toml, then run `fetchloom sync` to keep the packed archive"
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
    let fresh = lock.get(name).is_some_and(|locked| {
        manifest
            .datasets
            .get(name)
            .is_some_and(|entry| locked.spec == lock::spec(entry))
    });
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
    let survey = Survey::read(project, &manifest.data_dir, context.threads);
    let doubted = survey.doubted(&project.root, cache)?;
    let ready = planned
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
    name: &str,
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
        known(&manifest, name, "path")?;
        let lock = project.lock()?;
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
    use crate::project::manifest;
    use crate::project::state::Synced;

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
                lock::spec(&manifest.datasets["a"]),
                vec![lock::LockedFile {
                    path: "x".to_owned(),
                    size: 4,
                    sha256: digest,
                    md5: None,
                    url: "https://example.org/a".to_owned(),
                    unpacks_to: None,
                }],
            )],
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
            },
        );
        state.synced = Some(Synced {
            manifest: digest::sha256(text.as_bytes()),
            lock: digest::sha256(lock.render().as_bytes()),
            complete: true,
        });
        state
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
            "a",
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
