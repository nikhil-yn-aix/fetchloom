//! making the data folder match the lock: place what is missing, replace what fetchloom placed
//! before when the lock moved on, remove what no dataset needs, and leave alone every file
//! someone changed or added.

mod put;
mod tell;

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::Serialize;

use self::put::put_all;
use self::tell::tell;
use super::disk::Survey;
use crate::cache::{Cache, Doubt};
use crate::digest::Sha256;
use crate::error::Error;
use crate::output::Output;
use crate::place::{self, Method, Placer};
use crate::plan::{self, Target};
use crate::project::Project;
use crate::project::lock::{Lock, LockedFile};
use crate::project::manifest::Manifest;
use crate::project::state::{Look, Placed, nanos};
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
    /// files replaced because another project changed the bytes they shared through a link.
    pub(crate) restored: Vec<String>,
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
    /// a hardlink a mark put in doubt that still holds the locked bytes: kept, with the time
    /// hashing proved it and how it looks by name now.
    Vouch(Look),
    Leave(Reason),
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
    survey: Survey,
}

impl<'a> Placing<'a> {
    /// compares the data folder, as `survey` saw it, with `lock`, with each archive standing for
    /// the files the cache records it unpacked to. only files fetchloom did not place are hashed,
    /// to see whether they already hold the locked bytes.
    pub(crate) fn new(
        project: &'a Project,
        manifest: &'a Manifest,
        lock: &'a Lock,
        cache: &Cache,
        force: bool,
        survey: Survey,
    ) -> Result<Placing<'a>, Error> {
        let plan = plan::from_cache(manifest, lock, cache)?;
        let data_dir = project.data_dir(manifest)?;
        let _span = tracing::debug_span!("decide").entered();
        let actions = plan
            .targets
            .iter()
            .map(|target| decide(target, &survey, force, &project.root.join(&target.path)))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Placing {
            project,
            manifest,
            targets: plan.targets,
            unknown: plan.unknown,
            actions,
            data_dir,
            survey,
        })
    }

    /// what the data folder held when this placing was decided, to decide again from.
    pub(crate) fn into_survey(self) -> Survey {
        self.survey
    }

    /// the archives never unpacked on this machine, with the dataset each belongs to.
    pub(crate) fn unknown(&self) -> Vec<(String, &'a LockedFile)> {
        self.unknown
            .iter()
            .map(|(dataset, file)| ((*dataset).to_owned(), *file))
            .collect()
    }

    /// the locked files whose bytes placing needs and the cache lacks, each once, with the
    /// dataset each belongs to. an archive is listed when a file it unpacks to is missing. an
    /// object a mark puts in doubt is hashed first, and one whose bytes changed is removed, so it
    /// is listed too; one that cannot be removed is an error.
    pub(crate) fn missing(&self, cache: &Cache) -> Result<Vec<(String, &'a LockedFile)>, Error> {
        let needed: Vec<&Target> = self
            .targets
            .iter()
            .zip(&self.actions)
            .filter(|(_, action)| matches!(action, Action::Place | Action::Replace))
            .map(|(target, _)| target)
            .collect();
        let mut unique = HashSet::new();
        let objects: Vec<(Sha256, u64)> = needed
            .iter()
            .filter(|target| unique.insert(target.sha256))
            .map(|target| (target.sha256, target.size))
            .collect();
        let _span = tracing::debug_span!("find_objects", objects = objects.len()).entered();
        let marks = cache.changed();
        let found = place::run_all(&objects, |(digest, size)| {
            cache.trusted(digest, Some(*size), &marks)
        });
        let mut present = HashSet::new();
        for ((digest, _), found) in objects.iter().zip(found) {
            if found.map_err(|doubt| doubtful(&doubt))?.is_some() {
                present.insert(*digest);
            }
        }
        let mut seen = HashSet::new();
        Ok(needed
            .into_iter()
            .filter(|target| !present.contains(&target.sha256))
            .filter(|target| seen.insert(target.locked.sha256))
            .map(|target| (target.dataset.clone(), target.locked))
            .collect())
    }

    /// removes what no dataset needs, places what is missing or outdated, and records it.
    pub(crate) fn apply(
        self,
        output: &Output,
        work: &Work,
        cache: &Cache,
    ) -> Result<Placement, Error> {
        let _span = tracing::debug_span!("place").entered();
        let Placing {
            project,
            manifest,
            targets,
            actions,
            data_dir,
            mut survey,
            unknown,
        } = self;
        let mut placement = Placement::default();
        let wanted: HashSet<&str> = targets.iter().map(|target| target.path.as_str()).collect();
        let mut placer = Placer::new(&data_dir, &project.own_folder());
        // removing first means a file that moved never collides with its own old copy
        let removed = remove_unwanted(
            project,
            &mut placer,
            &mut survey,
            &wanted,
            &unknown,
            &mut placement,
        )?;
        let (todo, adopted) = settle(
            project,
            &mut placer,
            cache,
            &mut survey,
            &targets,
            actions,
            &mut placement,
        )?;
        let needs_writing = survey.needs_writing;
        let state = &mut survey.state;
        let (tallies, failure) = put_all(project, &mut placer, cache, &todo, state, work);
        if removed || adopted || needs_writing || !tallies.is_empty() {
            project.write_state(state)?;
        }
        if let Some(error) = failure {
            return Err(error);
        }
        tell(output, manifest, &placer, &tallies, &placement);
        Ok(placement)
    }

    /// removes only what no dataset needs, and places nothing, so `remove` never downloads.
    pub(crate) fn remove_only(self, output: &Output) -> Result<Placement, Error> {
        let _span = tracing::debug_span!("remove").entered();
        let Placing {
            project,
            manifest,
            targets,
            data_dir,
            mut survey,
            unknown,
            ..
        } = self;
        let mut placement = Placement::default();
        let wanted: HashSet<&str> = targets.iter().map(|target| target.path.as_str()).collect();
        let mut placer = Placer::new(&data_dir, &project.own_folder());
        let removed = remove_unwanted(
            project,
            &mut placer,
            &mut survey,
            &wanted,
            &unknown,
            &mut placement,
        )?;
        if removed || survey.needs_writing {
            project.write_state(&survey.state)?;
        }
        tell(output, manifest, &placer, &BTreeMap::new(), &placement);
        Ok(placement)
    }
}

/// carries out every decision short of placing files: counts what stays, records files found to
/// hold the locked bytes, and removes files about to be replaced unless they changed since the
/// scan. returns the targets left to place, and whether the state gained a file.
fn settle<'t, 'a>(
    project: &Project,
    placer: &mut Placer,
    cache: &Cache,
    survey: &mut Survey,
    targets: &'t [Target<'a>],
    actions: Vec<Action>,
    placement: &mut Placement,
) -> Result<(Vec<&'t Target<'a>>, bool), Error> {
    let mut todo = Vec::new();
    let mut adopted = false;
    for (target, action) in targets.iter().zip(actions) {
        match action {
            Action::Keep => placement.unchanged += 1,
            Action::Adopt(seen) => {
                let found = adoption(target, seen, &project.root.join(&target.path), cache);
                survey.state.files.insert(target.path.clone(), found);
                placement.unchanged += 1;
                adopted = true;
            }
            Action::Vouch(seen) => {
                if let Some(placed) = survey.state.files.get_mut(&target.path) {
                    placed.look = seen;
                    placed.checked = nanos(SystemTime::now());
                }
                placement.unchanged += 1;
                adopted = true;
            }
            Action::Leave(reason) => placement.kept.push(Kept {
                path: target.path.clone(),
                reason,
            }),
            Action::Replace => {
                let destination = project.root.join(&target.path);
                match now(&destination) {
                    Some(look) if survey.disk.get(&target.path) != Some(&look) => {
                        let reason = if survey.state.files.contains_key(&target.path) {
                            Reason::Changed
                        } else {
                            Reason::Foreign
                        };
                        placement.kept.push(Kept {
                            path: target.path.clone(),
                            reason,
                        });
                        continue;
                    }
                    Some(_) => {
                        let recorded = survey.state.files.get(&target.path);
                        placer
                            .remove(&destination, false, recorded.map(|placed| placed.method))
                            .map_err(|err| local(&destination, &err))?;
                    }
                    None => {}
                }
                if survey.elsewhere.contains(&target.path) {
                    placement.restored.push(target.path.clone());
                }
                placement.replaced += 1;
                todo.push(target);
            }
            Action::Place => {
                placement.placed += 1;
                todo.push(target);
            }
        }
    }
    Ok((todo, adopted))
}

/// what the file at `destination` needs, as `survey` saw it. a hardlink a mark put in doubt is
/// replaced when hashing found its bytes changed and kept when it found them intact, and a file
/// hashing found changed is an edit, though its size and time may not show either. a changed file
/// that holds the locked bytes is taken as placed: a run cut off after it replaced a file and
/// before it wrote the state leaves the new bytes recorded as the old file.
fn decide(
    target: &Target,
    survey: &Survey,
    force: bool,
    destination: &Path,
) -> Result<Action, Error> {
    let path = &target.path;
    let Some(look) = survey.disk.get(path) else {
        return Ok(Action::Place);
    };
    match survey.state.files.get(path) {
        Some(_) if survey.elsewhere.contains(path) => Ok(Action::Replace),
        Some(placed) if survey.vouched.contains(path) => {
            let fits = !target.copy || placed.writable;
            Ok(if placed.sha256 == target.sha256 && fits {
                Action::Vouch(*look)
            } else {
                Action::Replace
            })
        }
        Some(placed) if placed.look == *look && !survey.edited.contains(path) => {
            let fits = !target.copy || placed.writable;
            Ok(if placed.sha256 == target.sha256 && fits {
                Action::Keep
            } else {
                Action::Replace
            })
        }
        Some(_) if holds_target(target, look, destination) => Ok(Action::Adopt(*look)),
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

/// deletes files fetchloom placed that no dataset needs now, unless someone changed them. the
/// files of a dataset with an archive this machine never unpacked stay, since nothing tells which
/// of them it still needs. says whether the state lost a file.
fn remove_unwanted(
    project: &Project,
    placer: &mut Placer,
    survey: &mut Survey,
    wanted: &HashSet<&str>,
    unknown: &[(&str, &LockedFile)],
    placement: &mut Placement,
) -> Result<bool, Error> {
    let unsure: HashSet<&str> = unknown.iter().map(|(dataset, _)| *dataset).collect();
    let unwanted: Vec<String> = survey
        .state
        .files
        .iter()
        .filter(|(path, placed)| {
            !wanted.contains(path.as_str()) && !unsure.contains(placed.dataset.as_str())
        })
        .map(|(path, _)| path.clone())
        .collect();
    let changed = !unwanted.is_empty();
    for path in unwanted {
        let Some(before) = survey.state.files.remove(&path) else {
            continue;
        };
        match survey.disk.get(&path) {
            None => {}
            Some(look) if *look == before.look && !survey.edited.contains(&path) => {
                let file = project.root.join(&path);
                match now(&file) {
                    Some(current) if current == *look => {
                        placer
                            .remove(&file, true, Some(before.method))
                            .map_err(|err| local(&file, &err))?;
                        placement.removed += 1;
                    }
                    Some(_) => placement.kept.push(Kept {
                        path,
                        reason: Reason::Unneeded,
                    }),
                    None => {}
                }
            }
            Some(_) => placement.kept.push(Kept {
                path,
                reason: Reason::Unneeded,
            }),
        }
    }
    Ok(changed)
}

/// how the file at `path` looks right now. a file is read again right before it is deleted,
/// since downloads may have run for hours after the folder was scanned, and someone may have
/// changed it meanwhile.
fn now(path: &Path) -> Option<Look> {
    fs::symlink_metadata(path)
        .ok()
        .map(|metadata| Look::of(&metadata))
}

fn placed(target: &Target, method: Method, look: Look, writable: bool) -> Placed {
    Placed {
        dataset: target.dataset.clone(),
        sha256: target.sha256,
        method,
        look,
        writable,
        checked: nanos(SystemTime::now()),
    }
}

/// the record of a file at `path` found to hold the target's bytes. a link a run cut off before
/// it wrote the state placed is the cached object under another name, so it is recorded as a
/// hardlink, which the audit covers and whose read-only flag is never cleared.
fn adoption(target: &Target, look: Look, path: &Path, cache: &Cache) -> Placed {
    if same_file::is_same_file(path, cache.object_path(&target.sha256)).unwrap_or(false) {
        placed(target, Method::Hardlink, look, false)
    } else {
        placed(target, Method::Copy, look, now_writable(path))
    }
}

/// whether the file at `path`, seen as `look`, holds the target's bytes. only a file of the
/// target's size is hashed, and one that cannot be read does not.
pub(crate) fn holds_target(target: &Target, look: &Look, path: &Path) -> bool {
    look.size == target.size && Sha256::of_file(path).is_ok_and(|digest| digest == target.sha256)
}

/// whether the file at `path` may be written to now.
pub(crate) fn now_writable(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| !metadata.permissions().readonly())
}

pub(super) fn local(path: &Path, err: &std::io::Error) -> Error {
    Error::io(format_args!("could not place {}", path.display()), err)
}

/// what stops a run at a cached object in doubt: one hashing could not read, or one whose bytes
/// changed and which could not be removed, so it would be placed again as if it held them.
pub(crate) fn doubtful(doubt: &Doubt) -> Error {
    let object = doubt.object.display();
    if doubt.changed {
        Error::io(
            format_args!("could not remove {object}, a cached file whose bytes changed"),
            &doubt.error,
        )
    } else {
        Error::io(
            format_args!("could not read {object} to check its bytes"),
            &doubt.error,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_placement_the_disk_refuses_says_what_to_do() {
        let full = local(
            Path::new("data/iris/iris.csv"),
            &std::io::ErrorKind::StorageFull.into(),
        );
        assert_eq!(
            full.help.as_deref(),
            Some("free some space on that disk, then run the command again")
        );
    }
}
