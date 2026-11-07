//! making the data folder match the lock: place what is missing, replace what fetchloom placed
//! before when the lock moved on, remove what no dataset needs, and leave alone every file
//! someone changed or added.

#[cfg(test)]
#[cfg(unix)]
mod model_tests;
mod put;
mod tell;

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::Path;

use serde::Serialize;

use self::put::put_all;
use self::tell::tell;
pub(crate) use self::tell::tell_preview;
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
use crate::project::state::{Look, Placed, Synced};
use crate::work::Work;

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

/// what a dry run would change, path by path, as `--json` prints it.
#[derive(Debug, Default, Serialize)]
pub(crate) struct Changes {
    pub(crate) place: Vec<String>,
    pub(crate) replace: Vec<String>,
    pub(crate) remove: Vec<String>,
    /// files that would download first, by dataset and path.
    pub(crate) download: Vec<Download>,
    /// archives that would unpack first, whose files a dry run cannot list.
    pub(crate) unpack: Vec<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct Download {
    pub(crate) path: String,
    /// none when only the download tells.
    pub(crate) size: Option<u64>,
}

/// what applying a placing would do, decided without touching the data folder.
#[derive(Debug, Default)]
pub(crate) struct Preview {
    pub(crate) placement: Placement,
    pub(crate) changes: Changes,
    /// files to place and to replace, by dataset.
    pub(crate) by_dataset: BTreeMap<String, Planned>,
}

#[derive(Debug, Default)]
pub(crate) struct Planned {
    pub(crate) place: Vec<String>,
    pub(crate) replace: Vec<String>,
}

impl Preview {
    /// whether applying it would leave everything as it is, with nothing to report.
    pub(crate) fn changes_nothing(&self) -> bool {
        let placement = &self.placement;
        placement.placed + placement.replaced + placement.removed == 0
            && placement.kept.is_empty()
            && self.changes.download.is_empty()
            && self.changes.unpack.is_empty()
    }
}

/// what becomes of a file fetchloom placed that no dataset needs now.
enum Unwanted {
    /// it still looks as placed, so it goes.
    Remove(Look),
    /// someone changed it, so it stays.
    Keep,
    /// it is gone already.
    Gone,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Reason {
    /// fetchloom placed it, and it changed since.
    Changed,
    /// fetchloom placed it as a hardlink to the cache, and it changed since. the change is in
    /// every project sharing that link, and fetchloom cannot tell which one made it.
    Shared,
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
    /// the scan began and how it looks by name now.
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
    survey: Survey,
}

impl<'a> Placing<'a> {
    /// compares the data folder, as `survey` saw it, with `lock`, with each archive standing for
    /// the files the cache records it unpacked to. only files fetchloom did not place are hashed,
    /// to see whether they already hold the locked bytes. nothing is written, so a dry run can
    /// show what applying it would do.
    pub(crate) fn new(
        project: &'a Project,
        manifest: &'a Manifest,
        lock: &'a Lock,
        cache: &Cache,
        force: bool,
        survey: Survey,
    ) -> Result<Placing<'a>, Error> {
        let plan = plan::from_cache(manifest, lock, cache)?;
        Self::from_plan(project, manifest, plan, force, survey)
    }

    /// compares an already validated plan without reading archive member records again.
    pub(crate) fn from_plan(
        project: &'a Project,
        manifest: &'a Manifest,
        plan: plan::Plan<'a>,
        force: bool,
        mut survey: Survey,
    ) -> Result<Placing<'a>, Error> {
        project.planned_data_dir(manifest)?;
        survey.retarget(&project.root, &plan.targets);
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
    /// object a mark puts in doubt is hashed first, and changed bytes are listed for replacement.
    /// a failed read, seal or change mark stops the run.
    pub(crate) fn missing(&self, cache: &Cache) -> Result<Vec<(String, &'a LockedFile)>, Error> {
        let marks = cache.doubts()?;
        self.lacking(|digest, size| {
            cache
                .trusted(digest, Some(size), &marks)
                .map(|found| found.is_some())
                .map_err(|doubt| doubtful(&doubt))
        })
    }

    /// what [`Placing::missing`] lists, found without sealing or marking any cached object, for
    /// a dry run.
    pub(crate) fn missing_as_is(
        &self,
        cache: &Cache,
        damaged: &HashSet<Sha256>,
    ) -> Result<Vec<(String, &'a LockedFile)>, Error> {
        self.lacking(|digest, size| {
            Ok(!damaged.contains(digest) && cache.find(digest, Some(size)).is_some())
        })
    }

    fn lacking(
        &self,
        holds: impl Fn(&Sha256, u64) -> Result<bool, Error> + Sync,
    ) -> Result<Vec<(String, &'a LockedFile)>, Error> {
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
        let found = place::run_all(&objects, self.survey.threads, |(digest, size)| {
            holds(digest, *size)
        });
        let mut present = HashSet::new();
        for ((digest, _), found) in objects.iter().zip(found) {
            if found? {
                present.insert(*digest);
            }
        }
        let mut seen = HashSet::new();
        Ok(needed
            .into_iter()
            .filter(|target| !present.contains(&target.sha256))
            .filter(|target| seen.insert(target.locked.sha256))
            .map(|target| (target.dataset.to_owned(), target.locked))
            .collect())
    }

    /// what [`Placing::apply`] would do, decided as it decides and with nothing written. a file
    /// that changes between this look and placing is still left alone when it is placed for real.
    pub(crate) fn preview(&self) -> Preview {
        let mut preview = Preview::default();
        self.preview_unwanted(&mut preview);
        let placement = &mut preview.placement;
        for (target, action) in self.targets.iter().zip(&self.actions) {
            match action {
                Action::Keep | Action::Adopt(_) | Action::Vouch(_) => placement.unchanged += 1,
                Action::Leave(reason) => placement.kept.push(Kept {
                    path: target.path.clone(),
                    reason: *reason,
                }),
                Action::Replace => {
                    if self.survey.elsewhere.contains(&target.path) {
                        placement.restored.push(target.path.clone());
                    }
                    placement.replaced += 1;
                    preview.changes.replace.push(target.path.clone());
                    preview
                        .by_dataset
                        .entry(target.dataset.to_owned())
                        .or_default()
                        .replace
                        .push(target.path.clone());
                }
                Action::Place => {
                    placement.placed += 1;
                    preview.changes.place.push(target.path.clone());
                    preview
                        .by_dataset
                        .entry(target.dataset.to_owned())
                        .or_default()
                        .place
                        .push(target.path.clone());
                }
            }
        }
        preview
    }

    /// what [`Placing::remove_only`] would do, with nothing written.
    pub(crate) fn preview_removal(&self) -> Preview {
        let mut preview = Preview::default();
        self.preview_unwanted(&mut preview);
        preview
    }

    fn preview_unwanted(&self, preview: &mut Preview) {
        let wanted: HashSet<&str> = self
            .targets
            .iter()
            .map(|target| target.path.as_str())
            .collect();
        for (path, fate) in unwanted(&self.survey, &wanted, &self.unknown) {
            match fate {
                Unwanted::Remove(_) => {
                    preview.placement.removed += 1;
                    preview.changes.remove.push(path);
                }
                Unwanted::Keep => preview.placement.kept.push(Kept {
                    path,
                    reason: Reason::Unneeded,
                }),
                Unwanted::Gone => {}
            }
        }
    }

    /// removes what no dataset needs, places what is missing or outdated, and records it, with
    /// `synced` marked complete when every file the lock asks for ended up in place.
    pub(crate) fn apply(
        self,
        output: &Output,
        work: &Work,
        cache: &Cache,
        synced: Option<Synced>,
        told: Option<&str>,
        stop: &std::sync::Arc<crate::stop::Stop>,
    ) -> Result<Placement, Error> {
        stop.check()?;
        let _span = tracing::debug_span!("place").entered();
        let Placing {
            project,
            manifest,
            targets,
            actions,
            mut survey,
            unknown,
        } = self;
        let data_dir = project.data_dir(manifest)?;
        let mut placement = Placement::default();
        let wanted: HashSet<&str> = targets.iter().map(|target| target.path.as_str()).collect();
        let mut placer = Placer::new(&data_dir)
            .with_experiments(cache.experiments().clone())
            .with_threads(survey.threads)
            .with_stop(std::sync::Arc::clone(stop));
        // removing first means a file that moved never collides with its own old copy
        let removed = remove_unwanted(
            project,
            &mut placer,
            &mut survey,
            &wanted,
            &unknown,
            &mut placement,
        )?;
        sweep(project, &mut placer, &survey, &wanted);
        let (todo, adopted) = settle(
            project,
            &mut placer,
            cache,
            &mut survey,
            &targets,
            actions,
            &mut placement,
        )?;
        survey.smudge_edited();
        let needs_writing = survey.needs_writing;
        let checked = survey.started;
        let state = &mut survey.state;
        let (tallies, failure) = put_all(project, &mut placer, cache, &todo, state, checked, work);
        // place_all has flushed published copies; a stop discards its unfinished batch.
        if !stop.is_asked() {
            placer
                .flush_copies()
                .map_err(|err| Error::io("could not flush the copied files to disk", &err))?;
        }
        let complete = !stop.is_asked()
            && failure.is_none()
            && placement.kept.is_empty()
            && unknown.is_empty();
        let synced = synced.map(|synced| Synced { complete, ..synced });
        let resynced = state.synced != synced;
        state.synced = synced;
        if removed || adopted || needs_writing || resynced || !tallies.is_empty() {
            project.write_state(state, cache.experiments())?;
        }
        stop.check()?;
        if let Some(error) = failure {
            return Err(error);
        }
        tell(output, manifest, &placer, &tallies, &placement, told);
        Ok(placement)
    }

    /// removes only what no dataset needs, and places nothing, so `remove` never downloads.
    pub(crate) fn remove_only(self, output: &Output, cache: &Cache) -> Result<Placement, Error> {
        let _span = tracing::debug_span!("remove").entered();
        let Placing {
            project,
            manifest,
            targets,
            mut survey,
            unknown,
            ..
        } = self;
        let data_dir = project.data_dir(manifest)?;
        let mut placement = Placement::default();
        let wanted: HashSet<&str> = targets.iter().map(|target| target.path.as_str()).collect();
        let mut placer = Placer::new(&data_dir)
            .with_experiments(cache.experiments().clone())
            .with_threads(survey.threads);
        let removed = remove_unwanted(
            project,
            &mut placer,
            &mut survey,
            &wanted,
            &unknown,
            &mut placement,
        )?;
        survey.smudge_edited();
        let forgot = survey.state.synced.take().is_some();
        if removed || forgot || survey.needs_writing {
            project.write_state(&survey.state, cache.experiments())?;
        }
        tell(
            output,
            manifest,
            &placer,
            &BTreeMap::new(),
            &placement,
            None,
        );
        Ok(placement)
    }
}

/// data.toml and data.lock as a sync read them before it began.
pub(crate) struct Seen {
    pub(crate) manifest: Sha256,
    /// none when the project had no data.lock yet.
    pub(crate) lock: Option<Sha256>,
}

/// what a sync with nothing to do placed, found without reading what archives unpack to or
/// planning: data.toml and data.lock are byte for byte as a sync that left every file they ask
/// for in place left them, so the state records exactly those files, and each one still looks as
/// recorded. anything else, or any doubt the survey raised, takes the long way. a dry run passes
/// `make` false so a missing data folder is not created.
pub(crate) fn untouched(
    project: &Project,
    manifest: &Manifest,
    survey: &Survey,
    seen: &Seen,
    make: bool,
) -> Result<Option<Placement>, Error> {
    let _span = tracing::debug_span!("untouched").entered();
    let state = &survey.state;
    let settled = state.synced.is_some_and(|synced| {
        synced.complete && synced.manifest == seen.manifest && Some(synced.lock) == seen.lock
    });
    if !settled
        || survey.needs_writing
        || !survey.edited.is_empty()
        || !survey.elsewhere.is_empty()
        || !survey.vouched.is_empty()
        || survey.leftovers().next().is_some()
        || state
            .files
            .iter()
            .any(|(path, placed)| survey.disk.get(path) != Some(&placed.look))
    {
        return Ok(None);
    }
    // placing refuses a data folder that became a link, and so does this
    if make {
        project.data_dir(manifest)?;
    } else {
        project.planned_data_dir(manifest)?;
    }
    Ok(Some(Placement {
        unchanged: state.files.len() as u64,
        ..Placement::default()
    }))
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
        placer.stop().check()?;
        match action {
            Action::Keep => placement.unchanged += 1,
            Action::Adopt(seen) => {
                let found = adoption(
                    target,
                    seen,
                    &project.root.join(&target.path),
                    cache,
                    survey.started,
                );
                survey.state.files.insert(target.path.clone(), found);
                placement.unchanged += 1;
                adopted = true;
            }
            Action::Vouch(seen) => {
                if let Some(placed) = survey.state.files.get_mut(&target.path) {
                    placed.look = seen;
                    placed.checked = survey.started;
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
/// that holds the locked bytes is taken as placed if its mode fits the dataset: a run cut off
/// after it replaced a file and before it wrote the state leaves the new bytes recorded as old.
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
        Some(_) if holds_target(target, look, destination) => {
            Ok(if !target.copy || now_writable(destination) {
                Action::Adopt(*look)
            } else {
                Action::Replace
            })
        }
        Some(placed) => Ok(if force {
            Action::Replace
        } else if placed.method == Method::Hardlink && place::has_other_names(destination) {
            Action::Leave(Reason::Shared)
        } else {
            Action::Leave(Reason::Changed)
        }),
        None => {
            let same = look.size == target.size
                && Sha256::of_file(destination).map_err(|err| local(destination, &err))?
                    == target.sha256;
            Ok(if same && (!target.copy || now_writable(destination)) {
                Action::Adopt(*look)
            } else if same || force {
                Action::Replace
            } else {
                Action::Leave(Reason::Foreign)
            })
        }
    }
}

/// deletes files fetchloom placed that no dataset needs now, unless someone changed them. says
/// whether the state lost a file.
fn remove_unwanted(
    project: &Project,
    placer: &mut Placer,
    survey: &mut Survey,
    wanted: &HashSet<&str>,
    unknown: &[(&str, &LockedFile)],
    placement: &mut Placement,
) -> Result<bool, Error> {
    let unwanted = unwanted(survey, wanted, unknown);
    let changed = !unwanted.is_empty();
    for (path, fate) in unwanted {
        placer.stop().check()?;
        let Some(before) = survey.state.files.remove(&path) else {
            continue;
        };
        match fate {
            Unwanted::Gone => {}
            Unwanted::Remove(look) => {
                let file = project.root.join(survey.listed.get(&path).unwrap_or(&path));
                match now(&file) {
                    Some(current) if current == look => {
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
            Unwanted::Keep => placement.kept.push(Kept {
                path,
                reason: Reason::Unneeded,
            }),
        }
    }
    Ok(changed)
}

/// deletes what an interrupted placement left under the hidden names it stages files in. the
/// project lock keeps any other sync from staging files meanwhile. a leftover that changed since
/// the scan, or that cannot be deleted, stays for a later sync.
fn sweep(project: &Project, placer: &mut Placer, survey: &Survey, wanted: &HashSet<&str>) {
    for path in survey.leftovers() {
        if placer.stop().is_asked() {
            break;
        }
        let file = project.root.join(path);
        if !wanted.contains(path.as_str())
            && now(&file).is_some_and(|look| survey.disk.get(path) == Some(&look))
            && let Err(err) = placer.remove(&file, false, None)
        {
            tracing::debug!(%path, %err, "could not delete a leftover of a placement");
        }
    }
}

/// what becomes of each file fetchloom placed that no dataset needs now. the files of a dataset
/// with an archive this machine never unpacked stay, since nothing tells which of them it still
/// needs.
fn unwanted(
    survey: &Survey,
    wanted: &HashSet<&str>,
    unknown: &[(&str, &LockedFile)],
) -> Vec<(String, Unwanted)> {
    let unsure: HashSet<&str> = unknown.iter().map(|(dataset, _)| *dataset).collect();
    survey
        .state
        .files
        .iter()
        .filter(|(path, placed)| {
            !wanted.contains(path.as_str()) && !unsure.contains(placed.dataset.as_str())
        })
        .map(|(path, placed)| {
            let fate = match survey.disk.get(path) {
                None => Unwanted::Gone,
                Some(look) if *look == placed.look && !survey.edited.contains(path) => {
                    Unwanted::Remove(*look)
                }
                Some(_) => Unwanted::Keep,
            };
            (path.clone(), fate)
        })
        .collect()
}

/// how the file at `path` looks right now. a file is read again right before it is deleted,
/// since downloads may have run for hours after the folder was scanned, and someone may have
/// changed it meanwhile.
fn now(path: &Path) -> Option<Look> {
    fs::symlink_metadata(path)
        .ok()
        .map(|metadata| Look::of(&metadata))
}

fn placed(target: &Target, method: Method, look: Look, writable: bool, checked: u64) -> Placed {
    Placed {
        dataset: target.dataset.to_owned(),
        sha256: target.sha256,
        method,
        look,
        writable,
        checked,
    }
}

/// the record of a file at `path` found to hold the target's bytes. a link a run cut off before
/// it wrote the state placed is the cached object under another name, so it is recorded as a
/// hardlink, which the audit covers and whose read-only flag is never cleared.
fn adoption(target: &Target, look: Look, path: &Path, cache: &Cache, checked: u64) -> Placed {
    if same_file::is_same_file(path, cache.object_path(&target.sha256)).unwrap_or(false) {
        placed(target, Method::Hardlink, look, false, checked)
    } else {
        placed(target, Method::Copy, look, now_writable(path), checked)
    }
}

/// whether the file at `path`, seen as `look`, holds the target's bytes. only a file of the
/// target's size is hashed, and one that cannot be read does not.
pub(crate) fn holds_target(target: &Target, look: &Look, path: &Path) -> bool {
    look.size == target.size && Sha256::of_file(path).is_ok_and(|digest| digest == target.sha256)
}

pub(crate) fn now_writable(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| !metadata.permissions().readonly())
}

pub(super) fn local(path: &Path, err: &std::io::Error) -> Error {
    Error::io(format_args!("could not place {}", path.display()), err)
}

/// what stops a run at an object that could not be checked, sealed or marked for repair.
pub(crate) fn doubtful(doubt: &Doubt) -> Error {
    let object = doubt.object.display();
    if doubt.changed {
        Error::io(
            format_args!("could not mark {object}, a cached file whose bytes changed"),
            &doubt.error,
        )
    } else {
        Error::io(
            format_args!("could not check and seal {object} for placement"),
            &doubt.error,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::{Level, Style};
    use crate::project::state::State;

    fn surveyed(project: &Project) -> Survey {
        Survey::read(
            project,
            "data",
            crate::threads::Threads::default(),
            &crate::exp::Experiments::default(),
        )
    }

    #[test]
    fn placements_are_checked_no_later_than_their_survey() {
        let dir = tempfile::tempdir().unwrap();
        // FAT32 and exFAT cannot hardlink, and only a hardlink can be vouched for
        if !crate::cache::tests::hardlinks(dir.path()) {
            return;
        }
        let cache = Cache::open(dir.path().join("cache")).unwrap();
        let bytes = b"station,temperature\nlerwick,9.1\n";
        let stored = crate::cache::tests::store(&cache, bytes);
        let locked = LockedFile {
            path: "weather.csv".to_owned(),
            size: bytes.len() as u64,
            sha256: stored.digest,
            md5: None,
            url: "https://example.org/weather.csv".to_owned(),
            unpacks_to: None,
        };
        let names = ["placed", "replaced", "adopted", "vouched"];
        let targets: Vec<Target> = names
            .iter()
            .map(|name| Target {
                dataset: "weather",
                path: format!("data/weather/{name}.csv"),
                sha256: stored.digest,
                size: bytes.len() as u64,
                copy: false,
                locked: &locked,
            })
            .collect();
        let project = Project {
            root: dir.path().join("project"),
        };
        fs::create_dir_all(project.root.join("data/weather")).unwrap();
        let mut state = State::default();
        for (index, target) in targets.iter().enumerate().skip(1) {
            let path = project.root.join(&target.path);
            fs::hard_link(cache.object_path(&stored.digest), &path).unwrap();
            if index != 2 {
                state.files.insert(
                    target.path.clone(),
                    Placed {
                        dataset: target.dataset.to_owned(),
                        sha256: target.sha256,
                        method: Method::Hardlink,
                        look: now(&path).unwrap(),
                        writable: false,
                        checked: 0,
                    },
                );
            }
        }
        project
            .write_state(&state, &crate::exp::Experiments::default())
            .unwrap();
        let mut survey = surveyed(&project);
        let started = 42;
        survey.started = started;
        let actions = vec![
            Action::Place,
            Action::Replace,
            Action::Adopt(survey.disk[&targets[2].path]),
            Action::Vouch(survey.disk[&targets[3].path]),
        ];
        let manifest =
            Manifest::parse("[datasets.weather]\nref = \"https://example.org/weather.csv\"\n")
                .unwrap();
        let placing = Placing {
            project: &project,
            manifest: &manifest,
            targets,
            unknown: Vec::new(),
            actions,
            survey,
        };
        placing
            .apply(
                &quiet(),
                &Work::default(),
                &cache,
                None,
                None,
                &std::sync::Arc::default(),
            )
            .unwrap();
        let (recorded, _) = project.state();
        for name in names {
            let checked = recorded.files[&format!("data/weather/{name}.csv")].checked;

            assert_eq!(checked, started, "{name} did not record the survey's start");
        }
    }

    fn quiet() -> Output {
        Output::new(
            Style {
                color: false,
                unicode: false,
                interactive: false,
                watched: false,
                links: false,
            },
            Level::Quiet,
            false,
            false,
        )
    }

    /// a project whose weather folder holds a copy an interrupted sync left behind, and a file
    /// of someone's own named much like it.
    fn interrupted(dir: &Path) -> (Project, Manifest, std::path::PathBuf, std::path::PathBuf) {
        let project = Project {
            root: dir.join("project"),
        };
        let folder = project.root.join("data").join("weather");
        fs::create_dir_all(&folder).unwrap();
        let leftover = folder.join(".fetchloom-0123456789abcdef.copy");
        fs::write(&leftover, b"station,").unwrap();
        let own = folder.join(".fetchloom-notes.copy");
        fs::write(&own, b"mine").unwrap();
        let manifest =
            Manifest::parse("[datasets.weather]\nref = \"https://example.org/weather.csv\"\n")
                .unwrap();
        (project, manifest, leftover, own)
    }

    #[test]
    fn a_copy_an_interrupted_sync_left_behind_is_swept() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path().join("cache")).unwrap();
        let (project, manifest, leftover, own) = interrupted(dir.path());
        let survey = surveyed(&project);
        let placing = Placing {
            project: &project,
            manifest: &manifest,
            targets: Vec::new(),
            unknown: Vec::new(),
            actions: Vec::new(),
            survey,
        };

        placing
            .apply(
                &quiet(),
                &Work::default(),
                &cache,
                None,
                None,
                &std::sync::Arc::default(),
            )
            .unwrap();

        assert!(!leftover.exists());
        assert_eq!(fs::read(&own).unwrap(), b"mine");
    }

    #[cfg(unix)]
    #[test]
    fn a_swapped_folder_cannot_redirect_a_staging_sweep() {
        let dir = tempfile::tempdir().unwrap();
        let (project, _, leftover, _) = interrupted(dir.path());
        let fixed = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(42);
        fs::File::open(&leftover)
            .unwrap()
            .set_modified(fixed)
            .unwrap();
        let survey = Survey::read(&project, "data", crate::threads::Threads::default());
        let root = project.root.join("data");
        let outside = dir.path().join("outside");
        fs::create_dir(&outside).unwrap();
        let foreign = outside.join(leftover.file_name().unwrap());
        fs::write(&foreign, b"foreign!").unwrap();
        fs::File::open(&foreign)
            .unwrap()
            .set_modified(fixed)
            .unwrap();
        fs::rename(root.join("weather"), root.join("saved")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("weather")).unwrap();
        let mut placer = Placer::new(&root);
        sweep(&project, &mut placer, &survey, &HashSet::new());
        assert_eq!(fs::read(&foreign).unwrap(), b"foreign!");
    }

    #[test]
    fn a_leftover_copy_keeps_a_settled_sync_from_skipping_its_sweep() {
        let dir = tempfile::tempdir().unwrap();
        let (project, manifest, _, _) = interrupted(dir.path());
        let mut survey = surveyed(&project);
        let seen = Seen {
            manifest: crate::digest::sha256(b"data.toml"),
            lock: Some(crate::digest::sha256(b"data.lock")),
        };
        survey.state.synced = Some(Synced {
            manifest: seen.manifest,
            lock: crate::digest::sha256(b"data.lock"),
            complete: true,
        });

        let found = untouched(&project, &manifest, &survey, &seen, false).unwrap();

        assert!(found.is_none(), "a settled sync would leave the copy");
    }

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
