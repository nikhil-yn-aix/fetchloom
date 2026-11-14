//! `fetchloom build`: runs the recipes in data.toml, each after the datasets it is made from,
//! pinning and placing what each wrote. data.lock is written once, after every step, so a step
//! that fails or is stopped leaves data.lock and the data folder as they were.

mod accept;
mod preview;
mod room;

pub(super) use accept::again;

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use serde::Serialize;

use self::preview::{Previewed, preview};
use super::args;
use super::fetch::data_folder::DataFolder;
use super::project::{
    Run, committed, find, hold, ignore_data, known, locked, refetched, register, sync_all,
    warn_recipe_code,
};
use super::sync::disk::Survey;
use super::sync::place::{Placement, Seen};
use super::sync::{Before, Loop, Scope, Upgrade, repinned};
use super::{Context, fetch, open_cache};
use crate::build::capture::{self, Fault, Refused};
use crate::build::compare::{Compared, compare};
use crate::build::step::{self, Ended, Ran};
use crate::build::{Staging, inputs};
use crate::cache::{Cache, Member};
use crate::digest::{self, Sha256};
use crate::error::{Error, Kind};
use crate::plan;
use crate::project::Project;
use crate::project::lock::Lock;
use crate::project::lock::derived::Derived;
use crate::project::manifest::Manifest;
use crate::project::manifest::recipes::Slot;
use crate::project::state::Candidate;
use crate::recipe::{self, Step};
use crate::units;

/// what stands in a dry run for a step's private folder, which a build makes in the cache.
const BUILD: &str = "<build>";

/// what `fetchloom build` did, or with `--dry-run` would do, as `--json` prints it.
#[derive(Serialize)]
#[serde(untagged)]
pub(super) enum Report {
    Built(Box<Built>),
    Previewed(Previewed),
}

#[derive(Default, Serialize)]
pub(super) struct Built {
    project: PathBuf,
    /// the recipes named, or every recipe when none was.
    selected: Vec<String>,
    /// the datasets whose pins changed: new, moved to other files, or dropped.
    datasets: Vec<String>,
    lock_changed: bool,
    /// each recipe the selected ones need, in the order the build took them.
    steps: Vec<Done>,
    /// what placing the datasets the build took did.
    placement: Option<Placement>,
}

/// one step of a build.
#[derive(Serialize)]
struct Done {
    name: String,
    action: Did,
    /// the key of its result, `sha256:` and its hash, which `--prepare` leaves out when a recipe
    /// it reads would run first.
    key: Option<String>,
    /// the tree of the files its result holds, `sha256:` and its hash, which `--prepare` leaves
    /// out when data.lock pins no result of it for its key.
    tree: Option<String>,
    files: u64,
    bytes: u64,
    /// how a rerun's files differ from those data.lock pins for its key, when it ran for a pin.
    check: Option<Compared>,
    /// the tree of a rerun that differs from data.lock's, which the state keeps for `--accept`.
    candidate: Option<String>,
    /// the log of its output, below the project, when it ran.
    log: Option<String>,
    /// how long it ran, when it ran.
    seconds: Option<f64>,
}

/// what a build did with a step.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Did {
    /// it ran, and data.lock pins what it made.
    Ran,
    /// an earlier build of the same key left its whole result in the cache, so it did not run.
    Reused,
    /// data.lock pins its result as building it now would make, and the cache holds it.
    UpToDate,
    /// up to date in data.lock, with a mirror giving the cache what it lacked, so it did not run.
    Fetched,
    /// data.lock pins the files a rebuild of it made that differed from the ones pinned before,
    /// as `--accept` asked.
    Accepted,
    /// `--check` could not run it, since data.lock's result of a recipe it reads is not in the
    /// cache or on a mirror, as after that recipe's check made other files.
    Skipped,
    /// `--prepare` put in the cache everything a build of it reads, and ran nothing.
    Prepared,
}

pub(super) async fn run(context: &Context, args: &args::Build) -> Result<Report, Error> {
    let project = find(&context.output)?;
    let (_, manifest) = project.manifest()?;
    recipes_only(&manifest, &args.names)?;
    let current = match (args.locked, args.check) {
        (true, true) => Some("build without --locked and --check"),
        (false, true) => Some("build without --check"),
        (true, false) => Some("build without --locked"),
        (false, false) => None,
    };
    if args.dry.dry_run {
        return preview(context, &project, &manifest, &args.names, current).map(Report::Previewed);
    }
    let _held = hold(context, &project, "build").await?;
    if args.accept {
        return accept::accept(context, &project, &args.names)
            .await
            .map(|built| Report::Built(Box::new(built)));
    }
    let mut building = Building::start(context, &project, &args.names, current).await?;
    if args.prepare {
        building.prepare(args).await?;
        return Ok(Report::Built(Box::new(building.report)));
    }
    building.room(args.check)?;
    if args.check {
        building.check().await?;
        return Ok(Report::Built(Box::new(building.report)));
    }
    building.steps().await?;
    building
        .finish()
        .await
        .map(|built| Report::Built(Box::new(built)))
}

/// refuses a name data.toml does not have, and a dataset fetched from a source.
fn recipes_only(manifest: &Manifest, names: &[String]) -> Result<(), Error> {
    for name in names {
        known(manifest, name, "build")?;
        if manifest.datasets[name].source().is_some() {
            return Err(Error::new(
                Kind::Usage,
                format!(
                    "{name} is fetched, not built; `{}` places it",
                    super::project::command("sync")
                ),
            )
            .help("name the recipes to build, the datasets data.toml makes with from and run"));
        }
    }
    Ok(())
}

/// the recipes `names` asks for, or every recipe when it names none.
fn selected(manifest: &Manifest, names: &[String]) -> Vec<String> {
    if names.is_empty() {
        manifest
            .datasets
            .iter()
            .filter(|(_, entry)| entry.recipe().is_some())
            .map(|(name, _)| name.clone())
            .collect()
    } else {
        names.to_vec()
    }
}

/// the build `--prepare` readied this machine for, as a person types it there: without the
/// network, with the recipes named and `--check` and `--locked` as `args` gave them.
fn prepared_for(args: &args::Build) -> String {
    let mut words = vec!["--offline".to_owned(), "build".to_owned()];
    for (given, flag) in [(args.check, "--check"), (args.locked, "--locked")] {
        if given {
            words.push(flag.to_owned());
        }
    }
    words.extend(
        args.names
            .iter()
            .map(|name| super::shell_word(name).into_owned()),
    );
    super::project::command(&words.join(" "))
}

/// a word of a command as a person reads it among the others: quoted when a space or a quote
/// in it would hide where it ends.
fn shown(word: &str) -> String {
    let plain = !word.is_empty()
        && !word.contains(|c: char| c.is_whitespace() || matches!(c, '\'' | '"' | '\\'));
    if plain {
        word.to_owned()
    } else {
        format!("'{}'", word.replace('\'', "'\\''"))
    }
}

/// a build under way: the project's hold is taken, and the lock it makes is in memory until
/// every step is done.
struct Building<'a> {
    context: &'a Context,
    work: Loop<'a>,
    manifest: Manifest,
    /// the hash of data.toml as the build read it, which the state records.
    manifest_read: Sha256,
    /// data.lock as the build read it, with the hash of its text, which a failed step leaves as
    /// it was.
    old: Option<(Lock, Sha256)>,
    /// data.lock with the sources the steps read relocked, and each result as a step made it.
    lock: Lock,
    before: Before,
    /// the recipes the build takes, in the order their steps run.
    recipes: Vec<String>,
    /// the least free space a step may leave on the disk holding the cache.
    floor: u64,
    report: Built,
}

/// what one step made.
struct Made {
    tree: Sha256,
    members: Vec<Member>,
    ran: Ran,
}

impl<'a> Building<'a> {
    /// starts a build of `names` in `project`, whose hold is taken: reads data.toml and data.lock
    /// again, and relocks the sources the steps read, as a sync of them would, without writing.
    async fn start(
        context: &'a Context,
        project: &'a Project,
        names: &[String],
        current: Option<&str>,
    ) -> Result<Building<'a>, Error> {
        let (text, manifest) = project.manifest()?;
        recipes_only(&manifest, names)?;
        let read = project.lock_and_text()?;
        let selected = selected(&manifest, names);
        let roots: Vec<&str> = selected.iter().map(String::as_str).collect();
        let closure: Vec<String> = recipe::closure(&manifest.datasets, &roots)
            .into_iter()
            .map(str::to_owned)
            .collect();
        let recipes = closure
            .iter()
            .filter(|name| manifest.datasets[*name].recipe().is_some())
            .cloned()
            .collect();
        let scope = Scope::Only(closure.into_iter().collect());
        if let Some(instead) = current {
            pinned_as_asked(project, &manifest, read.as_ref(), instead, &scope)?;
        }
        let old = read.map(|(lock, text)| (lock, digest::sha256(text.as_bytes())));
        let work =
            Loop::new(context, project, Arc::new(context.net()), open_cache()?).within(scope);
        let (lock, before) = work
            .relock(
                &manifest,
                old.as_ref().map(|(lock, _)| lock.clone()),
                &Upgrade::Nothing,
                Vec::new(),
            )
            .await
            .map_err(|error| work.before_placing(error))?;
        if let Some(instead) = current {
            let changed = repinned(&before, &lock);
            if !changed.is_empty() {
                return Err(Error::new(
                    Kind::Outdated,
                    format!(
                        "data.lock is out of date: building would change what it pins of {}",
                        units::listed(&changed, "and")
                    ),
                )
                .help(format!(
                    "run `{}` and commit data.lock, or {instead}",
                    super::project::command("lock")
                )));
            }
        }
        let floor = crate::space::floor(
            context.settings.build_floor.value,
            crate::space::total(work.cache.root()),
        );
        Ok(Building {
            context,
            work,
            manifest_read: digest::sha256(text.as_bytes()),
            manifest,
            old,
            lock,
            before,
            recipes,
            floor,
            report: Built {
                project: project.root.clone(),
                selected,
                ..Built::default()
            },
        })
    }

    /// builds each recipe in turn, stopping at the first that fails or makes other files than
    /// data.lock pins for its key, which keeps what the build did so far for `--json`.
    async fn steps(&mut self) -> Result<(), Error> {
        for name in self.recipes.clone() {
            let done = self
                .step(&name)
                .await
                .map_err(|error| error.with_report(&self.report))?;
            let kept = done.candidate.is_some();
            self.report.steps.push(done);
            if kept {
                let error = differs(&self.manifest, &self.report.steps);
                return Err(error.with_report(&self.report));
            }
        }
        Ok(())
    }

    /// runs every step again from the inputs data.lock pins and compares each result with its pin,
    /// going on past one that differs and stopping at one that fails; it never writes data.lock.
    async fn check(&mut self) -> Result<(), Error> {
        for name in self.recipes.clone() {
            let done = self
                .check_step(&name)
                .await
                .map_err(|error| error.with_report(&self.report))?;
            self.report.steps.push(done);
        }
        if self
            .report
            .steps
            .iter()
            .any(|step| step.candidate.is_some())
        {
            let error = differs(&self.manifest, &self.report.steps);
            return Err(error.with_report(&self.report));
        }
        let output = &self.context.output;
        if self.recipes.is_empty() {
            output.done(
                "Ok",
                "data.toml has no recipes, so there is nothing to check",
            );
        } else {
            output.done("Ok", "every step made the files data.lock pins");
        }
        Ok(())
    }

    /// runs the step of `name` again from the inputs data.lock pins: a rebuild that matches enters
    /// the cache as the pinned result, and one that differs is kept for `--accept`.
    async fn check_step(&mut self, name: &str) -> Result<Done, Error> {
        let output = &self.context.output;
        let step = Step::read(&self.work.project.root, &self.manifest, &self.lock, name)?
            .ok_or_else(|| Error::new(Kind::ProjectInvalid, format!("{name} is no recipe")))?;
        let key = keyed(&step)?;
        let Some(pinned) = self.lock.result(name).filter(|pinned| pinned.key == key) else {
            return Err(Error::new(
                Kind::Outdated,
                format!("data.lock is out of date: {name} changed since it was built"),
            ));
        };
        let (tree, files) = (pinned.tree, &pinned.files);
        for read in step.inputs.iter().filter(|read| read.made) {
            let made = self
                .lock
                .result(&read.name)
                .map_or(&[][..], |result| result.files.as_slice());
            if !self.fetched(&read.name, made).await? {
                output.mark(
                    "Skipped",
                    &format!(
                        "{name} · it reads {}'s result as data.lock pins it, which is not on this machine or a mirror",
                        read.name
                    ),
                );
                return Ok(done(Did::Skipped, name, key, tree, files));
            }
        }
        let sources: BTreeSet<&str> = step
            .inputs
            .iter()
            .filter(|read| !read.made)
            .map(|read| read.name.as_str())
            .collect();
        self.ready(&sources).await?;
        output.step_in(
            "Running",
            &format!("{name} · {}", self.command(name)),
            crate::output::Tone::Work,
        );
        let made = tokio::task::block_in_place(|| self.make(&step, key, Some(tree)))?;
        let compared = compare(files, &made.members);
        let mut done = done(Did::Ran, name, key, tree, files);
        done.log = Some(log_shown(name));
        done.seconds = Some(made.ran.took.as_secs_f64());
        if made.tree == tree {
            output.step(
                "Checked",
                &format!("{name} · {} data.lock", matched(&compared)),
            );
            forget(self.work.project, &[name.to_owned()])?;
        } else {
            output.mark(
                "Differs",
                &format!("{name} · {} data.lock", matched(&compared)),
            );
            self.keep(
                name,
                Candidate {
                    key,
                    tree: made.tree,
                    pinned: tree,
                    files: made.members,
                },
            )?;
            done.candidate = Some(format!("sha256:{}", made.tree));
        }
        done.check = Some(compared);
        Ok(done)
    }

    /// puts in the cache everything a build of the recipes, or with `--check` a check, needs,
    /// mirrors first, running nothing. the sources it pins anew go into data.lock, so the build its
    /// last line names, as `args` asks, needs no network either.
    async fn prepare(&mut self, args: &args::Build) -> Result<(), Error> {
        let check = args.check;
        let output = &self.context.output;
        let mut running: BTreeSet<String> = BTreeSet::new();
        for name in self.recipes.clone() {
            let step = Step::read(&self.work.project.root, &self.manifest, &self.lock, &name)?
                .ok_or_else(|| Error::new(Kind::ProjectInvalid, format!("{name} is no recipe")))?;
            let after = step.inputs.iter().any(|read| running.contains(&read.name));
            let key = step.key();
            if !check
                && !after
                && let Some(key) = key
                && let Some(done) = self.as_pinned(&name, key).await?
            {
                self.report.steps.push(done);
                continue;
            }
            let pinned = self
                .lock
                .result(&name)
                .filter(|pinned| Some(pinned.key) == key);
            for read in step.inputs.iter().filter(|read| read.made) {
                if !running.contains(&read.name)
                    && let Some(result) = self.lock.result(&read.name)
                {
                    self.fetched(&read.name, &result.files).await?;
                }
            }
            let sources: BTreeSet<&str> = step
                .inputs
                .iter()
                .filter(|read| !read.made)
                .map(|read| read.name.as_str())
                .collect();
            self.ready(&sources).await?;
            output.step(
                "Prepared",
                &format!("{name} · what it reads is on this machine"),
            );
            let (tree, files) = match pinned {
                Some(pinned) => (Some(pinned.tree), pinned.files.as_slice()),
                None => (None, &[][..]),
            };
            self.report.steps.push(Done {
                name: name.clone(),
                action: Did::Prepared,
                key: key.map(|key| format!("sha256:{key}")),
                tree: tree.map(|tree| format!("sha256:{tree}")),
                files: files.len() as u64,
                bytes: files.iter().map(|file| file.size).sum(),
                check: None,
                candidate: None,
                log: None,
                seconds: None,
            });
            running.insert(name);
        }
        if !check {
            self.ready_to_place().await?;
        }
        self.last_look()?;
        let written = self.work.write_lock(&self.before, &self.lock)?;
        self.report.lock_changed = written.is_some();
        self.report.datasets = repinned(&self.before, &self.lock);
        if self.recipes.is_empty() {
            output.done(
                "Ok",
                "data.toml has no recipes, so there is nothing to prepare",
            );
        } else {
            output.done(
                "Ok",
                "a build of these steps needs only what this machine holds now; nothing ran",
            );
            let build = prepared_for(args);
            if running.is_empty() {
                output.next(&format!(
                    "run `{build}`, which places the results without running a step"
                ));
            } else {
                output.next(&format!("run `{build}` where the steps should run"));
            }
        }
        Ok(())
    }

    /// puts in the cache what placing the build's fetched datasets needs that the data folder
    /// lacks, as for a dataset only skipped steps read; results are left to steps and `prepare`.
    async fn ready_to_place(&self) -> Result<(), Error> {
        let fetched = Lock {
            datasets: self
                .lock
                .datasets
                .iter()
                .filter(|locked| self.work.scope.includes(&locked.name))
                .cloned()
                .collect(),
            derived: Vec::new(),
        };
        let project = self.work.project;
        let cache = &self.work.cache;
        let mut survey = Survey::read(
            project,
            &self.manifest.data_dir,
            self.context.threads,
            &self.context.stop,
        )?;
        survey.check_doubted(&project.root, cache)?;
        let plan = plan::from_cache(&self.manifest, &fetched, cache)?;
        self.work
            .prepare(&self.manifest, &fetched, plan, false, survey)
            .await?;
        Ok(())
    }

    /// the build's last look for a stop, right before it writes data.lock: a stop asked by then
    /// leaves data.lock and the data folder as they were, and one asked later lets it finish.
    fn last_look(&self) -> Result<(), Error> {
        if self.context.stop.is_asked() {
            return Err(stopped().with_report(&self.report));
        }
        self.context.stop.finish();
        Ok(())
    }

    /// builds the recipe `name` as [`course`] decides. a pin whose key is still the recipe's keeps
    /// its files: other files a step makes for it are kept for a person to choose.
    async fn step(&mut self, name: &str) -> Result<Done, Error> {
        let project = self.work.project;
        let output = &self.context.output;
        let step = Step::read(&project.root, &self.manifest, &self.lock, name)?
            .ok_or_else(|| Error::new(Kind::ProjectInvalid, format!("{name} is no recipe")))?;
        let key = keyed(&step)?;
        if let Some(done) = self.as_pinned(name, key).await? {
            return Ok(done);
        }
        // a pin of this key still here is one whose files no mirror gave, so a run must remake it
        let expected = self
            .lock
            .result(name)
            .filter(|pinned| pinned.key == key)
            .map(|pinned| pinned.tree);
        let _hold = tokio::task::block_in_place(|| {
            self.work.cache.hold_key(&key, &self.context.stop, || {
                output.step("Waiting", &format!("for another fetchloom building {name}"));
            })
        })
        .map_err(|err| {
            unless_stopped(&err, || {
                format!("could not lock the build of {name} in the cache")
            })
        })?;
        // under the key's lock, so a build of it another clone finished meanwhile is reused
        let cache = &self.work.cache;
        if expected.is_none()
            && let Course::Built(built) =
                course(&self.lock, name, key, |files| whole(cache, files), cache)
        {
            output.step(
                "Reused",
                &format!(
                    "{name} · built before from the same inputs, {}",
                    sized(&built.members)
                ),
            );
            let done = done(Did::Reused, name, key, built.tree, &built.members);
            self.pin(&step, key, built.tree, built.members);
            return Ok(done);
        }
        let sources: BTreeSet<&str> = step
            .inputs
            .iter()
            .filter(|read| !read.made)
            .map(|read| read.name.as_str())
            .collect();
        self.ready(&sources).await?;
        output.step_in(
            "Running",
            &format!("{name} · {}", self.command(name)),
            crate::output::Tone::Work,
        );
        let made = tokio::task::block_in_place(|| self.make(&step, key, expected))?;
        if let Some(pinned) = self.lock.result(name).filter(|pinned| pinned.key == key)
            && pinned.tree != made.tree
        {
            let mut done = done(Did::Ran, name, key, pinned.tree, &pinned.files);
            let compared = compare(&pinned.files, &made.members);
            output.mark(
                "Differs",
                &format!("{name} · {} data.lock", matched(&compared)),
            );
            self.keep(
                name,
                Candidate {
                    key,
                    tree: made.tree,
                    pinned: pinned.tree,
                    files: made.members,
                },
            )?;
            done.check = Some(compared);
            done.candidate = Some(format!("sha256:{}", made.tree));
            done.log = Some(log_shown(name));
            done.seconds = Some(made.ran.took.as_secs_f64());
            return Ok(done);
        }
        output.step(
            "Built",
            &format!(
                "{name} · {} in {}",
                sized(&made.members),
                units::duration(made.ran.took)
            ),
        );
        let mut done = done(Did::Ran, name, key, made.tree, &made.members);
        done.log = Some(log_shown(name));
        done.seconds = Some(made.ran.took.as_secs_f64());
        self.pin(&step, key, made.tree, made.members);
        Ok(done)
    }

    /// the result of `name`, keyed `key`, as data.lock pins it, up to date or fetched from a mirror
    /// as [`course`] decides; none when the step must run or may be reused.
    async fn as_pinned(&self, name: &str, key: Sha256) -> Result<Option<Done>, Error> {
        let cache = &self.work.cache;
        let output = &self.context.output;
        match course(&self.lock, name, key, |files| whole(cache, files), cache) {
            Course::Held(pinned) => {
                output.step("Up to date", name);
                Ok(Some(done(
                    Did::UpToDate,
                    name,
                    key,
                    pinned.tree,
                    &pinned.files,
                )))
            }
            Course::Lacking(pinned) if self.fetched(name, &pinned.files).await? => {
                output.step(
                    "Fetched",
                    &format!("{name} · {} as data.lock pins it", sized(&pinned.files)),
                );
                Ok(Some(done(
                    Did::Fetched,
                    name,
                    key,
                    pinned.tree,
                    &pinned.files,
                )))
            }
            Course::Lacking(_) | Course::Built(_) | Course::New => Ok(None),
        }
    }

    /// keeps `candidate`, a rebuild of `name` that differs from data.lock, in the state for
    /// `--accept`, in place of any before, registering the project so `cache gc` keeps its files.
    fn keep(&self, name: &str, candidate: Candidate) -> Result<(), Error> {
        let project = self.work.project;
        let (mut state, _) = project.state();
        state.candidates.insert(name.to_owned(), candidate);
        project.write_state(&state)?;
        register(&self.work);
        Ok(())
    }

    /// puts in the cache the files of `sources`, fetched datasets a step reads, as [`Loop::ready`]
    /// does; its error names placed files linked to a changed cache copy, the reason to refetch.
    async fn ready(&self, sources: &BTreeSet<&str>) -> Result<(), Error> {
        let Err(error) = self.work.ready(&self.manifest, &self.lock, sources).await else {
            return Ok(());
        };
        if error.kind == Kind::Interrupted {
            return Err(error);
        }
        let project = self.work.project;
        let Ok(mut survey) = Survey::read(
            project,
            &self.manifest.data_dir,
            self.context.threads,
            &self.context.stop,
        ) else {
            return Err(error);
        };
        if survey
            .check_doubted(&project.root, &self.work.cache)
            .is_err()
        {
            return Err(error);
        }
        let scope = Scope::Only(sources.iter().map(|name| (*name).to_owned()).collect());
        Err(refetched(&survey, &scope)
            .into_iter()
            .fold(error, Error::detail))
    }

    /// takes the files of `name`'s pinned result the cache lacks from the data folder, else the
    /// mirrors, and says whether the cache holds all of `files` now.
    async fn fetched(&self, name: &str, files: &[Member]) -> Result<bool, Error> {
        let cache = &self.work.cache;
        let lacked: Vec<crate::mirror::Wanted> = files
            .iter()
            .filter(|file| cache.find(&file.sha256, Some(file.size)).is_none())
            .map(|file| crate::mirror::Wanted {
                digest: file.sha256,
                size: file.size,
                label: format!("{name}/{}", file.path),
            })
            .collect();
        if lacked.is_empty() {
            return Ok(true);
        }
        let folder = DataFolder::planned(self.work.project, &self.manifest, &self.lock, cache);
        fetch::mirrors::fill(self.context, &self.work.net, cache, &folder, || lacked).await?;
        Ok(whole(cache, files))
    }

    /// runs `step` in a folder of its own and takes in its result for `key`. a result other than
    /// `expected`, the tree data.lock pins, enters the cache unrecorded, so no build reuses it.
    fn make(&self, step: &Step, key: Sha256, expected: Option<Sha256>) -> Result<Made, Error> {
        let name = &step.name;
        let cache = &self.work.cache;
        let threads = self.context.threads;
        let staging = Staging::new(cache).map_err(|err| {
            let err = cache.explained(&cache.root().join("tmp"), err);
            Error::io(
                format_args!("could not make a folder for {name} in the cache"),
                &err,
            )
        })?;
        tracing::debug!(folder = %staging.folder().display(), "{name} runs");
        for read in &step.inputs {
            let files = input_files(&self.manifest, &self.lock, cache, &read.name)?;
            inputs::make(
                cache,
                &staging.input(&read.name),
                &files,
                threads,
                &self.context.stop,
            )
            .map_err(|err| {
                unless_stopped(&err, || format!("could not copy {} for {name}", read.name))
            })?;
        }
        let ran = self.run(step, &staging)?;
        self.same_code(step)?;
        let most = crate::source::most_files();
        let found = capture::read(&staging.out(), most, threads, &self.context.stop)
            .map_err(|fault| taken(name, fault))?;
        capture::publish(
            cache,
            &found,
            threads,
            &self.context.stop,
            &self.context.work,
        )
        .map_err(|fault| taken(name, fault))?;
        if expected.is_none_or(|expected| expected == found.tree) {
            cache.record_result(&key, &found.members).map_err(|err| {
                Error::io(
                    format_args!("could not record what {name} made in the cache"),
                    &err,
                )
            })?;
        }
        Ok(Made {
            tree: found.tree,
            members: found.members,
            ran,
        })
    }

    /// runs the command of `step` in `staging`, and fails unless it exits with 0.
    fn run(&self, step: &Step, staging: &Staging) -> Result<Ran, Error> {
        let name = &step.name;
        let project = self.work.project;
        let recipe = self.manifest.datasets[name]
            .recipe()
            .ok_or_else(|| Error::new(Kind::ProjectInvalid, format!("{name} is no recipe")))?;
        let folder = |slot: Slot<'_>| match slot {
            Slot::Input(input) => staging.input(input).into_os_string(),
            Slot::Out => staging.out().into_os_string(),
        };
        let command = step::Command {
            words: step::words(&recipe.run, &folder),
            folder: &project.root,
            env: &recipe.env,
        };
        let program = recipe.run.first().map_or("", |word| word.text());
        let log = log_path(project, name)?;
        let output = &self.context.output;
        let forward = |line: &str| output.item(line);
        let forward: Option<&dyn Fn(&str)> = output.shows_items().then_some(&forward);
        let cache = self.work.cache.root();
        let free = || crate::space::free(cache);
        let floor = step::Floor {
            bytes: self.floor,
            free: &free,
        };
        let ran = step::run(
            &command,
            &project.root,
            &log,
            forward,
            &self.context.stop,
            &floor,
        )
        .map_err(|err| cannot_start(name, program, &project.root, &err))?;
        let folder = plan::folder(&self.manifest, name);
        match ran.ended {
            Ended::Exited(0) => Ok(ran),
            Ended::Stopped => Err(stopped()),
            Ended::Low { free } => Err(Error::new(
                Kind::LocalIo,
                format!(
                    "stopped {name}: the disk holding the cache has less than {} free; the step had written {}",
                    units::size(self.floor),
                    units::size(staging.written())
                ),
            )
            .detail(format!(
                "{} is free on the disk holding {}",
                units::size(free),
                cache.display()
            ))
            .help(format!(
                "free space on that disk, or lower build_floor in the settings file or FETCHLOOM_BUILD_FLOOR; data.lock and {folder} are as they were"
            ))),
            ended @ Ended::Exited(_) => Err(failed(name, program, ended, &ran, &folder)),
            #[cfg(unix)]
            ended @ Ended::Signaled(_) => Err(failed(name, program, ended, &ran, &folder)),
        }
    }

    /// fails when `step` changed its own code while it ran, since its result would answer code
    /// that is no longer there.
    fn same_code(&self, step: &Step) -> Result<(), Error> {
        let project = self.work.project;
        let now = Step::read(&project.root, &self.manifest, &self.lock, &step.name)?;
        let changed: Vec<&str> = step
            .code
            .iter()
            .zip(now.iter().flat_map(|now| &now.code))
            .filter(|(before, after)| before != after)
            .map(|((path, _), _)| path.as_str())
            .collect();
        if changed.is_empty() {
            return Ok(());
        }
        Err(Error::new(
            Kind::BuildFailed,
            format!(
                "{} changed its own code while it ran: {}",
                step.name,
                changed.join(", ")
            ),
        )
        .help(format!(
            "a step writes only into {{out}}; data.lock and {} are as they were",
            plan::folder(&self.manifest, &step.name)
        )))
    }

    /// the command of the recipe `name` as a person reads it, `<build>` standing for its folder.
    fn command(&self, name: &str) -> String {
        let folder = |slot: Slot<'_>| match slot {
            Slot::Input(input) => OsString::from(format!("{BUILD}/in/{input}")),
            Slot::Out => OsString::from(format!("{BUILD}/out")),
        };
        self.manifest.datasets[name]
            .recipe()
            .map(|recipe| {
                let words: Vec<String> = step::words(&recipe.run, &folder)
                    .iter()
                    .map(|word| shown(&word.to_string_lossy()))
                    .collect();
                words.join(" ")
            })
            .unwrap_or_default()
    }

    fn pin(&mut self, step: &Step, key: Sha256, tree: Sha256, members: Vec<Member>) {
        pin(&mut self.lock, step, key, tree, members);
    }

    /// writes data.lock when the build changed it, then places every dataset the build took,
    /// as a sync of them would.
    async fn finish(mut self) -> Result<Built, Error> {
        let output = &self.context.output;
        if self.recipes.is_empty() {
            output.done(
                "Ok",
                "data.toml has no recipes, so there is nothing to build",
            );
            return Ok(self.report);
        }
        let old = self.old.as_ref().map(|(lock, _)| lock);
        let changed = old != Some(&self.lock);
        let mut datasets = repinned(&self.before, &self.lock);
        let rebuilt: Vec<String> = self
            .lock
            .derived
            .iter()
            .filter(|result| {
                old.and_then(|old| old.result(&result.name))
                    .is_none_or(|was| was.key != result.key || was.tree != result.tree)
            })
            .map(|result| result.name.clone())
            .collect();
        datasets.extend(rebuilt.iter().cloned());
        datasets.sort();
        datasets.dedup();
        let lock_read = if changed {
            self.last_look()?;
            let text = self.lock.render();
            let text = self.work.write_rendered(&self.before, &self.lock, text)?;
            self.work.project.sweep_lists(&self.lock);
            forget(self.work.project, &rebuilt)?;
            Some(digest::sha256(text.as_bytes()))
        } else {
            self.old.as_ref().map(|(_, read)| *read)
        };
        let seen = Seen {
            manifest: self.manifest_read,
            lock: lock_read,
        };
        let outcome = sync_all(
            &self.work,
            &self.manifest,
            Some(self.lock.clone()),
            None,
            Run::planned(&seen),
        )
        .await?;
        ignore_data(&self.work, &self.manifest)?;
        let built: Vec<&str> = self
            .report
            .steps
            .iter()
            .filter(|step| !matches!(step.action, Did::UpToDate | Did::Fetched))
            .map(|step| step.name.as_str())
            .collect();
        warn_recipe_code(output, self.work.project, &self.manifest);
        output.done("Ok", &closing(&built, &rebuilt));
        if changed {
            output.next(&format!("commit {}", committed(self.work.project)));
        }
        self.report.datasets = datasets;
        self.report.lock_changed = changed;
        self.report.placement = Some(outcome.placed);
        Ok(self.report)
    }
}

/// the last line of a build whose steps that did more than find their result are `built`, those
/// in `repinned` pinned anew; the others made just the files data.lock pinned.
fn closing(built: &[&str], repinned: &[String]) -> String {
    let (pinned, matched): (Vec<&str>, Vec<&str>) = built
        .iter()
        .partition(|name| repinned.iter().any(|repinned| repinned == *name));
    let recipes = |names: &[&str]| units::count(names.len() as u64, "recipe", "recipes");
    let pinned = match pinned.as_slice() {
        [] => None,
        [name] => Some(format!("pinned {name}'s result in data.lock")),
        many => Some(format!(
            "pinned the results of {} in data.lock",
            recipes(many)
        )),
    };
    let matched = match matched.as_slice() {
        [] => None,
        [name] => Some(format!("{name}'s result matches data.lock")),
        many => Some(format!("the results of {} match data.lock", recipes(many))),
    };
    match (pinned, matched) {
        (None, None) => "every recipe is built as data.lock pins it".to_owned(),
        (Some(said), None) | (None, Some(said)) => said,
        (Some(pinned), Some(matched)) => format!("{pinned}; {matched}"),
    }
}

/// pins `tree` of `step` in `lock`, keeping when the pin was made while its key and tree stay.
fn pin(lock: &mut Lock, step: &Step, key: Sha256, tree: Sha256, members: Vec<Member>) {
    let timestamp = match lock.result(&step.name) {
        Some(old) if old.key == key && old.tree == tree => old.timestamp.clone(),
        _ => crate::units::timestamp(SystemTime::now()),
    };
    let derived = Derived {
        name: step.name.clone(),
        from: step.from().unwrap_or_default(),
        run: step.run.clone(),
        code: step.hashed().unwrap_or_default(),
        env: step.env.clone(),
        key,
        timestamp,
        tree,
        files: members,
    };
    match lock
        .derived
        .binary_search_by(|result| result.name.as_str().cmp(&step.name))
    {
        Ok(at) => lock.derived[at] = derived,
        Err(at) => lock.derived.insert(at, derived),
    }
}

/// drops from the state the rebuilds kept of `names`, whose pins changed since, so none of them
/// answers data.lock any more.
fn forget(project: &Project, names: &[String]) -> Result<(), Error> {
    let (mut state, _) = project.state();
    let before = state.candidates.len();
    state.candidates.retain(|name, _| !names.contains(name));
    if state.candidates.len() == before {
        return Ok(());
    }
    project.write_state(&state)
}

/// fails with `lock.outdated` unless `read` pins every dataset of `scope` as data.toml asks, each
/// recipe as building it now would make; `instead` is the help's other way out.
fn pinned_as_asked(
    project: &Project,
    manifest: &Manifest,
    read: Option<&(Lock, String)>,
    instead: &str,
    scope: &Scope,
) -> Result<(), Error> {
    locked(
        project,
        manifest,
        read.map(|(lock, _)| lock),
        instead,
        scope,
    )
    .map_err(|error| match error.kind {
        // a build's own check compares with data.lock, so a recipe it does not pin is drift
        Kind::BuildNeeded => Error {
            kind: Kind::Outdated,
            ..error
        },
        _ => error,
    })
}

/// the key of `step`, once data.lock pins every dataset it reads and all its code is there.
fn keyed(step: &Step) -> Result<Sha256, Error> {
    if let Some(key) = step.key() {
        return Ok(key);
    }
    let name = &step.name;
    if let Some((path, _)) = step.code.iter().find(|(_, code)| code.is_none()) {
        return Err(Error::new(
            Kind::ProjectInvalid,
            format!("{name} cannot be built: its code {path} is not in the project"),
        )
        .help(format!(
            "bring {path} back: `git restore {}` if git has it, else ask whoever built {name} to commit it; take it out of code in data.toml only once the step no longer reads it",
            super::shell_word(path)
        )));
    }
    let unpinned: Vec<&str> = step
        .inputs
        .iter()
        .filter(|read| read.content.is_none())
        .map(|read| read.name.as_str())
        .collect();
    Err(Error::new(
        Kind::ProjectInvalid,
        format!(
            "{name} cannot be built: data.lock pins nothing of {}",
            unpinned.join(", ")
        ),
    ))
}

/// whether the cache holds every file of `files`.
fn whole(cache: &Cache, files: &[Member]) -> bool {
    files
        .iter()
        .all(|file| cache.find(&file.sha256, Some(file.size)).is_some())
}

/// what a build does with a step whose key it knows, before asking a mirror or running anything,
/// a decision its dry run shares, so the dry run says what the build then does.
enum Course<'l> {
    /// data.lock pins this key, and the cache holds every file of it: up to date.
    Held(&'l Derived),
    /// data.lock pins this key, and the cache lacks files of it: a mirror that holds them gives
    /// them, and otherwise the step runs again and must make the files data.lock pins.
    Lacking(&'l Derived),
    /// data.lock pins no result for this key, and a build of it left a whole result: reused.
    Built(crate::cache::Built),
    /// data.lock pins no result for this key, and no build left one: the step runs.
    New,
}

/// the course of the recipe `name` whose key is `key`, with `lock` as the build holds it then:
/// `holds` says whether the cache holds a set of files, and `cache` keeps what builds left.
fn course<'l>(
    lock: &'l Lock,
    name: &str,
    key: Sha256,
    holds: impl Fn(&[Member]) -> bool,
    cache: &Cache,
) -> Course<'l> {
    match lock.result(name).filter(|pinned| pinned.key == key) {
        Some(pinned) if holds(&pinned.files) => Course::Held(pinned),
        Some(pinned) => Course::Lacking(pinned),
        None => cache
            .results(&key)
            .into_iter()
            .next()
            .map_or(Course::New, Course::Built),
    }
}

/// what a step did, before it says where its log is and how long it ran.
fn done(action: Did, name: &str, key: Sha256, tree: Sha256, files: &[Member]) -> Done {
    Done {
        name: name.to_owned(),
        action,
        key: Some(format!("sha256:{key}")),
        tree: Some(format!("sha256:{tree}")),
        files: files.len() as u64,
        bytes: files.iter().map(|file| file.size).sum(),
        check: None,
        candidate: None,
        log: None,
        seconds: None,
    }
}

/// how many files `members` are and what they hold, as `1 file, 4 B`.
fn sized(members: &[Member]) -> String {
    format!(
        "{}, {}",
        units::count(members.len() as u64, "file", "files"),
        units::size(members.iter().map(|member| member.size).sum())
    )
}

/// the files of `name` as a step reads them: a result as data.lock pins it, or a fetched dataset
/// laid out as its folder holds it, each archive by the files it unpacked to.
fn input_files(
    manifest: &Manifest,
    lock: &Lock,
    cache: &Cache,
    name: &str,
) -> Result<Vec<inputs::File>, Error> {
    if manifest.datasets[name].recipe().is_some() {
        let result = lock.result(name).ok_or_else(|| {
            Error::new(
                Kind::ProjectInvalid,
                format!("data.lock pins no result of {name}"),
            )
        })?;
        return Ok(result
            .files
            .iter()
            .map(|member| inputs::File {
                path: member.path.clone(),
                sha256: member.sha256,
                size: member.size,
            })
            .collect());
    }
    let (locked, members) = recipe::source_read(manifest, lock, name).ok_or_else(|| {
        Error::new(
            Kind::ProjectInvalid,
            format!("data.lock pins nothing of {name}"),
        )
    })?;
    let mut planned = plan::Plan::default();
    plan::expand(
        &mut planned,
        name,
        "",
        false,
        &locked.files,
        members,
        &|archive| archive.recorded(cache),
    )?;
    if let Some((_, archive)) = planned.unknown.first() {
        return Err(Error::new(
            Kind::LocalIo,
            format!("{name}/{} is not unpacked in the cache", archive.path),
        )
        .help("run the same command again"));
    }
    Ok(planned
        .targets
        .into_iter()
        .map(|target| inputs::File {
            path: target.path,
            sha256: target.sha256,
            size: target.size,
        })
        .collect())
}

/// the log of the recipe `name`'s step, in `.fetchloom/builds`, with its folders made.
fn log_path(project: &Project, name: &str) -> Result<PathBuf, Error> {
    let folder = project.own_folder().join("builds");
    for folder in [project.own_folder(), folder.clone()] {
        crate::place::real_folder(&folder, crate::place::FolderUse::Place { make: true })
            .map_err(|err| Error::io(format_args!("could not make {}", folder.display()), &err))?;
    }
    Ok(folder.join(format!("{name}.log")))
}

/// where the log of the recipe `name` is, below the project, as messages and JSON name it.
fn log_shown(name: &str) -> String {
    format!(".fetchloom/builds/{name}.log")
}

/// the error a stop gives during a build.
fn stopped() -> Error {
    Error::new(
        Kind::Interrupted,
        "stopped at your request, so data.lock and the data folder are as they were",
    )
}

/// `err`, from `doing`, or the stop it was.
fn unless_stopped(err: &io::Error, doing: impl FnOnce() -> String) -> Error {
    if err.kind() == io::ErrorKind::Interrupted {
        stopped()
    } else {
        Error::io(doing(), err)
    }
}

/// the error for the step of `name` that could not start `program` in `folder`, told by how run
/// names it: a bare name from PATH, a name with a folder from `folder`, a full path one machine's.
fn cannot_start(name: &str, program: &str, folder: &Path, err: &io::Error) -> Error {
    let absolute = Path::new(program).is_absolute();
    let has_folder = program.contains('/') || (cfg!(windows) && program.contains('\\'));
    // a script there whose interpreter is missing fails to start with NotFound too
    let there = (absolute || has_folder) && folder.join(program).is_file();
    if err.kind() == io::ErrorKind::NotFound && !there {
        let (message, help) = if absolute {
            (
                format!("{name} cannot start: there is no program at {program} on this machine"),
                "install it there, or change run in data.toml to a program PATH finds or a path from the project folder, since a full path works on one machine only".to_owned(),
            )
        } else if has_folder {
            (
                format!("{name} cannot start: the project folder has no {program}"),
                format!(
                    "a program named with a folder in it runs from the project folder, not from PATH: set up the project's environment so that {program} is there, then run the same command again"
                ),
            )
        } else {
            (
                format!("{name} cannot start: no program called {program} was found on PATH"),
                "install it, or add the folder that holds it to PATH".to_owned(),
            )
        };
        return Error::new(Kind::BuildFailed, message).help(help);
    }
    Error::new(
        Kind::BuildFailed,
        format!("{name} cannot start {program}: {}", crate::error::said(err)),
    )
    .help("check that run in data.toml names a program you may run")
}

/// the error for the step of `name` whose `program` ended as `ended`, with its last lines unless
/// every line already reached the terminal as the step ran.
fn failed(name: &str, program: &str, ended: Ended, ran: &Ran, folder: &str) -> Error {
    let took = units::duration(ran.took);
    let unchanged = format!(
        "the whole output is in {}, and data.lock and {folder} are as they were",
        log_shown(name)
    );
    let (message, help) = match ended {
        #[cfg(unix)]
        Ended::Signaled(signal) => (
            format!("{name} stopped: {program} was ended by signal {signal} after {took}"),
            format!("the system may have run out of memory; {unchanged}"),
        ),
        Ended::Exited(code) => (
            format!("{name} failed: {program} exited with code {code} after {took}"),
            unchanged,
        ),
        Ended::Stopped | Ended::Low { .. } => return stopped(),
    };
    let tail = if ran.told { &[][..] } else { &ran.tail[..] };
    tail.iter()
        .fold(Error::new(Kind::BuildFailed, message), |error, line| {
            error.detail(line.clone())
        })
        .help(help)
}

/// the error for a result of `name` that could not be taken in.
fn taken(name: &str, fault: Fault) -> Error {
    let refused = match fault {
        Fault::Refused(refused) => refused,
        Fault::Io(err) => {
            return unless_stopped(&err, || format!("could not take in what {name} wrote"));
        }
    };
    let (message, help) = match refused {
        Refused::Nothing => (
            format!("{name} wrote nothing into {{out}}"),
            "write the result into the folder {out} stands for in run, not into the data folder or the project",
        ),
        Refused::Link(path) => (
            format!("{name} wrote a link at {path}, and results hold plain files only"),
            "write a copy, or delete the link at the end of the step",
        ),
        Refused::Special(path) => (
            format!("{name} wrote a special file at {path}, and results hold plain files only"),
            "delete it at the end of the step",
        ),
        Refused::NotText(path) => (
            format!("{name} wrote a name that is not UTF-8 text: {path}"),
            "rename it in the step",
        ),
        Refused::Name(path, reason) => (
            format!("{name} wrote \"{path}\", which a result cannot hold: {reason}"),
            "rename it in the step",
        ),
        Refused::Clash(first, second) => (
            format!(
                "{name} wrote \"{first}\" and \"{second}\", which are one name on disks that ignore case or how an accent is written"
            ),
            "rename one of them in the step",
        ),
        Refused::TooMany(most) => (
            format!(
                "{name} wrote more than {}",
                units::count(most as u64, "file", "files")
            ),
            "pack small files into archives in the step, or raise most_files in the settings file or FETCHLOOM_MOST_FILES",
        ),
        Refused::Changed(path) => (
            format!("{name}'s {path} changed while fetchloom read it"),
            "make sure nothing the step started still writes into {out} once it ends",
        ),
    };
    Error::new(Kind::BuildFailed, message).help(help)
}

/// how many of the files data.lock pins a rerun made as they are: `9,998 of 10,000 files match`.
fn matched(compared: &Compared) -> String {
    format!(
        "{} of {} {}",
        units::grouped(compared.matching as u64),
        units::count(compared.pinned() as u64, "file", "files"),
        if compared.matching == 1 {
            "matches"
        } else {
            "match"
        }
    )
}

/// the most paths an error lists for each step that differs. `--json` has them all.
const LISTED: usize = 20;

/// the error for the steps among `steps` that made other files than data.lock pins for the same
/// key, each kept for `--accept`: what differs, at most [`LISTED`] paths a step, and what to do.
fn differs(manifest: &Manifest, steps: &[Done]) -> Error {
    let differing: Vec<(&Done, &Compared)> = steps
        .iter()
        .filter(|step| step.candidate.is_some())
        .filter_map(|step| Some((step, step.check.as_ref()?)))
        .collect();
    let names: Vec<&str> = differing
        .iter()
        .map(|(step, _)| step.name.as_str())
        .collect();
    let message = match differing.as_slice() {
        [(step, compared)] => format!(
            "{} differs from data.lock: {}",
            step.name,
            matched(compared)
        ),
        _ => format!("{} differ from data.lock", units::listed(&names, "and")),
    };
    let mut error = Error::new(Kind::BuildDiffers, message);
    for (step, compared) in &differing {
        if differing.len() > 1 {
            error = error.detail(format!("{}: {} data.lock", step.name, matched(compared)));
        }
        let lines: Vec<String> = [
            ("changed", &compared.changed),
            ("missing", &compared.missing),
            ("new", &compared.extra),
        ]
        .into_iter()
        .flat_map(|(how, paths)| paths.iter().map(move |path| format!("{how} {path}")))
        .collect();
        error = error.details_up_to(&lines, LISTED);
    }
    for step in steps.iter().filter(|step| step.action == Did::Skipped) {
        error = error.detail(format!(
            "{} was not checked: it reads a result data.lock pins that is not on this machine or a mirror",
            step.name
        ));
    }
    let accept = format!(
        "`{}`",
        super::project::command(&format!("build --accept {}", names.join(" ")))
    );
    let status = super::project::command("status -v");
    error.help(match names.as_slice() {
        [name] => format!(
            "the step made other files from the same inputs, command and code. another version of a program or library it uses can do that, as can the time or an unseeded random number; data.lock and {} are as they were. the rebuild is kept for you to compare; for example, `{status}` says where its files are. once you choose it, {accept} pins it",
            plan::folder(manifest, name)
        ),
        _ => format!(
            "the steps made other files from the same inputs, commands and code. another version of a program or library they use can do that, as can the time or an unseeded random number; data.lock and the data folder are as they were. the rebuilds are kept for you to compare; for example, `{status}` says where their files are. once you choose them, {accept} pins them"
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_with_spaces_or_quotes_are_quoted() {
        assert_eq!(shown("python3"), "python3");
        assert_eq!(shown("<build>/in/raw"), "<build>/in/raw");
        assert_eq!(shown("two words"), "'two words'");
        assert_eq!(shown("it's"), "'it'\\''s'");
        assert_eq!(shown(""), "''");
    }

    /// one file, pinned or matching, takes the singular.
    #[test]
    fn a_rerun_tells_how_many_of_its_files_match() {
        let compared = |matching, changed| Compared {
            matching,
            changed: vec!["out.bin".to_owned(); changed],
            missing: Vec::new(),
            extra: Vec::new(),
        };
        for (matching, changed, said) in [
            (1, 0, "1 of 1 file matches"),
            (0, 1, "0 of 1 file match"),
            (1, 2, "1 of 3 files matches"),
            (2, 1, "2 of 3 files match"),
            (9_998, 2, "9,998 of 10,000 files match"),
        ] {
            assert_eq!(matched(&compared(matching, changed)), said);
        }
    }

    /// a result matches when a rerun for a pin the cache lost makes what data.lock already pins.
    #[test]
    fn the_last_line_tells_new_pins_from_results_that_match() {
        let repinned = ["clean".to_owned(), "tidy".to_owned()];
        for (built, said) in [
            (&[][..], "every recipe is built as data.lock pins it"),
            (&["clean"], "pinned clean's result in data.lock"),
            (&["raw"], "raw's result matches data.lock"),
            (
                &["clean", "tidy"],
                "pinned the results of 2 recipes in data.lock",
            ),
            (
                &["raw", "other"],
                "the results of 2 recipes match data.lock",
            ),
            (
                &["raw", "clean"],
                "pinned clean's result in data.lock; raw's result matches data.lock",
            ),
        ] {
            assert_eq!(closing(built, &repinned), said, "{built:?}");
        }
    }

    /// a bare name is looked for on PATH, one with a folder in the project folder set-up fills, and
    /// a full path on this machine alone; no help asks for a full path, which works on one machine.
    #[test]
    fn a_missing_program_is_told_by_how_run_names_it() {
        let absolute = if cfg!(windows) {
            r"C:\tools\prep.exe"
        } else {
            "/opt/tools/prep"
        };
        let project = Path::new("/no/project/here");
        let told = |program: &str| {
            let missing = io::Error::from(io::ErrorKind::NotFound);
            let error = cannot_start("clean", program, project, &missing);
            (error.message, error.help.unwrap_or_default().into_string())
        };
        assert_eq!(
            told("prep"),
            (
                "clean cannot start: no program called prep was found on PATH".to_owned(),
                "install it, or add the folder that holds it to PATH".to_owned()
            )
        );
        assert_eq!(
            told(".venv/bin/python"),
            (
                "clean cannot start: the project folder has no .venv/bin/python".to_owned(),
                "a program named with a folder in it runs from the project folder, not from PATH: set up the project's environment so that .venv/bin/python is there, then run the same command again".to_owned()
            )
        );
        assert_eq!(
            told(absolute),
            (
                format!("clean cannot start: there is no program at {absolute} on this machine"),
                "install it there, or change run in data.toml to a program PATH finds or a path from the project folder, since a full path works on one machine only".to_owned()
            )
        );
    }

    /// the help names possible causes, not one: another version of what a step uses, the time and
    /// random numbers. then comes `status -v`, and `--accept` last, for one step and several.
    #[test]
    fn a_differing_rebuild_is_compared_before_it_is_accepted() {
        let manifest = Manifest::parse("").unwrap();
        let differing = |name: &str| {
            let key = digest::sha256(name.as_bytes());
            let mut step = done(Did::Ran, name, key, key, &[]);
            step.check = Some(Compared {
                changed: vec!["vectors.bin".to_owned()],
                ..Compared::default()
            });
            step.candidate = Some(format!("sha256:{key}"));
            step
        };
        let help = |steps: &[Done]| {
            differs(&manifest, steps)
                .help
                .unwrap_or_default()
                .into_string()
        };
        assert_eq!(
            help(&[differing("emb")]),
            "the step made other files from the same inputs, command and code. another version of a program or library it uses can do that, as can the time or an unseeded random number; data.lock and data/emb are as they were. the rebuild is kept for you to compare; for example, `fetchloom status -v` says where its files are. once you choose it, `fetchloom build --accept emb` pins it"
        );
        assert_eq!(
            help(&[differing("emb"), differing("tok")]),
            "the steps made other files from the same inputs, commands and code. another version of a program or library they use can do that, as can the time or an unseeded random number; data.lock and the data folder are as they were. the rebuilds are kept for you to compare; for example, `fetchloom status -v` says where their files are. once you choose them, `fetchloom build --accept emb tok` pins them"
        );
    }

    /// one differing step is named in the message with its count, several are listed with a count
    /// each, and each lists its paths up to 20. a rerun that matched stays out; one that could not
    /// be checked is said.
    #[test]
    fn a_differing_rebuild_names_each_step_and_the_paths_that_differ() {
        let manifest = Manifest::parse("").unwrap();
        let step = |name: &str, action: Did, check: Option<Compared>, kept: bool| {
            let key = digest::sha256(name.as_bytes());
            let mut step = done(action, name, key, key, &[]);
            step.check = check;
            step.candidate = kept.then(|| format!("sha256:{key}"));
            step
        };
        let changed = |paths: &[&str]| Compared {
            changed: paths.iter().map(|path| (*path).to_owned()).collect(),
            ..Compared::default()
        };
        let emb = Compared {
            matching: 2,
            changed: vec!["vectors.bin".to_owned()],
            missing: vec!["index.json".to_owned()],
            extra: vec!["notes.txt".to_owned()],
        };
        let many: Vec<String> = (0..21).map(|part| format!("part-{part:02}.bin")).collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        let unchecked = "clean was not checked: it reads a result data.lock pins that is not on this machine or a mirror";
        let rows = [
            (
                vec![step("emb", Did::Ran, Some(emb), true)],
                "emb differs from data.lock: 2 of 4 files match",
                vec![
                    "changed vectors.bin".to_owned(),
                    "missing index.json".to_owned(),
                    "new notes.txt".to_owned(),
                ],
            ),
            (
                vec![
                    step("emb", Did::Ran, Some(changed(&["vectors.bin"])), true),
                    step("raw", Did::Ran, Some(Compared::default()), false),
                    step("tok", Did::Ran, Some(changed(&["tokens.bin"])), true),
                    step("clean", Did::Skipped, None, false),
                ],
                "emb and tok differ from data.lock",
                vec![
                    "emb: 0 of 1 file match data.lock".to_owned(),
                    "changed vectors.bin".to_owned(),
                    "tok: 0 of 1 file match data.lock".to_owned(),
                    "changed tokens.bin".to_owned(),
                    unchecked.to_owned(),
                ],
            ),
            (
                vec![step("emb", Did::Ran, Some(changed(&many)), true)],
                "emb differs from data.lock: 0 of 21 files match",
                many[..20]
                    .iter()
                    .map(|path| format!("changed {path}"))
                    .chain(["and 1 more".to_owned()])
                    .collect(),
            ),
        ];
        for (steps, message, details) in rows {
            let error = differs(&manifest, &steps);
            assert_eq!(error.kind, Kind::BuildDiffers, "{message}");
            assert_eq!(error.message, message);
            assert_eq!(error.details, details, "{message}");
        }
    }
}
