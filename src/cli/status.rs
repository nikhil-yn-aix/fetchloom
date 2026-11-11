//! `fetchloom status`: what differs between the data folder and data.lock. it changes nothing,
//! downloads nothing and hashes only files whose size and time cannot vouch for them, so it
//! answers at once even for large projects. asked to verify, it hashes every placed file.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use serde::Serialize;

use super::Context;
use super::sync::disk::{Blocker, Entry, Survey};
use super::sync::{Scope, place};
use crate::digest;
use crate::error::Error;
use crate::output::Output;
use crate::plan;
use crate::project::Project;
use crate::project::lock::Lock;
use crate::project::manifest::Manifest;
use crate::recipe;
use crate::threads::Threads;
use crate::units;

/// the most paths printed for each kind of difference. `--json` lists them all.
const SHOWN: usize = 20;

#[derive(Default, Serialize)]
pub(super) struct Status {
    project: PathBuf,
    /// datasets whose entry in data.toml changed since data.lock was written, and recipes whose
    /// result data.lock does not pin as building them now would make.
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
    /// files in the data folder that no dataset places.
    extra: Vec<String>,
    /// archives this machine has not unpacked yet, so the files they hold are missing.
    not_unpacked: Vec<String>,
    /// entries in the way of files data.lock places, by path: a link or special file at a
    /// file's path, a folder there, or a file, link or special file where a folder on a file's
    /// way belongs. `sync --force` replaces them, except a folder that holds anything.
    #[serde(serialize_with = "blocker_paths")]
    blocked: Vec<Blocker>,
    /// placed files that no longer hold the pinned bytes, though no edit of this project's shows:
    /// links to the cache's copy, which an edit through a link changed, here or in another
    /// project, and copies and reflinks that still look as placed, which only hashing finds.
    /// `sync` puts them back, or `sync --verify` for those only hashing finds.
    damaged: Vec<String>,
    /// whether every placed file was hashed, so a match is a match of bytes.
    #[serde(skip)]
    verified: bool,
    /// damaged files that are links to the cache's changed copy, for the reason the terminal
    /// gives.
    #[serde(skip)]
    linked: HashSet<String>,
    /// edited files placed as hardlinks, whose change every project sharing the link sees.
    #[serde(skip)]
    shared: HashSet<String>,
    /// damaged files only hashing every placed file found, which a plain sync cannot see.
    #[serde(skip)]
    hashed_only: HashSet<String>,
    /// outdated files that are links to the cache's copy someone made writable.
    #[serde(skip)]
    unsealed: HashSet<String>,
    /// the recipes among `unlocked`, each with what moved since it was built, which only
    /// `fetchloom build` brings in line.
    #[serde(skip)]
    building: BTreeMap<String, String>,
    /// exclude patterns that name files inside an archive, which they leave where they are.
    #[serde(skip)]
    unreached: Vec<plan::Unreached>,
}

/// `status`, hashing every placed file when `verify` asks, as `status --verify` does.
pub(super) fn run(context: &Context, verify: bool) -> Result<Status, Error> {
    let status = check(
        &super::project::find(&context.output)?,
        context.threads,
        verify,
    )?;
    let output = &context.output;
    list(output, &status);
    if sum_up(output, &status) {
        if status.needs_sync() {
            output.hint(
                "`fetchloom sync` places missing or updated files and reports any conflicts it keeps",
            );
        }
        if !status.hashed_only.is_empty() {
            output.hint(
                "`fetchloom sync --verify` checks every file the same way and puts back those whose bytes changed",
            );
        }
        if !status.edited.is_empty() || !status.untracked.is_empty() || !status.blocked.is_empty() {
            output.hint("save any changes you need, then use `fetchloom sync --force` to replace edited or conflicting files at paths in data.lock");
        }
        if !status.building.is_empty() {
            output.hint("`fetchloom build --dry-run` shows the commands that would build them");
        }
    }
    for found in &status.unreached {
        let (said, help) = place::unreached_said(found);
        output.hint(&format!("{said}; {help}"));
    }
    Ok(status)
}

/// `fetchloom` alone inside a project: the line `status` ends with, without its lists.
pub(super) fn brief(context: &Context, project: &Project) -> Result<Status, Error> {
    let status = check(project, context.threads, false)?;
    let output = &context.output;
    if sum_up(output, &status) {
        output.hint("`fetchloom status` lists what differs");
    }
    Ok(status)
}

/// the datasets data.lock no longer pins as data.toml asks: those data.toml fetches that the lock
/// lacks or pins for another spec or extract, and those the lock pins that data.toml dropped.
fn out_of_date(manifest: &Manifest, lock: &Lock) -> (Vec<String>, Vec<String>) {
    let unlocked = manifest
        .datasets
        .iter()
        .filter(|(name, entry)| {
            entry.source().is_some()
                && lock
                    .get(name)
                    .is_none_or(|locked| locked.drift(entry).is_some())
        })
        .map(|(name, _)| name.clone())
        .collect();
    let names = lock.datasets.iter().map(|dataset| &dataset.name);
    let removed = names
        .chain(lock.derived.iter().map(|result| &result.name))
        .filter(|name| !manifest.datasets.contains_key(*name))
        .cloned()
        .collect();
    (unlocked, removed)
}

/// each recipe whose result data.lock does not pin as building it now would make, with what
/// moved since it was built.
fn unbuilt(
    project: &Project,
    manifest: &Manifest,
    lock: &Lock,
) -> Result<BTreeMap<String, String>, Error> {
    let mut building = BTreeMap::new();
    for (name, entry) in &manifest.datasets {
        if entry.recipe().is_none() {
            continue;
        }
        let changes = recipe::drift(&project.root, manifest, lock, name)?;
        if !changes.is_empty() {
            building.insert(name.clone(), super::project::said(&changes));
        }
    }
    Ok(building)
}

/// compares the data folder with data.lock, reading only what the disk says about each file,
/// or, with `verify`, hashing every placed file too. a project still as a complete sync left it
/// is settled as sync finds it, without a plan, so status agrees with sync even once the cache
/// no longer records what an archive holds, as after a move and `cache gc --now`.
fn check(project: &Project, threads: Threads, verify: bool) -> Result<Status, Error> {
    let (text, manifest) = project.manifest()?;
    let read = project.lock_and_text()?;
    let seen = place::Seen {
        manifest: digest::sha256(text.as_bytes()),
        lock: read
            .as_ref()
            .map(|(_, text)| digest::sha256(text.as_bytes())),
    };
    let lock = read.map(|(lock, _)| lock).unwrap_or_default();
    let (mut unlocked, removed) = out_of_date(&manifest, &lock);
    let building = unbuilt(project, &manifest, &lock)?;
    unlocked.extend(building.keys().cloned());
    unlocked.sort();
    let mut status = Status {
        project: project.root.clone(),
        verified: verify,
        unlocked,
        removed,
        building,
        ..Status::default()
    };
    let cache = super::peek_cache()?;
    let mut survey = Survey::read(project, &manifest.data_dir, threads);
    // status writes no state, so files hashing found unchanged ask for nothing
    survey.needs_writing = false;
    survey.check_doubted(&project.root, &cache)?;
    if verify {
        status.hashed_only = survey.verify(&project.root, &Scope::All)?;
    }
    if place::untouched(project, &manifest, &survey, &seen, false)?.is_some() {
        let recorded: HashSet<&str> = survey.state.files.keys().map(String::as_str).collect();
        status.extra = extra(&survey, &recorded);
        return Ok(status);
    }
    let planned = plan::from_cache(&manifest, &lock, &cache)?;
    status.not_unpacked = planned
        .unknown
        .iter()
        .map(|(dataset, file)| format!("{}/{}", plan::folder(&manifest, dataset), file.path))
        .collect();
    survey.retarget(&project.root, &planned.targets);
    compare(&mut status, project, &survey, &planned);
    status.unreached = plan::unreached(&manifest, &planned.targets);
    let listed: HashSet<&String> = status.damaged.iter().collect();
    status.hashed_only.retain(|path| listed.contains(path));
    Ok(status)
}

/// files each target of `planned` into the list of what differs, by how `survey` saw it, and
/// lists as extra every other file in the data folder.
fn compare(status: &mut Status, project: &Project, survey: &Survey, planned: &plan::Plan<'_>) {
    let targets = &planned.targets;
    let mut going = None;
    for target in targets {
        let seen = survey.disk.get(&target.path);
        let file = project.root.join(&target.path);
        let list = match (seen, survey.state.files.get(&target.path)) {
            (None, _) => match survey.blocker(&target.path) {
                // what sync removes before it places anything stands in no file's way
                Some(blocker)
                    if !survey.frees(
                        &blocker,
                        going
                            .get_or_insert_with(|| place::going(survey, targets, &planned.unknown)),
                    ) =>
                {
                    if !status
                        .blocked
                        .iter()
                        .any(|known| known.path == blocker.path)
                    {
                        status.blocked.push(blocker);
                    }
                    continue;
                }
                _ => &mut status.missing,
            },
            (Some(_), Some(_)) if survey.elsewhere.contains(&target.path) => {
                status.linked.insert(target.path.clone());
                &mut status.damaged
            }
            (Some(_), Some(_)) if survey.damaged.contains(&target.path) => &mut status.damaged,
            (Some(seen), Some(record))
                if record.look == *seen && !survey.edited.contains(&target.path) =>
            {
                if record.sha256 == target.sha256 {
                    if place::fits(target, record, survey) {
                        continue;
                    }
                    if !target.copy {
                        status.unsealed.insert(target.path.clone());
                    }
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
    let mut wanted: HashSet<&str> = targets.iter().map(|target| target.path.as_str()).collect();
    // a file in the way is listed as blocking, not also as extra
    wanted.extend(status.blocked.iter().map(|blocker| blocker.path.as_str()));
    status.extra = extra(survey, &wanted);
}

/// the files in the data folder that no dataset places, sorted. what an interrupted placement
/// left under the hidden names it stages files in is not listed, as `get` never counts it,
/// since the next sync deletes it.
fn extra(survey: &Survey, wanted: &HashSet<&str>) -> Vec<String> {
    let leftovers: HashSet<&String> = survey.leftovers().collect();
    let mut extra: Vec<String> = survey
        .disk
        .keys()
        .filter(|path| !wanted.contains(path.as_str()) && !leftovers.contains(path))
        .cloned()
        .collect();
    extra.sort();
    extra
}

fn blocker_paths<S: serde::Serializer>(
    blocked: &[Blocker],
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.collect_seq(blocked.iter().map(|blocker| &blocker.path))
}

/// what stands in a file's way, in a few words.
fn in_the_way(blocker: &Blocker) -> &'static str {
    match (blocker.found, blocker.folder) {
        (Entry::File, _) => "a file is where a folder belongs",
        (Entry::Link, true) => "a link is where a folder belongs",
        (Entry::Special, true) => "a special file is where a folder belongs",
        (Entry::Link, false) => "a link is where a file belongs",
        (Entry::Special, false) => "a special file is where a file belongs",
        (Entry::Folder, _) if blocker.clearable => "an empty folder is where a file belongs",
        (Entry::Folder, _) => "a folder with files in it is where a file belongs",
    }
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
            || self
                .unlocked
                .iter()
                .any(|name| !self.building.contains_key(name))
            || !self.missing.is_empty()
            || !self.not_unpacked.is_empty()
            || !self.outdated.is_empty()
            || !self.untracked.is_empty()
            || self
                .damaged
                .iter()
                .any(|path| !self.hashed_only.contains(path))
    }
}

/// a line for each difference, at most `SHOWN` of each kind.
fn list(output: &Output, status: &Status) {
    for name in &status.unlocked {
        let reason = status.building.get(name).map_or(
            "data.toml changed since data.lock was written",
            String::as_str,
        );
        output.mark("Unlocked", &format!("{name} · {reason}"));
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
            } else if status.unsealed.contains(path) {
                output.mark(
                    verb,
                    &format!(
                        "{path} · a link to the cache's copy made writable, so an edit would change it for every project"
                    ),
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
    for blocker in status.blocked.iter().take(SHOWN) {
        output.mark(
            "Blocked",
            &format!("{} · {}", blocker.path, in_the_way(blocker)),
        );
    }
    if status.blocked.len() > SHOWN {
        let more = status.blocked.len() - SHOWN;
        output.mark("", &format!("and {more} more"));
    }
    for path in status.damaged.iter().take(SHOWN) {
        let cause = if status.linked.contains(path) {
            "linked to the cache's copy, which was changed"
        } else {
            "its bytes changed though its size and time did not"
        };
        output.mark("Damaged", &format!("{path} · {cause}"));
    }
    if status.damaged.len() > SHOWN {
        let more = status.damaged.len() - SHOWN;
        output.mark("", &format!("and {more} more"));
    }
}

/// the closing line, and whether anything differs.
fn sum_up(output: &Output, status: &Status) -> bool {
    let differences = status.removed.len()
        + status.unlocked.len()
        + status.not_unpacked.len()
        + status.blocked.len()
        + status.damaged.len()
        + status
            .lists()
            .iter()
            .map(|(_, paths)| paths.len())
            .sum::<usize>();
    if differences == 0 {
        output.done(
            "Ok",
            if status.verified {
                "everything matches data.lock, checked byte for byte"
            } else {
                "everything matches data.lock by size and time"
            },
        );
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
                links: false,
            },
            Level::Quiet,
            false,
            false,
        );
        let clean = Status::default();
        assert!(!clean.needs_sync());
        assert!(!sum_up(&output, &clean));
        for category in 0..8 {
            let mut status = Status::default();
            let paths = match category {
                0 => &mut status.removed,
                1 => &mut status.unlocked,
                2 => &mut status.missing,
                3 => &mut status.not_unpacked,
                4 => &mut status.outdated,
                5 => &mut status.untracked,
                6 => &mut status.edited,
                _ => &mut status.extra,
            };
            paths.push("data/set/file.csv".to_owned());
            assert_eq!(status.needs_sync(), category < 6, "category {category}");
            assert!(sum_up(&output, &status), "category {category}");
        }
        let mut damaged = Status::default();
        damaged.damaged.push("data/set/file.csv".to_owned());
        assert!(damaged.needs_sync());
        damaged.hashed_only.insert("data/set/file.csv".to_owned());
        assert!(!damaged.needs_sync(), "only sync --verify finds it");
        assert!(sum_up(&output, &damaged));
    }

    #[test]
    fn json_status_keeps_every_path_including_link_edits_and_unknown_archives() {
        let status = Status {
            damaged: (0..25)
                .map(|index| format!("data/set/{index}.csv"))
                .collect(),
            not_unpacked: vec!["data/set/archive.zip".to_owned()],
            removed: vec!["old".to_owned()],
            ..Status::default()
        };
        let value = serde_json::to_value(&status).unwrap();
        assert_eq!(value["damaged"].as_array().unwrap().len(), 25);
        assert_eq!(value["damaged"][24], "data/set/24.csv");
        assert!(value.get("edited_elsewhere").is_none());
        assert_eq!(
            value["not_unpacked"],
            serde_json::json!(["data/set/archive.zip"])
        );
        assert_eq!(value["removed"], serde_json::json!(["old"]));
        assert_eq!(value["edited"], serde_json::json!([]));
    }

    #[test]
    fn a_copy_an_interrupted_sync_left_behind_is_not_extra() {
        let dir = tempfile::tempdir().unwrap();
        let project = Project {
            root: dir.path().to_path_buf(),
        };
        let folder = project.root.join("data").join("weather");
        std::fs::create_dir_all(&folder).unwrap();
        for name in [
            "table.csv",
            ".fetchloom-0123456789abcdef.copy",
            ".fetchloom-notes.copy",
        ] {
            std::fs::write(folder.join(name), name).unwrap();
        }
        let survey = Survey::read(&project, "data", Threads::default());

        assert_eq!(
            extra(&survey, &HashSet::new()),
            [
                "data/weather/.fetchloom-notes.copy",
                "data/weather/table.csv"
            ]
        );
    }
}
