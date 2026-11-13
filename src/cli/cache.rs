//! `fetchloom cache`: where the cache is and what it holds. `cache gc` frees what no project needs,
//! and `cache verify` hashes every object again.

mod inputs;

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::Serialize;

use super::{Context, open_cache};
use crate::cache::{Access, Cache, Checked, Failure, Freed, Member, Needed, Supply, Usage, Use};
use crate::digest::Sha256;
use crate::error::{Error, Kind};
use crate::output::{Output, Warning};
use crate::project::Project;
use crate::project::lock::archives::{self, Pinned};
use crate::project::lock::{self, Lock};
use crate::project::manifest::{Entry, Manifest};
use crate::recipe;
use crate::stop::Stop;
use crate::threads::Threads;
use crate::units;

/// what `fetchloom cache` reports, as `--json` prints it.
#[derive(Serialize)]
pub(super) struct Info {
    path: PathBuf,
    #[serde(flatten)]
    usage: Usage,
    projects: Vec<PathBuf>,
    /// downloads a run stopped before they finished, kept so a later run can continue them.
    unfinished_downloads: u64,
    unfinished_bytes: u64,
}

pub(super) fn info(context: &Context) -> Result<Info, Error> {
    let cache = open_cache()?;
    let Found {
        needed, projects, ..
    } = needed_with(&cache, None, &context.stop, false, Uses::Told)?;
    let scan = cache.scan_objects(context.threads, &context.stop)?;
    if let Some(failure) = scan.unreadable.first() {
        return Err(failure.error("cannot inspect"));
    }
    let usage = Usage::measure(
        &scan,
        &needed,
        &projects,
        SystemTime::now(),
        context.threads,
        &context.stop,
    )?;
    let output = &context.output;
    output.step(
        "Cache",
        &format!(
            "{} · {} · {}",
            units::path(cache.root()),
            units::count(usage.objects, "file", "files"),
            units::size(usage.bytes)
        ),
    );
    tell_projects(output, &projects);
    let unfinished = cache.unfinished();
    if unfinished.downloads > 0 {
        output.step(
            "Unfinished",
            &format!(
                "{} · {}, kept so a later run can continue",
                units::count(unfinished.downloads, "download", "downloads"),
                units::size(unfinished.bytes)
            ),
        );
    }
    tell_usage(output, &usage, projects.len());
    if unfinished.downloads > 0 {
        output.hint("`fetchloom cache gc --now` also frees unfinished downloads no fetchloom is running; those start over next time");
    }
    output.answer_untold(&cache.root().display().to_string());
    Ok(Info {
        path: cache.root().to_path_buf(),
        usage,
        projects,
        unfinished_downloads: unfinished.downloads,
        unfinished_bytes: unfinished.bytes,
    })
}

fn tell_projects(output: &Output, projects: &[PathBuf]) {
    const LISTED: usize = 3;
    if projects.len() <= LISTED {
        for project in projects {
            output.step("Used by", &units::path(project));
        }
        return;
    }
    let count = units::count(projects.len() as u64, "project", "projects");
    if output.shows_details() {
        output.step("Used by", &count);
        for project in projects {
            output.detail(&units::path(project));
        }
    } else {
        output.step(
            "Used by",
            &format!("{count} · `fetchloom cache -v` lists them"),
        );
    }
}

fn tell_usage(output: &Output, usage: &Usage, projects: usize) {
    if usage.objects == 0 {
        output.done("Ok", "the cache has no data files");
        return;
    }
    if projects > 0 {
        output.step("Needed", &needed_line(usage, projects));
    }
    let unneeded = unneeded_line(usage);
    if let Some(line) = &unneeded {
        output.step("Unneeded", line);
    }
    if let Some(line) = copies_line(usage) {
        output.step("Copies", &line);
    }
    if unneeded.is_none() {
        output.done("Ok", "every cached file is needed by a project");
    } else if usage.unneeded == 0 || usage.freeable + usage.young > 0 {
        // gc frees nothing of what folders outside the cache link
        output.hint("`fetchloom cache gc` frees what no project needs once it is a day old; `fetchloom cache gc --now` frees it now");
    }
    if usage.unlinked_copies > 0 {
        output.hint("to link those files to the cache again, delete their project's data folder, if it holds nothing else you need, and run `fetchloom sync` there");
    }
}

fn needed_line(usage: &Usage, projects: usize) -> String {
    let mut line = format!(
        "{} by {}",
        units::size(usage.needed),
        units::count(projects as u64, "project", "projects")
    );
    if let Some(inputs) = usage.inputs_only.filter(|inputs| *inputs > 0) {
        line = format!(
            "{line} · {} is read only by builds",
            of_it(inputs, usage.needed)
        );
    }
    if usage.linked > 0 {
        line.push_str(" · ");
        line.push_str(&linked_part(usage.linked, usage.needed));
    }
    line
}

fn unneeded_line(usage: &Usage) -> Option<String> {
    if usage.unneeded_objects == 0 {
        return None;
    }
    let (freeable, young) = (usage.freeable, usage.young);
    let elsewhere = usage.unneeded.saturating_sub(freeable + young);
    let size = units::size;
    let mut parts = Vec::new();
    match (freeable, young) {
        (0, 0) => {}
        (_, 0) if elsewhere == 0 => parts.push("gc frees it now".to_owned()),
        (0, _) if elsewhere == 0 => parts.push("gc frees it once it is a day old".to_owned()),
        (_, _) if elsewhere == 0 => parts.push(format!(
            "gc frees {} now, the rest once it is a day old",
            size(freeable)
        )),
        (_, 0) => parts.push(format!("gc frees {} now", size(freeable))),
        (0, _) => parts.push(format!("gc frees {} once it is a day old", size(young))),
        (_, _) => parts.push(format!(
            "gc frees {} now and {} once it is a day old",
            size(freeable),
            size(young)
        )),
    }
    if elsewhere > 0 {
        parts.push(linked_part(elsewhere, usage.unneeded));
    }
    let mut line = size(usage.unneeded);
    if !parts.is_empty() {
        line.push_str(" · ");
        line.push_str(&parts.join("; "));
    }
    Some(line)
}

fn linked_part(linked: u64, whole: u64) -> String {
    format!(
        "{} is linked outside the cache too, so deleting it would free nothing",
        of_it(linked, whole)
    )
}

fn of_it(part: u64, whole: u64) -> String {
    if part == whole {
        "all of it".to_owned()
    } else {
        format!("{} of it", units::size(part))
    }
}

fn copies_line(usage: &Usage) -> Option<String> {
    if usage.copies == 0 {
        return None;
    }
    let mut parts = vec![format!(
        "{} in {} are separate copies of cached files",
        units::size(usage.copies),
        units::count(usage.copied_projects, "project", "projects")
    )];
    if usage.unlinked_copies > 0 {
        parts.push(format!(
            "{} was linked when placed, as `rsync -a` without -H or `cp -a` leaves it",
            of_it(usage.unlinked_copies, usage.copies)
        ));
    }
    if usage.cloned > 0 {
        parts.push(format!(
            "{} was placed as clones, which share their blocks with the cache's copy until either changes",
            of_it(usage.cloned, usage.copies)
        ));
    }
    Some(parts.join("; "))
}

/// what `cache gc` freed, or with `--dry-run` would free, as `--json` prints it.
#[derive(Serialize)]
pub(super) struct Collected {
    #[serde(flatten)]
    freed: Freed,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    dry_run: bool,
    /// with `--dry-run`, every path gc would remove.
    #[serde(skip_serializing_if = "Option::is_none")]
    changes: Option<Removals>,
    /// with `--inputs`, the raw input it freed, or would free, and what it kept.
    #[serde(skip_serializing_if = "Option::is_none")]
    inputs: Option<inputs::Inputs>,
}

#[derive(Serialize)]
struct Removals {
    remove: Vec<PathBuf>,
}

/// frees what no project needs, and with `inputs` what [`inputs::gc`] frees. a dry run sweeps a
/// cache that only records removals, so it decides as a real sweep does, but takes no lock.
pub(super) fn gc(
    context: &Context,
    now: bool,
    inputs: bool,
    dry_run: bool,
) -> Result<Collected, Error> {
    if inputs {
        return inputs::gc(context, now, dry_run);
    }
    let cache = if dry_run {
        let cache = super::peek_cache()?;
        if !cache.root().exists() {
            return Ok(tell_dry_gc(context, Freed::default(), Vec::new(), None));
        }
        cache.pretending()
    } else {
        hold(context)?
    };
    let found = needed_with(&cache, None, &context.stop, true, Uses::Untold)?;
    let freed = sweep(context, &cache, &found, now)?;
    if dry_run {
        return Ok(tell_dry_gc(context, freed, cache.pretended(), None));
    }
    tell_swept(context, freed, found.projects.len(), None)
}

/// the cache, held alone for gc once no other run uses it.
fn hold(context: &Context) -> Result<Cache, Error> {
    super::hold_cache(
        super::open_unheld()?,
        Access::Exclusive,
        &context.stop,
        &|| {
            context.output.step(
                "Waiting",
                "for other fetchloom runs to finish with the cache",
            );
        },
    )
}

fn sweep(context: &Context, cache: &Cache, found: &Found, now: bool) -> Result<Freed, Error> {
    let grace = if now {
        std::time::Duration::ZERO
    } else {
        crate::cache::GRACE
    };
    let freed = cache.sweep_with_stop(
        &found.needed,
        &found.stale_projects,
        SystemTime::now(),
        grace,
        context.threads,
        &context.stop,
    )?;
    // the sweep looked for a stop last, so what it freed is told whatever is asked from here
    context.stop.finish();
    Ok(freed)
}

fn tell_swept(
    context: &Context,
    freed: Freed,
    projects: usize,
    inputs: Option<inputs::Inputs>,
) -> Result<Collected, Error> {
    let output = &context.output;
    for failure in &freed.not_removed {
        warn_failure(output, "could not read or remove", failure);
    }
    tell_forgotten(output, &freed.forgotten);
    let kept = kept(&freed);
    let freed_inputs = inputs
        .as_ref()
        .is_some_and(|inputs| inputs.freed_objects > 0);
    if freed.removed_objects == 0
        && freed.removed_leftovers == 0
        && freed.not_removed.is_empty()
        && !freed_inputs
    {
        match &kept {
            Some(kept) => {
                output.done("Ok", "nothing to free yet");
                output.hint(kept);
            }
            None => output.done("Ok", "nothing to free"),
        }
        hint_forgotten(output, &freed);
        return Ok(collected(freed, inputs));
    }
    tell_removed(output, "Removed", &freed);
    if !freed.not_removed.is_empty() {
        let error = with_failures(
            Error::new(
                Kind::LocalIo,
                format!(
                    "{} in the cache could not be read or removed",
                    units::count(freed.not_removed.len() as u64, "item", "items")
                ),
            ),
            &freed.not_removed,
        );
        let result = serde_json::to_value(collected(freed, inputs)).map_err(|err| {
            Error::new(
                Kind::LocalIo,
                format!("cannot format the cache result: {err}"),
            )
        })?;
        return Err(error.with_result(result));
    }
    let needs = if projects == 1 { "needs" } else { "need" };
    let projects = units::count(projects as u64, "project", "projects");
    if freed_inputs {
        output.done(
            "Cleaned",
            &format!("the cache, keeping the rest of what {projects} {needs}"),
        );
    } else {
        output.done(
            "Cleaned",
            &format!("the cache, keeping files needed by {projects}"),
        );
    }
    if let Some(kept) = &kept {
        output.hint(kept);
    }
    hint_forgotten(output, &freed);
    Ok(collected(freed, inputs))
}

fn tell_removed(output: &Output, verb: &str, freed: &Freed) {
    if freed.removed_objects > 0 {
        output.step(
            verb,
            &format!(
                "{} · {} from the cache",
                units::count(freed.removed_objects, "file", "files"),
                units::size(freed.removed_bytes)
            ),
        );
    }
    if freed.removed_leftovers > 0 {
        output.step(
            verb,
            &format!(
                "{} · {} of unfinished downloads and temporary files",
                units::count(freed.removed_leftovers, "leftover item", "leftover items"),
                units::size(freed.removed_leftover_bytes)
            ),
        );
    }
}

fn collected(freed: Freed, inputs: Option<inputs::Inputs>) -> Collected {
    Collected {
        freed,
        dry_run: false,
        changes: None,
        inputs,
    }
}

/// a line for each project gc forgets, since the files only it needed go with it.
fn tell_forgotten(output: &Output, forgotten: &[PathBuf]) {
    for root in forgotten {
        output.mark(
            "Forgot",
            &format!("{} · it has no data.toml anymore", root.display()),
        );
    }
}

fn hint_forgotten(output: &Output, freed: &Freed) {
    if !freed.forgotten.is_empty() {
        output.hint(
            "if a project moved, run `fetchloom sync` in its new folder, so gc keeps its files",
        );
    }
}

fn kept(freed: &Freed) -> Option<String> {
    (freed.kept_objects > 0).then(|| {
        format!(
            "{} no project needs is kept for a day in case you want it back; `fetchloom cache gc --now` frees it now",
            units::size(freed.kept_bytes)
        )
    })
}

fn tell_dry_gc(
    context: &Context,
    freed: Freed,
    remove: Vec<PathBuf>,
    inputs: Option<inputs::Inputs>,
) -> Collected {
    let output = &context.output;
    for failure in &freed.not_removed {
        warn_failure(
            output,
            "could not read; gc would leave this in the cache:",
            failure,
        );
    }
    for root in &freed.forgotten {
        output.detail(&format!(
            "would forget {}, which has no data.toml anymore",
            root.display()
        ));
    }
    tell_removed(output, "Would remove", &freed);
    for path in &remove {
        output.detail(&path.display().to_string());
    }
    let freeing_inputs = inputs
        .as_ref()
        .is_some_and(|inputs| inputs.freed_objects > 0);
    let nothing = match (
        freed.removed_objects == 0 && freed.removed_leftovers == 0 && !freeing_inputs,
        freed.kept_objects > 0,
    ) {
        (false, _) => None,
        (true, false) => Some("nothing to free"),
        (true, true) => Some("nothing to free yet"),
    };
    super::project::dry_run_done(output, nothing);
    if let Some(kept) = kept(&freed) {
        output.hint(&kept);
    }
    Collected {
        freed,
        dry_run: true,
        changes: Some(Removals { remove }),
        inputs,
    }
}

pub(super) fn verify(context: &Context) -> Result<Checked, Error> {
    let cache = open_cache()?;
    let scan = cache.scan_objects(context.threads, &context.stop)?;
    let total: u64 = scan.objects.iter().map(|object| object.size).sum();
    let what = "cached data files";
    let bar = context.output.bar("Checking", what.to_owned());
    bar.start(Some(total), 0);
    let ticker = context
        .output
        .ticker("Checking", what.to_owned(), [Some(total)]);
    let threads = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    let checked = cache.verify_scanned(
        scan,
        context.threads.limit(threads),
        &|bytes| {
            bar.advance(bytes);
            ticker.advance(bytes);
        },
        &context.stop,
    )?;
    // the hashing looked for a stop last, so what it found is told whatever is asked from here
    context.stop.finish();
    drop((bar, ticker));
    if let Err(error) = report_checked(context, &cache, &checked) {
        let result = serde_json::to_value(&checked).map_err(|err| {
            Error::new(
                Kind::LocalIo,
                format!("cannot format the cache result: {err}"),
            )
        })?;
        return Err(error.with_result(result));
    }
    Ok(checked)
}

fn report_checked(context: &Context, cache: &Cache, checked: &Checked) -> Result<(), Error> {
    let output = &context.output;
    let what = format!(
        "{} · {}",
        units::count(checked.objects, "file", "files"),
        units::size(checked.bytes)
    );
    for failure in &checked.unreadable {
        warn_failure(output, "left unreadable in the cache:", failure);
    }
    if checked.changed.is_empty() {
        if checked.unreadable.is_empty() && checked.objects == 0 {
            output.done("Ok", "the cache has no data files");
            return Ok(());
        }
        if checked.unreadable.is_empty() {
            output.done("Verified", &format!("{what}, every one is intact"));
            return Ok(());
        }
        return Err(with_failures(
            Error::new(
                Kind::LocalIo,
                format!(
                    "checked {what}. {} could not be read",
                    units::count(checked.unreadable.len() as u64, "path", "paths")
                ),
            ),
            &checked.unreadable,
        ));
    }
    for digest in &checked.changed {
        if !checked.stuck.contains(digest) {
            output.warn(
                Warning::CacheChanged,
                &format!(
                    "{} has changed, so fetchloom downloads it again the next time it is needed",
                    cache.object_path(digest).display()
                ),
            );
        }
    }
    let verb = if checked.unreadable.is_empty() {
        "verified"
    } else {
        "checked"
    };
    let mut error = Error::new(
        Kind::IntegrityMismatch,
        format!(
            "{verb} {what}. {} had changed",
            units::count(checked.changed.len() as u64, "file", "files")
        ),
    );
    if checked.stuck.is_empty() {
        return Err(error.help(if checked.unreadable.is_empty() {
            "run `fetchloom sync` in each project; it puts the pinned file back wherever a project's file is linked to a changed copy"
        } else {
            "close programs using the unreadable files or check their permissions, then run `fetchloom cache verify` again and `fetchloom sync` in each project"
        }));
    }
    error = error.detail(format!(
        "fetchloom could not note that {} had changed:",
        units::count(checked.stuck.len() as u64, "of them", "of them")
    ));
    for digest in checked.stuck.iter().take(5) {
        error = error.detail(cache.object_path(digest).display().to_string());
    }
    Err(error.help(format!(
        "check permissions on {}, then run `fetchloom cache verify` again",
        cache.root().join("changed").display()
    )))
}

fn warn_failure(output: &Output, action: &str, failure: &Failure) {
    output.warn_with(
        Warning::CacheUnreadable,
        &format!("{action} {}", failure.description()),
        failure.advice(),
    );
}

fn with_failures(mut error: Error, failures: &[Failure]) -> Error {
    for failure in failures {
        error = error.detail(format!("{}; {}", failure.description(), failure.advice()));
    }
    error.help("address the filesystem errors above, then run the command again")
}

const REMIND_EVERY: std::time::Duration = std::time::Duration::from_hours(24 * 7);
const REMIND_FROM: u64 = 1 << 30;

/// hints at what gc frees, after a command that changed what projects need. it deletes nothing:
/// freeing bytes stays `cache gc`'s, which a person runs on purpose.
pub(super) fn remind(context: &Context) {
    if !context.output.can_ask() {
        return;
    }
    let Some(root) = super::cache_folder() else {
        return;
    };
    remind_at(&context.output, &root, context.threads, &context.stop);
}

/// the reminder for the cache at `root`, which [`open_cache`] opens too, on at most `threads`
/// workers.
fn remind_at(output: &Output, root: &Path, threads: Threads, stop: &Stop) {
    // its only output is a hint, so with hints hidden, as by `-q`, it reads nothing at all
    if !output.shows_hints() || !root.join("objects").is_dir() {
        return;
    }
    let marker = root.join("reminded");
    let now = SystemTime::now();
    let Ok(last) = fs::metadata(&marker).and_then(|metadata| metadata.modified()) else {
        // the first run starts the clock rather than reading the cache right away
        let _ = stamp(&marker, now);
        return;
    };
    // a marker dated in the future, after a clock was wrong, counts as long ago
    let due = now
        .duration_since(last)
        .ok()
        .is_none_or(|age| age >= REMIND_EVERY);
    if !due || stamp(&marker, now).is_err() {
        return;
    }
    let Ok(cache) = open_cache() else { return };
    let Some(freeable) = freeable(&cache, now, threads, stop) else {
        return;
    };
    if freeable >= REMIND_FROM {
        output.hint(&format!(
            "the cache holds {} no project needs; `fetchloom cache gc` frees it",
            units::size(freeable)
        ));
    }
}

/// the bytes gc would free at `now`, as `fetchloom cache` tells them, read on at most `threads`
/// workers; none when a project is out of reach, which gc would forget, or the cache unreadable.
fn freeable(cache: &Cache, now: SystemTime, threads: Threads, stop: &Stop) -> Option<u64> {
    let found = needed_with(cache, None, stop, false, Uses::Untold).ok()?;
    if !found.complete {
        return None;
    }
    let scan = cache.scan_objects(threads, stop).ok()?;
    // the reminder tells nothing of copies, so it reads no project's files
    let usage = Usage::measure(&scan, &found.needed, &[], now, threads, stop).ok()?;
    Some(usage.freeable)
}

/// sets the time itself, since writing an empty file again leaves its time alone on windows.
fn stamp(marker: &std::path::Path, now: SystemTime) -> io::Result<()> {
    crate::cache::open_unfollowed(marker, true)?.set_modified(now)
}

/// every object a registered project, or `current`, needs, as gc keeps them. it fails, as gc
/// stops, on a registration, data.toml or lock that is there but unreadable, so callers keep all.
pub(super) fn needed_by_all(
    cache: &Cache,
    current: Option<&Lock>,
    stop: &Stop,
) -> Result<Needed, Error> {
    Ok(needed_with(cache, current, stop, false, Uses::Untold)?.needed)
}

/// [`needed_by_all`] as far as `candidates`, archives a drop may remove, are concerned: only an
/// archive with one of its files among them checks that the cache holds every member, and each
/// member is looked at once, on `threads` workers, however many projects pin it.
pub(in crate::cli) fn needed_for_drop(
    cache: &Cache,
    current: Option<&Lock>,
    candidates: &[Sha256],
    threads: Threads,
    stop: &Stop,
) -> Result<Needed, Error> {
    let focus = Focus {
        candidates: candidates.iter().copied().collect(),
        present: RefCell::new(HashMap::new()),
        threads,
    };
    Ok(needed_focused(cache, current, stop, false, Uses::Untold, Some(&focus))?.needed)
}

/// the archives a drop asks about, and which of their members the cache was found to hold.
struct Focus {
    candidates: HashSet<Sha256>,
    present: RefCell<HashMap<Sha256, bool>>,
    threads: Threads,
}

impl Focus {
    /// whether one of the files of `archive` is among the candidates.
    fn asks(&self, archive: &archives::Archive<'_>) -> bool {
        archive
            .files()
            .iter()
            .any(|file| self.candidates.contains(&file.sha256))
    }

    /// whether the cache holds every one of `members`.
    fn whole(&self, cache: &Cache, members: &[&Member], stop: &Stop) -> Result<bool, Error> {
        let unknown: Vec<&Member> = {
            let present = self.present.borrow();
            members
                .iter()
                .filter(|member| !present.contains_key(&member.sha256))
                .copied()
                .collect()
        };
        let found = crate::place::run_all(&unknown, self.threads, |member| {
            cache.contains_for_gc(&member.sha256, member.size, stop)
        });
        let mut present = self.present.borrow_mut();
        for (member, found) in unknown.iter().zip(found) {
            present.insert(member.sha256, found?);
        }
        Ok(members
            .iter()
            .all(|member| present.get(&member.sha256).copied().unwrap_or(false)))
    }
}

/// whether [`needed_with`] tells how each project uses each object it needs, which reads the
/// code of the projects' recipes to find which are built.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Uses {
    Told,
    /// told, with the datasets `cache gc --inputs` may free.
    Grouped,
    Untold,
}

struct Found {
    needed: Needed,
    projects: Vec<PathBuf>,
    stale_projects: Vec<PathBuf>,
    /// false when a registered project's data.toml was missing, so `needed` lacks what it needs.
    complete: bool,
    /// with [`Uses::Grouped`], each dataset with `place = false` that recipes read.
    groups: Vec<Group>,
}

/// a dataset with `place = false` that recipes read, in one registered project, with the objects
/// its files are as gc keeps them: what `cache gc --inputs` may free.
#[derive(Debug)]
struct Group {
    project: PathBuf,
    name: String,
    /// its use, as [`Readers::use_of`] tells: an input once every recipe reading it is built.
    used: Use,
    readers: Vec<String>,
    /// of those, the ones not built as data.lock pins them.
    unbuilt: Vec<String>,
    /// the objects its files are, each once.
    objects: Vec<Sha256>,
}

/// what registered projects, and `current`, need. unreadable registrations, manifests and locks
/// stop gc rather than lose protection. `collect_stale` queues roots whose data.toml is gone for
/// the sweep, so inspection does not discard removal errors or delete records early.
fn needed_with(
    cache: &Cache,
    current: Option<&Lock>,
    stop: &Stop,
    collect_stale: bool,
    uses: Uses,
) -> Result<Found, Error> {
    needed_focused(cache, current, stop, collect_stale, uses, None)
}

/// [`needed_with`], narrowed to a drop's candidates by `focus`.
fn needed_focused(
    cache: &Cache,
    current: Option<&Lock>,
    stop: &Stop,
    collect_stale: bool,
    uses: Uses,
    focus: Option<&Focus>,
) -> Result<Found, Error> {
    let told = uses != Uses::Untold;
    let mut needed = Needed {
        told,
        ..Needed::default()
    };
    let mut projects = Vec::new();
    let mut stale_projects = Vec::new();
    let mut complete = true;
    let mut groups = Vec::new();
    for root in cache.registered_projects(stop)? {
        stop.check()?;
        let project = Project { root };
        let manifest = project.manifest_path();
        let text = match fs::read(&manifest) {
            Ok(text) => text,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                if collect_stale {
                    stale_projects.push(project.root);
                }
                complete = false;
                continue;
            }
            Err(err) => {
                return Err(Error::io(
                    format!("cannot read {}", manifest.display()),
                    &err,
                ));
            }
        };
        // data.toml that does not read counts as one without members tables: more is kept
        let kept = std::str::from_utf8(&text)
            .ok()
            .and_then(|text| Manifest::parse(text).ok());
        let lock = project.lock().map_err(|err| {
            // a broken lock is fixed in its own project, which is not where gc runs
            let here = format!("`{}`", crate::suggested::command("lock"));
            let there = format!(
                "`fetchloom -C {} lock`",
                super::shell_word(&project.root.to_string_lossy())
            );
            let help = match err.help.as_deref() {
                Some(lock::BROKEN) => format!("in that project, {}", lock::BROKEN),
                Some(help) => help.replace(&here, &there),
                None => "fix that project's data.lock, then try again".to_owned(),
            };
            Error::new(
                err.kind,
                format!(
                    "{} cannot be read, so the cache stays as it is",
                    project.root.join(lock::FILE).display()
                ),
            )
            .detail(err.message)
            .help(help)
        })?;
        if let Some(lock) = &lock {
            let read = kept.as_ref().map(|manifest| Read {
                manifest,
                root: told.then_some(project.root.as_path()),
            });
            let grouping = (uses == Uses::Grouped).then_some(&mut groups);
            add_with_stop(&mut needed, cache, lock, read, grouping, focus, stop)?;
        }
        // a rebuild kept for `fetchloom build --accept` is in no lock, only in the state, and
        // reading none of its records skips nearly all of it
        let (state, _) = project.state_within(&[], &|_| false);
        for file in state.candidates.values().flat_map(|kept| &kept.files) {
            stop.check()?;
            needed.add(file.sha256, Use::Output);
        }
        projects.push(project.root);
    }
    if let Some(lock) = current {
        add_with_stop(&mut needed, cache, lock, None, None, focus, stop)?;
    }
    Ok(Found {
        needed,
        projects,
        stale_projects,
        complete,
        groups,
    })
}

/// a registered project's data.toml as [`add_with_stop`] reads it, with the project's folder
/// when it tells how each object is used.
#[derive(Clone, Copy)]
struct Read<'a> {
    manifest: &'a Manifest,
    root: Option<&'a Path>,
}

/// adds what one lock needs. an archive keeps its own bytes while this machine never unpacked it
/// or lost a file of it that data.toml keeps, since unpacking again is the only way back.
fn add_with_stop(
    needed: &mut Needed,
    cache: &Cache,
    lock: &Lock,
    read: Option<Read<'_>>,
    mut groups: Option<&mut Vec<Group>>,
    focus: Option<&Focus>,
    stop: &Stop,
) -> Result<(), Error> {
    let mut readers = read.and_then(|read| {
        read.root
            .map(|root| Readers::new(root, read.manifest, lock, cache))
    });
    for result in &lock.derived {
        needed.results.insert((result.key, result.tree));
        let used = match &mut readers {
            Some(readers) => readers.use_of(&result.name, true, stop)?,
            None => Use::Output,
        };
        let mut group = match (&mut readers, &groups) {
            (Some(readers), Some(_)) => readers.group(&result.name, used, stop)?,
            _ => None,
        };
        for file in &result.files {
            stop.check()?;
            note(needed, &mut group, file.sha256, used);
        }
        gather(&mut groups, group);
    }
    for dataset in &lock.datasets {
        let filters = read
            .and_then(|read| read.manifest.datasets.get(&dataset.name))
            .and_then(Entry::source)
            .map(|source| &source.members);
        let used = match &mut readers {
            Some(readers) => readers.use_of(&dataset.name, false, stop)?,
            None => Use::Placed,
        };
        let mut group = match (&mut readers, &groups) {
            (Some(readers), Some(_)) => readers.group(&dataset.name, used, stop)?,
            _ => None,
        };
        for pinned in archives::pinned(&dataset.files) {
            stop.check()?;
            let archive = match pinned {
                Pinned::File(file) => {
                    note(needed, &mut group, file.sha256, used);
                    continue;
                }
                Pinned::Archive(archive) => archive,
            };
            let Some(members) = archive.recorded(cache) else {
                for file in archive.files() {
                    note(needed, &mut group, file.sha256, used);
                }
                continue;
            };
            needed.unpacked.insert(archive.key());
            let filter = filters.and_then(|filters| filters.get(&archive.file.path));
            let kept: Vec<&Member> = members
                .iter()
                .filter(|member| filter.is_none_or(|filter| filter.keeps(&member.path)))
                .collect();
            for member in &kept {
                stop.check()?;
                note(needed, &mut group, member.sha256, used);
            }
            // a drop asks only whether its candidates stay; any other archive keeps its own bytes
            // as they are
            let whole = match focus {
                Some(focus) if !focus.asks(&archive) => true,
                Some(focus) => focus.whole(cache, &kept, stop)?,
                None => {
                    let mut whole = true;
                    for member in &kept {
                        whole &= cache.contains_for_gc(&member.sha256, member.size, stop)?;
                    }
                    whole
                }
            };
            if !whole {
                for file in archive.files() {
                    note(needed, &mut group, file.sha256, used);
                }
            }
        }
        gather(&mut groups, group);
    }
    stop.check()
}

fn note(needed: &mut Needed, group: &mut Option<Group>, digest: Sha256, used: Use) {
    needed.add(digest, used);
    if let Some(group) = group {
        group.objects.push(digest);
    }
}

fn gather(groups: &mut Option<&mut Vec<Group>>, group: Option<Group>) {
    if let (Some(groups), Some(mut group)) = (groups, group) {
        group.objects.sort_unstable();
        group.objects.dedup();
        groups.push(group);
    }
}

/// the recipes that read each dataset of one project, to tell how it uses each dataset's files.
struct Readers<'a> {
    root: &'a Path,
    manifest: &'a Manifest,
    lock: &'a Lock,
    cache: &'a Cache,
    /// the recipes that read each dataset, by the dataset's name.
    by_input: BTreeMap<&'a str, Vec<&'a str>>,
    /// whether each recipe asked about so far is built.
    built: HashMap<&'a str, bool>,
}

impl<'a> Readers<'a> {
    fn new(root: &'a Path, manifest: &'a Manifest, lock: &'a Lock, cache: &'a Cache) -> Self {
        let mut by_input: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for (name, entry) in &manifest.datasets {
            for input in entry.recipe().map_or(&[][..], |recipe| &recipe.from) {
                by_input.entry(input).or_default().push(name);
            }
        }
        Readers {
            root,
            manifest,
            lock,
            cache,
            by_input,
            built: HashMap::new(),
        }
    }

    /// how the project uses the files of the dataset `name`, a recipe's result when `made`.
    fn use_of(&mut self, name: &str, made: bool, stop: &Stop) -> Result<Use, Error> {
        let readers = self.by_input.get(name).cloned().unwrap_or_default();
        let mut pending = false;
        for reader in &readers {
            pending |= !self.built(reader, stop)?;
        }
        let entry = self.manifest.datasets.get(name);
        if !readers.is_empty() && !pending && entry.is_some_and(|entry| !entry.place) {
            return Ok(Use::Input(if made {
                Supply::Derived
            } else if entry.is_some_and(Entry::is_import) {
                Supply::Imported
            } else {
                Supply::Source
            }));
        }
        Ok(if made {
            Use::Output
        } else if pending {
            Use::Pending
        } else {
            Use::Placed
        })
    }

    fn group(&mut self, name: &str, used: Use, stop: &Stop) -> Result<Option<Group>, Error> {
        let Some(readers) = self.by_input.get(name).cloned() else {
            return Ok(None);
        };
        if self
            .manifest
            .datasets
            .get(name)
            .is_none_or(|entry| entry.place)
        {
            return Ok(None);
        }
        let mut unbuilt = Vec::new();
        for reader in &readers {
            if !self.built(reader, stop)? {
                unbuilt.push((*reader).to_owned());
            }
        }
        Ok(Some(Group {
            project: self.root.to_path_buf(),
            name: name.to_owned(),
            used,
            readers: readers.iter().map(|reader| (*reader).to_owned()).collect(),
            unbuilt,
            objects: Vec::new(),
        }))
    }

    /// whether data.lock pins the result building recipe `name` now would make, all of it cached.
    /// a recipe whose code cannot be read counts as changed, which keeps more.
    fn built(&mut self, name: &'a str, stop: &Stop) -> Result<bool, Error> {
        if let Some(built) = self.built.get(name) {
            return Ok(*built);
        }
        let mut built = self.lock.result(name).is_some()
            && recipe::drift(self.root, self.manifest, self.lock, name)
                .is_ok_and(|changes| changes.is_empty());
        for file in self
            .lock
            .result(name)
            .map_or(&[][..], |result| &result.files)
        {
            if !built {
                break;
            }
            built = self.cache.contains_for_gc(&file.sha256, file.size, stop)?;
        }
        self.built.insert(name, built);
        Ok(built)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::Member;
    use crate::cache::tests::store;
    use crate::output::{Level, Style};
    use crate::project::lock::derived::Derived;
    use crate::project::lock::{Locked, LockedFile};

    #[test]
    fn a_reminder_nobody_would_see_reads_nothing() {
        let style = Style {
            color: false,
            unicode: true,
            interactive: true,
            watched: true,
            links: false,
            accessible: false,
        };
        let quiet = Output::new(style, Level::Quiet, false, false);
        let unhinted = Output::new(style, Level::Normal, false, false).without_hints(true);
        for output in [quiet, unhinted] {
            let folder = tempfile::tempdir().unwrap();
            fs::create_dir(folder.path().join("objects")).unwrap();
            remind_at(&output, folder.path(), Threads::default(), &Stop::default());
            assert!(
                !folder.path().join("reminded").exists(),
                "the reminder ran though its hint could not show"
            );
        }
    }

    /// the reminder reads the cache on the run's workers alone: under a cap of one it starts no
    /// thread, where its fan folders would take several.
    #[test]
    fn a_reminder_under_one_worker_starts_no_thread() {
        let folder = tempfile::tempdir().unwrap();
        let cache = Cache::open(folder.path().join("cache")).unwrap();
        for n in 0..300 {
            store(&cache, format!("object {n}").as_bytes());
        }
        let one = Threads::read(&|_| Some("1".into())).unwrap();
        let _counting = crate::threads::refuse_after(usize::MAX);
        assert_eq!(
            freeable(&cache, SystemTime::now(), one, &Stop::default()),
            Some(0),
            "nothing is a day old"
        );
        assert_eq!(crate::threads::spawns_left(), Some(usize::MAX));
    }

    /// a lock pinning set.zip, cached and unpacked to a.txt and b.txt, and the three as stored.
    fn unpacked_archive(cache: &Cache) -> (Lock, [crate::cache::Stored; 3]) {
        let archive = store(cache, b"the archive itself");
        let first = store(cache, b"first\n");
        let second = store(cache, b"second\n");
        let members = [("a.txt", &first), ("b.txt", &second)].map(|(path, stored)| Member {
            path: path.to_owned(),
            size: stored.size,
            sha256: stored.digest,
        });
        let tree = cache.record_unpacked(&archive.digest, &members).unwrap();
        let lock = Lock {
            datasets: vec![Locked {
                name: "set".to_owned(),
                reference: "https://example.org/set.zip".to_owned(),
                spec: String::new(),
                extract: true,
                resolved: "https://example.org/set.zip".to_owned(),
                title: None,
                license: None,
                doi: None,
                access: crate::source::Access::Unknown,
                timestamp: String::new(),
                files: vec![LockedFile {
                    path: "set.zip".to_owned(),
                    size: archive.size,
                    sha256: archive.digest,
                    md5: None,
                    url: "https://example.org/set.zip".to_owned(),
                    unpacks_to: Some(tree),
                }],
            }],
            ..Lock::default()
        };
        (lock, [archive, first, second])
    }

    /// a drop asks only about the archives it may remove, and one of them stays while the cache
    /// lacks a file it unpacked to, however many members it has to look at.
    #[test]
    fn a_drop_keeps_a_candidate_archive_while_a_file_it_unpacked_to_is_missing() {
        let folder = tempfile::tempdir().unwrap();
        let cache = Cache::open(folder.path().join("cache")).unwrap();
        let (mut lock, [archive, _, second]) = unpacked_archive(&cache);
        let large = store(&cache, b"an archive of many files");
        let members: Vec<Member> = (0..130)
            .map(|n| {
                let stored = store(&cache, format!("file {n}\n").as_bytes());
                Member {
                    path: format!("{n}.txt"),
                    size: stored.size,
                    sha256: stored.digest,
                }
            })
            .collect();
        let tree = cache.record_unpacked(&large.digest, &members).unwrap();
        let mut many = lock.datasets[0].clone();
        many.name = "many".to_owned();
        many.files[0] = LockedFile {
            path: "many.zip".to_owned(),
            size: large.size,
            sha256: large.digest,
            md5: None,
            url: "https://example.org/many.zip".to_owned(),
            unpacks_to: Some(tree),
        };
        lock.datasets.push(many);
        let needed = |candidates: &[Sha256]| {
            needed_for_drop(
                &cache,
                Some(&lock),
                candidates,
                Threads::default(),
                &Stop::default(),
            )
            .unwrap()
            .objects
        };
        let both = [archive.digest, large.digest];
        assert!(both.iter().all(|digest| !needed(&both).contains(digest)));
        cache.drop_object(&second.digest).unwrap();
        cache.drop_object(&members[77].sha256).unwrap();
        assert!(both.iter().all(|digest| needed(&both).contains(digest)));
        assert!(needed(&[large.digest]).contains(&large.digest));
        assert!(
            !needed(&[large.digest]).contains(&archive.digest),
            "not asked about"
        );
    }

    /// a split archive's record goes by its parts.
    #[test]
    fn a_split_archive_needs_its_parts_only_while_a_file_it_unpacked_to_is_missing() {
        let folder = tempfile::tempdir().unwrap();
        let cache = Cache::open(folder.path().join("cache")).unwrap();
        let (first, second) = (store(&cache, b"first half"), store(&cache, b"second half"));
        let inside = store(&cache, b"inside\n");
        let key =
            crate::cache::parts_key([(first.digest, first.size), (second.digest, second.size)]);
        let member = Member {
            path: "inside.txt".to_owned(),
            size: inside.size,
            sha256: inside.digest,
        };
        let tree = cache.record_unpacked(&key, &[member]).unwrap();
        let (mut lock, _) = unpacked_archive(&cache);
        let part = |path: &str, stored: &crate::cache::Stored, unpacks_to| LockedFile {
            path: path.to_owned(),
            size: stored.size,
            sha256: stored.digest,
            md5: None,
            url: format!("https://example.org/{path}"),
            unpacks_to,
        };
        lock.datasets[0].files = vec![
            part("scans.zip.001", &first, Some(tree)),
            part("scans.zip.002", &second, None),
        ];
        let mut whole = Needed::default();
        add_with_stop(
            &mut whole,
            &cache,
            &lock,
            None,
            None,
            None,
            &Stop::default(),
        )
        .unwrap();
        assert!(whole.objects.contains(&inside.digest));
        assert!(whole.unpacked.contains(&key), "its record");
        for part in [&first, &second] {
            assert!(
                !whole.objects.contains(&part.digest),
                "its files stand for it"
            );
        }
        cache.drop_object(&inside.digest).unwrap();
        let mut missing = Needed::default();
        add_with_stop(
            &mut missing,
            &cache,
            &lock,
            None,
            None,
            None,
            &Stop::default(),
        )
        .unwrap();
        for part in [&first, &second] {
            assert!(
                missing.objects.contains(&part.digest),
                "every part unpacks again"
            );
        }
    }

    #[test]
    fn an_archive_stays_needed_while_a_file_it_unpacked_to_is_missing() {
        let folder = tempfile::tempdir().unwrap();
        let cache = Cache::open(folder.path().join("cache")).unwrap();
        let (lock, [archive, _, second]) = unpacked_archive(&cache);
        let mut whole = Needed::default();
        add_with_stop(
            &mut whole,
            &cache,
            &lock,
            None,
            None,
            None,
            &Stop::default(),
        )
        .unwrap();
        assert!(whole.objects.contains(&second.digest));
        assert!(
            !whole.objects.contains(&archive.digest),
            "its files stand for it"
        );

        cache.drop_object(&second.digest).unwrap();
        let mut missing = Needed::default();
        add_with_stop(
            &mut missing,
            &cache,
            &lock,
            None,
            None,
            None,
            &Stop::default(),
        )
        .unwrap();
        assert!(
            missing.objects.contains(&archive.digest),
            "unpacking it again is the only way back to b.txt"
        );
    }

    /// data.toml that cannot be read keeps both the member and its archive.
    #[test]
    fn an_excluded_member_neither_counts_as_needed_nor_holds_back_its_archive() {
        let folder = tempfile::tempdir().unwrap();
        let cache = Cache::open(folder.path().join("cache")).unwrap();
        let (lock, [archive, first, second]) = unpacked_archive(&cache);
        let manifest = Manifest::parse(
            "[datasets.set]\nref = \"https://example.org/set.zip\"\n\n[datasets.set.members.\"set.zip\"]\nexclude = \"b.txt\"\n",
        )
        .unwrap();
        let mut kept = Needed::default();
        let read = Some(Read {
            manifest: &manifest,
            root: None,
        });
        add_with_stop(&mut kept, &cache, &lock, read, None, None, &Stop::default()).unwrap();
        assert!(kept.objects.contains(&first.digest));
        assert!(
            !kept.objects.contains(&second.digest),
            "no project places it"
        );

        cache.drop_object(&second.digest).unwrap();
        let mut freed = Needed::default();
        add_with_stop(
            &mut freed,
            &cache,
            &lock,
            read,
            None,
            None,
            &Stop::default(),
        )
        .unwrap();
        assert!(
            !freed.objects.contains(&archive.digest),
            "a.txt, the only file the project places, is cached"
        );
        let mut unread = Needed::default();
        add_with_stop(
            &mut unread,
            &cache,
            &lock,
            None,
            None,
            None,
            &Stop::default(),
        )
        .unwrap();
        assert!(unread.objects.contains(&archive.digest));
    }

    /// raw, a csv file, and clean, a recipe made from it.
    pub(super) const RAW_AND_CLEAN: &str = "[datasets.raw]\nref = \"https://example.org/raw.csv\"\nplace = false\n\n[datasets.clean]\nfrom = [\"raw\"]\nrun = [\"t\", \"{raw}\", \"{out}\"]\n";

    /// data.toml `text`, pinning each of `sources` to one file in `cache`, then each of `results`,
    /// given in the order they are made, as building it now would; it gives each file by name.
    pub(super) fn pinned(
        root: &Path,
        cache: &Cache,
        text: &str,
        sources: &[&str],
        results: &[&str],
    ) -> (Manifest, Lock, BTreeMap<String, crate::cache::Stored>) {
        let manifest = Manifest::parse(text).unwrap();
        let mut lock = Lock::default();
        let mut stored = BTreeMap::new();
        for name in sources {
            let file = store(cache, format!("{name}\n").as_bytes());
            let reference = format!("https://example.org/{name}.csv");
            lock.datasets.push(Locked {
                name: (*name).to_owned(),
                reference: reference.clone(),
                spec: lock::spec(manifest.datasets[*name].source().unwrap()),
                extract: true,
                resolved: reference.clone(),
                title: None,
                license: None,
                doi: None,
                access: crate::source::Access::Unknown,
                timestamp: String::new(),
                files: vec![LockedFile {
                    path: format!("{name}.csv"),
                    size: file.size,
                    sha256: file.digest,
                    md5: None,
                    url: reference,
                    unpacks_to: None,
                }],
            });
            stored.insert((*name).to_owned(), file);
        }
        for name in results {
            let file = store(cache, format!("made {name}\n").as_bytes());
            let step = recipe::Step::read(root, &manifest, &lock, name)
                .unwrap()
                .unwrap();
            let files = vec![Member {
                path: format!("{name}.csv"),
                size: file.size,
                sha256: file.digest,
            }];
            lock.derived.push(Derived {
                name: (*name).to_owned(),
                from: step.from().unwrap(),
                run: step.run.clone(),
                code: step.hashed().unwrap(),
                env: step.env.clone(),
                key: step.key().unwrap(),
                timestamp: String::new(),
                tree: crate::cache::tree_hash(&files),
                files,
            });
            stored.insert((*name).to_owned(), file);
        }
        (manifest, lock, stored)
    }

    /// what the project needs of `cache`, with how it uses each object.
    fn uses(root: &Path, cache: &Cache, manifest: &Manifest, lock: &Lock) -> Needed {
        let mut needed = Needed {
            told: true,
            ..Needed::default()
        };
        let read = Some(Read {
            manifest,
            root: Some(root),
        });
        add_with_stop(&mut needed, cache, lock, read, None, None, &Stop::default()).unwrap();
        needed
    }

    /// a dataset that is placed, or that no recipe reads, counts as placed data.
    #[test]
    fn an_unplaced_source_read_only_by_built_recipes_is_input() {
        let folder = tempfile::tempdir().unwrap();
        let cache = Cache::open(folder.path().join("cache")).unwrap();
        let text = format!(
            "{RAW_AND_CLEAN}\n[datasets.notes]\nref = \"https://example.org/notes.csv\"\nplace = false\n"
        );
        let (manifest, lock, stored) =
            pinned(folder.path(), &cache, &text, &["notes", "raw"], &["clean"]);
        let needed = uses(folder.path(), &cache, &manifest, &lock);
        assert_eq!(
            needed.uses[&stored["raw"].digest],
            Use::Input(Supply::Source)
        );
        assert_eq!(needed.uses[&stored["notes"].digest], Use::Placed);
        assert_eq!(needed.uses[&stored["clean"].digest], Use::Output);
        let placed = Manifest::parse(&text.replacen("place = false\n", "", 1)).unwrap();
        let needed = uses(folder.path(), &cache, &placed, &lock);
        assert_eq!(needed.uses[&stored["raw"].digest], Use::Placed);
    }

    /// a recipe is unbuilt while unpinned, changed since its build, or missing its cached result.
    #[test]
    fn an_input_of_an_unbuilt_recipe_is_pending() {
        let folder = tempfile::tempdir().unwrap();
        let cache = Cache::open(folder.path().join("cache")).unwrap();
        let root = folder.path();
        let (manifest, unpinned, stored) = pinned(root, &cache, RAW_AND_CLEAN, &["raw"], &[]);
        let raw = stored["raw"].digest;
        assert_eq!(
            uses(root, &cache, &manifest, &unpinned).uses[&raw],
            Use::Pending
        );
        let (_, lock, stored) = pinned(root, &cache, RAW_AND_CLEAN, &["raw"], &["clean"]);
        assert_eq!(
            uses(root, &cache, &manifest, &lock).uses[&raw],
            Use::Input(Supply::Source)
        );
        let changed = Manifest::parse(&RAW_AND_CLEAN.replace("[\"t\", ", "[\"t2\", ")).unwrap();
        assert_eq!(uses(root, &cache, &changed, &lock).uses[&raw], Use::Pending);
        cache.drop_object(&stored["clean"].digest).unwrap();
        assert_eq!(
            uses(root, &cache, &manifest, &lock).uses[&raw],
            Use::Pending
        );
    }

    /// a pinned result's files are needed, as its output, and so is its record; one with
    /// `place = false` that only built recipes read is an input a build makes again.
    #[test]
    fn results_are_needed() {
        let folder = tempfile::tempdir().unwrap();
        let cache = Cache::open(folder.path().join("cache")).unwrap();
        let text = format!(
            "{RAW_AND_CLEAN}\n[datasets.tidy]\nfrom = [\"clean\"]\nrun = [\"t\", \"{{clean}}\", \"{{out}}\"]\n"
        );
        let (manifest, lock, stored) =
            pinned(folder.path(), &cache, &text, &["raw"], &["clean", "tidy"]);
        let needed = uses(folder.path(), &cache, &manifest, &lock);
        for name in ["clean", "tidy"] {
            let result = lock.result(name).unwrap();
            assert!(
                needed.results.contains(&(result.key, result.tree)),
                "{name}"
            );
            assert!(needed.objects.contains(&stored[name].digest), "{name}");
            assert_eq!(needed.uses[&stored[name].digest], Use::Output, "{name}");
        }
        let unplaced = Manifest::parse(
            &text.replace("[datasets.clean]\n", "[datasets.clean]\nplace = false\n"),
        )
        .unwrap();
        let needed = uses(folder.path(), &cache, &unplaced, &lock);
        assert_eq!(
            needed.uses[&stored["clean"].digest],
            Use::Input(Supply::Derived)
        );
        assert_eq!(needed.uses[&stored["tidy"].digest], Use::Output);
    }

    /// the bytes a kept rebuild shares with an input are no longer read only by builds.
    #[test]
    fn a_kept_rebuild_needs_its_files_as_a_result_does() {
        let folder = tempfile::tempdir().unwrap();
        let cache = Cache::open(folder.path().join("cache")).unwrap();
        let project = Project {
            root: folder.path().join("study"),
        };
        fs::create_dir(&project.root).unwrap();
        let (_, lock, stored) = pinned(&project.root, &cache, RAW_AND_CLEAN, &["raw"], &["clean"]);
        fs::write(project.manifest_path(), RAW_AND_CLEAN).unwrap();
        project.write_lists(&lock).unwrap();
        project.write(lock::FILE, lock.render().as_bytes()).unwrap();
        cache.register(&project.root).unwrap();
        let raw = &stored["raw"];
        let used = || {
            needed_with(&cache, None, &Stop::default(), false, Uses::Told)
                .unwrap()
                .needed
                .uses[&raw.digest]
        };
        assert_eq!(used(), Use::Input(Supply::Source));
        let clean = lock.result("clean").unwrap();
        let mut state = crate::project::state::State::default();
        state.candidates.insert(
            "clean".to_owned(),
            crate::project::state::Candidate {
                key: clean.key,
                tree: crate::digest::sha256(b"another tree"),
                pinned: clean.tree,
                files: vec![Member {
                    path: "raw.csv".to_owned(),
                    size: raw.size,
                    sha256: raw.digest,
                }],
            },
        );
        project.write_state(&state).unwrap();
        assert_eq!(used(), Use::Output);
    }

    #[test]
    fn the_unneeded_line_says_what_gc_frees_now_later_and_never() {
        for ((freeable, young, linked), said) in [
            ((1024, 0, 0), "1.0 KiB · gc frees it now"),
            ((0, 1024, 0), "1.0 KiB · gc frees it once it is a day old"),
            (
                (1024, 2048, 0),
                "3.0 KiB · gc frees 1.0 KiB now, the rest once it is a day old",
            ),
            (
                (1024, 2048, 3072),
                "6.0 KiB · gc frees 1.0 KiB now and 2.0 KiB once it is a day old; 3.0 KiB of it \
                 is linked outside the cache too, so deleting it would free nothing",
            ),
            (
                (0, 0, 1024),
                "1.0 KiB · all of it is linked outside the cache too, so deleting it would free \
                 nothing",
            ),
            ((0, 0, 0), "0 B"),
        ] {
            let usage = Usage {
                unneeded: freeable + young + linked,
                freeable,
                young,
                unneeded_objects: 1,
                ..Usage::default()
            };
            assert_eq!(unneeded_line(&usage).as_deref(), Some(said));
        }
        assert_eq!(
            unneeded_line(&Usage::default()),
            None,
            "all of it is needed"
        );
    }

    #[test]
    fn the_needed_line_says_what_only_builds_read() {
        let usage = |inputs_only| Usage {
            needed: 3 * 1024,
            inputs_only,
            ..Usage::default()
        };
        assert_eq!(needed_line(&usage(None), 1), "3.0 KiB by 1 project");
        assert_eq!(needed_line(&usage(Some(0)), 1), "3.0 KiB by 1 project");
        assert_eq!(
            needed_line(&usage(Some(1024)), 2),
            "3.0 KiB by 2 projects · 1.0 KiB of it is read only by builds"
        );
        assert_eq!(
            needed_line(&usage(Some(3 * 1024)), 1),
            "3.0 KiB by 1 project · all of it is read only by builds"
        );
    }

    #[test]
    fn the_copies_line_names_what_was_linked_and_what_is_cloned() {
        for ((copies, unlinked_copies, cloned), said) in [
            (
                (1024, 0, 0),
                "1.0 KiB in 1 project are separate copies of cached files",
            ),
            (
                (1024, 1024, 0),
                "1.0 KiB in 1 project are separate copies of cached files; all of it was linked \
                 when placed, as `rsync -a` without -H or `cp -a` leaves it",
            ),
            (
                (3072, 1024, 2048),
                "3.0 KiB in 1 project are separate copies of cached files; 1.0 KiB of it was \
                 linked when placed, as `rsync -a` without -H or `cp -a` leaves it; 2.0 KiB of \
                 it was placed as clones, which share their blocks with the cache's copy until \
                 either changes",
            ),
        ] {
            let usage = Usage {
                copies,
                unlinked_copies,
                cloned,
                copied_projects: 1,
                ..Usage::default()
            };
            assert_eq!(copies_line(&usage).as_deref(), Some(said));
        }
        assert_eq!(copies_line(&Usage::default()), None);
    }
}
