//! `fetchloom drop`: frees this machine's copy of pinned datasets once each of their files is
//! found elsewhere, on a folder mirror whose copy hashes to its pin, or with `--yes` at a web or
//! bucket mirror or source serving it at its size. their unchanged files leave the data folder and
//! the cache copies nothing else needs leave the cache. data.toml and data.lock keep them, and the
//! state marks them dropped here until `sync <name>` or `path <name>` brings one back.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;

use super::args;
use super::probe::{self, Ask, Found, Lack, Pinned};
use super::project::{dry_run_done, find, hold, known};
use super::sync::Scope;
use super::sync::disk::{Folders, Survey};
use super::sync::place::{self, Placement};
use super::{Context, peek_cache};
use crate::cache::{Cache, Failure};
use crate::digest::Sha256;
use crate::error::{Error, Kind};
use crate::mirror::folder::{self, Held};
use crate::mirror::{Location, Wanted};
use crate::output::Warning;
use crate::place::Method;
use crate::project::lock::archives::{self, Pinned as Packed};
use crate::project::lock::{Lock, LockedFile};
use crate::project::manifest::Manifest;
use crate::project::native;
use crate::project::state::State;
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
    /// the datasets named of which nothing is on this machine, which nothing changed.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    absent: Vec<String>,
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

/// a dataset named to drop: its pinned files, and whether a mirror alone may stand in for them.
struct Named<'a> {
    name: &'a str,
    files: Vec<LockedFile>,
    /// whether data.toml asks for its link's web page.
    page: bool,
    /// the version data.lock pins, as `zenodo:3608990`, and data.toml's patterns, which its
    /// source lists again to say what checksums it states.
    resolved: String,
    select: Vec<crate::glob::Glob>,
}

impl Named<'_> {
    /// whether no source gives its files back: files imported from a folder.
    fn owned(&self) -> bool {
        self.files.iter().any(|file| file.url.is_empty())
    }
}

/// what the check found of the files of the datasets named to drop.
#[derive(Default)]
struct Checked {
    /// by dataset, the files that do not count as found elsewhere.
    lacks: BTreeMap<String, Lacks>,
    /// by dataset, the folder mirrors whose copies hashed to their pins.
    folders: BTreeMap<String, BTreeSet<String>>,
}

/// the files of one dataset that do not count as found elsewhere, by why.
#[derive(Default)]
struct Lacks {
    /// those no mirror or source has, as lines.
    lost: Vec<String>,
    /// those no clear answer came for, as lines, each with the error a download of it would meet.
    unchecked: Vec<(String, Error)>,
    /// how many a web or bucket mirror or source serves at their size alone, which does not show
    /// their bytes, while their source states no checksum data.lock records for them.
    sized: u64,
    /// how many their source serves without saying their size, which shows still less of them.
    no_size: u64,
}

impl Checked {
    /// whether `name` stays: a file of it is lost or could not be checked, or, without `yes`, one
    /// was checked by its size alone or served without a size.
    fn keeps(&self, name: &str, yes: bool) -> bool {
        self.lacks.get(name).is_some_and(|lacks| {
            !lacks.lost.is_empty()
                || !lacks.unchecked.is_empty()
                || (!yes && lacks.sized + lacks.no_size > 0)
        })
    }

    fn lacks(&mut self, name: &str) -> &mut Lacks {
        self.lacks.entry(name.to_owned()).or_default()
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "every name is checked before anything is dropped"
)]
pub(super) async fn run(context: &Context, args: args::Drop) -> Result<Report, Error> {
    let mut given: Vec<String> = Vec::with_capacity(args.names.len());
    for name in args.names {
        if !given.contains(&name) {
            given.push(name);
        }
    }
    let project = find(context)?;
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
    lock.refuse_waiting(given.iter().map(String::as_str))?;
    let marked = project.dropped();
    let (state, _) = project.state();
    let cache = peek_cache().ok();
    let planned = cache
        .as_ref()
        .and_then(|cache| crate::plan::from_cache(&manifest, &lock, cache).ok());
    let (mut asked, mut already, mut absent) = (Vec::new(), Vec::new(), Vec::new());
    for name in given {
        // a dataset dropped before whose bytes came back, as a push brings them, goes again
        if here(&project.root, &state, cache.as_ref(), &lock, &name)
            || in_folder(&project.root, planned.as_ref(), &name)
        {
            asked.push(name);
        } else if marked.contains(&name) {
            context.output.faint_step(
                "Dropped",
                &format!("{name} · dropped on this machine already"),
            );
            already.push(name);
        } else {
            context.output.faint_step(
                "Skipped",
                &format!(
                    "{name} · {}, so there is nothing to drop",
                    away(&lock, &name)
                ),
            );
            absent.push(name);
        }
    }
    let named = asked
        .iter()
        .map(|name| pinned_files(&manifest, &lock, name))
        .collect::<Result<Vec<_>, _>>()?;
    let checked = {
        let what = units::count(
            named.iter().map(|named| named.files.len() as u64).sum(),
            "file",
            "files",
        );
        let _waiting = context
            .output
            .wait("Checking", format!("where {what} are found"));
        unfound(context, &named).await?
    };
    let freeing: BTreeSet<String> = asked
        .iter()
        .filter(|name| !checked.keeps(name, context.yes))
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
        absent,
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
    if let Some(error) = refused(&checked, &asked, context.yes) {
        return Err(error.with_report(&report));
    }
    if dry_run {
        dry_run_done(&context.output, None);
    } else {
        tell_back(&context.output, &named, &checked, &report.dropped);
    }
    Ok(report)
}

/// whether this machine holds any file of `name`: one fetchloom placed for it in the data folder
/// under `root`, as `state` records, or one of its files in `cache`.
fn here(root: &Path, state: &State, cache: Option<&Cache>, lock: &Lock, name: &str) -> bool {
    let placed = state
        .files
        .iter()
        .any(|(path, placed)| placed.dataset == name && native(root, path).exists());
    placed
        || cache.is_some_and(|cache| {
            let named = BTreeSet::from([name.to_owned()]);
            held(cache, lock, &named)
                .sizes
                .keys()
                .any(|digest| cache.find(digest, None).is_some())
        })
}

/// whether the data folder under `root` holds the pinned bytes of a file of `name` at its place in
/// `planned`, as a copy of the project does while its state records none of them.
fn in_folder(root: &Path, planned: Option<&crate::plan::Plan<'_>>, name: &str) -> bool {
    planned.is_some_and(|planned| {
        planned
            .targets
            .iter()
            .filter(|target| target.dataset == name)
            .any(|target| {
                let file = native(root, &target.path);
                std::fs::symlink_metadata(&file).is_ok_and(|found| {
                    found.is_file()
                        && place::holds_target(
                            target,
                            &crate::project::state::Look::of(&found),
                            &file,
                        )
                })
            })
    })
}

/// why nothing of `name` is on this machine, as drop tells it.
fn away(lock: &Lock, name: &str) -> &'static str {
    if lock.stated(name).is_some() {
        "not downloaded yet"
    } else if lock.get(name).is_none() {
        "not pinned yet"
    } else {
        "not on this machine"
    }
}

/// a line saying how each of `dropped` comes back: a sync from its source, or an import's from the
/// folder mirror whose copy stood in for it.
fn tell_back(
    output: &crate::output::Output,
    named: &[Named<'_>],
    checked: &Checked,
    dropped: &[String],
) {
    let mut sourced: Vec<&str> = Vec::new();
    for named in named
        .iter()
        .filter(|named| dropped.iter().any(|name| name == named.name))
    {
        let folder = checked
            .folders
            .get(named.name)
            .and_then(|folders| folders.first());
        match folder.filter(|_| named.owned()) {
            Some(folder) => output.next(&format!(
                "`{}` brings it back from the folder mirror that holds it",
                super::project::command(&format!(
                    "sync {} --mirror {}",
                    named.name,
                    crate::suggested::shell_word(folder)
                ))
            )),
            None => sourced.push(named.name),
        }
    }
    if !sourced.is_empty() {
        let them = if sourced.len() == 1 { "it" } else { "them" };
        output.next(&format!(
            "`{}` brings {them} back",
            super::project::command(&format!("sync {}", sourced.join(" ")))
        ));
    }
}

/// the files of `name` to find elsewhere before it is dropped: those data.lock pins for it, an
/// archive as itself.
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
        page: entry.source.web_page,
        resolved: locked.resolved.clone(),
        select: entry.source.select.clone(),
    })
}

/// where each file of `named` is found elsewhere. a folder mirror's copy counts once it hashes to
/// its pin; with the network, a web or bucket mirror or the source serving the file at its size
/// counts by its size alone, but never for an import's file, which no source gives
/// back.
async fn unfound(context: &Context, named: &[Named<'_>]) -> Result<Checked, Error> {
    let locked: Vec<(&Named<'_>, &LockedFile)> = named
        .iter()
        .flat_map(|named| named.files.iter().map(move |file| (named, file)))
        .collect();
    let pinned: Vec<Pinned<'_>> = locked
        .iter()
        .map(|(named, file)| Pinned::locked(named.name, file, named.page))
        .collect();
    let net = Arc::new(context.net());
    let mut checked = Checked::default();
    let found = on_folders(context, &net, &locked, &pinned, &mut checked).await?;
    let (mut rest, mut rest_at): (Vec<Pinned<'_>>, Vec<usize>) = (Vec::new(), Vec::new());
    for (at, pinned) in pinned.iter().enumerate().filter(|(at, _)| !found[*at]) {
        let lacks = checked.lacks(pinned.dataset);
        // a size from a web or bucket mirror is too little for bytes no source gives back
        if locked[at].1.url.is_empty() {
            lacks.lost.push(format!(
                "{} · it was imported from files, and no folder mirror holds a copy that hashes to its pin",
                pinned.label()
            ));
        } else if context.offline.enabled() {
            let error = Error::new(
                Kind::OfflineMissing,
                "offline, and no folder mirror holds a copy that hashes to its pin",
            )
            .help(format!(
                "{} to ask the web and S3 mirrors and the sources too",
                context.offline.retry()
            ));
            lacks
                .unchecked
                .push((format!("{} · {}", pinned.label(), error.message), error));
        } else {
            rest.push(*pinned);
            rest_at.push(at);
        }
    }
    if rest.is_empty() {
        return Ok(checked);
    }
    let answers = probe::find(context, &net, &rest, Ask::Elsewhere).await?;
    // the files served at their size alone or with none, by their place in `locked`, each with
    // whether it came with none
    let mut by_size: Vec<(usize, bool)> = Vec::new();
    for ((pinned, answer), &at) in rest.iter().zip(answers).zip(&rest_at) {
        let lacks = checked.lacks(pinned.dataset);
        let why = match answer {
            Found::Mirror { .. } | Found::Source { .. } => {
                lacks.sized += 1;
                by_size.push((at, false));
                continue;
            }
            // its source answered with the file, and asking again gets that answer again
            Found::Nowhere(Lack::Unsized(_)) => {
                lacks.no_size += 1;
                by_size.push((at, true));
                continue;
            }
            Found::Nowhere(why) => why,
        };
        let (lost, outcome) = (why.lost(), why.outcome());
        let error = why.error(pinned.size);
        let said = format!("{} · {outcome}: {}", pinned.label(), error.message);
        if lost {
            lacks.lost.push(said);
        } else {
            lacks.unchecked.push((said, error));
        }
    }
    if !context.yes {
        stated(&net, &locked, &by_size, &mut checked).await?;
    }
    Ok(checked)
}

/// takes off the counts in `checked` each file of `by_size`, by its place in `locked`, whose
/// source states at the version data.lock pins a checksum data.lock records for it: its SHA-256,
/// or an MD5 data.lock keeps, as zenodo states one. each dataset's source lists it once; one that
/// does not answer leaves its files counted by their size. fails at a stop.
async fn stated(
    net: &Arc<crate::net::Net>,
    locked: &[(&Named<'_>, &LockedFile)],
    by_size: &[(usize, bool)],
    checked: &mut Checked,
) -> Result<(), Error> {
    let mut asked: Vec<&Named<'_>> = Vec::new();
    for &(at, _) in by_size {
        let named = locked[at].0;
        if !asked.iter().any(|seen| seen.name == named.name) {
            asked.push(named);
        }
    }
    let mut tasks = tokio::task::JoinSet::new();
    for (index, named) in asked.iter().enumerate() {
        let (net, resolved, select) = (
            Arc::clone(net),
            named.resolved.clone(),
            named.select.clone(),
        );
        tasks.spawn(async move {
            let listing = match crate::source::Reference::parse(&resolved) {
                Ok(parsed) => parsed.reference.resolve_in(&*net, &select).await,
                Err(error) => Err(error),
            };
            (index, listing)
        });
    }
    let mut listings: Vec<Option<crate::source::Listing>> = asked.iter().map(|_| None).collect();
    while let Some(joined) = tasks.join_next().await {
        let Ok((index, listing)) = joined else {
            continue;
        };
        match listing {
            Ok(listing) => listings[index] = Some(listing),
            Err(error) if error.kind == Kind::Interrupted => {
                tasks.abort_all();
                return Err(error);
            }
            Err(error) => tracing::debug!(%error, "a source did not list a dataset to drop"),
        }
    }
    for &(at, without_size) in by_size {
        let (named, file) = locked[at];
        let Some(listing) = asked
            .iter()
            .position(|seen| seen.name == named.name)
            .and_then(|index| listings[index].as_ref())
        else {
            continue;
        };
        let states = listing.files.iter().any(|remote| {
            remote.url.as_str() == file.url
                && (remote.sha256 == Some(file.sha256)
                    || (file.md5.is_some() && remote.md5 == file.md5))
        });
        if states {
            let lacks = checked.lacks(named.name);
            if without_size {
                lacks.no_size = lacks.no_size.saturating_sub(1);
            } else {
                lacks.sized = lacks.sized.saturating_sub(1);
            }
        }
    }
    Ok(())
}

/// which of `pinned` a folder mirror holds a copy of that hashes to its pin, noting in `checked`
/// the folders that do. fails at a stop.
async fn on_folders(
    context: &Context,
    net: &crate::net::Net,
    locked: &[(&Named<'_>, &LockedFile)],
    pinned: &[Pinned<'_>],
    checked: &mut Checked,
) -> Result<Vec<bool>, Error> {
    let cache = peek_cache()?;
    let mut found = vec![false; pinned.len()];
    for index in context.mirrors.up(context, net).await? {
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
                link: None,
            })
            .collect();
        let held = tokio::task::block_in_place(|| {
            folder::check_all(root, &wanted, context.threads, &|_| {}, &context.stop)
        })
        .map_err(|err| unread(root, &err))?;
        for (at, held) in asked.into_iter().zip(held) {
            let digest = locked[at].1.sha256;
            // the cache's own file, reached through a link, is no second copy
            if let Held::Verified(seen) = held
                && !folder::is_cache_copy(&seen, &digest, &cache)
            {
                found[at] = true;
                let folders = checked.folders.entry(pinned[at].dataset.to_owned());
                folders.or_default().insert(root.display().to_string());
                context
                    .mirrors
                    .hashed(index, digest, seen, pinned[at].label());
                continue;
            }
            // a push keeps an archive as what it unpacks to, which a sync places from
            let Some(tree) = locked[at].1.unpacks_to else {
                continue;
            };
            let unpacked = tokio::task::block_in_place(|| {
                unpacked_on(root, &tree, &cache, pinned[at].dataset, context)
            })
            .map_err(|err| unread(root, &err))?;
            if let Some(copies) = unpacked {
                found[at] = true;
                let folders = checked.folders.entry(pinned[at].dataset.to_owned());
                folders.or_default().insert(root.display().to_string());
                for (digest, seen, label) in copies {
                    context.mirrors.hashed(index, digest, seen, label);
                }
            }
        }
    }
    Ok(found)
}

/// the error for a read of the folder mirror at `root` that failed, or a stop.
fn unread(root: &Path, err: &std::io::Error) -> Error {
    if err.kind() == std::io::ErrorKind::Interrupted {
        crate::stop::Stop::stopped()
    } else {
        Error::io(
            format_args!("could not read the mirror {}", root.display()),
            err,
        )
    }
}

/// the copies on the folder mirror at `root` of every file of `dataset` that the archive whose
/// tree is `tree` unpacks to, each with how it looked when it hashed to its pin and its name, as a
/// push keeps an archive: none unless the mirror holds that tree and each file, none of them the
/// cache's own. fails at a stop.
fn unpacked_on(
    root: &Path,
    tree: &Sha256,
    cache: &Cache,
    dataset: &str,
    context: &Context,
) -> std::io::Result<Option<Vec<(Sha256, folder::Seen, String)>>> {
    let folder::Tree::Bytes(bytes) = folder::read_tree(root, tree, &context.stop)? else {
        return Ok(None);
    };
    let Some(members) = crate::cache::members_of(tree, &bytes) else {
        return Ok(None);
    };
    let wanted: Vec<Wanted> = members
        .iter()
        .map(|member| Wanted {
            digest: member.sha256,
            size: member.size,
            label: format!("{dataset}/{}", member.path),
            link: None,
        })
        .collect();
    let held = folder::check_all(root, &wanted, context.threads, &|_| {}, &context.stop)?;
    let mut copies = Vec::with_capacity(wanted.len());
    for (want, held) in wanted.into_iter().zip(held) {
        match held {
            Held::Verified(seen) if !folder::is_cache_copy(&seen, &want.digest, cache) => {
                copies.push((want.digest, seen, want.label));
            }
            _ => return Ok(None),
        }
    }
    Ok(Some(copies))
}

/// the error naming each dataset of `asked` that `checked` keeps, none when every one may go:
/// `data.not_ready` for a file no mirror or source has; else, for files that could not be checked,
/// the error a download of one would meet, a refusal's first; else `unconfirmed` for files checked
/// by their size alone, which `yes` lets go.
fn refused(checked: &Checked, asked: &[String], yes: bool) -> Option<Error> {
    let kept: Vec<&str> = asked
        .iter()
        .map(String::as_str)
        .filter(|name| checked.keeps(name, yes))
        .collect();
    let first = kept.first()?;
    let lacks: Vec<&Lacks> = kept.iter().map(|name| &checked.lacks[*name]).collect();
    let (said, them) = match kept.as_slice() {
        [_] => (format!("{first} stays"), "its"),
        many => (format!("{} stay", units::listed(many, "and")), "their"),
    };
    let it = if kept.len() == 1 { "it" } else { "them" };
    let names = kept.join(" ");
    // a plain push leaves a damaged copy of the right size, which a drop then cannot count
    let push = format!(
        "`{}` keeps a checked copy on a folder mirror, replacing a damaged one, then `{}` frees {it}",
        super::project::command(&format!("push {names} --verify --to <folder>")),
        super::project::command(&format!("drop {names} --mirror <folder>"))
    );
    let lost: Vec<&String> = lacks.iter().flat_map(|lacks| &lacks.lost).collect();
    if !lost.is_empty() {
        return Some(
            Error::new(
                Kind::NotReady,
                format!(
                    "{said} on this machine, since no mirror or source has {} of {them} files",
                    units::grouped(lost.len() as u64)
                ),
            )
            .details_up_to(&lost, NAMED)
            .help(push),
        );
    }
    let unchecked: Vec<&(String, Error)> =
        lacks.iter().flat_map(|lacks| &lacks.unchecked).collect();
    if let Some((_, error)) = unchecked
        .iter()
        .min_by_key(|(_, error)| probe::urgency(error.kind))
    {
        let lines: Vec<&String> = unchecked.iter().map(|(line, _)| line).collect();
        return Some(
            Error::new(
                error.kind,
                format!(
                    "{said} on this machine, since {} of {them} files could not be checked elsewhere",
                    units::grouped(lines.len() as u64)
                ),
            )
            .details_up_to(&lines, NAMED)
            .help(error.help.as_deref().unwrap_or(probe::AGAIN)),
        );
    }
    let sized: u64 = lacks.iter().map(|lacks| lacks.sized).sum();
    let no_size: u64 = lacks.iter().map(|lacks| lacks.no_size).sum();
    let were = |count: u64| if count == 1 { "was" } else { "were" };
    let (checked, alone) = match (sized, no_size) {
        (_, 0) => (
            format!(
                "{} of {them} files {} checked elsewhere by size alone",
                units::grouped(sized),
                were(sized)
            ),
            "on the sizes alone",
        ),
        (0, _) => (
            format!(
                "{} of {them} files {} served without a size",
                units::grouped(no_size),
                were(no_size)
            ),
            "anyway",
        ),
        _ => (
            format!(
                "{} of {them} files {} checked elsewhere by size alone and {} served without a size",
                units::grouped(sized),
                were(sized),
                units::grouped(no_size)
            ),
            "anyway",
        ),
    };
    Some(
        Error::new(
            Kind::Unconfirmed,
            format!(
                "{said} on this machine, since {checked}, which does not show the bytes are the same"
            ),
        )
        .help(format!(
            "{push}; or `{}` frees {it} {alone}",
            super::project::command(&format!("drop {names} --yes"))
        )),
    )
}

/// `manifest` as a sync of every dataset reads it while `dropped` are dropped here: each with
/// `place` false, so the run neither fetches nor places it.
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

/// whether a run of `scope`, for which the person named `named`, leaves `name` out: it is
/// dropped here and the run only reaches it, as an add reaches the rest of the project, so the run
/// neither fetches nor places it.
pub(super) fn stays_dropped(
    name: &str,
    scope: &Scope,
    named: &BTreeSet<String>,
    dropped: &BTreeSet<String>,
) -> bool {
    dropped.contains(name) && scope.includes(name) && !named.contains(name)
}

/// `manifest` as a run of `scope`, for which the person named `named`, places it while `dropped`
/// are dropped here: each dataset [`stays_dropped`] says it leaves out has `place` false.
pub(super) fn placed_naming<'m>(
    manifest: &'m Manifest,
    scope: &Scope,
    named: &BTreeSet<String>,
    dropped: &BTreeSet<String>,
) -> std::borrow::Cow<'m, Manifest> {
    let out: BTreeSet<String> = dropped
        .iter()
        .filter(|name| stays_dropped(name, scope, named, dropped))
        .cloned()
        .collect();
    if out.is_empty() {
        return std::borrow::Cow::Borrowed(manifest);
    }
    let mut placed = manifest.clone();
    left_out(&mut placed, &out);
    std::borrow::Cow::Owned(placed)
}

/// `manifest` as an add of `names` places it while `survey` notes datasets dropped here: each one
/// the add does not name stays dropped, as a plain sync leaves it.
pub(super) fn placed_adding<'m>(
    manifest: &'m Manifest,
    names: &[String],
    survey: &Survey,
) -> std::borrow::Cow<'m, Manifest> {
    let added = names.iter().cloned().collect();
    placed_naming(manifest, &Scope::All, &added, &survey.state.dropped)
}

/// whether `scope` names a dataset `dropped` here, which its run brings back.
pub(super) fn names_any(scope: &Scope, dropped: &BTreeSet<String>) -> bool {
    match scope {
        Scope::All => false,
        Scope::Only(names) => names.iter().any(|name| dropped.contains(name)),
    }
}

/// takes the datasets the person named, `named`, off `survey`'s list of those dropped here, so
/// the run places them and writes the state without them. the run then looks at every file it
/// places, since the last sync's word that all was in place left them out.
pub(super) fn brought_back(survey: &mut Survey, named: &BTreeSet<String>) {
    let back: BTreeSet<String> = survey.state.dropped.intersection(named).cloned().collect();
    if back.is_empty() {
        return;
    }
    survey.state.dropped.retain(|name| !back.contains(name));
    survey.state.freeing.retain(|name| !back.contains(name));
    let disk = &survey.disk;
    forget_gone(
        &mut survey.state.files,
        |placed| placed.dataset.as_str(),
        &back,
        |path| disk.contains_key(path),
    );
    survey.state.synced = None;
    survey.needs_writing = true;
}

/// forgets the records in `files` of the datasets `back` whose files are gone from disk, as
/// `there` says: a drop cut short took them, so placing them again puts back nothing a person
/// deleted.
fn forget_gone<V>(
    files: &mut BTreeMap<String, V>,
    dataset: impl Fn(&V) -> &str,
    back: &BTreeSet<String>,
    there: impl Fn(&str) -> bool,
) {
    files.retain(|path, placed| !back.contains(dataset(placed)) || there(path));
}

/// `lock` without the datasets `dropped` here, as the cache's needs read this project.
pub(super) fn kept_here(mut lock: Lock, dropped: &BTreeSet<String>) -> Lock {
    if dropped.is_empty() {
        return lock;
    }
    lock.datasets
        .retain(|locked| !dropped.contains(&locked.name));
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
    let mut survey = Survey::read(
        project,
        &Folders::of(manifest),
        context.threads,
        &context.stop,
    )?;
    in_place(
        context,
        project,
        (manifest, lock),
        &cache,
        &mut survey,
        freeing,
        "would keep",
    );
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
        &freed_line(report, "free", &manifest.data_dir),
    );
    tell_kept(&context.output, &preview.placement, "would keep");
    report.placement = Some(preview.placement);
    Ok(())
}

/// takes as placed the files of `freeing` that hold their pinned bytes at their places while the
/// state records none there, as in a copy of the project, whose state is not used, so the drop
/// frees them as status counts them in place. one with other bytes stays, and a warning says so,
/// with `verb`.
fn in_place(
    context: &Context,
    project: &crate::project::Project,
    (manifest, lock): (&Manifest, &Lock),
    cache: &Cache,
    survey: &mut Survey,
    freeing: &BTreeSet<String>,
    verb: &str,
) {
    // a plan that fails here fails the removal's own plan too, which says why
    let Ok(planned) = crate::plan::from_cache(manifest, lock, cache) else {
        return;
    };
    let targets: Vec<&crate::plan::Target> = planned
        .targets
        .iter()
        .filter(|target| freeing.contains(target.dataset))
        .collect();
    for path in place::take_in_place(project, cache, survey, &targets) {
        context.output.warn(
            Warning::PlaceKept,
            &format!(
                "{path} holds other bytes than data.lock pins and fetchloom did not place it, so drop {verb} it"
            ),
        );
    }
}

/// what `report` frees, after `verb`: its datasets, their bytes, and their files in the data
/// folder `data_dir` and in the cache.
fn freed_line(report: &Report, verb: &str, data_dir: &str) -> String {
    format!(
        "{} · {verb} {}: {} from {data_dir}/ and {} from the cache",
        units::listed(&report.dropped, "and"),
        units::size(report.bytes),
        units::count(report.files, "file", "files"),
        units::count(report.objects, "file", "files")
    )
}

/// a warning for each changed file a drop keeps, or with `verb` "would keep", would keep.
fn tell_kept(output: &crate::output::Output, placement: &Placement, verb: &str) {
    for kept in &placement.kept {
        output.warn(
            Warning::PlaceKept,
            &format!(
                "{} has changed since fetchloom placed it, so drop {verb} it",
                kept.path
            ),
        );
    }
}

/// notes `freeing` as dropped here in the state, and as not freed from the cache yet.
fn mark(project: &crate::project::Project, freeing: &BTreeSet<String>) -> Result<(), Error> {
    let (mut state, _) = project.state();
    state.dropped.extend(freeing.iter().cloned());
    state.freeing.extend(freeing.iter().cloned());
    project.write_state(&state)
}

/// notes that the drop of `freeing` freed what the cache held of them, so none is told cut short.
fn done_freeing(
    project: &crate::project::Project,
    freeing: &BTreeSet<String>,
) -> Result<(), Error> {
    let (mut state, _) = project.state();
    let before = state.freeing.len();
    state.freeing.retain(|name| !freeing.contains(name));
    if state.freeing.len() == before {
        return Ok(());
    }
    project.write_state(&state)
}

/// drops `freeing`: reads what the cache must keep for everything else, then marks them dropped
/// in the state, removes their unchanged files from the data folder, and frees the cache files
/// nothing else needs, under the cache's own lock, as gc does. a file whose copy on a folder
/// mirror changed since it hashed stays in the cache.
fn free(
    context: &Context,
    project: &crate::project::Project,
    manifest: &Manifest,
    lock: &Lock,
    freeing: &BTreeSet<String>,
    report: &mut Report,
) -> Result<(), Error> {
    let cache = super::cache::hold(context)?;
    let mut survey = Survey::read(
        project,
        &Folders::of(manifest),
        context.threads,
        &context.stop,
    )?;
    in_place(
        context,
        project,
        (manifest, lock),
        &cache,
        &mut survey,
        freeing,
        "keeps",
    );
    let copies = copies(&survey, freeing);
    let (without, unpinned) = without(manifest, lock, freeing);
    let scope = Scope::Only(freeing.clone());
    let mut placing =
        place::Placing::new(project, &without, &unpinned, &cache, false, &scope, survey)?;
    placing.keep_marked(outputs_at(manifest, freeing));
    // what the cache keeps for every other dataset and project is read before anything changes,
    // so a record that cannot be read refuses the drop with nothing dropped
    let objects = held(&cache, lock, freeing);
    let mut dropped = project.dropped();
    dropped.extend(freeing.iter().cloned());
    let current = kept_here(lock.clone(), &dropped);
    let needed = super::cache::needed_besides(
        &cache,
        (&project.root, &current),
        &objects.archives,
        context.threads,
        &context.stop,
    )?;
    // the last look for a stop: once files go, the drop runs to its end
    context.stop.check()?;
    context.stop.finish();
    // noted before any file goes, so a drop cut short leaves them dropped: a plain sync leaves
    // them out and one naming a dataset places it again, and a later gc frees their cache files
    mark(project, freeing)?;
    let placement = placing.remove_quietly(&context.output)?;
    // placing forgets the notes of datasets its manifest lacks, as these are there
    mark(project, freeing)?;
    tell_kept(&context.output, &placement, "keeps");
    let removed: u64 = copies
        .iter()
        .filter(|(path, _)| !native(&project.root, path).exists())
        .map(|(_, size)| size)
        .sum();
    #[cfg(debug_assertions)]
    crate::stop::hold("FETCHLOOM_TEST_HOLD_DROP");
    let mut digests: Vec<(&Sha256, &u64)> = objects.sizes.iter().collect();
    digests.sort();
    let (mut freed, mut bytes) = (0, 0);
    for (digest, size) in digests {
        if needed.objects.contains(digest) {
            continue;
        }
        if let Some((root, label)) = context.mirrors.changed(digest) {
            context.output.warn(
                Warning::MirrorDamaged,
                &format!(
                    "kept {label} in the cache, since its copy on {} changed after it was checked",
                    root.display()
                ),
            );
            continue;
        }
        if cache.free_input(digest, &mut report.not_removed) {
            freed += 1;
            bytes += size;
        }
    }
    // a file the cache could not let go of keeps the drop unfinished, which the same drop ends
    if report.not_removed.is_empty()
        && let Err(error) = done_freeing(project, freeing)
    {
        tracing::debug!(error = %error, "could not note that the drop freed the cache");
    }
    report.files = placement.removed;
    report.objects = freed;
    report.bytes = bytes + removed;
    report.placement = Some(placement);
    context
        .output
        .done("Dropped", &freed_line(report, "freed", &manifest.data_dir));
    Ok(())
}

/// the folders of the outputs among `freeing`, which keep the files data.lock pins.
fn outputs_at(manifest: &Manifest, freeing: &BTreeSet<String>) -> Vec<String> {
    freeing
        .iter()
        .filter_map(|name| manifest.datasets.get(name)?.at.clone())
        .collect()
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

/// the cache files the datasets `freeing` of `lock` may hold: their pinned files and the files their
/// archives unpacked to.
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
    held
}

#[cfg(test)]
mod tests {
    use super::*;

    /// a dataset brought back loses the records of its files a drop cut short took, and keeps
    /// those still on disk; another dataset's records stay as they are.
    #[test]
    fn a_drop_cut_short_leaves_no_file_a_person_deleted() {
        let mut files: BTreeMap<String, String> = [
            ("data/many/a.csv", "many"),
            ("data/many/b.csv", "many"),
            ("data/iris/iris.csv", "iris"),
        ]
        .into_iter()
        .map(|(path, dataset)| (path.to_owned(), dataset.to_owned()))
        .collect();
        let back = BTreeSet::from(["many".to_owned()]);
        forget_gone(&mut files, String::as_str, &back, |path| {
            path == "data/many/b.csv"
        });
        assert_eq!(
            files.keys().collect::<Vec<_>>(),
            ["data/iris/iris.csv", "data/many/b.csv"]
        );
    }
}
