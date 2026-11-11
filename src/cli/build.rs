//! `fetchloom build`: the recipes in data.toml, each step after the datasets it is made from.
//! this fetchloom only previews a build: `--dry-run` says each step's command with its folders in
//! place, what it reads and where its result goes, and runs nothing.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::PathBuf;

use serde::Serialize;

use super::Context;
use super::args;
use super::project::{find, known};
use crate::error::{Error, Kind};
use crate::output::Output;
use crate::plan;
use crate::project::manifest::Manifest;
use crate::recipe::{self, Change, Slot, Step};

/// what stands in a dry run for a step's private folder, which a build makes in the cache.
const BUILD: &str = "<build>";

/// what `build --dry-run` found, as `--json` prints it.
#[derive(Serialize)]
pub(super) struct Report {
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

pub(super) fn run(context: &Context, args: &args::Build) -> Result<Report, Error> {
    let project = find(&context.output)?;
    let (_, manifest) = project.manifest()?;
    for name in &args.names {
        known(&manifest, name, "build")?;
        if manifest.datasets[name].source().is_some() {
            return Err(Error::new(
                Kind::Usage,
                format!("{name} is fetched, not built; `fetchloom sync` places it"),
            )
            .help("name the recipes to build, the datasets data.toml makes with from and run"));
        }
    }
    if !args.dry.dry_run {
        let names: String = args
            .names
            .iter()
            .flat_map(|name| [" ", name.as_str()])
            .collect();
        return Err(Error::new(
            Kind::Usage,
            "this fetchloom shows what building would run, and cannot run recipes yet",
        )
        .help(format!(
            "run `fetchloom build --dry-run{names}` to see each step's command"
        )));
    }
    let lock = project.lock()?.unwrap_or_default();
    let selected: Vec<String> = if args.names.is_empty() {
        manifest
            .datasets
            .iter()
            .filter(|(_, entry)| entry.recipe().is_some())
            .map(|(name, _)| name.clone())
            .collect()
    } else {
        args.names.clone()
    };
    let named: Vec<&str> = selected.iter().map(String::as_str).collect();
    let mut steps = Vec::new();
    for name in recipe::closure(&manifest.datasets, &named) {
        if let Some(step) = Step::read(&project.root, &manifest, &lock, name)? {
            let changes = step.changes(lock.result(name));
            steps.push((step, changes));
        }
    }
    let actions = decide(&steps);
    let steps: Vec<Planned> = steps
        .into_iter()
        .zip(actions)
        .map(|((step, changes), action)| planned(&manifest, &step, &changes, action))
        .collect();
    tell(&context.output, &steps);
    Ok(Report {
        project: project.root,
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
