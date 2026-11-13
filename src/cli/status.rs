//! `fetchloom status`: what differs between the data folder and data.lock, changing and downloading
//! nothing and hashing only files their size and time cannot vouch for, or all for `--verify`.

use std::cell::OnceCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;

use serde::Serialize;

use super::Context;
use super::sync::disk::{Blocker, Entry, Survey};
use super::sync::{Scope, place};
use crate::build::compare;
use crate::cache::{Cache, Member};
use crate::digest::{self, Sha256};
use crate::error::Error;
use crate::mirror::{Location, Mirrors};
use crate::output::{Output, Warning};
use crate::plan;
use crate::project::Project;
use crate::project::lock::archives::{self, Pinned};
use crate::project::lock::{Lock, Locked};
use crate::project::manifest::{Manifest, Members};
use crate::project::state::Candidate;
use crate::recipe;
use crate::stop::Stop;
use crate::threads::Threads;
use crate::units;

pub(super) mod ready;

/// the most rows of each kind a terminal shows; a pipe, a file and `--json` get them all.
const SHOWN: usize = 20;

#[derive(Default, Serialize)]
pub(super) struct Status {
    project: PathBuf,
    /// datasets data.toml changed since data.lock was written, and stale recipes.
    unlocked: Vec<String>,
    /// datasets still in data.lock whose entries were removed from data.toml.
    removed: Vec<String>,
    missing: Vec<String>,
    /// files fetchloom placed that changed since.
    edited: Vec<String>,
    /// files fetchloom did not place at paths the lock places, with other bytes than it pins.
    untracked: Vec<String>,
    /// untracked files holding the pinned bytes, which `sync` adopts without `--force`.
    adoptable: Vec<String>,
    /// files fetchloom placed for an older lock, or read-only for a dataset that now asks for
    /// writable copies, which `sync` replaces.
    outdated: Vec<String>,
    /// files no dataset places, not placed by fetchloom or changed since, which `sync` keeps.
    extra: Vec<String>,
    /// unchanged files fetchloom placed that no dataset needs now, which the next `sync` removes.
    unneeded: Vec<String>,
    /// archives this machine has not unpacked yet, so the files they hold are missing.
    not_unpacked: Vec<String>,
    /// recipes whose pinned result lacks files here that no cache or folder mirror holds, so only
    /// `fetchloom build` makes it; their files are in no other list.
    unbuilt: Vec<String>,
    /// datasets data.toml keeps out of the data folder with `place = false`.
    unplaced: Vec<String>,
    /// entries in the way of files data.lock places, by path, at a file's path or where a folder on
    /// its way belongs; `sync --force` replaces them, except a folder that holds anything.
    #[serde(serialize_with = "blocker_paths")]
    blocked: Vec<Blocker>,
    /// placed files that no longer hold the pinned bytes though no edit of this project's shows:
    /// cache links an edit changed, here or in another project, which `sync` puts back, and copies
    /// only hashing finds, which `sync --verify` puts back.
    damaged: Vec<String>,
    /// recipes with a rebuild that differs from data.lock kept, which `fetchloom build --accept`
    /// pins, each with where its files that differ are.
    candidates: Vec<Kept>,
    /// whether every placed file was hashed, so a match is a match of bytes.
    #[serde(skip)]
    verified: bool,
    /// whether the project has no data.lock, so nothing pins its files yet.
    #[serde(skip)]
    lockless: bool,
    /// damaged files linked to the cache's changed copy, for the reason the terminal gives.
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
    /// recipes among `unlocked` with what moved since each was built, which only a build fixes.
    #[serde(skip)]
    building: BTreeMap<String, String>,
    /// the datasets among `unlocked` that data.lock pins under another name data.toml does not
    /// have, which a sync carries over, each with that name.
    #[serde(skip)]
    renamed: BTreeMap<String, String>,
    /// the fetched datasets each of `unbuilt` is made from that the cache lacks files of, which
    /// building it reads first.
    #[serde(skip)]
    lacking: BTreeMap<String, Vec<String>>,
    /// the objects of the files in place with the bytes data.lock pins there, which a sync or a
    /// build takes into the cache from the data folder when it lacks them.
    #[serde(skip)]
    holding: HashSet<Sha256>,
    /// of `unneeded`, the files of each dataset of `unplaced`, which an earlier sync placed and the
    /// next one retires, told in its line rather than one by one, with its folder.
    #[serde(skip)]
    retiring: BTreeMap<String, (String, Vec<String>)>,
    /// of `missing`, those in a folder that holds no file now, as one deleted whole, with that
    /// folder, so a person at a terminal reads the folder once.
    #[serde(skip)]
    gone: HashMap<String, String>,
}

/// a rebuild a build kept of a recipe, whose files differ from what data.lock pins for its key.
#[derive(Serialize)]
struct Kept {
    name: String,
    /// the tree of its files, `sha256:` and its hash.
    tree: String,
    /// whether it came from another recipe or pin than data.lock has now, as after a checkout of
    /// another commit, so `build --accept` refuses it.
    stale: bool,
    /// each file that differs from data.lock, by path: changed ones, then missing, then extra.
    files: Vec<Differing>,
}

/// a file of a kept rebuild that differs from data.lock, with the cache files to compare.
#[derive(Serialize)]
struct Differing {
    path: String,
    /// `changed`, `missing` from the rebuild, or `extra`, made by the rebuild alone.
    difference: &'static str,
    /// the cache file holding the rebuild's bytes, unless it is missing or the cache lost it.
    file: Option<PathBuf>,
    /// the cache file holding the bytes data.lock pins, unless it is extra or the cache lacks it.
    pinned: Option<PathBuf>,
}

/// `status`, hashing every placed file for `verify`, as `--verify` asks, and failing with
/// `data.not_ready` for `strict`, as `--check` asks, when the data is not ready to use.
pub(super) fn run(context: &Context, verify: bool, strict: bool) -> Result<Status, Error> {
    let project = super::project::find(&context.output)?;
    let status = check(
        &project,
        context.threads,
        &context.stop,
        &context.mirrors.mirrors,
        verify.then_some(&context.output),
    )?;
    let output = &context.output;
    warn_git(output, &project)?;
    list(output, &status);
    if sum_up(output, &status) {
        // a sync would pin what the sources serve now, which a clone may not want
        if status.lockless {
            output.hint(&format!(
                "nothing pins the files' bytes yet: {}",
                super::project::unpinned()
            ));
        } else if status.needs_sync() {
            let does = match (status.places(), status.unneeded.is_empty()) {
                (false, false) => "removes the files fetchloom placed that no dataset needs now",
                (true, false) => {
                    "places missing or updated files, removes those no dataset needs now and reports any conflicts it keeps"
                }
                (_, true) => "places missing or updated files and reports any conflicts it keeps",
            };
            output.hint(&format!("`{}` {does}", super::project::command("sync")));
        }
        if !status.hashed_only.is_empty() {
            output.hint(&format!(
                "`{}` checks every file the same way and puts back those whose bytes changed",
                super::project::command("sync --verify")
            ));
        }
        if !status.edited.is_empty() || !status.untracked.is_empty() || !status.blocked.is_empty() {
            output.hint(&format!(
                "save any changes you need, then use `{}` to replace edited or conflicting files at paths in data.lock",
                super::project::command("sync --force")
            ));
        }
        if !status.building.is_empty() {
            output.hint(&format!(
                "`{}` shows the commands that would build them",
                super::project::command("build --dry-run")
            ));
        }
        match status.unbuilt.as_slice() {
            [] => {}
            [name] => output.hint(&format!(
                "`{}` makes it on this machine",
                super::project::command(&format!("build {name}"))
            )),
            names => output.hint(&format!(
                "`{}` makes them on this machine",
                super::project::command(&format!("build {}", names.join(" ")))
            )),
        }
    }
    for kept in status.candidates.iter().filter(|kept| kept.stale) {
        output.hint(&super::build::again(&kept.name));
    }
    if !status.candidates.is_empty() && !output.shows_details() {
        output.hint(&format!(
            "`{}` lists where the files of each kept rebuild that differ from data.lock are",
            super::project::command("status -v")
        ));
    }
    if strict {
        ready::ready(&ready::of_status(&status)).map_err(|error| error.with_report(&status))?;
    }
    Ok(status)
}

/// `fetchloom` alone inside a project: the line `status` ends with, without its lists.
pub(super) fn brief(context: &Context, project: &Project) -> Result<Status, Error> {
    let status = check(
        project,
        context.threads,
        &context.stop,
        &context.mirrors.mirrors,
        None,
    )?;
    let output = &context.output;
    warn_git(output, project)?;
    if sum_up(output, &status) {
        output.hint(&format!(
            "`{}` lists what differs",
            super::project::command("status")
        ));
    }
    Ok(status)
}

/// warns of the files a commit would leave out that a clone needs: lists data.lock names and code
/// recipes take in, and of lists data.lock no longer names, which a commit should not take.
fn warn_git(output: &Output, project: &Project) -> Result<(), Error> {
    warn_untracked(output, project);
    warn_unnamed(output, project);
    let (_, manifest) = project.manifest()?;
    super::project::warn_recipe_code(output, project, &manifest);
    Ok(())
}

/// the most lists a warning about lists nothing reads names in its command; `-v` names each.
const NAMED_LISTS: usize = 5;

/// warns of the lists in data.lock.d that data.lock no longer names, as after `git checkout` of an
/// older data.lock, which `git add data.lock.d` would commit. the help names the command that
/// removes them, since status deletes nothing and only data.lock's next write sweeps them.
fn warn_unnamed(output: &Output, project: &Project) {
    use crate::project::lock::{self, lists};
    let Ok(text) = std::fs::read_to_string(project.root.join(lock::FILE)) else {
        return;
    };
    let Some(entries) = lists::folder(&project.root)
        .ok()
        .and_then(|folder| std::fs::read_dir(folder).ok())
    else {
        return;
    };
    let named: HashSet<String> = lock::parts(&text).into_iter().collect();
    let mut unnamed: Vec<String> = entries
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|file| lists::is_part(file))
        .map(|file| format!("{}/{file}", lists::FOLDER))
        .filter(|part| !named.contains(part))
        .collect();
    if unnamed.is_empty() {
        return;
    }
    unnamed.sort();
    let tracked = std::env::var_os("PATH")
        .and_then(|path| crate::project::git::find(Some(&path), &project.root))
        .and_then(|git| crate::project::git::tracked(&git, &project.root, lists::FOLDER))
        .unwrap_or_default();
    let (in_git, loose): (Vec<String>, Vec<String>) =
        unnamed.into_iter().partition(|part| tracked.contains(part));
    for (parts, is_tracked) in [(in_git, true), (loose, false)] {
        let Some(first) = parts.first() else {
            continue;
        };
        let words = |parts: &[String]| {
            parts
                .iter()
                .map(|part| super::shell_word(part).into_owned())
                .collect::<Vec<_>>()
                .join(" ")
        };
        let (said, them) = match parts.len() {
            1 => (format!("data.lock no longer names {first}"), "it"),
            count => (
                format!(
                    "data.lock no longer names {} lists in data.lock.d, such as {first}",
                    units::grouped(count as u64)
                ),
                "them",
            ),
        };
        let help = match (is_tracked, parts.len() <= NAMED_LISTS) {
            (true, true) => format!("run `git rm {}`, then commit", words(&parts)),
            (true, false) => format!(
                "run `git rm` with their paths, as `git rm {}`, then commit",
                super::shell_word(first)
            ),
            (false, true) => format!("delete {}", parts.join(", ")),
            (false, false) => format!(
                "delete {them}; `{}` names each",
                super::project::command("status -v")
            ),
        };
        output.warn_detailed(
            Warning::LockStalePart,
            &format!("{said}, so nothing reads {them}"),
            Some(&help),
            &parts,
        );
        if parts.len() > 1 {
            for part in &parts {
                output.detail(part);
            }
        }
    }
}

/// warns of the lists data.lock names that git does not have, so a clone could not read data.lock;
/// `-v` names each. without git or a repository, nothing is said.
fn warn_untracked(output: &Output, project: &Project) {
    let Ok(text) = std::fs::read_to_string(project.root.join(crate::project::lock::FILE)) else {
        return;
    };
    let parts = crate::project::lock::parts(&text);
    let missing = super::project::untracked(&project.root, parts.iter().map(String::as_str));
    let said = match missing.as_slice() {
        [] => return,
        [one] => format!("data.lock names {one}, which git does not have"),
        [first, ..] => format!(
            "data.lock names {} lists git does not have, such as {first}",
            units::grouped(missing.len() as u64)
        ),
    };
    let them = if missing.len() == 1 { "it" } else { "them" };
    output.warn_detailed(
        Warning::GitLockPartUntracked,
        &said,
        Some(&format!(
            "run `git add data.lock.d`, then commit {them} with data.lock, since a clone without {them} cannot read data.lock"
        )),
        &missing,
    );
    if missing.len() > 1 {
        for part in &missing {
            output.detail(part);
        }
    }
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

/// each stale recipe, with what moved since it was built.
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

/// compares the data folder with data.lock by what the disk says, or with `verify` by hashing every
/// placed file under its bar. a settled project is answered as sync finds it, without a plan, and a
/// result is looked for in the cache and folder `mirrors` only, as status makes no request.
fn check(
    project: &Project,
    threads: Threads,
    stop: &Stop,
    mirrors: &Mirrors,
    verify: Option<&Output>,
) -> Result<Status, Error> {
    let (text, manifest) = project.manifest()?;
    let read = project.lock_and_text()?;
    let seen = place::Seen {
        manifest: digest::sha256(text.as_bytes()),
        lock: read
            .as_ref()
            .map(|(_, text)| digest::sha256(text.as_bytes())),
    };
    let lockless = read.is_none();
    // a sync carries a pin over to the name data.toml gives it now, so status reads data.lock
    // as that sync would leave it, and names what it renames
    let (lock, carried) =
        super::sync::rename::forecast(&manifest, read.map(|(lock, _)| lock).unwrap_or_default());
    let renamed: BTreeMap<String, String> = carried
        .into_iter()
        .map(|rename| (rename.new, rename.old))
        .collect();
    let (mut unlocked, removed) = out_of_date(&manifest, &lock);
    let building = unbuilt(project, &manifest, &lock)?;
    unlocked.extend(building.keys().cloned());
    unlocked.extend(renamed.keys().cloned());
    unlocked.sort();
    unlocked.dedup();
    let mut status = Status {
        project: project.root.clone(),
        verified: verify.is_some(),
        lockless,
        unlocked,
        removed,
        building,
        renamed,
        unplaced: manifest
            .datasets
            .iter()
            .filter(|(_, entry)| !entry.place)
            .map(|(name, _)| name.clone())
            .collect(),
        ..Status::default()
    };
    let cache = super::peek_cache()?;
    let mut survey = Survey::read(project, &manifest.data_dir, threads, stop)?;
    status.candidates = survey
        .state
        .candidates
        .iter()
        .filter(|(name, _)| {
            manifest
                .datasets
                .get(*name)
                .is_some_and(|entry| entry.recipe().is_some())
        })
        .map(|(name, candidate)| {
            // a step that cannot be read cannot be accepted either
            let key = recipe::Step::read(&project.root, &manifest, &lock, name)
                .ok()
                .flatten()
                .and_then(|step| step.key());
            kept(name, candidate, &lock, &cache, key)
        })
        .collect();
    // status writes no state, so files hashing found unchanged ask for nothing
    survey.needs_writing = false;
    survey.check_doubted(&project.root, &cache)?;
    if let Some(output) = verify {
        status.hashed_only = survey.verify(&project.root, &Scope::All, Some(output))?;
    }
    if place::untouched(project, &manifest, &survey, &seen, false, &Scope::All)?.is_some() {
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
    compare(&mut status, project, &survey, &planned)?;
    let unplaced: HashSet<&str> = status.unplaced.iter().map(String::as_str).collect();
    for path in &status.unneeded {
        if let Some(placed) = survey.state.files.get(path)
            && unplaced.contains(placed.dataset.as_str())
        {
            let folder = || plan::folder(&manifest, &placed.dataset);
            let (_, paths) = status
                .retiring
                .entry(placed.dataset.clone())
                .or_insert_with(|| (folder(), Vec::new()));
            paths.push(path.clone());
        }
    }
    unbuilt_here(&mut status, &manifest, &lock, &planned, &cache, mirrors);
    status.gone = gone(&status.missing, &manifest, &survey, &planned);
    let listed: HashSet<&String> = status.damaged.iter().collect();
    status.hashed_only.retain(|path| listed.contains(path));
    Ok(status)
}

/// the files of `missing` in a folder that holds no file now, as one deleted whole, each with
/// that folder, no higher than its dataset's own.
fn gone(
    missing: &[String],
    manifest: &Manifest,
    survey: &Survey,
    planned: &plan::Plan<'_>,
) -> HashMap<String, String> {
    let holding = place::holding(survey.disk.keys());
    let missing: HashSet<&str> = missing.iter().map(String::as_str).collect();
    planned
        .targets
        .iter()
        .filter(|target| missing.contains(target.path.as_str()))
        .filter_map(|target| {
            let floor = plan::folder(manifest, target.dataset);
            let folder = place::emptied(&holding, &target.path, &floor)?;
            Some((target.path.clone(), format!("{folder}/")))
        })
        .collect()
}

/// moves to `unbuilt` each recipe whose result sync would leave out, a missing or outdated file of
/// it found nowhere: not in the cache, the data folder, a folder mirror or the sync's downloads.
/// with a web mirror set nothing moves, as only a sync asks one.
fn unbuilt_here(
    status: &mut Status,
    manifest: &Manifest,
    lock: &Lock,
    planned: &plan::Plan<'_>,
    cache: &Cache,
    mirrors: &Mirrors,
) {
    if mirrors.web().next().is_some() {
        return;
    }
    let folders: Vec<&std::path::Path> = mirrors
        .list
        .iter()
        .filter_map(|location| match location {
            Location::Folder(root) => Some(root.as_path()),
            Location::Web(_) => None,
        })
        .filter(|root| crate::mirror::folder::probe(root).is_ok())
        .collect();
    let placing: HashSet<&str> = status
        .missing
        .iter()
        .chain(&status.outdated)
        .map(String::as_str)
        .collect();
    let needed = || {
        planned
            .targets
            .iter()
            .filter(|target| placing.contains(target.path.as_str()))
    };
    let fetched: HashSet<Sha256> = needed()
        .filter(|target| target.holder != plan::Holder::Result)
        .map(|target| target.sha256)
        .collect();
    let mut unbuilt: Vec<String> = needed()
        .filter(|target| {
            target.holder == plan::Holder::Result
                && !fetched.contains(&target.sha256)
                && !status.holding.contains(&target.sha256)
                && cache.find(&target.sha256, Some(target.size)).is_none()
                && !folders
                    .iter()
                    .any(|root| crate::mirror::folder::has(root, &target.sha256, target.size))
        })
        .map(|target| target.dataset.to_owned())
        .collect();
    unbuilt.sort();
    unbuilt.dedup();
    let left_out: HashSet<&str> = planned
        .targets
        .iter()
        .filter(|target| unbuilt.iter().any(|name| name == target.dataset))
        .map(|target| target.path.as_str())
        .collect();
    status
        .missing
        .retain(|path| !left_out.contains(path.as_str()));
    status
        .outdated
        .retain(|path| !left_out.contains(path.as_str()));
    for name in &unbuilt {
        let inputs = manifest
            .datasets
            .get(name)
            .and_then(crate::project::manifest::Entry::recipe)
            .map(|recipe| recipe.from.as_slice())
            .unwrap_or_default();
        let lacking: Vec<String> = inputs
            .iter()
            .filter(|input| {
                let Some(source) = manifest.datasets[*input].source() else {
                    return false;
                };
                lock.get(input)
                    .is_some_and(|locked| !held(cache, &status.holding, locked, &source.members))
            })
            .cloned()
            .collect();
        if !lacking.is_empty() {
            status.lacking.insert(name.clone(), lacking);
        }
    }
    status.unbuilt = unbuilt;
}

/// the kept rebuild `candidate` of the recipe `name`, beside the result `lock` pins for it, and
/// whether it answers `key`, the recipe's key now.
fn kept(
    name: &str,
    candidate: &Candidate,
    lock: &Lock,
    cache: &Cache,
    key: Option<Sha256>,
) -> Kept {
    let pinned = lock
        .result(name)
        .map_or(&[][..], |result| result.files.as_slice());
    let compared = compare::compare(pinned, &candidate.files);
    let made: HashMap<&str, &Member> = candidate
        .files
        .iter()
        .map(|file| (file.path.as_str(), file))
        .collect();
    let was: HashMap<&str, &Member> = pinned
        .iter()
        .map(|file| (file.path.as_str(), file))
        .collect();
    let held = |files: &HashMap<&str, &Member>, path: &str| {
        files
            .get(path)
            .and_then(|file| cache.find(&file.sha256, Some(file.size)))
    };
    let files = [
        ("changed", &compared.changed),
        ("missing", &compared.missing),
        ("extra", &compared.extra),
    ]
    .into_iter()
    .flat_map(|(difference, paths)| paths.iter().map(move |path| (difference, path)))
    .map(|(difference, path)| Differing {
        path: path.clone(),
        difference,
        file: held(&made, path),
        pinned: held(&was, path),
    })
    .collect();
    Kept {
        name: name.to_owned(),
        tree: format!("sha256:{}", candidate.tree),
        stale: !candidate.answers(key, lock.result(name)),
        files,
    }
}

/// whether this machine holds every file a build reads of `locked`: each in the cache or in place,
/// as `holding` says, and an archive whole in the cache or as the recorded files its `members`
/// table keeps, as gc keeps them.
fn held(cache: &Cache, holding: &HashSet<Sha256>, locked: &Locked, members: &Members) -> bool {
    let has = |sha256: &Sha256, size| {
        cache.find(sha256, Some(size)).is_some() || holding.contains(sha256)
    };
    archives::pinned(&locked.files)
        .iter()
        .all(|pinned| match pinned {
            Pinned::File(file) => has(&file.sha256, file.size),
            Pinned::Archive(archive) => {
                let filter = members.get(&archive.file.path);
                archive
                    .files()
                    .iter()
                    .all(|file| cache.find(&file.sha256, Some(file.size)).is_some())
                    || archive.recorded(cache).is_some_and(|recorded| {
                        recorded
                            .iter()
                            .filter(|member| filter.is_none_or(|filter| filter.keeps(&member.path)))
                            .all(|member| has(&member.sha256, member.size))
                    })
            }
        })
}

/// files each target of `planned` into the list of what differs, by how `survey` saw it, and
/// lists as extra every other file in the data folder.
fn compare(
    status: &mut Status,
    project: &Project,
    survey: &Survey,
    planned: &plan::Plan<'_>,
) -> Result<(), Error> {
    let targets = &planned.targets;
    let going = OnceCell::new();
    let going_now =
        || going.get_or_init(|| place::going(survey, targets, &planned.unknown, &project.root));
    let freeing = OnceCell::new();
    let mut listed = HashSet::new();
    for target in targets {
        let seen = survey.disk.get(&target.path);
        let file = project.root.join(&target.path);
        let list = match (seen, survey.state.files.get(&target.path)) {
            (None, _) => match survey.blocker(&target.path) {
                // what sync removes before it places anything stands in no file's way
                Some(blocker)
                    if !freeing
                        .get_or_init(|| survey.freeing(going_now()))
                        .frees(&blocker) =>
                {
                    if listed.insert(blocker.path.clone()) {
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
                if let Some(label) = place::pinned_label(target)
                    && let Some(actual) =
                        place::placed_other_size(&file, record, &target.sha256, target.size)
                {
                    return Err(place::mispinned(&label, target.size, actual));
                }
                if record.sha256 == target.sha256 {
                    if place::fits(target, record, survey) {
                        status.holding.insert(target.sha256);
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
                    status.holding.insert(target.sha256);
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
            // sync adopts it, and a build copies its bytes from here as from a placed file
            (Some(seen), None) if place::holds_target(target, seen, &file) => {
                status.holding.insert(target.sha256);
                &mut status.adoptable
            }
            (Some(_), None) => &mut status.untracked,
        };
        list.push(target.path.clone());
    }
    let mut wanted: HashSet<&str> = targets.iter().map(|target| target.path.as_str()).collect();
    // a file in the way is listed as blocking, not also as extra
    wanted.extend(status.blocked.iter().map(|blocker| blocker.path.as_str()));
    let going = going_now();
    // what fetchloom placed of a dataset whose archive's list this machine lacks may be that
    // archive's files, which only the list tells, so it is neither extra nor going
    let unsure: HashSet<&str> = planned
        .unknown
        .iter()
        .map(|(dataset, _)| *dataset)
        .collect();
    (status.unneeded, status.extra) = extra(survey, &wanted)
        .into_iter()
        .filter(|path| {
            !survey
                .state
                .files
                .get(path)
                .is_some_and(|placed| unsure.contains(placed.dataset.as_str()))
        })
        .partition(|path| going.contains(path));
    Ok(())
}

/// the files in the data folder that no dataset places, sorted, but for the staging leftovers of an
/// interrupted placement, which the next sync deletes.
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
    /// each kind of file difference with its verb and what follows each path, if anything.
    fn lists(&self) -> [(&'static str, &Vec<String>, Option<&'static str>); 7] {
        [
            ("Missing", &self.missing, None),
            ("Edited", &self.edited, None),
            ("Untracked", &self.untracked, None),
            (
                "Untracked",
                &self.adoptable,
                Some("it holds the pinned bytes, so `fetchloom sync` adopts it"),
            ),
            ("Outdated", &self.outdated, None),
            ("Extra", &self.extra, None),
            (
                "Extra",
                &self.unneeded,
                Some(
                    "fetchloom placed it and no dataset needs it now, so `fetchloom sync` removes it",
                ),
            ),
        ]
    }

    fn needs_sync(&self) -> bool {
        !self.unneeded.is_empty() || self.places()
    }

    /// whether a sync has more to do than remove what no dataset needs: pin, place or put back.
    fn places(&self) -> bool {
        !self.removed.is_empty()
            || self
                .unlocked
                .iter()
                .any(|name| !self.building.contains_key(name))
            || !self.missing.is_empty()
            || !self.not_unpacked.is_empty()
            || !self.outdated.is_empty()
            || !self.untracked.is_empty()
            || !self.adoptable.is_empty()
            || self
                .damaged
                .iter()
                .any(|path| !self.hashed_only.contains(path))
    }
}

/// a line for each dataset with `place = false`: faint on stderr once no file of it is left, else a
/// row saying the next sync removes them.
fn tell_unplaced(output: &Output, status: &Status) {
    for name in &status.unplaced {
        let Some((folder, paths)) = status.retiring.get(name) else {
            output.faint_step(
                "Not placed",
                &format!("{name} · place = false in data.toml"),
            );
            continue;
        };
        let files = match paths.len() {
            1 => "its file".to_owned(),
            count => format!("its {} files", units::grouped(count as u64)),
        };
        output.report(
            "Not placed",
            &format!(
                "{name} · place = false in data.toml, so `fetchloom sync` removes {files} from {folder}"
            ),
        );
        for path in paths {
            report_detail(output, path);
        }
    }
}

/// a row on stdout per difference, so a pipe or file gets rows and stderr the story; a script gets
/// every file, a terminal one row per folder gone whole and at most `SHOWN` per kind.
fn list(output: &Output, status: &Status) {
    let shown = output.answer_width().map(|_| SHOWN);
    tell_unplaced(output, status);
    let unpinned = if status.lockless {
        "data.lock does not exist"
    } else {
        "data.toml changed since data.lock was written"
    };
    for name in &status.unlocked {
        let reason = match (status.building.get(name), status.renamed.get(name)) {
            (Some(moved), _) => moved.clone(),
            (None, Some(old)) => format!(
                "data.lock pins it as {old}, which data.toml does not have, and `fetchloom sync` keeps that pin under this name without asking the source"
            ),
            (None, None) => unpinned.to_owned(),
        };
        output.report("Unlocked", &format!("{name} · {reason}"));
    }
    for name in &status.removed {
        output.report(
            "Removed",
            &format!("{name} · not in data.toml, so `fetchloom sync` takes it out of data.lock"),
        );
    }
    for kept in &status.candidates {
        tell_kept(output, kept, shown);
    }
    let retiring: HashSet<&String> = status
        .retiring
        .values()
        .flat_map(|(_, paths)| paths)
        .collect();
    for (verb, paths, said) in status.lists() {
        let mut rows: Vec<(String, u64)> = Vec::new();
        let mut folders: HashMap<&str, usize> = HashMap::new();
        for path in paths.iter().filter(|path| !retiring.contains(path)) {
            if let Some(folder) = status.gone.get(path).filter(|_| shown.is_some()) {
                // a folder that held one file is named by that file
                let at = *folders.entry(folder).or_insert_with(|| {
                    rows.push(((*path).clone(), 0));
                    rows.len() - 1
                });
                rows[at].1 += 1;
                if rows[at].1 > 1 {
                    rows[at].0 =
                        format!("{folder} · {}", units::count(rows[at].1, "file", "files"));
                }
            } else if let Some(said) = said {
                rows.push((format!("{path} · {said}"), 1));
            } else if status.shared.contains(path) {
                rows.push((
                    format!("{path} · changed through a hardlink other projects may share"),
                    1,
                ));
            } else if status.unsealed.contains(path) {
                rows.push((
                    format!(
                        "{path} · a link to the cache's copy made writable, so an edit would change it for every project"
                    ),
                    1,
                ));
            } else {
                rows.push((path.clone(), 1));
            }
        }
        report_rows(output, verb, rows, shown);
    }
    for archive in &status.not_unpacked {
        output.report(
            "Unchecked",
            &format!(
                "{archive} · this machine has no list of the files it holds, so they are not checked"
            ),
        );
    }
    for name in &status.unbuilt {
        let said = format!("{name} · its pinned result is not on this machine or a mirror");
        let said = match status.lacking.get(name).map(Vec::as_slice) {
            None | Some([]) => said,
            Some([one]) => format!("{said}, nor is {one}, which it is made from"),
            Some(many) => format!(
                "{said}, nor are {}, which it is made from",
                units::listed(many, "and")
            ),
        };
        output.report("Unbuilt", &said);
    }
    let blocked = status
        .blocked
        .iter()
        .map(|blocker| (format!("{} · {}", blocker.path, in_the_way(blocker)), 1));
    report_rows(output, "Blocked", blocked.collect(), shown);
    let damaged = status.damaged.iter().map(|path| {
        let cause = if status.linked.contains(path) {
            "linked to the cache's copy, which was changed"
        } else {
            "its bytes changed though its size and time did not"
        };
        (format!("{path} · {cause}"), 1)
    });
    report_rows(output, "Damaged", damaged.collect(), shown);
}

/// at most `shown` of `rows` under `verb`, each a row's text and the files it stands for, then
/// how many files the rest stand for and how to see them. none of `shown` prints every row.
fn report_rows(output: &Output, verb: &str, rows: Vec<(String, u64)>, shown: Option<usize>) {
    let most = shown.unwrap_or(usize::MAX);
    let hidden: u64 = rows.iter().skip(most).map(|(_, files)| files).sum();
    for (text, _) in rows.into_iter().take(most) {
        output.report(verb, &text);
    }
    report_more(output, hidden, false);
}

/// the row after those a terminal shows, saying how many more there are and that a pager gets
/// them all, as a script reading stdout does.
fn report_more(output: &Output, hidden: u64, detail: bool) {
    if hidden == 0 {
        return;
    }
    let text = format!(
        "and {} more · `{} | less` shows all",
        units::grouped(hidden),
        super::project::command("status")
    );
    if detail {
        report_detail(output, &text);
    } else {
        output.report("", &text);
    }
}

/// the row of a kept rebuild, and with `-v` each of its files that differs from data.lock with
/// the cache file that holds the rebuild's bytes, at most `shown` of them.
fn tell_kept(output: &Output, kept: &Kept, shown: Option<usize>) {
    let name = &kept.name;
    if kept.stale {
        output.report(
            "Kept",
            &format!("{name} · a rebuild from another recipe or pin than data.lock has now, so `fetchloom build --accept` refuses it"),
        );
    } else {
        output.report(
            "Kept",
            &format!("{name} · a rebuild that differs from data.lock; `fetchloom build --accept {name}` pins it"),
        );
    }
    let most = shown.unwrap_or(usize::MAX);
    for file in kept.files.iter().take(most) {
        let how = match file.difference {
            "extra" => "new",
            how => how,
        };
        let path = &file.path;
        report_detail(
            output,
            &match (&file.file, file.difference) {
                (Some(found), _) => format!("{how} {path}: {}", found.display()),
                (None, "missing") => format!("{how} {path}"),
                (None, _) => format!("{how} {path}, which the cache no longer holds"),
            },
        );
    }
    report_more(output, kept.files.len().saturating_sub(most) as u64, true);
}

/// a `-v` row under the row before it, on stdout with it.
fn report_detail(output: &Output, text: &str) {
    if output.shows_details() {
        output.report("", text);
    }
}

/// the closing line, and whether anything differs.
fn sum_up(output: &Output, status: &Status) -> bool {
    let differences = status.removed.len()
        + status.unlocked.len()
        + status.not_unpacked.len()
        + status.unbuilt.len()
        + status.blocked.len()
        + status.damaged.len()
        + status
            .lists()
            .iter()
            .map(|(_, paths, _)| paths.len())
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
                accessible: false,
            },
            Level::Quiet,
            false,
            false,
        );
        let clean = Status::default();
        assert!(!clean.needs_sync());
        assert!(!sum_up(&output, &clean));
        for category in 0..10 {
            let mut status = Status::default();
            let paths = match category {
                0 => &mut status.removed,
                1 => &mut status.unlocked,
                2 => &mut status.missing,
                3 => &mut status.not_unpacked,
                4 => &mut status.outdated,
                5 => &mut status.untracked,
                6 => &mut status.adoptable,
                7 => &mut status.unneeded,
                8 => &mut status.edited,
                _ => &mut status.extra,
            };
            paths.push("data/set/file.csv".to_owned());
            assert_eq!(status.needs_sync(), category < 8, "category {category}");
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

    /// a result whose missing file nothing here holds is unbuilt, its file in no other list,
    /// unless a web mirror is set, which only sync asks, so the file stays missing.
    #[test]
    fn a_web_mirror_keeps_a_result_missing_rather_than_unbuilt() {
        let folder = tempfile::tempdir().unwrap();
        let cache = Cache::open(folder.path().join("cache")).unwrap();
        let manifest = Manifest::parse(
            "[datasets.raw]\nref = \"https://example.org/raw.csv\"\n\n[datasets.clean]\nfrom = [\"raw\"]\nrun = [\"t\", \"{raw}\", \"{out}\"]\n",
        )
        .unwrap();
        let planned = plan::Plan {
            targets: vec![plan::Target {
                dataset: "clean",
                path: "data/clean/clean.csv".to_owned(),
                sha256: digest::sha256(b"done"),
                size: 4,
                copy: false,
                holder: plan::Holder::Result,
            }],
            unknown: Vec::new(),
        };
        let web = url::Url::parse("https://lab.example/fetchloom/").unwrap();
        for (mirrors, unbuilt) in [
            (Mirrors::default(), true),
            (Mirrors::new(vec![Location::Web(web)], &[], false), false),
        ] {
            let mut status = Status {
                missing: vec!["data/clean/clean.csv".to_owned()],
                ..Status::default()
            };
            unbuilt_here(
                &mut status,
                &manifest,
                &Lock::default(),
                &planned,
                &cache,
                &mirrors,
            );
            assert_eq!(status.unbuilt.len(), usize::from(unbuilt), "{mirrors:?}");
            assert_eq!(status.missing.is_empty(), unbuilt, "{mirrors:?}");
        }
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
        let survey = Survey::read(&project, "data", Threads::default(), &Stop::default()).unwrap();

        assert_eq!(
            extra(&survey, &HashSet::new()),
            [
                "data/weather/.fetchloom-notes.copy",
                "data/weather/table.csv"
            ]
        );
    }
}
