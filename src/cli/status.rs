//! `fetchloom status`: what differs between the data folder and data.lock. it changes nothing,
//! downloads nothing and hashes nothing, so it answers at once even for large projects.

use std::collections::HashSet;
use std::path::PathBuf;

use serde::Serialize;

use super::Context;
use super::sync::place;
use crate::error::Error;
use crate::output::Output;
use crate::plan;
use crate::project::{Project, lock};
use crate::units;

/// the most paths printed for each kind of difference. `--json` lists them all.
const SHOWN: usize = 20;

#[derive(Default, Serialize)]
pub(super) struct Status {
    project: PathBuf,
    /// datasets whose entry in data.toml changed since data.lock was written.
    unlocked: Vec<String>,
    missing: Vec<String>,
    /// files fetchloom placed that changed since.
    edited: Vec<String>,
    /// files where the lock places one, which fetchloom did not place.
    untracked: Vec<String>,
    /// files fetchloom placed for an older lock, which `sync` replaces.
    outdated: Vec<String>,
    /// files in the data folder that no dataset places.
    extra: Vec<String>,
    /// archives this machine has not unpacked yet, so the files they hold are missing.
    not_unpacked: Vec<String>,
}

pub(super) fn run(context: &Context) -> Result<Status, Error> {
    let status = check(&super::project::find()?)?;
    let output = &context.output;
    list(output, &status);
    if sum_up(output, &status) {
        if status.needs_sync() {
            output.hint(
                "`fetchloom sync` brings the rest in line. edited and extra files stay as they are",
            );
        } else if !status.edited.is_empty() {
            output.hint("`fetchloom sync --force` puts the locked files back over edited ones");
        }
    }
    Ok(status)
}

/// `fetchloom` alone inside a project: the line `status` ends with, without its lists.
pub(super) fn brief(context: &Context, project: &Project) -> Result<Status, Error> {
    let status = check(project)?;
    let output = &context.output;
    if sum_up(output, &status) {
        output.hint(if status.needs_sync() {
            "`fetchloom status` lists what differs, and `fetchloom sync` brings it in line"
        } else {
            "`fetchloom status` lists what differs"
        });
    }
    Ok(status)
}

/// compares the data folder with data.lock, reading only what the disk says about each file.
fn check(project: &Project) -> Result<Status, Error> {
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
    let planned = plan::from_cache(&manifest, &lock, &super::open_cache()?)?;
    status.not_unpacked = planned
        .unknown
        .iter()
        .map(|(dataset, file)| format!("{}/{}", plan::folder(&manifest, dataset), file.path))
        .collect();
    let targets = planned.targets;
    let state = project.state();
    let disk = place::scan(&project.root, &manifest.data_dir);
    for target in &targets {
        let list = match (disk.get(&target.path), state.files.get(&target.path)) {
            (None, _) => &mut status.missing,
            (Some(seen), Some(record)) if record.look == *seen => {
                if record.sha256 == target.sha256 {
                    continue;
                }
                &mut status.outdated
            }
            (Some(_), Some(_)) => &mut status.edited,
            (Some(_), None) => &mut status.untracked,
        };
        list.push(target.path.clone());
    }
    let wanted: HashSet<&str> = targets.iter().map(|target| target.path.as_str()).collect();
    status.extra = disk
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

    /// whether `sync` would change something.
    fn needs_sync(&self) -> bool {
        !self.unlocked.is_empty()
            || !self.missing.is_empty()
            || !self.not_unpacked.is_empty()
            || !self.outdated.is_empty()
            || !self.untracked.is_empty()
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
    for (verb, paths) in status.lists() {
        for path in paths.iter().take(SHOWN) {
            output.mark(verb, path);
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
}

/// the closing line, and whether anything differs.
fn sum_up(output: &Output, status: &Status) -> bool {
    let differences = status.unlocked.len()
        + status.not_unpacked.len()
        + status
            .lists()
            .iter()
            .map(|(_, paths)| paths.len())
            .sum::<usize>();
    if differences == 0 {
        output.done("everything matches data.lock");
        return false;
    }
    output.done(&format!(
        "found {} from data.lock",
        units::count(differences as u64, "difference", "differences")
    ));
    true
}
