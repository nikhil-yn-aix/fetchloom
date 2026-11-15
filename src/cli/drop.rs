//! `fetchloom drop`: frees this machine's copy of pinned datasets once each of their files is
//! found elsewhere, on a folder mirror whose copy hashes to its pin or at a web mirror or source
//! serving it at its size. their unchanged files leave the data folder and the cache copies nothing
//! else needs leave the cache. data.toml and data.lock keep them, and the state marks them dropped
//! here until `sync <name>` or `path <name>` brings one back.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

use serde::Serialize;

use super::args;
use super::probe::{self, Ask, Found, Lack, Pinned};
use super::project::{dry_run_done, find, hold, known};
use super::sync::Scope;
use super::sync::disk::Survey;
use super::sync::place::{self, Placement};
use super::{Context, peek_cache};
use crate::cache::{Cache, Failure};
use crate::digest::Sha256;
use crate::error::{Error, Kind};
use crate::mirror::folder::{self, Held};
use crate::mirror::{Location, Wanted};
use crate::place::Method;
use crate::project::lock::archives::{self, Pinned as Packed};
use crate::project::lock::{Lock, LockedFile};
use crate::project::manifest::Manifest;
use crate::units;

/// the most files a refusal names; `-v` and `--json` name them all.
const NAMED: usize = 20;

/// what `drop` freed, or with `--dry-run` would free, as `--json` prints it.
#[derive(Serialize)]
pub(super) struct Report {
    project: PathBuf,
    /// the datasets freed on this machine, or that would be.
    dropped: Vec<String>,
    /// the datasets named that were dropped already, which nothing changed.
    already: Vec<String>,
    /// the files removed from the data folder, and the cache files freed.
    files: u64,
    objects: u64,
    /// the bytes those took on this machine's disks.
    bytes: u64,
    placement: Option<Placement>,
    /// cache files whose removal failed, which stay.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    not_removed: Vec<Failure>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    dry_run: bool,
}

/// a dataset named to drop: its pinned files, owned here for a result, whose files data.lock
/// lists as members, and whether a mirror alone may stand in for them.
struct Named<'a> {
    name: &'a str,
    files: Vec<LockedFile>,
    /// whether data.toml asks for its link's web page.
    page: bool,
    /// a recipe's result, which only a build or a mirror brings back.
    result: bool,
}

pub(super) async fn run(context: &Context, args: args::Drop) -> Result<Report, Error> {
    let mut given: Vec<String> = Vec::with_capacity(args.names.len());
    for name in args.names {
        if !given.contains(&name) {
            given.push(name);
        }
    }
    let project = find(&context.output)?;
    let dry_run = args.dry.dry_run;
    let _held = if dry_run {
        None
    } else {
        Some(hold(context, &project, "drop").await?)
    };
    let (_, manifest) = project.manifest()?;
    for name in &given {
        known(&manifest, name, "drop")?;
    }
    let lock = project.lock()?.unwrap_or_default();
    let marked = project.dropped();
    let (already, asked): (Vec<String>, Vec<String>) =
        given.into_iter().partition(|name| marked.contains(name));
    for name in &already {
        context.output.faint_step(
            "Dropped",
            &format!("{name} · dropped on this machine already"),
        );
    }
    let named = asked
        .iter()
        .map(|name| pinned_files(&manifest, &lock, name))
        .collect::<Result<Vec<_>, _>>()?;
    let lacking = {
        let what = units::count(
            named.iter().map(|named| named.files.len() as u64).sum(),
            "file",
            "files",
        );
        let _waiting = context
            .output
            .wait("Checking", format!("{what} are found elsewhere"));
        unfound(context, &named).await?
    };
    let freeing: BTreeSet<String> = asked
        .iter()
        .filter(|name| !lacking.contains_key(*name))
        .cloned()
        .collect();
    let mut report = Report {
        project: project.root.clone(),
        dropped: asked
            .iter()
            .filter(|name| freeing.contains(*name))
            .cloned()
            .collect(),
        already,
        files: 0,
        objects: 0,
        bytes: 0,
        placement: None,
        not_removed: Vec::new(),
        dry_run,
    };
    if !freeing.is_empty() {
        if dry_run {
            preview(context, &project, &manifest, &lock, &freeing, &mut report)?;
        } else {
            free(context, &project, &manifest, &lock, &freeing, &mut report)?;
        }
    }
    if let Some(error) = refused(&lacking, &asked, context.offline) {
        return Err(error.with_report(&report));
    }
    if dry_run {
        dry_run_done(&context.output, None);
    } else if !report.dropped.is_empty() {
        let them = if report.dropped.len() == 1 {
            "it"
        } else {
            "them"
        };
        context.output.next(&format!(
            "`{}` brings {them} back",
            super::project::command(&format!("sync {}", report.dropped.join(" ")))
        ));
    }
    Ok(report)
}

/// the files of `name` to find elsewhere before it is dropped: those data.lock pins for it, an
/// archive as itself, or for a recipe the files of its pinned result.
fn pinned_files<'a>(
    manifest: &'a Manifest,
    lock: &Lock,
    name: &'a str,
) -> Result<Named<'a>, Error> {
    let entry = &manifest.datasets[name];
    let unpinned = || {
        Error::new(
            Kind::Usage,
            format!("{name} is not pinned yet, so nothing of it is on this machine to drop"),
        )
        .help(format!(
            "`{}` pins and places it",
            super::project::command(&format!("sync {name}"))
        ))
    };
    if entry.recipe().is_some() {
        let result = lock.result(name).ok_or_else(unpinned)?;
        let files = result
            .files
            .iter()
            .map(|member| LockedFile {
                path: member.path.clone(),
                size: member.size,
                sha256: member.sha256,
                md5: None,
                url: String::new(),
                unpacks_to: None,
            })
            .collect();
        return Ok(Named {
            name,
            files,
            page: false,
            result: true,
        });
    }
    let locked = lock.get(name).ok_or_else(|| match lock.stated(name) {
        Some(stated) => Error::new(
            Kind::Usage,
            format!(
                "{name} is pinned by its source's {} but not downloaded yet, so nothing of it is on this machine to drop",
                stated.digests()
            ),
        ),
        None => unpinned(),
    })?;
    Ok(Named {
        name,
        files: locked.files.clone(),
        page: entry.source().is_some_and(|source| source.web_page),
        result: false,
    })
}

/// why each file of `named` is found nowhere, as lines by dataset; a dataset with none may go. a
/// folder mirror's copy counts once it hashes to its pin, and with the network a web mirror or the
/// source serving the file at its size, never a source for a result or an import.
async fn unfound(
    context: &Context,
    named: &[Named<'_>],
) -> Result<BTreeMap<String, Vec<String>>, Error> {
    let locked: Vec<(&Named<'_>, &LockedFile)> = named
        .iter()
        .flat_map(|named| named.files.iter().map(move |file| (named, file)))
        .collect();
    let pinned: Vec<Pinned<'_>> = locked
        .iter()
        .map(|(named, file)| Pinned::locked(named.name, file, named.page))
        .collect();
    let results: BTreeSet<&str> = named
        .iter()
        .filter(|named| named.result)
        .map(|named| named.name)
        .collect();
    let net = Arc::new(context.net());
    let mut found = vec![false; pinned.len()];
    for index in context.mirrors.up(context, &net).await? {
        let Location::Folder(root) = &context.mirrors.mirrors.list[index] else {
            continue;
        };
        let asked: Vec<usize> = (0..pinned.len()).filter(|at| !found[*at]).collect();
        if asked.is_empty() {
            break;
        }
        let wanted: Vec<Wanted> = asked
            .iter()
            .map(|at| Wanted {
                digest: locked[*at].1.sha256,
                size: locked[*at].1.size,
                label: pinned[*at].label(),
            })
            .collect();
        let held = tokio::task::block_in_place(|| {
            folder::check_all(root, &wanted, context.threads, &|_| {}, &context.stop)
        })
        .map_err(|err| {
            if err.kind() == std::io::ErrorKind::Interrupted {
                crate::stop::Stop::stopped()
            } else {
                Error::io(
                    format_args!("could not read the mirror {}", root.display()),
                    &err,
                )
            }
        })?;
        for (at, held) in asked.into_iter().zip(held) {
            found[at] |= matches!(held, Held::Verified(_));
        }
    }
    let mut lacking: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut lack = |pinned: &Pinned<'_>, why: String| {
        lacking
            .entry(pinned.dataset.to_owned())
            .or_default()
            .push(format!("{} · {why}", pinned.label()));
    };
    let rest: Vec<Pinned<'_>> = pinned
        .iter()
        .zip(&found)
        .filter(|(_, found)| !**found)
        .map(|(pinned, _)| *pinned)
        .collect();
    if context.offline.enabled() {
        for pinned in &rest {
            lack(
                pinned,
                "offline, and no folder mirror holds a copy that hashes to its pin".to_owned(),
            );
        }
        return Ok(lacking);
    }
    let answers = probe::find(context, &net, &rest, Ask::Elsewhere).await?;
    for (pinned, answer) in rest.iter().zip(answers) {
        let Found::Nowhere(why) = answer else {
            continue;
        };
        let said = match why {
            Lack::Imported if results.contains(pinned.dataset) => {
                "no mirror holds this file of a recipe's result".to_owned()
            }
            Lack::Imported => "it was imported from files, and no mirror holds it".to_owned(),
            why => format!("{}: {}", why.outcome(), why.error(pinned.size).message),
        };
        lack(pinned, said);
    }
    Ok(lacking)
}

/// the error naming each dataset of `asked` that `lacking` keeps, with each file found nowhere,
/// none when every one may go. `offline`, only a folder mirror was asked.
fn refused(
    lacking: &BTreeMap<String, Vec<String>>,
    asked: &[String],
    offline: crate::net::Offline,
) -> Option<Error> {
    let kept: Vec<&str> = asked
        .iter()
        .map(String::as_str)
        .filter(|name| lacking.contains_key(*name))
        .collect();
    let first = kept.first()?;
    let lines: Vec<&String> = kept.iter().flat_map(|name| &lacking[*name]).collect();
    let (said, them) = match kept.as_slice() {
        [_] => (format!("{first} stays"), "its"),
        many => (format!("{} stay", units::listed(many, "and")), "their"),
    };
    let it = if kept.len() == 1 { "it" } else { "them" };
    let error = if offline.enabled() {
        Error::new(
            Kind::OfflineMissing,
            format!(
                "{said} on this machine, since offline only a folder mirror is asked, and none has {} of {them} files",
                units::grouped(lines.len() as u64)
            ),
        )
        .help(format!(
            "{} to ask the web mirrors and sources too",
            offline.retry()
        ))
    } else {
        Error::new(
            Kind::ReferenceUnresolved,
            format!(
                "{said} on this machine, since no mirror or source has {} of {them} files",
                units::grouped(lines.len() as u64)
            ),
        )
        .help(format!(
            "`{}` keeps a copy of each on a mirror first, then drop {it}",
            super::project::command(&format!("push {}", kept.join(" ")))
        ))
    };
    Some(error.details_up_to(&lines, NAMED))
}

/// `manifest` as a sync of every dataset reads it while `dropped` are dropped here: each with
/// `place = false`, so the run neither fetches nor places it.
pub(super) fn left_out(manifest: &mut Manifest, dropped: &BTreeSet<String>) {
    for name in dropped {
        if let Some(entry) = manifest.datasets.get_mut(name) {
            entry.place = false;
        }
    }
}

/// `manifest` as a run of `scope` places it while `dropped` are dropped here: a run of every
/// dataset leaves them out, and a run that names one brings it back.
pub(super) fn placed_by<'m>(
    manifest: &'m Manifest,
    scope: &Scope,
    dropped: &BTreeSet<String>,
) -> std::borrow::Cow<'m, Manifest> {
    if *scope != Scope::All || dropped.is_empty() {
        return std::borrow::Cow::Borrowed(manifest);
    }
    let mut placed = manifest.clone();
    left_out(&mut placed, dropped);
    std::borrow::Cow::Owned(placed)
}

/// whether `scope` names a dataset `dropped` here, which its run brings back.
pub(super) fn names_any(scope: &Scope, dropped: &BTreeSet<String>) -> bool {
    match scope {
        Scope::All => false,
        Scope::Only(names) => names.iter().any(|name| dropped.contains(name)),
    }
}

/// takes the datasets `scope` names off `survey`'s list of those dropped here, so its run places
/// them and writes the state without them. the run then looks at every file it places, since the
/// last sync's word that all was in place left them out.
pub(super) fn brought_back(survey: &mut Survey, scope: &Scope) {
    let Scope::Only(names) = scope else {
        return;
    };
    let before = survey.state.dropped.len();
    survey.state.dropped.retain(|name| !names.contains(name));
    if survey.state.dropped.len() != before {
        survey.state.synced = None;
        survey.needs_writing = true;
    }
}

/// `lock` without the datasets `dropped` here, as the cache's needs read this project.
pub(super) fn kept_here(mut lock: Lock, dropped: &BTreeSet<String>) -> Lock {
    if dropped.is_empty() {
        return lock;
    }
    lock.datasets
        .retain(|locked| !dropped.contains(&locked.name));
    lock.derived
        .retain(|result| !dropped.contains(&result.name));
    lock
}

/// tells, a line each, the datasets of `manifest` dropped here that a run of `scope` leaves out.
pub(super) fn tell_left_out(
    output: &crate::output::Output,
    manifest: &Manifest,
    scope: &Scope,
    dropped: &BTreeSet<String>,
) {
    if *scope != Scope::All {
        return;
    }
    for name in dropped
        .iter()
        .filter(|name| manifest.datasets.contains_key(*name))
    {
        output.faint_step(
            "Dropped",
            &format!(
                "{name} · left out on this machine; `{}` brings it back",
                super::project::command(&format!("sync {name}"))
            ),
        );
    }
}

/// what `drop` of `freeing` would remove, told and counted without changing anything.
fn preview(
    context: &Context,
    project: &crate::project::Project,
    manifest: &Manifest,
    lock: &Lock,
    freeing: &BTreeSet<String>,
    report: &mut Report,
) -> Result<(), Error> {
    let cache = peek_cache()?;
    let survey = Survey::read(project, &manifest.data_dir, context.threads, &context.stop)?;
    let copies = copies(&survey, freeing);
    let (without, unpinned) = without(manifest, lock, freeing);
    let scope = Scope::Only(freeing.clone());
    let placing = place::Placing::new(project, &without, &unpinned, &cache, false, &scope, survey)?;
    let preview = placing.preview_removal();
    let objects = held(&cache, lock, freeing);
    let current = kept_here(
        lock.clone(),
        &project.dropped().union(freeing).cloned().collect(),
    );
    let needed = super::cache::needed_besides(
        &cache,
        (&project.root, &current),
        &objects.archives,
        context.threads,
        &context.stop,
    )?;
    let freed: Vec<(&Sha256, &u64)> = objects
        .sizes
        .iter()
        .filter(|(digest, _)| {
            !needed.objects.contains(*digest) && cache.find(digest, None).is_some()
        })
        .collect();
    report.files = preview.changes.remove.len() as u64;
    report.objects = freed.len() as u64;
    let removed: u64 = preview
        .changes
        .remove
        .iter()
        .filter_map(|path| copies.get(path))
        .sum();
    report.bytes = freed.iter().map(|(_, size)| **size).sum::<u64>() + removed;
    context.output.step(
        "Would drop",
        &format!(
            "{} · free {}: {} from {}/ and {} from the cache",
            units::listed(&report.dropped, "and"),
            units::size(report.bytes),
            units::count(report.files, "file", "files"),
            manifest.data_dir,
            units::count(report.objects, "file", "files")
        ),
    );
    place::tell_preview(&context.output, &without, &scope, &preview);
    report.placement = Some(preview.placement);
    Ok(())
}

/// drops `freeing`: removes their unchanged files from the data folder, marks them dropped in the
/// state, then frees the cache files nothing else needs, under the cache's own lock, as gc does.
fn free(
    context: &Context,
    project: &crate::project::Project,
    manifest: &Manifest,
    lock: &Lock,
    freeing: &BTreeSet<String>,
    report: &mut Report,
) -> Result<(), Error> {
    let cache = super::cache::hold(context)?;
    let survey = Survey::read(project, &manifest.data_dir, context.threads, &context.stop)?;
    let copies = copies(&survey, freeing);
    let (without, unpinned) = without(manifest, lock, freeing);
    let scope = Scope::Only(freeing.clone());
    let placing = place::Placing::new(project, &without, &unpinned, &cache, false, &scope, survey)?;
    // the last look for a stop: once files go, the drop runs to its end
    context.stop.check()?;
    context.stop.finish();
    let placement = placing.remove_only(&context.output)?;
    // noted once its files are gone, before any cache file goes, so a drop cut short between
    // the two leaves what a later gc frees
    let (mut state, _) = project.state();
    state.dropped.extend(freeing.iter().cloned());
    project.write_state(&state)?;
    let removed: u64 = copies
        .iter()
        .filter(|(path, _)| !project.root.join(path).exists())
        .map(|(_, size)| size)
        .sum();
    let objects = held(&cache, lock, freeing);
    let current = kept_here(lock.clone(), &project.dropped());
    let needed = super::cache::needed_besides(
        &cache,
        (&project.root, &current),
        &objects.archives,
        context.threads,
        &crate::stop::Stop::default(),
    )?;
    let mut digests: Vec<(&Sha256, &u64)> = objects.sizes.iter().collect();
    digests.sort();
    let (mut freed, mut bytes) = (0, 0);
    for (digest, size) in digests {
        if !needed.objects.contains(digest) && cache.free_input(digest, &mut report.not_removed) {
            freed += 1;
            bytes += size;
        }
    }
    report.files = placement.removed;
    report.objects = freed;
    report.bytes = bytes + removed;
    report.placement = Some(placement);
    context.output.done(
        "Dropped",
        &format!(
            "{} · freed {}: {} from {}/ and {} from the cache",
            units::listed(&report.dropped, "and"),
            units::size(report.bytes),
            units::count(report.files, "file", "files"),
            manifest.data_dir,
            units::count(report.objects, "file", "files")
        ),
    );
    Ok(())
}

/// `manifest` and `lock` without the datasets `freeing`, so their placed files are ones no
/// dataset needs, which placing removes when unchanged.
fn without(manifest: &Manifest, lock: &Lock, freeing: &BTreeSet<String>) -> (Manifest, Lock) {
    let mut without = manifest.clone();
    without.datasets.retain(|name, _| !freeing.contains(name));
    (without, kept_here(lock.clone(), freeing))
}

/// the files of `freeing` that `survey` records as copies, by path, with their sizes: their
/// bytes are their own, unlike a link's, which the cache's copy holds.
fn copies(survey: &Survey, freeing: &BTreeSet<String>) -> HashMap<String, u64> {
    survey
        .state
        .files
        .iter()
        .filter(|(_, placed)| freeing.contains(&placed.dataset) && placed.method == Method::Copy)
        .map(|(path, placed)| (path.clone(), placed.look.size))
        .collect()
}

/// the cache files some datasets may hold, each with its size, and the archives among them.
struct Objects {
    sizes: HashMap<Sha256, u64>,
    archives: Vec<Sha256>,
}

/// the cache files the datasets `freeing` of `lock` may hold: their pinned files, the files their
/// archives unpacked to, and a result's files.
fn held(cache: &Cache, lock: &Lock, freeing: &BTreeSet<String>) -> Objects {
    let mut held = Objects {
        sizes: HashMap::new(),
        archives: Vec::new(),
    };
    for locked in lock
        .datasets
        .iter()
        .filter(|locked| freeing.contains(&locked.name))
    {
        for pinned in archives::pinned(&locked.files) {
            match pinned {
                Packed::File(file) => {
                    held.sizes.insert(file.sha256, file.size);
                }
                Packed::Archive(archive) => {
                    for file in archive.files() {
                        held.sizes.insert(file.sha256, file.size);
                        held.archives.push(file.sha256);
                    }
                    for member in archive.recorded(cache).unwrap_or_default() {
                        held.sizes.insert(member.sha256, member.size);
                    }
                }
            }
        }
    }
    for result in lock
        .derived
        .iter()
        .filter(|result| freeing.contains(&result.name))
    {
        for member in &result.files {
            held.sizes.insert(member.sha256, member.size);
        }
    }
    held
}
