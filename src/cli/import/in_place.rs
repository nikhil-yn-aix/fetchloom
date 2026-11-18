//! `add <folder> --as <name>` for a folder of the project: an output, pinned where its pipeline
//! wrote it. its files are read once, sealed against a change while they are read, and copied or
//! cloned into the cache from that read, which is what gets pinned; they stay the author's, never
//! replaced by links, so the pipeline's next run writes over them.

use std::collections::HashSet;
use std::path::Path;

use super::super::add::{AS, AddProject, free_name, from_here, render_add, sync_add, usable};
use super::super::args::Add;
use super::super::diff::{self, Change, Pinned};
use super::super::project::{Outcome, Report, dry_run_done};
use super::super::sync::disk;
use super::super::sync::place::{Placement, is_marker_form, tell_marked};
use super::super::sync::{Given, Loop};
use super::super::{Context, open_cache, peek_cache};
use super::{Found, Walked, dataset_name, into_cache, pinned, room, tell_left_out, walk};
use crate::cache::Cache;
use crate::digest::{Block, Sha256};
use crate::error::{Error, Kind};
use crate::glob::texts_of;
use crate::project::lock::{Lock, Locked, LockedFile};
use crate::project::manifest::{self, Manifest, Source};
use crate::project::state::{Placed, State};
use crate::project::{Project, git};
use crate::source::local::output_pin;
use crate::stop::Stop;
use crate::{name, place, plan, units};

/// the folder `start`, a real path, as [`super::in_project`] names it in `project`, whose data
/// folder is `data_dir`: an output's `at`, empty for the project's own folder. none for a path
/// outside the project or one that is not a folder.
pub(super) fn at_of(
    project: &Project,
    data_dir: &str,
    start: &Path,
) -> Result<Option<String>, Error> {
    if !start.is_dir() {
        return Ok(None);
    }
    super::in_project(project, data_dir, start)
}

/// a folder read to be pinned as an output: its name, data.toml's table if it has one, its folder
/// as data.toml names it, its patterns, the walk of it and the cache.
struct Read<'m> {
    name: String,
    entry: Option<&'m manifest::Entry>,
    at: String,
    source: Source,
    walk: Walk,
    cache: Cache,
}

/// what a walk of an output's folder found, the markers it left out, and when it began, which a
/// second walk must find again.
struct Walk {
    walked: Walked,
    markers: HashSet<String>,
    started: u64,
}

/// pins the folder `start`, `shown` as typed, found at `at` in the project, as the output `--as`
/// names, or one named after the folder: new, pinned again once the person says yes to what
/// changed, or in a dry run only compared.
pub(super) async fn pin_held(
    context: &Context,
    adding: &mut AddProject,
    args: &Add,
    (start, shown, at): (&Path, &str, &str),
    here: &Path,
    starting: bool,
) -> Result<Report, Error> {
    let old = adding.old.take();
    adding.project.captured = Some(at.to_owned());
    let adding = &*adding;
    let project = &adding.project;
    let manifest = &adding.manifest;
    let read = read(context, adding, args, (start, shown, at), old.as_ref())?;
    let Read {
        name,
        entry,
        at,
        source,
        walk,
        cache,
    } = read;
    let walked = &walk.walked;
    let pin = entry.and(old.as_ref()).and_then(|lock| lock.get(&name));
    let net = std::sync::Arc::new(context.net());
    if args.dry.dry_run {
        let work = Loop::new(context, project, net, cache).adding(&name);
        let read = (&walk, start, &source);
        return compared(&work, read, (&name, shown, &at), pin);
    }
    room(context, &cache, walked, shown)?;
    let copied = into_cache(context, &cache, walked, &|copies| {
        reread(context, &walk, (start, shown), &source, copies)
    })?;
    let told = read_line(walked, copied.cloned, shown);
    let changes = pin.map(|pin| Changes::of(&copied.files, &pin.files));
    if let Some(changes) = changes.as_ref().filter(|changes| !changes.is_empty()) {
        approved(context, (&name, shown), changes)?;
    }
    adding.seed(seeds(&name, &at, walked, &copied.files));
    adding.git_keeps(&at, git_tracks(&project.root, &at));
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
            let said = match changes {
                Some(changes) => format!("{name} · {}", changes.summary()),
                None => format!("{name} in data.lock, its files are in {folder}"),
            };
            output.done("Pinned", &said);
            (outcome, names.to_vec())
        }
        (None, _) => {
            let new = (name.as_str(), at.as_str(), &source);
            let outcome =
                pin_new(&work, adding, new, (locked, old), (&told, here, starting)).await?;
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

/// the files git users commit only to keep a folder, or its rules, in git.
const HOLDERS: [&str; 3] = [".gitkeep", ".keep", ".gitignore"];

/// whether git tracks files of the person's in the output folder `at` of the project at `root`,
/// besides [`HOLDERS`]: a folder they keep in git, where a marker would hide their next file. read
/// at each pin of an output alone, so no sync runs git.
fn git_tracks(root: &Path, at: &str) -> bool {
    Project::repository(root).is_some()
        && std::env::var_os("PATH")
            .and_then(|path| git::find(Some(&path), root))
            .and_then(|program| git::tracked(&program, root, at))
            .is_some_and(|tracked| {
                tracked.iter().any(|path| {
                    let name = path
                        .rsplit_once('/')
                        .map_or(path.as_str(), |(_, name)| name);
                    !HOLDERS.contains(&name)
                })
            })
}

/// pins `locked`, the new output `name` at `at` with `source`'s patterns, writing its table into
/// data.toml, and tells it as an add of the project `starting` does, `told` its read.
async fn pin_new(
    work: &Loop<'_>,
    adding: &AddProject,
    (name, at, source): (&str, &str, &Source),
    (locked, old): (Locked, Option<Lock>),
    (told, here, starting): (&str, &Path, bool),
) -> Result<Outcome<Placement>, Error> {
    let text = adding.text.as_deref().unwrap_or_default();
    let (select, exclude) = (texts_of(&source.select), texts_of(&source.exclude));
    let updated = manifest::with_output(text, name, at, &select, &exclude)?;
    let manifest = Manifest::parse(&updated)?;
    let names = [name.to_owned()];
    let given = vec![Given::Locked(locked)];
    let outcome = sync_add(
        work,
        adding,
        &manifest,
        Some(&updated),
        old,
        (&names, given),
    )
    .await?;
    let output = &work.context.output;
    output.step("Imported", &format!("{name} · {told}"));
    let has_files = outcome.placed.planned.contains(name);
    render_add(
        work,
        &manifest,
        (name, has_files),
        here,
        (starting, &outcome.placed),
        (&[], None),
    );
    Ok(outcome)
}

/// the folder `start`, `shown` as typed, at `at`, checked and walked for `args`: refused when it
/// cannot be an output's folder or holds another dataset's file or the cache.
fn read<'m>(
    context: &Context,
    adding: &'m AddProject,
    args: &Add,
    (start, shown, at): (&Path, &str, &str),
    old: Option<&Lock>,
) -> Result<Read<'m>, Error> {
    let manifest = &adding.manifest;
    for (given, flag, why) in [
        (
            args.decompress,
            "--decompress",
            "kept as its pipeline wrote it",
        ),
        (
            args.web_page,
            "--web-page",
            "a folder of this project rather than a link",
        ),
    ] {
        if given {
            return Err(Error::new(
                Kind::Usage,
                format!("{shown} is an output, {why}, so it takes no {flag}"),
            )
            .help(format!("leave out {flag}")));
        }
    }
    refuse_folder(manifest, at, shown)?;
    let (name, entry) = output_name(context, manifest, args, at, start, shown)?;
    let writes = !args.dry.dry_run;
    if writes {
        adding.project.check_writable()?;
    }
    let cache = if writes { open_cache()? } else { peek_cache()? };
    refuse_cache(start, shown, cache.root())?;
    refuse_owned(manifest, old, &cache, &name, at, shown)?;
    let source = match entry {
        Some(entry) => {
            refuse_other_patterns(args, &name, &entry.source)?;
            entry.source.clone()
        }
        None => new_source(args)?,
    };
    // data.toml's spelling, which a pin made for it repeats, so the pin answers data.toml
    let at = entry
        .and_then(|entry| entry.at.clone())
        .unwrap_or_else(|| at.to_owned());
    let started = units::nanos(std::time::SystemTime::now());
    let mut walked = walk(
        start,
        shown,
        &source,
        context.settings.most_files.value,
        &context.stop,
    )?;
    // a marker is named .gitignore, so the state is read only for a folder that holds one
    let markers = if walked
        .files
        .iter()
        .any(|file| file.path.ends_with(".gitignore"))
    {
        markers_in(&adding.project.state().0, &at)
    } else {
        HashSet::new()
    };
    leave_out(&mut walked, &markers);
    tell_left_out(&context.output, shown, &walked);
    Ok(Read {
        name,
        entry,
        at,
        source,
        walk: Walk {
            walked,
            markers,
            started,
        },
        cache,
    })
}

/// the paths in the folder `at` of the markers `state` says fetchloom made there, the .gitignore
/// that keeps a dataset folder out of git, which is fetchloom's and never part of an output.
fn markers_in(state: &State, at: &str) -> HashSet<String> {
    let below = format!("{at}/");
    state
        .markers
        .keys()
        .filter_map(|folder| disk::marker(folder).strip_prefix(&below).map(str::to_owned))
        .collect()
}

/// takes the files at `markers` out of `walked`, any other .gitignore in the form fetchloom writes
/// its marker in, as one a renamed folder took along, and git's placeholders, never part of an
/// output.
fn leave_out(walked: &mut Walked, markers: &HashSet<String>) {
    walked.files.retain(|file| {
        let name = file.path.rsplit('/').next().unwrap_or(&file.path);
        let marker = name == ".gitignore" && is_marker_form(&file.source);
        !markers.contains(&file.path) && !name::git_placeholder(name) && !marker
    });
}

/// a dry run: the folder `walk` found at `start` compared with `pin`, read and hashed where it is,
/// writing nothing. a new output's dry run hashes nothing.
fn compared(
    work: &Loop<'_>,
    (walk, start, source): (&Walk, &Path, &Source),
    (name, shown, at): (&str, &str, &str),
    pin: Option<&Locked>,
) -> Result<Report, Error> {
    let context = work.context;
    let walked = &walk.walked;
    if pin.is_none() {
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
    let read: Vec<Sha256> = hashed.iter().map(|file| file.sha256).collect();
    reread(context, walk, (start, shown), source, &read)?;
    let changes = Changes::of(&hashed, pin.map_or(&[][..], |pin| &pin.files));
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
            crate::suggested::command("add ./data/processed --as processed")
        )));
    }
    let said = match manifest::folder(at, "at") {
        Err((reason, _)) => format!("{shown} cannot be an output's folder: {reason}"),
        Ok(at) if manifest::holds_data(&at, &manifest.data_dir) => format!(
            "{shown} holds the data folder {}, where fetched datasets land, so it cannot be an output's folder",
            manifest.data_dir
        ),
        Ok(_) => return Ok(()),
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
            crate::suggested::command(&output_pin(owner, shown))
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

/// refuses patterns on the command line for an output data.toml has, `source`, but those it
/// keeps: a pipeline that runs the same add each time gives them again. data.toml's stay its own.
fn refuse_other_patterns(args: &Add, name: &str, source: &Source) -> Result<(), Error> {
    let given = |typed: &[String], kept: &[crate::glob::Glob]| {
        let mut typed = typed.to_vec();
        let mut kept = texts_of(kept);
        typed.sort_unstable();
        kept.sort_unstable();
        typed.is_empty() || typed == kept
    };
    if given(&args.filter.select, &source.select) && given(&args.filter.exclude, &source.exclude) {
        return Ok(());
    }
    Err(Error::new(
        Kind::Usage,
        format!(
            "{name} is in data.toml already, with other --select and --exclude patterns than these"
        ),
    )
    .help(format!(
        "leave out --select and --exclude to keep data.toml's, or change them in datasets.{name} in data.toml"
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

/// refuses the files `walk` found in `start` once a second walk finds them otherwise: one more or
/// fewer, or one of another size, time or file, or once a file a coarse clock leaves in doubt
/// holds other bytes than were `read`, the digests in the files' order, so what was read is no
/// state the folder held. files written apart may still belong to no single run, which no walk
/// can tell.
fn reread(
    context: &Context,
    walk: &Walk,
    (start, shown): (&Path, &str),
    source: &Source,
    read: &[Sha256],
) -> Result<(), Error> {
    // so a CLI test can change the folder between the read and its second walk
    #[cfg(debug_assertions)]
    crate::stop::hold("FETCHLOOM_TEST_HOLD_CAPTURE");
    let most = context.settings.most_files.value;
    let again = super::walk(start, shown, source, most, &context.stop).map(|mut again| {
        leave_out(&mut again, &walk.markers);
        again
    });
    let now = |file: &Found| {
        let block = Block::sized(file.size.saturating_add(1));
        super::super::sync::disk::hash_plain(&file.source, block).ok()
    };
    if again.is_ok_and(|again| same_files(&again, &walk.walked))
        && !rewritten_unseen(&walk.walked, read, walk.started, context.threads, &now)
    {
        return Ok(());
    }
    Err(Error::new(
        Kind::ProjectChanged,
        format!("{shown} changed while it was read: let the pipeline finish, then add again"),
    )
    .help("fetchloom pins a folder only as it stood the whole time it read it, and pinned nothing"))
}

/// whether a file of `walked` that its clock leaves in doubt holds other bytes `now` than were
/// `read`, in the files' order. a file whose time [`disk::may_hide_a_write`] after the walk
/// `started`, by the system's clock, is in doubt, so no second walk tells; an older one's size
/// and time vouch for it, and it is not read again.
fn rewritten_unseen(
    walked: &Walked,
    read: &[Sha256],
    started: u64,
    threads: crate::threads::Threads,
    now: &(dyn Fn(&Found) -> Option<Sha256> + Sync),
) -> bool {
    let doubted: Vec<(&Found, &Sha256)> = walked
        .files
        .iter()
        .zip(read)
        .filter(|(file, _)| disk::may_hide_a_write(file.seen.look.modified, started, true))
        .collect();
    place::run_all(&doubted, threads, |(file, read)| now(file) != Some(**read))
        .into_iter()
        .any(|changed| changed)
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

/// each of `files` by its path, with its pin.
fn pins(files: &[LockedFile]) -> Vec<(&str, Pinned)> {
    files
        .iter()
        .map(|file| (file.path.as_str(), Pinned::of(file)))
        .collect()
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
    /// how `found` differs from `pinned`, both sorted by path, as diff tells two pins apart.
    pub(super) fn of(found: &[LockedFile], pinned: &[LockedFile]) -> Changes {
        let mut changes = Changes::default();
        let size = |pin: Option<Pinned>| pin.map_or(0, |pin| pin.size);
        for file in diff::files(pins(pinned), pins(found)) {
            match file.change {
                Change::Changed => {
                    let sizes = (size(file.before), size(file.after));
                    changes.changed.push((file.path, sizes.0, sizes.1));
                }
                Change::Added => changes.added.push((file.path, size(file.after))),
                Change::Removed => changes.removed.push((file.path, size(file.before))),
            }
        }
        changes
    }

    pub(super) fn is_empty(&self) -> bool {
        self.changed.is_empty() && self.added.is_empty() && self.removed.is_empty()
    }

    /// `1 file changed, 2 added, 1 removed`, leaving out what is none.
    pub(super) fn summary(&self) -> String {
        let count = |files: usize| files as u64;
        super::super::sync::place::changes_said(
            count(self.changed.len()),
            count(self.added.len()),
            count(self.removed.len()),
        )
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
    let again = crate::suggested::command(&format!("{} --yes", output_pin(name, shown)));
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
                "run `{again}` to pin what it holds now, or `{}` to make it hold just the files {name} pins",
                crate::suggested::command(&format!("sync --force {name}"))
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
    use crate::project::state::Look;

    /// a walk that found `files`, each a path with its size, time and inode.
    fn walked(files: &[(&str, u64, u64, u64)]) -> Walked {
        Walked {
            files: files
                .iter()
                .map(|&(path, size, modified, inode)| Found {
                    path: path.to_owned(),
                    source: std::path::PathBuf::from(path),
                    size,
                    seen: super::super::Seen {
                        look: Look { size, modified },
                        node: (inode, 0, 0),
                        writable: true,
                    },
                })
                .collect(),
            ..Walked::default()
        }
    }

    /// a second walk matches the first only with the same files, each as it was: one more, one
    /// fewer, or one of another size, time or inode is a folder that changed while it was read.
    #[test]
    fn a_second_walk_matches_only_the_same_files_as_they_were() {
        let first = walked(&[("a.csv", 3, 10, 1), ("b.csv", 4, 20, 2)]);
        assert!(same_files(
            &first,
            &walked(&[("a.csv", 3, 10, 1), ("b.csv", 4, 20, 2)])
        ));
        for (changed, how) in [
            (walked(&[("a.csv", 3, 10, 1)]), "one went"),
            (
                walked(&[
                    ("a.csv", 3, 10, 1),
                    ("b.csv", 4, 20, 2),
                    ("c.csv", 1, 30, 3),
                ]),
                "one came",
            ),
            (
                walked(&[("a.csv", 3, 10, 1), ("c.csv", 4, 20, 2)]),
                "one renamed",
            ),
            (
                walked(&[("a.csv", 3, 11, 1), ("b.csv", 4, 20, 2)]),
                "a later time",
            ),
            (
                walked(&[("a.csv", 3, 10, 1), ("b.csv", 5, 20, 2)]),
                "another size",
            ),
            (
                walked(&[("a.csv", 3, 10, 9), ("b.csv", 4, 20, 2)]),
                "another file",
            ),
        ] {
            assert!(!same_files(&first, &changed), "{how}");
        }
    }

    /// a file written in its clock's last step before the walk is read again, as such a clock
    /// keeps its size and time through a rewrite in that step: other bytes than were read are a
    /// change. a time on a whole hundredth, as fat, exfat and hfs+ keep, may stand for any time in
    /// two seconds; any other was kept within a clock's tick. an older file's size and time vouch
    /// for it, so it is never read again.
    #[test]
    fn a_file_written_in_the_clocks_last_tick_is_read_again() {
        let second = 1_000_000_000;
        let started = 100 * second;
        let first = walked(&[
            ("old.csv", 3, started - 3 * second, 1),
            ("edge.csv", 3, started - 2 * second, 2),
            ("new.csv", 3, started - second, 3),
            ("fine.csv", 3, started - second + 7, 4),
            ("tick.csv", 3, started - 15_000_007, 5),
        ]);
        let read =
            [b"old", b"edg", b"new", b"fin", b"tic"].map(|bytes| crate::digest::sha256(bytes));
        let reads = |now: [&[u8]; 5]| {
            let asked = std::sync::Mutex::new(Vec::new());
            let threads = crate::threads::Threads::default();
            let changed = rewritten_unseen(&first, &read, started, threads, &|file: &Found| {
                asked.lock().unwrap().push(file.path.clone());
                let at = first
                    .files
                    .iter()
                    .position(|found| found.path == file.path)?;
                Some(crate::digest::sha256(now[at]))
            });
            let mut asked = asked.into_inner().unwrap();
            asked.sort();
            (changed, asked)
        };
        let doubted = ["edge.csv", "new.csv", "tick.csv"]
            .map(str::to_owned)
            .to_vec();
        let same = [b"old", b"edg", b"new", b"fin", b"tic"].map(|bytes| &bytes[..]);
        assert_eq!(reads(same), (false, doubted.clone()));
        for (at, other) in [(0, &b"OLD"[..]), (3, b"FIN")] {
            let mut now = same;
            now[at] = other;
            assert_eq!(reads(now), (false, doubted.clone()), "{at}");
        }
        for (at, other) in [(1, &b"EDG"[..]), (2, b"NEW"), (4, b"TIC")] {
            let mut now = same;
            now[at] = other;
            assert_eq!(reads(now), (true, doubted.clone()), "{at}");
        }
    }

    /// the second walk reads a file of the clock's last tick again: bytes other than the copy's
    /// are a change though the file looks as it did, and the copy's bytes are none.
    #[test]
    fn the_second_walk_reads_a_file_of_the_last_tick_again() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("processed");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("clean.csv"), b"a,b\n").unwrap();
        let context = crate::cli::project::tests::offline_context();
        let text = manifest::with_output("", "processed", "processed", &[], &[]).unwrap();
        let source = Manifest::parse(&text).unwrap().datasets["processed"]
            .source
            .clone();
        let most = crate::net::MOST_FILES;
        let walked =
            super::super::walk(&folder, "processed", &source, most, &context.stop).unwrap();
        let started = walked.files[0].seen.look.modified + 1_000_000;
        let walk = Walk {
            walked,
            markers: HashSet::new(),
            started,
        };
        let shown = (folder.as_path(), "processed");
        let read = |bytes: &[u8]| {
            reread(
                &context,
                &walk,
                shown,
                &source,
                &[crate::digest::sha256(bytes)],
            )
        };
        assert!(read(b"a,b\n").is_ok());
        assert_eq!(read(b"a,c\n").unwrap_err().kind, Kind::ProjectChanged);
    }

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
