//! `add <folder> --as <name>` for a folder of the project: an output, pinned where its pipeline
//! wrote it. its files are read once, sealed against a change while they are read, and copied or
//! cloned into the cache from that read, which is what gets pinned; they stay the author's, never
//! replaced by links, so the pipeline's next run writes over them.

use std::path::Path;

use super::super::add::{AS, AddProject, free_name, from_here, render_add, sync_add, usable};
use super::super::args::Add;
use super::super::project::{Report, dry_run_done};
use super::super::sync::place::tell_marked;
use super::super::sync::{Given, Loop};
use super::super::{Context, open_cache, peek_cache};
use super::{
    Found, Walked, dataset_name, into_cache, pinned, room, tell_skipped, tell_system, walk,
};
use crate::cache::Cache;
use crate::digest::{Block, Sha256};
use crate::error::{Error, Kind};
use crate::project::Project;
use crate::project::lock::{Lock, Locked, LockedFile};
use crate::project::manifest::{self, Manifest, Source};
use crate::project::state::Placed;
use crate::stop::Stop;
use crate::{name, place, plan, units};

/// the folder `start`, found below the project at `root`, as a path from the project's top with
/// `/` between names: an output's `at`, empty for the project's own folder. none for a path
/// outside the project or one that is not a folder.
pub(super) fn at_of(root: &Path, start: &Path) -> Option<String> {
    if !start.is_dir() {
        return None;
    }
    let root = std::fs::canonicalize(root).ok()?;
    let below = start.strip_prefix(&root).ok()?;
    let names: Option<Vec<&str>> = below.iter().map(std::ffi::OsStr::to_str).collect();
    Some(names?.join("/"))
}

/// a folder read to be pinned as an output: its name, data.toml's table if it has one, its folder
/// as data.toml names it, its patterns, what a walk found there and the cache.
struct Read<'m> {
    name: String,
    entry: Option<&'m manifest::Entry>,
    at: String,
    source: Source,
    walked: Walked,
    cache: Cache,
}

/// pins the folder `start`, `shown` as typed, found at `at` in the project, as the output `--as`
/// names, or one named after the folder: new, pinned again once the person says yes to what
/// changed, or with `--check` only compared.
pub(super) async fn pin_held(
    context: &Context,
    adding: &mut AddProject,
    args: &Add,
    (start, shown, at): (&Path, &str, &str),
    here: &Path,
    starting: bool,
) -> Result<Report, Error> {
    let old = adding.old.take();
    let adding = &*adding;
    let project = &adding.project;
    let manifest = &adding.manifest;
    let read = read(context, adding, args, (start, shown, at), old.as_ref())?;
    let Read {
        name,
        entry,
        at,
        source,
        walked,
        cache,
    } = read;
    let pin = entry.and(old.as_ref()).and_then(|lock| lock.get(&name));
    let net = std::sync::Arc::new(context.net());
    if args.check || args.dry.dry_run {
        let work = Loop::new(context, project, net, cache).adding(&name);
        let read = (&walked, start, &source);
        return compared(&work, read, (&name, shown, &at), pin, args.check);
    }
    room(context, &cache, &walked, shown)?;
    let copied = into_cache(context, &cache, &walked, &|| {
        reread(context, &walked, (start, shown), &source)
    })?;
    let told = read_line(&walked, copied.cloned, shown);
    let changes = pin.map(|pin| Changes::of(&copied.files, &pin.files));
    if let Some(changes) = changes.as_ref().filter(|changes| !changes.is_empty()) {
        approved(context, (&name, shown), changes)?;
    }
    adding.seed(seeds(&name, &at, &walked, &copied.files));
    let work = Loop::new(context, project, net, cache).adding(&name);
    let mut locked = pinned(&name, &source, copied.files);
    locked.at = Some(at.clone());
    let names = [name.clone()];
    let output = &work.context.output;
    let folder = from_here(project, here, &at);
    let (outcome, datasets) = match (entry, changes) {
        (Some(_), Some(changes)) if changes.is_empty() => {
            let outcome =
                sync_add(&work, adding, manifest, None, old, (&names, Vec::new())).await?;
            output.step("Read", &format!("{name} · {told}"));
            tell_marked(output, &outcome.placed);
            output.done("Ok", &format!("{name} is pinned as {folder} holds it"));
            (outcome, Vec::new())
        }
        (Some(_), changes) => {
            let given = vec![Given::Locked(locked)];
            let outcome = sync_add(&work, adding, manifest, None, old, (&names, given)).await?;
            output.step("Read", &format!("{name} · {told}"));
            tell_marked(output, &outcome.placed);
            let said = changes.map_or_else(
                || format!("{name} in data.lock, its files are in {folder}"),
                |changes| format!("{name} · {}", changes.summary()),
            );
            output.done("Pinned", &said);
            (outcome, names.to_vec())
        }
        (None, _) => {
            let text = adding.text.as_deref().unwrap_or_default();
            let patterns = |globs: &[crate::glob::Glob]| -> Vec<String> {
                globs.iter().map(|glob| glob.as_str().to_owned()).collect()
            };
            let (select, exclude) = (patterns(&source.select), patterns(&source.exclude));
            let updated = manifest::with_output(text, &name, &at, &select, &exclude)?;
            let manifest = Manifest::parse(&updated)?;
            let given = vec![Given::Locked(locked)];
            let outcome = sync_add(
                &work,
                adding,
                &manifest,
                Some(&updated),
                old,
                (&names, given),
            )
            .await?;
            output.step("Imported", &format!("{name} · {told}"));
            let has_files = outcome.placed.planned.contains(&name);
            render_add(
                &work,
                &manifest,
                (&name, has_files),
                here,
                (starting, &outcome.placed),
                (&[], None),
            );
            (outcome, names.to_vec())
        }
    };
    Ok(Report {
        project: project.root.clone(),
        datasets,
        lock_changed: outcome.lock_changed,
        placement: Some(outcome.placed),
        ..Report::default()
    })
}

/// the folder `start`, `shown` as typed, at `at`, checked and walked for `args`: refused when it
/// cannot be an output's folder, holds another dataset's file or the cache, or `--check` names
/// no output.
fn read<'m>(
    context: &Context,
    adding: &'m AddProject,
    args: &Add,
    (start, shown, at): (&Path, &str, &str),
    old: Option<&Lock>,
) -> Result<Read<'m>, Error> {
    let manifest = &adding.manifest;
    refuse_folder(manifest, at, shown)?;
    let (name, entry) = output_name(context, manifest, args, at, start, shown)?;
    if args.check && entry.is_none() {
        return Err(Error::new(
            Kind::Usage,
            format!(
                "{name} is no output data.toml pins, so there is nothing to check {shown} against"
            ),
        )
        .help(format!(
            "run `{}` to pin it first",
            command(&format!("add {} --as {name}", shell(shown)))
        )));
    }
    let writes = !args.dry.dry_run && !args.check;
    if writes {
        adding.project.check_writable()?;
    }
    let cache = if writes { open_cache()? } else { peek_cache()? };
    refuse_cache(start, shown, cache.root())?;
    refuse_owned(manifest, old, &cache, &name, at, shown)?;
    let source = match entry {
        Some(entry) => {
            refuse_patterns(args, &name)?;
            entry.source.clone()
        }
        None => new_source(args)?,
    };
    // data.toml's spelling, which a pin made for it repeats, so the pin answers data.toml
    let at = entry
        .and_then(|entry| entry.at.clone())
        .unwrap_or_else(|| at.to_owned());
    let walked = walk(
        start,
        shown,
        &source,
        context.settings.most_files.value,
        &context.stop,
    )?;
    tell_skipped(&context.output, shown, &walked.skipped);
    tell_system(&context.output, shown, &walked.system);
    Ok(Read {
        name,
        entry,
        at,
        source,
        walked,
        cache,
    })
}

/// `--check`, or a dry run: the folder `walked` found at `start` compared with `pin`, read and
/// hashed where it is, writing nothing. a new output's dry run hashes nothing.
fn compared(
    work: &Loop<'_>,
    (walked, start, source): (&Walked, &Path, &Source),
    (name, shown, at): (&str, &str, &str),
    pin: Option<&Locked>,
    check: bool,
) -> Result<Report, Error> {
    let context = work.context;
    if pin.is_none() && !check {
        let files = units::count(walked.files.len() as u64, "file", "files");
        context.output.step(
            "Would add",
            &format!(
                "{name} to data.toml, an output pinned where it is, at {at}: {files} · {}",
                units::size(walked.bytes())
            ),
        );
        return Ok(preview(work, (name, shown), None));
    }
    let hashed = hashed(context, walked)?;
    reread(context, walked, (start, shown), source)?;
    let changes = Changes::of(&hashed, pin.map_or(&[][..], |pin| &pin.files));
    if check {
        return checked(context, work.project, (name, shown), &changes);
    }
    Ok(preview(work, (name, shown), Some(&changes)))
}

/// what reading the folder `shown` did: its files and bytes, copied or `cloned` into the cache.
fn read_line(walked: &Walked, cloned: bool, shown: &str) -> String {
    let how = if cloned { "cloned" } else { "copied" };
    format!(
        "{} · {}, {how} into the cache; {shown} is unchanged",
        units::count(walked.files.len() as u64, "file", "files"),
        units::size(walked.bytes())
    )
}

fn command(rest: &str) -> String {
    super::super::project::command(rest)
}

fn shell(word: &str) -> String {
    super::super::shell_word(word).into_owned()
}

/// refuses a folder that cannot be an output's: the project's own, or one data.toml could not
/// name as `at`, by the rules a cloned data.toml's is held to.
fn refuse_folder(manifest: &Manifest, at: &str, shown: &str) -> Result<(), Error> {
    if at.is_empty() {
        return Err(Error::new(
            Kind::Usage,
            format!("{shown} holds the whole project, so it cannot be an output's folder"),
        )
        .help(format!(
            "pin the folder your pipeline writes, as in `{}`",
            command("add data/processed --as processed")
        )));
    }
    let text = format!(
        "[project]\ndata_dir = {}\n[datasets.output]\nref = \"local\"\nat = {}\n",
        toml_edit::value(manifest.data_dir.as_str()),
        toml_edit::value(at)
    );
    let Err(error) = Manifest::parse(&text) else {
        return Ok(());
    };
    let said = match error.message.split_once(" is refused because ") {
        Some((_, reason)) => format!("{shown} cannot be an output's folder: {reason}"),
        None => format!(
            "{shown} holds the data folder {}, where fetched datasets land, so it cannot be an output's folder",
            manifest.data_dir
        ),
    };
    Err(Error::new(Kind::Usage, said)
        .help("pin a folder of its own, such as data/processed or results"))
}

/// refuses a folder that is the cache, inside it or holds it, as a data folder is refused.
fn refuse_cache(start: &Path, shown: &str, cache: &Path) -> Result<(), Error> {
    use crate::project::overlap::{Overlap, overlap};
    let said = match overlap(start, cache) {
        None => return Ok(()),
        Some(Overlap::Inside) => format!("{shown} is inside the cache folder"),
        Some(Overlap::Holds) => format!("{shown} holds the cache folder"),
    };
    Err(
        Error::new(Kind::Usage, format!("{said} {}", cache.display()))
            .help("pin a folder your pipeline writes, outside the cache"),
    )
}

/// the output's name and its table in data.toml, when it is one already: the one pinned at `at`,
/// the one `--as` names, or one named after the folder, numbered when data.toml has the name.
fn output_name<'m>(
    context: &Context,
    manifest: &'m Manifest,
    args: &Add,
    at: &str,
    start: &Path,
    shown: &str,
) -> Result<(String, Option<&'m manifest::Entry>), Error> {
    let folded = name::fold(at);
    let here = manifest.datasets.iter().find(|(_, entry)| {
        entry
            .at
            .as_deref()
            .is_some_and(|other| name::fold(other) == folded)
    });
    match (args.name.as_deref(), here) {
        (Some(asked), Some((owner, _))) if asked != owner => Err(Error::new(
            Kind::Usage,
            format!("{shown} is the folder of the output {owner}"),
        )
        .help(format!(
            "run `{}` to pin it again, or rename datasets.{owner} in data.toml",
            command(&format!("add {} --as {owner}", shell(shown)))
        ))),
        (_, Some((owner, entry))) => Ok((owner.clone(), Some(entry))),
        (Some(asked), None) => {
            usable(asked, manifest)?;
            if let Some(entry) = manifest.datasets.get(asked) {
                let what = match &entry.at {
                    Some(other) => format!("the output at {other}"),
                    None => "a dataset of its own".to_owned(),
                };
                return Err(Error::new(
                    Kind::Usage,
                    format!("{asked} is in data.toml already, as {what}"),
                )
                .help(AS));
            }
            Ok((asked.to_owned(), None))
        }
        (None, None) => {
            let own = dataset_name(start, false).unwrap_or_else(|| "output".to_owned());
            let name = free_name(
                &context.output,
                own,
                &|name| manifest.datasets.contains_key(name),
                AS,
            )?;
            Ok((name, None))
        }
    }
}

/// refuses an output's folder that is in another output's, holds one, or holds a file another
/// dataset places, as a disk that ignores case sees them: every file there is the output's.
fn refuse_owned(
    manifest: &Manifest,
    lock: Option<&Lock>,
    cache: &Cache,
    output: &str,
    at: &str,
    shown: &str,
) -> Result<(), Error> {
    let folded = name::fold(at);
    let inside = format!("{folded}/");
    let refused = |said: String| {
        Error::new(Kind::Usage, said)
            .help("pin a folder that holds only what your pipeline writes, such as data/processed")
    };
    for (other, entry) in &manifest.datasets {
        let Some(theirs) = entry.at.as_deref().filter(|_| other != output) else {
            continue;
        };
        let folded_theirs = name::fold(theirs);
        if folded == folded_theirs || folded.starts_with(&format!("{folded_theirs}/")) {
            return Err(refused(format!(
                "{shown} is in {theirs}, the folder of the output {other}, where every file is the output's"
            )));
        }
        if folded_theirs.starts_with(&inside) {
            return Err(refused(format!(
                "{shown} holds {theirs}, the folder of the output {other}"
            )));
        }
    }
    let Some(lock) = lock else {
        return Ok(());
    };
    // a plan data.toml cannot make is told by the command that places it
    let Ok(planned) = plan::from_cache(manifest, lock, cache) else {
        return Ok(());
    };
    let taken = planned
        .targets
        .iter()
        .find(|target| target.dataset != output && name::fold(&target.path).starts_with(&inside));
    match taken {
        Some(target) => Err(refused(format!(
            "{shown} holds {}, a file of {}, so it cannot be an output's folder",
            target.path, target.dataset
        ))),
        None => Ok(()),
    }
}

/// refuses patterns on the command line for an output data.toml has, whose patterns stay its own.
fn refuse_patterns(args: &Add, name: &str) -> Result<(), Error> {
    if args.filter.select.is_empty() && args.filter.exclude.is_empty() && !args.no_extract {
        return Ok(());
    }
    Err(Error::new(
        Kind::Usage,
        format!("{name} is in data.toml already, so its patterns come from there"),
    )
    .help(format!(
        "leave out --select, --exclude and --no-extract, or change datasets.{name} in data.toml"
    )))
}

/// the source of a new output, with the patterns `args` give: nothing it holds is unpacked.
fn new_source(args: &Add) -> Result<Source, Error> {
    let text = manifest::with_output("", "new", "out", &args.filter.select, &args.filter.exclude)?;
    Ok(Manifest::parse(&text)?.datasets["new"].source.clone())
}

/// each file `walked` found, read and hashed where it is, writing nothing, as a check reads them.
fn hashed(context: &Context, walked: &Walked) -> Result<Vec<LockedFile>, Error> {
    let read = place::run_all(&walked.files, context.threads, |file: &Found| {
        if context.stop.is_asked() {
            return None;
        }
        let block = Block::sized(file.size.saturating_add(1));
        Some(super::super::sync::disk::hash_plain(&file.source, block))
    });
    context.stop.check()?;
    walked
        .files
        .iter()
        .zip(read)
        .map(|(file, read)| {
            let sha256: Sha256 = read
                .ok_or_else(Stop::stopped)?
                .map_err(|err| Error::io(format_args!("could not read {}", file.path), &err))?;
            Ok(LockedFile {
                path: file.path.clone(),
                size: file.size,
                sha256,
                md5: None,
                url: String::new(),
                unpacks_to: None,
            })
        })
        .collect()
}

/// refuses the files `walked` found in `start` once a second walk finds them otherwise: one more
/// or fewer, or one of another size, time or file, so what was read is no state the folder held.
/// files written apart may still belong to no single run, which no walk can tell.
fn reread(
    context: &Context,
    walked: &Walked,
    (start, shown): (&Path, &str),
    source: &Source,
) -> Result<(), Error> {
    let most = context.settings.most_files.value;
    let again = walk(start, shown, source, most, &context.stop);
    if again.is_ok_and(|again| same_files(&again, walked)) {
        return Ok(());
    }
    Err(Error::new(
        Kind::ProjectChanged,
        format!("{shown} changed while it was read: let the pipeline finish, then add again"),
    )
    .help("fetchloom pins a folder only as it stood the whole time it read it, and pinned nothing"))
}

/// whether two walks of one folder found the same files, each as it was.
fn same_files(now: &Walked, then: &Walked) -> bool {
    now.files.len() == then.files.len()
        && now
            .files
            .iter()
            .zip(&then.files)
            .all(|(now, then)| now.path == then.path && now.seen == then.seen)
}

/// what an output's folder holds compared with its pin, each list in path order.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Changes {
    /// files of other bytes, with the pinned size and the size now.
    pub(super) changed: Vec<(String, u64, u64)>,
    pub(super) added: Vec<(String, u64)>,
    pub(super) removed: Vec<(String, u64)>,
}

impl Changes {
    /// how `found` differs from `pinned`, both sorted by path.
    pub(super) fn of(found: &[LockedFile], pinned: &[LockedFile]) -> Changes {
        let mut changes = Changes::default();
        let (mut found, mut pinned) = (found.iter().peekable(), pinned.iter().peekable());
        loop {
            match (found.peek(), pinned.peek()) {
                (None, None) => return changes,
                (Some(now), Some(then)) if now.path == then.path => {
                    if now.sha256 != then.sha256 || now.size != then.size {
                        changes
                            .changed
                            .push((now.path.clone(), then.size, now.size));
                    }
                    found.next();
                    pinned.next();
                }
                (Some(now), then) if then.is_none_or(|then| now.path < then.path) => {
                    changes.added.push((now.path.clone(), now.size));
                    found.next();
                }
                (_, Some(then)) => {
                    changes.removed.push((then.path.clone(), then.size));
                    pinned.next();
                }
                (Some(_), None) => unreachable!("a file found with no pin left is added above"),
            }
        }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.changed.is_empty() && self.added.is_empty() && self.removed.is_empty()
    }

    /// `1 file changed, 2 added, 1 removed`, leaving out what is none.
    pub(super) fn summary(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        for (count, done) in [
            (self.changed.len(), "changed"),
            (self.added.len(), "added"),
            (self.removed.len(), "removed"),
        ] {
            if count == 0 {
                continue;
            }
            let count = count as u64;
            parts.push(if parts.is_empty() {
                format!("{} {done}", units::count(count, "file", "files"))
            } else {
                format!("{} {done}", units::grouped(count))
            });
        }
        parts.join(", ")
    }

    /// a line for each file, as an error's details and a question list them.
    pub(super) fn lines(&self) -> Vec<String> {
        let changed = self.changed.iter().map(|(path, then, now)| {
            format!(
                "changed {path} · {} then, {} now",
                units::size(*then),
                units::size(*now)
            )
        });
        let added = self
            .added
            .iter()
            .map(|(path, size)| format!("added {path} · {}", units::size(*size)));
        let removed = self
            .removed
            .iter()
            .map(|(path, size)| format!("removed {path} · {}", units::size(*size)));
        changed.chain(added).chain(removed).collect()
    }

    /// the paths of each kind, as `--json` lists them.
    pub(super) fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "changed": self.changed.iter().map(|(path, _, _)| path).collect::<Vec<_>>(),
            "added": self.added.iter().map(|(path, _)| path).collect::<Vec<_>>(),
            "removed": self.removed.iter().map(|(path, _)| path).collect::<Vec<_>>(),
        })
    }
}

/// `--check`'s answer: ok when the folder holds the pinned files, else `output.differs` naming
/// each difference.
fn checked(
    context: &Context,
    project: &Project,
    (name, shown): (&str, &str),
    changes: &Changes,
) -> Result<Report, Error> {
    if changes.is_empty() {
        context
            .output
            .done("Ok", &format!("{shown} holds the files {name} pins"));
        return Ok(Report {
            project: project.root.clone(),
            ..Report::default()
        });
    }
    let mut error = Error::new(
        Kind::OutputDiffers,
        format!(
            "{shown} holds other files than {name} pins: {}",
            changes.summary()
        ),
    );
    for line in changes.lines() {
        error = error.detail(line);
    }
    Err(error
        .help(format!(
            "rerun your pipeline, or run `{}` to pin what it holds now",
            command(&format!("add {} --as {name} --yes", shell(shown)))
        ))
        .with_result(serde_json::json!({ "differences": changes.json() })))
}

/// a dry run's report: what pinning would change, said, writing nothing.
fn preview(work: &Loop<'_>, (name, shown): (&str, &str), changes: Option<&Changes>) -> Report {
    let output = &work.context.output;
    match changes {
        Some(changes) if changes.is_empty() => {
            output.step(
                "Would keep",
                &format!("{name} as it is: {shown} holds its files"),
            );
        }
        Some(changes) => {
            output.step("Would pin", &format!("{name} · {}", changes.summary()));
            for line in changes.lines() {
                output.detail(&line);
            }
        }
        None => {}
    }
    dry_run_done(output, None);
    let unchanged = changes.is_some_and(Changes::is_empty);
    Report {
        project: work.project.root.clone(),
        datasets: if unchanged {
            Vec::new()
        } else {
            vec![name.to_owned()]
        },
        lock_changed: !unchanged,
        dry_run: true,
        ..Report::default()
    }
}

/// asks to pin `changes`, what the folder read holds, in place of the output's pin: a yes at a
/// terminal or `--yes`, else refused as usage, since a script that meant to check would repin.
fn approved(
    context: &Context,
    (name, shown): (&str, &str),
    changes: &Changes,
) -> Result<(), Error> {
    if context.yes {
        return Ok(());
    }
    let output = &context.output;
    let again = command(&format!("add {} --as {name} --yes", shell(shown)));
    if output.can_ask() {
        output.step("Changed", &format!("{name} · {}", changes.summary()));
        for line in changes.lines() {
            output.detail(&line);
        }
    }
    match output.confirm(&format!("pin what {shown} holds now as {name}?"))? {
        Some(true) => Ok(()),
        Some(false) => Err(
            Error::new(Kind::Unconfirmed, format!("{name} stays pinned as it was"))
                .help(format!("run `{again}` once you mean to pin it")),
        ),
        None => {
            let mut error = Error::new(
                Kind::Usage,
                format!("{shown} holds other files than {name} pins"),
            );
            for line in changes.lines() {
                error = error.detail(line);
            }
            Err(error.help(format!(
                "run `{again}` to pin what it holds now, `{}` to compare only, or `{}` to put back the files {name} pins",
                command(&format!("add {} --as {name} --check", shell(shown))),
                command(&format!("sync --force {name}"))
            )))
        }
    }
}

/// the records of the author's files just pinned at `at`, each as the walk saw it, so the next
/// command finds them in place without reading a byte. `checked` is the survey's to set.
fn seeds(name: &str, at: &str, walked: &Walked, files: &[LockedFile]) -> Vec<(String, Placed)> {
    walked
        .files
        .iter()
        .zip(files)
        .map(|(found, file)| {
            (
                name::joined(at, &found.path),
                Placed {
                    dataset: name.to_owned(),
                    sha256: file.sha256,
                    method: place::Method::Copy,
                    look: found.seen.look,
                    writable: found.seen.writable,
                    checked: 0,
                    adopted: true,
                    output: true,
                    unlinkable: false,
                    racy_intact: std::sync::OnceLock::new(),
                },
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, bytes: &[u8]) -> LockedFile {
        LockedFile {
            path: path.to_owned(),
            size: bytes.len() as u64,
            sha256: crate::digest::sha256(bytes),
            md5: None,
            url: String::new(),
            unpacks_to: None,
        }
    }

    /// a rerun's tree against its pin: other bytes of one size are a change too, and each list
    /// keeps path order whichever side runs out first.
    #[test]
    fn changes_name_each_file_changed_added_and_removed() {
        let pinned = [
            file("a.csv", b"1"),
            file("b/summary.txt", b"150 rows"),
            file("c.csv", b"same"),
            file("d.csv", b"x"),
        ];
        let found = [
            file("a.csv", b"2"),
            file("b/plot.png", b"png"),
            file("c.csv", b"same"),
            file("e.csv", b"new"),
        ];
        let changes = Changes::of(&found, &pinned);
        assert_eq!(changes.changed, [("a.csv".to_owned(), 1, 1)]);
        assert_eq!(
            changes.added,
            [("b/plot.png".to_owned(), 3), ("e.csv".to_owned(), 3)]
        );
        assert_eq!(
            changes.removed,
            [("b/summary.txt".to_owned(), 8), ("d.csv".to_owned(), 1)]
        );
        assert_eq!(changes.summary(), "1 file changed, 2 added, 2 removed");
        assert!(Changes::of(&pinned, &pinned).is_empty());
        assert_eq!(Changes::of(&[], &pinned[..1]).summary(), "1 file removed");
        assert_eq!(Changes::of(&found[..2], &[]).summary(), "2 files added");
    }
}
