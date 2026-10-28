//! `fetchloom cache`: where the cache is and what it holds. `cache gc` frees what no project needs,
//! and `cache verify` hashes every object again.

use std::path::PathBuf;
use std::time::SystemTime;

use serde::Serialize;

use super::{Context, open_cache};
use crate::cache::{Cache, Checked, Freed, Needed};
use crate::error::{Error, Kind};
use crate::project::Project;
use crate::project::lock::Lock;
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
    let (needed, projects) = needed(&cache, None)?;
    let objects = cache.objects();
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
            "{} · {} in {}",
            cache.root().display(),
            units::size(bytes),
            units::count(objects.len() as u64, "object", "objects")
        ),
    );
    for project in &projects {
        output.step("Used by", &project.display().to_string());
    }
    if bytes == kept {
        output.done("every object is needed by a project");
    } else {
        output.done(&format!(
            "{} of it is needed by a project",
            units::size(kept)
        ));
        output.hint("`fetchloom cache gc` frees the rest, once it is a day old");
    }
    output.answer(&cache.root().display().to_string());
    Ok(Info {
        path: cache.root().to_path_buf(),
        objects: objects.len() as u64,
        bytes,
        needed: kept,
        projects,
    })
}

pub(super) fn gc(context: &Context) -> Result<Freed, Error> {
    let cache = open_cache()?;
    let (needed, projects) = needed(&cache, None)?;
    let freed = cache.sweep(&needed, SystemTime::now());
    let output = &context.output;
    if freed.objects == 0 && freed.leftovers == 0 {
        output.done("nothing to free. every object is needed by a project or newer than a day");
        return Ok(freed);
    }
    if freed.objects > 0 {
        output.step(
            "Freed",
            &format!(
                "{} · {}",
                units::size(freed.bytes),
                units::count(freed.objects, "object", "objects")
            ),
        );
    }
    if freed.leftovers > 0 {
        output.step(
            "Removed",
            &units::count(freed.leftovers, "abandoned download", "abandoned downloads"),
        );
    }
    output.done(&format!(
        "the cache keeps what {} need",
        units::count(projects.len() as u64, "project", "projects")
    ));
    Ok(freed)
}

pub(super) fn verify(context: &Context) -> Result<Checked, Error> {
    let cache = open_cache()?;
    let total: u64 = cache.objects().iter().map(|object| object.size).sum();
    let bar = context
        .output
        .bar("checking every file in the cache".to_owned());
    bar.start(Some(total), 0);
    let threads = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    let checked = cache.verify_all(threads, &|bytes| bar.advance(bytes));
    drop(bar);
    let output = &context.output;
    let what = format!(
        "{} · {}",
        units::count(checked.objects, "object", "objects"),
        units::size(checked.bytes)
    );
    if checked.changed.is_empty() {
        output.done(&format!("verified {what}. every one is intact"));
        return Ok(checked);
    }
    for digest in &checked.changed {
        if !checked.stuck.contains(digest) {
            output.warn(&format!("object {digest} had changed and was removed"));
        }
    }
    let mut error = Error::new(
        Kind::IntegrityMismatch,
        format!(
            "verified {what}. {} had changed",
            units::count(checked.changed.len() as u64, "object", "objects")
        ),
    );
    if checked.stuck.is_empty() {
        return Err(error.help("the next sync downloads what a project still needs"));
    }
    error = error.detail(format!(
        "{} could not be removed:",
        units::count(checked.stuck.len() as u64, "of them", "of them")
    ));
    for digest in checked.stuck.iter().take(5) {
        error = error.detail(cache.object_path(digest).display().to_string());
    }
    Err(error.help("remove them by hand. if others write to this cache, check who changed them"))
}

/// every object a registered project's data.lock, or `current`, needs, following each archive to
/// the files it unpacked to, and the projects still on disk. a project whose data.toml is gone is
/// forgotten. a lock that cannot be read is an error, so nothing a project needs is ever deleted
/// by mistake.
pub(super) fn needed(
    cache: &Cache,
    current: Option<&Lock>,
) -> Result<(Needed, Vec<PathBuf>), Error> {
    let mut needed = Needed::default();
    let mut projects = Vec::new();
    for root in cache.projects() {
        let project = Project { root };
        if !project.manifest_path().is_file() {
            cache.forget(&project.root);
            continue;
        }
        let lock = project.lock().map_err(|err| {
            Error::new(
                Kind::ProjectInvalid,
                format!(
                    "data.lock of {} cannot be read, so the cache stays as it is",
                    project.root.display()
                ),
            )
            .detail(err.message)
            .help("fix that project's data.lock, or run `fetchloom lock` there, then try again")
        })?;
        if let Some(lock) = &lock {
            add(&mut needed, cache, lock);
        }
        projects.push(project.root);
    }
    if let Some(lock) = current {
        add(&mut needed, cache, lock);
    }
    Ok((needed, projects))
}

/// adds what one lock needs. an archive needs the files it unpacked to, and its own bytes too
/// while this machine never unpacked it or lost a file it unpacked to, since unpacking again is
/// the only way back.
fn add(needed: &mut Needed, cache: &Cache, lock: &Lock) {
    for file in lock.datasets.iter().flat_map(|dataset| &dataset.files) {
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
            needed.objects.insert(member.sha256);
            whole &= cache.find(&member.sha256, Some(member.size)).is_some();
        }
        if !whole {
            needed.objects.insert(file.sha256);
        }
    }
}
