//! `fetchloom build`: runs the recipes in data.toml, each step after the datasets it is made
//! from, takes in what each step wrote, pins it in data.lock and places it in the data folder.
//! `--dry-run` says what the build would do with each step, and runs nothing.
//!
//! a build takes data.lock, relocks the sources its steps read and pins each result in memory,
//! then writes data.lock once, after every step: a step that fails or is stopped leaves
//! data.lock, data.lock.d and the data folder as they were, while the results of the steps
//! before it stay in the cache, so a build run again reuses them.

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
use crate::project::lock::derived::Derived;
use crate::project::lock::{self, Lock};
use crate::project::manifest::Manifest;
use crate::project::state::Candidate;
use crate::recipe::{self, Slot, Step};
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

/// what a build did.
#[derive(Default, Serialize)]
pub(super) struct Built {
    project: PathBuf,
    /// the recipes named, or every recipe when none was.
    selected: Vec<String>,
    /// the datasets whose pins changed: pinned for the first time, pinned to other files, or
    /// no longer pinned.
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
    /// how the files it made when it ran again differ from the ones data.lock pins for its key,
    /// when it ran for a pin.
    check: Option<Compared>,
    /// the tree of the files it made that differ from data.lock's, which the state keeps for
    /// `--accept`.
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
    /// a build of the same key made its result before, and the cache holds all of it, so it
    /// did not run.
    Reused,
    /// data.lock pins its result as building it now would make, and the cache holds it.
    UpToDate,
    /// data.lock pins its result as building it now would make, and a mirror gave the cache
    /// what it lacked of it, so it did not run.
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
    #[cfg(unix)]
    let _terminate = Terminate::listen(context);
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

/// SIGTERM, which a scheduler sends a job it ends, stops a build as ctrl-c does, saying what a
/// second SIGTERM does, and when it stops, ends the group of the step running now at once, since
/// a SIGKILL that ended fetchloom would leave the step running. past the build's mark it
/// finishes, as after ctrl-c. a second one quits as a second ctrl-c does. dropping it stops
/// listening.
#[cfg(unix)]
struct Terminate(tokio::task::AbortHandle);

#[cfg(unix)]
impl Terminate {
    fn listen(context: &Context) -> Option<Terminate> {
        use tokio::signal::unix::{SignalKind, signal};
        let mut terminate = signal(SignalKind::terminate()).ok()?;
        let (stop, output) = (Arc::clone(&context.stop), Arc::clone(&context.output));
        let task = tokio::spawn(async move {
            if terminate.recv().await.is_none() {
                return;
            }
            let heard = super::heard_first(&stop, &output, crate::output::Interrupt::Terminate);
            if heard == crate::stop::Heard::Stop {
                step::end_running();
            }
            if terminate.recv().await.is_some() {
                step::end_running();
                output.quit();
                stop.quitting();
                std::process::exit(130);
            }
        });
        Some(Terminate(task.abort_handle()))
    }
}

#[cfg(unix)]
impl Drop for Terminate {
    fn drop(&mut self) {
        self.0.abort();
    }
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

/// `words` as a person reads a list: `a`, `a and b`, `a, b and c`.
fn listed(words: &[&str]) -> String {
    match words.split_last() {
        None => String::new(),
        Some((last, [])) => (*last).to_owned(),
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
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
                let names: Vec<&str> = changed.iter().map(String::as_str).collect();
                return Err(Error::new(
                    Kind::Outdated,
                    format!(
                        "data.lock is out of date: building would change what it pins of {}",
                        listed(&names)
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

    /// runs every step again from the inputs data.lock pins and compares what each makes with
    /// the result data.lock pins for it, going on past a step that differs and stopping at one
    /// that fails. data.lock and the data folder stay as they are.
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

    /// runs the step of the recipe `name` again from the inputs data.lock pins and compares what
    /// it makes with its pin. a rebuild that matches enters the cache as the pinned result, and
    /// one that differs is kept for `--accept`.
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
        output.step("Running", &format!("{name} · {}", self.command(name)));
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

    /// puts in the cache everything a build of the recipes, or with `--check` a check of them,
    /// needs, mirrors first, and runs nothing: the datasets each step that would run reads, the
    /// pinned results a build would take as they are and, for a build, what placing its datasets
    /// needs. the sources the build pins anew are written to data.lock, so a later build needs no
    /// network for them either. it ends with that later build, as `args` asks for it.
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

    /// puts in the cache what placing the fetched datasets of the build needs that the data
    /// folder lacks, as the build places them once its steps are done, such as a dataset only
    /// steps that do not run read. results stay out: a step that runs makes its own, and
    /// [`Building::prepare`] fetched the others.
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
        let mut survey = Survey::read(project, &self.manifest.data_dir, self.context.threads);
        survey.check_doubted(&project.root, cache)?;
        let plan = plan::from_cache(&self.manifest, &fetched, cache)?;
        self.work
            .prepare(&self.manifest, &fetched, plan, false, survey)
            .await?;
        Ok(())
    }

    /// `error`, with what the build did so far beside it for `--json`.
    /// the build's last look for a stop, right before it writes data.lock: a stop asked by then
    /// leaves data.lock and the data folder as they were, and one asked later lets it finish.
    fn last_look(&self) -> Result<(), Error> {
        if self.context.stop.is_asked() {
            return Err(stopped().with_report(&self.report));
        }
        self.context.stop.finish();
        Ok(())
    }

    /// builds the recipe `name`: up to date when data.lock pins what building it now would make
    /// and the cache holds it, fetched when a mirror holds what the cache lacks of it, reused
    /// when an earlier build of its key made a result the cache holds whole, and otherwise run. a
    /// pin whose key is still the recipe's keeps its files: other files a step makes for it are
    /// kept for a person to choose, and data.lock stays as it is.
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
        output.step("Running", &format!("{name} · {}", self.command(name)));
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

    /// the result of the recipe `name`, keyed `key`, as data.lock pins it, told as up to date
    /// when the cache holds it or as fetched when a mirror gave what the cache lacked of it, as
    /// [`course`] decides. none when the step must run or may be reused.
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
    /// `--accept`, in place of any kept before. the project joins the cache's list, so `cache gc`
    /// reads the state and keeps its files.
    fn keep(&self, name: &str, candidate: Candidate) -> Result<(), Error> {
        let project = self.work.project;
        let (mut state, _) = project.state();
        state.candidates.insert(name.to_owned(), candidate);
        project.write_state(&state)?;
        register(&self.work);
        Ok(())
    }

    /// puts in the cache the files of `sources`, fetched datasets a step reads, as
    /// [`Loop::ready`] does. when that fails, the error names each placed file of theirs linked to
    /// a cache copy someone changed, as a sync's does, since that is why their pinned bytes had to
    /// be fetched again.
    async fn ready(&self, sources: &BTreeSet<&str>) -> Result<(), Error> {
        let Err(error) = self.work.ready(&self.manifest, &self.lock, sources).await else {
            return Ok(());
        };
        if error.kind == Kind::Interrupted {
            return Err(error);
        }
        let project = self.work.project;
        let mut survey = Survey::read(project, &self.manifest.data_dir, self.context.threads);
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

    /// takes the files of `files`, the result data.lock pins for `name`, that the cache lacks
    /// from the data folder where it holds their pinned bytes, else asks the mirrors for them, and
    /// says whether the cache holds all of them now.
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

    /// runs `step` in a folder of its own and takes in what it wrote, as the result of `key`.
    /// with `expected`, the tree data.lock pins for that key, a result of other files enters the
    /// cache without a record, so no build reuses it.
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

    /// pins in the lock being built the result `tree` of `step`, keyed `key`, holding `members`.
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

/// the last line of a build whose steps that did more than find their result are `built`, of
/// which `repinned` holds those whose pin in data.lock changed. the others made exactly the files
/// data.lock pinned, as a step run again for a pin the cache lost does.
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

/// pins in `lock` the result `tree` of `step`, keyed `key`, holding `members`. a pin keeps when
/// it was made while its key and tree stay.
fn pin(lock: &mut Lock, step: &Step, key: Sha256, tree: Sha256, members: Vec<Member>) {
    let timestamp = match lock.result(&step.name) {
        Some(old) if old.key == key && old.tree == tree => old.timestamp.clone(),
        _ => lock::timestamp(SystemTime::now()),
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

/// fails with `lock.outdated` unless data.lock, `read`, pins every dataset of `scope` as data.toml
/// asks: each fetched one for its entry and each recipe's result as building it now would make.
/// `instead` is the other way out the help names, such as "build without --check".
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

/// what a build does with the step of a recipe whose key it knows, before it asks a mirror or
/// runs anything. a build and its dry run both take this decision, so the dry run says what the
/// build then does.
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

/// the files of the dataset `name` as a step reads them: a result as data.lock pins it, or a
/// fetched dataset's files laid out as its folder holds them, each archive by the files it
/// unpacked to.
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

/// the error for the step of `name` that could not start `program`, run in `folder`. a program
/// that is not there is told by how run names it: a bare name is looked for on PATH, one with a
/// folder in it from `folder`, the project's, and a full path names one place on one machine.
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
        "{} of {} files match",
        units::grouped(compared.matching as u64),
        units::grouped(compared.pinned() as u64)
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
        _ => format!("{} differ from data.lock", listed(&names)),
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

    /// a build's last line says which results it pinned anew and which made exactly what
    /// data.lock already pinned, as a step run again for a pin the cache lost does.
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

    /// a step whose program is not there gets help for how run names it: a bare name is looked
    /// for on PATH, a name with a folder in it in the project folder, which setting up the
    /// project's environment fills, and a full path on this machine alone. none asks for a full
    /// path in data.toml, which would build on one machine only.
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

    /// a rebuild that differs is told without a guess at why: the help names another version of
    /// what a step uses beside the time and random numbers, then `status -v` to compare the
    /// files, and `--accept` last, for one step and for several.
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
}
