//! `fetchloom status`: what differs between the data folder and data.lock, changing and downloading
//! nothing and hashing only files their size and time cannot vouch for, or all for `--verify`.

use std::cell::OnceCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;

use serde::Serialize;

use super::Context;
use super::sync::disk::{Blocker, Entry, Folders, Survey};
use super::sync::placed_archives::PlacedArchives;
use super::sync::{Scope, place};
use crate::cache::Cache;
use crate::digest::{self, Sha256};
use crate::error::Error;
use crate::output::{Output, Warning};
use crate::plan;
use crate::project::lock::archives;
use crate::project::lock::{Lock, LockedFile, Waiting};
use crate::project::manifest::{self, Manifest};
use crate::project::state::Look;
use crate::project::{Project, native};
use crate::stop::Stop;
use crate::threads::Threads;
use crate::units;

pub(super) mod ready;

/// the most rows of each kind a terminal shows; a pipe, a file and `--json` get them all.
const SHOWN: usize = 20;

#[derive(Default, Serialize)]
pub(super) struct Status {
    project: PathBuf,
    /// datasets data.toml changed since data.lock was written.
    unlocked: Vec<String>,
    /// datasets still in data.lock whose entries were removed from data.toml.
    removed: Vec<String>,
    /// datasets data.lock pins by what their sources state, which a sync downloads and checks.
    not_downloaded: Vec<String>,
    missing: Vec<String>,
    /// files fetchloom placed that changed since.
    edited: Vec<String>,
    /// files fetchloom did not place at paths the lock places, with other bytes than it pins.
    untracked: Vec<String>,
    /// files fetchloom did not place that hold the pinned bytes, as in a copy of the project: no
    /// difference, since `sync` records them as placed.
    adoptable: Vec<String>,
    /// files fetchloom did not place at paths the lock places, of the pinned size, that this
    /// account cannot read, so what they hold is unknown: a sync cannot read them either.
    unreadable: Vec<String>,
    /// files fetchloom placed for an older lock, or read-only for a dataset that now asks for
    /// writable copies, which `sync` replaces.
    outdated: Vec<String>,
    /// files no dataset places, not placed by fetchloom or changed since, which `sync` keeps, that
    /// count as differences: a changed copy sync kept, a file moved aside, or one in a folder
    /// whose extras count.
    extra: Vec<String>,
    /// files a person put in a dataset's folder, which fetchloom keeps and never counts.
    yours: Vec<String>,
    /// the files of `extra` named as the changed copy sync keeps of a file it put back, with that
    /// file.
    #[serde(skip)]
    kept_copies: HashMap<String, String>,
    /// the files of `extra` that are a person's file `sync --force` moved out of the way, with the
    /// path it had and whether that path is free, so a sync gives it back.
    #[serde(skip)]
    set_aside: HashMap<String, (String, bool)>,
    /// unchanged files fetchloom placed that no dataset needs now, which the next `sync` removes.
    unneeded: Vec<String>,
    /// archives this machine has not unpacked yet, so the files they hold are missing.
    not_unpacked: Vec<String>,
    /// why the files in place of each of `not_unpacked` that has some cannot stand for it.
    #[serde(skip)]
    unproven: HashMap<String, String>,
    /// the imports with a file of `missing` whose bytes this machine's cache lacks, with those
    /// files: no link fetches them, so no sync places them.
    #[serde(skip)]
    imported_elsewhere: BTreeMap<String, Vec<String>>,
    /// datasets `fetchloom drop` freed on this machine, which a plain sync leaves out.
    dropped: Vec<String>,
    /// those of `dropped` no source gives back: imports.
    #[serde(skip)]
    dropped_owned: HashSet<String>,
    /// those of `dropped` whose drop was cut short before it freed what the cache holds of them.
    #[serde(skip)]
    dropped_unfreed: HashSet<String>,
    /// entries in the way of files data.lock places, by path, at a file's path or where a folder on
    /// its way belongs; `sync --force` replaces them, except a folder that holds anything.
    #[serde(serialize_with = "blocker_paths")]
    blocked: Vec<Blocker>,
    /// placed files that no longer hold the pinned bytes though no edit of this project's shows:
    /// cache links an edit changed, here or in another project, which `sync` puts back, and copies
    /// only hashing finds, which `sync --verify` puts back.
    damaged: Vec<String>,
    /// what a sync would download for the files it places, with about how long it takes.
    to_download: crate::net::estimate::Total,
    /// those downloads by the link each starts at, and its size.
    #[serde(skip)]
    downloads: Vec<(url::Url, Option<u64>)>,
    /// copies a stopped placement left in the data folder under hidden names, which the next
    /// sync removes.
    #[serde(skip)]
    unfinished: usize,
    /// whether every placed file was hashed, so a match is a match of bytes.
    #[serde(skip)]
    verified: bool,
    /// whether the project has no data.lock, so nothing pins its files yet.
    #[serde(skip)]
    lockless: bool,
    /// for a project with no data.lock, what pins its files' bytes, as the hint and `--check` say.
    #[serde(skip)]
    pins_lockless: String,
    /// damaged files linked to the cache's changed copy, for the reason the terminal gives.
    #[serde(skip)]
    linked: HashSet<String>,
    /// edited files placed as hardlinks, whose change every project sharing the link sees.
    #[serde(skip)]
    shared: HashSet<String>,
    /// damaged files only hashing every placed file found, which a plain sync cannot see.
    #[serde(skip)]
    hashed_only: HashSet<String>,
    /// what pins each of `not_downloaded`, as its row names it, such as its source's MD5.
    #[serde(skip)]
    pinned_by: HashMap<String, &'static str>,
    /// outdated files that are links to the cache's copy someone made writable.
    #[serde(skip)]
    unsealed: HashSet<String>,
    /// files of outputs that hashing found holding their pinned bytes though they look otherwise
    /// than recorded, each with its recorded look and its look now, which the state keeps.
    #[serde(skip)]
    verified_outputs: Vec<(String, Look, Look)>,
    /// the datasets among `unlocked` that data.lock pins under another name data.toml does not
    /// have, which a sync carries over, each with that name.
    #[serde(skip)]
    renamed: BTreeMap<String, String>,
    /// the datasets among `unlocked` that wait for their file list, by name, which a commit of
    /// data.lock.d brings back.
    #[serde(skip)]
    waiting: BTreeMap<String, Waiting>,
    /// the imports among `unlocked` that no sync pins, since no source lists their files, each
    /// with whether data.lock pins it, so its entry in data.toml changed since.
    #[serde(skip)]
    imports: BTreeMap<String, bool>,
    /// whether a plain sync is refused until an import is pinned, as one of `imports` is, or any
    /// import of a project with no data.lock, so no row or hint names one, and `--check` names
    /// the sync of each line's own dataset, which runs.
    #[serde(skip)]
    sync_refused: bool,
    /// the dataset of each file and archive `--check` names while a plain sync is refused.
    #[serde(skip)]
    dataset_of: HashMap<String, String>,
    /// the objects of the files in place with the bytes data.lock pins there, which a sync takes
    /// into the cache from the data folder when it lacks them.
    #[serde(skip)]
    holding: HashSet<Sha256>,
    /// of `unneeded`, the files of each dataset of `dropped` that a drop cut short left, which the
    /// next drop retires, told in its line rather than one by one, with its folder.
    #[serde(skip)]
    retiring: BTreeMap<String, (String, Vec<String>)>,
    /// of `missing`, those in a folder that holds no file now, as one deleted whole, with that
    /// folder, so a person at a terminal reads the folder once.
    #[serde(skip)]
    gone: HashMap<String, String>,
}

/// `status`, hashing every placed file for `verify`, as `--verify` asks, and failing with
/// `data.not_ready` for `strict`, as `--check` asks, when the data is not ready to use.
pub(super) fn run(context: &Context, verify: bool, strict: bool) -> Result<Status, Error> {
    let project = super::project::find(context)?;
    let (mut status, manifest, lock) = check(
        &project,
        context.threads,
        &context.stop,
        verify.then_some(&context.output),
    )?;
    let downloads = super::fetch::askable(context, std::mem::take(&mut status.downloads));
    status.to_download = super::fetch::planned(context, downloads);
    let output = &context.output;
    warn_git(output, &project, lock.as_deref());
    let waiting: Vec<Waiting> = status.waiting.values().cloned().collect();
    super::project::tell_waiting(output, &waiting, &super::sync::Scope::All);
    list(output, &status);
    if sum_up(output, &status) {
        let total = &status.to_download;
        super::fetch::tell_would(output, &units::count(total.files, "file", "files"), total);
        // a sync would pin what the sources serve now, which a clone may not want
        if status.lockless && !manifest.datasets.is_empty() {
            output.hint(&format!(
                "nothing pins the files' bytes yet: {}",
                status.pins_lockless
            ));
        } else {
            tell_imports(output, &status.imports);
        }
        if !status.lockless && status.needs_sync() {
            let does = match (status.places(), status.unneeded.is_empty()) {
                (false, false) => "removes the files fetchloom placed that no dataset needs now",
                (true, false) => {
                    "places missing or updated files, removes those no dataset needs now and reports any conflicts it keeps"
                }
                (_, true) => "places missing or updated files and reports any conflicts it keeps",
            };
            // a sync refuses while an import waits for the steps above
            let then = if status.sync_refused { "then " } else { "" };
            output.hint(&format!(
                "{then}`{}` {does}",
                super::project::command("sync")
            ));
        }
        tell_imported_elsewhere(output, &status.imported_elsewhere);
        tell_repairs(output, &status);
    }
    if status.unfinished > 0 {
        let them = if status.unfinished == 1 { "it" } else { "them" };
        let first = if status.sync_refused { FIRST } else { "" };
        output.hint(&format!(
            "the data folder holds {} from a stopped run; {first}`{}` removes {them}",
            units::count(
                status.unfinished as u64,
                "unfinished copy",
                "unfinished copies"
            ),
            super::project::command("sync")
        ));
    }
    if strict {
        ready::ready_by(&ready::of_status(&status), &|line| {
            status.only_sync_of(line)
        })
        .map_err(|error| error.with_report(&status))?;
    }
    Ok(status)
}

/// what the hints that name a sync say first while one is refused, after the steps that pin an
/// import.
const FIRST: &str = "once the steps above are done, ";

/// how a sync with more than its plain run sets right what `status` found: hashing every file,
/// replacing a person's file, or reading one this account could not.
fn tell_repairs(output: &Output, status: &Status) {
    let first = if status.sync_refused { FIRST } else { "" };
    if !status.hashed_only.is_empty() {
        output.hint(&format!(
            "{first}`{}` checks every file the same way and puts back those whose bytes changed",
            super::project::command("sync --verify")
        ));
    }
    if !status.edited.is_empty() || !status.untracked.is_empty() || !status.blocked.is_empty() {
        output.hint(&format!(
            "{first}save any changes you need, then use `{}` to replace edited or conflicting files at paths in data.lock",
            super::project::command("sync --force")
        ));
    }
    if !status.unreadable.is_empty() {
        let (files, its) = match status.unreadable.len() {
            1 => ("1 file at a path data.lock places".to_owned(), "its"),
            count => (
                format!(
                    "{} files at paths data.lock places",
                    units::grouped(count as u64)
                ),
                "their",
            ),
        };
        output.hint(&format!(
            "this account cannot read {files}: check {its} permissions, then {first}run `{}`",
            super::project::command("sync")
        ));
    }
}

/// how to pin each import of `imports`, which no sync pins, by the words a sync refuses it with:
/// an import whose entry changed since data.lock pinned it, or one data.lock holds no pin of.
fn tell_imports(output: &Output, imports: &BTreeMap<String, bool>) {
    for (name, pinned) in imports {
        let refused = if *pinned {
            crate::source::local::changed(name)
        } else {
            crate::source::local::unpinned(name)
        };
        if let Some(help) = refused.help {
            output.hint(&help);
        }
    }
}

/// how to bring the missing files of each import of `lacking`, which no sync fetches.
fn tell_imported_elsewhere(output: &Output, lacking: &BTreeMap<String, Vec<String>>) {
    let names: Vec<&str> = lacking.keys().map(String::as_str).collect();
    let text = match names.as_slice() {
        [] => return,
        [one] => format!(
            "{one} was imported from files, which no sync fetches: `{}` brings them from where they are",
            super::project::command(&format!("add <folder> --as {one}"))
        ),
        many => format!(
            "{} were imported from files, which no sync fetches: `{}` brings each one's from where they are",
            units::listed(many, "and"),
            super::project::command("add <folder> --as <name>")
        ),
    };
    output.hint(&text);
}

/// `fetchloom` alone inside a project: the line `status` ends with, without its lists.
pub(super) fn brief(context: &Context, project: &Project) -> Result<Status, Error> {
    let (status, manifest, lock) = check(project, context.threads, &context.stop, None)?;
    let output = &context.output;
    warn_git(output, project, lock.as_deref());
    if !sum_up(output, &status) {
        return Ok(status);
    }
    let status_command = super::project::command("status");
    if status.sync_repairs_all() {
        output.hint(&format!(
            "`{}` puts the pinned files in {}/; `{status_command}` lists them",
            super::project::command("sync"),
            manifest.data_dir
        ));
    } else {
        output.hint(&format!("`{status_command}` lists what differs"));
    }
    Ok(status)
}

/// warns of the files a commit would leave out that a clone needs, the lists data.lock names,
/// and of lists data.lock no longer names, which a commit should not take.
fn warn_git(output: &Output, project: &Project, lock: Option<&str>) {
    warn_untracked(output, project);
    warn_unnamed(output, project, lock);
}

/// the most lists a warning about lists nothing reads names in its command; `-v` names each.
const NAMED_LISTS: usize = 5;

/// warns of the lists in data.lock.d that data.lock no longer names, as after `git checkout` of an
/// older data.lock, which `git add data.lock.d` would commit. the help names the command that
/// removes them, since status deletes nothing and only data.lock's next write sweeps them.
fn warn_unnamed(output: &Output, project: &Project, lock: Option<&str>) {
    use crate::project::lock::{self, lists};
    let Some(text) = lock else {
        return;
    };
    let Some(entries) = lists::folder(&project.root)
        .ok()
        .and_then(|folder| std::fs::read_dir(folder).ok())
    else {
        return;
    };
    let named: HashSet<String> = lock::parts(text).into_iter().collect();
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
        let rm = |words: &str| crate::suggested::git(&project.root, &format!("rm {words}"));
        let help = match (is_tracked, parts.len() <= NAMED_LISTS) {
            (true, true) => format!("run `{}`, then commit", rm(&words(&parts))),
            (true, false) => format!(
                "run `git rm` with their paths, as `{}`, then commit",
                rm(&super::shell_word(first))
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
            "run `{}`, then commit {them} with data.lock, since a clone without {them} cannot read data.lock",
            crate::suggested::git(&project.root, "add data.lock.d")
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
            lock.get(name)
                .or(lock.stated(name).map(|stated| &stated.pin))
                .is_none_or(|locked| locked.drift(entry).is_some())
        })
        .map(|(name, _)| name.clone())
        .collect();
    let names = lock.datasets.iter().map(|dataset| &dataset.name);
    // one waiting for its list goes from data.lock too once data.toml no longer has it
    let removed = names
        .chain(lock.stated.iter().map(|stated| &stated.pin.name))
        .chain(lock.still_waiting().map(|waiting| &waiting.name))
        .filter(|name| !manifest.datasets.contains_key(*name))
        .cloned()
        .collect();
    (unlocked, removed)
}

/// the datasets of data.toml that data.lock pins by their sources' digests alone, but those
/// `unlocked`, with what pins each.
fn not_downloaded(
    manifest: &Manifest,
    lock: &Lock,
    unlocked: &[String],
) -> (Vec<String>, HashMap<String, &'static str>) {
    let pinned: Vec<&crate::project::lock::Stated> = lock
        .stated
        .iter()
        .filter(|stated| {
            manifest.datasets.contains_key(stated.name())
                && !unlocked.iter().any(|name| name == stated.name())
        })
        .collect();
    let names = pinned
        .iter()
        .map(|stated| stated.name().to_owned())
        .collect();
    let by = pinned
        .iter()
        .map(|stated| (stated.name().to_owned(), stated.digests()))
        .collect();
    (names, by)
}

/// the datasets of data.toml that wait for their file list, by name.
/// the imports among `unlocked` that no sync pins, since no source lists their files, each with
/// whether `lock` pins it, and whether a plain sync is refused for them, as it is for any import
/// of a project with no data.lock, as `lockless` says. a dataset of `apart`, waiting for its
/// file list or renamed, is told as such.
fn imports(
    manifest: &Manifest,
    (lock, lockless): (&Lock, bool),
    unlocked: &[String],
    apart: (&BTreeMap<String, Waiting>, &BTreeMap<String, String>),
) -> (BTreeMap<String, bool>, bool) {
    let imports: BTreeMap<String, bool> = unlocked
        .iter()
        .filter(|name| !lockless && !apart.0.contains_key(*name) && !apart.1.contains_key(*name))
        .filter(|name| {
            manifest
                .datasets
                .get(*name)
                .is_some_and(manifest::Entry::is_import)
        })
        .map(|name| (name.clone(), lock.get(name).is_some()))
        .collect();
    let refused = !imports.is_empty()
        || (lockless && manifest.datasets.values().any(manifest::Entry::is_import));
    (imports, refused)
}

/// the plan for `lock`, with each archive the cache knows nothing of whose files sit in place
/// standing for them, as a sync would take it, though nothing is recorded. the archives it still
/// knows nothing of go in `status`, with why the files in place of any cannot stand for it.
fn planned<'l>(
    status: &mut Status,
    project: &Project,
    (manifest, lock): (&Manifest, &'l Lock),
    (cache, survey): (&Cache, &Survey),
    (threads, stop): (Threads, &Stop),
) -> Result<plan::Plan<'l>, Error> {
    let mut planned = plan::from_cache(manifest, lock, cache)?;
    if !planned.unknown.is_empty() {
        let unknown: Vec<(String, &LockedFile)> = planned
            .unknown
            .iter()
            .map(|(dataset, file)| ((*dataset).to_owned(), *file))
            .collect();
        let unknown = archives::of(lock, unknown);
        let found = PlacedArchives::find(
            &project.root,
            (manifest, lock),
            survey,
            &unknown,
            (threads, stop),
        )?;
        status.unproven = found.unproven();
        if !found.is_empty() {
            planned = found.plan(manifest, lock, cache)?;
        }
    }
    status.not_unpacked = planned
        .unknown
        .iter()
        .map(|(dataset, file)| format!("{}/{}", plan::folder(manifest, dataset), file.path))
        .collect();
    Ok(planned)
}

/// the dataset of each file and archive of `not_unpacked` that `planned` places.
fn datasets_of(planned: &plan::Plan<'_>, not_unpacked: &[String]) -> HashMap<String, String> {
    let archives = planned
        .unknown
        .iter()
        .map(|(dataset, _)| (*dataset).to_owned());
    planned
        .targets
        .iter()
        .map(|target| (target.path.clone(), target.dataset.to_owned()))
        .chain(not_unpacked.iter().cloned().zip(archives))
        .collect()
}

fn waiting(lock: &Lock, manifest: &Manifest) -> BTreeMap<String, Waiting> {
    lock.still_waiting()
        .filter(|waiting| manifest.datasets.contains_key(&waiting.name))
        .map(|waiting| (waiting.name.clone(), waiting.clone()))
        .collect()
}

/// compares the data folder with data.lock by what the disk says, or with `verify` by hashing every
/// placed file under its bar. a settled project is answered as sync finds it, without a plan. it
/// hands back data.toml and data.lock's text as it read them, so the warnings read neither again.
fn check(
    project: &Project,
    threads: Threads,
    stop: &Stop,
    verify: Option<&Output>,
) -> Result<(Status, Manifest, Option<String>), Error> {
    let (text, manifest) = project.manifest()?;
    let (read, lock_text) = project.lock_and_text()?.unzip();
    let seen = place::Seen {
        manifest: digest::sha256(text.as_bytes()),
        lock: lock_text
            .as_ref()
            .map(|text| digest::sha256(text.as_bytes())),
    };
    let lockless = read.is_none();
    // a sync carries a pin over to the name data.toml gives it now, so status reads data.lock
    // as that sync would leave it, and names what it renames
    let (lock, carried) = super::sync::rename::forecast(&manifest, read.unwrap_or_default());
    let renamed: BTreeMap<String, String> = carried
        .into_iter()
        .map(|rename| (rename.new, rename.old))
        .collect();
    let (mut unlocked, removed) = out_of_date(&manifest, &lock);
    let waiting = waiting(&lock, &manifest);
    unlocked.extend(waiting.keys().cloned());
    unlocked.extend(renamed.keys().cloned());
    unlocked.sort();
    unlocked.dedup();
    let (not_downloaded, pinned_by) = not_downloaded(&manifest, &lock, &unlocked);
    let (imports, sync_refused) = imports(
        &manifest,
        (&lock, lockless),
        &unlocked,
        (&waiting, &renamed),
    );
    let mut status = Status {
        project: project.root.clone(),
        verified: verify.is_some(),
        sync_refused,
        lockless,
        unlocked,
        removed,
        not_downloaded,
        pinned_by,
        renamed,
        waiting,
        imports,
        pins_lockless: if lockless {
            super::project::unpinned(&manifest)
        } else {
            String::new()
        },
        ..Status::default()
    };
    let cache = super::peek_cache()?;
    let mut survey = Survey::read(project, &Folders::of(&manifest), threads, stop)?;
    let manifest = dropped_here(&mut status, &manifest, &survey);
    status.unfinished = survey.leftovers().count();
    // status writes no state, so files hashing found unchanged ask for nothing
    survey.needs_writing = false;
    survey.check_doubted(&project.root, &cache)?;
    if let Some(output) = verify {
        status.hashed_only = survey.verify(&project.root, &Scope::All, Some(output))?;
    }
    if place::untouched(project, &manifest, &survey, &seen, false, &Scope::All)?.is_some() {
        let recorded: HashSet<&str> = survey.state.files.keys().map(String::as_str).collect();
        status.extra = extra(&survey, &recorded, &manifest);
        status.kept_copies = kept_copies(&status.extra, &recorded);
        status.set_aside = set_aside(&status.extra, &survey);
        sort_extra(&mut status, &plan::Owned::of(&manifest));
        return Ok((status, manifest, lock_text));
    }
    let planned = planned(
        &mut status,
        project,
        (&manifest, &lock),
        (&cache, &survey),
        (threads, stop),
    )?;
    if status.sync_refused {
        status.dataset_of = datasets_of(&planned, &status.not_unpacked);
    }
    survey.retarget(&project.root, &planned.targets);
    compare(&mut status, project, &manifest, &survey, &planned)?;
    keep_verified(project, &status.verified_outputs, survey.started);
    // a dataset dropped here whose files a drop cut short left is told in one line
    let dropped: HashSet<&str> = status.dropped.iter().map(String::as_str).collect();
    for path in &status.unneeded {
        if let Some(placed) = survey.state.files.get(path)
            && dropped.contains(placed.dataset.as_str())
        {
            let folder = || plan::folder(&manifest, &placed.dataset);
            let (_, paths) = status
                .retiring
                .entry(placed.dataset.clone())
                .or_insert_with(|| (folder(), Vec::new()));
            paths.push(path.clone());
        }
    }
    status.downloads = downloads(&status, &planned, &cache);
    status.imported_elsewhere = imported_elsewhere(&status.missing, &manifest, &planned, &cache);
    status.gone = gone(&status.missing, &manifest, &survey, &planned);
    let listed: HashSet<&String> = status.damaged.iter().collect();
    status.hashed_only.retain(|path| listed.contains(path));
    drop(planned);
    Ok((status, manifest, lock_text))
}

/// `manifest` as a plain sync places it, without the datasets dropped here, which `status` names:
/// a dataset dropped here is no difference, since a plain sync leaves it out.
fn dropped_here(status: &mut Status, manifest: &Manifest, survey: &Survey) -> Manifest {
    status.dropped = manifest
        .datasets
        .keys()
        .filter(|name| survey.state.dropped.contains(*name))
        .cloned()
        .collect();
    status.dropped_owned = status
        .dropped
        .iter()
        .filter(|name| manifest.datasets[*name].is_import())
        .cloned()
        .collect();
    status.dropped_unfreed = status
        .dropped
        .iter()
        .filter(|name| survey.state.freeing.contains(*name))
        .cloned()
        .collect();
    super::drop::placed_by(manifest, &Scope::All, &survey.state.dropped).into_owned()
}

/// the files a sync of `status` would download, once each, by the link it starts at: one neither
/// the cache nor the data folder holds, or the archive holding it when the cache lacks that too,
/// and each archive never unpacked here that the cache lacks, which a sync downloads first.
fn downloads(
    status: &Status,
    planned: &plan::Plan<'_>,
    cache: &Cache,
) -> Vec<(url::Url, Option<u64>)> {
    let placing: HashSet<&str> = status
        .missing
        .iter()
        .chain(&status.outdated)
        .chain(&status.damaged)
        .map(String::as_str)
        .collect();
    let mut seen = HashSet::new();
    let lacks = |sha256: &Sha256, size: u64| {
        !status.holding.contains(sha256) && cache.find(sha256, Some(size)).is_none()
    };
    planned
        .targets
        .iter()
        .filter(|target| placing.contains(target.path.as_str()))
        .filter(|target| lacks(&target.sha256, target.size))
        .map(|target| target.file)
        .chain(planned.unknown.iter().map(|(_, archive)| *archive))
        .filter(|file| !file.url.is_empty() && seen.insert(file.sha256))
        .filter(|file| lacks(&file.sha256, file.size))
        .filter_map(|file| Some((url::Url::parse(&file.url).ok()?, Some(file.size))))
        .collect()
}

/// the imports of `planned` with a file of `missing` whose bytes `cache` lacks, each with those
/// files.
fn imported_elsewhere(
    missing: &[String],
    manifest: &Manifest,
    planned: &plan::Plan<'_>,
    cache: &Cache,
) -> BTreeMap<String, Vec<String>> {
    let missing: HashSet<&str> = missing.iter().map(String::as_str).collect();
    let mut lacking: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for target in &planned.targets {
        if missing.contains(target.path.as_str())
            && manifest
                .datasets
                .get(target.dataset)
                .is_some_and(manifest::Entry::is_import)
            && cache.find(&target.sha256, Some(target.size)).is_none()
        {
            lacking
                .entry(target.dataset.to_owned())
                .or_default()
                .push(target.path.clone());
        }
    }
    lacking
}

/// the files of `missing` in a folder that holds no file now, as one deleted whole, each with
/// that folder, no higher than its dataset's own.
fn gone(
    missing: &[String],
    manifest: &Manifest,
    survey: &Survey,
    planned: &plan::Plan<'_>,
) -> HashMap<String, String> {
    let holding = place::holding(
        survey
            .disk
            .keys()
            .filter(|path| !survey.made_by_system(path)),
    );
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

/// files each target of `planned` into the list of what differs, by how `survey` saw it, and
/// lists as extra every other file in the data folder.
fn compare(
    status: &mut Status,
    project: &Project,
    manifest: &Manifest,
    survey: &Survey,
    planned: &plan::Plan<'_>,
) -> Result<(), Error> {
    let targets = &planned.targets;
    let going = OnceCell::new();
    let going_now = || {
        going.get_or_init(|| {
            place::going(
                survey,
                targets,
                (&planned.unknown, &planned.waiting),
                (&place::Outputs::of(manifest), &project.root),
            )
        })
    };
    let freeing = OnceCell::new();
    let mut listed = HashSet::new();
    for target in targets {
        let seen = survey.disk.get(&target.path);
        let file = native(&project.root, &target.path);
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
                    if !target.copy && record.method == crate::place::Method::Hardlink {
                        status.unsealed.insert(target.path.clone());
                    }
                }
                &mut status.outdated
            }
            // sync takes a changed file that holds the locked bytes as placed, and replaces one
            // that still holds the bytes placed for another pin
            (Some(seen), Some(record)) => match place::held(target, record, seen, &file) {
                place::Held::Pinned => {
                    if !target.copy || place::now_writable(&file) {
                        status.holding.insert(target.sha256);
                        if record.output {
                            let found = (target.path.clone(), record.look, *seen);
                            status.verified_outputs.push(found);
                        }
                        continue;
                    }
                    &mut status.outdated
                }
                place::Held::Earlier => &mut status.outdated,
                place::Held::Other => {
                    if record.method == crate::place::Method::Hardlink
                        && crate::place::has_other_names(&file)
                    {
                        status.shared.insert(target.path.clone());
                    }
                    &mut status.edited
                }
            },
            (Some(seen), None) => unrecorded(status, target, seen, &file),
        };
        list.push(target.path.clone());
    }
    let mut wanted: HashSet<&str> = targets.iter().map(|target| target.path.as_str()).collect();
    // a file in the way is listed as blocking, not also as extra
    wanted.extend(status.blocked.iter().map(|blocker| blocker.path.as_str()));
    let going = going_now();
    // what fetchloom placed of a dataset whose archive's list this machine lacks may be that
    // archive's files, which only the list tells, and so may what it placed of a dataset waiting
    // for its file list, so it is neither extra nor going, and nor is what it placed of a dataset
    // data.lock does not pin, which a sync pins before it places
    let mut unsure = place::unsure((&planned.unknown, &planned.waiting));
    unsure.extend(status.unlocked.iter().map(String::as_str));
    let unrecorded = unrecorded_folders(manifest, survey, &planned.unknown);
    (status.unneeded, status.extra) = extra(survey, &wanted, manifest)
        .into_iter()
        .filter(|path| match survey.state.files.get(path) {
            Some(placed) => !unsure.contains(placed.dataset.as_str()),
            None => !unrecorded
                .iter()
                .any(|folder| path.starts_with(folder.as_str())),
        })
        .partition(|path| going.contains(path));
    status.kept_copies = kept_copies(&status.extra, &wanted);
    status.set_aside = set_aside(&status.extra, survey);
    sort_extra(status, &plan::Owned::of(manifest));
    Ok(())
}

/// leaves in `extra` only the files fetchloom did not place that count as differences: a changed
/// copy sync kept, a file moved aside, and any in a folder whose extras count. the others in a
/// dataset's folder are a person's, kept in `yours`, and those outside every dataset's folder are
/// none of fetchloom's business.
fn sort_extra(status: &mut Status, owned: &plan::Owned) {
    for path in std::mem::take(&mut status.extra) {
        if status.kept_copies.contains_key(&path) || status.set_aside.contains_key(&path) {
            status.extra.push(path);
            continue;
        }
        match owned.holding(&path) {
            Some(folder) if folder.extras_count => status.extra.push(path),
            Some(_) => status.yours.push(path),
            None => {}
        }
    }
}

/// the list of `status` for a file fetchloom did not place at `target`'s path, seen as `seen`, as
/// a sync would take it: one it adopts, as in a copy of the project, taking its bytes into the
/// cache from there as from a placed file, one it replaces, read-only where the dataset asks for
/// writable copies, one with other bytes it keeps, or one no one here can read.
fn unrecorded<'s>(
    status: &'s mut Status,
    target: &plan::Target<'_>,
    seen: &crate::project::state::Look,
    file: &std::path::Path,
) -> &'s mut Vec<String> {
    match place::reads_as_target(target, seen, file) {
        Ok(true) if target.copy && !place::now_writable(file) => &mut status.outdated,
        Ok(true) => {
            status.holding.insert(target.sha256);
            &mut status.adoptable
        }
        Ok(false) => &mut status.untracked,
        Err(_) => &mut status.unreadable,
    }
}

/// the folders, each ending in `/`, where an archive of `unknown` unpacks for a dataset of which
/// `survey` records no placed file, as in a copy, whose state is not used: any file there may be
/// one of the archive's.
fn unrecorded_folders(
    manifest: &Manifest,
    survey: &Survey,
    unknown: &[(&str, &LockedFile)],
) -> Vec<String> {
    if unknown.is_empty() {
        return Vec::new();
    }
    let recorded: HashSet<&str> = survey
        .state
        .files
        .values()
        .map(|placed| placed.dataset.as_str())
        .collect();
    unknown
        .iter()
        .filter(|(dataset, _)| !recorded.contains(dataset))
        .map(|(dataset, archive)| {
            format!(
                "{}/",
                plan::unpacked_folder(manifest, dataset, &archive.path)
            )
        })
        .collect()
}

/// the files of `extra` the state records as a person's file `sync --force` moved out of the way,
/// each with the path it had and whether nothing stands there, so a sync gives it back.
fn set_aside(extra: &[String], survey: &Survey) -> HashMap<String, (String, bool)> {
    extra
        .iter()
        .filter_map(|path| {
            let file = survey.state.aside.get(path)?;
            Some((path.clone(), (file.clone(), survey.is_free(file))))
        })
        .collect()
}

/// the line for `path`, the hidden name `sync --force` kept a person's file `file` under: one a run
/// cut off at once left, which sync moves back while `free`, or one that could not go back.
fn set_aside_row(path: &str, (file, free): &(String, bool), sync: &str) -> String {
    if *free {
        format!(
            "{path} · {file}, which a `sync --force` cut off before its end moved out of the way: {sync} moves it back"
        )
    } else {
        format!(
            "{path} · {file}, which `sync --force` moved out of the way and could not put back: move it or give it another name to keep it"
        )
    }
}

/// the files of `extra` named as sync names the changed copy it keeps of a file of `wanted` it put
/// back, `<file>.changed` or `<file>.changed.2` and on, each with that file.
fn kept_copies(extra: &[String], wanted: &HashSet<&str>) -> HashMap<String, String> {
    extra
        .iter()
        .filter_map(|path| {
            let at = path.rfind(".changed")?;
            let number = &path[at + ".changed".len()..];
            let numbered = number
                .strip_prefix('.')
                .and_then(|number| number.parse::<u32>().ok())
                .is_some_and(|number| number >= 2);
            let file = &path[..at];
            ((number.is_empty() || numbered) && wanted.contains(file))
                .then(|| (path.clone(), file.to_owned()))
        })
        .collect()
}

/// keeps, for the next status, the look of each output file this one hashed and found holding its
/// pinned bytes, so that one reads none of them again: written only while no other fetchloom holds
/// the project, and only for records still as this run read them, of files still as it saw them.
/// a project this account cannot write keeps nothing, and its next status hashes them again.
fn keep_verified(project: &Project, verified: &[(String, Look, Look)], started: u64) {
    if verified.is_empty() {
        return;
    }
    let Ok(crate::project::Hold::Held(_held)) = project.hold("status") else {
        return;
    };
    let (mut state, _) = project.state();
    let mut kept = false;
    for (path, recorded, seen) in verified {
        let now = std::fs::symlink_metadata(native(&project.root, path))
            .ok()
            .map(|metadata| Look::of(&metadata));
        if let Some(record) = state.files.get_mut(path)
            && record.look == *recorded
            && now == Some(*seen)
        {
            record.look = *seen;
            record.checked = started;
            kept = true;
        }
    }
    if kept && let Err(error) = project.write_state(&state) {
        tracing::debug!(message = %error.message, "the state could not keep what status verified");
    }
}

/// the files in the data folder that no dataset places, sorted, but for the staging leftovers of an
/// interrupted placement, which the next sync deletes, the files an operating system made, and the
/// files an output's patterns leave out of its folder.
fn extra(survey: &Survey, wanted: &HashSet<&str>, manifest: &Manifest) -> Vec<String> {
    let leftovers: HashSet<&String> = survey.leftovers().collect();
    let mut extra: Vec<String> = survey
        .disk
        .keys()
        .filter(|path| !wanted.contains(path.as_str()) && !leftovers.contains(path))
        .filter(|path| !survey.made_by_system(path))
        .filter(|path| !place::passed_over(manifest, path))
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
    fn lists(&self) -> [(&'static str, &Vec<String>, Option<String>); 7] {
        [
            ("Missing", &self.missing, None),
            ("Edited", &self.edited, None),
            (
                "Untracked",
                &self.untracked,
                Some("holds other bytes than data.lock pins".to_owned()),
            ),
            (
                "Unreadable",
                &self.unreadable,
                Some("this account cannot read it, so what it holds is unknown".to_owned()),
            ),
            ("Outdated", &self.outdated, None),
            ("Extra", &self.extra, None),
            (
                "Extra",
                &self.unneeded,
                Some(format!(
                    "fetchloom placed it and no dataset needs it now, so {} removes it",
                    self.a_sync()
                )),
            ),
        ]
    }

    /// a plain sync as a row names it: its command, or just "a sync" while one is refused, as the
    /// hints then say what comes first.
    fn a_sync(&self) -> String {
        if self.sync_refused {
            "a sync".to_owned()
        } else {
            format!("`{}`", super::project::command("sync"))
        }
    }

    /// the dataset `line` of `--check` names, whose own sync sets it right while a plain sync is
    /// refused; none where a plain sync runs.
    fn only_sync_of(&self, line: &ready::Unready) -> Option<String> {
        if !self.sync_refused {
            return None;
        }
        if let ready::Unready::Unpinned(name) = line {
            return Some(name.clone());
        }
        let path = line.path()?;
        // a blocker may stand where a folder on a file's way belongs
        self.dataset_of.get(path).cloned().or_else(|| {
            let folder = format!("{path}/");
            self.dataset_of
                .iter()
                .find(|(placed, _)| placed.starts_with(&folder))
                .map(|(_, dataset)| dataset.clone())
        })
    }

    fn needs_sync(&self) -> bool {
        !self.unneeded.is_empty() || self.places()
    }

    /// whether a plain `sync` sets every difference right, so nothing needs a person's look first,
    /// as in a fresh clone.
    fn sync_repairs_all(&self) -> bool {
        self.needs_sync()
            && !self.lockless
            && self.imports.is_empty()
            && self.unreadable.is_empty()
            && self.edited.is_empty()
            && self.untracked.is_empty()
            && self.extra.is_empty()
            && self.blocked.is_empty()
            && self
                .damaged
                .iter()
                .all(|path| !self.hashed_only.contains(path))
    }

    /// whether a file of `missing` is one a sync can bring, as no import this cache lacks is.
    fn fetchable_missing(&self) -> bool {
        let elsewhere: HashSet<&String> = self.imported_elsewhere.values().flatten().collect();
        self.missing.iter().any(|path| !elsewhere.contains(path))
    }

    /// whether a sync has more to do than remove what no dataset needs: pin, place or put back.
    fn places(&self) -> bool {
        !self.removed.is_empty()
            || !self.not_downloaded.is_empty()
            || self
                .unlocked
                .iter()
                .any(|name| !self.waiting.contains_key(name) && !self.imports.contains_key(name))
            || self.fetchable_missing()
            || !self.not_unpacked.is_empty()
            || !self.outdated.is_empty()
            || !self.untracked.is_empty()
            || self
                .damaged
                .iter()
                .any(|path| !self.hashed_only.contains(path))
    }
}

/// a faint line on stderr for each dataset dropped on this machine.
fn tell_dropped(output: &Output, status: &Status) {
    for name in &status.dropped {
        if let Some((folder, paths)) = status.retiring.get(name) {
            let left = match paths.len() {
                1 => "its file".to_owned(),
                count => format!("{} of its files", units::grouped(count as u64)),
            };
            output.report(
                "Dropped",
                &format!(
                    "{name} · a drop cut short left {left} in {folder}; `{}` finishes it",
                    super::project::command(&format!("drop {name}"))
                ),
            );
            continue;
        }
        if status.dropped_unfreed.contains(name) {
            output.faint_step(
                "Dropped",
                &format!(
                    "{name} · a drop cut short before it freed the cache; `{}` frees what the cache still holds of it",
                    super::project::command(&format!("drop {name}"))
                ),
            );
            continue;
        }
        // no source gives back an import's files, so only the folder mirror drop found them on can
        let back = if status.dropped_owned.contains(name) {
            format!(
                "`{}` brings it back from a folder mirror that holds it",
                super::project::command(&format!("sync {name} --mirror <folder>"))
            )
        } else {
            format!(
                "`{}` brings it back",
                super::project::command(&format!("sync {name}"))
            )
        };
        output.faint_step(
            "Dropped",
            &format!("{name} · freed on this machine; {back}"),
        );
    }
}

/// a faint line on stderr counting the files that hold the pinned bytes though fetchloom did not
/// place them, which are no difference.
fn tell_adoptable(output: &Output, status: &Status) {
    let (hold, them) = match status.adoptable.len() {
        0 => return,
        1 => ("holds", "it"),
        _ => ("hold", "them"),
    };
    output.faint_step(
        "In place",
        &format!(
            "{} already {hold} the pinned bytes; {} records {them} as placed",
            units::count(status.adoptable.len() as u64, "file", "files"),
            status.a_sync()
        ),
    );
}

/// a row for each dataset only its source's digests pin, saying which sync downloads it.
fn tell_not_downloaded(output: &Output, status: &Status) {
    for name in &status.not_downloaded {
        let by = status.pinned_by.get(name).copied().unwrap_or("digests");
        // MD5 alone checks the first download, which then records the SHA-256 the pin lacks
        let does = match by {
            "MD5" => "downloads it, checks its MD5 and records its SHA-256",
            "SHA-256" => "downloads and checks it",
            _ => {
                "downloads and checks it, and records the SHA-256 of each file pinned by MD5 alone"
            }
        };
        output.report(
            "To download",
            &format!(
                "{name} · pinned by its source's {by}, not downloaded yet; `{}` {does}",
                super::project::command(&format!("sync {name}"))
            ),
        );
    }
}

/// a row per dataset data.lock does not pin as data.toml has it now.
fn tell_unlocked(output: &Output, status: &Status) {
    let unpinned = if status.lockless {
        "data.lock does not exist"
    } else {
        "data.toml changed since data.lock was written"
    };
    for name in &status.unlocked {
        if let Some(waiting) = status.waiting.get(name) {
            output.report(
                "Waiting",
                &format!("{name} · its file list {} is not there", waiting.part),
            );
            continue;
        }
        let reason = match (status.renamed.get(name), status.imports.get(name)) {
            (Some(old), _) => format!(
                "data.lock pins it as {old}, which data.toml does not have, and {} keeps that pin under this name without asking the source",
                status.a_sync()
            ),
            (None, Some(true)) => {
                "imported from files, so a change to its entry in data.toml cannot pin other files"
                    .to_owned()
            }
            (None, Some(false)) => {
                "imported from files, and data.lock holds no pin of them".to_owned()
            }
            (None, None) => unpinned.to_owned(),
        };
        output.report("Unlocked", &format!("{name} · {reason}"));
    }
}

/// a row on stdout per difference, so a pipe or file gets rows and stderr the story; a script gets
/// every file, a terminal one row per folder gone whole and at most `SHOWN` per kind.
fn list(output: &Output, status: &Status) {
    let shown = output.answer_width().map(|_| SHOWN);
    tell_dropped(output, status);
    tell_adoptable(output, status);
    tell_unlocked(output, status);
    for name in &status.removed {
        output.report(
            "Removed",
            &format!(
                "{name} · not in data.toml, so {} takes it out of data.lock",
                status.a_sync()
            ),
        );
    }
    tell_not_downloaded(output, status);
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
            } else if let Some(said) = &said {
                rows.push((format!("{path} · {said}"), 1));
            } else if let Some(file) = status.set_aside.get(path) {
                rows.push((set_aside_row(path, file, &status.a_sync()), 1));
            } else if let Some(file) = status.kept_copies.get(path) {
                rows.push((
                    format!(
                        "{path} · a changed copy sync kept of {file}: compare the two, keep what you need, then delete it"
                    ),
                    1,
                ));
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
        let said = match status.unproven.get(archive) {
            Some(why) => format!("{archive} · {why}, so a sync downloads it"),
            None => format!(
                "{archive} · this machine has no list of the files it holds, so they are not checked"
            ),
        };
        output.report("Unchecked", &said);
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
    tell_yours(output, status);
}

/// with `-v`, a faint line on stderr for the files a person put in dataset folders, which
/// fetchloom keeps and never counts: one by its path, more by their number, each then on a line of
/// its own. a plain status says nothing of them.
fn tell_yours(output: &Output, status: &Status) {
    if !output.shows_details() {
        return;
    }
    match status.yours.as_slice() {
        [] => {}
        [one] => output.faint_step("Kept", &format!("{one} · yours")),
        many => {
            let count = units::count(many.len() as u64, "file", "files");
            output.faint_step("Kept", &format!("{count} of yours in dataset folders"));
            for path in many {
                output.detail(path);
            }
        }
    }
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
        + status.not_downloaded.len()
        + status.not_unpacked.len()
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
        for category in 0..9 {
            let mut status = Status::default();
            let paths = match category {
                0 => &mut status.removed,
                1 => &mut status.unlocked,
                2 => &mut status.missing,
                3 => &mut status.not_unpacked,
                4 => &mut status.outdated,
                5 => &mut status.untracked,
                6 => &mut status.unneeded,
                7 => &mut status.edited,
                _ => &mut status.extra,
            };
            paths.push("data/set/file.csv".to_owned());
            assert_eq!(status.needs_sync(), category < 7, "category {category}");
            // an untracked file is a person's own until they say otherwise
            assert_eq!(
                status.sync_repairs_all(),
                category < 7 && category != 5,
                "category {category}"
            );
            assert!(sum_up(&output, &status), "category {category}");
        }
        // a file that holds the pinned bytes is no difference, though sync records it
        let adoptable = Status {
            adoptable: vec!["data/set/file.csv".to_owned()],
            ..Status::default()
        };
        assert!(!adoptable.needs_sync());
        assert!(!sum_up(&output, &adoptable));
        let mut damaged = Status::default();
        damaged.damaged.push("data/set/file.csv".to_owned());
        assert!(damaged.needs_sync() && damaged.sync_repairs_all());
        damaged.hashed_only.insert("data/set/file.csv".to_owned());
        assert!(!damaged.needs_sync(), "only sync --verify finds it");
        assert!(!damaged.sync_repairs_all());
        assert!(sum_up(&output, &damaged));
        // a file of an import this cache lacks is none a sync can bring, while one beside it is
        let mut missing = Status {
            missing: vec!["data/raw/a.csv".to_owned(), "data/set/file.csv".to_owned()],
            ..Status::default()
        };
        missing
            .imported_elsewhere
            .insert("raw".to_owned(), vec!["data/raw/a.csv".to_owned()]);
        assert!(missing.needs_sync());
        missing.missing.pop();
        assert!(!missing.needs_sync());
        let mut unlocked = Status {
            unlocked: vec!["set".to_owned()],
            ..Status::default()
        };
        assert!(unlocked.sync_repairs_all());
        unlocked.lockless = true;
        assert!(!unlocked.sync_repairs_all(), "a sync would pin new bytes");
    }

    /// a file fetchloom did not place is a difference only where it means one: a changed copy sync
    /// kept and a file moved aside wherever they are, and any file in a folder whose extras count,
    /// the innermost folder deciding. the rest of a dataset's folder is the person's, and a file
    /// outside every dataset's folder is dropped.
    #[test]
    fn only_owned_folders_judge_the_files_fetchloom_did_not_place() {
        let owned = plan::Owned::from_folders(&[
            ("data/iris", false),
            ("data/iris/inner", true),
            ("data/out", true),
        ]);
        let paths = |paths: &[&str]| paths.iter().map(|path| (*path).to_owned()).collect();
        let mut status = Status {
            extra: paths(&[
                "data/iris/notes.txt",
                "data/iris/iris.csv.changed",
                "data/iris/inner/made.csv",
                "data/iris/innermost.csv",
                "data/iris2/notes.txt",
                "data/out/new.csv",
                "data/processed/clean.csv",
                "data/processed/.clean.csv.aside",
                "notes.txt",
            ]),
            ..Status::default()
        };
        status.kept_copies.insert(
            "data/iris/iris.csv.changed".to_owned(),
            "data/iris/iris.csv".to_owned(),
        );
        status.set_aside.insert(
            "data/processed/.clean.csv.aside".to_owned(),
            ("data/processed/clean.csv".to_owned(), true),
        );
        sort_extra(&mut status, &owned);
        assert_eq!(
            status.extra,
            paths(&[
                "data/iris/iris.csv.changed",
                "data/iris/inner/made.csv",
                "data/out/new.csv",
                "data/processed/.clean.csv.aside",
            ]) as Vec<String>
        );
        assert_eq!(
            status.yours,
            paths(&["data/iris/notes.txt", "data/iris/innermost.csv"]) as Vec<String>
        );
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

    /// status reads data.toml once: the warnings after its check use what the check read.
    #[test]
    fn status_reads_data_toml_once() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("data.toml"),
            "[datasets.iris]\nref = \"zenodo:1\"\n",
        )
        .unwrap();
        let project = Project::at(dir.path().to_path_buf());
        let context = crate::cli::project::tests::offline_context();
        let before = crate::project::manifest::tests::reads();
        brief(&context, &project).unwrap();
        assert_eq!(crate::project::manifest::tests::reads() - before, 1);
    }

    #[test]
    fn a_copy_an_interrupted_sync_left_behind_is_not_extra() {
        let dir = tempfile::tempdir().unwrap();
        let project = Project::at(dir.path().to_path_buf());
        let folder = project.root.join("data").join("weather");
        std::fs::create_dir_all(&folder).unwrap();
        for name in [
            "table.csv",
            ".fetchloom-0123456789abcdef.copy",
            ".fetchloom-notes.copy",
        ] {
            std::fs::write(folder.join(name), name).unwrap();
        }
        let survey = Survey::read(
            &project,
            &Folders::data("data"),
            Threads::default(),
            &Stop::default(),
        )
        .unwrap();

        assert_eq!(
            extra(&survey, &HashSet::new(), &Manifest::parse("").unwrap()),
            [
                "data/weather/.fetchloom-notes.copy",
                "data/weather/table.csv"
            ]
        );
    }
}
