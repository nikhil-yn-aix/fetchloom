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
pub(crate) use self::tell::{tell_preview, unreached_said};
use super::Scope;
use super::disk::{Blocker, Entry, LeftBehind, Retired, Survey};
use crate::cache::{Cache, Doubt};
use crate::digest::Sha256;
use crate::error::Error;
use crate::output::Output;
use crate::place::{self, Method, Placer};
use crate::plan::{self, Target};
use crate::project::Project;
use crate::project::lock::{Lock, LockedFile};
use crate::project::manifest::Manifest;
use crate::project::state::{Look, Placed, State, Synced};
use crate::stop::Stop;
use crate::work::Work;

#[derive(Debug, Default, Serialize)]
pub(crate) struct Placement {
    pub(crate) placed: u64,
    pub(crate) replaced: u64,
    pub(crate) removed: u64,
    pub(crate) unchanged: u64,
    /// files left alone: changed by hand, or never placed by fetchloom.
    pub(crate) kept: Vec<Kept>,
    /// files replaced though no edit of this project's showed: hardlinks to the cache's copy,
    /// which an edit through a link changed, here or in another project, and files `--verify`
    /// found changed under an unchanged size and time.
    pub(crate) restored: Vec<String>,
    /// files `--force` replaced that sync would otherwise keep, with the reason it would have.
    pub(crate) overwritten: Vec<Kept>,
    /// the files of `restored` that were links to the cache's changed copy, with the name that
    /// copy was kept under beside each, so an edit made in place through a link is not lost.
    pub(crate) changed_copies: Vec<ChangedCopy>,
    /// files fetchloom placed that were gone, deleted or moved by someone, and that it placed
    /// again since data.lock still pins them. `placed` counts them.
    pub(crate) put_back: Vec<String>,
    /// recipes whose pinned results a sync of every dataset left out, since neither the cache nor
    /// a mirror holds them and only `fetchloom build` makes them.
    pub(crate) unbuilt: Vec<String>,
    /// exclude patterns of datasets in the run that name files inside an archive, which they
    /// leave where they are.
    #[serde(skip)]
    pub(crate) unreached: Vec<plan::Unreached>,
    /// those of `restored` that were copies or reflinks, which only `--verify` finds changed.
    #[serde(skip)]
    pub(crate) repaired: HashSet<String>,
    /// entries `--force` removed from the way of placed files, by path.
    #[serde(skip)]
    pub(crate) cleared: Vec<(String, Entry)>,
    /// files that were already at their places with the pinned bytes, which sync took as placed
    /// and counts as unchanged, by dataset.
    #[serde(skip)]
    pub(crate) adopted: BTreeMap<String, Vec<String>>,
    /// those of `adopted` now linked to the cache's copy of their bytes.
    #[serde(skip)]
    pub(crate) relinked: HashSet<String>,
    /// the files `removed` counts that were already there when sync adopted them.
    #[serde(skip)]
    pub(crate) removed_adopted: Vec<String>,
    /// files `--force` moved aside to replace them, kept until what replaces them is in place.
    #[serde(skip)]
    set_aside: Vec<SetAside>,
    /// folders no dataset uses now that still hold files fetchloom did not place, so they stay.
    #[serde(skip)]
    pub(crate) retired: Vec<Retired>,
    /// of `removed`, the files of each dataset `place = false` keeps out of the data folder now.
    #[serde(skip)]
    pub(crate) unplaced: BTreeMap<String, u64>,
    /// files fetchloom placed before `data_dir` changed, which stay outside the data folder.
    #[serde(skip)]
    pub(crate) left_behind: Option<LeftBehind>,
}

impl Placement {
    /// notes what a placing tells beside its counts: the files it places that the state records
    /// as placed, which someone deleted or moved while data.lock still pins them, and the
    /// excludes of datasets in `scope` that name files inside an archive.
    fn notice(
        &mut self,
        manifest: &Manifest,
        targets: &[Target],
        actions: &[Action],
        survey: &Survey,
        scope: &Scope,
    ) {
        self.put_back = targets
            .iter()
            .zip(actions)
            .filter(|(target, action)| {
                matches!(action, Action::Place) && survey.state.files.contains_key(&target.path)
            })
            .map(|(target, _)| target.path.clone())
            .collect();
        self.unreached = plan::unreached(manifest, targets)
            .into_iter()
            .filter(|found| scope.includes(&found.dataset))
            .collect();
    }
}

/// a file `--force` moved aside to replace it, which goes once what replaces it is in place and
/// goes back where that failed.
#[derive(Debug)]
struct SetAside {
    /// its path in the project.
    path: String,
    /// the hidden name beside it that it was moved to.
    aside: std::path::PathBuf,
    /// the files placed in its stead, by path and the bytes data.lock pins there: the one at its
    /// path, or those under it for a file where a folder belongs.
    stead: Vec<(String, Sha256)>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ChangedCopy {
    pub(crate) path: String,
    pub(crate) kept_as: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct Kept {
    pub(crate) path: String,
    pub(crate) reason: Reason,
    /// for reason `blocked`, the entry in the way, as `blocked_by`: its path in the project.
    #[serde(
        rename = "blocked_by",
        serialize_with = "blocker_path",
        skip_serializing_if = "Option::is_none"
    )]
    pub(crate) blocker: Option<Blocker>,
}

impl Kept {
    fn new(path: String, reason: Reason) -> Kept {
        Kept {
            path,
            reason,
            blocker: None,
        }
    }
}

#[expect(
    clippy::ref_option,
    reason = "serde hands a field to its serializer by reference"
)]
fn blocker_path<S: serde::Serializer>(
    blocker: &Option<Blocker>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match blocker {
        Some(blocker) => serializer.serialize_str(&blocker.path),
        None => serializer.serialize_none(),
    }
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
    /// files already at the place of a file whose hash only its download can learn, so only
    /// the download tells whether they hold the pinned bytes.
    pub(crate) check: Vec<String>,
    /// files fetchloom did not place that already hold the pinned bytes, which sync would take
    /// as placed.
    pub(crate) adopt: Vec<String>,
}

impl Preview {
    /// whether applying it would leave everything as it is, with nothing to report.
    pub(crate) fn changes_nothing(&self) -> bool {
        let placement = &self.placement;
        placement.placed + placement.replaced + placement.removed == 0
            && placement.kept.is_empty()
            && placement.unbuilt.is_empty()
            && self.changes.download.is_empty()
            && self.changes.unpack.is_empty()
            && self
                .by_dataset
                .values()
                .all(|planned| planned.adopt.is_empty())
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
    /// fetchloom placed it as a hardlink to the cache, and it changed since, while other names
    /// still share it. the cache neither holds nor records that copy, so nothing tells which
    /// project made the change.
    Shared,
    /// fetchloom did not place it, and it holds other bytes than the lock pins.
    Foreign,
    /// no dataset needs it now, but it changed since fetchloom placed it.
    Unneeded,
    /// another entry stands in its way: a link or special file at its path, or a file where a
    /// folder on its way belongs. `--force` replaces that entry, though never a folder that
    /// holds anything.
    Blocked,
}

enum Action {
    Keep,
    Place,
    /// `--force` removes the entry in the way, without following it, and places the file.
    Unblock(Blocker),
    Blocked(Blocker),
    /// the entry in the way is a file fetchloom placed that no dataset needs now, or a folder of
    /// only such files, so removing those first clears the way and the file is placed. one that
    /// changed since the scan stays, and keeps the file out as blocked.
    Freed(Blocker),
    Replace,
    /// `--force` replaces a file sync would otherwise keep for this reason, and names it.
    Overwrite(Reason),
    Adopt(Look),
    /// a hardlink a mark put in doubt that still holds the locked bytes: kept, with the time
    /// the scan began and how it looks by name now.
    Vouch(Look),
    Leave(Reason),
    /// a file of a dataset the run leaves alone: nothing happens to it, and nothing counts it.
    Unselected,
}

/// the data folder compared with a lock, with what each file needs decided before anything
/// downloads, so only the bytes placing lacks are fetched.
pub(crate) struct Placing<'a> {
    project: &'a Project,
    manifest: &'a Manifest,
    /// every file the lock places, those of datasets outside `scope` too, which stay wanted.
    targets: Vec<Target<'a>>,
    /// the archives of datasets in `scope` that this machine never unpacked.
    unknown: Vec<(&'a str, &'a LockedFile)>,
    actions: Vec<Action>,
    survey: Survey,
    /// the datasets this placing works on.
    scope: Scope,
    /// the recipes whose results it leaves out, as [`Placing::leave_out`] says.
    unbuilt: Vec<String>,
}

impl<'a> Placing<'a> {
    /// compares the data folder, as `survey` saw it, with `lock`, with each archive standing for
    /// the files the cache records it unpacked to. only files fetchloom did not place are hashed,
    /// to see whether they already hold the locked bytes. nothing is written, so a dry run can
    /// show what applying it would do. the files of datasets outside `scope` are left as they
    /// are.
    pub(crate) fn new(
        project: &'a Project,
        manifest: &'a Manifest,
        lock: &'a Lock,
        cache: &Cache,
        force: bool,
        scope: &Scope,
        survey: Survey,
    ) -> Result<Placing<'a>, Error> {
        let plan = plan::from_cache(manifest, lock, cache)?;
        Self::from_plan(project, manifest, plan, force, scope, survey)
    }

    /// compares an already validated plan without reading archive member records again.
    pub(crate) fn from_plan(
        project: &'a Project,
        manifest: &'a Manifest,
        plan: plan::Plan<'a>,
        force: bool,
        scope: &Scope,
        mut survey: Survey,
    ) -> Result<Placing<'a>, Error> {
        project.planned_data_dir(manifest)?;
        survey.retarget(&project.root, &plan.targets);
        let _span = tracing::debug_span!("decide").entered();
        let mut actions = plan
            .targets
            .iter()
            .map(|target| {
                if scope.includes(target.dataset) {
                    decide(target, &survey, force, &project.root.join(&target.path))
                } else {
                    Ok(Action::Unselected)
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        let unknown: Vec<_> = plan
            .unknown
            .into_iter()
            .filter(|(dataset, _)| scope.includes(dataset))
            .collect();
        if actions
            .iter()
            .any(|action| matches!(action, Action::Blocked(_) | Action::Unblock(_)))
        {
            let wanted: HashSet<&str> = plan
                .targets
                .iter()
                .map(|target| target.path.as_str())
                .collect();
            let going = removable(&survey, &wanted, &unknown, scope, &project.root);
            actions = actions
                .into_iter()
                .map(|action| clearing(action, &survey, &going, scope))
                .collect();
        }
        Ok(Placing {
            project,
            manifest,
            targets: plan.targets,
            unknown,
            actions,
            survey,
            scope: scope.clone(),
            unbuilt: Vec::new(),
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

    /// the files placing needs that the cache lacks, each once by its SHA-256: whole files and
    /// the files archives unpack to alike, as a mirror holds them.
    pub(crate) fn wanted(&self, cache: &Cache) -> Vec<crate::mirror::Wanted> {
        let mut unique = HashSet::new();
        let needed: Vec<&Target> = self
            .targets
            .iter()
            .zip(&self.actions)
            .filter(|(target, action)| needs_bytes(action) && unique.insert(target.sha256))
            .map(|(target, _)| target)
            .collect();
        let found = place::run_all(&needed, self.survey.threads, |target| {
            cache.find(&target.sha256, Some(target.size)).is_some()
        });
        needed
            .into_iter()
            .zip(found)
            .filter(|(_, found)| !found)
            .map(|(target, _)| crate::mirror::Wanted {
                digest: target.sha256,
                size: target.size,
                label: target.path.clone(),
            })
            .collect()
    }

    /// the recipes in scope whose result placing needs bytes of that `holds` says the cache
    /// lacks, by name. nothing downloads a result; only a build makes it again. bytes a fetched
    /// file this placing needs holds too come with its download.
    pub(crate) fn unbuilt(&self, holds: impl Fn(&Sha256, u64) -> bool) -> Vec<String> {
        let needed = || {
            self.targets
                .iter()
                .zip(&self.actions)
                .filter(|(_, action)| needs_bytes(action))
                .map(|(target, _)| target)
        };
        let fetched: HashSet<Sha256> = needed()
            .filter(|target| target.holder != plan::Holder::Result)
            .map(|target| target.sha256)
            .collect();
        let mut names: Vec<String> = needed()
            .filter(|target| {
                target.holder == plan::Holder::Result
                    && !fetched.contains(&target.sha256)
                    && !holds(&target.sha256, target.size)
            })
            .map(|target| target.dataset.to_owned())
            .collect();
        names.sort();
        names.dedup();
        names
    }

    /// leaves out the results of the recipes `names`, which [`Placing::unbuilt`] found nowhere:
    /// their files already in place stay, the rest wait for a build, and what an older pin
    /// placed where those go leaves as any file no dataset needs does. the run then counts as
    /// incomplete.
    pub(crate) fn leave_out(&mut self, names: &[String]) {
        let (targets, actions) = std::mem::take(&mut self.targets)
            .into_iter()
            .zip(std::mem::take(&mut self.actions))
            .filter(|(target, action)| {
                !(target.holder == plan::Holder::Result
                    && needs_bytes(action)
                    && names.iter().any(|name| name == target.dataset))
            })
            .unzip();
        self.targets = targets;
        self.actions = actions;
        self.unbuilt.extend_from_slice(names);
    }

    /// what placing needs that `holds` says the cache lacks, as [`Placing::missing`] lists it.
    fn lacking(
        &self,
        holds: impl Fn(&Sha256, u64) -> Result<bool, Error> + Sync,
    ) -> Result<Vec<(String, &'a LockedFile)>, Error> {
        let needed: Vec<&Target> = self
            .targets
            .iter()
            .zip(&self.actions)
            .filter(|(_, action)| needs_bytes(action))
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
        // a recipe's result is made by a build, so nothing downloads it
        Ok(needed
            .into_iter()
            .filter(|target| !present.contains(&target.sha256))
            .filter_map(|target| Some((target.dataset, target.file()?)))
            .filter(|(_, file)| seen.insert(file.sha256))
            .map(|(dataset, file)| (dataset.to_owned(), file))
            .collect())
    }

    /// what [`Placing::apply`] would do, decided as it decides and with nothing written. a file
    /// that changes between this look and placing is still left alone when it is placed for real.
    /// a file already at the place of one whose hash is a dry run's stand-in, `guessed` as
    /// `<dataset>/<path>`, is one to check, since only the download tells whether it holds the
    /// pinned bytes.
    pub(crate) fn preview(&self, guessed: &HashSet<String>) -> Preview {
        let mut preview = Preview::default();
        self.preview_unwanted(&mut preview);
        preview.placement.unbuilt.clone_from(&self.unbuilt);
        preview.placement.notice(
            self.manifest,
            &self.targets,
            &self.actions,
            &self.survey,
            &self.scope,
        );
        let placement = &mut preview.placement;
        for (target, action) in self.targets.iter().zip(&self.actions) {
            if matches!(action, Action::Unselected) {
                continue;
            }
            if !guessed.is_empty()
                && self.survey.disk.contains_key(&target.path)
                && target.file().is_some_and(|file| {
                    guessed.contains(&format!("{}/{}", target.dataset, file.path))
                })
            {
                preview
                    .by_dataset
                    .entry(target.dataset.to_owned())
                    .or_default()
                    .check
                    .push(target.path.clone());
                continue;
            }
            match action {
                Action::Unselected => {}
                Action::Keep | Action::Vouch(_) => placement.unchanged += 1,
                Action::Adopt(_) => {
                    placement.unchanged += 1;
                    if !self.survey.state.files.contains_key(&target.path) {
                        preview
                            .by_dataset
                            .entry(target.dataset.to_owned())
                            .or_default()
                            .adopt
                            .push(target.path.clone());
                    }
                }
                Action::Leave(reason) => {
                    placement.kept.push(Kept::new(target.path.clone(), *reason));
                }
                Action::Blocked(blocker) => placement.kept.push(Kept {
                    blocker: Some(blocker.clone()),
                    ..Kept::new(target.path.clone(), Reason::Blocked)
                }),
                Action::Replace | Action::Overwrite(_) | Action::Unblock(_) => {
                    if let Action::Overwrite(reason) = action {
                        placement
                            .overwritten
                            .push(Kept::new(target.path.clone(), *reason));
                    }
                    restore(&self.survey, &target.path, placement);
                    if !matches!(action, Action::Unblock(_))
                        && self.survey.elsewhere.contains(&target.path)
                        && let Some(kept_as) =
                            aside_names(&target.path, &self.targets).find(|name| {
                                fs::symlink_metadata(self.project.root.join(name)).is_err()
                            })
                    {
                        placement.changed_copies.push(ChangedCopy {
                            path: target.path.clone(),
                            kept_as,
                        });
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
                Action::Place | Action::Freed(_) => {
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
        let holds = still_placed(&self.survey, &self.project.root);
        let unwanted = unwanted(&self.survey, &wanted, &self.unknown, &self.scope, holds);
        let going: HashSet<&str> = unwanted.iter().map(|(path, _)| path.as_str()).collect();
        preview.placement.retired = self.survey.retired(&wanted, &going);
        preview.placement.left_behind = self.survey.left_behind(&wanted, &going);
        for (path, fate) in unwanted {
            match fate {
                Unwanted::Remove(_) => {
                    preview.placement.removed += 1;
                    let placed = self.survey.state.files.get(&path);
                    if let Some(placed) = placed.filter(|placed| unplaced(self.manifest, placed)) {
                        *preview
                            .placement
                            .unplaced
                            .entry(placed.dataset.clone())
                            .or_default() += 1;
                    }
                    if placed.is_some_and(|placed| placed.adopted) {
                        preview.placement.removed_adopted.push(path.clone());
                    }
                    preview.changes.remove.push(path);
                }
                Unwanted::Keep => preview
                    .placement
                    .kept
                    .push(Kept::new(path, Reason::Unneeded)),
                Unwanted::Gone => {}
            }
        }
    }

    /// removes what no dataset needs, places what is missing or outdated, and records it, with
    /// `synced` marked complete when every file the lock asks for ended up in place. `told` is
    /// the dataset an add adds: its line stays out of what this tells, since the add tells it
    /// last, and when placing fails or stops, what this placed for it goes again, since the add
    /// is then rolled back.
    pub(crate) fn apply(
        self,
        output: &Output,
        work: &Work,
        cache: &Cache,
        synced: Option<Synced>,
        told: Option<&str>,
        stop: &std::sync::Arc<Stop>,
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
            scope,
            unbuilt,
        } = self;
        let data_dir = project.data_dir(manifest)?;
        let mut placement = Placement {
            unbuilt,
            ..Placement::default()
        };
        let wanted: HashSet<&str> = targets.iter().map(|target| target.path.as_str()).collect();
        let mut placer = Placer::new(&data_dir)
            .with_threads(survey.threads)
            .with_stop(std::sync::Arc::clone(stop));
        // removing first means a file that moved never collides with its own old copy
        let removed = remove_unwanted(
            project,
            manifest,
            &mut placer,
            &mut survey,
            &wanted,
            &unknown,
            &scope,
            &mut placement,
        )?;
        // told at once, since the state no longer records those files once this run writes it
        tell::left_behind(output, manifest, placement.left_behind.as_ref());
        let folders = swept(&scope, &targets, &actions);
        sweep(project, &mut placer, &survey, &wanted, folders.as_ref());
        let adding = told.map(|name| added_here(name, &targets, &actions));
        let found = found_in_place(&targets, &actions, &survey);
        placement.notice(manifest, &targets, &actions, &survey, &scope);
        let settled = settle(
            project,
            &mut placer,
            cache,
            &mut survey,
            &targets,
            actions,
            &mut placement,
        );
        let (todo, adopted) = match settled {
            Ok(settled) => settled,
            Err(error) => {
                settle_aside(project, &data_dir, survey.threads, &placement, |_| false);
                return Err(error);
            }
        };
        survey.smudge_edited();
        let needs_writing = survey.needs_writing;
        let checked = survey.started;
        let threads = survey.threads;
        let state = &mut survey.state;
        let (tallies, failure) = put_all(project, &mut placer, cache, &todo, state, checked, work);
        // one look at the stop decides flushing, taking back an add's files and the result, so
        // a stop asked between them cannot leave an add's files placed and the add rolled back
        let stopped = stop.is_asked();
        // place_all has flushed published copies; a stop discards its unfinished batch.
        let flushed = if stopped {
            Ok(())
        } else {
            placer
                .flush_copies()
                .map_err(|err| Error::io("could not flush the copied files to disk", &err))
        };
        // a run that placed everything it set out to links what it adopted to the cache, so a
        // failed add, which is rolled back, leaves those files as they were
        if !stopped && failure.is_none() && flushed.is_ok() {
            link_adopted(project, &placer, cache, work, &found, state, &mut placement);
        }
        settle_aside(project, &data_dir, threads, &placement, |(path, sha256)| {
            flushed.is_ok()
                && state
                    .files
                    .get(path)
                    .is_some_and(|placed| placed.checked == checked && placed.sha256 == *sha256)
        });
        // an add that fails is rolled back, so what it placed for its dataset goes too
        let undone = (stopped || failure.is_some() || flushed.is_err())
            && adding
                .is_some_and(|adding| undo(project, &data_dir, threads, state, &adding, checked));
        flushed?;
        // a run of named datasets never vouches for the others, so the next sync looks at all
        let whole = scope == Scope::All && unknown.is_empty() && placement.unbuilt.is_empty();
        let complete = whole && !stopped && failure.is_none() && placement.kept.is_empty();
        let synced = synced.map(|synced| Synced { complete, ..synced });
        let recorded = removed || adopted || undone || !tallies.is_empty();
        record(project, state, synced, recorded, needs_writing)?;
        if stopped {
            return Err(Stop::stopped());
        }
        if let Some(error) = failure {
            return Err(error);
        }
        tell(
            output, manifest, &placer, &tallies, &placement, told, &scope,
        );
        Ok(placement)
    }

    /// removes only what no dataset needs, and places nothing, so `remove` never downloads.
    pub(crate) fn remove_only(self, output: &Output) -> Result<Placement, Error> {
        let _span = tracing::debug_span!("remove").entered();
        let Placing {
            project,
            manifest,
            targets,
            mut survey,
            unknown,
            scope,
            ..
        } = self;
        let data_dir = project.data_dir(manifest)?;
        let mut placement = Placement::default();
        let wanted: HashSet<&str> = targets.iter().map(|target| target.path.as_str()).collect();
        let mut placer = Placer::new(&data_dir).with_threads(survey.threads);
        let removed = remove_unwanted(
            project,
            manifest,
            &mut placer,
            &mut survey,
            &wanted,
            &unknown,
            &scope,
            &mut placement,
        )?;
        tell::left_behind(output, manifest, placement.left_behind.as_ref());
        survey.smudge_edited();
        let forgot = survey.state.synced.take().is_some();
        if removed || forgot || survey.needs_writing {
            project.write_state(&survey.state)?;
        }
        tell(
            output,
            manifest,
            &placer,
            &BTreeMap::new(),
            &placement,
            None,
            &scope,
        );
        Ok(placement)
    }
}

/// writes `synced` and what a run did to the state. what it placed, removed or took as placed,
/// `recorded`, must be written. a state that only settles files hashed as unchanged, or marks
/// the run synced, spares later runs work, so a project folder that takes no write, as on a full
/// disk, fails no run that changed nothing.
fn record(
    project: &Project,
    state: &mut State,
    synced: Option<Synced>,
    recorded: bool,
    needs_writing: bool,
) -> Result<(), Error> {
    let resynced = state.synced != synced;
    state.synced = synced;
    if recorded || needs_writing || resynced {
        match project.write_state(state) {
            Err(error) if !recorded => {
                tracing::debug!(error = %error, "could not settle the state");
            }
            written => written?,
        }
    }
    Ok(())
}

/// data.toml and data.lock as a sync read them before it began.
pub(crate) struct Seen {
    pub(crate) manifest: Sha256,
    /// none when the project had no data.lock yet.
    pub(crate) lock: Option<Sha256>,
}

/// whether `action` places bytes the cache must hold.
fn needs_bytes(action: &Action) -> bool {
    matches!(
        action,
        Action::Place
            | Action::Replace
            | Action::Overwrite(_)
            | Action::Unblock(_)
            | Action::Freed(_)
    )
}

/// what a sync with nothing to do placed, found without reading what archives unpack to or
/// planning: data.toml and data.lock are byte for byte as a sync that left every file they ask
/// for in place left them, so the state records exactly those files, and each one still looks as
/// recorded. files as new as the state that hashing found unchanged need only a state written
/// again, which the caller does. anything else, or any doubt the survey raised, takes the long
/// way. a dry run passes `make` false so a missing data folder is not created.
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
        || !survey.edited.is_empty()
        || !survey.elsewhere.is_empty()
        || !survey.damaged.is_empty()
        || !survey.vouched.is_empty()
        || survey.leftovers().next().is_some()
        || state.files.iter().any(|(path, placed)| {
            survey.disk.get(path) != Some(&placed.look)
                // a link made writable leaves the cache's copy open to edits until sealed again
                || (placed.method == Method::Hardlink
                    && !placed.writable
                    && survey.writable.contains(path))
        })
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

/// the files fetchloom did not place that already hold their target's bytes, those sync adopts,
/// each with how the scan saw it, for [`link_adopted`]. a dataset that asks for writable copies
/// keeps its own files as they are.
fn found_in_place<'t, 'a>(
    targets: &'t [Target<'a>],
    actions: &[Action],
    survey: &Survey,
) -> Vec<(&'t Target<'a>, Look)> {
    targets
        .iter()
        .zip(actions)
        .filter_map(|(target, action)| match action {
            Action::Adopt(seen)
                if !target.copy && !survey.state.files.contains_key(&target.path) =>
            {
                Some((target, *seen))
            }
            _ => None,
        })
        .collect()
}

/// links each file of `found` that sync adopted to the cache's copy of its bytes, so one copy
/// stays on disk, while the file still looks as the scan saw it. a file with another name stays,
/// since linking this one would free nothing, and so does one whose bytes the cache cannot vouch
/// for, or one on a drive that makes no hardlinks. a failure leaves the file as it was.
fn link_adopted(
    project: &Project,
    placer: &Placer,
    cache: &Cache,
    work: &Work,
    found: &[(&Target, Look)],
    state: &mut State,
    placement: &mut Placement,
) {
    if found.is_empty() {
        return;
    }
    let Ok(marks) = cache.doubts() else {
        return;
    };
    for (target, seen) in found {
        if placer.stop().is_asked() {
            break;
        }
        let Some(record) = state.files.get_mut(&target.path) else {
            continue;
        };
        let destination = project.root.join(&target.path);
        if !record.adopted || place::has_other_names(&destination) {
            continue;
        }
        let Ok(Some(object)) = cache.trusted(&target.sha256, Some(target.size), &marks) else {
            continue;
        };
        match placer.relink(&object, &destination, |now| Look::of(now) == *seen) {
            Ok(Some(linked)) => {
                record.method = Method::Hardlink;
                record.look = Look::of(&linked);
                record.writable = false;
                Work::add(&work.hardlinks, 1);
                placement.relinked.insert(target.path.clone());
            }
            Ok(None) => {}
            Err(err) => {
                tracing::debug!(path = %target.path, %err, "could not link an adopted file to the cache");
            }
        }
    }
}

/// the paths of `dataset`'s files a placing records afresh, each with whether it places the file
/// where nothing stood, rather than taking a file already there as placed.
fn added_here(dataset: &str, targets: &[Target], actions: &[Action]) -> Vec<(String, bool)> {
    targets
        .iter()
        .zip(actions)
        .filter(|(target, _)| target.dataset == dataset)
        .filter_map(|(target, action)| match action {
            Action::Place | Action::Freed(_) => Some((target.path.clone(), true)),
            Action::Adopt(_) => Some((target.path.clone(), false)),
            _ => None,
        })
        .collect()
}

/// takes back what this run recorded for the dataset an add adds, as `adding` names it, since
/// the add is rolled back when placing fails: a file it placed where nothing stood goes, unless
/// it changed since, and so does its record, while a file it took as placed stays and only its
/// record goes. a record of a file that cannot be removed stays, so a later sync removes it. a
/// stop asked for does not stop this, so the placer it removes with does not listen for one.
/// says whether the state changed.
fn undo(
    project: &Project,
    data_dir: &Path,
    threads: crate::threads::Threads,
    state: &mut crate::project::state::State,
    adding: &[(String, bool)],
    checked: u64,
) -> bool {
    let mut placer = Placer::new(data_dir).with_threads(threads);
    let mut changed = false;
    for (path, placed_here) in adding {
        let Some(record) = state.files.get(path) else {
            continue;
        };
        if record.checked != checked {
            continue;
        }
        if *placed_here {
            let file = project.root.join(path);
            if now(&file) != Some(record.look) {
                continue;
            }
            if let Err(err) = placer.remove(&file, true, Some(record.method)) {
                tracing::debug!(%path, %err, "could not take back a file a failed add placed");
                continue;
            }
        }
        state.files.remove(path);
        changed = true;
    }
    changed
}

/// deletes each file `--force` moved aside once every file placed in its stead is in place, as
/// `in_place` says, and moves it back where one is not, so a forced replacement that failed or
/// stopped leaves what it would have replaced. a file that cannot go back, as when a folder now
/// stands at its name, stays under the hidden name it was moved to. a stop does not stop this, so
/// the placer it works with does not listen for one.
fn settle_aside(
    project: &Project,
    data_dir: &Path,
    threads: crate::threads::Threads,
    placement: &Placement,
    in_place: impl Fn(&(String, Sha256)) -> bool,
) {
    if placement.set_aside.is_empty() {
        return;
    }
    let mut placer = Placer::new(data_dir).with_threads(threads);
    for set in &placement.set_aside {
        let done = if set.stead.iter().all(&in_place) {
            placer.remove(&set.aside, false, None)
        } else {
            placer.move_back(&set.aside, &project.root.join(&set.path))
        };
        if let Err(err) = done {
            tracing::debug!(path = %set.path, %err, "a file --force moved aside stays where it was moved");
        }
    }
}

/// carries out every decision short of placing files: counts what stays, records files found to
/// hold the locked bytes, and removes files about to be replaced unless they changed since the
/// scan, moving those `--force` replaces aside instead. returns the targets left to place, and
/// whether the state gained a file or changed a record.
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
    // each entry in the way once, by path: whether it is gone
    let mut cleared: BTreeMap<String, bool> = BTreeMap::new();
    for (target, action) in targets.iter().zip(actions) {
        placer.stop().check()?;
        // a file that stays where another dataset placed it is this dataset's now, so its
        // removal follows this dataset
        if let Some(placed) = survey.state.files.get_mut(&target.path)
            && placed.dataset != target.dataset
            && !matches!(action, Action::Unselected)
        {
            target.dataset.clone_into(&mut placed.dataset);
            adopted = true;
        }
        match action {
            Action::Unselected => {}
            Action::Keep => placement.unchanged += 1,
            Action::Unblock(blocker) => {
                let gone = if let Some(gone) = cleared.get(&blocker.path) {
                    *gone
                } else {
                    let gone = clear(project, placer, survey, &blocker, placement)?;
                    cleared.insert(blocker.path.clone(), gone);
                    if gone {
                        placement
                            .cleared
                            .push((blocker.path.clone(), blocker.found));
                    }
                    gone
                };
                if gone {
                    if let Some(set) = placement
                        .set_aside
                        .iter_mut()
                        .find(|set| set.path == blocker.path)
                    {
                        set.stead.push((target.path.clone(), target.sha256));
                    }
                    placement.replaced += 1;
                    todo.push(target);
                } else {
                    placement.kept.push(Kept {
                        blocker: Some(blocker),
                        ..Kept::new(target.path.clone(), Reason::Blocked)
                    });
                }
            }
            // removing what no dataset needs came first, and cleared the way unless it found
            // what stood there changed
            Action::Freed(blocker) if now(&project.root.join(&blocker.path)).is_none() => {
                placement.placed += 1;
                todo.push(target);
            }
            Action::Blocked(blocker) | Action::Freed(blocker) => placement.kept.push(Kept {
                blocker: Some(blocker),
                ..Kept::new(target.path.clone(), Reason::Blocked)
            }),
            Action::Adopt(seen) => {
                adopt(project, cache, survey, target, seen, placement);
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
            Action::Leave(reason) => placement.kept.push(Kept::new(target.path.clone(), reason)),
            Action::Replace | Action::Overwrite(_) => {
                let destination = project.root.join(&target.path);
                match now(&destination) {
                    Some(look) if survey.disk.get(&target.path) != Some(&look) => {
                        let reason = if survey.state.files.contains_key(&target.path) {
                            Reason::Changed
                        } else {
                            Reason::Foreign
                        };
                        placement.kept.push(Kept::new(target.path.clone(), reason));
                        continue;
                    }
                    Some(_) => {
                        let forced = matches!(action, Action::Overwrite(_));
                        make_way(project, placer, survey, targets, target, forced, placement)?;
                    }
                    None => {}
                }
                if let Action::Overwrite(reason) = action {
                    placement
                        .overwritten
                        .push(Kept::new(target.path.clone(), reason));
                }
                restore(survey, &target.path, placement);
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

/// records the file at `target`'s place, seen as `seen`, which holds its bytes, as placed. a file
/// nothing recorded, other than a link to the cache's copy a run cut off before writing the state
/// left, was put there by someone, so its record and the run's lines say it was adopted.
fn adopt(
    project: &Project,
    cache: &Cache,
    survey: &mut Survey,
    target: &Target,
    seen: Look,
    placement: &mut Placement,
) {
    let destination = project.root.join(&target.path);
    let mut found = adoption(target, seen, &destination, cache, survey.started);
    if found.method != Method::Hardlink && !survey.state.files.contains_key(&target.path) {
        found.adopted = true;
        placement
            .adopted
            .entry(target.dataset.to_owned())
            .or_default()
            .push(target.path.clone());
    }
    survey.state.files.insert(target.path.clone(), found);
    placement.unchanged += 1;
}

/// counts a replaced file as restored when no edit of this project's put it in the survey.
fn restore(survey: &Survey, path: &str, placement: &mut Placement) {
    if survey.elsewhere.contains(path) {
        placement.restored.push(path.to_owned());
    } else if survey.damaged.contains(path) {
        placement.restored.push(path.to_owned());
        placement.repaired.insert(path.to_owned());
    }
}

/// the most names [`keep_aside`] tries beside one file.
const MOST_KEPT: u32 = 100;

/// takes the file at `target`'s place away for its pinned copy. a link to the cache's changed
/// copy is kept beside it instead, so an edit made in place through the link is not lost, and a
/// file `--force` replaces, as `forced` says, is moved aside until the pinned file is in place.
fn make_way(
    project: &Project,
    placer: &mut Placer,
    survey: &Survey,
    targets: &[Target<'_>],
    target: &Target,
    forced: bool,
    placement: &mut Placement,
) -> Result<(), Error> {
    let path = target.path.as_str();
    let method = survey.state.files.get(path).map(|placed| placed.method);
    if survey.elsewhere.contains(path) {
        let kept_as = keep_aside(project, placer, targets, path, method)?;
        placement.changed_copies.push(ChangedCopy {
            path: path.to_owned(),
            kept_as,
        });
        return Ok(());
    }
    let destination = project.root.join(path);
    if forced {
        let aside = placer
            .move_aside(&destination)
            .map_err(|err| local(&destination, &err))?;
        placement.set_aside.push(SetAside {
            path: path.to_owned(),
            aside,
            stead: vec![(path.to_owned(), target.sha256)],
        });
        return Ok(());
    }
    placer
        .remove(&destination, false, method)
        .map_err(|err| local(&destination, &err))
}

/// the names a changed copy of `path` may be kept under, in the order tried: `<path>.changed`,
/// then `<path>.changed.2` and on, skipping any name another locked file takes.
fn aside_names<'n>(path: &'n str, targets: &'n [Target<'_>]) -> impl Iterator<Item = String> + 'n {
    (1..=MOST_KEPT)
        .map(move |number| {
            if number == 1 {
                format!("{path}.changed")
            } else {
                format!("{path}.changed.{number}")
            }
        })
        .filter(move |name| targets.iter().all(|target| target.path != *name))
}

/// keeps the cache's changed copy, linked at `path`, under the first free name of
/// [`aside_names`], then takes `path` away for the pinned file. a copy nothing else names gets
/// the new name as a second link, which costs nothing. one other names still share, as other
/// projects' links do, is cloned or copied instead, so each project's changed copy is its own
/// and an edit of one changes no other. a name already there is refused, so no file is replaced.
/// returns that name.
fn keep_aside(
    project: &Project,
    placer: &mut Placer,
    targets: &[Target<'_>],
    path: &str,
    method: Option<Method>,
) -> Result<String, Error> {
    let from = project.root.join(path);
    let shared = place::has_other_names(&from);
    let writable = now_writable(&from);
    for name in aside_names(path, targets) {
        let aside = project.root.join(&name);
        let kept = if shared {
            placer.copy_file(&from, &aside, writable).map(drop)
        } else {
            placer.link_file(&from, &aside)
        };
        match kept {
            Ok(()) => {
                placer
                    .remove(&from, false, method)
                    .map_err(|err| local(&from, &err))?;
                return Ok(name);
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(err) => return Err(local(&aside, &err)),
        }
    }
    let full = std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        format!("{MOST_KEPT} changed copies are already kept beside it"),
    );
    Err(local(&from, &full))
}

/// removes the entry in the way of placing, without following it, once it still is what the
/// survey listed: a link or special file, an empty folder, or a file whose size and time are
/// unchanged, so a file written to since the scan stays. a file is moved aside instead, and
/// stays until the files placed in its stead are in place. says whether it is gone.
fn clear(
    project: &Project,
    placer: &mut Placer,
    survey: &Survey,
    blocker: &Blocker,
    placement: &mut Placement,
) -> Result<bool, Error> {
    let path = project.root.join(&blocker.path);
    let Ok(metadata) = fs::symlink_metadata(&path) else {
        return Ok(true);
    };
    let file_type = metadata.file_type();
    let unchanged = match blocker.found {
        Entry::File => {
            file_type.is_file() && survey.disk.get(&blocker.path) == Some(&Look::of(&metadata))
        }
        Entry::Folder => file_type.is_dir(),
        Entry::Link => file_type.is_symlink(),
        Entry::Special => !file_type.is_file() && !file_type.is_dir() && !file_type.is_symlink(),
    };
    if !unchanged {
        return Ok(false);
    }
    if blocker.found == Entry::File {
        let aside = placer.move_aside(&path).map_err(|err| {
            Error::io(
                format_args!("could not move {}, which is in the way", path.display()),
                &err,
            )
        })?;
        placement.set_aside.push(SetAside {
            path: blocker.path.clone(),
            aside,
            stead: Vec::new(),
        });
        return Ok(true);
    }
    let removed = if blocker.found == Entry::Folder {
        placer.delete_folder(&path)
    } else {
        // windows removes a link to a folder as a folder
        placer
            .remove(&path, false, None)
            .or_else(|err| match blocker.found {
                Entry::Link if cfg!(windows) => placer.delete_folder(&path),
                _ => Err(err),
            })
    };
    match removed {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == std::io::ErrorKind::DirectoryNotEmpty => Ok(false),
        Err(err) => Err(Error::io(
            format_args!("could not remove {}, which is in the way", path.display()),
            &err,
        )),
    }
}

/// what the file at `destination` needs, as `survey` saw it. a hardlink a mark put in doubt is
/// replaced when hashing found its bytes changed and kept when it found them intact, and a file
/// hashing found changed is an edit, though its size and time may not show either. a changed file
/// that holds the locked bytes is taken as placed if its mode fits the dataset: a run cut off
/// after it replaced a file and before it wrote the state leaves the new bytes recorded as old.
/// an entry in the way of a file that is not there stays unless `force` may replace it.
fn decide(
    target: &Target,
    survey: &Survey,
    force: bool,
    destination: &Path,
) -> Result<Action, Error> {
    let path = &target.path;
    let Some(look) = survey.disk.get(path) else {
        return Ok(match survey.blocker(path) {
            None => Action::Place,
            Some(blocker) if force && blocker.clearable => Action::Unblock(blocker),
            Some(blocker) => Action::Blocked(blocker),
        });
    };
    match survey.state.files.get(path) {
        Some(_) if survey.elsewhere.contains(path) || survey.damaged.contains(path) => {
            Ok(Action::Replace)
        }
        Some(placed) if survey.vouched.contains(path) => {
            let fits = fits(target, placed, survey);
            Ok(if placed.sha256 == target.sha256 && fits {
                Action::Vouch(*look)
            } else {
                Action::Replace
            })
        }
        Some(placed) if placed.look == *look && !survey.edited.contains(path) => {
            let fits = fits(target, placed, survey);
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
        Some(placed) => {
            let reason = if placed.method == Method::Hardlink && place::has_other_names(destination)
            {
                Reason::Shared
            } else {
                Reason::Changed
            };
            Ok(if force {
                Action::Overwrite(reason)
            } else {
                Action::Leave(reason)
            })
        }
        None => {
            let same = look.size == target.size
                && Sha256::of_file(destination).map_err(|err| local(destination, &err))?
                    == target.sha256;
            Ok(if same && (!target.copy || now_writable(destination)) {
                Action::Adopt(*look)
            } else if same {
                Action::Replace
            } else if force {
                Action::Overwrite(Reason::Foreign)
            } else {
                Action::Leave(Reason::Foreign)
            })
        }
    }
}

/// deletes files fetchloom placed for a dataset in `scope` that no dataset needs now, unless
/// someone changed them, counting those of datasets `manifest` keeps out of the data folder.
/// says whether the state lost a file.
#[expect(
    clippy::too_many_arguments,
    reason = "each is a part of the placing whose unwanted files go"
)]
fn remove_unwanted(
    project: &Project,
    manifest: &Manifest,
    placer: &mut Placer,
    survey: &mut Survey,
    wanted: &HashSet<&str>,
    unknown: &[(&str, &LockedFile)],
    scope: &Scope,
    placement: &mut Placement,
) -> Result<bool, Error> {
    let holds = still_placed(survey, &project.root);
    let unwanted = unwanted(survey, wanted, unknown, scope, holds);
    let changed = !unwanted.is_empty();
    let going: HashSet<&str> = unwanted.iter().map(|(path, _)| path.as_str()).collect();
    placement.retired = survey.retired(wanted, &going);
    placement.left_behind = survey.left_behind(wanted, &going);
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
                        if unplaced(manifest, &before) {
                            *placement.unplaced.entry(before.dataset).or_default() += 1;
                        }
                        if before.adopted {
                            placement.removed_adopted.push(path);
                        }
                    }
                    Some(_) => placement.kept.push(Kept::new(path, Reason::Unneeded)),
                    None => {}
                }
            }
            Unwanted::Keep => placement.kept.push(Kept::new(path, Reason::Unneeded)),
        }
    }
    Ok(changed)
}

/// whether `placed` is a file of a dataset `manifest` keeps out of the data folder with
/// `place = false`.
fn unplaced(manifest: &Manifest, placed: &Placed) -> bool {
    manifest
        .datasets
        .get(&placed.dataset)
        .is_some_and(|entry| !entry.place)
}

/// deletes what an interrupted placement left under the hidden names it stages files in, in
/// `folders` when it names some. the project lock keeps any other sync from staging files
/// meanwhile. a leftover that changed since the scan, or that cannot be deleted, stays for a
/// later sync.
fn sweep(
    project: &Project,
    placer: &mut Placer,
    survey: &Survey,
    wanted: &HashSet<&str>,
    folders: Option<&HashSet<&str>>,
) {
    for path in survey.leftovers() {
        if placer.stop().is_asked() {
            break;
        }
        let folder = path.rsplit_once('/').map_or("", |(folder, _)| folder);
        if folders.is_some_and(|folders| !folders.contains(folder)) {
            continue;
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

/// the folders a sweep of staging leftovers covers: those `actions` place files in for a run of
/// named datasets, and every folder, as none, for a run of all.
fn swept<'t>(
    scope: &Scope,
    targets: &'t [Target<'_>],
    actions: &[Action],
) -> Option<HashSet<&'t str>> {
    (*scope != Scope::All).then(|| {
        targets
            .iter()
            .zip(actions)
            .filter(|(_, action)| !matches!(action, Action::Unselected))
            .filter_map(|(target, _)| target.path.rsplit_once('/').map(|(folder, _)| folder))
            .collect()
    })
}

/// the files fetchloom placed that no dataset among `targets` needs now and nobody changed, which
/// sync removes before it places anything, so they stand in no file's way.
pub(crate) fn going(
    survey: &Survey,
    targets: &[Target],
    unknown: &[(&str, &LockedFile)],
    root: &Path,
) -> HashSet<String> {
    let wanted: HashSet<&str> = targets.iter().map(|target| target.path.as_str()).collect();
    removable(survey, &wanted, unknown, &Scope::All, root)
}

/// the files fetchloom placed for a dataset in `scope` that no target of `wanted` needs now and
/// nobody changed, which sync removes before it places anything.
fn removable(
    survey: &Survey,
    wanted: &HashSet<&str>,
    unknown: &[(&str, &LockedFile)],
    scope: &Scope,
    root: &Path,
) -> HashSet<String> {
    unwanted(survey, wanted, unknown, scope, still_placed(survey, root))
        .into_iter()
        .filter_map(|(path, fate)| matches!(fate, Unwanted::Remove(_)).then_some(path))
        .collect()
}

/// what becomes of a file an entry keeps out, as `action` decided: placed once removing what
/// sync removes first, `going`, clears the way. an entry that is a file of a dataset outside
/// `scope`, or a folder holding one, stays even with `--force` and names that dataset.
fn clearing(action: Action, survey: &Survey, going: &HashSet<String>, scope: &Scope) -> Action {
    let (Action::Blocked(blocker) | Action::Unblock(blocker)) = &action else {
        return action;
    };
    if survey.frees(blocker, going) {
        return Action::Freed(blocker.clone());
    }
    if *scope == Scope::All {
        return action;
    }
    let below = format!("{}/", blocker.path);
    let files = &survey.state.files;
    let alone = files
        .get(&blocker.path)
        .into_iter()
        .chain(
            files
                .range(below.clone()..)
                .take_while(|(path, _)| path.starts_with(&below))
                .map(|(_, placed)| placed),
        )
        .map(|placed| &placed.dataset)
        .find(|dataset| !scope.includes(dataset));
    match alone {
        Some(dataset) => Action::Blocked(Blocker {
            dataset: Some(dataset.clone()),
            ..blocker.clone()
        }),
        None => action,
    }
}

/// what becomes of each file fetchloom placed for a dataset in `scope` that no dataset needs
/// now. the files of a dataset with an archive this machine never unpacked stay, since nothing
/// tells which of them it still needs. one that no longer looks as placed goes only when `holds`
/// finds the placed bytes still in it; otherwise someone changed it, and it stays.
fn unwanted(
    survey: &Survey,
    wanted: &HashSet<&str>,
    unknown: &[(&str, &LockedFile)],
    scope: &Scope,
    holds: impl Fn(&str, &Placed, &Look) -> bool,
) -> Vec<(String, Unwanted)> {
    let unsure: HashSet<&str> = unknown.iter().map(|(dataset, _)| *dataset).collect();
    survey
        .state
        .files
        .iter()
        .filter(|(path, placed)| {
            !wanted.contains(path.as_str())
                && !unsure.contains(placed.dataset.as_str())
                && scope.includes(&placed.dataset)
        })
        .map(|(path, placed)| {
            let fate = match survey.disk.get(path) {
                None => Unwanted::Gone,
                Some(_) if survey.edited.contains(path) => Unwanted::Keep,
                Some(look) if *look == placed.look || holds(path, placed, look) => {
                    Unwanted::Remove(*look)
                }
                Some(_) => Unwanted::Keep,
            };
            (path.clone(), fate)
        })
        .collect()
}

/// whether a file fetchloom placed below `root`, seen as `look`, still holds the bytes it placed
/// there though it no longer looks as placed: a disk such as fat settles a file's time after it
/// is written. only a file of the placed size is hashed, and one that cannot be read does not.
fn still_placed<'s>(survey: &'s Survey, root: &'s Path) -> impl Fn(&str, &Placed, &Look) -> bool {
    move |path, placed, look| {
        let file = root.join(survey.listed.get(path).map_or(path, String::as_str));
        look.size == placed.look.size
            && Sha256::of_file(&file).is_ok_and(|digest| digest == placed.sha256)
    }
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
        adopted: false,
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

/// whether a file placed as `placed` has the mode its dataset asks for: writable for `copy =
/// true`, and otherwise no link to the cache's copy someone made writable, which leaves the copy
/// every project shares open to an edit.
pub(crate) fn fits(target: &Target, placed: &Placed, survey: &Survey) -> bool {
    if target.copy {
        placed.writable
    } else {
        placed.method != Method::Hardlink || !survey.writable.contains(&target.path)
    }
}

pub(super) fn local(path: &Path, err: &std::io::Error) -> Error {
    Error::io(format_args!("could not place {}", path.display()), err)
}

/// what stops a run at an object that could not be checked, sealed or marked for repair.
pub(crate) fn doubtful(doubt: &Doubt) -> Error {
    let object = doubt.object.display();
    if doubt.changed {
        Error::io(
            format_args!("could not note that {object}, a file in the cache, changed"),
            &doubt.error,
        )
    } else {
        Error::io(
            format_args!("could not check {object}, a file in the cache, before placing it"),
            &doubt.error,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::{Level, Style};
    use crate::project::state::State;

    /// a project's data folder cannot leave the project, so a sync whose cache is on another
    /// drive tells the person to move the project, while other reasons name the data folder.
    #[test]
    fn a_copy_warning_names_the_project_when_the_cache_is_on_another_drive() {
        assert!(
            tell::copy_warning(crate::place::Unlinked::OtherDrive, "data")
                .to_string()
                .starts_with(
                    "the cache is on another drive than the project, so files were copied. "
                )
        );
        assert_eq!(
            tell::copy_warning(crate::place::Unlinked::NoLinks, "data").to_string(),
            "the drive holding data cannot link files, so files were copied"
        );
    }

    /// a cached file that cannot be checked or noted as changed is named as a file in the cache,
    /// without the words fetchloom uses inside for those steps.
    #[test]
    fn a_cached_file_that_cannot_be_checked_is_named_plainly() {
        let object = Path::new("cache").join("objects").join("ab").join("cdef");
        let doubt = |changed| Doubt {
            object: object.clone(),
            changed,
            error: std::io::Error::other("the disk is full"),
        };
        assert_eq!(
            doubtful(&doubt(true)).message,
            format!(
                "could not note that {}, a file in the cache, changed: the disk is full",
                object.display()
            )
        );
        assert_eq!(
            doubtful(&doubt(false)).message,
            format!(
                "could not check {}, a file in the cache, before placing it: the disk is full",
                object.display()
            )
        );
    }

    fn surveyed(project: &Project) -> Survey {
        Survey::read(project, "data", crate::threads::Threads::default())
    }

    /// a file that stays where it is but now belongs to another dataset, as when two datasets in
    /// one folder trade a file, is recorded for its new dataset, which removal then follows.
    #[test]
    fn a_kept_file_is_recorded_for_the_dataset_that_places_it_now() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path().join("cache")).unwrap();
        let bytes = b"station,temperature\nlerwick,9.1\n";
        let locked = LockedFile {
            path: "weather.csv".to_owned(),
            size: bytes.len() as u64,
            sha256: crate::digest::sha256(bytes),
            md5: None,
            url: "https://example.org/weather.csv".to_owned(),
            unpacks_to: None,
        };
        let target = Target {
            dataset: "weather",
            path: "data/weather.csv".to_owned(),
            sha256: locked.sha256,
            size: locked.size,
            copy: false,
            holder: crate::plan::Holder::File(&locked),
        };
        let project = Project {
            root: dir.path().join("project"),
        };
        let placed = project.root.join(&target.path);
        fs::create_dir_all(placed.parent().unwrap()).unwrap();
        fs::write(&placed, bytes).unwrap();
        let mut state = State::default();
        state.files.insert(
            target.path.clone(),
            Placed {
                dataset: "climate".to_owned(),
                sha256: target.sha256,
                method: Method::Copy,
                look: now(&placed).unwrap(),
                writable: true,
                checked: 0,
                adopted: false,
            },
        );
        project.write_state(&state).unwrap();
        let manifest = Manifest::parse(
            "[datasets.weather]\nref = \"https://example.org/weather.csv\"\npath = \".\"\n",
        )
        .unwrap();
        let placing = Placing {
            project: &project,
            manifest: &manifest,
            targets: vec![target],
            unknown: Vec::new(),
            actions: vec![Action::Keep],
            survey: surveyed(&project),
            scope: Scope::All,
            unbuilt: Vec::new(),
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
        assert_eq!(recorded.files["data/weather.csv"].dataset, "weather");
    }

    /// a sync of named datasets never takes another dataset's file out of a named file's way,
    /// even with `--force`: the file stays, and the named file is kept out as blocked, naming
    /// the dataset the file is of. a sync of every dataset clears it first, as no dataset needs
    /// it now.
    #[test]
    fn a_named_sync_never_clears_another_datasets_file_from_the_way() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path().join("cache")).unwrap();
        let project = Project {
            root: dir.path().join("project"),
        };
        let data = project.root.join("data");
        fs::create_dir_all(&data).unwrap();
        // a dataset gone from data.toml placed a file where the named one now needs a folder
        let shared = data.join("shared");
        fs::write(&shared, b"old\n").unwrap();
        let mut state = State::default();
        state.files.insert(
            "data/shared".to_owned(),
            Placed {
                dataset: "old".to_owned(),
                sha256: crate::digest::sha256(b"old\n"),
                method: Method::Copy,
                look: now(&shared).unwrap(),
                writable: true,
                checked: 0,
                adopted: false,
            },
        );
        project.write_state(&state).unwrap();
        let manifest = Manifest::parse(
            "[datasets.new]\nref = \"https://example.org/new.csv\"\npath = \"shared\"\n",
        )
        .unwrap();
        let locked = LockedFile {
            path: "new.csv".to_owned(),
            size: 4,
            sha256: crate::digest::sha256(b"new\n"),
            md5: None,
            url: "https://example.org/new.csv".to_owned(),
            unpacks_to: None,
        };
        let plan = || plan::Plan {
            targets: vec![Target {
                dataset: "new",
                path: "data/shared/new.csv".to_owned(),
                sha256: locked.sha256,
                size: locked.size,
                copy: false,
                holder: crate::plan::Holder::File(&locked),
            }],
            unknown: Vec::new(),
        };
        let all = Placing::from_plan(
            &project,
            &manifest,
            plan(),
            true,
            &Scope::All,
            surveyed(&project),
        )
        .unwrap();
        let named = Scope::Only(std::collections::BTreeSet::from(["new".to_owned()]));
        let placing = Placing::from_plan(
            &project,
            &manifest,
            plan(),
            true,
            &named,
            surveyed(&project),
        )
        .unwrap();

        assert!(matches!(all.actions[..], [Action::Freed(_)]));
        let placed = placing
            .apply(
                &quiet(),
                &Work::default(),
                &cache,
                None,
                None,
                &std::sync::Arc::default(),
            )
            .unwrap();

        assert_eq!(fs::read(&shared).unwrap(), b"old\n");
        let [kept] = &placed.kept[..] else {
            panic!("{placed:?}")
        };
        assert_eq!(kept.reason, Reason::Blocked);
        assert_eq!(
            kept.blocker
                .as_ref()
                .and_then(|blocker| blocker.dataset.as_deref()),
            Some("old")
        );
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
                holder: crate::plan::Holder::File(&locked),
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
                        adopted: false,
                    },
                );
            }
        }
        project.write_state(&state).unwrap();
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
            scope: Scope::All,
            unbuilt: Vec::new(),
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

    /// `--force` takes an edited file's place only once the pinned file can take it: when placing
    /// fails, as when the cache lost the pinned bytes, the edit is still at its path, and when it
    /// works, nothing of the edit is left beside the pinned file.
    #[test]
    fn a_forced_replacement_keeps_the_edit_until_the_pinned_file_is_in_place() {
        let bytes = b"station,temperature\nlerwick,9.1\n";
        for cached in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let cache = Cache::open(dir.path().join("cache")).unwrap();
            let digest = if cached {
                crate::cache::tests::store(&cache, bytes).digest
            } else {
                crate::digest::sha256(bytes)
            };
            let locked = LockedFile {
                path: "weather.csv".to_owned(),
                size: bytes.len() as u64,
                sha256: digest,
                md5: None,
                url: "https://example.org/weather.csv".to_owned(),
                unpacks_to: None,
            };
            let target = Target {
                dataset: "weather",
                path: "data/weather/weather.csv".to_owned(),
                sha256: digest,
                size: bytes.len() as u64,
                copy: false,
                holder: crate::plan::Holder::File(&locked),
            };
            let project = Project {
                root: dir.path().join("project"),
            };
            let edited = project.root.join(&target.path);
            let folder = project.root.join("data/weather");
            fs::create_dir_all(&folder).unwrap();
            fs::write(&edited, b"my own edit\n").unwrap();
            let manifest =
                Manifest::parse("[datasets.weather]\nref = \"https://example.org/weather.csv\"\n")
                    .unwrap();
            let placing = Placing {
                project: &project,
                manifest: &manifest,
                targets: vec![target],
                unknown: Vec::new(),
                actions: vec![Action::Overwrite(Reason::Foreign)],
                survey: surveyed(&project),
                scope: Scope::All,
                unbuilt: Vec::new(),
            };

            let placed = placing.apply(
                &quiet(),
                &Work::default(),
                &cache,
                None,
                None,
                &std::sync::Arc::default(),
            );

            assert_eq!(placed.is_ok(), cached, "{placed:?}");
            let expected: &[u8] = if cached { bytes } else { b"my own edit\n" };
            assert_eq!(fs::read(&edited).unwrap(), expected);
            let names: Vec<String> = fs::read_dir(&folder)
                .unwrap()
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            assert_eq!(names, ["weather.csv"]);
        }
    }

    /// `--force` moves a file that stands where a dataset's folder belongs out of the way, and
    /// deletes it only once the files under that folder are in place: when placing them fails,
    /// its bytes are still in the data folder.
    #[test]
    fn a_file_in_the_way_stays_until_the_files_in_its_stead_are_in_place() {
        let bytes = b"station,temperature\nlerwick,9.1\n";
        for cached in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let cache = Cache::open(dir.path().join("cache")).unwrap();
            let digest = if cached {
                crate::cache::tests::store(&cache, bytes).digest
            } else {
                crate::digest::sha256(bytes)
            };
            let locked = LockedFile {
                path: "weather.csv".to_owned(),
                size: bytes.len() as u64,
                sha256: digest,
                md5: None,
                url: "https://example.org/weather.csv".to_owned(),
                unpacks_to: None,
            };
            let target = Target {
                dataset: "weather",
                path: "data/weather/weather.csv".to_owned(),
                sha256: digest,
                size: bytes.len() as u64,
                copy: false,
                holder: crate::plan::Holder::File(&locked),
            };
            let project = Project {
                root: dir.path().join("project"),
            };
            let data = project.root.join("data");
            fs::create_dir_all(&data).unwrap();
            fs::write(data.join("weather"), b"my notes\n").unwrap();
            let survey = surveyed(&project);
            let blocker = survey.blocker(&target.path).unwrap();
            let manifest =
                Manifest::parse("[datasets.weather]\nref = \"https://example.org/weather.csv\"\n")
                    .unwrap();
            let placing = Placing {
                project: &project,
                manifest: &manifest,
                targets: vec![target],
                unknown: Vec::new(),
                actions: vec![Action::Unblock(blocker)],
                survey,
                scope: Scope::All,
                unbuilt: Vec::new(),
            };

            let placed = placing.apply(
                &quiet(),
                &Work::default(),
                &cache,
                None,
                None,
                &std::sync::Arc::default(),
            );

            assert_eq!(placed.is_ok(), cached, "{placed:?}");
            let kept: Vec<Vec<u8>> = fs::read_dir(&data)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .filter(|path| path.is_file())
                .map(|path| fs::read(path).unwrap())
                .collect();
            if cached {
                assert_eq!(fs::read(data.join("weather/weather.csv")).unwrap(), bytes);
                assert!(kept.is_empty(), "{kept:?}");
            } else {
                assert_eq!(kept, [b"my notes\n"]);
            }
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
            scope: Scope::All,
            unbuilt: Vec::new(),
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
        let survey = surveyed(&project);
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
        sweep(&project, &mut placer, &survey, &HashSet::new(), None);
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
