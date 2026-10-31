//! `fetchloom status`: what differs between the data folder and data.lock. it changes nothing,
//! downloads nothing and hashes only files whose size and time cannot vouch for them, so it
//! answers at once even for large projects.

use std::collections::HashSet;
use std::path::PathBuf;

use serde::Serialize;

use super::Context;
use super::sync::disk::Survey;
use super::sync::place;
use crate::error::Error;
use crate::output::Output;
use crate::plan;
use crate::project::{Project, lock};
use crate::threads::Threads;
use crate::units;

/// the most paths printed for each kind of difference. `--json` lists them all.
const SHOWN: usize = 20;

#[derive(Default, Serialize)]
pub(super) struct Status {
    project: PathBuf,
    /// datasets whose entry in data.toml changed since data.lock was written.
    unlocked: Vec<String>,
    /// datasets still in data.lock whose entries were removed from data.toml.
    removed: Vec<String>,
    missing: Vec<String>,
    /// files fetchloom placed that changed since.
    edited: Vec<String>,
    /// files where the lock places one, which fetchloom did not place.
    untracked: Vec<String>,
    /// files fetchloom placed for an older lock, or read-only for a dataset that now asks for
    /// writable copies, which `sync` replaces.
    outdated: Vec<String>,
    /// files placed as links whose bytes another project changed through its own link, which
    /// `sync` puts back.
    edited_elsewhere: Vec<String>,
    /// files in the data folder that no dataset places.
    extra: Vec<String>,
    /// archives this machine has not unpacked yet, so the files they hold are missing.
    not_unpacked: Vec<String>,
    /// edited files placed as hardlinks, whose change every project sharing the link sees.
    #[serde(skip)]
    shared: HashSet<String>,
}

pub(super) fn run(context: &Context) -> Result<Status, Error> {
    let status = check(&super::project::find()?, context.threads)?;
    let output = &context.output;
    list(output, &status);
    if sum_up(output, &status) {
        if status.needs_sync() {
            output.hint(
                "`fetchloom sync` places missing or updated files and reports any conflicts it keeps",
            );
        }
        if !status.edited.is_empty() || !status.untracked.is_empty() {
            output.hint("save any changes you need, then use `fetchloom sync --force` to replace edited or conflicting files at paths in data.lock");
        }
    }
    Ok(status)
}

/// `fetchloom` alone inside a project: the line `status` ends with, without its lists.
pub(super) fn brief(context: &Context, project: &Project) -> Result<Status, Error> {
    let status = check(project, context.threads)?;
    let output = &context.output;
    if sum_up(output, &status) {
        output.hint("`fetchloom status` lists what differs");
    }
    Ok(status)
}

/// compares the data folder with data.lock, reading only what the disk says about each file.
fn check(project: &Project, threads: Threads) -> Result<Status, Error> {
    let (_, manifest) = project.manifest()?;
    let lock = project.lock()?.unwrap_or_default();
    let mut status = Status {
        project: project.root.clone(),
        ..Status::default()
    };
    for (name, entry) in &manifest.datasets {
        if lock
            .get(name)
            .is_none_or(|locked| locked.spec != lock::spec(entry))
        {
            status.unlocked.push(name.clone());
        }
    }
    status.removed = lock
        .datasets
        .iter()
        .filter(|dataset| !manifest.datasets.contains_key(&dataset.name))
        .map(|dataset| dataset.name.clone())
        .collect();
    let cache = super::open_cache()?;
    let planned = plan::from_cache(&manifest, &lock, &cache)?;
    status.not_unpacked = planned
        .unknown
        .iter()
        .map(|(dataset, file)| format!("{}/{}", plan::folder(&manifest, dataset), file.path))
        .collect();
    let targets = planned.targets;
    let mut survey = Survey::read(project, &manifest.data_dir, threads);
    survey.retarget(&project.root, &targets);
    survey.check_doubted(&project.root, &cache)?;
    for target in &targets {
        let seen = survey.disk.get(&target.path);
        let file = project.root.join(&target.path);
        let list = match (seen, survey.state.files.get(&target.path)) {
            (None, _) => &mut status.missing,
            (Some(_), Some(_)) if survey.elsewhere.contains(&target.path) => {
                &mut status.edited_elsewhere
            }
            (Some(seen), Some(record))
                if record.look == *seen && !survey.edited.contains(&target.path) =>
            {
                if record.sha256 == target.sha256 && (!target.copy || record.writable) {
                    continue;
                }
                &mut status.outdated
            }
            // sync takes a changed file that holds the locked bytes as placed
            (Some(seen), Some(_)) if place::holds_target(target, seen, &file) => {
                if !target.copy || place::now_writable(&file) {
                    continue;
                }
                &mut status.outdated
            }
            (Some(_), Some(record)) => {
                if record.method == crate::place::Method::Hardlink
                    && crate::place::has_other_names(&file)
                {
                    status.shared.insert(target.path.clone());
                }
                &mut status.edited
            }
            (Some(_), None) => &mut status.untracked,
        };
        list.push(target.path.clone());
    }
    let wanted: HashSet<&str> = targets.iter().map(|target| target.path.as_str()).collect();
    status.extra = survey
        .disk
        .into_keys()
        .filter(|path| !wanted.contains(path.as_str()))
        .collect();
    status.extra.sort();
    Ok(status)
}

impl Status {
    /// each kind of file difference, under the verb that marks it.
    fn lists(&self) -> [(&'static str, &Vec<String>); 5] {
        [
            ("Missing", &self.missing),
            ("Edited", &self.edited),
            ("Untracked", &self.untracked),
            ("Outdated", &self.outdated),
            ("Extra", &self.extra),
        ]
    }

    fn needs_sync(&self) -> bool {
        !self.removed.is_empty()
            || !self.unlocked.is_empty()
            || !self.missing.is_empty()
            || !self.not_unpacked.is_empty()
            || !self.outdated.is_empty()
            || !self.untracked.is_empty()
            || !self.edited_elsewhere.is_empty()
    }
}

/// a line for each difference, at most `SHOWN` of each kind.
fn list(output: &Output, status: &Status) {
    for name in &status.unlocked {
        output.mark(
            "Unlocked",
            &format!("{name} · data.toml changed since data.lock was written"),
        );
    }
    for name in &status.removed {
        output.mark(
            "Removed",
            &format!("{name} from data.toml; `fetchloom sync` updates data.lock"),
        );
    }
    for (verb, paths) in status.lists() {
        for path in paths.iter().take(SHOWN) {
            if status.shared.contains(path) {
                output.mark(
                    verb,
                    &format!("{path} · changed through a hardlink other projects may share"),
                );
            } else {
                output.mark(verb, path);
            }
        }
        if paths.len() > SHOWN {
            output.mark("", &format!("and {} more", paths.len() - SHOWN));
        }
    }
    for archive in &status.not_unpacked {
        output.mark(
            "Missing",
            &format!("{archive} · not unpacked on this machine yet"),
        );
    }
    for path in status.edited_elsewhere.iter().take(SHOWN) {
        output.mark("Edited", &format!("{path} · linked file has changed"));
    }
    if status.edited_elsewhere.len() > SHOWN {
        let more = status.edited_elsewhere.len() - SHOWN;
        output.mark("", &format!("and {more} more"));
    }
}

/// the closing line, and whether anything differs.
fn sum_up(output: &Output, status: &Status) -> bool {
    let differences = status.removed.len()
        + status.unlocked.len()
        + status.not_unpacked.len()
        + status.edited_elsewhere.len()
        + status
            .lists()
            .iter()
            .map(|(_, paths)| paths.len())
            .sum::<usize>();
    if differences == 0 {
        output.done("Ok", "everything matches data.lock");
        return false;
    }
    output.mark(
        "Found",
        &format!(
            "{} from data.lock",
            units::count(differences as u64, "difference", "differences")
        ),
    );
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::{Level, Style};

    #[test]
    fn sync_is_suggested_for_repairable_differences_but_not_for_personal_edits() {
        let output = Output::new(
            Style {
                color: false,
                unicode: false,
                interactive: false,
                watched: false,
            },
            Level::Quiet,
            false,
            false,
        );
        let clean = Status::default();
        assert!(!clean.needs_sync());
        assert!(!sum_up(&output, &clean));
        for category in 0..9 {
            let mut status = Status::default();
            let paths = match category {
                0 => &mut status.removed,
                1 => &mut status.unlocked,
                2 => &mut status.missing,
                3 => &mut status.not_unpacked,
                4 => &mut status.outdated,
                5 => &mut status.untracked,
                6 => &mut status.edited_elsewhere,
                7 => &mut status.edited,
                _ => &mut status.extra,
            };
            paths.push("data/set/file.csv".to_owned());
            assert_eq!(status.needs_sync(), category < 7, "category {category}");
            assert!(sum_up(&output, &status), "category {category}");
        }
    }

    #[test]
    fn json_status_keeps_every_path_including_link_edits_and_unknown_archives() {
        let status = Status {
            edited_elsewhere: (0..25)
                .map(|index| format!("data/set/{index}.csv"))
                .collect(),
            not_unpacked: vec!["data/set/archive.zip".to_owned()],
            removed: vec!["old".to_owned()],
            ..Status::default()
        };
        let value = serde_json::to_value(&status).unwrap();
        assert_eq!(value["edited_elsewhere"].as_array().unwrap().len(), 25);
        assert_eq!(value["edited_elsewhere"][24], "data/set/24.csv");
        assert_eq!(
            value["not_unpacked"],
            serde_json::json!(["data/set/archive.zip"])
        );
        assert_eq!(value["removed"], serde_json::json!(["old"]));
        assert_eq!(value["edited"], serde_json::json!([]));
    }
}
