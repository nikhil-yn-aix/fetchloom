//! `fetchloom cache`: where the cache is and what it holds. `cache gc` frees what no project needs,
//! and `cache verify` hashes every object again.

use std::fs;
use std::io;
use std::path::PathBuf;
use std::time::SystemTime;

use serde::Serialize;

use super::{Context, open_cache};
use crate::cache::{Cache, Checked, Freed, Needed};
use crate::error::{Error, Kind};
use crate::project::Project;
use crate::project::lock::{self, Lock};
use crate::stop::Stop;
use crate::units;

/// what `fetchloom cache` reports, as `--json` prints it.
#[derive(Serialize)]
pub(super) struct Info {
    path: PathBuf,
    objects: u64,
    bytes: u64,
    /// bytes some registered project's data.lock needs.
    needed: u64,
    projects: Vec<PathBuf>,
}

pub(super) fn info(context: &Context) -> Result<Info, Error> {
    let cache = open_cache()?;
    let (needed, projects) = needed_with(&cache, None, &context.stop, false)?;
    let scan = cache.scan_objects(&context.stop)?;
    if let Some(path) = scan.unreadable.first() {
        return Err(
            Error::new(Kind::LocalIo, format!("cannot inspect {}", path.display()))
                .help("check the cache folder's permissions, then run the command again"),
        );
    }
    let objects = scan.objects;
    let bytes: u64 = objects.iter().map(|object| object.size).sum();
    let kept: u64 = objects
        .iter()
        .filter(|object| needed.objects.contains(&object.digest))
        .map(|object| object.size)
        .sum();
    let output = &context.output;
    output.step(
        "Cache",
        &format!(
            "{} · {} · {}",
            cache.root().display(),
            units::count(objects.len() as u64, "file", "files"),
            units::size(bytes)
        ),
    );
    for project in &projects {
        output.step("Used by", &project.display().to_string());
    }
    if objects.is_empty() {
        output.done("Ok", "the cache has no data files");
    } else if objects
        .iter()
        .all(|object| needed.objects.contains(&object.digest))
    {
        output.done("Ok", "every cached file is needed by a project");
    } else {
        output.done(
            "Ok",
            &format!("{} of it is needed by a project", units::size(kept)),
        );
        output.hint("`fetchloom cache gc` frees the rest once it is a day old; `fetchloom cache gc --now` frees it now");
    }
    output.answer_untold(&cache.root().display().to_string());
    Ok(Info {
        path: cache.root().to_path_buf(),
        objects: objects.len() as u64,
        bytes,
        needed: kept,
        projects,
    })
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
/// making it, so it decides exactly as a real sweep does.
pub(super) fn gc(context: &Context, now: bool, dry_run: bool) -> Result<Collected, Error> {
    let cache = if dry_run {
        let cache = super::peek_cache()?;
        if !cache.root().exists() {
            return Ok(tell_dry_gc(context, Freed::default(), Vec::new()));
        }
        cache.pretending()
    } else {
        open_cache()?
    };
    let (needed, projects) = needed_with(&cache, None, &context.stop, true)?;
    let grace = if now {
        std::time::Duration::ZERO
    } else {
        crate::cache::GRACE
    };
    let freed = cache.sweep_with_stop(&needed, SystemTime::now(), grace, &context.stop)?;
    if dry_run {
        return Ok(tell_dry_gc(context, freed, cache.pretended()));
    }
    let output = &context.output;
    for path in &freed.not_removed {
        output.warn(&format!("could not read or remove {}; close programs using it or check its permissions, then run the command again", path.display()));
    }
    let kept = kept(&freed);
    if freed.objects == 0 && freed.leftovers == 0 && freed.not_removed.is_empty() {
        match &kept {
            Some(kept) => {
                output.done("Ok", "nothing to free yet");
                output.hint(kept);
            }
            None => output.done("Ok", "nothing to free"),
        }
        return Ok(collected(freed));
    }
    if freed.objects > 0 {
        output.step(
            "Removed",
            &format!(
                "{} · {} from the cache",
                units::count(freed.objects, "file", "files"),
                units::size(freed.bytes)
            ),
        );
    }
    if freed.leftovers > 0 {
        output.step(
            "Removed",
            &units::count(freed.leftovers, "leftover item", "leftover items"),
        );
    }
    if !freed.not_removed.is_empty() {
        let result = serde_json::to_value(&freed).map_err(|err| {
            Error::new(
                Kind::LocalIo,
                format!("cannot format the cache result: {err}"),
            )
        })?;
        return Err(Error::new(Kind::LocalIo, format!("{} in the cache could not be read or removed", units::count(freed.not_removed.len() as u64, "item", "items")))
            .help("close programs using these paths or check their permissions, then run the command again")
            .with_result(result));
    }
    output.done(
        "Cleaned",
        &format!(
            "the cache, keeping files needed by {}",
            units::count(projects.len() as u64, "project", "projects")
        ),
    );
    if let Some(kept) = &kept {
        output.hint(kept);
    }
    Ok(collected(freed))
}

fn collected(freed: Freed) -> Collected {
    Collected {
        freed,
        dry_run: false,
        changes: None,
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
    for path in &freed.not_removed {
        output.warn(&format!(
            "could not read {}; gc would leave it in the cache",
            path.display()
        ));
    }
    if freed.objects > 0 {
        output.step(
            "Would free",
            &format!(
                "{} · {} from the cache",
                units::count(freed.objects, "file", "files"),
                units::size(freed.bytes)
            ),
        );
    }
    if freed.leftovers > 0 {
        output.step(
            "Would drop",
            &units::count(freed.leftovers, "leftover item", "leftover items"),
        );
    }
    for path in &remove {
        output.detail(&path.display().to_string());
    }
    let nothing = match (
        freed.objects == 0 && freed.leftovers == 0,
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
    for path in &checked.unreadable {
        output.warn(&format!(
            "{} is unreadable and was left in the cache",
            path.display()
        ));
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
        .help("close any program using these cached files or check their permissions, then run `fetchloom cache verify` again"));
    }
    for digest in &checked.changed {
        if !checked.stuck.contains(digest) {
            output.warn(&format!(
                "{} changed and was marked for replacement",
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
            "run `fetchloom sync` in each project. a project whose own linked file holds the changed bytes keeps it as an edit; there, save any change you need, then run `fetchloom sync --force`"
        } else {
            "close programs using the unreadable files or check their permissions, then run `fetchloom cache verify` again and `fetchloom sync` in each project"
        }));
    }
    error = error.detail(format!(
        "{} could not be marked for repair:",
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
    let Some(root) = Cache::locate(&|name| std::env::var_os(name)) else {
        return;
    };
    if !root.join("objects").is_dir() {
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
    // a project out of reach, as on a drive not plugged in, still needs its bytes, and gc would
    // forget it, so the number would mislead
    let Ok(roots) = cache.registered_projects(&context.stop) else {
        return;
    };
    if roots
        .iter()
        .any(|root| !Project { root: root.clone() }.manifest_path().is_file())
    {
        return;
    }
    let Ok((needed, _)) = needed_with(&cache, None, &context.stop, false) else {
        return;
    };
    let Ok(scan) = cache.scan_objects(&context.stop) else {
        return;
    };
    // only what gc would free and what frees disk space: past its day, and not also a file a
    // folder made by `get` still links to
    let freeable: u64 = scan
        .objects
        .iter()
        .filter(|object| {
            !needed.objects.contains(&object.digest)
                && now
                    .duration_since(object.modified)
                    .is_ok_and(|age| age >= crate::cache::GRACE)
                && !crate::place::has_other_names(&object.path)
        })
        .map(|object| object.size)
        .sum();
    if freeable >= REMIND_FROM {
        context.output.hint(&format!(
            "the cache holds {} no project needs; `fetchloom cache gc` frees it",
            units::size(freeable)
        ));
    }
}

/// sets `marker`'s time to `now`. writing an empty file again leaves its time alone on windows.
fn stamp(marker: &std::path::Path, now: SystemTime) -> io::Result<()> {
    fs::File::create(marker)?.set_modified(now)
}
/// as [`needed_with`], keeping every registration: only `cache gc`, run on purpose, forgets.
pub(super) fn needed(
    cache: &Cache,
    current: Option<&Lock>,
) -> Result<(Needed, Vec<PathBuf>), Error> {
    needed_with(cache, current, &Stop::default(), false)
}

/// every object a registered project's data.lock, or `current`, needs, following each archive to
/// the files it unpacked to, and the projects still on disk. unreadable registrations, manifests
/// and locks stop gc rather than lose protection. with `forget`, a project whose data.toml is
/// gone is forgotten; otherwise it stays registered, as when its drive is only unplugged.
fn needed_with(
    cache: &Cache,
    current: Option<&Lock>,
    stop: &Stop,
    forget: bool,
) -> Result<(Needed, Vec<PathBuf>), Error> {
    let mut needed = Needed::default();
    let mut projects = Vec::new();
    for root in cache.registered_projects(stop)? {
        stop.check()?;
        let project = Project { root };
        let manifest = project.manifest_path();
        match fs::read(&manifest) {
            Ok(_) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                if forget {
                    cache.forget(&project.root);
                }
                continue;
            }
            Err(err) => {
                return Err(Error::io(
                    format!("cannot read {}", manifest.display()),
                    &err,
                ));
            }
        }
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
            add_with_stop(&mut needed, cache, lock, stop)?;
        }
        projects.push(project.root);
    }
    if let Some(lock) = current {
        add_with_stop(&mut needed, cache, lock, stop)?;
    }
    Ok((needed, projects))
}

/// adds what one lock needs. an archive needs the files it unpacked to, and its own bytes too
/// while this machine never unpacked it or lost a file it unpacked to, since unpacking again is
/// the only way back.
fn add_with_stop(
    needed: &mut Needed,
    cache: &Cache,
    lock: &Lock,
    stop: &Stop,
) -> Result<(), Error> {
    for file in lock.datasets.iter().flat_map(|dataset| &dataset.files) {
        stop.check()?;
        let members = file
            .unpacks_to
            .and_then(|tree| cache.unpacked(&file.sha256, Some(&tree)));
        let Some(members) = members else {
            needed.objects.insert(file.sha256);
            continue;
        };
        needed.unpacked.insert(file.sha256);
        let mut whole = true;
        for member in &members {
            stop.check()?;
            needed.objects.insert(member.sha256);
            whole &= cache.contains_for_gc(&member.sha256, member.size, stop)?;
        }
        if !whole {
            needed.objects.insert(file.sha256);
        }
    }
    stop.check()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::Member;
    use crate::cache::tests::store;
    use crate::project::lock::{Locked, LockedFile};

    #[test]
    fn an_archive_stays_needed_while_a_file_it_unpacked_to_is_missing() {
        let folder = tempfile::tempdir().unwrap();
        let cache = Cache::open(folder.path().join("cache")).unwrap();
        let archive = store(&cache, b"the archive itself");
        let first = store(&cache, b"first\n");
        let second = store(&cache, b"second\n");
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
                resolved: "https://example.org/set.zip".to_owned(),
                title: None,
                license: None,
                doi: None,
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
        };
        let mut whole = Needed::default();
        add_with_stop(&mut whole, &cache, &lock, &Stop::default()).unwrap();
        assert!(whole.objects.contains(&second.digest));
        assert!(
            !whole.objects.contains(&archive.digest),
            "its files stand for it"
        );

        cache.drop_object(&second.digest).unwrap();
        let mut missing = Needed::default();
        add_with_stop(&mut missing, &cache, &lock, &Stop::default()).unwrap();
        assert!(
            missing.objects.contains(&archive.digest),
            "unpacking it again is the only way back to b.txt"
        );
    }
}
