//! `fetchloom cache`: where the cache is and what it holds. `cache gc` frees what no project needs,
//! and `cache verify` hashes every object again.

mod usage;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::Serialize;

use self::usage::Usage;
use super::{Context, open_cache};
use crate::cache::{Access, Cache, Checked, Failure, Freed, Member, Needed, Scan, Unfinished};
use crate::digest::Sha256;
use crate::error::{Error, Kind};
use crate::output::{Output, Warning};
use crate::project::Project;
use crate::project::lock::archives::{self, Pinned};
use crate::project::lock::{self, Lock};
use crate::project::manifest::Manifest;
use crate::stop::Stop;
use crate::threads::Threads;
use crate::units;

/// what `fetchloom cache` reports, as `--json` prints it.
#[derive(Serialize)]
pub(super) struct Info {
    path: PathBuf,
    #[serde(flatten)]
    usage: Usage,
    /// the `cache_budget` setting, which `bytes` is held against, none when no budget is set.
    budget: Option<u64>,
    projects: Vec<PathBuf>,
    /// the fetched datasets of registered projects that wait for their file lists, none of whose
    /// files `needed` counts, as gc counts none.
    waiting: Vec<WaitingIn>,
    /// downloads runs left unfinished, which a later run continues where the server allows.
    unfinished_downloads: u64,
    unfinished_bytes: u64,
}

/// a fetched dataset waiting for its file list in the registered project at `project`.
#[derive(Serialize)]
struct WaitingIn {
    project: PathBuf,
    dataset: String,
}

pub(super) fn info(context: &Context) -> Result<Info, Error> {
    // a report makes no cache where there is none yet
    let cache = super::peek_cache()?;
    super::tell_exposed(&context.output, cache.root());
    let Found {
        needed,
        projects,
        unread,
        waiting,
        ..
    } = needed_with(&cache, None, &context.stop, false, true)?;
    // gc stops at a project it cannot read, so it frees nothing then
    let blocked = !unread.is_empty();
    for error in unread {
        let said = format!("{}; what that project needs is not counted", error.message);
        match &error.help {
            Some(help) => context
                .output
                .warn_with(Warning::CacheUnreadable, &said, help),
            None => context.output.warn(Warning::CacheUnreadable, &said),
        }
    }
    let scan = cache.scan_objects(context.threads, &context.stop)?;
    if let Some(failure) = scan.unreadable.first() {
        return Err(failure.error("cannot inspect"));
    }
    tell_writable(&context.output, &scan.exposed);
    let usage = Usage::measure(
        &scan,
        &needed,
        &projects,
        SystemTime::now(),
        context.threads,
        &context.stop,
    )?;
    let output = &context.output;
    let held = match usage.on_disk {
        Some(disk) => format!(
            "{} in files, {} on disk",
            units::size(usage.bytes),
            units::size(disk)
        ),
        None => format!("{} in files", units::size(usage.bytes)),
    };
    output.label(
        "Cache",
        &format!(
            "{} · {} · {held}",
            units::path(cache.root()),
            units::count(usage.objects, "file", "files"),
        ),
    );
    let budget = context.settings.cache_budget.value;
    if let Some(budget) = budget {
        output.label("Budget", &budget_line(usage.held(), budget));
    }
    tell_projects(output, &projects);
    let unfinished = cache.unfinished();
    if unfinished.downloads > 0 {
        output.label("Unfinished", &unfinished_line(&unfinished));
    }
    tell_usage(output, &usage, projects.len(), blocked);
    if usage.unneeded_objects > 0 {
        tell_waiting(output, &waiting);
    }
    if unfinished.downloads > 0 {
        output.hint("`fetchloom cache gc --now` also frees unfinished downloads no fetchloom is running; those start over next time");
    }
    output.answer_untold(&cache.root().display().to_string());
    Ok(Info {
        path: cache.root().to_path_buf(),
        usage,
        budget,
        projects,
        waiting,
        unfinished_downloads: unfinished.downloads,
        unfinished_bytes: unfinished.bytes,
    })
}

/// warns of the cached files at `exposed`, which every user may write, though fetchloom keeps each
/// read-only: through them others can change the files projects placed from them.
fn tell_writable(output: &Output, exposed: &[PathBuf]) {
    let said = match exposed {
        [] => return,
        [one] => format!(
            "the file {} in the cache lets every user of this machine write to it, so they can change the files projects placed from it",
            one.display()
        ),
        [first, ..] => format!(
            "{} in the cache, as {}, let every user of this machine write to them, so they can change the files projects placed from them",
            units::count(exposed.len() as u64, "file", "files"),
            first.display()
        ),
    };
    output.warn_with(
        Warning::CacheForeign,
        &said,
        "take away their write permission, as fetchloom keeps every file there read-only",
    );
}

/// the downloads runs left unfinished, and which of them a later run continues: those with the
/// record of their bytes, and any other only when it knows the file's hash, which then checks it.
fn unfinished_line(unfinished: &Unfinished) -> String {
    let what = format!(
        "{} · {}",
        units::count(unfinished.downloads, "download", "downloads"),
        units::size(unfinished.bytes)
    );
    let unnoted = unfinished.downloads.saturating_sub(unfinished.recorded);
    match (unfinished.recorded, unnoted) {
        (_, 0) => format!("{what}, which a later run continues where the server allows"),
        (0, _) => format!(
            "{what}, which a later run starts again from the first byte unless it knows the file's hash"
        ),
        (recorded, 1) => format!(
            "{what}; a later run continues {recorded} where the server allows, and starts the other again from its first byte unless it knows its hash"
        ),
        (recorded, unnoted) => format!(
            "{what}; a later run continues {recorded} where the server allows, and starts the other {unnoted} again from the first byte unless it knows their hashes"
        ),
    }
}

/// what the cache's files take of `budget`, on disk where the system says, and by how much they
/// pass it.
fn budget_line(bytes: u64, budget: u64) -> String {
    let used = format!("{} of {} used", units::size(bytes), units::size(budget));
    match bytes.checked_sub(budget).filter(|past| *past > 0) {
        Some(past) => format!(
            "{used} · {} past it, so a sync that downloads more asks first",
            units::size(past)
        ),
        None => used,
    }
}

fn tell_projects(output: &Output, projects: &[PathBuf]) {
    const LISTED: usize = 3;
    if projects.len() <= LISTED {
        for project in projects {
            output.label("Used by", &units::path(project));
        }
        return;
    }
    let count = units::count(projects.len() as u64, "project", "projects");
    if output.shows_details() {
        output.label("Used by", &count);
        for project in projects {
            output.detail(&units::path(project));
        }
    } else {
        output.label(
            "Used by",
            &format!("{count} · `fetchloom cache -v` lists them"),
        );
    }
}

/// says that the files of each of `waiting`, a dataset that waits for its file list, count as
/// unneeded, as gc counts them.
fn tell_waiting(output: &Output, waiting: &[WaitingIn]) {
    let text = match waiting {
        [] => return,
        [one] => format!(
            "{} in {} waits for its file list, so the cache counts none of its files as needed and gc may free them, which a sync there downloads again once the list is back",
            one.dataset,
            units::path(&one.project)
        ),
        [first, ..] => format!(
            "{} datasets wait for their file lists, as {} in {}, so the cache counts none of their files as needed and gc may free them, which a sync downloads again once each list is back",
            waiting.len(),
            first.dataset,
            units::path(&first.project)
        ),
    };
    output.hint(&text);
}

fn tell_usage(output: &Output, usage: &Usage, projects: usize, blocked: bool) {
    if usage.objects == 0 {
        output.done("Ok", "the cache has no data files");
        return;
    }
    if projects > 0 {
        output.label("Needed", &needed_line(usage, projects));
    }
    let unneeded = unneeded_line(usage, blocked);
    if let Some(line) = &unneeded {
        output.label("Unneeded", line);
    }
    if let Some(line) = copies_line(usage) {
        output.label("Copies", &line);
    }
    if unneeded.is_none() {
        output.done("Ok", "every cached file is needed by a project");
    } else if !blocked && (usage.unneeded == 0 || usage.freeable + usage.young > 0) {
        // gc frees nothing of what folders outside the cache link
        output.hint("`fetchloom cache gc` frees what no project needs once it is a day old; `fetchloom cache gc --now` frees it now");
    }
    if usage.unlinked_copies > 0 {
        output.hint("`fetchloom sync` in a project links its copies to the cache again");
    }
}

fn needed_line(usage: &Usage, projects: usize) -> String {
    let mut line = format!(
        "{} by {}",
        units::size(usage.needed),
        units::count(projects as u64, "project", "projects")
    );
    if usage.linked > 0 {
        line.push_str(" · ");
        line.push_str(&linked_part(usage.linked, usage.needed));
    }
    line
}

/// what no project needs, and what gc frees of it; `blocked` when a project could not be read,
/// which stops gc before it frees anything.
fn unneeded_line(usage: &Usage, blocked: bool) -> Option<String> {
    if usage.unneeded_objects == 0 {
        return None;
    }
    if blocked {
        return Some(format!(
            "{} · needed by none of the projects fetchloom could read; gc frees none of it while a project cannot be read",
            units::size(usage.unneeded)
        ));
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
    /// of the objects removed, those imported from files, which no source can give back.
    imported_objects: u64,
    imported_bytes: u64,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    dry_run: bool,
    /// with `--dry-run`, every path gc would remove.
    #[serde(skip_serializing_if = "Option::is_none")]
    changes: Option<Removals>,
}

#[derive(Serialize)]
struct Removals {
    remove: Vec<PathBuf>,
}

/// frees what no project needs. a dry run sweeps a cache that only records removals, so it
/// decides as a real sweep does, but takes no lock.
pub(super) fn gc(context: &Context, now: bool, dry_run: bool) -> Result<Collected, Error> {
    if let Some(root) = super::absent_cache("to free")? {
        let nothing = format!(
            "there is no cache at {} yet, so there is nothing to free",
            units::path(&root)
        );
        if dry_run {
            super::project::dry_run_done(&context.output, Some(&nothing));
            return Ok(Collected {
                dry_run: true,
                changes: Some(Removals { remove: Vec::new() }),
                ..collected(Swept::default())
            });
        }
        context.output.done("Ok", &nothing);
        return Ok(collected(Swept::default()));
    }
    // what `--now` would free of imported files is asked about before gc holds the cache, and the
    // sweep frees only what the answer covers
    let agreed = if now && !dry_run {
        Some(ask_imported(context)?)
    } else {
        None
    };
    let cache = if dry_run {
        let cache = super::peek_cache()?;
        if !cache.root().exists() {
            return Ok(tell_dry_gc(context, Swept::default(), Vec::new()));
        }
        cache.pretending()
    } else {
        hold(context)?
    };
    let mut found = needed_with(&cache, None, &context.stop, true, false)?;
    let swept = sweep(context, &cache, &mut found, now, agreed.as_ref())?;
    if dry_run {
        return Ok(tell_dry_gc(context, swept, cache.pretended()));
    }
    tell_swept(context, swept, found.projects.len())
}

/// asks whether `gc --now` may free what was imported from files on this machine and no project
/// needs, read without holding the cache. the imported objects a yes, or `--yes`, covers.
fn ask_imported(context: &Context) -> Result<HashSet<Sha256>, Error> {
    let cache = super::peek_cache()?;
    let imported = cache.imported();
    if imported.is_empty() {
        return Ok(HashSet::new());
    }
    let found = needed_with(&cache, None, &context.stop, false, false)?;
    let freeing: Vec<(Sha256, u64)> = imported
        .into_iter()
        .filter(|(digest, _)| !found.needed.objects.contains(digest))
        .collect();
    let agreed = freeing.iter().map(|(digest, _)| *digest).collect();
    if freeing.is_empty() || context.yes {
        return Ok(agreed);
    }
    let what = format!(
        "{} · {}",
        units::count(freeing.len() as u64, "file", "files"),
        units::size(freeing.iter().map(|(_, size)| size).sum())
    );
    let (was, it) = if freeing.len() == 1 {
        ("was", "it")
    } else {
        ("were", "them")
    };
    let why =
        format!("{what} gc would free {was} imported from files, and no source can give {it} back");
    let cancel = format!("freeing {what} from the cache");
    match context
        .output
        .confirm(&format!("{why}. free {it} anyway?"))
        .map_err(|error| error.cancelling(&cancel))?
    {
        Some(true) => Ok(agreed),
        Some(false) => Err(Error::new(Kind::Unconfirmed, format!("did not free {what}"))
            .cancelled(cancel)
            .unchanged()),
        None => Err(
            Error::new(Kind::Unconfirmed, format!("{why}, so freeing {it} needs a yes")).help(
                format!(
                    "answer yes at a terminal, or add --yes once you keep {it} elsewhere; a dry run counts {it}, as in `fetchloom cache gc --now --dry-run`"
                ),
            ),
        ),
    }
}

/// the cache, held alone for gc once no other run uses it.
pub(super) fn hold(context: &Context) -> Result<Cache, Error> {
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

/// what a sweep freed, and the part of it imported from files, which no source can give back.
#[derive(Default)]
struct Swept {
    freed: Freed,
    imported_objects: u64,
    imported_bytes: u64,
}

/// frees what `found` says no project needs. with `agreed`, the imported objects a person agreed
/// to free, any other import stays, as one no project needed only once they were asked.
fn sweep(
    context: &Context,
    cache: &Cache,
    found: &mut Found,
    now: bool,
    agreed: Option<&HashSet<Sha256>>,
) -> Result<Swept, Error> {
    let grace = if now {
        std::time::Duration::ZERO
    } else {
        crate::cache::GRACE
    };
    let imported = cache.imported();
    if let Some(agreed) = agreed {
        let unasked = imported.keys().filter(|digest| !agreed.contains(*digest));
        found.needed.objects.extend(unasked);
    }
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
    // a pretending cache keeps what it would remove, and says so
    let pretended: HashSet<PathBuf> = cache.pretended().into_iter().collect();
    let mut swept = Swept {
        freed,
        ..Swept::default()
    };
    for (digest, size) in &imported {
        let path = cache.object_path(digest);
        if !found.needed.objects.contains(digest)
            && (pretended.contains(&path) || fs::symlink_metadata(&path).is_err())
        {
            swept.imported_objects += 1;
            swept.imported_bytes += size;
        }
    }
    cache.forget_imported();
    Ok(swept)
}

fn tell_swept(context: &Context, swept: Swept, projects: usize) -> Result<Collected, Error> {
    let output = &context.output;
    let freed = &swept.freed;
    for failure in &freed.not_removed {
        warn_failure(output, "could not read or remove", failure);
    }
    tell_forgotten(output, &freed.forgotten);
    let kept = kept(freed);
    if freed.removed_objects == 0 && freed.removed_leftovers == 0 && freed.not_removed.is_empty() {
        match &kept {
            Some(kept) => {
                output.done("Ok", "nothing to free yet");
                output.hint(kept);
            }
            None => output.done("Ok", "nothing to free"),
        }
        hint_forgotten(output, freed);
        return Ok(collected(swept));
    }
    tell_removed(output, "Removed", &swept);
    if !freed.not_removed.is_empty() {
        let error = Error::new(
            Kind::LocalIo,
            format!(
                "{} in the cache could not be read or removed",
                units::count(freed.not_removed.len() as u64, "item", "items")
            ),
        )
        .help(FAILED_ABOVE);
        return Err(error.with_report(&collected(swept)));
    }
    let projects = units::count(projects as u64, "project", "projects");
    output.done(
        "Cleaned",
        &format!("the cache, keeping files needed by {projects}"),
    );
    if let Some(kept) = &kept {
        output.hint(kept);
    }
    hint_forgotten(output, freed);
    Ok(collected(swept))
}

fn tell_removed(output: &Output, verb: &str, swept: &Swept) {
    let freed = &swept.freed;
    if freed.removed_objects > 0 {
        let imported = match swept.imported_bytes {
            0 if swept.imported_objects == 0 => String::new(),
            bytes => format!(
                ", {} of it imported from files no source can give back",
                units::size(bytes)
            ),
        };
        output.step(
            verb,
            &format!(
                "{} · {} from the cache{imported}",
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

fn collected(swept: Swept) -> Collected {
    Collected {
        freed: swept.freed,
        imported_objects: swept.imported_objects,
        imported_bytes: swept.imported_bytes,
        dry_run: false,
        changes: None,
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

fn tell_dry_gc(context: &Context, swept: Swept, remove: Vec<PathBuf>) -> Collected {
    let output = &context.output;
    let freed = &swept.freed;
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
    tell_removed(output, "Would remove", &swept);
    for path in &remove {
        output.detail(&path.display().to_string());
    }
    let nothing = match (
        freed.removed_objects == 0 && freed.removed_leftovers == 0,
        freed.kept_objects > 0,
    ) {
        (false, _) => None,
        (true, false) => Some("nothing to free"),
        (true, true) => Some("nothing to free yet"),
    };
    super::project::dry_run_done(output, nothing);
    if let Some(kept) = kept(freed) {
        output.hint(&kept);
    }
    Collected {
        dry_run: true,
        changes: Some(Removals { remove }),
        ..collected(swept)
    }
}

pub(super) fn verify(context: &Context) -> Result<Checked, Error> {
    if let Some(root) = super::absent_cache("to check")? {
        context.output.done(
            "Ok",
            &format!(
                "there is no cache at {} yet, so there is nothing to check",
                units::path(&root)
            ),
        );
        return Ok(Checked::default());
    }
    let cache = open_cache()?;
    let scan = cache.scan_objects(context.threads, &context.stop)?;
    let total: u64 = scan.objects.iter().map(|object| object.size).sum();
    let what = "cached data files";
    let bar = context.output.bar("Checking", what.to_owned());
    bar.start(Some(total), 0);
    let ticker = context
        .output
        .ticker("Checking", what.to_owned(), [Some(total)]);
    let threads = crate::threads::available_parallelism().map_or(1, std::num::NonZero::get);
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
        return Err(error.with_report(&checked));
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
        return Err(Error::new(
            Kind::LocalIo,
            format!(
                "checked {what}. {} could not be read",
                units::count(checked.unreadable.len() as u64, "path", "paths")
            ),
        )
        .help(FAILED_ABOVE));
    }
    // a file imported from a folder comes back from no source, only from a mirror or its folder
    let imported = cache.imported();
    let mut lost = false;
    for digest in &checked.changed {
        if !checked.stuck.contains(digest) {
            let object = cache.object_path(digest);
            let said = if imported.contains_key(digest) {
                lost = true;
                format!(
                    "{} has changed, and it was imported from files, which no source gives back",
                    object.display()
                )
            } else {
                format!(
                    "{} has changed, so fetchloom downloads it again the next time it is needed",
                    object.display()
                )
            };
            output.warn(Warning::CacheChanged, &said);
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
        let help = if !checked.unreadable.is_empty() {
            "close programs using the unreadable files or check their permissions, then run `fetchloom cache verify` again and `fetchloom sync` in each project"
        } else if lost {
            "run `fetchloom sync` in each project; it puts the pinned file back wherever a project's file is linked to a changed copy. a file imported from files comes back only from a folder mirror that holds it, or by running `fetchloom add <folder> --as <name>` again where the files are"
        } else {
            "run `fetchloom sync` in each project; it puts the pinned file back wherever a project's file is linked to a changed copy"
        };
        return Err(error.help(help));
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

/// the help of an error about paths gc or verify could not read or remove, which a warning above
/// names each of with what to do for it, so the error does not name them again.
const FAILED_ABOVE: &str = "address the filesystem errors above, then run the command again";

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
    let scan = cache.scan_objects(threads, stop).ok()?;
    unneeded_in(cache, &scan, now, threads, stop).map(|(freeable, _)| freeable)
}

/// what gc would free of the objects `scan` found in `cache` at `now`, as [`freeable`] reads it:
/// what `cache gc` frees, and what only `cache gc --now` does, as it is less than a day old.
pub(super) fn unneeded_in(
    cache: &Cache,
    scan: &Scan,
    now: SystemTime,
    threads: Threads,
    stop: &Stop,
) -> Option<(u64, u64)> {
    let found = needed_with(cache, None, stop, false, false).ok()?;
    if !found.complete {
        return None;
    }
    // what gc frees tells nothing of copies, so it reads no project's files
    let usage = Usage::measure(scan, &found.needed, &[], now, threads, stop).ok()?;
    Some((usage.freeable, usage.young))
}

/// sets the time itself, since writing an empty file again leaves its time alone on windows.
fn stamp(marker: &std::path::Path, now: SystemTime) -> io::Result<()> {
    crate::volume::open_unfollowed(marker, true)?.set_modified(now)
}

/// every object a registered project, or `current`, needs, as gc keeps them. it fails, as gc
/// stops, on a registration, data.toml or lock that is there but unreadable, so callers keep all.
pub(super) fn needed_by_all(
    cache: &Cache,
    current: Option<&Lock>,
    stop: &Stop,
) -> Result<Needed, Error> {
    Ok(needed_with(cache, current, stop, false, false)?.needed)
}

/// [`needed_by_all`] for `candidates`, archives a drop may remove: only these check the cache holds
/// every member, each member looked at once on `threads` workers however many projects pin it.
pub(in crate::cli) fn needed_for_drop(
    cache: &Cache,
    current: Option<&Lock>,
    candidates: &[Sha256],
    threads: Threads,
    stop: &Stop,
) -> Result<Needed, Error> {
    let focus = Focus::new(candidates, threads, None);
    Ok(needed_focused(cache, current, stop, false, false, Some(&focus))?.needed)
}

/// [`needed_for_drop`] with `current` standing for the project at `here` in place of the data.lock
/// it is registered with, as `fetchloom drop` frees its datasets before or without marking them.
pub(in crate::cli) fn needed_besides(
    cache: &Cache,
    (here, current): (&Path, &Lock),
    candidates: &[Sha256],
    threads: Threads,
    stop: &Stop,
) -> Result<Needed, Error> {
    let focus = Focus::new(candidates, threads, Some(here));
    Ok(needed_focused(cache, Some(current), stop, false, false, Some(&focus))?.needed)
}

/// the archives a drop asks about, and which of their members the cache was found to hold.
struct Focus {
    candidates: HashSet<Sha256>,
    present: RefCell<HashMap<Sha256, bool>>,
    threads: Threads,
    /// the project whose needs the walk's `current` lock gives, whose registered lock it skips.
    here: Option<PathBuf>,
}

impl Focus {
    fn new(candidates: &[Sha256], threads: Threads, here: Option<&Path>) -> Focus {
        Focus {
            candidates: candidates.iter().copied().collect(),
            present: RefCell::new(HashMap::new()),
            threads,
            here: here.map(Path::to_path_buf),
        }
    }

    /// whether the walk's `current` lock stands for the project at `root`.
    fn stands_for(&self, root: &Path) -> bool {
        self.here.as_deref().is_some_and(|here| {
            here == root
                || fs::canonicalize(here)
                    .is_ok_and(|here| fs::canonicalize(root).is_ok_and(|root| here == root))
        })
    }

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

struct Found {
    needed: Needed,
    projects: Vec<PathBuf>,
    stale_projects: Vec<PathBuf>,
    /// false when a registered project's data.toml was missing, so `needed` lacks what it needs.
    complete: bool,
    /// for a report, each registered project whose data.lock could not be read, as one
    /// mid-merge, which only gc must stop at.
    unread: Vec<Error>,
    /// the fetched datasets of registered projects that wait for their file lists, which
    /// `needed` cannot count.
    waiting: Vec<WaitingIn>,
}

/// what registered projects, and `current`, need. unreadable registrations, manifests and locks
/// stop gc rather than lose protection. `collect_stale` queues roots whose data.toml is gone for
/// the sweep, so inspection does not discard removal errors or delete records early. a `report`
/// notes a project it cannot read in [`Found::unread`] and goes on.
fn needed_with(
    cache: &Cache,
    current: Option<&Lock>,
    stop: &Stop,
    collect_stale: bool,
    report: bool,
) -> Result<Found, Error> {
    needed_focused(cache, current, stop, collect_stale, report, None)
}

/// [`needed_with`], narrowed to a drop's candidates by `focus`.
fn needed_focused(
    cache: &Cache,
    current: Option<&Lock>,
    stop: &Stop,
    collect_stale: bool,
    report: bool,
    focus: Option<&Focus>,
) -> Result<Found, Error> {
    let mut needed = Needed::default();
    let mut projects = Vec::new();
    let mut stale_projects = Vec::new();
    let mut complete = true;
    let mut unread = Vec::new();
    let mut waiting = Vec::new();
    let mut counted = HashSet::new();
    for root in cache.registered_projects(stop)? {
        stop.check()?;
        let project = Project::at(root);
        let text = match manifest_of(cache, &project) {
            Ok(Some(text)) => text,
            Ok(None) => {
                if collect_stale {
                    stale_projects.push(project.root);
                }
                complete = false;
                continue;
            }
            Err(error) if report => {
                unread.push(error);
                complete = false;
                continue;
            }
            Err(error) => return Err(error),
        };
        // windows registers a folder under each spelling it was reached by, another case, a short
        // name or a junction, so the folder itself counts once; a gone one's records all go
        let folder = fs::canonicalize(&project.root).unwrap_or_else(|_| project.root.clone());
        if !counted.insert(folder) {
            continue;
        }
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
                Some(help) if help == lock::recovery() => {
                    format!("in that project, {}", help.replace(&here, &there))
                }
                Some(help) => help.replace(&here, &there),
                None => "fix that project's data.lock, then try again".to_owned(),
            };
            let error = Error::new(
                err.kind,
                format!(
                    "{} cannot be read, so the cache stays as it is",
                    project.root.join(lock::FILE).display()
                ),
            )
            .detail(err.message)
            .help(help);
            theirs(&project.root, error)
        });
        let lock = match lock {
            Ok(lock) => lock,
            Err(error) if report => {
                unread.push(error);
                complete = false;
                continue;
            }
            Err(error) => return Err(error),
        };
        // reading none of the state's records skips nearly all of it
        let (state, _) = project.state_within(&[], &|_| false);
        // a dataset `fetchloom drop` freed there is needed there no more
        let lock = lock
            .map(|lock| super::drop::kept_here(lock, &state.dropped))
            .filter(|_| !focus.is_some_and(|focus| focus.stands_for(&project.root)));
        if let Some(lock) = &lock {
            add_with_stop(&mut needed, cache, lock, kept.as_ref(), focus, stop)?;
            // an import's files are all kept while it waits, so only a fetched one goes uncounted
            waiting.extend(
                lock.still_waiting()
                    .filter(|waits| !crate::source::local::is_imported(&waits.pin().reference))
                    .map(|waits| WaitingIn {
                        project: project.root.clone(),
                        dataset: waits.name.clone(),
                    }),
            );
        }
        projects.push(project.root);
    }
    if let Some(lock) = current {
        add_with_stop(&mut needed, cache, lock, None, focus, stop)?;
    }
    Ok(Found {
        needed,
        projects,
        stale_projects,
        complete,
        unread,
        waiting,
    })
}

/// the text of the data.toml of `project`, a registered one, or none when it is gone, as from a
/// folder that moved: an error when it cannot be read, or when the project is out of reach. it is
/// read as [`Project::read_own`] reads it: a commit pulled into a clone, or anyone sharing the
/// cache in their own project, can make it a link, a fifo or a huge file, which every walk of the
/// projects reads: gc, the cache report, and every unpack in any project.
fn manifest_of(cache: &Cache, project: &Project) -> Result<Option<Vec<u8>>, Error> {
    let manifest = project.manifest_path();
    match project.read_own(crate::project::manifest::FILE) {
        Ok(text) => Ok(Some(text)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            if cache.out_of_reach(&project.root) {
                Err(out_of_reach(cache, &project.root))
            } else {
                Ok(None)
            }
        }
        Err(err) => {
            let error = Error::io(format!("cannot read {}", manifest.display()), &err);
            Err(theirs(&project.root, error))
        }
    }
}

/// the refusal of the registered project at `root`, which is not there and whose disk may only be
/// not connected: gc frees nothing it may need.
fn out_of_reach(cache: &Cache, root: &Path) -> Error {
    Error::new(
        Kind::LocalIo,
        format!(
            "cannot reach {}: the disk it was on is not there, as when a drive is not connected",
            root.display()
        ),
    )
    .help(format!(
        "connect that disk, then run the command again; until then gc keeps every file in the cache. if the project is gone for good, delete {}, its record in the cache",
        cache.registration(root).display()
    ))
}

/// `error`, from reading the registered project at `root`, with help that names whose project it
/// is when it is another user's, who alone can fix it, as in a lab's shared cache.
fn theirs(root: &Path, error: Error) -> Error {
    #[cfg(unix)]
    if let Some(owner) = owner_besides(root, crate::cache::run_as()) {
        let owner = crate::cache::said_user(owner);
        return error.help(format!(
            "{} belongs to {owner}: ask them to let your group read its data.toml, data.lock and \
             data.lock.d, or to fix them; until then gc keeps every file in the cache",
            root.display()
        ));
    }
    #[cfg(not(unix))]
    let _ = root;
    error
}

/// who owns the project at `root`, when it is not `me`: the owner of its data.toml, or of the
/// nearest folder on the way to it whose details this user may read, as a private folder lets
/// others read nothing inside it.
#[cfg(unix)]
fn owner_besides(root: &Path, me: u32) -> Option<u32> {
    let manifest = root.join(crate::project::manifest::FILE);
    let owner = std::iter::once(manifest.as_path())
        .chain(root.ancestors())
        .find_map(crate::cache::owner_of)?;
    (owner != me).then_some(owner)
}

/// adds what one lock needs. an archive keeps its own bytes while this machine never unpacked it
/// or lost a file of it that data.toml keeps, since unpacking again is the only way back.
/// a fetched dataset waiting for its list keeps nothing, as its source has it, and an import
/// keeps what was imported, which [`add_waiting_imports`] adds.
fn add_with_stop(
    needed: &mut Needed,
    cache: &Cache,
    lock: &Lock,
    manifest: Option<&Manifest>,
    focus: Option<&Focus>,
    stop: &Stop,
) -> Result<(), Error> {
    for dataset in &lock.datasets {
        let filters = manifest
            .and_then(|manifest| manifest.datasets.get(&dataset.name))
            .map(|entry| &entry.source.members);
        for pinned in archives::pinned(&dataset.files) {
            stop.check()?;
            let archive = match pinned {
                Pinned::File(file) => {
                    needed.objects.insert(file.sha256);
                    continue;
                }
                Pinned::Archive(archive) => archive,
            };
            let Some(members) = archive.recorded(cache) else {
                needed
                    .objects
                    .extend(archive.files().iter().map(|file| file.sha256));
                continue;
            };
            needed.unpacked.insert(archive.key());
            if let Some(tree) = &archive.file.unpacks_to {
                needed.unpacked.insert(crate::cache::tree_key(tree));
            }
            needed
                .archives
                .extend(archive.files().into_iter().map(|file| file.sha256));
            let filter = filters.and_then(|filters| filters.get(&archive.file.path));
            let kept: Vec<&Member> = members
                .iter()
                .filter(|member| filter.is_none_or(|filter| filter.keeps(&member.path)))
                .collect();
            for member in &kept {
                stop.check()?;
                needed.objects.insert(member.sha256);
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
                needed
                    .objects
                    .extend(archive.files().iter().map(|file| file.sha256));
            }
        }
    }
    add_stated(needed, cache, lock, stop)?;
    add_waiting_imports(needed, cache, lock, stop)?;
    stop.check()
}

/// adds every object imported from files while `lock` has an import waiting for its list, as on a
/// branch where data.lock.d is untracked: no source gives one back, and which are its own is
/// unknown until its list is back.
fn add_waiting_imports(
    needed: &mut Needed,
    cache: &Cache,
    lock: &Lock,
    stop: &Stop,
) -> Result<(), Error> {
    let waits = lock
        .still_waiting()
        .any(|waiting| crate::source::local::is_imported(&waiting.pin().reference));
    if waits {
        for digest in cache.imported().into_keys() {
            stop.check()?;
            needed.objects.insert(digest);
        }
    }
    Ok(())
}

/// adds what a first sync cut short downloaded of each dataset `add --no-sync` pinned in `lock`:
/// a file found by the SHA-256 its source states, or by the MD5 the cache recorded for its link.
fn add_stated(needed: &mut Needed, cache: &Cache, lock: &Lock, stop: &Stop) -> Result<(), Error> {
    for stated in &lock.stated {
        for file in &stated.files {
            stop.check()?;
            let recorded = || {
                let md5 = file.md5.as_ref()?;
                cache.stated_md5(md5, &file.url).map(|(digest, _)| digest)
            };
            if let Some(digest) = file.sha256.or_else(recorded) {
                needed.objects.insert(digest);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::Member;
    use crate::cache::tests::store;
    use crate::output::{Level, Style};
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

    /// a sweep that could not remove a file says so though its report has no JSON form, as where
    /// the cache's folder is named in bytes that are not UTF-8.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_failed_sweep_says_what_failed_though_its_report_has_no_json() {
        use std::os::unix::ffi::OsStrExt;
        let folder = tempfile::tempdir().unwrap();
        let root = folder.path().join(std::ffi::OsStr::from_bytes(b"caf\xe9"));
        let cache = Cache::open(root).unwrap();
        let digest = crate::digest::sha256(b"abc");
        fs::create_dir_all(cache.object_path(&digest).join("inside")).unwrap();
        let mut swept = Swept::default();
        cache.free_input(&digest, &mut swept.freed.not_removed);
        assert_eq!(swept.freed.not_removed.len(), 1);
        let style = Style {
            color: false,
            unicode: true,
            interactive: false,
            watched: false,
            links: false,
            accessible: false,
        };
        let context = super::super::Context::new(Output::new(style, Level::Quiet, false, false));
        let Err(error) = tell_swept(&context, swept, 0) else {
            panic!("the sweep failed");
        };
        assert_eq!(
            error.message,
            "1 item in the cache could not be read or removed"
        );
        // each item's warning names it and what to do, so the error says it once more for none
        assert_eq!(error.details, Vec::<String>::new());
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
                at: None,
                extract: true,
                decompress: false,
                web_page: false,
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
        add_with_stop(&mut whole, &cache, &lock, None, None, &Stop::default()).unwrap();
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
        add_with_stop(&mut missing, &cache, &lock, None, None, &Stop::default()).unwrap();
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
        add_with_stop(&mut whole, &cache, &lock, None, None, &Stop::default()).unwrap();
        assert!(whole.objects.contains(&second.digest));
        assert!(
            !whole.objects.contains(&archive.digest),
            "its files stand for it"
        );

        cache.drop_object(&second.digest).unwrap();
        let mut missing = Needed::default();
        add_with_stop(&mut missing, &cache, &lock, None, None, &Stop::default()).unwrap();
        assert!(
            missing.objects.contains(&archive.digest),
            "unpacking it again is the only way back to b.txt"
        );
    }

    /// a member data.toml excludes is not needed, and once it is gone the archive is not needed
    /// either; with no data.toml read, the archive is kept.
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
        let read = Some(&manifest);
        add_with_stop(&mut kept, &cache, &lock, read, None, &Stop::default()).unwrap();
        assert!(kept.objects.contains(&first.digest));
        assert!(
            !kept.objects.contains(&second.digest),
            "no project places it"
        );

        cache.drop_object(&second.digest).unwrap();
        let mut freed = Needed::default();
        add_with_stop(&mut freed, &cache, &lock, read, None, &Stop::default()).unwrap();
        assert!(
            !freed.objects.contains(&archive.digest),
            "a.txt, the only file the project places, is cached"
        );
        let mut unread = Needed::default();
        add_with_stop(&mut unread, &cache, &lock, None, None, &Stop::default()).unwrap();
        assert!(unread.objects.contains(&archive.digest));
    }

    /// a dataset `add --no-sync` pinned keeps what its first sync downloaded before it was cut
    /// short: a file by the SHA-256 its source states, or by the MD5 the cache recorded of the
    /// bytes its link served, for the sync that comes back to it.
    #[test]
    fn what_a_cut_short_sync_downloaded_of_a_stated_dataset_is_needed() {
        use crate::project::lock::{Stated, StatedFile, Words};
        let folder = tempfile::tempdir().unwrap();
        let cache = Cache::open(folder.path().join("cache")).unwrap();
        let iris = store(&cache, b"length,width\n");
        let species = store(&cache, b"species\n");
        let md5 = crate::digest::Md5::from_hex(&"5d".repeat(16)).unwrap();
        let link = |name: &str| format!("https://example.org/{name}");
        let stated = crate::cache::Stated {
            digest: species.digest,
            size: species.size,
            version: None,
        };
        cache
            .save_stated_md5(&md5, &link("species.csv"), &stated)
            .unwrap();
        let file = |name: &str, size, sha256, md5| StatedFile {
            path: name.to_owned(),
            size,
            sha256,
            md5,
            url: link(name),
        };
        let lock = Lock {
            stated: vec![Stated {
                pin: Locked {
                    name: "two".to_owned(),
                    reference: link(""),
                    spec: String::new(),
                    at: None,
                    extract: true,
                    decompress: false,
                    web_page: false,
                    resolved: link(""),
                    title: None,
                    license: None,
                    doi: None,
                    access: crate::source::Access::Unknown,
                    timestamp: String::new(),
                    files: Vec::new(),
                },
                files: vec![
                    file(
                        "gone.csv",
                        12,
                        Some(crate::digest::sha256(b"never came")),
                        None,
                    ),
                    file("iris.csv", iris.size, Some(iris.digest), None),
                    file("species.csv", species.size, None, Some(md5)),
                ],
                written: Words::default(),
            }],
            ..Lock::default()
        };

        let mut needed = Needed::default();
        add_with_stop(&mut needed, &cache, &lock, None, None, &Stop::default()).unwrap();

        assert!(needed.objects.contains(&iris.digest), "by its SHA-256");
        assert!(needed.objects.contains(&species.digest), "by its MD5");
    }

    /// a project another member keeps to themselves, as in a private folder of a lab's shared
    /// scratch, is told and left out of the cache's report, which goes on; gc still stops before
    /// it frees anything, since what that project needs is unknown.
    #[cfg(unix)]
    #[test]
    fn a_project_that_cannot_be_read_is_told_and_gc_keeps_everything() {
        use std::os::unix::fs::PermissionsExt;
        if rustix::process::geteuid().is_root() {
            return crate::test_skips::skip("unprivileged", "root ignores file permissions");
        }
        let folder = tempfile::tempdir().unwrap();
        let cache = Cache::open(folder.path().join("cache")).unwrap();
        let project = folder.path().join("theirs");
        fs::create_dir(&project).unwrap();
        fs::write(project.join("data.toml"), "").unwrap();
        cache.register(&project).unwrap();
        fs::set_permissions(&project, fs::Permissions::from_mode(0o000)).unwrap();
        let report = needed_with(&cache, None, &Stop::default(), false, true);
        let gc = needed_with(&cache, None, &Stop::default(), true, false);
        fs::set_permissions(&project, fs::Permissions::from_mode(0o755)).unwrap();

        let report = report.unwrap();
        let unread: Vec<&str> = report
            .unread
            .iter()
            .map(|error| error.message.as_str())
            .collect();
        let message = format!(
            "cannot read {}: permission was denied",
            project.join("data.toml").display()
        );
        assert_eq!(unread, [message.as_str()]);
        assert!(!report.complete);
        assert_eq!(gc.map(drop).map_err(|error| error.message), Err(message));
    }

    /// a dataset imported from files on some machine, whose list data.lock.d lacks for now, as on
    /// a branch where the list is untracked, keeps every imported object: no source gives one back,
    /// and which are its own is unknown until its list is back.
    #[test]
    fn a_waiting_import_keeps_every_imported_object() {
        let folder = tempfile::tempdir().unwrap();
        let cache = Cache::open(folder.path().join("cache")).unwrap();
        let imported = store(&cache, b"measured\n");
        cache.mark_imported(&[imported.digest]).unwrap();
        let fetched = store(&cache, b"fetched\n");
        let project = folder.path().join("p");
        fs::create_dir(&project).unwrap();
        fs::write(
            project.join("data.toml"),
            "[datasets.meas]\nref = \"local\"\n",
        )
        .unwrap();
        let listed = "ab".repeat(32);
        fs::write(
            project.join("data.lock"),
            format!(
                "version = 1\n\n[[dataset]]\nname = \"meas\"\nref = \"local\"\nspec = \"sha256:{}\"\nresolved = \"local\"\nlocked = \"2026-11-15T23:47:00Z\"\nfiles_in = [\"data.lock.d/meas.{}.tsv.zst\"]\nfile_count = 1\nfile_bytes = 9\nfiles_sha256 = \"sha256:{listed}\"\n",
                "cd".repeat(32),
                &listed[..12]
            ),
        )
        .unwrap();
        cache.register(&project).unwrap();

        let needed = needed_by_all(&cache, None, &Stop::default()).unwrap();

        assert!(
            needed.objects.contains(&imported.digest),
            "the import's own"
        );
        assert!(
            !needed.objects.contains(&fetched.digest),
            "only what was imported"
        );
    }

    /// a registered project gone from its folder whose disk the folder above it is not on, as
    /// when the drive it was on is not connected, is out of reach, not gone: gc keeps it and every
    /// file in the cache, where it would forget it and free what only it needed.
    #[test]
    fn a_project_whose_disk_is_not_there_is_out_of_reach() {
        let folder = tempfile::tempdir().unwrap();
        let cache = Cache::open(folder.path().join("cache")).unwrap();
        let root = folder.path().join("drive").join("project");
        fs::create_dir_all(&root).unwrap();
        cache.register(&root).unwrap();
        // as a project registers from a disk with a number of its own, which the drive's
        // folder, now empty, is not on
        let record = fs::read_dir(cache.root().join("projects"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let mut written: serde_json::Value =
            serde_json::from_slice(&fs::read(&record).unwrap()).unwrap();
        written["disk"] = serde_json::json!(1);
        crate::volume::set_read_only(&record, false).unwrap();
        fs::write(&record, serde_json::to_vec(&written).unwrap()).unwrap();
        fs::remove_dir_all(folder.path().join("drive")).unwrap();

        let walked = needed_with(&cache, None, &Stop::default(), true, false);

        let refused = walked.map(|found| found.stale_projects).unwrap_err();
        assert!(
            refused
                .message
                .starts_with(&format!("cannot reach {}", root.display())),
            "{}",
            refused.message
        );
        assert!(record.exists(), "its record stays");
    }

    /// one folder registered under two spellings, as windows registers a project reached by
    /// another case, a short name or a junction, counts as one project, and once its data.toml is
    /// gone gc forgets both records.
    #[test]
    fn a_project_registered_under_two_spellings_counts_once() {
        let folder = tempfile::tempdir().unwrap();
        let cache = Cache::open(folder.path().join("cache")).unwrap();
        let root = folder.path().join("project");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("data.toml"), "").unwrap();
        #[cfg(unix)]
        let other = {
            let link = folder.path().join("link");
            std::os::unix::fs::symlink(&root, &link).unwrap();
            link
        };
        #[cfg(windows)]
        let other = folder.path().join("PROJECT");
        cache.register(&root).unwrap();
        cache.register(&other).unwrap();
        assert_eq!(
            cache.registered_projects(&Stop::default()).unwrap().len(),
            2
        );

        let found = needed_with(&cache, None, &Stop::default(), true, false).unwrap();
        assert_eq!(found.projects.len(), 1, "{:?}", found.projects);

        fs::remove_file(root.join("data.toml")).unwrap();
        let gone = needed_with(&cache, None, &Stop::default(), true, false).unwrap();
        assert_eq!(gone.stale_projects.len(), 2, "{:?}", gone.stale_projects);
    }

    /// a registered project's data.toml that a clone or someone sharing the cache made a fifo, a
    /// link or a huge file is refused at once by every walk of the projects, as gc's, never waited
    /// on for a writer, read without end or followed to the file a link names.
    #[cfg(unix)]
    #[test]
    fn a_registered_data_toml_that_is_no_plain_file_is_refused_at_once() {
        let folder = tempfile::tempdir().unwrap();
        let cache = Cache::open(folder.path().join("cache")).unwrap();
        let project = folder.path().join("clone");
        fs::create_dir(&project).unwrap();
        cache.register(&project).unwrap();
        let manifest = project.join("data.toml");
        let walk = || {
            needed_by_all(&cache, None, &Stop::default())
                .map(drop)
                .map_err(|error| error.message)
        };
        let special = Err(format!(
            "cannot read {}: it is a special file, not a file",
            manifest.display()
        ));
        let linked = Err(format!(
            "cannot read {}: it is a link, and fetchloom does not follow it",
            manifest.display()
        ));

        let made = std::process::Command::new("mkfifo")
            .arg(&manifest)
            .status()
            .unwrap();
        assert!(made.success());
        // a writer answers a reader that waits for one, so an open that waits still ends
        let path = manifest.clone();
        let writer = std::thread::spawn(move || {
            let mut opened = fs::OpenOptions::new().write(true).open(path).unwrap();
            // a reader that refused the fifo is gone, so the write may find none
            let _ = io::Write::write_all(&mut opened, b"[datasets]\n");
        });
        let fifo = walk();
        let flags = rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::NONBLOCK;
        let released = rustix::fs::open(&manifest, flags, rustix::fs::Mode::empty()).unwrap();
        writer.join().unwrap();
        drop(released);
        assert_eq!(fifo, special, "a fifo");

        fs::remove_file(&manifest).unwrap();
        std::os::unix::fs::symlink("/dev/zero", &manifest).unwrap();
        assert_eq!(walk(), linked, "a link to /dev/zero");

        fs::remove_file(&manifest).unwrap();
        let elsewhere = folder.path().join("data.toml");
        fs::write(&elsewhere, "[datasets]\n").unwrap();
        std::os::unix::fs::symlink(&elsewhere, &manifest).unwrap();
        assert_eq!(walk(), linked, "a link to a data.toml");

        fs::remove_file(&manifest).unwrap();
        fs::File::create(&manifest)
            .unwrap()
            .set_len((256 << 20) + 1)
            .unwrap();
        let refused = walk().unwrap_err();
        assert!(refused.contains("larger than"), "{refused}");
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
            assert_eq!(unneeded_line(&usage, false).as_deref(), Some(said));
        }
        assert_eq!(
            unneeded_line(&Usage::default(), false),
            None,
            "all of it is needed"
        );
        let usage = Usage {
            unneeded: 1024,
            freeable: 1024,
            unneeded_objects: 1,
            ..Usage::default()
        };
        assert_eq!(
            unneeded_line(&usage, true).as_deref(),
            Some(
                "1.0 KiB · needed by none of the projects fetchloom could read; gc frees none of \
                 it while a project cannot be read"
            ),
            "gc stops at a project it cannot read"
        );
    }

    /// the owner of a project in a folder only its owner may enter is read from that folder, and
    /// named only when it is someone else.
    #[cfg(unix)]
    #[test]
    fn a_project_another_user_keeps_is_named_with_its_owner() {
        use std::os::unix::fs::PermissionsExt;
        if rustix::process::geteuid().is_root() {
            return crate::test_skips::skip("unprivileged", "root ignores file permissions");
        }
        let folder = tempfile::tempdir().unwrap();
        let private = folder.path().join("bob");
        let project = private.join("p");
        fs::create_dir_all(&project).unwrap();
        fs::set_permissions(&private, fs::Permissions::from_mode(0o000)).unwrap();
        let me = rustix::process::geteuid().as_raw();
        let mine = owner_besides(&project, me);
        let others = owner_besides(&project, me + 1);
        fs::set_permissions(&private, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(mine, None, "this user's own project");
        assert_eq!(
            others,
            Some(me),
            "read from the folder only its owner may enter"
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
