//! `fetchloom path`: where a dataset, or a file or folder in it, is, syncing that dataset alone
//! first when its files are not in place.

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
use super::sync::{Loop, Scope};
use super::{Context, open_cache};
use crate::cache::Cache;
use crate::digest;
use crate::error::{Error, Kind};
use crate::name;
use crate::plan;
use crate::project::lock::Lock;
use crate::project::manifest::Manifest;
use crate::project::state::Look;
use crate::project::{Project, native};

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
    let target = placed(&project, &manifest, &args.target).unwrap_or(args.target);
    named(&manifest, &target)?;
    let (name, file) = match target.split_once('/') {
        Some((name, file)) => (name, Some(file.trim_matches('/'))),
        None => (target.as_str(), None),
    };
    let rest = &target[name.len()..];
    known_with_rest(&manifest, name, rest, "path").map_err(|error| {
        match held_by(&project, &manifest, &target) {
            Some(help) => error.help(help),
            None => error,
        }
    })?;
    let lock = pins(&project, &manifest, name)?;
    let scope = answered(&project, &manifest, lock.as_ref(), name, args.locked)?;
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
        match hold_path(context, &project, &mut cache, (name, rest), file, before).await? {
            Waited::Ready(path) => path,
            Waited::Held(held) => {
                let (text, manifest) = project.manifest()?;
                known_with_rest(&manifest, name, rest, "path")?;
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
                            &scope,
                        )?;
                    }
                    let cache = cache.map_or_else(open_cache, Ok)?;
                    let work = Loop::new(context, &project, Arc::new(context.net()), cache)
                        .within(scope.clone())
                        .filling();
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
                    ignore_data(&work, &manifest, false)?;
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

/// `target` as path reads it, a dataset's name and a path in it, when it is typed as a path to a
/// dataset's folder or to something in it, as `ls` shows them: `data/iris/Iris.csv` from the
/// project's folder, or `Iris.csv` from data/iris. none when it starts with a dataset's name or
/// names no one dataset's folder.
fn placed(project: &Project, manifest: &Manifest, target: &str) -> Option<String> {
    let first = target.split(['/', '\\']).next().unwrap_or(target);
    if manifest.datasets.contains_key(first) {
        return None;
    }
    let here = std::env::current_dir().ok()?;
    let path = super::exclude::in_data_folder(
        &project.root,
        &here,
        &manifest.data_dir,
        std::path::Path::new(target),
    )
    .ok()?;
    let parts: Vec<&str> = path.split('/').collect();
    let mut holders: Vec<(usize, &String)> = manifest
        .datasets
        .keys()
        .filter_map(|dataset| {
            let folder = plan::folder(manifest, dataset);
            let depth = folder.split('/').count();
            let inside = parts.len() >= depth
                && folder
                    .split('/')
                    .zip(&parts)
                    .all(|(theirs, typed)| name::fold(theirs) == name::fold(typed));
            inside.then_some((depth, dataset))
        })
        .collect();
    holders.sort_by_key(|(depth, _)| std::cmp::Reverse(*depth));
    let (depth, dataset) = match holders.as_slice() {
        [only] => *only,
        [deepest, next, ..] if deepest.0 > next.0 => *deepest,
        _ => return None,
    };
    Some(match parts[depth..].join("/") {
        rest if rest.is_empty() => dataset.clone(),
        rest => format!("{dataset}/{rest}"),
    })
}

/// the help for `target`, which names no dataset, when the folder of one dataset alone holds a
/// file or folder at that path, as data/iris holds `Iris.csv`: the command that names it.
fn held_by(project: &Project, manifest: &Manifest, target: &str) -> Option<String> {
    let typed = target.trim_matches('/');
    let plain = typed
        .split('/')
        .all(|part| !part.is_empty() && part != "." && part != "..");
    let first = typed.split('/').next().unwrap_or(typed);
    if !plain || manifest.datasets.contains_key(first) {
        return None;
    }
    let mut holding = manifest.datasets.keys().filter(|dataset| {
        let folder = plan::folder(manifest, dataset);
        std::fs::symlink_metadata(project.root.join(folder).join(typed)).is_ok()
    });
    let dataset = holding.next()?;
    if holding.next().is_some() {
        return None;
    }
    let meant = format!("{dataset}/{typed}");
    let instead = format!("fetchloom path {}", super::shell_word(&meant));
    Some(format!(
        "did you mean `{}`?",
        super::meant(&[target], &[&meant], instead)
    ))
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

/// data.lock with only the pins of [`needed`] for `name`, or none when the project has none yet.
fn pins(project: &Project, manifest: &Manifest, name: &str) -> Result<Option<Lock>, Error> {
    let needed = needed(manifest, name);
    let lock = project
        .lock_only(&|other| needed.contains(other))?
        .map(|(lock, _)| lock);
    if let Some(lock) = &lock {
        lock.refuse_waiting([name])?;
    }
    Ok(lock)
}

/// the datasets path reads from data.lock for `name`: it, and those whose folders meet its folder,
/// so planning finds every clash with its files.
fn needed(manifest: &Manifest, name: &str) -> BTreeSet<String> {
    let folder = crate::name::fold(&plan::folder(manifest, name));
    manifest
        .datasets
        .keys()
        .filter(|other| {
            let theirs = crate::name::fold(&plan::folder(manifest, other));
            is_inside(&theirs, &folder) || is_inside(&folder, &theirs)
        })
        .cloned()
        .collect()
}

/// the scope of a path to `name`, once nothing keeps it from an answer: with `locked`, a dataset
/// data.lock does not pin as data.toml asks, which stops it as `sync --locked` stops.
fn answered(
    project: &Project,
    manifest: &Manifest,
    lock: Option<&Lock>,
    name: &str,
    is_locked: bool,
) -> Result<Scope, Error> {
    let scope = Scope::Only(BTreeSet::from([name.to_owned()]));
    if is_locked {
        locked(project, manifest, lock, "run path without --locked", &scope)?;
    }
    Ok(scope)
}

/// where `file` in `name`, or `name`'s folder, is among what `planned` places: the plan that
/// decided whether it is ready, so the answer never names a path that plan leaves out.
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
    Ok(native(&project.root, &relative))
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
        let instead = format!("fetchloom path {}", super::shell_word(&meant));
        super::meant(&[&typed], &[&meant], instead)
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

/// the answer when the files it names are in place, from one read of data.lock and one plan,
/// without holding the project; none when a sync must place them first.
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
    let fresh = manifest.datasets.get(name).is_some_and(|entry| {
        lock.get(name)
            .is_some_and(|locked| locked.drift(entry).is_none())
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
    // reject a known impossible member before a missing file can trigger downloads
    let answer = planned_path(project, manifest, &planned, name, file)?;
    // a file or folder named inside the dataset is all that has to be ready
    let wanted = match file.filter(|file| !file.is_empty()) {
        Some(file) => format!("{}/{file}", plan::folder(manifest, name)),
        None => plan::folder(manifest, name),
    };
    let survey = Survey::read_within(
        project,
        &manifest.data_dir,
        std::slice::from_ref(&wanted),
        context.threads,
        &context.stop,
    )?;
    let doubted = survey.doubted(&project.root, cache)?;
    let ready = planned
        .targets
        .iter()
        .filter(|target| target.dataset == name && is_inside(&target.path, &wanted))
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
        known_with_rest(&manifest, name, rest, "path")?;
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
                lock::spec(&manifest.datasets["a"].source),
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
                    modified: crate::units::nanos(stamp),
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

    /// a file's path comes from its source's listing, so the path a guess names reads back as one
    /// word even where the command line cannot be repeated.
    #[test]
    fn a_guessed_path_is_one_word_for_a_shell() {
        let error = not_in("iris", "notes (id).csv", &["notes $(id).csv"]).told();
        let help = error.help.unwrap_or_default();
        // no command on windows holds `$` safely, so a placeholder stands in for the path there
        if cfg!(windows) {
            assert_eq!(&*help, r#"did you mean `fetchloom path "<file>"`?"#);
            let named = "<file> is iris/notes $(id).csv, which no command here can hold safely";
            assert!(
                error.details.iter().any(|line| line == named),
                "{:?}",
                error.details
            );
            return;
        }
        let meant = help
            .strip_prefix("did you mean `")
            .and_then(|rest| rest.strip_suffix("`?"))
            .unwrap_or_default();
        assert_eq!(
            crate::suggested::command_words(meant, cfg!(windows)),
            Some(
                ["fetchloom", "path", "iris/notes $(id).csv"]
                    .map(str::to_owned)
                    .to_vec()
            ),
            "{help}"
        );
    }

    /// `path` needs its dataset's pins and those of overlapping folders.
    #[test]
    fn path_reads_only_the_pins_its_dataset_needs() {
        let text = concat!(
            "[datasets.a]\nref = 'https://example.org/a'\n\n",
            "[datasets.inner]\nref = 'https://example.org/inner'\npath = 'A/inner'\n\n",
            "[datasets.ab]\nref = 'https://example.org/ab'\n",
        );
        let manifest = Manifest::parse(text).unwrap();
        assert_eq!(needed_in(&manifest, "a"), ["a", "inner"]);
        assert_eq!(needed_in(&manifest, "inner"), ["a", "inner"]);
        assert_eq!(needed_in(&manifest, "ab"), ["ab"]);
        let whole = Manifest::parse(&format!(
            "{text}\n[datasets.all]\nref = 'https://example.org/all'\npath = '.'\n"
        ))
        .unwrap();
        assert_eq!(needed_in(&whole, "ab"), ["ab", "all"]);
        assert_eq!(needed_in(&whole, "all").len(), 4);
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
