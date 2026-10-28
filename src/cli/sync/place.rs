//! making the data folder match the lock: place what is missing, replace what fetchloom placed
//! before when the lock moved on, remove what no dataset needs, and leave alone every file
//! someone changed or added.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::cache::Cache;
use crate::digest::Sha256;
use crate::error::{Error, Kind};
use crate::output::Output;
use crate::place::{Method, Placer};
use crate::plan::{self, Target};
use crate::project::Project;
use crate::project::lock::{Lock, LockedFile};
use crate::project::manifest::Manifest;
use crate::project::state::{Look, Placed, State};
use crate::units;
use crate::work::Work;

/// what placing did.
#[derive(Debug, Default, Serialize)]
pub(crate) struct Placement {
    pub(crate) placed: u64,
    pub(crate) replaced: u64,
    pub(crate) removed: u64,
    pub(crate) unchanged: u64,
    /// files left alone: changed by hand, or never placed by fetchloom.
    pub(crate) kept: Vec<Kept>,
}

#[derive(Debug, Serialize)]
pub(crate) struct Kept {
    pub(crate) path: String,
    pub(crate) reason: Reason,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Reason {
    /// fetchloom placed it, and it changed since.
    Changed,
    /// fetchloom did not place it, and it holds other bytes than the lock pins.
    Foreign,
    /// no dataset needs it now, but it changed since fetchloom placed it.
    Unneeded,
}

enum Action {
    Keep,
    Place,
    Replace,
    Adopt(Look),
    Leave(Reason),
}

/// what one dataset's placing cost, for its summary line.
#[derive(Default)]
struct Tally {
    files: u64,
    links: u64,
    copied: u64,
}

/// the data folder compared with a lock, with what each file needs decided before anything
/// downloads, so only the bytes placing lacks are fetched.
pub(crate) struct Placing<'a> {
    project: &'a Project,
    manifest: &'a Manifest,
    targets: Vec<Target<'a>>,
    unknown: Vec<(&'a str, &'a LockedFile)>,
    actions: Vec<Action>,
    data_dir: PathBuf,
    state: State,
    disk: HashMap<String, Look>,
}

impl<'a> Placing<'a> {
    /// compares the data folder with `lock`, with each archive standing for the files the cache
    /// records it unpacked to. only files fetchloom did not place are hashed, to see whether
    /// they already hold the locked bytes.
    pub(crate) fn new(
        project: &'a Project,
        manifest: &'a Manifest,
        lock: &'a Lock,
        cache: &Cache,
        force: bool,
    ) -> Result<Placing<'a>, Error> {
        let plan = plan::from_cache(manifest, lock, cache)?;
        let data_dir = project.data_dir(manifest)?;
        let state = project.state();
        let disk = scan(&project.root, &manifest.data_dir);
        let actions = plan
            .targets
            .iter()
            .map(|target| {
                decide(
                    target,
                    disk.get(&target.path),
                    state.files.get(&target.path),
                    force,
                    &project.root.join(&target.path),
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Placing {
            project,
            manifest,
            targets: plan.targets,
            unknown: plan.unknown,
            actions,
            data_dir,
            state,
            disk,
        })
    }

    /// the archives never unpacked on this machine, with the dataset each belongs to.
    pub(crate) fn unknown(&self) -> Vec<(String, &'a LockedFile)> {
        self.unknown
            .iter()
            .map(|(dataset, file)| ((*dataset).to_owned(), *file))
            .collect()
    }

    /// the locked files whose bytes placing needs and the cache lacks, each once, with the
    /// dataset each belongs to. an archive is listed when a file it unpacks to is missing.
    pub(crate) fn missing(&self, cache: &Cache) -> Vec<(String, &'a LockedFile)> {
        let mut seen = HashSet::new();
        self.targets
            .iter()
            .zip(&self.actions)
            .filter(|(_, action)| matches!(action, Action::Place | Action::Replace))
            .filter(|(target, _)| cache.find(&target.sha256, Some(target.size)).is_none())
            .filter(|(target, _)| seen.insert(target.locked.sha256))
            .map(|(target, _)| (target.dataset.clone(), target.locked))
            .collect()
    }
    /// removes what no dataset needs, places what is missing or outdated, and records it.
    pub(crate) fn apply(
        self,
        output: &Output,
        work: &Work,
        cache: &Cache,
    ) -> Result<Placement, Error> {
        let Placing {
            project,
            manifest,
            targets,
            actions,
            data_dir,
            mut state,
            disk,
            unknown: _,
        } = self;
        let mut placement = Placement::default();
        let wanted: HashSet<&str> = targets.iter().map(|target| target.path.as_str()).collect();
        let mut placer = Placer::new(&data_dir);
        // removing first means a file that moved never collides with its own old copy
        remove_unwanted(
            project,
            &data_dir,
            &mut placer,
            &mut state,
            &disk,
            &wanted,
            &mut placement,
        )?;
        let mut tallies: BTreeMap<&str, Tally> = BTreeMap::new();
        for (target, action) in targets.iter().zip(actions) {
            let destination = project.root.join(&target.path);
            match action {
                Action::Keep => placement.unchanged += 1,
                Action::Adopt(seen) => {
                    state
                        .files
                        .insert(target.path.clone(), placed(target, Method::Copy, seen));
                    placement.unchanged += 1;
                }
                Action::Leave(reason) => placement.kept.push(Kept {
                    path: target.path.clone(),
                    reason,
                }),
                action @ (Action::Place | Action::Replace) => {
                    if matches!(action, Action::Replace) {
                        fs::remove_file(&destination).map_err(|err| local(&destination, &err))?;
                        placement.replaced += 1;
                    } else {
                        placement.placed += 1;
                    }
                    let method = put(&mut placer, cache, target, &destination)?;
                    let seen = Look::of(
                        &fs::metadata(&destination).map_err(|err| local(&destination, &err))?,
                    );
                    state
                        .files
                        .insert(target.path.clone(), placed(target, method, seen));
                    count(work, method);
                    let tally = tallies.entry(target.dataset.as_str()).or_default();
                    tally.files += 1;
                    if method == Method::Copy {
                        tally.copied += target.size;
                    } else {
                        tally.links += 1;
                    }
                }
            }
        }
        project.write_state(&state)?;
        tell(output, manifest, &placer, &tallies, &placement);
        Ok(placement)
    }

    /// removes only what no dataset needs, and places nothing, so `remove` never downloads.
    pub(crate) fn remove_only(self, output: &Output) -> Result<Placement, Error> {
        let Placing {
            project,
            manifest,
            targets,
            data_dir,
            mut state,
            disk,
            ..
        } = self;
        let mut placement = Placement::default();
        let wanted: HashSet<&str> = targets.iter().map(|target| target.path.as_str()).collect();
        let mut placer = Placer::new(&data_dir);
        remove_unwanted(
            project,
            &data_dir,
            &mut placer,
            &mut state,
            &disk,
            &wanted,
            &mut placement,
        )?;
        project.write_state(&state)?;
        tell(output, manifest, &placer, &BTreeMap::new(), &placement);
        Ok(placement)
    }
}

fn put(
    placer: &mut Placer,
    cache: &Cache,
    target: &Target,
    destination: &Path,
) -> Result<Method, Error> {
    let object = cache
        .find(&target.sha256, Some(target.size))
        .ok_or_else(|| {
            Error::new(
                Kind::LocalIo,
                format!("the cache lost {} while placing it", target.path),
            )
            .help("run the same command again to download it")
        })?;
    placer
        .place(&object, destination, target.copy)
        .map_err(|err| local(destination, &err))
}

/// hashes the objects behind hardlinks someone edited, before anything downloads. an edit
/// through a hardlink also changes the cached object, which is then dropped and fetched again,
/// so it never spreads to other projects.
pub(crate) fn audit(project: &Project, manifest: &Manifest, cache: &Cache) -> Result<(), Error> {
    let state = project.state();
    let edited: Vec<&Placed> = state
        .files
        .values()
        .filter(|placed| placed.method == Method::Hardlink)
        .collect();
    if edited.is_empty() {
        return Ok(());
    }
    let disk = scan(&project.root, &manifest.data_dir);
    for (path, placed) in &state.files {
        if placed.method == Method::Hardlink
            && disk.get(path).is_some_and(|seen| *seen != placed.look)
            && !cache
                .verify(&placed.sha256)
                .map_err(|err| local(&project.root.join(path), &err))?
        {
            tracing::debug!(%path, "an edit through a hardlink changed its cached object");
        }
    }
    Ok(())
}

fn decide(
    target: &Target,
    look: Option<&Look>,
    placed: Option<&Placed>,
    force: bool,
    destination: &Path,
) -> Result<Action, Error> {
    let Some(look) = look else {
        return Ok(Action::Place);
    };
    match placed {
        Some(placed) if placed.look == *look => {
            let fits = !target.copy || placed.method == Method::Copy;
            Ok(if placed.sha256 == target.sha256 && fits {
                Action::Keep
            } else {
                Action::Replace
            })
        }
        Some(_) => Ok(if force {
            Action::Replace
        } else {
            Action::Leave(Reason::Changed)
        }),
        None => {
            let same = look.size == target.size
                && Sha256::of_file(destination).map_err(|err| local(destination, &err))?
                    == target.sha256;
            Ok(if same {
                Action::Adopt(*look)
            } else if force {
                Action::Replace
            } else {
                Action::Leave(Reason::Foreign)
            })
        }
    }
}

/// deletes files fetchloom placed that no dataset needs now, unless someone changed them.
fn remove_unwanted(
    project: &Project,
    data_dir: &Path,
    placer: &mut Placer,
    state: &mut State,
    disk: &HashMap<String, Look>,
    wanted: &HashSet<&str>,
    placement: &mut Placement,
) -> Result<(), Error> {
    let unwanted: Vec<String> = state
        .files
        .keys()
        .filter(|path| !wanted.contains(path.as_str()))
        .cloned()
        .collect();
    for path in unwanted {
        let Some(before) = state.files.remove(&path) else {
            continue;
        };
        match disk.get(&path) {
            None => {}
            Some(look) if *look == before.look => {
                let file = project.root.join(&path);
                placer
                    .check_folders(&file, false)
                    .and_then(|()| fs::remove_file(&file))
                    .map_err(|err| local(&file, &err))?;
                prune(file.parent(), data_dir);
                placement.removed += 1;
            }
            Some(_) => placement.kept.push(Kept {
                path,
                reason: Reason::Unneeded,
            }),
        }
    }
    Ok(())
}

/// removes folders left empty, up to the data folder.
fn prune(mut folder: Option<&Path>, data_dir: &Path) {
    while let Some(current) = folder {
        if current == data_dir || !current.starts_with(data_dir) || fs::remove_dir(current).is_err()
        {
            return;
        }
        folder = current.parent();
    }
}

/// every file under the data folder and how it looks, by its path in the project. links are
/// neither followed nor touched.
pub(crate) fn scan(root: &Path, data_dir: &str) -> HashMap<String, Look> {
    let mut found = HashMap::new();
    let mut folders = vec![(root.join(data_dir), data_dir.to_owned())];
    while let Some((folder, relative)) = folders.pop() {
        let Ok(entries) = fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let path = format!("{relative}/{}", entry.file_name().to_string_lossy());
            if kind.is_dir() {
                folders.push((entry.path(), path));
            } else if kind.is_file()
                && let Ok(metadata) = entry.metadata()
            {
                found.insert(path, Look::of(&metadata));
            }
        }
    }
    found
}

fn placed(target: &Target, method: Method, look: Look) -> Placed {
    Placed {
        dataset: target.dataset.clone(),
        sha256: target.sha256,
        method,
        look,
    }
}

fn count(work: &Work, method: Method) {
    let counter = match method {
        Method::Reflink => &work.reflinks,
        Method::Hardlink => &work.hardlinks,
        Method::Copy => &work.copies,
    };
    Work::add(counter, 1);
}

fn tell(
    output: &Output,
    manifest: &Manifest,
    placer: &Placer,
    tallies: &BTreeMap<&str, Tally>,
    placement: &Placement,
) {
    for (dataset, tally) in tallies {
        let how = match (tally.links, tally.copied) {
            (_, 0) => "linked, no extra disk".to_owned(),
            (0, copied) => format!("copied, {}", units::size(copied)),
            (_, copied) => format!("partly copied, {}", units::size(copied)),
        };
        output.step(
            "Placed",
            &format!(
                "{} · {} {how}",
                plan::folder(manifest, dataset),
                units::count(tally.files, "file", "files")
            ),
        );
    }
    if placement.removed > 0 {
        output.step(
            "Removed",
            &units::count(
                placement.removed,
                "file no dataset needs",
                "files no dataset needs",
            ),
        );
    }
    if let Some(client) = placer.synced()
        && tallies.values().any(|tally| tally.copied > 0)
    {
        output.warn(&format!(
            "{} is inside {client}, which breaks links, so files were copied. a project outside {client} uses no extra disk",
            manifest.data_dir
        ));
    }
    for kept in &placement.kept {
        let message = match kept.reason {
            Reason::Changed => format!(
                "{} was changed by hand, so it stays. `fetchloom sync --force` replaces it",
                kept.path
            ),
            Reason::Foreign => format!(
                "{} was not placed by fetchloom and holds other bytes, so it stays. `fetchloom sync --force` replaces it",
                kept.path
            ),
            Reason::Unneeded => format!(
                "{} is no longer needed, but it was changed by hand, so it stays. delete it if you do not need it",
                kept.path
            ),
        };
        output.warn(&message);
    }
}

fn local(path: &Path, err: &std::io::Error) -> Error {
    Error::new(
        Kind::LocalIo,
        format!("could not place {}: {err}", path.display()),
    )
}
