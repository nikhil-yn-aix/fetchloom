//! `sync` and `lock`, and the steps the other project commands share with them: finding the
//! project, taking its lock and the run that brings data.lock, the cache and the data folder in
//! line. each command reads the project, takes its lock when it changes something, and leaves the
//! order of work to the sync loop.

mod conflicts;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tracing::Instrument as _;

use super::args;
use super::sync::disk::{self, Survey};
use super::sync::place::{self, Changes, Download, Placement, Preview, Seen};
use super::sync::{self as engine, Given, Loop, Scope, Upgrade};
use super::{Context, open_cache, peek_cache};
use crate::digest;
use crate::error::{Error, Kind};
use crate::name;
use crate::output::Output;
use crate::plan;
use crate::project::lock::merge;
use crate::project::lock::{self, Drift, Lock};
use crate::project::manifest::Manifest;
use crate::project::state::Synced;
use crate::project::{Held, Hold, Project, git};
use crate::recipe::{self, Change};
use crate::source::{Listing, local};
use crate::units;

/// what a project command did, or with `--dry-run` would do, as `--json` prints it.
#[derive(Default, Serialize)]
pub(super) struct Report {
    pub(super) project: PathBuf,
    /// for `sync`, the datasets it was asked for: their names, or null for every dataset.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) selected: Option<Scope>,
    /// datasets added, removed or upgraded.
    pub(super) datasets: Vec<String>,
    pub(super) lock_changed: bool,
    pub(super) placement: Option<Placement>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(super) dry_run: bool,
    /// with `--dry-run`, the paths that would change.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) changes: Option<Changes>,
    /// how each dataset came out when `lock` merged both sides of a git merge.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) merge: Option<merge::Summary>,
}

impl Report {
    /// what a dry run found: whether data.lock would change and what placing would do.
    pub(super) fn dry(project: PathBuf, datasets: Vec<String>, outcome: Outcome<Preview>) -> Self {
        Report {
            project,
            selected: None,
            datasets,
            lock_changed: outcome.lock_changed,
            placement: Some(outcome.placed.placement),
            dry_run: true,
            changes: Some(outcome.placed.changes),
            merge: None,
        }
    }
}

pub(super) fn preview_ignore(work: &Loop<'_>, manifest: &Manifest) -> Result<(), Error> {
    if work.project.ignored_in_git(&manifest.data_dir)?.is_some() {
        work.context.output.step(
            "Would add",
            &format!(
                "{}/ to .gitignore, since its files come from data.lock",
                manifest.data_dir
            ),
        );
    }
    Ok(())
}

pub(super) fn ignore_data(work: &Loop<'_>, manifest: &Manifest) -> Result<(), Error> {
    if work.project.ignore_in_git(&manifest.data_dir)? {
        work.context.output.step(
            "Ignored",
            &format!(
                "{}/ in .gitignore, since its files come from data.lock",
                manifest.data_dir
            ),
        );
    }
    Ok(())
}

/// ends a run that wrote lists anew into data.lock.d that git does not have, which `git commit
/// -a` would leave out and a clone then misses: a `next:` line for a person at a terminal, else a
/// warning, which scripts and `--json` read too. it says nothing when git has them, and with no
/// git or outside a repository.
pub(super) fn remind_lists(context: &Context) {
    let written = std::mem::take(
        &mut *context
            .lists
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    );
    let Some((root, _)) = written.first() else {
        return;
    };
    let missing = untracked(root, written.iter().map(|(_, part)| part.as_str()));
    let named = match missing.as_slice() {
        [] => return,
        [one] => one.clone(),
        [first, ..] => format!(
            "{} lists data.lock names now, such as {first}",
            missing.len()
        ),
    };
    let output = &context.output;
    let since = if missing.len() == 1 {
        ", which data.lock names now"
    } else {
        ""
    };
    if output.shows_next() {
        output.next(&format!(
            "run `git add data.lock.d` before you commit: git does not have {named}{since}"
        ));
    } else {
        output.warn_with(
            &format!("git does not have {named}{since}"),
            "run `git add data.lock.d` before you commit, since `git commit -a` leaves new files out",
        );
    }
}

/// the parts of `parts`, lists below the project at `root`, that are there and that git does not
/// have, sorted. none when git cannot tell, as with no git or outside a repository.
pub(super) fn untracked<'a>(root: &Path, parts: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut parts: Vec<&str> = parts.filter(|part| root.join(part).is_file()).collect();
    if parts.is_empty() {
        return Vec::new();
    }
    let Some(tracked) = std::env::var_os("PATH")
        .and_then(|path| git::find(Some(&path), root))
        .and_then(|program| git::tracked(&program, root, lock::lists::FOLDER))
    else {
        return Vec::new();
    };
    parts.sort_unstable();
    parts.dedup();
    parts
        .into_iter()
        .filter(|part| !tracked.contains(*part))
        .map(str::to_owned)
        .collect()
}

/// the project files a person commits: data.lock.d as well when a recipe's result or a big
/// dataset keeps its file list there, since a clone without it cannot read data.lock, and in a
/// git repository the .gitignore beside data.toml, where fetchloom keeps the data folder out.
pub(super) fn committed(project: &Project) -> &'static str {
    let lists = project.root.join(lock::lists::FOLDER).is_dir();
    let ignore =
        Project::repository(&project.root).is_some() && project.root.join(".gitignore").is_file();
    match (lists, ignore) {
        (true, true) => "data.toml, data.lock, data.lock.d and .gitignore",
        (true, false) => "data.toml, data.lock and data.lock.d",
        (false, true) => "data.toml, data.lock and .gitignore",
        (false, false) => "data.toml and data.lock",
    }
}

/// `fetchloom`, then `-C` and the folder the run worked in when its command line gave `-C`, then
/// `words`: a command a help names, which then finds the same project from whatever folder a
/// person runs it in. the folder is quoted for a shell as typed commands are.
pub(super) fn command(words: &str) -> String {
    static DIRECTORY: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    // the run changed into the folder -C named, so only the command line still says it did
    let directory = DIRECTORY.get_or_init(|| {
        let typed = args::read().ok()?.global.directory?;
        let folder = std::env::current_dir().unwrap_or(typed);
        Some(super::shell_word(&folder.to_string_lossy()).into_owned())
    });
    match directory {
        Some(directory) => format!("fetchloom -C {directory} {words}"),
        None => format!("fetchloom {words}"),
    }
}

/// the most files of recipe code a warning names in its `git add`; `-v` names each one.
const NAMED_CODE: usize = 5;

/// a file of a recipe's code git does not have: its path from the project's folder, the code
/// entry of data.toml that holds it and the recipes that name that entry.
struct Uncommitted<'m> {
    file: String,
    code: &'m str,
    recipes: &'m [&'m str],
}

/// warns of the files a recipe's code in data.toml takes in that git does not have, which a
/// commit then leaves out, so a clone cannot build the recipe from the same code: new files, as
/// a new file in a code folder, apart from those .gitignore leaves out. `-v` names each file.
/// it asks git once, and says nothing with no git, outside a repository, or when git has them
/// all.
pub(super) fn warn_recipe_code(output: &Output, project: &Project, manifest: &Manifest) {
    let mut code: Vec<(&str, Vec<&str>)> = Vec::new();
    for (name, entry) in &manifest.datasets {
        for path in entry.recipe().map_or(&[][..], |recipe| &recipe.code[..]) {
            match code.iter_mut().find(|(seen, _)| seen == path) {
                Some((_, recipes)) => recipes.push(name),
                None => code.push((path, vec![name])),
            }
        }
    }
    let files: Vec<Uncommitted<'_>> = code
        .iter()
        .flat_map(|(path, recipes)| {
            // code a build would refuse or cannot read is the build's to tell
            recipe::code::files(&project.root, recipes[0], path)
                .unwrap_or_default()
                .into_iter()
                .map(|file| Uncommitted {
                    file,
                    code: path,
                    recipes,
                })
        })
        .collect();
    if files.is_empty() {
        return;
    }
    let paths: Vec<&str> = code.iter().map(|(path, _)| *path).collect();
    let Some(known) = std::env::var_os("PATH")
        .and_then(|path| git::find(Some(&path), &project.root))
        .and_then(|program| git::known(&program, &project.root, &paths))
    else {
        return;
    };
    let (untracked, ignored): (Vec<Uncommitted<'_>>, Vec<Uncommitted<'_>>) = files
        .into_iter()
        .filter(|file| !in_git(&known.tracked, &file.file, ""))
        .partition(|file| in_git(&known.untracked, &file.file, "/"));
    for (files, ignored) in [(untracked, false), (ignored, true)] {
        let Some((said, help)) = uncommitted(&files, ignored) else {
            continue;
        };
        output.warn_with(&said, &help);
        if files.len() > 1 {
            for file in &files {
                output.detail(&file.file);
            }
        }
    }
}

/// whether `listed` holds `file`, or a folder above it, written with `end`, as git lists a
/// submodule or a repository inside the work tree whole.
fn in_git(listed: &std::collections::HashSet<String>, file: &str, end: &str) -> bool {
    listed.contains(file)
        || file
            .match_indices('/')
            .any(|(at, _)| listed.contains(&format!("{}{end}", &file[..at])))
}

/// the warning and its help for `files`, recipe code git does not have, which .gitignore leaves
/// out when `ignored`.
fn uncommitted(files: &[Uncommitted<'_>], ignored: bool) -> Option<(String, String)> {
    let first = files.first()?;
    let mut recipes: Vec<&str> = files
        .iter()
        .flat_map(|file| file.recipes.iter().copied())
        .collect();
    recipes.sort_unstable();
    recipes.dedup();
    let recipes = match recipes.split_last() {
        Some((last, rest)) if !rest.is_empty() => format!("{} and {last}", rest.join(", ")),
        _ => recipes.join(""),
    };
    let what = match files.len() {
        1 => format!("{}, code of {recipes}", first.file),
        count => format!(
            "{count} files of the code of {recipes}, such as {}",
            first.file
        ),
    };
    let it = if files.len() == 1 { "it" } else { "them" };
    let words = |paths: &mut dyn Iterator<Item = &str>| {
        let mut words: Vec<String> = paths
            .map(|path| super::shell_word(path).into_owned())
            .collect();
        words.dedup();
        words.join(" ")
    };
    let since = format!("a clone without {it} cannot build {recipes} from the same code");
    if ignored {
        let add = if files.len() <= NAMED_CODE {
            format!(
                "`git add -f {}`",
                words(&mut files.iter().map(|file| &*file.file))
            )
        } else {
            format!(
                "`git add -f`, as `git add -f {}`",
                super::shell_word(&first.file)
            )
        };
        return Some((
            format!(".gitignore leaves out {what}"),
            format!("{since}: commit {it} with {add}, or move {it} out of the recipe's code"),
        ));
    }
    let add = if files.len() <= NAMED_CODE {
        words(&mut files.iter().map(|file| &*file.file))
    } else {
        words(&mut files.iter().map(|file| file.code))
    };
    Some((
        format!("git does not have {what}"),
        format!("run `git add {add}`, then commit {it}, since {since}"),
    ))
}

pub(super) async fn sync(context: &Context, args: args::Sync) -> Result<Report, Error> {
    let project = find(&context.output)?;
    let dry_run = args.dry.dry_run;
    // a dry run changes nothing, so like `lock --check` it needs no hold
    let _held = if dry_run {
        None
    } else {
        match hold(context, &project, "sync").await {
            Ok(held) => Some(held),
            // a checkout this user may not write, as a job's read-only mount, still answers a
            // sync that would write nothing
            Err(error) if error.kind == Kind::LocalIo => {
                return match settled_unheld(context, &project, &args)? {
                    Some(report) => Ok(report),
                    None => Err(error),
                };
            }
            Err(error) => return Err(error),
        }
    };
    let (text, manifest) = project.manifest()?;
    let scope = selected(&manifest, &args.names, "sync")?;
    let read = project.lock_and_text()?;
    if args.locked {
        locked(
            &project,
            &manifest,
            read.as_ref().map(|(lock, _)| lock),
            "sync without --locked",
            &scope,
        )?;
    }
    let seen = Seen {
        manifest: digest::sha256(text.as_bytes()),
        lock: read
            .as_ref()
            .map(|(_, text)| digest::sha256(text.as_bytes())),
    };
    if dry_run {
        return preview_sync(context, &project, &manifest, read, &seen, &args, scope).await;
    }
    let old = read.map(|(lock, _)| lock);
    let work = Loop::new(context, &project, Arc::new(context.net()), open_cache()?)
        .within(scope)
        .stopping_on_unbuilt(args.locked);
    let Outcome {
        lock_changed,
        pins,
        placed: placement,
    } = sync_all(
        &work,
        &manifest,
        old,
        None,
        Run {
            upgrade: Upgrade::Nothing,
            force: args.force,
            verify: args.verify,
            read: Read::Settled(&seen),
        },
    )
    .await?;
    // a project that became a git repository after its add gets its ignore rule now
    ignore_data(&work, &manifest)?;
    // a locked run places what the sources serve before it stops for the results only a
    // build makes, so a job still checks every source
    let stops = args.locked && !placement.unbuilt.is_empty();
    if !stops {
        tell_synced(
            context,
            &project,
            &manifest,
            &work.scope,
            lock_changed,
            &placement,
            args.verify,
        );
    }
    unbuilt_locked(
        stops,
        Report {
            selected: Some(work.scope.clone()),
            project: project.root,
            datasets: pins,
            lock_changed,
            placement: Some(placement),
            ..Report::default()
        },
    )
}

/// `report`, or when the run `stops` for the recipes whose pinned results its placement left
/// out, the `build.needed` error for them, which keeps it.
fn unbuilt_locked(stops: bool, report: Report) -> Result<Report, Error> {
    let unbuilt = report
        .placement
        .as_ref()
        .map_or(&[][..], |placement| &placement.unbuilt);
    if !stops || unbuilt.is_empty() {
        return Ok(report);
    }
    let error = engine::unbuilt_here(unbuilt);
    Err(match serde_json::to_value(&report) {
        Ok(value) => error.with_result(value),
        Err(_) => error,
    })
}

/// the datasets `names` asks `command` for: every one when it names none. a name data.toml does
/// not have is refused before anything else happens, with the closest name it has.
pub(super) fn selected(
    manifest: &Manifest,
    names: &[String],
    command: &str,
) -> Result<Scope, Error> {
    if names.is_empty() {
        return Ok(Scope::All);
    }
    for name in names {
        known(manifest, name, command)?;
    }
    Ok(Scope::Only(names.iter().cloned().collect()))
}

/// a sync that would write nothing, found without the project's hold: data.toml and data.lock
/// are byte for byte as the last complete sync left them, every file still looks as placed, no
/// doubt is open, and the ignore rule is in place. none when a sync would write anything, which
/// needs the hold.
fn settled_unheld(
    context: &Context,
    project: &Project,
    args: &args::Sync,
) -> Result<Option<Report>, Error> {
    let (text, manifest) = project.manifest()?;
    let scope = selected(&manifest, &args.names, "sync")?;
    let Some((lock, lock_text)) = project.lock_and_text()? else {
        return Ok(None);
    };
    if args.locked {
        locked(
            project,
            &manifest,
            Some(&lock),
            "sync without --locked",
            &scope,
        )?;
    }
    let seen = Seen {
        manifest: digest::sha256(text.as_bytes()),
        lock: Some(digest::sha256(lock_text.as_bytes())),
    };
    // the cache, unlike the project, may well be writable, and a project listed there needs no
    // write. a cache this user cannot hold either is only read, as a dry run reads it
    let (cache, held) = match open_cache() {
        Ok(cache) => (cache, true),
        Err(_) => (peek_cache()?, false),
    };
    let mut survey = Survey::read(project, &manifest.data_dir, context.threads);
    // hashing proved the files placed too recently to trust by time unchanged, and only asks
    // for the state to be written again, which this project cannot be. an edit hashing found
    // stays an edit
    survey.needs_writing = false;
    survey.check_doubted(&project.root, &cache)?;
    if args.verify {
        survey.verify(&project.root, &scope, Some(&context.output))?;
    }
    let Some(placement) = place::untouched(project, &manifest, &survey, &seen, false, &scope)?
    else {
        return Ok(None);
    };
    if project.ignored_in_git(&manifest.data_dir)?.is_some() {
        return Ok(None);
    }
    if held && let Err(err) = cache.register(&project.root) {
        tracing::debug!(%err, "could not list a read-only project in the cache");
    }
    tell_synced(
        context,
        project,
        &manifest,
        &scope,
        false,
        &placement,
        args.verify,
    );
    Ok(Some(Report {
        project: project.root.clone(),
        selected: Some(scope),
        datasets: Vec::new(),
        lock_changed: false,
        placement: Some(placement),
        ..Report::default()
    }))
}

/// the last lines of a sync of `scope`: nothing to do, or what it synced and what to do next.
fn tell_synced(
    context: &Context,
    project: &Project,
    manifest: &Manifest,
    scope: &Scope,
    lock_changed: bool,
    placement: &Placement,
    verified: bool,
) {
    if !lock_changed
        && placement.placed + placement.replaced + placement.removed == 0
        && placement.kept.is_empty()
        && placement.unbuilt.is_empty()
        && placement.adopted.is_empty()
    {
        let found = in_place(manifest, scope, verified);
        context.output.done(
            "Ok",
            &if verified {
                format!("{found}; nothing to do")
            } else {
                format!("{found}, nothing to do")
            },
        );
    } else {
        let (verb, text) = synced(manifest, scope, placement.kept.len(), &placement.unbuilt);
        context.output.done(verb, &text);
        // `path` answers only a dataset placed in the data folder
        let placed = |name: &&String| manifest.datasets[*name].place;
        let first = match scope {
            Scope::All => manifest
                .datasets
                .keys()
                .filter(placed)
                .find(|name| !placement.unbuilt.contains(*name)),
            Scope::Only(names) => names.iter().find(placed),
        };
        if lock_changed {
            context.output.next(&format!(
                "commit {}, so other clones use these pins",
                committed(project)
            ));
        } else if let Some(name) = first
            && placement.placed + placement.replaced > 0
        {
            context.output.next(&format!(
                "`fetchloom path {name}` tells your scripts where {name} is"
            ));
        }
    }
}

/// the last line of a sync of `scope` that changed something, as its verb and text: what it
/// synced, with how many other datasets it left as they were, how many files, `kept`, it left
/// unchanged and the recipes whose results it left out, `unbuilt`. a sync of every dataset counts
/// those it placed, so neither those left out nor those with `place = false`, and one that placed
/// none says there was nothing to place, and until which build.
fn synced(
    manifest: &Manifest,
    scope: &Scope,
    kept: usize,
    unbuilt: &[String],
) -> (&'static str, String) {
    let kept = match kept {
        0 => String::new(),
        kept => format!(
            ", {}; see the warnings above",
            units::count(kept as u64, "file left unchanged", "files left unchanged")
        ),
    };
    let (datasets, others) = match scope {
        Scope::All => {
            let placed = manifest
                .datasets
                .iter()
                .filter(|(name, entry)| entry.place && !unbuilt.contains(*name))
                .count();
            if placed == 0 {
                let until = match unbuilt {
                    [] => String::new(),
                    [recipe] => format!(" until {recipe} is built"),
                    many => format!(
                        " until {} are built",
                        units::count(many.len() as u64, "result", "results")
                    ),
                };
                let data = &manifest.data_dir;
                return ("Ok", format!("nothing to place in {data}{until}{kept}"));
            }
            (units::count(placed as u64, "dataset", "datasets"), 0)
        }
        Scope::Only(names) => (
            named(names),
            manifest
                .datasets
                .keys()
                .filter(|name| !names.contains(*name))
                .count(),
        ),
    };
    let mut said = format!("{datasets} into {}", manifest.data_dir);
    if others > 0 {
        let others = units::count(
            others as u64,
            "other dataset left as it was",
            "other datasets left as they were",
        );
        said = format!("{said} · {others}");
    }
    if !unbuilt.is_empty() {
        let left = units::count(unbuilt.len() as u64, "result", "results");
        said = format!("{said} · {left} left out until built");
    }
    ("Synced", format!("{said}{kept}"))
}

/// what a sync of `scope` with nothing to do found in place: every pinned file, or the named
/// datasets' files, by size and time or, `verified`, byte for byte. a dataset with `place = false`
/// has no files in the data folder, so it is named as not placed instead.
fn in_place(manifest: &Manifest, scope: &Scope, verified: bool) -> String {
    let placed = |name: &String| manifest.datasets.get(name).is_some_and(|entry| entry.place);
    let (files, unplaced) = match scope {
        Scope::All if !manifest.datasets.keys().any(placed) => {
            return format!("nothing to place in {}", manifest.data_dir);
        }
        Scope::All => ("all pinned files".to_owned(), None),
        Scope::Only(names) => {
            let (shown, unplaced): (BTreeSet<String>, BTreeSet<String>) =
                names.iter().cloned().partition(placed);
            let unplaced = (!unplaced.is_empty()).then(|| {
                let are = if unplaced.len() == 1 { "is" } else { "are" };
                format!("{} {are} not placed", named(&unplaced))
            });
            if shown.is_empty() {
                let unplaced = unplaced.unwrap_or_default();
                return format!("{unplaced} · place = false in data.toml");
            }
            (format!("the pinned files of {}", named(&shown)), unplaced)
        }
    };
    let found = if verified {
        format!("{files} are in place, checked byte for byte")
    } else {
        format!("{files} are in place by size and time")
    };
    match unplaced {
        Some(unplaced) => format!("{found} · {unplaced}, place = false in data.toml"),
        None => found,
    }
}

/// the datasets a sync was asked for, as its last line names them: the one name, or how many.
fn named(names: &BTreeSet<String>) -> String {
    match names.iter().next() {
        Some(only) if names.len() == 1 => only.clone(),
        _ => units::count(names.len() as u64, "dataset", "datasets"),
    }
}

/// `sync --dry-run` of `scope`: what syncing would change, given data.lock as `read` and both
/// files as `seen`.
async fn preview_sync(
    context: &Context,
    project: &Project,
    manifest: &Manifest,
    read: Option<(Lock, String)>,
    seen: &Seen,
    args: &args::Sync,
    scope: Scope,
) -> Result<Report, Error> {
    let old = read.map(|(lock, _)| lock);
    let work = Loop::new(context, project, Arc::new(context.net()), peek_cache()?)
        .within(scope)
        .stopping_on_unbuilt(args.locked);
    let outcome = preview_all(
        &work,
        manifest,
        old,
        args.force,
        args.verify,
        Vec::new(),
        Some(seen),
    )
    .await?;
    preview_ignore(&work, manifest)?;
    let nothing = !outcome.lock_changed && outcome.placed.changes_nothing();
    let stops = args.locked && !outcome.placed.placement.unbuilt.is_empty();
    if !stops {
        dry_run_done(
            &context.output,
            nothing
                .then(|| in_place(manifest, &work.scope, args.verify))
                .as_deref(),
        );
    }
    let pins = outcome.pins.clone();
    unbuilt_locked(
        stops,
        Report {
            selected: Some(work.scope.clone()),
            ..Report::dry(project.root.clone(), pins, outcome)
        },
    )
}

pub(super) async fn lock(context: &Context, args: args::Lock) -> Result<Report, Error> {
    let project = find(&context.output)?;
    // a check or a dry run changes nothing, so it needs no hold. anything else reads what it
    // changes only once no other fetchloom can change it
    let _held = if args.check || args.dry.dry_run {
        None
    } else {
        Some(hold(context, &project, "lock").await?)
    };
    // a check or an upgrade reads both files as they are, and so says to merge them first
    if !args.check
        && args.upgrade.is_none()
        && let Some(report) = conflicts::merge(context, &project, &args)?
    {
        return Ok(report);
    }
    let (_, manifest) = project.manifest()?;
    let old = project.lock()?;
    if args.check {
        return check(&project, &manifest, old.as_ref(), &Scope::All);
    }
    let upgrade = match args.upgrade {
        None => Upgrade::Nothing,
        Some(names) if names.is_empty() => {
            tell_imports_kept(&context.output, &manifest);
            Upgrade::All
        }
        Some(names) => {
            for name in &names {
                known(&manifest, name, "lock --upgrade")?;
                if manifest.datasets[name].is_import() {
                    return Err(local::no_newer(name));
                }
            }
            Upgrade::Only(names.into_iter().collect::<BTreeSet<_>>())
        }
    };
    if args.dry.dry_run {
        return preview_lock(context, &project, &manifest, old, &upgrade).await;
    }
    let work = Loop::new(context, &project, Arc::new(context.net()), open_cache()?);
    let (lock, before) = work.relock(&manifest, old, &upgrade, Vec::new()).await?;
    // a stop asked while the datasets were resolved leaves data.lock as it was
    context.stop.check()?;
    context.stop.finish();
    // lock keeps a recipe's pin, which only a build moves, so it says which ones moved
    let stale = work.tell_stale(&manifest, &lock);
    let changed = work.write_lock(&before, &lock)?.is_some();
    work.drop_unpacked(&lock);
    engine::tell_upgrades(
        &context.output,
        &before,
        &lock,
        "Upgraded",
        &std::collections::HashSet::new(),
    );
    let pins = engine::repinned(&before, &lock);
    if changed {
        context.output.done("Locked", "data.toml into data.lock");
        context.output.next(&format!(
            "run `fetchloom sync` to update {}/, then commit {}",
            manifest.data_dir,
            committed(&project)
        ));
    } else if stale.is_empty() {
        context
            .output
            .done("Ok", "data.lock is already up to date, nothing to do");
    } else {
        context.output.done("Ok", &unbuilt_pins(&stale));
    }
    Ok(Report {
        project: project.root,
        datasets: pins,
        lock_changed: changed,
        placement: None,
        ..Report::default()
    })
}

/// what a lock that changed nothing says of data.lock while the recipes `stale` need a build,
/// so it never calls data.lock up to date.
fn unbuilt_pins(stale: &[String]) -> String {
    let names = match stale {
        [first @ .., last] if !first.is_empty() => format!("{} and {last} need", first.join(", ")),
        _ => format!("{} needs", stale.join("")),
    };
    format!("data.lock pins the fetched datasets as data.toml asks; {names} a build")
}

/// says which datasets an upgrade of all leaves as they are: imports, which no source has a newer
/// version of.
fn tell_imports_kept(output: &Output, manifest: &Manifest) {
    let imported: Vec<&str> = manifest
        .datasets
        .iter()
        .filter(|(_, entry)| entry.is_import())
        .map(|(name, _)| name.as_str())
        .collect();
    let names = match imported.as_slice() {
        [] => return,
        [first @ .., last] if !first.is_empty() => format!("{} and {last}", first.join(", ")),
        _ => imported.join(""),
    };
    output.step(
        "Skipped",
        &format!("{names}, imported from files, which no source has a newer version of"),
    );
}

/// `lock --dry-run`: what locking would change, found without downloading, unpacking or
/// writing. a file no checksum pins is planned with the pin it had and listed as a download,
/// since only its bytes can tell whether it changed.
async fn preview_lock(
    context: &Context,
    project: &Project,
    manifest: &Manifest,
    old: Option<Lock>,
    upgrade: &Upgrade,
) -> Result<Report, Error> {
    let work = Loop::new(context, project, Arc::new(context.net()), peek_cache()?);
    let dry = work.relock_dry(manifest, old, upgrade, Vec::new()).await?;
    let output = &context.output;
    let stale = work.tell_stale(manifest, &dry.lock);
    let changed = !dry.before.holds(&dry.lock);
    if changed {
        engine::tell_lock(
            output,
            &dry.before,
            &dry.lock,
            engine::Verbs::WOULD,
            None,
            &engine::sizeless(&dry.jobs),
        );
    }
    engine::tell_upgrades(
        output,
        &dry.before,
        &dry.lock,
        "Would upgrade",
        &dry.guessed,
    );
    let download: Vec<Download> = dry
        .jobs
        .iter()
        .map(|job| Download {
            path: job.label.clone(),
            size: job.fetch.expect.size,
        })
        .collect();
    for download in &download {
        let size = download
            .size
            .map_or_else(|| "size unknown".to_owned(), units::size);
        output.step("Would fetch", &format!("{} · {size}", download.path));
    }
    for archive in &dry.unpack {
        output.step("Would open", archive);
    }
    if !matches!(upgrade, Upgrade::Nothing) && dry.jobs.iter().any(|job| job.fetch.revalidate) {
        output.hint("whether a file no checksum pins has changed shows only once it is downloaded");
    }
    let nothing = !changed && download.is_empty() && dry.unpack.is_empty();
    let found = if stale.is_empty() {
        "data.lock is already up to date".to_owned()
    } else {
        unbuilt_pins(&stale)
    };
    dry_run_done(output, nothing.then_some(found.as_str()));
    Ok(Report {
        project: project.root.clone(),
        selected: None,
        datasets: engine::repinned(&dry.before, &dry.lock),
        lock_changed: changed,
        placement: None,
        dry_run: true,
        changes: Some(Changes {
            download,
            unpack: dry.unpack,
            ..Changes::default()
        }),
        merge: None,
    })
}

/// the two ways on for a project without data.lock, such as a clone whose owner never committed
/// it: the owner's pins, or new pins of what the sources serve now.
pub(super) const UNPINNED: &str = "ask whoever shares the project to commit data.lock, or run `fetchloom lock` to pin what the sources serve today";

/// what `--locked` asks: data.lock exists and its entries for the datasets of `scope` are up to
/// date with data.toml, or the command fails without writing it. `instead` is the other way out
/// the help names for an outdated data.lock, such as "sync without --locked".
pub(super) fn locked(
    project: &Project,
    manifest: &Manifest,
    lock: Option<&Lock>,
    instead: &str,
    scope: &Scope,
) -> Result<(), Error> {
    let Some(lock) = lock else {
        return Err(Error::new(
            Kind::Outdated,
            "there is no data.lock, so nothing pins the files' bytes yet",
        )
        .help(UNPINNED));
    };
    let help = format!("run `fetchloom lock` and commit data.lock, or {instead}");
    check(project, manifest, Some(lock), scope)
        .map(drop)
        .map_err(|error| match error.kind {
            Kind::Outdated => error.help(help),
            _ => error,
        })
}

/// `lock --check`: compares the entries of the datasets in `scope` without the network and
/// without writing. a fetched dataset data.lock does not pin as data.toml asks is
/// `lock.outdated`, which `fetchloom lock` fixes; a recipe whose result data.lock does not pin as
/// building it now would make is `build.needed`, which only `fetchloom build` fixes, since it
/// runs the project's code.
fn check(
    project: &Project,
    manifest: &Manifest,
    old: Option<&Lock>,
    scope: &Scope,
) -> Result<Report, Error> {
    let empty = Lock::default();
    let old = old.unwrap_or(&empty);
    let mut stale: Vec<String> = Vec::new();
    let mut unbuilt: Vec<(String, Vec<Change>)> = Vec::new();
    for (name, entry) in &manifest.datasets {
        if !scope.includes(name) {
            continue;
        }
        if entry.recipe().is_some() {
            if old.get(name).is_some() {
                stale.push(format!(
                    "{name} is made by a recipe in data.toml, but data.lock has it as fetched"
                ));
            }
            let changes = recipe::drift(&project.root, manifest, old, name)?;
            if !changes.is_empty() {
                unbuilt.push((name.clone(), changes));
            }
            continue;
        }
        match old.get(name).map(|locked| locked.drift(entry)) {
            None => stale.push(format!("{name} is not in data.lock yet")),
            Some(Some(Drift::Spec)) => stale.push(format!("{name} changed in data.toml")),
            Some(Some(Drift::Extract)) => stale.push(format!(
                "{name} changed extract in data.toml; `fetchloom lock` needs no network for it while its archives are cached"
            )),
            Some(None) => {}
        }
    }
    let names = old.datasets.iter().map(|dataset| &dataset.name);
    for name in names.chain(old.derived.iter().map(|result| &result.name)) {
        if !scope.includes(name) {
            continue;
        }
        match manifest.datasets.get(name) {
            None => stale.push(format!("{name} is in data.lock but no longer in data.toml")),
            Some(entry) if entry.source().is_some() && old.result(name).is_some() => {
                stale.push(format!(
                    "{name} is fetched in data.toml, but data.lock has it as made by a recipe"
                ));
            }
            Some(_) => {}
        }
    }
    if !stale.is_empty() {
        let mut error = Error::new(Kind::Outdated, "data.lock is out of date with data.toml");
        for line in stale {
            error = error.detail(line);
        }
        return Err(error.help("run `fetchloom lock` and commit data.lock"));
    }
    if let Some(error) = needs_building(&unbuilt) {
        return Err(error);
    }
    if let Some(error) = mispinned(project, manifest, old, scope) {
        return Err(error);
    }
    Ok(Report {
        project: project.root.clone(),
        ..Report::default()
    })
}

/// a file data.lock pins in `scope` at another size than the bytes its SHA-256 names, as a file
/// fetchloom placed shows them: one the state records with that SHA-256 that still looks as
/// placed and hashes to it. only a file the state records at another size is read.
fn mispinned(project: &Project, manifest: &Manifest, lock: &Lock, scope: &Scope) -> Option<Error> {
    let (state, _) = project.state();
    if state.files.is_empty() {
        return None;
    }
    for locked in lock
        .datasets
        .iter()
        .filter(|locked| scope.includes(&locked.name))
    {
        let folder = plan::folder(manifest, &locked.name);
        for file in locked.files.iter().filter(|file| file.unpacks_to.is_none()) {
            let path = format!("{folder}/{}", file.path);
            let Some(placed) = state.files.get(&path) else {
                continue;
            };
            if let Some(actual) =
                place::placed_other_size(&project.root.join(&path), placed, &file.sha256, file.size)
            {
                let label = format!("{}/{}", locked.name, file.path);
                return Some(place::mispinned(&label, file.size, actual));
            }
        }
    }
    None
}

/// the error for recipes whose result data.lock does not pin as building them now would make,
/// each named with what moved since it was built, or nothing when there are none.
pub(super) fn needs_building(unbuilt: &[(String, Vec<Change>)]) -> Option<Error> {
    let message = match unbuilt {
        [] => return None,
        [(name, changes)] if changes.first() == Some(&Change::Unbuilt) => {
            format!("data.lock is out of date: {name} is not built yet")
        }
        [(name, _)] => format!("data.lock is out of date: {name} changed since it was built"),
        many => {
            let names: Vec<&str> = many.iter().map(|(name, _)| name.as_str()).collect();
            let (last, rest) = names.split_last()?;
            format!(
                "data.lock is out of date: {} and {last} need building",
                rest.join(", ")
            )
        }
    };
    let mut error = Error::new(Kind::BuildNeeded, message);
    for (name, changes) in unbuilt {
        error = error.detail(format!("{name}: {}", said(changes)));
    }
    let build = match unbuilt {
        [(name, _)] => format!("fetchloom build {name}"),
        _ => "fetchloom build".to_owned(),
    };
    Some(error.help(format!(
        "run `{build}`, then commit data.lock and data.lock.d"
    )))
}

/// what moved since a recipe was built, in a few words.
pub(super) fn said(changes: &[Change]) -> String {
    changes
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ")
}

/// data.toml and data.lock as a run read them, which the state records as the run leaves them.
#[derive(Clone, Copy)]
pub(super) enum Read<'a> {
    /// a data folder still as a complete sync left them needs no plan, as `sync` finds.
    Settled(&'a Seen),
    /// the run plans even so, as `add` and `path` must, since they answer from what planning
    /// learns, such as what an archive holds.
    Planned(&'a Seen),
}

/// what a run did, or with `--dry-run` would do, to data.lock and the data folder.
#[derive(Debug)]
pub(super) struct Outcome<T> {
    pub(super) lock_changed: bool,
    /// the datasets whose pins changed: pinned for the first time, pinned to other data or files,
    /// or no longer pinned.
    pub(super) pins: Vec<String>,
    /// what placing did, or would do.
    pub(super) placed: T,
}

/// the choices a command makes for one [`sync_all`].
pub(super) struct Run<'a> {
    /// which datasets resolve their newest version.
    pub(super) upgrade: Upgrade,
    /// whether edited files and files fetchloom did not place are replaced.
    pub(super) force: bool,
    /// whether every placed file is hashed, so changes that kept their size and time show.
    pub(super) verify: bool,
    pub(super) read: Read<'a>,
}

impl<'a> Run<'a> {
    /// a run that plans even when the data folder looks settled, upgrades nothing, replaces no
    /// edit and hashes only the files it must, as `path` makes when its dataset is not in place.
    pub(super) fn planned(seen: &'a Seen) -> Self {
        Run {
            upgrade: Upgrade::Nothing,
            force: false,
            verify: false,
            read: Read::Planned(seen),
        }
    }
}

/// the lock, the cache and the data folder brought in line, in that order, and the state left
/// recording data.toml and data.lock as the run read them and as it leaves them. `given` is a
/// listing already resolved for one dataset.
pub(super) async fn sync_all(
    work: &Loop<'_>,
    manifest: &Manifest,
    old: Option<Lock>,
    given: Option<(String, Listing)>,
    run: Run<'_>,
) -> Result<Outcome<Placement>, Error> {
    let Run {
        upgrade,
        force,
        verify,
        read,
    } = run;
    refuse_unbuilt(manifest, old.as_ref(), &work.scope)?;
    let mut survey = Survey::read(work.project, &manifest.data_dir, work.context.threads);
    survey.check_doubted(&work.project.root, &work.cache)?;
    if verify {
        survey.verify(&work.project.root, &work.scope, Some(&work.context.output))?;
    }
    disk::audit(work.project, &survey, &work.cache)?;
    let seen = match read {
        Read::Settled(seen) => {
            if let Some(placement) =
                place::untouched(work.project, manifest, &survey, seen, true, &work.scope)?
            {
                work.tell_stale(manifest, old.as_ref().unwrap_or(&Lock::default()));
                // files hashing found unchanged settle with a state written now; a project folder
                // that takes no write fails no run that changed nothing
                if survey.needs_writing
                    && let Err(error) = work.project.write_state(&survey.state)
                {
                    tracing::debug!(error = %error, "could not settle the state");
                }
                // nothing is left to do, so a stop asked from here on, as by ctrl-c while the
                // last lines print, leaves the sync done
                work.context
                    .stop
                    .check()
                    .map_err(|error| work.before_placing(error))?;
                work.context.stop.finish();
                register(work);
                return Ok(Outcome {
                    lock_changed: false,
                    pins: Vec::new(),
                    placed: placement,
                });
            }
            seen
        }
        Read::Planned(seen) => seen,
    };
    let (lock, before) = work
        .relock(
            manifest,
            old,
            &upgrade,
            given
                .into_iter()
                .map(|(name, listing)| Given::Listing(name, listing))
                .collect(),
        )
        .instrument(tracing::debug_span!("relock"))
        .await
        .map_err(|error| work.before_placing(error))?;
    // a renamed recipe's kept rebuild follows its pin to the new name
    for (old, new) in before.renamed() {
        if let Some(candidate) = survey.state.candidates.remove(old) {
            survey.state.candidates.insert(new.to_owned(), candidate);
            survey.needs_writing = true;
        }
    }
    // a lock whose files cannot all land is refused before it is written, so data.lock never
    // pins what no sync can place. archives not unpacked here yet are checked once they are
    let plan = plan::from_cache(manifest, &lock, &work.cache)?;
    work.tell_stale(manifest, &lock);
    let written = work.write_lock(&before, &lock)?;
    let changed = written.is_some();
    let pins = engine::repinned(&before, &lock);
    let written = match &written {
        Some(text) => Some(digest::sha256(text.as_bytes())),
        None => seen.lock,
    };
    let synced = written.map(|lock| Synced {
        manifest: seen.manifest,
        lock,
        complete: false,
    });
    let refetched = refetched(&survey, &work.scope);
    let placement = work
        .place(manifest, &lock, plan, force, survey, synced)
        .await
        .map_err(|error| refetched.iter().cloned().fold(error, Error::detail))?;
    work.context.stop.finish();
    work.drop_unpacked(&lock);
    register(work);
    Ok(Outcome {
        lock_changed: changed,
        pins,
        placed: placement,
    })
}

/// what [`sync_all`] would do, told as it is found: the lock it would write when that changes,
/// the lines a sync that stops before it places adds to its error for the files of `scope` linked
/// to a cache copy someone changed, whose pinned bytes it must fetch again: why it needed them,
/// since the error alone may name only the download. at most five, then how many more.
fn refetched(survey: &Survey, scope: &Scope) -> Vec<String> {
    let mut paths: Vec<&String> = survey
        .elsewhere
        .iter()
        .filter(|path| {
            survey
                .state
                .files
                .get(*path)
                .is_some_and(|placed| scope.includes(&placed.dataset))
        })
        .collect();
    paths.sort_unstable();
    let mut said: Vec<String> = paths
        .iter()
        .take(5)
        .map(|path| {
            format!(
                "{path} is linked to the cache's copy, which was changed, so its pinned bytes must be fetched again"
            )
        })
        .collect();
    if paths.len() > 5 {
        said.push(format!(
            "and {} more files linked to changed cache copies",
            paths.len() - 5
        ));
    }
    said
}

/// the downloads and unpacking it would need, then what placing would change. it stops before
/// anything [`sync_all`] changes: no download, unpacking, lock, state or registration, and cached
/// objects are read without being sealed or marked. given `seen`, it takes the settled-sync
/// shortcut as [`sync_all`] does, so a settled project needs neither the cache nor the network.
pub(super) async fn preview_all(
    work: &Loop<'_>,
    manifest: &Manifest,
    old: Option<Lock>,
    force: bool,
    verify: bool,
    given: Vec<Given>,
    seen: Option<&Seen>,
) -> Result<Outcome<Preview>, Error> {
    refuse_unbuilt(manifest, old.as_ref(), &work.scope)?;
    let mut survey = Survey::read(work.project, &manifest.data_dir, work.context.threads);
    survey.check_doubted(&work.project.root, &work.cache)?;
    if verify {
        survey.verify(&work.project.root, &work.scope, Some(&work.context.output))?;
    }
    let damaged = work.damaged_objects(&survey)?;
    if let Some(seen) = seen
        && let Some(placement) =
            place::untouched(work.project, manifest, &survey, seen, false, &work.scope)?
    {
        work.tell_stale(manifest, old.as_ref().unwrap_or(&Lock::default()));
        return Ok(Outcome {
            lock_changed: false,
            pins: Vec::new(),
            placed: Preview {
                placement,
                ..Preview::default()
            },
        });
    }
    let engine::DryRelock {
        lock,
        before,
        jobs,
        guessed,
        ..
    } = work
        .relock_dry(manifest, old, &Upgrade::Nothing, given)
        .await?;
    let changed = !before.holds(&lock);
    let output = &work.context.output;
    work.tell_stale(manifest, &lock);
    if changed {
        engine::tell_lock(
            output,
            &before,
            &lock,
            engine::Verbs::WOULD,
            None,
            &engine::sizeless(&jobs),
        );
    }
    let preview = work.preview(manifest, &lock, force, survey, jobs, &damaged, &guessed)?;
    for download in &preview.changes.download {
        let size = download
            .size
            .map_or_else(|| "size unknown".to_owned(), units::size);
        output.step("Would fetch", &format!("{} · {size}", download.path));
    }
    for archive in &preview.changes.unpack {
        output.step("Would open", archive);
    }
    if !preview.changes.unpack.is_empty() {
        output.hint("an archive's files are known once it is unpacked, so they are not counted");
    }
    place::tell_preview(output, manifest, &work.scope, &preview);
    Ok(Outcome {
        lock_changed: changed,
        pins: engine::repinned(&before, &lock),
        placed: preview,
    })
}

/// refuses a run of named datasets that names a recipe `lock` pins no result of, before
/// anything else: only `fetchloom build` makes one, so the run has nothing to place for it.
fn refuse_unbuilt(manifest: &Manifest, lock: Option<&Lock>, scope: &Scope) -> Result<(), Error> {
    let Scope::Only(names) = scope else {
        return Ok(());
    };
    let unbuilt: Vec<(String, Vec<Change>)> = names
        .iter()
        .filter(|name| {
            manifest
                .datasets
                .get(*name)
                .is_some_and(|entry| entry.recipe().is_some())
                && lock.is_none_or(|lock| lock.result(name).is_none())
        })
        .map(|name| (name.clone(), vec![Change::Unbuilt]))
        .collect();
    needs_building(&unbuilt).map_or(Ok(()), Err)
}

/// the last line of every dry run, with what it found when there is nothing to do.
pub(super) fn dry_run_done(output: &Output, nothing_to_do: Option<&str>) {
    if let Some(found) = nothing_to_do {
        output.done("Ok", &format!("{found}; dry run, nothing was changed"));
    } else {
        output.done("Ok", "dry run, nothing was changed");
        output.next("run the same command without --dry-run to make these changes");
    }
}

/// registers the project with the cache. gc keeps what registered projects need, so a failure
/// here only risks a later download.
pub(super) fn register(work: &Loop<'_>) {
    if let Err(err) = work.cache.register(&work.project.root) {
        work.context.output.warn_with(
            &format!(
                "could not add this project to the cache's list of projects in {}: {err}. `fetchloom cache gc` may then delete files this project needs",
                work.cache.root().display()
            ),
            "check the cache folder's permissions, then run the command again",
        );
    }
}

/// the project around the current folder. a person working in a folder below it is told which
/// data.toml the command uses.
pub(super) fn find(output: &Output) -> Result<Project, Error> {
    let here =
        std::env::current_dir().map_err(|err| Error::io("cannot read the current folder", &err))?;
    let project = Project::find(&here).ok_or_else(|| match Project::repository(&here) {
        Some(repository) => Error::new(
            Kind::ProjectInvalid,
            format!(
                "there is no data.toml in this folder or any folder above it inside the git repository at {}",
                repository.display()
            ),
        )
        .help("run `fetchloom add <link>` to start a project here, or give -C the folder of a project outside this repository, as in `fetchloom -C <folder> <command>`"),
        None => Error::new(
            Kind::ProjectInvalid,
            "there is no data.toml in this folder or any folder above it",
        )
        .help("run `fetchloom add <link>` to start a project here"),
    })?;
    tracing::debug!(root = %project.root.display(), "found data.toml");
    if project.root != here {
        output.aside(&format!("using the project in {}", project.root.display()));
    }
    Ok(project)
}

/// takes the project lock, waiting while another fetchloom changes the project.
pub(super) enum Waited<T> {
    Held(Held),
    Ready(T),
}

pub(super) async fn hold(
    context: &Context,
    project: &Project,
    command: &str,
) -> Result<Held, Error> {
    match hold_until(context, project, command, || {
        Ok(None::<std::convert::Infallible>)
    })
    .await?
    {
        Waited::Held(held) => Ok(held),
        Waited::Ready(never) => match never {},
    }
}

pub(super) async fn hold_until<T>(
    context: &Context,
    project: &Project,
    command: &str,
    mut ready: impl FnMut() -> Result<Option<T>, Error>,
) -> Result<Waited<T>, Error> {
    if let Some(building) = &context.building
        && same_folder(building, &project.root)
    {
        return Err(Error::new(
            Kind::Usage,
            format!(
                "`fetchloom {command}` runs inside `fetchloom build` of this project, which holds the project until the step ends"
            ),
        )
        .help("a step reads each dataset it is made from in the folder {name} stands for in its run, and writes into {out}, with no fetchloom command"));
    }
    let mut told = false;
    let mut told_owner = false;
    loop {
        if context.stop.is_asked() {
            return Err(Error::new(
                Kind::Interrupted,
                "stopped while waiting for another fetchloom",
            ));
        }
        if told && let Some(answer) = ready()? {
            return Ok(Waited::Ready(answer));
        }
        match project.hold(command)? {
            Hold::Held(held) => return Ok(Waited::Held(held)),
            Hold::Busy(holder) if !told => {
                let who = holder.as_deref().unwrap_or("another fetchloom");
                context
                    .output
                    .step("Waiting", &format!("for {who} working in this project"));
                told = true;
            }
            Hold::Owner(message) if !told_owner => {
                context.output.step("Waiting", &message);
                told = true;
                told_owner = true;
            }
            Hold::Busy(_) | Hold::Owner(_) => {}
        }
        tokio::select! {
            () = tokio::time::sleep(Duration::from_millis(200)) => {}
            () = context.stop.asked() => return Err(Error::new(Kind::Interrupted, "stopped while waiting for another fetchloom")),
        }
    }
}

/// whether `a` and `b` name one folder, by their paths or, through links, where they lead.
fn same_folder(a: &std::path::Path, b: &std::path::Path) -> bool {
    a == b || std::fs::canonicalize(a).is_ok_and(|a| std::fs::canonicalize(b).is_ok_and(|b| a == b))
}

/// refuses a dataset name data.toml does not have, suggesting the closest one as the `command`
/// that names it, such as `path` in `fetchloom path iris`. a link given where a name belongs is
/// shown with its secrets masked.
pub(super) fn known(manifest: &Manifest, name: &str, command: &str) -> Result<(), Error> {
    known_with_rest(manifest, name, "", command)
}

/// [`known`] for a name typed with `rest` after it, such as `/iris.csv` in `irs/iris.csv`,
/// which a guess at the name keeps.
pub(super) fn known_with_rest(
    manifest: &Manifest,
    name: &str,
    rest: &str,
    command: &str,
) -> Result<(), Error> {
    if manifest.datasets.contains_key(name) {
        return Ok(());
    }
    let error = Error::new(
        Kind::Usage,
        format!(
            "data.toml has no dataset called \"{}\"",
            crate::net::mask_query(name)
        ),
    );
    let typed = format!("{name}{rest}");
    Err(
        match name::closest(name, manifest.datasets.keys().map(String::as_str)) {
            Some(best) => {
                let shown = super::meant(
                    &[&typed],
                    &[&format!("{best}{rest}")],
                    format!("fetchloom {command} {best}{}", crate::net::mask_query(rest)),
                );
                error.help(format!("did you mean `{shown}`?"))
            }
            None if manifest.datasets.is_empty() => {
                error.help("data.toml has no datasets yet. add one with `fetchloom add <link>`")
            }
            None => {
                let names: Vec<&str> = manifest.datasets.keys().map(String::as_str).collect();
                let shown = super::meant(
                    &[&typed],
                    &names[..1],
                    format!("fetchloom {command} {}", names[0]),
                );
                error.help(format!("name one of {}, as in `{shown}`", names.join(", ")))
            }
        },
    )
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::cache::Cache;
    use crate::output::{Level, Output, Style};
    use crate::project::manifest;
    use crate::source::Reference;
    use crate::source::recorded::block_on;

    pub(crate) fn waiting_project() -> (tempfile::TempDir, Project, Context) {
        let dir = tempfile::tempdir().unwrap();
        let project = Project {
            root: dir.path().to_path_buf(),
        };
        std::fs::create_dir(project.own_folder()).unwrap();
        let os = if cfg!(windows) { "linux" } else { "windows" };
        let owner = serde_json::json!({
            "os": os,
            "pid": 42,
            "started": "2026-10-30T12:00:00Z",
            "command": "sync",
        });
        std::fs::write(
            project.own_folder().join("owner"),
            serde_json::to_vec(&owner).unwrap(),
        )
        .unwrap();
        let style = Style {
            color: false,
            unicode: false,
            interactive: false,
            watched: false,
            links: false,
        };
        let context = Context::new(Output::new(style, Level::Quiet, false, false));
        (dir, project, context)
    }

    /// git lists a submodule as one tracked entry and a repository inside the work tree as one
    /// untracked folder, so a file below either is git's, and nothing else is.
    #[test]
    fn a_file_inside_a_folder_git_lists_whole_counts_as_listed() {
        let listed = |paths: &[&str]| -> std::collections::HashSet<String> {
            paths.iter().map(|path| (*path).to_owned()).collect()
        };
        let tracked = listed(&["prep.py", "lib/vendor"]);
        assert!(in_git(&tracked, "prep.py", ""));
        assert!(in_git(&tracked, "lib/vendor/x.py", ""));
        assert!(!in_git(&tracked, "lib/vendored.py", ""));
        assert!(!in_git(&tracked, "lib/new.py", ""));
        let untracked = listed(&["lib/new.py", "lib/nested/"]);
        assert!(in_git(&untracked, "lib/nested/deep/x.py", "/"));
        assert!(!in_git(&untracked, "lib/nested.py", "/"));
    }

    #[test]
    fn an_owner_wait_stops_on_ctrl_c() {
        let (_dir, project, context) = waiting_project();
        let path = project.own_folder().join("owner");
        let before = std::fs::read(&path).unwrap();
        block_on(async {
            let mut waiting = Box::pin(hold(&context, &project, "remove"));
            std::future::poll_fn(|task| {
                assert!(std::future::Future::poll(waiting.as_mut(), task).is_pending());
                std::task::Poll::Ready(())
            })
            .await;
            context.stop.ask();
            let Err(error) = tokio::time::timeout(Duration::from_secs(2), &mut waiting)
                .await
                .unwrap()
            else {
                panic!("a stopped wait must not acquire the project")
            };
            assert_eq!(error.kind, Kind::Interrupted);
            assert_eq!(error.message, "stopped while waiting for another fetchloom");
        });
        assert_eq!(std::fs::read(&path).unwrap(), before);
        std::fs::remove_file(&path).unwrap();
        let Err(error) = block_on(hold(&context, &project, "remove")) else {
            panic!("a pending stop must not claim a free project")
        };
        assert_eq!(error.kind, Kind::Interrupted);
        assert!(!path.exists());
    }

    #[test]
    fn an_owner_wait_resumes_when_the_record_is_removed() {
        let (_dir, project, context) = waiting_project();
        let path = project.own_folder().join("owner");
        block_on(async {
            let mut waiting = Box::pin(hold(&context, &project, "remove"));
            std::future::poll_fn(|task| {
                assert!(std::future::Future::poll(waiting.as_mut(), task).is_pending());
                std::task::Poll::Ready(())
            })
            .await;
            std::fs::remove_file(&path).unwrap();
            let held = tokio::time::timeout(Duration::from_secs(2), &mut waiting)
                .await
                .unwrap()
                .unwrap();
            let owner: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            assert_eq!(owner["command"], "remove");
            drop(held);
        });
        assert!(!path.exists());
    }

    pub(crate) fn offline_context() -> Context {
        let style = Style {
            color: false,
            unicode: false,
            interactive: false,
            watched: false,
            links: false,
        };
        let mut context = Context::new(Output::new(style, Level::Quiet, false, false));
        context.offline = crate::net::Offline::Flag;
        context
    }

    /// a file a source states the hash of, so pinning it needs no download.
    pub(crate) fn stated(path: &str, url: &str, bytes: &[u8]) -> crate::source::Remote {
        crate::source::Remote {
            path: path.to_owned(),
            size: Some(bytes.len() as u64),
            sha256: Some(digest::sha256(bytes)),
            md5: None,
            url: url::Url::parse(url).unwrap(),
            named_by_server: false,
        }
    }

    pub(crate) fn listed(url: &str, files: Vec<crate::source::Remote>) -> Listing {
        Listing {
            within: Vec::new(),
            access: crate::source::Access::Unknown,
            resolved: Reference::Url(url::Url::parse(url).unwrap()),
            title: None,
            license: None,
            doi: None,
            files,
            unavailable: Vec::new(),
            warnings: Vec::new(),
        }
    }

    pub(crate) fn pinned(name: &str, spec: String, files: Vec<lock::LockedFile>) -> lock::Locked {
        lock::Locked {
            name: name.to_owned(),
            reference: format!("https://example.org/{name}"),
            spec,
            extract: true,
            resolved: format!("https://example.org/{name}"),
            title: None,
            license: None,
            doi: None,
            access: crate::source::Access::Unknown,
            timestamp: "2026-01-01T00:00:00Z".to_owned(),
            files,
        }
    }

    /// data.toml and data.lock as a test pretends to have read them.
    fn unread() -> Seen {
        Seen {
            manifest: digest::sha256(b"data.toml"),
            lock: None,
        }
    }

    pub(crate) fn pinned_file(path: &str, url: &str, bytes: &[u8]) -> lock::LockedFile {
        lock::LockedFile {
            path: path.to_owned(),
            size: bytes.len() as u64,
            sha256: digest::sha256(bytes),
            md5: None,
            url: url.to_owned(),
            unpacks_to: None,
        }
    }

    #[test]
    fn a_lock_that_cannot_be_placed_is_not_written() {
        let dir = tempfile::tempdir().unwrap();
        let project = Project {
            root: dir.path().to_path_buf(),
        };
        let context = offline_context();
        let cache = crate::cache::Cache::open(dir.path().join("cache")).unwrap();
        let manifest = Manifest::parse(
            "[datasets.a]\nref = \"https://example.org/a\"\npath = \"shared\"\n\n[datasets.b]\nref = \"https://example.org/b\"\npath = \"shared\"\n",
        )
        .unwrap();
        let a = pinned(
            "a",
            lock::spec(manifest.datasets["a"].source().unwrap()),
            vec![pinned_file("README", "https://example.org/a", b"a")],
        );
        let old = Lock {
            datasets: vec![a],
            ..Lock::default()
        };
        std::fs::write(project.root.join(lock::FILE), old.render()).unwrap();
        let work = Loop::new(&context, &project, Arc::new(context.net()), cache);
        let given = (
            "b".to_owned(),
            listed(
                "https://example.org/b",
                vec![stated("README", "https://example.org/b", b"b")],
            ),
        );

        let error = block_on(sync_all(
            &work,
            &manifest,
            Some(old.clone()),
            Some(given),
            Run::planned(&unread()),
        ))
        .unwrap_err();

        assert_eq!(
            error.message, "datasets a and b would both place data/shared/README",
            "{error:?}"
        );
        assert_eq!(
            std::fs::read_to_string(project.root.join(lock::FILE)).unwrap(),
            old.render(),
            "data.lock changed though nothing could be placed"
        );
    }

    /// two datasets in the data folder itself: `b` pinned with a README, and `a`, whose listing
    /// has `remote` as its one file and needs a download to learn its hash. the error a sync
    /// meets first, when every download fails, and whether data.lock stayed as it was.
    fn sync_beside_a_readme(remote: crate::source::Remote) -> (Error, bool) {
        let dir = tempfile::tempdir().unwrap();
        let project = Project {
            root: dir.path().to_path_buf(),
        };
        let context = offline_context();
        let cache = crate::cache::Cache::open(dir.path().join("cache")).unwrap();
        let manifest = Manifest::parse(
            "[datasets.a]\nref = \"https://example.org/a\"\npath = \".\"\n\n[datasets.b]\nref = \"https://example.org/b\"\npath = \".\"\n",
        )
        .unwrap();
        let b = pinned(
            "b",
            lock::spec(manifest.datasets["b"].source().unwrap()),
            vec![pinned_file("README", "https://example.org/b", b"b")],
        );
        let old = Lock {
            datasets: vec![b],
            ..Lock::default()
        };
        std::fs::write(project.root.join(lock::FILE), old.render()).unwrap();
        let work = Loop::new(&context, &project, Arc::new(context.net()), cache);
        let given = (
            "a".to_owned(),
            listed("https://example.org/a", vec![remote]),
        );
        let error = block_on(sync_all(
            &work,
            &manifest,
            Some(old.clone()),
            Some(given),
            Run::planned(&unread()),
        ))
        .unwrap_err();
        let kept = std::fs::read_to_string(project.root.join(lock::FILE)).unwrap() == old.render();
        (error, kept)
    }

    /// a listing names its files before any is downloaded, so a clash it shows stops the sync
    /// before the download that would only learn the file's hash. the datasets are named in
    /// data.lock's order, as the check after the downloads names them.
    #[test]
    fn a_clash_the_listing_shows_is_found_before_any_download() {
        let mut listed = stated("README", "https://example.org/a/README", b"a");
        listed.sha256 = None;
        // the MD5 of a, as zenodo and S3 list it
        listed.md5 = crate::digest::Md5::from_hex("0cc175b9c0f1b6a831c399e269772661");

        let (error, kept) = sync_beside_a_readme(listed);

        assert_eq!(error.kind, Kind::ProjectInvalid, "{error:?}");
        assert_eq!(
            error.message,
            "datasets a and b would both place data/README"
        );
        assert!(kept, "data.lock changed though nothing could be placed");
    }

    /// a name only a guess from the link, which the server's name for the file replaces, cannot
    /// clash before the download that tells it.
    #[test]
    fn a_name_the_server_gives_is_checked_once_downloaded() {
        let mut guessed = stated("README", "https://example.org/a/README", b"a");
        guessed.sha256 = None;
        guessed.size = None;
        guessed.named_by_server = true;

        let (error, kept) = sync_beside_a_readme(guessed);

        assert_eq!(error.kind, Kind::OfflineMissing, "{error:?}");
        assert!(kept);
    }

    /// kaggle may send a competition file as a zip of itself, which data.lock then pins as
    /// `<name>.zip`. a listing that also holds a real `<name>.zip` must not pin both on one path,
    /// or no later run could read data.lock.
    #[test]
    fn two_files_on_one_path_are_refused_before_data_lock_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let project = Project {
            root: dir.path().to_path_buf(),
        };
        let context = offline_context();
        let cache = crate::cache::Cache::open(dir.path().join("cache")).unwrap();
        let manifest = Manifest::parse(
            "[datasets.comp]\nref = \"https://example.org/comp\"\nextract = false\n",
        )
        .unwrap();
        let download = "https://api.kaggle.com/v1/competitions/data/download/titanic";
        let wrapped = format!("{download}/train.csv");
        let zipped = format!("{download}/train.csv.zip");
        let old = Lock {
            datasets: vec![pinned(
                "comp",
                "sha256:an older entry".to_owned(),
                vec![pinned_file("train.csv.zip", &wrapped, b"wrapped")],
            )],
            ..Lock::default()
        };
        std::fs::write(project.root.join(lock::FILE), old.render()).unwrap();
        let work = Loop::new(&context, &project, Arc::new(context.net()), cache);
        let mut unstated = stated("train.csv", &wrapped, b"wrapped");
        unstated.sha256 = None;
        let given = (
            "comp".to_owned(),
            listed(
                "https://example.org/comp",
                vec![unstated, stated("train.csv.zip", &zipped, b"a real zip")],
            ),
        );

        let error = block_on(sync_all(
            &work,
            &manifest,
            Some(old.clone()),
            Some(given),
            Run::planned(&unread()),
        ))
        .unwrap_err();

        assert_eq!(error.kind, Kind::ProjectInvalid);
        assert!(
            error.message.contains("comp")
                && error.message.contains("train.csv.zip")
                && error.message.contains("pinned"),
            "{error:?}"
        );
        assert_eq!(
            std::fs::read_to_string(project.root.join(lock::FILE)).unwrap(),
            old.render(),
            "data.lock lists a path twice"
        );
        assert!(Lock::parse(&old.render()).is_ok());
    }

    /// a name data.toml lacks is shown with any secret it holds masked, as when a link is given
    /// where a dataset's name belongs.
    #[test]
    fn an_unknown_name_never_shows_a_secret() {
        let manifest =
            Manifest::parse("[datasets.iris]\nref = \"https://example.org/iris.csv\"\n").unwrap();

        let error = known(
            &manifest,
            "https://example.org/x?token=AUDIT_CANARY",
            "sync",
        )
        .unwrap_err();

        let shown = format!("{} {:?} {:?}", error.message, error.details, error.help);
        assert!(!shown.contains("AUDIT_CANARY"), "{shown}");
        assert!(shown.contains("token=***"), "{shown}");
    }

    /// a sync that finds its project settled has nothing left to do, so a stop asked once it
    /// found so, as by ctrl-c while it prints its last lines, leaves it done, while one asked
    /// before ends it as stopped.
    #[test]
    fn a_stop_once_a_settled_sync_found_nothing_to_do_leaves_it_done() {
        let dir = tempfile::tempdir().unwrap();
        let project = Project {
            root: dir.path().join("project"),
        };
        std::fs::create_dir(&project.root).unwrap();
        project.write(manifest::FILE, b"").unwrap();
        let manifest = Manifest::parse("").unwrap();
        let cache = Cache::open(dir.path().join("cache")).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _entered = runtime.enter();
        // a first sync writes data.lock and the state that vouch for the project
        let first = offline_context();
        let unlocked = Seen {
            manifest: digest::sha256(b""),
            lock: None,
        };
        let work = Loop::new(&first, &project, Arc::new(first.net()), cache.clone());
        let synced = sync_all(&work, &manifest, None, None, Run::planned(&unlocked));
        assert!(super::super::block_on(&first, synced).is_ok());
        let (lock, text) = project.lock_and_text().unwrap().unwrap();
        let seen = Seen {
            manifest: digest::sha256(b""),
            lock: Some(digest::sha256(text.as_bytes())),
        };
        let survey = Survey::read(&project, &manifest.data_dir, first.threads);
        assert!(
            place::untouched(&project, &manifest, &survey, &seen, false, &Scope::All)
                .unwrap()
                .is_some(),
            "the project is not settled"
        );
        let mut stopped = Vec::new();
        for late in [true, false] {
            let context = offline_context();
            let work = Loop::new(&context, &project, Arc::new(context.net()), cache.clone());
            let synced = super::super::block_on(&context, async {
                if !late {
                    context.stop.ask();
                }
                let synced = sync_all(
                    &work,
                    &manifest,
                    Some(lock.clone()),
                    None,
                    Run {
                        read: Read::Settled(&seen),
                        ..Run::planned(&seen)
                    },
                )
                .await;
                context.stop.ask();
                synced.map(|_| ())
            });
            stopped.push(synced.err().map(|error| error.kind));
        }
        assert_eq!(stopped, [None, Some(Kind::Interrupted)]);
    }

    /// a sync of every dataset counts the datasets it placed. one that placed none, since each
    /// has `place = false` or waits for a build, says there was nothing to place, and until what.
    #[test]
    fn a_sync_that_places_no_dataset_says_so() {
        let raw = "[datasets.raw]\nplace = false\nref = \"zenodo:1\"\n";
        let clean = "[datasets.clean]\nfrom = [\"raw\"]\nrun = [\"python3\", \"clean.py\", \"{raw}\", \"{out}\"]\n";
        let tidy = "[datasets.tidy]\nfrom = [\"clean\"]\nrun = [\"python3\", \"tidy.py\", \"{clean}\", \"{out}\"]\n";
        let iris = "[datasets.iris]\nref = \"zenodo:2\"\n";
        let names = |names: &[&str]| {
            names
                .iter()
                .map(|&name| name.to_owned())
                .collect::<Vec<_>>()
        };
        for (tables, kept, unbuilt, said) in [
            (
                &[raw, clean, iris][..],
                0,
                names(&["clean"]),
                (
                    "Synced",
                    "1 dataset into data · 1 result left out until built",
                ),
            ),
            (
                &[raw, clean],
                0,
                names(&["clean"]),
                ("Ok", "nothing to place in data until clean is built"),
            ),
            (
                &[raw, clean, tidy],
                0,
                names(&["clean", "tidy"]),
                ("Ok", "nothing to place in data until 2 results are built"),
            ),
            (&[raw], 0, Vec::new(), ("Ok", "nothing to place in data")),
            (
                &[raw],
                1,
                Vec::new(),
                (
                    "Ok",
                    "nothing to place in data, 1 file left unchanged; see the warnings above",
                ),
            ),
        ] {
            let manifest = Manifest::parse(&tables.join("\n")).unwrap();
            let (verb, text) = synced(&manifest, &Scope::All, kept, &unbuilt);
            assert_eq!((verb, text.as_str()), said, "{tables:?}");
        }
    }
}
