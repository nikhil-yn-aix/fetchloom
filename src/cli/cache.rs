//! `fetchloom cache`: where the cache is and what it holds. `cache gc` frees what no project needs,
//! and `cache verify` hashes every object again.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::Serialize;

use super::{Context, open_cache};
use crate::cache::{Access, Cache, Checked, Failure, Freed, Needed, Usage};
use crate::error::{Error, Kind};
use crate::output::Output;
use crate::project::Project;
use crate::project::lock::{self, Lock};
use crate::project::manifest::{Entry, Manifest};
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
    /// the bytes those downloads hold.
    unfinished_bytes: u64,
}

pub(super) fn info(context: &Context) -> Result<Info, Error> {
    let cache = open_cache()?;
    let Found {
        needed, projects, ..
    } = needed_with(&cache, None, &context.stop, false)?;
    let scan = cache.scan_objects(&context.stop)?;
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
            cache.root().display(),
            units::count(usage.objects, "file", "files"),
            units::size(usage.bytes)
        ),
    );
    for project in &projects {
        output.step("Used by", &project.display().to_string());
    }
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

/// where the cache's space goes: what the `projects` registered need, what no project needs and
/// what gc frees of it, and the copies of cached files projects hold.
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

/// what the projects need, and how much of it deleting would not free.
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

/// what no project needs and when gc frees it, if any object is unneeded.
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

/// says that `linked` bytes of `whole` have a name outside the cache too.
fn linked_part(linked: u64, whole: u64) -> String {
    format!(
        "{} is linked outside the cache too, so deleting it would free nothing",
        of_it(linked, whole)
    )
}

/// `part` of `whole` in words: all of it, or how much of it.
fn of_it(part: u64, whole: u64) -> String {
    if part == whole {
        "all of it".to_owned()
    } else {
        format!("{} of it", units::size(part))
    }
}

/// the files projects hold that are copies of cached files, if any: those placed as copies or
/// clones, and those whose link to the cache a copy of their project broke.
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
}

#[derive(Serialize)]
struct Removals {
    remove: Vec<PathBuf>,
}

/// frees what no project needs. a dry run sweeps a cache that records each removal instead of
/// making it, so it decides exactly as a real sweep does. it takes no lock and so never waits,
/// while a real sweep waits until no other run uses the cache.
pub(super) fn gc(context: &Context, now: bool, dry_run: bool) -> Result<Collected, Error> {
    let cache = if dry_run {
        let cache = super::peek_cache()?;
        if !cache.root().exists() {
            return Ok(tell_dry_gc(context, Freed::default(), Vec::new()));
        }
        cache.pretending()
    } else {
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
        )?
    };
    let found = needed_with(&cache, None, &context.stop, true)?;
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
        &context.stop,
    )?;
    if dry_run {
        return Ok(tell_dry_gc(context, freed, cache.pretended()));
    }
    let output = &context.output;
    for failure in &freed.not_removed {
        warn_failure(output, "could not read or remove", failure);
    }
    tell_forgotten(output, &freed.forgotten);
    let kept = kept(&freed);
    if freed.removed_objects == 0 && freed.removed_leftovers == 0 && freed.not_removed.is_empty() {
        match &kept {
            Some(kept) => {
                output.done("Ok", "nothing to free yet");
                output.hint(kept);
            }
            None => output.done("Ok", "nothing to free"),
        }
        hint_forgotten(output, &freed);
        return Ok(collected(freed));
    }
    tell_removed(output, "Removed", "Removed", &freed);
    if !freed.not_removed.is_empty() {
        let result = serde_json::to_value(&freed).map_err(|err| {
            Error::new(
                Kind::LocalIo,
                format!("cannot format the cache result: {err}"),
            )
        })?;
        return Err(with_failures(
            Error::new(
                Kind::LocalIo,
                format!(
                    "{} in the cache could not be read or removed",
                    units::count(freed.not_removed.len() as u64, "item", "items")
                ),
            ),
            &freed.not_removed,
        )
        .with_result(result));
    }
    output.done(
        "Cleaned",
        &format!(
            "the cache, keeping files needed by {}",
            units::count(found.projects.len() as u64, "project", "projects")
        ),
    );
    if let Some(kept) = &kept {
        output.hint(kept);
    }
    hint_forgotten(output, &freed);
    Ok(collected(freed))
}

/// the lines for the objects and the leftovers a sweep removed, or would remove, under `objects`
/// and `leftovers` as their verbs.
fn tell_removed(output: &Output, objects: &str, leftovers: &str, freed: &Freed) {
    if freed.removed_objects > 0 {
        output.step(
            objects,
            &format!(
                "{} · {} from the cache",
                units::count(freed.removed_objects, "file", "files"),
                units::size(freed.removed_bytes)
            ),
        );
    }
    if freed.removed_leftovers > 0 {
        output.step(
            leftovers,
            &format!(
                "{} · {} of unfinished downloads and temporary files",
                units::count(freed.removed_leftovers, "leftover item", "leftover items"),
                units::size(freed.removed_leftover_bytes)
            ),
        );
    }
}

fn collected(freed: Freed) -> Collected {
    Collected {
        freed,
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

/// how a project gc forgot because it moved gets its files kept again.
fn hint_forgotten(output: &Output, freed: &Freed) {
    if !freed.forgotten.is_empty() {
        output.hint(
            "if a project moved, run `fetchloom sync` in its new folder, so gc keeps its files",
        );
    }
}

/// the line about unneeded files gc keeps for now, if any.
fn kept(freed: &Freed) -> Option<String> {
    (freed.kept_objects > 0).then(|| {
        format!(
            "{} no project needs is kept for a day in case you want it back; `fetchloom cache gc --now` frees it now",
            units::size(freed.kept_bytes)
        )
    })
}

/// says what a sweep that only pretended would have freed, naming each path with `-v`.
fn tell_dry_gc(context: &Context, freed: Freed, remove: Vec<PathBuf>) -> Collected {
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
    tell_removed(output, "Would free", "Would drop", &freed);
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
    if let Some(kept) = kept(&freed) {
        output.hint(&kept);
    }
    Collected {
        freed,
        dry_run: true,
        changes: Some(Removals { remove }),
    }
}

pub(super) fn verify(context: &Context) -> Result<Checked, Error> {
    let cache = open_cache()?;
    let scan = cache.scan_objects(&context.stop)?;
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
            output.warn(&format!(
                "{} has changed, so fetchloom downloads it again the next time it is needed",
                cache.object_path(digest).display()
            ));
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

/// how often the cache may remind a person of what it holds that no project needs, and how much
/// that has to be before it does.
const REMIND_EVERY: std::time::Duration = std::time::Duration::from_hours(24 * 7);
const REMIND_FROM: u64 = 1 << 30;

/// after a command that changed what projects need, at most once a week and only for a person at
/// a terminal: one hint when the cache holds a GiB or more no project needs. it deletes nothing;
/// freeing bytes stays `cache gc`'s, which a person runs on purpose. its errors are its own.
pub(super) fn remind(context: &Context) {
    if !context.output.can_ask() {
        return;
    }
    let Some(root) = super::cache_folder() else {
        return;
    };
    remind_at(&context.output, &root, &context.stop);
}

/// the reminder for the cache at `root`, which [`open_cache`] opens too.
fn remind_at(output: &Output, root: &Path, stop: &Stop) {
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
    // gc would forget a project out of reach, so the number would mislead
    let Ok(Some(needed)) = needed_by_all(&cache, None, stop) else {
        return;
    };
    let Ok(scan) = cache.scan_objects(stop) else {
        return;
    };
    // what `fetchloom cache` says gc frees now, so the two never disagree. the reminder tells
    // nothing of copies, so it reads no project's files
    let Ok(usage) = Usage::measure(&scan, &needed, &[], now, Threads::default(), stop) else {
        return;
    };
    let freeable = usage.freeable;
    if freeable >= REMIND_FROM {
        output.hint(&format!(
            "the cache holds {} no project needs; `fetchloom cache gc` frees it",
            units::size(freeable)
        ));
    }
}

/// sets `marker`'s time to `now`. writing an empty file again leaves its time alone on windows,
/// and the marker is opened without following a link someone planted in its place.
fn stamp(marker: &std::path::Path, now: SystemTime) -> io::Result<()> {
    crate::cache::open_unfollowed(marker, true)?.set_modified(now)
}
/// as [`needed_with`], or none when a registered project is out of reach, as on a drive not
/// plugged in: its lock cannot be read, so nothing it may need can be told apart. only
/// `cache gc`, run on purpose, forgets such a project.
pub(super) fn needed_by_all(
    cache: &Cache,
    current: Option<&Lock>,
    stop: &Stop,
) -> Result<Option<Needed>, Error> {
    let found = needed_with(cache, current, stop, false)?;
    Ok(found.complete.then_some(found.needed))
}

/// what [`needed_with`] found. `complete` is false when a registered project's data.toml was
/// missing, so what that project needs is not in `needed`.
struct Found {
    needed: Needed,
    projects: Vec<PathBuf>,
    stale_projects: Vec<PathBuf>,
    complete: bool,
}

/// every object a registered project's data.lock, or `current`, needs, following each archive to
/// the files it unpacked to, and the projects still on disk. unreadable registrations, manifests
/// and locks stop gc rather than lose protection. `collect_stale` queues roots whose data.toml
/// is gone for the sweep, so inspection does not discard removal errors or delete records early.
fn needed_with(
    cache: &Cache,
    current: Option<&Lock>,
    stop: &Stop,
    collect_stale: bool,
) -> Result<Found, Error> {
    let mut needed = Needed::default();
    let mut projects = Vec::new();
    let mut stale_projects = Vec::new();
    let mut complete = true;
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
            let help = match err.help.as_deref() {
                Some(lock::BROKEN) => format!("in that project, {}", lock::BROKEN),
                Some(help) => help.to_owned(),
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
            add_with_stop(&mut needed, cache, lock, kept.as_ref(), stop)?;
        }
        projects.push(project.root);
    }
    if let Some(lock) = current {
        add_with_stop(&mut needed, cache, lock, None, stop)?;
    }
    Ok(Found {
        needed,
        projects,
        stale_projects,
        complete,
    })
}

/// adds what one lock needs. an archive needs the files it unpacked to that `manifest`'s members
/// tables keep, and its own bytes too while this machine never unpacked it or lost such a file,
/// since unpacking again is the only way back. without `manifest`, every file is kept. a
/// recipe's pinned result needs its files, which only a build makes again.
fn add_with_stop(
    needed: &mut Needed,
    cache: &Cache,
    lock: &Lock,
    manifest: Option<&Manifest>,
    stop: &Stop,
) -> Result<(), Error> {
    for file in lock.derived.iter().flat_map(|result| &result.files) {
        stop.check()?;
        needed.objects.insert(file.sha256);
    }
    for dataset in &lock.datasets {
        let filters = manifest
            .and_then(|manifest| manifest.datasets.get(&dataset.name))
            .and_then(Entry::source)
            .map(|source| &source.members);
        for file in &dataset.files {
            stop.check()?;
            let members = file
                .unpacks_to
                .and_then(|tree| cache.unpacked(&file.sha256, Some(&tree)));
            let Some(members) = members else {
                needed.objects.insert(file.sha256);
                continue;
            };
            needed.unpacked.insert(file.sha256);
            let filter = filters.and_then(|filters| filters.get(&file.path));
            let mut whole = true;
            for member in members
                .iter()
                .filter(|member| filter.is_none_or(|filter| filter.keeps(&member.path)))
            {
                stop.check()?;
                needed.objects.insert(member.sha256);
                whole &= cache.contains_for_gc(&member.sha256, member.size, stop)?;
            }
            if !whole {
                needed.objects.insert(file.sha256);
            }
        }
    }
    stop.check()
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
        };
        let quiet = Output::new(style, Level::Quiet, false, false);
        let unhinted = Output::new(style, Level::Normal, false, false).without_hints(true);
        for output in [quiet, unhinted] {
            let folder = tempfile::tempdir().unwrap();
            fs::create_dir(folder.path().join("objects")).unwrap();
            remind_at(&output, folder.path(), &Stop::default());
            assert!(
                !folder.path().join("reminded").exists(),
                "the reminder ran though its hint could not show"
            );
        }
    }

    /// a cache holding the archive set.zip, whose two files a.txt and b.txt it unpacked to, and a
    /// lock pinning it, with the cached archive, a.txt and b.txt.
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

    #[test]
    fn an_archive_stays_needed_while_a_file_it_unpacked_to_is_missing() {
        let folder = tempfile::tempdir().unwrap();
        let cache = Cache::open(folder.path().join("cache")).unwrap();
        let (lock, [archive, _, second]) = unpacked_archive(&cache);
        let mut whole = Needed::default();
        add_with_stop(&mut whole, &cache, &lock, None, &Stop::default()).unwrap();
        assert!(whole.objects.contains(&second.digest));
        assert!(
            !whole.objects.contains(&archive.digest),
            "its files stand for it"
        );

        cache.drop_object(&second.digest).unwrap();
        let mut missing = Needed::default();
        add_with_stop(&mut missing, &cache, &lock, None, &Stop::default()).unwrap();
        assert!(
            missing.objects.contains(&archive.digest),
            "unpacking it again is the only way back to b.txt"
        );
    }

    /// a file data.toml's members table leaves out is placed by no project, so gc may free it,
    /// and its absence no longer keeps the archive. data.toml that cannot be read keeps both.
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
        add_with_stop(&mut kept, &cache, &lock, Some(&manifest), &Stop::default()).unwrap();
        assert!(kept.objects.contains(&first.digest));
        assert!(
            !kept.objects.contains(&second.digest),
            "no project places it"
        );

        cache.drop_object(&second.digest).unwrap();
        let mut freed = Needed::default();
        add_with_stop(&mut freed, &cache, &lock, Some(&manifest), &Stop::default()).unwrap();
        assert!(
            !freed.objects.contains(&archive.digest),
            "a.txt, the only file the project places, is cached"
        );
        let mut unread = Needed::default();
        add_with_stop(&mut unread, &cache, &lock, None, &Stop::default()).unwrap();
        assert!(unread.objects.contains(&archive.digest));
    }

    /// what gc frees of what no project needs: now, once a day old, or nothing, for what a folder
    /// outside the cache links too.
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

    /// copies say how much of them was linked when placed, and how much is clones.
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
