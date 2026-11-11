//! `fetchloom build`: runs the recipes in data.toml, each step after the datasets it is made
//! from, takes in what each step wrote, pins it in data.lock and places it in the data folder.
//! `--dry-run` says each step's command with its folders in place, what it reads and where its
//! result goes, and runs nothing.
//!
//! a build takes data.lock, relocks the sources its steps read and pins each result in memory,
//! then writes data.lock once, after every step: a step that fails or is stopped leaves
//! data.lock, data.lock.d and the data folder as they were, while the results of the steps
//! before it stay in the cache, so a build run again reuses them.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use serde::Serialize;

use super::args;
use super::project::{Run, committed, find, hold, ignore_data, known, sync_all};
use super::sync::place::{Placement, Seen};
use super::sync::{Before, Loop, Scope, Upgrade, repinned};
use super::{Context, open_cache};
use crate::build::capture::{self, Fault, Refused};
use crate::build::step::{self, Ended, Ran};
use crate::build::{Staging, inputs};
use crate::cache::{Cache, Member};
use crate::digest::{self, Sha256};
use crate::error::{Error, Kind};
use crate::output::Output;
use crate::plan;
use crate::project::Project;
use crate::project::lock::derived::Derived;
use crate::project::lock::{self, Lock};
use crate::project::manifest::Manifest;
use crate::recipe::{self, Change, Slot, Step};
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
    /// the key of its result, `sha256:` and its hash.
    key: String,
    /// the tree of the files its result holds, `sha256:` and its hash.
    tree: String,
    files: u64,
    bytes: u64,
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
}

/// what `build --dry-run` found.
#[derive(Serialize)]
pub(super) struct Previewed {
    project: PathBuf,
    /// the recipes named, or every recipe when none was.
    selected: Vec<String>,
    /// each recipe the selected ones need, in the order the steps would run.
    steps: Vec<Planned>,
    dry_run: bool,
}

/// one step as a dry run finds it.
#[derive(Serialize)]
struct Planned {
    name: String,
    action: Action,
    /// the key of the result building it now would make, `sha256:` and its hash, once data.lock
    /// pins every dataset it reads and all its code is there.
    key: Option<String>,
    /// what moved since data.lock's result of it was built.
    changes: Vec<String>,
    /// the program and its arguments, each folder in place.
    run: Vec<String>,
    /// the variables set for the step alone.
    env: BTreeMap<String, String>,
    /// each dataset it reads, with the folder the step finds a copy of its files in.
    inputs: Vec<Input>,
    /// the files and folders of code whose bytes enter its key.
    code: Vec<String>,
    /// the folder in the project its result goes in.
    output: String,
}

#[derive(Serialize)]
struct Input {
    name: String,
    folder: String,
}

/// what a build would do with a step.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Action {
    /// data.lock does not pin what building it now would make.
    WouldRun,
    /// it agrees with data.lock, but reads the result of a step that would run, so it runs
    /// again only if that result changes.
    MayRun,
    /// data.lock pins its result as building it now would make.
    UpToDate,
}

pub(super) async fn run(context: &Context, args: &args::Build) -> Result<Report, Error> {
    let project = find(&context.output)?;
    let (_, manifest) = project.manifest()?;
    recipes_only(&manifest, &args.names)?;
    if args.dry.dry_run {
        return preview(context, &project, &manifest, &args.names).map(Report::Previewed);
    }
    let _held = hold(context, &project, "build").await?;
    let mut building = Building::start(context, &project, &args.names).await?;
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
                format!("{name} is fetched, not built; `fetchloom sync` places it"),
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

/// what building `names` would do, told as a dry run tells it, with nothing run or changed.
fn preview(
    context: &Context,
    project: &Project,
    manifest: &Manifest,
    names: &[String],
) -> Result<Previewed, Error> {
    let lock = project.lock()?.unwrap_or_default();
    let selected = selected(manifest, names);
    let roots: Vec<&str> = selected.iter().map(String::as_str).collect();
    let mut steps = Vec::new();
    for name in recipe::closure(&manifest.datasets, &roots) {
        if let Some(step) = Step::read(&project.root, manifest, &lock, name)? {
            let changes = step.changes(lock.result(name));
            steps.push((step, changes));
        }
    }
    let actions = decide(&steps);
    let steps: Vec<Planned> = steps
        .into_iter()
        .zip(actions)
        .map(|((step, changes), action)| planned(manifest, &step, &changes, action))
        .collect();
    tell(&context.output, &steps);
    Ok(Previewed {
        project: project.root.clone(),
        selected,
        steps,
        dry_run: true,
    })
}

/// what a build would do with each of `steps`, given in the order they run, each with what moved
/// since data.lock's result of it was built: run what moved, and treat a step that reads the
/// result of one that may run as one that may run too, since only that result tells.
fn decide(steps: &[(Step, Vec<Change>)]) -> Vec<Action> {
    let mut running: BTreeSet<&str> = BTreeSet::new();
    steps
        .iter()
        .map(|(step, changes)| {
            let action = if !changes.is_empty() {
                Action::WouldRun
            } else if step
                .inputs
                .iter()
                .any(|read| running.contains(read.name.as_str()))
            {
                Action::MayRun
            } else {
                Action::UpToDate
            };
            if action != Action::UpToDate {
                running.insert(&step.name);
            }
            action
        })
        .collect()
}

/// `step` as a dry run reports it, its folders named by where a build puts them.
fn planned(manifest: &Manifest, step: &Step, changes: &[Change], action: Action) -> Planned {
    let folder = |slot: Slot<'_>| match slot {
        Slot::Input(name) => OsString::from(format!("{BUILD}/in/{name}")),
        Slot::Out => OsString::from(format!("{BUILD}/out")),
    };
    let run = manifest.datasets[&step.name]
        .recipe()
        .map(|recipe| {
            recipe
                .run
                .iter()
                .map(|word| word.expand(&folder).to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    Planned {
        name: step.name.clone(),
        action,
        key: step.key().map(|key| format!("sha256:{key}")),
        changes: changes.iter().map(ToString::to_string).collect(),
        run,
        env: step.env.clone(),
        inputs: step
            .inputs
            .iter()
            .map(|read| Input {
                name: read.name.clone(),
                folder: format!("{BUILD}/in/{}", read.name),
            })
            .collect(),
        code: step.code.iter().map(|(path, _)| path.clone()).collect(),
        output: plan::folder(manifest, &step.name),
    }
}

/// a line for each step, and under one that may run its command and what it reads and writes.
fn tell(output: &Output, steps: &[Planned]) {
    for step in steps {
        match step.action {
            Action::UpToDate => {
                output.step("Up to date", &step.name);
                continue;
            }
            Action::WouldRun => output.step(
                "Would run",
                &format!("{} \u{b7} {}", step.name, step.changes.join("; ")),
            ),
            Action::MayRun => {
                let after = steps
                    .iter()
                    .find(|other| {
                        other.action != Action::UpToDate
                            && step.inputs.iter().any(|input| input.name == other.name)
                    })
                    .map_or("", |other| other.name.as_str());
                output.step(
                    "May run",
                    &format!(
                        "{} \u{b7} runs again if {after}'s result changes",
                        step.name
                    ),
                );
            }
        }
        let words: Vec<String> = step.run.iter().map(|word| shown(word)).collect();
        output.item(&words.join(" "));
        let read: Vec<&str> = step
            .inputs
            .iter()
            .map(|input| input.name.as_str())
            .chain(step.code.iter().map(String::as_str))
            .collect();
        let with = if step.env.is_empty() {
            String::new()
        } else {
            let pairs: Vec<String> = step
                .env
                .iter()
                .map(|(name, value)| shown(&format!("{name}={value}")))
                .collect();
            format!(", with {}", pairs.join(" "))
        };
        output.item(&format!(
            "reads {}{with}; writes {}",
            listed(&read),
            step.output
        ));
    }
    if steps.is_empty() {
        output.done(
            "Ok",
            "data.toml has no recipes, so there is nothing to build",
        );
    } else if steps.iter().all(|step| step.action == Action::UpToDate) {
        output.done(
            "Ok",
            "every recipe is built as data.lock pins it; dry run, nothing was run or changed",
        );
    } else {
        output.done("Ok", "dry run, nothing was run or changed");
        output.hint(&format!(
            "{BUILD} is a folder a build makes in the cache for each step: the step finds a copy of each dataset it reads in {BUILD}/in, and what it writes in {BUILD}/out becomes its dataset"
        ));
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
    ) -> Result<Building<'a>, Error> {
        let (text, manifest) = project.manifest()?;
        recipes_only(&manifest, names)?;
        let old = project
            .lock_and_text()?
            .map(|(lock, text)| (lock, digest::sha256(text.as_bytes())));
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
        Ok(Building {
            context,
            work,
            manifest_read: digest::sha256(text.as_bytes()),
            manifest,
            old,
            lock,
            before,
            recipes,
            report: Built {
                project: project.root.clone(),
                selected,
                ..Built::default()
            },
        })
    }

    /// builds each recipe in turn, stopping at the first that fails, which keeps what the build
    /// did so far for `--json`.
    async fn steps(&mut self) -> Result<(), Error> {
        for name in self.recipes.clone() {
            match self.step(&name).await {
                Ok(done) => self.report.steps.push(done),
                Err(error) => {
                    return Err(match serde_json::to_value(&self.report) {
                        Ok(value) => error.with_result(value),
                        Err(_) => error,
                    });
                }
            }
        }
        Ok(())
    }

    /// builds the recipe `name`: up to date when data.lock pins what building it now would make
    /// and the cache holds it, reused when an earlier build of its key made a result the cache
    /// holds whole, and otherwise run. a pin whose key is still the recipe's keeps its files: a
    /// step that makes others for it is not reproducible, and data.lock stays as it is.
    async fn step(&mut self, name: &str) -> Result<Done, Error> {
        let project = self.work.project;
        let output = &self.context.output;
        let step = Step::read(&project.root, &self.manifest, &self.lock, name)?
            .ok_or_else(|| Error::new(Kind::ProjectInvalid, format!("{name} is no recipe")))?;
        let key = keyed(&step)?;
        let pinned = self
            .lock
            .result(name)
            .filter(|pinned| pinned.key == key)
            .map(|pinned| (pinned.tree, whole(&self.work.cache, &pinned.files)));
        if let Some((_, true)) = pinned
            && let Some(pinned) = self.lock.result(name)
        {
            output.step("Up to date", name);
            return Ok(done(Did::UpToDate, name, key, pinned.tree, &pinned.files));
        }
        let expected = pinned.map(|(tree, _)| tree);
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
        if expected.is_none()
            && let Some(built) = self.work.cache.results(&key).into_iter().next()
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
        self.work
            .ready(&self.manifest, &self.lock, &sources)
            .await?;
        output.step("Running", &format!("{name} · {}", self.command(name)));
        let made = tokio::task::block_in_place(|| self.make(&step, key, expected))?;
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

    /// runs `step` in a folder of its own and takes in what it wrote, as the result of `key`.
    /// with `expected`, the tree data.lock pins for that key, a result of other files is refused
    /// before anything enters the cache.
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
        if let Some(expected) = expected
            && found.tree != expected
        {
            return Err(differs(
                name,
                expected,
                found.tree,
                &plan::folder(&self.manifest, name),
            ));
        }
        capture::publish(
            cache,
            &found,
            threads,
            &self.context.stop,
            &self.context.work,
        )
        .map_err(|fault| taken(name, fault))?;
        cache.record_result(&key, &found.members).map_err(|err| {
            Error::io(
                format_args!("could not record what {name} made in the cache"),
                &err,
            )
        })?;
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
        let forward: Option<&dyn Fn(&str)> = (!output.json()).then_some(&forward);
        let ran = step::run(&command, &project.root, &log, forward, &self.context.stop)
            .map_err(|err| cannot_start(name, program, &err))?;
        let folder = plan::folder(&self.manifest, name);
        match ran.ended {
            Ended::Exited(0) => Ok(ran),
            Ended::Stopped => Err(stopped()),
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
    /// a pin keeps when it was made while its key and tree stay.
    fn pin(&mut self, step: &Step, key: Sha256, tree: Sha256, members: Vec<Member>) {
        let timestamp = match self.lock.result(&step.name) {
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
        match self
            .lock
            .derived
            .binary_search_by(|result| result.name.as_str().cmp(&step.name))
        {
            Ok(at) => self.lock.derived[at] = derived,
            Err(at) => self.lock.derived.insert(at, derived),
        }
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
        datasets.extend(
            self.lock
                .derived
                .iter()
                .filter(|result| {
                    old.and_then(|old| old.result(&result.name))
                        .is_none_or(|was| was.key != result.key || was.tree != result.tree)
                })
                .map(|result| result.name.clone()),
        );
        datasets.sort();
        datasets.dedup();
        let lock_read = if changed {
            let text = self.lock.render();
            let text = self.work.write_rendered(&self.before, &self.lock, text)?;
            self.work.project.sweep_lists(&self.lock);
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
            .filter(|step| step.action != Did::UpToDate)
            .map(|step| step.name.as_str())
            .collect();
        match built.as_slice() {
            [] => output.done("Ok", "every recipe is built as data.lock pins it"),
            [name] => output.done("Ok", &format!("pinned {name}'s result in data.lock")),
            many => output.done(
                "Ok",
                &format!(
                    "pinned the results of {} in data.lock",
                    units::count(many.len() as u64, "recipe", "recipes")
                ),
            ),
        }
        if changed {
            output.next(&format!("commit {}", committed(self.work.project)));
        }
        self.report.datasets = datasets;
        self.report.lock_changed = changed;
        self.report.placement = Some(outcome.placed);
        Ok(self.report)
    }
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
        .help("restore it, or take it out of code in data.toml"));
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

/// what a step did, before it says where its log is and how long it ran.
fn done(action: Did, name: &str, key: Sha256, tree: Sha256, files: &[Member]) -> Done {
    Done {
        name: name.to_owned(),
        action,
        key: format!("sha256:{key}"),
        tree: format!("sha256:{tree}"),
        files: files.len() as u64,
        bytes: files.iter().map(|file| file.size).sum(),
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
        &|file| cache.unpacked(&file.sha256, file.unpacks_to.as_ref()),
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

/// the error for the step of `name` that could not start `program`.
fn cannot_start(name: &str, program: &str, err: &io::Error) -> Error {
    if err.kind() == io::ErrorKind::NotFound {
        return Error::new(
            Kind::BuildFailed,
            format!("{name} cannot start: no program called {program} was found on PATH"),
        )
        .help("install it, or give its full path in run in data.toml");
    }
    Error::new(
        Kind::BuildFailed,
        format!("{name} cannot start {program}: {}", crate::error::said(err)),
    )
    .help("check that run in data.toml names a program you may run")
}

/// the error for the step of `name` whose `program` ended as `ended`, with its last lines.
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
        Ended::Stopped => return stopped(),
    };
    ran.tail
        .iter()
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

/// the error for a step of `name` that made `tree` where data.lock pins `expected` for the same
/// key.
fn differs(name: &str, expected: Sha256, tree: Sha256, folder: &str) -> Error {
    Error::new(
        Kind::BuildDiffers,
        format!("{name} made other files than data.lock pins for the same inputs, command and code"),
    )
    .detail(format!(
        "data.lock pins the tree sha256:{expected}, and this build made sha256:{tree}"
    ))
    .help(format!(
        "the step is not reproducible, as one that uses the time or an unseeded random number is; data.lock and {folder} are as they were"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recipe::Read;

    fn step(name: &str, reads: &[&str]) -> Step {
        Step {
            name: name.to_owned(),
            inputs: reads
                .iter()
                .map(|read| Read {
                    name: (*read).to_owned(),
                    content: Some(crate::digest::sha256(read.as_bytes())),
                    made: true,
                })
                .collect(),
            run: Vec::new(),
            code: Vec::new(),
            env: BTreeMap::new(),
        }
    }

    /// a step whose input may change runs again only if it does, and so may every step after it
    /// that reads it, directly or not; a step reading only settled results stays as it is.
    #[test]
    fn a_step_after_one_that_runs_may_run() {
        let steps = vec![
            (step("a", &["raw"]), vec![Change::Run]),
            (step("b", &["a"]), Vec::new()),
            (step("c", &["b"]), Vec::new()),
            (step("d", &["raw"]), Vec::new()),
            (step("e", &["d", "c"]), vec![Change::Env]),
        ];
        assert_eq!(
            decide(&steps),
            [
                Action::WouldRun,
                Action::MayRun,
                Action::MayRun,
                Action::UpToDate,
                Action::WouldRun
            ]
        );
    }

    #[test]
    fn words_with_spaces_or_quotes_are_quoted() {
        assert_eq!(shown("python3"), "python3");
        assert_eq!(shown("<build>/in/raw"), "<build>/in/raw");
        assert_eq!(shown("two words"), "'two words'");
        assert_eq!(shown("it's"), "'it'\\''s'");
        assert_eq!(shown(""), "''");
    }
}
