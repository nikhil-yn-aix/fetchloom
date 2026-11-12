//! `cache gc --inputs`: frees the cache's copy of raw data that only built recipes read, where a
//! folder mirror holds a copy that hashes to its name in the same run. a person short on disk
//! keeps every result, and a build takes the raw data from the mirror again when it needs it.
//!
//! the copies are read before anything is held or asked, as a dry run reads, so neither the long
//! read nor the question keeps other runs from the cache. the frees then happen under gc's own
//! lock, each only once the needs walk, read again, still finds it raw input only builds read and
//! its copy is still the one that hashed.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::Serialize;

use super::{Collected, Found, Group, Uses};
use crate::cache::{Cache, Failure, Freed, Needed, Supply, Use};
use crate::cli::Context;
use crate::digest::Sha256;
use crate::error::{Error, Kind};
use crate::mirror::folder::{self, Held, Missing, Seen};
use crate::mirror::{Location, Mirrors, Wanted};
use crate::output::{Output, Warning};
use crate::stop::Stop;
use crate::threads::Threads;
use crate::units;

/// what `cache gc --inputs` freed, or would free, and what it kept, as `--json` prints it.
#[derive(Debug, Default, Serialize)]
pub(super) struct Inputs {
    /// the objects freed, each once, and their payload bytes.
    pub(super) freed_objects: u64,
    freed_bytes: u64,
    /// each dataset whose files were freed.
    datasets: Vec<Dataset>,
    /// each dataset with files kept, once for each reason.
    kept: Vec<Kept>,
}

/// a dataset whose files were freed, or would be.
#[derive(Debug, Serialize)]
struct Dataset {
    project: PathBuf,
    name: String,
    objects: u64,
    bytes: u64,
    route: Route,
    costs: Costs,
}

/// where a build finds a freed dataset's files again.
#[derive(Debug, Serialize)]
struct Route {
    /// the folder mirrors whose copies hashed to their names.
    mirrors: Vec<PathBuf>,
}

/// what freeing a dataset costs.
#[derive(Debug, Serialize)]
struct Costs {
    /// the recipes that read it, which take its files back from its route to run again.
    recipes: Vec<String>,
}

/// files of a dataset kept for one reason.
#[derive(Debug, Serialize)]
struct Kept {
    project: PathBuf,
    name: String,
    objects: u64,
    bytes: u64,
    reason: Reason,
    /// the folder mirror the reason is about, for a damaged, unreadable or changed copy.
    mirror: Option<PathBuf>,
}

/// why `cache gc --inputs` keeps a file it would free otherwise.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
enum Reason {
    /// a recipe that reads it is not built as data.lock pins it, so the next build reads it.
    Pending,
    /// a recipe makes it, and the next build would put it back.
    Derived,
    /// the same bytes are needed another way too, as by a dataset that places them.
    Shared,
    /// a name outside the cache, as a project's hardlink, keeps the bytes anyway.
    Linked,
    /// no folder mirror is set.
    NoMirror,
    /// no folder mirror holds a copy.
    NoCopy,
    /// a mirror's copy is of another size or holds other bytes.
    Damaged,
    /// a mirror's copy could not be read.
    Unreadable,
    /// a mirror's copy changed after it hashed to its name.
    Changed,
}

/// what becomes of one cached object a raw input needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fate {
    /// freed, or to be: the folder mirror at `at` held a copy that hashed to its name, which
    /// looked as `seen` then.
    Free { at: usize, seen: Seen },
    /// kept for `reason`, about the folder mirror at `at` when it names one.
    Keep { reason: Reason, at: Option<usize> },
    /// left out of the report: gone from the cache by other means, or its removal failed, which
    /// the run reports as a failure.
    Left,
}

/// a cached object a dataset in a [`Group`] needs: its size, and whether a name outside the cache
/// keeps its bytes anyway.
#[derive(Clone, Copy, Debug)]
struct Cached {
    size: u64,
    linked: bool,
}

/// what gc `--inputs` found: the datasets it may free, the folder mirrors it read, and what
/// becomes of each cached object those datasets need.
#[derive(Debug)]
struct Plan {
    groups: Vec<Group>,
    /// the folder mirrors, as absolute folders, in the order they were asked.
    mirrors: Vec<PathBuf>,
    /// the cached objects the groups need, with their sizes.
    sizes: HashMap<Sha256, u64>,
    /// what becomes of each of them that a raw input needs.
    fates: HashMap<Sha256, Fate>,
    /// objects a later look kept, after the plan would have freed them.
    dropped: HashSet<Sha256>,
    /// whether `mirrors_only` lets a build ask mirrors alone, so no source brings a file back.
    only_mirrors: bool,
    /// whether web mirrors are set, which gc `--inputs` never counts.
    web: bool,
}

/// frees the raw input a folder mirror holds, as `cache gc --inputs` does, with what plain gc
/// frees. a dry run reads the copies and says what it would free, freeing and asking nothing.
/// otherwise it asks first, unless `--yes` answered, then takes gc's lock and frees.
pub(super) fn gc(context: &Context, now: bool, dry_run: bool) -> Result<Collected, Error> {
    let output = &context.output;
    let peeked = crate::cli::peek_cache()?;
    if !peeked.root().exists() {
        let inputs = Some(Inputs::default());
        if dry_run {
            return Ok(super::tell_dry_gc(
                context,
                Freed::default(),
                Vec::new(),
                inputs,
            ));
        }
        output.done("Ok", "nothing to free");
        return Ok(super::collected(Freed::default(), inputs));
    }
    if dry_run {
        let cache = peeked.pretending();
        let (found, mut plan) = look(context, &cache, true)?;
        let mut not_removed = Vec::new();
        free(&mut plan, &cache, &mut not_removed, &context.stop)?;
        tell(output, &plan, Told::Would);
        let mut freed = super::sweep(context, &cache, &found, now)?;
        merge(&mut freed, not_removed);
        let inputs = Some(report(&plan));
        return Ok(super::tell_dry_gc(
            context,
            freed,
            cache.pretended(),
            inputs,
        ));
    }
    let (_, mut plan) = look(context, &peeked, false)?;
    if !plan.frees() {
        tell(output, &plan, Told::Did);
        let cache = super::hold(context)?;
        let found = super::needed_with(&cache, None, &context.stop, true, Uses::Untold)?;
        let freed = super::sweep(context, &cache, &found, now)?;
        return super::tell_swept(context, freed, found.projects.len(), Some(report(&plan)));
    }
    let asked = ask(context, &plan)?;
    let cache = super::hold(context)?;
    let found = super::needed_with(&cache, None, &context.stop, true, Uses::Grouped)?;
    let cached = cached_of(&cache, &found.groups, &context.stop)?;
    recheck(&mut plan, &found.needed, &found.groups, &cached);
    let mut not_removed = Vec::new();
    free(&mut plan, &cache, &mut not_removed, &context.stop)?;
    tell(output, &plan, if asked { Told::After } else { Told::Did });
    let mut freed = super::sweep(context, &cache, &found, now)?;
    merge(&mut freed, not_removed);
    super::tell_swept(context, freed, found.projects.len(), Some(report(&plan)))
}

/// adds the removals of raw input that failed, `not_removed`, to the sweep's.
fn merge(freed: &mut Freed, not_removed: Vec<Failure>) {
    freed.not_removed.extend(not_removed);
    freed
        .not_removed
        .sort_by(|left, right| left.path.cmp(&right.path));
}

/// the needs walk, with the datasets it may free, and the plan it leads to, with each copy it
/// relies on read: what gc `--inputs` would free from `cache`. `collect_stale` queues the
/// projects whose data.toml is gone, as gc's walk does.
fn look(context: &Context, cache: &Cache, collect_stale: bool) -> Result<(Found, Plan), Error> {
    let mut found = super::needed_with(cache, None, &context.stop, collect_stale, Uses::Grouped)?;
    let groups = std::mem::take(&mut found.groups);
    let cached = cached_of(cache, &groups, &context.stop)?;
    let plan = plan(
        &context.output,
        groups,
        &found.needed,
        &cached,
        &context.mirrors.mirrors,
        context.threads,
        &context.stop,
    )?;
    Ok((found, plan))
}

/// the cached objects the datasets in `groups` need, from one scan of the cache.
///
/// # Errors
///
/// an object folder that cannot be read, which could hide what is cached.
fn cached_of(
    cache: &Cache,
    groups: &[Group],
    stop: &Stop,
) -> Result<HashMap<Sha256, Cached>, Error> {
    let wanted: HashSet<Sha256> = groups
        .iter()
        .flat_map(|group| group.objects.iter().copied())
        .collect();
    let scan = cache.scan_objects(stop)?;
    if let Some(failure) = scan.unreadable.first() {
        return Err(failure.error("cannot inspect; the cache stays unchanged:"));
    }
    Ok(scan
        .objects
        .iter()
        .filter(|object| wanted.contains(&object.digest))
        .map(|object| {
            let found = Cached {
                size: object.size,
                linked: object.has_other_names(),
            };
            (object.digest, found)
        })
        .collect())
}

/// whether `group` is raw input: fetched or imported data that only built recipes read.
fn raw(group: &Group) -> bool {
    matches!(group.used, Use::Input(Supply::Source | Supply::Imported))
}

/// why a dataset that is no raw input keeps all its files: a recipe reading it is not built,
/// or a recipe makes it.
fn kept_whole(group: &Group) -> Option<Reason> {
    match group.used {
        Use::Input(Supply::Source | Supply::Imported) => None,
        Use::Input(Supply::Derived) => Some(Reason::Derived),
        Use::Pending | Use::Output => Some(Reason::Pending),
        Use::Placed => Some(Reason::Shared),
    }
}

/// what gc `--inputs` would free of what `groups` need, given the needs walk's `needed`, the
/// `cached` objects and the folder mirrors among `mirrors`, whose copies are read on up to
/// `threads` threads each. a copy is read only for an object nothing else keeps.
fn plan(
    output: &Output,
    groups: Vec<Group>,
    needed: &Needed,
    cached: &HashMap<Sha256, Cached>,
    mirrors: &Mirrors,
    threads: Threads,
    stop: &Stop,
) -> Result<Plan, Error> {
    let derived = derived(&groups);
    let mut sizes = HashMap::new();
    let mut fates = HashMap::new();
    let mut wanted = Vec::new();
    for group in &groups {
        for object in &group.objects {
            let Some(found) = cached.get(object) else {
                continue;
            };
            sizes.insert(*object, found.size);
            if !raw(group) || fates.contains_key(object) {
                continue;
            }
            let reason = kept_anyway(object, needed, &derived, found);
            if reason.is_none() {
                wanted.push(Wanted {
                    digest: *object,
                    size: found.size,
                    label: group.name.clone(),
                });
            }
            let reason = reason.unwrap_or(Reason::NoCopy);
            fates.insert(*object, Fate::Keep { reason, at: None });
        }
    }
    let folders = folders(mirrors);
    route(output, &folders, wanted, threads, stop, &mut fates)?;
    Ok(Plan {
        groups,
        mirrors: folders,
        sizes,
        fates,
        dropped: HashSet::new(),
        only_mirrors: mirrors.only,
        web: mirrors.web().next().is_some(),
    })
}

/// the objects of recipes' results that only builds read, which the next build would put back.
fn derived(groups: &[Group]) -> HashSet<Sha256> {
    groups
        .iter()
        .filter(|group| group.used == Use::Input(Supply::Derived))
        .flat_map(|group| group.objects.iter().copied())
        .collect()
}

/// why an object of raw input stays whatever a mirror holds, if it does: a use of its bytes
/// other than as raw input, in any project, or a name outside the cache that keeps them anyway.
fn kept_anyway(
    object: &Sha256,
    needed: &Needed,
    derived: &HashSet<Sha256>,
    found: &Cached,
) -> Option<Reason> {
    if !matches!(needed.uses.get(object), Some(Use::Input(_))) || derived.contains(object) {
        return Some(Reason::Shared);
    }
    found.linked.then_some(Reason::Linked)
}

/// reads the copy of each of `wanted` on each folder mirror among `mirrors` in turn, asking a
/// mirror only for what the ones before it held no good copy of, and notes in `fates` what each
/// came to: free, by the first copy that hashed to its name; else kept, by the first damaged or
/// unreadable copy, or as on no mirror. a mirror that is not there is told and skipped.
///
/// # Errors
///
/// a stop.
fn route(
    output: &Output,
    mirrors: &[PathBuf],
    mut wanted: Vec<Wanted>,
    threads: Threads,
    stop: &Stop,
    fates: &mut HashMap<Sha256, Fate>,
) -> Result<(), Error> {
    let mut answers: HashMap<Sha256, (Reason, usize)> = HashMap::new();
    for (at, root) in mirrors.iter().enumerate() {
        if wanted.is_empty() {
            break;
        }
        if let Err(missing) = folder::probe(root) {
            warn_missing(output, root, missing);
            continue;
        }
        let total = wanted.iter().map(|want| want.size).sum();
        let what = format!("copies on {}", root.display());
        let bar = output.bar("Checking", what.clone());
        bar.start(Some(total), 0);
        let ticker = output.ticker("Checking", what, [Some(total)]);
        let started = Instant::now();
        let read = |bytes| {
            bar.advance(bytes);
            ticker.advance(bytes);
        };
        let held = folder::check_all(root, &wanted, threads, &read, stop);
        drop((bar, ticker));
        stop.check()?;
        let held = held.map_err(|err| {
            Error::io(
                format_args!("could not read the mirror {}", root.display()),
                &err,
            )
        })?;
        let (mut files, mut bytes) = (0, 0);
        let (mut unreadable, mut cause) = (0, None);
        let mut left = Vec::new();
        for (want, held) in wanted.into_iter().zip(held) {
            let answer = match held {
                Held::Verified(seen) => {
                    fates.insert(want.digest, Fate::Free { at, seen });
                    files += 1;
                    bytes += want.size;
                    continue;
                }
                Held::Absent => None,
                Held::Damaged => Some(Reason::Damaged),
                Held::Unreadable(err) => {
                    unreadable += 1;
                    cause.get_or_insert_with(|| crate::error::said(&err));
                    Some(Reason::Unreadable)
                }
            };
            if let Some(reason) = answer {
                files += 1;
                bytes += want.size;
                answers.entry(want.digest).or_insert((reason, at));
            }
            left.push(want);
        }
        if files > 0 {
            output.step(
                "Checked",
                &format!(
                    "{} · {} on {} in {}",
                    units::count(files, "file", "files"),
                    units::size(bytes),
                    root.display(),
                    units::duration(started.elapsed())
                ),
            );
        }
        if let Some(cause) = cause {
            output.warn_with(
                Warning::MirrorUnreadable,
                &format!(
                    "could not read {} on {}: {cause}",
                    units::count(unreadable, "copy", "copies"),
                    root.display()
                ),
                "check that this account may read them, then run gc again",
            );
        }
        wanted = left;
    }
    for want in wanted {
        let fate = match answers.get(&want.digest) {
            Some((reason, at)) => Fate::Keep {
                reason: *reason,
                at: Some(*at),
            },
            None if mirrors.is_empty() => Fate::Keep {
                reason: Reason::NoMirror,
                at: None,
            },
            None => Fate::Keep {
                reason: Reason::NoCopy,
                at: None,
            },
        };
        fates.insert(want.digest, fate);
    }
    Ok(())
}

/// the folder mirrors among `mirrors`, as absolute folders, in order.
fn folders(mirrors: &Mirrors) -> Vec<PathBuf> {
    mirrors
        .list
        .iter()
        .filter_map(|location| match location {
            Location::Folder(root) => {
                Some(std::path::absolute(root).unwrap_or_else(|_| root.clone()))
            }
            Location::Web(_) => None,
        })
        .collect()
}

/// keeps each object `plan` would free that the needs walk, read again, no longer finds raw input
/// only builds read, or nothing else keeps: `needed` and `groups` from that walk, and `cached`
/// from a scan after it. an object gone from the cache meanwhile is left out.
fn recheck(plan: &mut Plan, needed: &Needed, groups: &[Group], cached: &HashMap<Sha256, Cached>) {
    let derived = derived(groups);
    let raw: HashSet<Sha256> = groups
        .iter()
        .filter(|group| raw(group))
        .flat_map(|group| group.objects.iter().copied())
        .collect();
    for (object, fate) in &mut plan.fates {
        if !matches!(fate, Fate::Free { .. }) {
            continue;
        }
        let reason = if !raw.contains(object) {
            Some(Reason::Shared)
        } else if let Some(found) = cached.get(object) {
            kept_anyway(object, needed, &derived, found)
        } else {
            *fate = Fate::Left;
            continue;
        };
        if let Some(reason) = reason {
            *fate = Fate::Keep { reason, at: None };
            plan.dropped.insert(*object);
        }
    }
}

/// frees each object `plan` would free whose copy on its mirror is still the one that hashed,
/// recording each refused removal in `not_removed`.
///
/// # Errors
///
/// a stop, which leaves the rest in the cache.
fn free(
    plan: &mut Plan,
    cache: &Cache,
    not_removed: &mut Vec<Failure>,
    stop: &Stop,
) -> Result<(), Error> {
    let mut objects: Vec<Sha256> = plan.fates.keys().copied().collect();
    objects.sort_unstable();
    for object in objects {
        stop.check()?;
        let Some(Fate::Free { at, seen }) = plan.fates.get(&object).copied() else {
            continue;
        };
        if !folder::still(&plan.mirrors[at], &object, &seen) {
            plan.fates.insert(
                object,
                Fate::Keep {
                    reason: Reason::Changed,
                    at: Some(at),
                },
            );
            plan.dropped.insert(object);
        } else if !cache.free_input(&object, not_removed) {
            plan.fates.insert(object, Fate::Left);
        }
    }
    Ok(())
}

impl Plan {
    /// whether it frees anything.
    fn frees(&self) -> bool {
        self.fates
            .values()
            .any(|fate| matches!(fate, Fate::Free { .. }))
    }

    /// what becomes of `object` of `group`.
    fn fate(&self, group: &Group, object: &Sha256) -> Option<Fate> {
        match kept_whole(group) {
            Some(reason) => Some(Fate::Keep { reason, at: None }),
            None => self.fates.get(object).copied(),
        }
    }

    /// what `group` comes to.
    fn tally(&self, group: &Group) -> Tally {
        let mut tally = Tally::default();
        for object in &group.objects {
            let Some(size) = self.sizes.get(object).copied() else {
                continue;
            };
            match self.fate(group, object) {
                Some(Fate::Free { at, .. }) => {
                    tally.freed.0 += 1;
                    tally.freed.1 += size;
                    tally.mirrors.insert(at);
                }
                Some(Fate::Keep { reason, at }) => {
                    let kept = tally.kept.entry((reason, at)).or_default();
                    kept.0 += 1;
                    kept.1 += size;
                    if self.dropped.contains(object) {
                        tally.dropped.insert((reason, at));
                    }
                }
                Some(Fate::Left) | None => {}
            }
        }
        tally
    }
}

/// what one dataset comes to.
#[derive(Default)]
struct Tally {
    /// the objects freed, or to be, and their bytes.
    freed: (u64, u64),
    /// the mirrors they rely on.
    mirrors: BTreeSet<usize>,
    /// the objects kept and their bytes, by why and which mirror that is about.
    kept: BTreeMap<(Reason, Option<usize>), (u64, u64)>,
    /// the reasons among `kept` that a later look found.
    dropped: BTreeSet<(Reason, Option<usize>)>,
}

/// what `plan` comes to, as `--json` reports it.
fn report(plan: &Plan) -> Inputs {
    let mut inputs = Inputs::default();
    let mut counted = HashSet::new();
    for group in &plan.groups {
        let tally = plan.tally(group);
        for object in &group.objects {
            if let Some(Fate::Free { .. }) = plan.fate(group, object)
                && counted.insert(*object)
            {
                inputs.freed_objects += 1;
                inputs.freed_bytes += plan.sizes.get(object).copied().unwrap_or_default();
            }
        }
        if tally.freed.0 > 0 {
            inputs.datasets.push(Dataset {
                project: group.project.clone(),
                name: group.name.clone(),
                objects: tally.freed.0,
                bytes: tally.freed.1,
                route: Route {
                    mirrors: tally
                        .mirrors
                        .iter()
                        .map(|at| plan.mirrors[*at].clone())
                        .collect(),
                },
                costs: Costs {
                    recipes: group.readers.clone(),
                },
            });
        }
        for ((reason, at), (objects, bytes)) in &tally.kept {
            inputs.kept.push(Kept {
                project: group.project.clone(),
                name: group.name.clone(),
                objects: *objects,
                bytes: *bytes,
                reason: *reason,
                mirror: at.map(|at| plan.mirrors[at].clone()),
            });
        }
    }
    inputs
}

/// when the lines tell what became of the files.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Told {
    /// before anything is freed: what would be.
    Would,
    /// after freeing, with no question asked before.
    Did,
    /// after freeing what a person said yes to, having seen what would be: what was freed, and
    /// only what a later look kept of that.
    After,
}

/// the lines for what `plan` frees and keeps of each dataset, and what freeing costs.
fn tell(output: &Output, plan: &Plan, told: Told) {
    let projects: BTreeSet<&Path> = plan
        .groups
        .iter()
        .map(|group| group.project.as_path())
        .collect();
    for group in &plan.groups {
        let tally = plan.tally(group);
        let name = if projects.len() > 1 {
            format!("{} in {}", group.name, group.project.display())
        } else {
            group.name.clone()
        };
        let (files, bytes) = tally.freed;
        if files > 0 {
            let verb = if told == Told::Would {
                "Would free"
            } else {
                "Freed"
            };
            output.step(
                verb,
                &format!(
                    "{name} · {}, {} of raw input that only builds read",
                    units::count(files, "file", "files"),
                    units::size(bytes)
                ),
            );
            let mirrors: Vec<String> = tally
                .mirrors
                .iter()
                .map(|at| plan.mirrors[*at].display().to_string())
                .collect();
            let (holds, of) = match (mirrors.len(), files) {
                (1, 1) => ("holds", "it"),
                (1, _) => ("holds", "each"),
                (_, _) => ("hold", "each"),
            };
            output.item(&format!(
                "{} {holds} a copy of {of}, hashed in this run",
                units::listed(&mirrors, "and")
            ));
            if told != Told::After {
                output.step("Costs", &costs(plan, group, &mirrors, bytes));
            }
        }
        for ((reason, at), (files, bytes)) in &tally.kept {
            if told == Told::After && !tally.dropped.contains(&(*reason, *at)) {
                continue;
            }
            let verb = if told == Told::Would {
                "Would keep"
            } else {
                "Kept"
            };
            output.step(
                verb,
                &format!(
                    "{name} · {}, {}: {}",
                    units::count(*files, "file", "files"),
                    units::size(*bytes),
                    said(plan, group, *reason, *at, *files)
                ),
            );
        }
    }
}

/// what freeing `bytes` of `group` costs: running a recipe that reads it again takes them back
/// from one of `mirrors`, or for data from a source, that source.
fn costs(plan: &Plan, group: &Group, mirrors: &[String], bytes: u64) -> String {
    let source = if group.used == Use::Input(Supply::Source) && !plan.only_mirrors {
        format!(" or {}'s source", group.name)
    } else {
        String::new()
    };
    format!(
        "rerunning {} fetches these {} back from {}{source}",
        units::listed(&group.readers, "or"),
        units::size(bytes),
        units::listed(mirrors, "or")
    )
}

/// why `files` files of `group` are kept, for `reason`, about the mirror at `at`.
fn said(plan: &Plan, group: &Group, reason: Reason, at: Option<usize>, files: u64) -> String {
    let one = files == 1;
    let mirror = at
        .map(|at| plan.mirrors[at].display().to_string())
        .unwrap_or_default();
    match reason {
        Reason::Pending => {
            let (reads, is, it) = if group.unbuilt.len() == 1 {
                ("reads", "is", "it")
            } else {
                ("read", "are", "them")
            };
            format!(
                "{} {reads} it and {is} not built as data.lock pins {it}",
                units::listed(&group.unbuilt, "and")
            )
        }
        Reason::Derived => "a recipe makes it, and the next build would put it back".to_owned(),
        Reason::Shared => {
            "the same bytes are needed another way too, as by a dataset that places them".to_owned()
        }
        Reason::Linked if one => {
            "it is linked outside the cache too, so freeing it would free nothing".to_owned()
        }
        Reason::Linked => {
            "they are linked outside the cache too, so freeing them would free nothing".to_owned()
        }
        Reason::NoMirror if plan.web => {
            "no folder mirror is set, and web mirrors do not count".to_owned()
        }
        Reason::NoMirror => "no folder mirror is set".to_owned(),
        Reason::NoCopy => "no folder mirror holds a copy".to_owned(),
        Reason::Damaged if one => format!("its copy on {mirror} does not hash to its name"),
        Reason::Damaged => format!("their copies on {mirror} do not hash to their names"),
        Reason::Unreadable if one => format!("its copy on {mirror} could not be read"),
        Reason::Unreadable => format!("their copies on {mirror} could not be read"),
        Reason::Changed if one => format!("its copy on {mirror} changed after it was checked"),
        Reason::Changed => format!("their copies on {mirror} changed after they were checked"),
    }
}

/// asks before freeing what `plan` would free, unless `--yes` answered, and says whether it
/// asked. a person at a terminal sees what would be freed and kept first.
///
/// # Errors
///
/// `unconfirmed` when nobody said yes, with what a yes would free; a no changed nothing.
fn ask(context: &Context, plan: &Plan) -> Result<bool, Error> {
    if context.yes {
        return Ok(false);
    }
    let output = &context.output;
    let inputs = report(plan);
    let names: Vec<String> = inputs
        .datasets
        .iter()
        .map(|dataset| dataset.name.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let what = format!(
        "{}, {},",
        units::listed(&names, "and"),
        units::size(inputs.freed_bytes)
    );
    let cancel = format!("freeing {} from the cache", units::listed(&names, "and"));
    if output.can_ask() {
        tell(output, plan, Told::Would);
    }
    match output
        .confirm(&format!("free {what} from the cache?"))
        .map_err(|error| error.cancelling(&cancel))?
    {
        Some(true) => Ok(true),
        Some(false) => Err(Error::new(
            Kind::Unconfirmed,
            format!("did not free {}", units::listed(&names, "and")),
        )
        .cancelled(cancel)
        .unchanged()),
        None => {
            tell(output, plan, Told::Would);
            let error = Error::new(
                Kind::Unconfirmed,
                format!("freeing {what} from the cache needs a yes"),
            )
            .help("answer yes at a terminal, or add --yes, once you are sure the mirror named above keeps these files");
            let result = serde_json::to_value(super::collected(Freed::default(), Some(inputs)))
                .map_err(|err| {
                    Error::new(
                        Kind::LocalIo,
                        format!("cannot format the cache result: {err}"),
                    )
                })?;
            Err(error.with_result(result))
        }
    }
}

/// warns that the folder mirror at `root` is `missing`, so no copy on it is checked.
fn warn_missing(output: &Output, root: &Path, missing: Missing) {
    let shown = root.display();
    let (warning, help) = match missing {
        Missing::Folder => (
            format!("the mirror {shown} is not there, so gc checks no copy on it"),
            "mount it, or check the mirrors setting and --mirror".to_owned(),
        ),
        Missing::Marker => (
            format!(
                "the mirror {shown} holds no fetchloom-mirror file, so gc checks no copy on it"
            ),
            format!("check that {shown} is a mirror's top folder, and that its disk is mounted"),
        ),
        Missing::Unreadable(err) => (
            format!(
                "could not read the mirror {shown}: {}, so gc checks no copy on it",
                crate::error::said(&err)
            ),
            "check that this account may read it".to_owned(),
        ),
    };
    output.warn_with(Warning::MirrorSkipped, &warning, &help);
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::super::{Read, add_with_stop};
    use super::*;
    use crate::cli::cache::tests::{RAW_AND_CLEAN, pinned};
    use crate::mirror::folder::tests::mirror;
    use crate::output::{Level, Style};
    use crate::project::lock::Lock;
    use crate::project::manifest::Manifest;

    /// output nobody sees, as `--json` keeps it off the terminal.
    fn quiet() -> Output {
        let style = Style {
            color: false,
            unicode: true,
            interactive: false,
            watched: false,
            links: false,
            accessible: false,
        };
        Output::new(style, Level::Quiet, true, false)
    }

    /// what the project at `root`, with `manifest` and `lock`, needs of `cache`, with how it uses
    /// each object, and the datasets gc `--inputs` may free.
    fn walked(
        root: &Path,
        cache: &Cache,
        manifest: &Manifest,
        lock: &Lock,
    ) -> (Needed, Vec<Group>) {
        let mut needed = Needed {
            told: true,
            ..Needed::default()
        };
        let mut groups = Vec::new();
        let read = Some(Read {
            manifest,
            root: Some(root),
        });
        add_with_stop(
            &mut needed,
            cache,
            lock,
            read,
            Some(&mut groups),
            &Stop::default(),
        )
        .unwrap();
        (needed, groups)
    }

    /// what gc `--inputs` would free of what `walked` found in `cache`, with the folder mirrors at
    /// `mirrors`.
    fn planned(cache: &Cache, (needed, groups): (Needed, Vec<Group>), mirrors: &[&Path]) -> Plan {
        let stop = Stop::default();
        let cached = cached_of(cache, &groups, &stop).unwrap();
        let mirrors = Mirrors::new(
            mirrors
                .iter()
                .map(|root| Location::Folder(root.to_path_buf()))
                .collect(),
            &[],
            false,
        );
        plan(
            &quiet(),
            groups,
            &needed,
            &cached,
            &mirrors,
            Threads::default(),
            &stop,
        )
        .unwrap()
    }

    /// what became of the files of the dataset `name` in `plan`: freed, from which mirrors, or
    /// kept, why and about which mirror.
    fn fates(plan: &Plan, name: &str) -> Vec<String> {
        let inputs = report(plan);
        let folder = |root: &PathBuf| root.file_name().unwrap().to_string_lossy().into_owned();
        let freed = inputs
            .datasets
            .iter()
            .filter(|dataset| dataset.name == name)
            .map(|dataset| {
                let mirrors: Vec<String> = dataset.route.mirrors.iter().map(folder).collect();
                format!("freed from {}", mirrors.join(", "))
            });
        let kept = inputs
            .kept
            .iter()
            .filter(|kept| kept.name == name)
            .map(|kept| {
                let mirror = kept
                    .mirror
                    .as_ref()
                    .map(|root| format!(" on {}", folder(root)))
                    .unwrap_or_default();
                format!("kept: {:?}{mirror}", kept.reason)
            });
        freed.chain(kept).collect()
    }

    /// raw data with `place = false` that the built recipe clean reads, pinned in a cache in
    /// `folder`, and a folder mirror there holding its file.
    fn raw_and_clean(folder: &Path) -> (Cache, Manifest, Lock, Sha256, PathBuf) {
        let cache = Cache::open(folder.join("cache")).unwrap();
        let (manifest, lock, stored) = pinned(folder, &cache, RAW_AND_CLEAN, &["raw"], &["clean"]);
        let lab = folder.join("lab");
        mirror(&lab, &[b"raw\n"]);
        (cache, manifest, lock, stored["raw"].digest, lab)
    }

    #[test]
    fn an_input_also_placed_elsewhere_is_kept() {
        let folder = tempfile::tempdir().unwrap();
        let root = folder.path();
        let (cache, manifest, lock, raw, lab) = raw_and_clean(root);
        let plan = planned(&cache, walked(root, &cache, &manifest, &lock), &[&lab]);
        assert_eq!(fates(&plan, "raw"), ["freed from lab"]);
        // another project places the same bytes
        let (mut needed, groups) = walked(root, &cache, &manifest, &lock);
        needed.add(raw, Use::Placed);
        let plan = planned(&cache, (needed, groups), &[&lab]);
        assert_eq!(fates(&plan, "raw"), ["kept: Shared"]);
        assert!(!plan.frees());
    }

    /// a copy of the right size with other bytes, a longer one and a shorter one are no route,
    /// while a later mirror's good copy is.
    #[test]
    fn a_damaged_mirror_copy_is_no_route() {
        let folder = tempfile::tempdir().unwrap();
        let root = folder.path();
        let (cache, manifest, lock, raw, lab) = raw_and_clean(root);
        let damaged = root.join("damaged");
        mirror(&damaged, &[]);
        let hex = raw.hex();
        let copy = damaged.join("objects").join(&hex[..2]).join(&hex[2..]);
        fs::create_dir_all(copy.parent().unwrap()).unwrap();
        for bytes in [&b"RAW\n"[..], b"raw and more\n", b"ra"] {
            fs::write(&copy, bytes).unwrap();
            let plan = planned(&cache, walked(root, &cache, &manifest, &lock), &[&damaged]);
            assert_eq!(
                fates(&plan, "raw"),
                ["kept: Damaged on damaged"],
                "{bytes:?}"
            );
        }
        let plan = planned(
            &cache,
            walked(root, &cache, &manifest, &lock),
            &[&damaged, &lab],
        );
        assert_eq!(fates(&plan, "raw"), ["freed from lab"]);
    }

    #[test]
    fn a_pending_input_is_kept() {
        let folder = tempfile::tempdir().unwrap();
        let root = folder.path();
        let cache = Cache::open(root.join("cache")).unwrap();
        let (manifest, lock, _) = pinned(root, &cache, RAW_AND_CLEAN, &["raw"], &[]);
        let lab = root.join("lab");
        mirror(&lab, &[b"raw\n"]);
        let plan = planned(&cache, walked(root, &cache, &manifest, &lock), &[&lab]);
        assert_eq!(fates(&plan, "raw"), ["kept: Pending"]);
        assert!(plan.fates.is_empty(), "no copy is read for it");
    }

    /// only raw input goes: a recipe's result with `place = false` that recipes read stays, since
    /// the next build would put it back.
    #[test]
    fn a_result_only_builds_read_is_kept() {
        let folder = tempfile::tempdir().unwrap();
        let root = folder.path();
        let cache = Cache::open(root.join("cache")).unwrap();
        let text = format!(
            "{}\n[datasets.tidy]\nfrom = [\"clean\"]\nrun = [\"t\", \"{{clean}}\", \"{{out}}\"]\n",
            RAW_AND_CLEAN.replace("[datasets.clean]\n", "[datasets.clean]\nplace = false\n")
        );
        let (manifest, lock, _) = pinned(root, &cache, &text, &["raw"], &["clean", "tidy"]);
        let lab = root.join("lab");
        mirror(&lab, &[b"raw\n", b"made clean\n"]);
        let plan = planned(&cache, walked(root, &cache, &manifest, &lock), &[&lab]);
        assert_eq!(fates(&plan, "clean"), ["kept: Derived"]);
        assert_eq!(fates(&plan, "raw"), ["freed from lab"]);
    }

    #[test]
    fn an_input_a_hardlink_shares_frees_nothing() {
        let folder = tempfile::tempdir().unwrap();
        let root = folder.path();
        let (cache, manifest, lock, raw, lab) = raw_and_clean(root);
        if fs::hard_link(cache.object_path(&raw), root.join("placed.csv")).is_err() {
            return crate::test_skips::skip("hardlinks", "this disk makes no hardlinks");
        }
        let plan = planned(&cache, walked(root, &cache, &manifest, &lock), &[&lab]);
        assert_eq!(fates(&plan, "raw"), ["kept: Linked"]);
    }

    /// a copy that hashed right is relied on only while it is still that copy: one gone since
    /// keeps the file, and the next run frees it once a good copy is back.
    #[test]
    fn a_copy_that_changed_after_its_check_is_kept() {
        let folder = tempfile::tempdir().unwrap();
        let root = folder.path();
        let (cache, manifest, lock, raw, lab) = raw_and_clean(root);
        let mut plan = planned(&cache, walked(root, &cache, &manifest, &lock), &[&lab]);
        assert!(plan.frees());
        let hex = raw.hex();
        fs::remove_file(lab.join("objects").join(&hex[..2]).join(&hex[2..])).unwrap();
        let mut not_removed = Vec::new();
        free(&mut plan, &cache, &mut not_removed, &Stop::default()).unwrap();
        assert_eq!(fates(&plan, "raw"), ["kept: Changed on lab"]);
        assert!(cache.object_path(&raw).is_file());

        let plan = planned(&cache, walked(root, &cache, &manifest, &lock), &[&lab]);
        assert_eq!(fates(&plan, "raw"), ["kept: NoCopy"]);
        mirror(&lab, &[b"raw\n"]);
        let mut plan = planned(&cache, walked(root, &cache, &manifest, &lock), &[&lab]);
        free(&mut plan, &cache, &mut not_removed, &Stop::default()).unwrap();
        assert_eq!(fates(&plan, "raw"), ["freed from lab"]);
        assert!(!cache.object_path(&raw).exists());
        assert_eq!(not_removed, Vec::<Failure>::new());
    }

    /// the look again under gc's lock keeps what a project began to use another way since the
    /// copies were read, as by setting `place = true`.
    #[test]
    fn what_a_project_uses_otherwise_by_the_time_of_freeing_is_kept() {
        let folder = tempfile::tempdir().unwrap();
        let root = folder.path();
        let (cache, manifest, lock, raw, lab) = raw_and_clean(root);
        let mut plan = planned(&cache, walked(root, &cache, &manifest, &lock), &[&lab]);
        assert!(plan.frees());
        let placed = Manifest::parse(&RAW_AND_CLEAN.replacen("place = false\n", "", 1)).unwrap();
        let (needed, groups) = walked(root, &cache, &placed, &lock);
        let stop = Stop::default();
        let cached = cached_of(&cache, &groups, &stop).unwrap();
        recheck(&mut plan, &needed, &groups, &cached);
        let mut not_removed = Vec::new();
        free(&mut plan, &cache, &mut not_removed, &stop).unwrap();
        assert_eq!(fates(&plan, "raw"), ["kept: Shared"]);
        assert!(cache.object_path(&raw).is_file());
    }

    /// only a folder mirror's copy counts: with none set, or a folder without the file or
    /// without its marker, the file stays.
    #[test]
    fn an_input_needs_a_copy_on_a_folder_mirror() {
        let folder = tempfile::tempdir().unwrap();
        let root = folder.path();
        let (cache, manifest, lock, _, _) = raw_and_clean(root);
        let plan = planned(&cache, walked(root, &cache, &manifest, &lock), &[]);
        assert_eq!(fates(&plan, "raw"), ["kept: NoMirror"]);
        let empty = root.join("empty");
        mirror(&empty, &[b"other\n"]);
        let unmarked = root.join("unmarked");
        mirror(&unmarked, &[b"raw\n"]);
        fs::remove_file(unmarked.join(crate::mirror::MARKER)).unwrap();
        let plan = planned(
            &cache,
            walked(root, &cache, &manifest, &lock),
            &[&empty, &unmarked],
        );
        assert_eq!(fates(&plan, "raw"), ["kept: NoCopy"]);
    }
}
