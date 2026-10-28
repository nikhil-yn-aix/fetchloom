//! `fetchloom status`: what differs between the data folder and data.lock. it changes nothing,
//! downloads nothing and hashes nothing, so it answers at once even for large projects.

use std::collections::HashSet;
use std::path::PathBuf;

use serde::Serialize;

use super::Context;
use super::sync::disk;
use crate::error::Error;
use crate::plan;
use crate::project::lock;
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
    let project = super::project::find()?;
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
    let disk = disk::scan(&project.root, &manifest.data_dir);
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
    tell(context, &status);
    Ok(status)
}

fn tell(context: &Context, status: &Status) {
    let output = &context.output;
    for name in &status.unlocked {
        output.mark(
            "Unlocked",
            &format!("{name} · data.toml changed since data.lock was written"),
        );
    }
    let lists = [
        ("Missing", &status.missing),
        ("Edited", &status.edited),
        ("Untracked", &status.untracked),
        ("Outdated", &status.outdated),
        ("Extra", &status.extra),
    ];
    for (verb, paths) in lists {
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
    let differences = status.unlocked.len()
        + status.not_unpacked.len()
        + lists.iter().map(|(_, paths)| paths.len()).sum::<usize>();
    if differences == 0 {
        output.done("everything matches data.lock");
        return;
    }
    output.done(&format!(
        "found {} from data.lock",
        units::count(differences as u64, "difference", "differences")
    ));
    if !status.unlocked.is_empty()
        || !status.missing.is_empty()
        || !status.not_unpacked.is_empty()
        || !status.outdated.is_empty()
        || !status.untracked.is_empty()
    {
        output.hint(
            "`fetchloom sync` brings the rest in line. edited and extra files stay as they are",
        );
    } else if !status.edited.is_empty() {
        output.hint("`fetchloom sync --force` puts the locked files back over edited ones");
    }
}
