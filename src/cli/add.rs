//! `fetchloom add`: a dataset into data.toml, pinned in data.lock and placed with the rest of the
//! project. every byte goes into the cache before data.toml or data.lock is written, and a failure
//! after that puts back what the add wrote.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tracing::Instrument as _;

use super::args::Add;
use super::project::{
    Outcome, Report, committed, dry_run_done, hold, ignore_data, preview_all, preview_ignore,
    register,
};
use super::sync::disk::{self, Survey};
use super::sync::place::Placement;
use super::sync::{self as engine, Loop, Upgrade};
use super::{Context, fetch, open_cache, peek_cache};
use crate::digest::{self, Sha256};
use crate::error::{Error, Kind};
use crate::name;
use crate::net::Net;
use crate::output::Output;
use crate::plan;
use crate::project::lock::{self, Lock};
use crate::project::manifest::{self, Manifest};
use crate::project::state::Synced;
use crate::project::{Held, Project};
use crate::source::{Listing, Parsed, Reference};

struct AddProject {
    project: Project,
    /// none for a dry run, which changes nothing and so needs no hold.
    held: Option<Held>,
    /// data.toml as read, or none when there was none.
    text: Option<String>,
    manifest: Manifest,
    /// data.lock as read, until the add takes it to lock again.
    old: Option<Lock>,
    /// the text data.lock was read from.
    lock_text: Option<String>,
    /// a failed add could not put back what it changed in data.toml or data.lock, so its error
    /// holds the steps that do.
    unrestored: AtomicBool,
}

impl AddProject {
    /// reads data.toml and data.lock as the add finds them once it holds the project.
    fn read(&mut self) -> Result<(), Error> {
        if self.project.manifest_path().is_file() {
            let (text, manifest) = self.project.manifest()?;
            self.text = Some(text);
            self.manifest = manifest;
        }
        (self.old, self.lock_text) = self.project.lock_and_text()?.unzip();
        Ok(())
    }

    /// which of data.toml and data.lock are no longer as the add found them.
    fn changed(&self) -> Vec<&'static str> {
        [(manifest::FILE, &self.text), (lock::FILE, &self.lock_text)]
            .into_iter()
            .filter(|(name, found)| self.differs(name, found.as_deref()))
            .map(|(name, _)| name)
            .collect()
    }

    /// whether the project file `name` no longer holds `found`, what the add read of it, or
    /// exists when the add found none.
    fn differs(&self, name: &str, found: Option<&str>) -> bool {
        match std::fs::read(self.project.root.join(name)) {
            Ok(bytes) => found.is_none_or(|text| text.as_bytes() != bytes),
            Err(err) => found.is_some() || err.kind() != std::io::ErrorKind::NotFound,
        }
    }

    /// ends an add that set out to start the project and added nothing, leaving its folder as
    /// `fresh` found it.
    fn unstart(&mut self, fresh: Fresh) {
        if let Some(held) = self.held.take() {
            let data = fresh
                .data
                .then(|| self.project.root.join(manifest::DATA_DIR));
            self.project.unstart(held, data.as_deref(), fresh.own);
        }
    }
}

/// what a first add found in its folder, so that one which adds nothing leaves it as it was.
#[derive(Clone, Copy)]
struct Fresh {
    /// `.fetchloom` was not there.
    own: bool,
    /// the data folder was not there.
    data: bool,
}

impl Fresh {
    fn look(project: &Project) -> Fresh {
        let absent = |path: PathBuf| std::fs::symlink_metadata(path).is_err();
        Fresh {
            own: absent(project.own_folder()),
            data: absent(project.root.join(manifest::DATA_DIR)),
        }
    }
}

/// a dataset data.toml already has, which an add asked for again.
#[derive(Clone, Copy)]
struct Present<'a> {
    name: &'a str,
    entry: &'a manifest::Entry,
    listing: &'a Listing,
}

struct Addition {
    listing: Listing,
    reference: String,
    select: Vec<String>,
    entry: manifest::Entry,
}

enum BeforeFile<'a> {
    Absent,
    Saved { text: &'a str, backup: PathBuf },
}

/// one project file as an add found it, and what the add set out to write there.
struct SavedFile<'a> {
    name: &'static str,
    before: BeforeFile<'a>,
    /// the digest of the bytes the add writes, recorded before it writes them, so a write that
    /// failed after the bytes were in place still counts as the add's.
    writing: Option<Sha256>,
}

struct BeforeAdd<'a> {
    files: [SavedFile<'a>; 2],
    keep: bool,
}

impl<'a> BeforeAdd<'a> {
    fn save(project: &Project, before: (Option<&'a str>, Option<&'a str>)) -> Result<Self, Error> {
        let mut saved = Self {
            files: [manifest::FILE, lock::FILE].map(|name| SavedFile {
                name,
                before: BeforeFile::Absent,
                writing: None,
            }),
            keep: false,
        };
        for (file, text) in saved.files.iter_mut().zip([before.0, before.1]) {
            if let Some(text) = text {
                file.before = BeforeFile::Saved {
                    text,
                    backup: project.backup(file.name, text.as_bytes())?,
                };
            }
        }
        #[cfg(test)]
        tests::reached("saved", &project.root);
        Ok(saved)
    }

    /// notes that the add is about to write `bytes` to the project file `name`.
    fn writing(&mut self, name: &str, bytes: &[u8]) {
        if let Some(file) = self.files.iter_mut().find(|file| file.name == name) {
            file.writing = Some(digest::sha256(bytes));
        }
    }
}

impl Drop for BeforeAdd<'_> {
    fn drop(&mut self) {
        if self.keep {
            return;
        }
        for file in &self.files {
            if let BeforeFile::Saved { backup, .. } = &file.before
                && let Err(err) = std::fs::remove_file(backup)
                && err.kind() != std::io::ErrorKind::NotFound
            {
                tracing::debug!(%err, path = %backup.display(), "could not remove a recovery copy");
            }
        }
    }
}

pub(super) async fn add(context: &Context, mut args: Add) -> Result<Report, Error> {
    let dry_run = args.dry.dry_run;
    let here =
        std::env::current_dir().map_err(|err| Error::io("cannot read the current folder", &err))?;
    super::search::refuse_local(&args.link)?;
    let found = Project::find(&here);
    let starting = found.is_none();
    let project = found.unwrap_or_else(|| Project { root: here.clone() });
    if project.root != here {
        context
            .output
            .aside(&format!("using the project in {}", project.root.display()));
    }
    before_holding(context, &mut args, &project.root, starting && !dry_run)
        .await
        .map_err(|error| stopped(context, error, true))?;
    let fresh = starting.then(|| Fresh::look(&project));
    let held = if dry_run {
        None
    } else {
        Some(
            hold(context, &project, "add")
                .await
                .map_err(|error| stopped(context, error, true))?,
        )
    };
    let mut adding = AddProject {
        project,
        held,
        text: None,
        manifest: Manifest::parse("")?,
        old: None,
        lock_text: None,
        unrestored: AtomicBool::new(false),
    };
    let added = match adding.read() {
        Ok(()) => add_held(context, &mut adding, args, &here, starting).await,
        Err(error) => Err(error),
    };
    added.map_err(|error| {
        let restored = !adding.unrestored.load(Ordering::Relaxed);
        if restored && let Some(fresh) = fresh {
            adding.unstart(fresh);
        }
        stopped(context, error, restored)
    })
}

/// what an add settles before it holds the project: whether to start a project in `root`, and
/// which dataset search words name. a question answered no, q at the list or ctrl-c then leaves
/// nothing behind.
async fn before_holding(
    context: &Context,
    args: &mut Add,
    root: &Path,
    starting: bool,
) -> Result<(), Error> {
    if starting {
        start_here(context, root, std::env::home_dir().as_deref())?;
    }
    if crate::source::search::is_search(&args.link) {
        args.link = super::search::pick(context, &context.net(), &args.link, "add").await?;
    }
    Ok(())
}

/// asks before starting a project in `root` when a project there takes in far more than one
/// project's folders: `root` is the home folder or the root of a disk. with nobody to ask it
/// refuses, unless `--yes` said yes.
fn start_here(context: &Context, root: &Path, home: Option<&Path>) -> Result<(), Error> {
    let what = if root.parent().is_none() {
        "the root of a disk"
    } else if home.is_some_and(|home| same_file::is_same_file(home, root).unwrap_or(false)) {
        "your home folder"
    } else {
        return Ok(());
    };
    if context.yes {
        return Ok(());
    }
    let why = format!(
        "{} is {what}, and a project there takes in every folder below it",
        root.display()
    );
    let elsewhere = "make a folder for the project and go into it, such as with `mkdir my-project` and `cd my-project`, then run the command again";
    let cancel = format!(
        "starting a project in {}, so nothing changed",
        root.display()
    );
    match context
        .output
        .ask_yes_no(&start_question(root, what), false)
        .map_err(|error| error.cancelling(&cancel))?
    {
        Some(true) => Ok(()),
        Some(false) => Err(Error::new(
            Kind::Unconfirmed,
            format!(
                "did not start a project in {}, so nothing changed",
                root.display()
            ),
        )
        .help(elsewhere)
        .cancelled(cancel)),
        None => Err(Error::new(Kind::Unconfirmed, why).help(format!(
            "{elsewhere}, or add --yes to start the project there anyway"
        ))),
    }
}

/// the question before a project starts in `root`, which is `what`: the home folder or the
/// root of a disk.
fn start_question(root: &Path, what: &str) -> String {
    format!(
        "{} is {what}.\na project there takes in every folder below it. start one anyway?",
        root.display()
    )
}

/// `error`, once it says what state the project is in when a stop ended the add, which the stop's
/// own message does not. `restored` says whether the add left no change of its own in data.toml
/// and data.lock, having made none or put back what it made.
fn stopped(context: &Context, error: Error, restored: bool) -> Error {
    // a cancel the person chose says itself what it cancelled, and a stop asked once the add was
    // in place changed nothing about it
    if error.cancelled.is_some()
        || (error.kind != Kind::Interrupted && context.stop.check_unfinished().is_ok())
    {
        return error;
    }
    if !restored {
        // the error's details give the steps that put them back
        context
            .output
            .warn("stopped, but data.toml and data.lock could not be put back as they were");
        return error;
    }
    // what a stopped download kept stays in the details, so the same add continues from there.
    // an error of another kind, met as the stop took effect, ends as the stop
    let mut stop = if error.kind == Kind::Interrupted {
        error
    } else {
        crate::stop::Stop::stopped()
    };
    "stopped, and nothing was added".clone_into(&mut stop.message);
    stop
}

/// the add once the project is held: everything but the steps before holding it.
async fn add_held(
    context: &Context,
    adding: &mut AddProject,
    mut args: Add,
    here: &Path,
    starting: bool,
) -> Result<Report, Error> {
    let dry_run = args.dry.dry_run;
    let old = adding.old.take();
    let adding = &*adding;
    let project = &adding.project;
    if !dry_run {
        project.check_writable()?;
    }
    let manifest = &adding.manifest;
    let net = Arc::new(context.net());
    let parsed = validate_add(context, manifest, &net, &args)?;
    let cache = if dry_run {
        peek_cache()?
    } else {
        open_cache()?
    };
    let mut addition = resolve_add(context, &net, &args, &parsed).await?;
    if args.name.is_none()
        && let Some(name) = present(manifest, old.as_ref(), &addition.entry, &addition.listing)
    {
        let work = Loop::new(context, project, net, cache);
        let present = Present {
            name: &name,
            entry: &addition.entry,
            listing: &addition.listing,
        };
        return again(&work, adding, old, &present, dry_run, here).await;
    }
    // a dry run downloads nothing, so it names the dataset from the link alone. patterns were
    // written against the link's own name, which the server's name would replace before they apply
    let patterns = !args.filter.select.is_empty() || !args.filter.exclude.is_empty();
    if args.name.is_none() && !dry_run && !patterns {
        learn_name(
            context,
            &net,
            &cache,
            &mut addition.listing,
            !args.no_extract,
        )
        .await?;
    }
    let name = match args.name.take() {
        Some(asked) => asked,
        None => choose_name(&context.output, &addition.listing, &parsed, manifest)?,
    };
    let text = adding.text.as_deref().unwrap_or_default();
    let (updated, new_manifest) = added(text, &name, &args, &addition)?;
    let work = Loop::new(context, project, net, cache).telling(&name);
    if dry_run {
        return preview_add(&work, &new_manifest, old, name, addition.listing, here).await;
    }
    let outcome = sync_add(
        &work,
        adding,
        &new_manifest,
        Some(&updated),
        old,
        (&name, Some(addition.listing)),
    )
    .await?;
    render_add(&work, &new_manifest, &name, here, starting)?;
    Ok(Report {
        project: project.root.clone(),
        datasets: vec![name],
        lock_changed: outcome.lock_changed,
        placement: Some(outcome.placed),
        ..Report::default()
    })
}

fn validate_add(
    context: &Context,
    manifest: &Manifest,
    net: &Net,
    args: &Add,
) -> Result<Parsed, Error> {
    let parsed = Reference::parse(&args.link)?;
    engine::tell_raw(&context.output, &args.link, &parsed.reference);
    parsed.reference.check_shareable()?;
    // no answer from the source changes whether the name --as gives is usable
    if let Some(asked) = args.name.as_deref() {
        usable(asked, manifest)?;
    }
    // the host the person typed, such as a dataverse install on a university network
    net.trust(parsed.reference.written_host());
    Ok(parsed)
}

async fn resolve_add(
    context: &Context,
    net: &Net,
    args: &Add,
    parsed: &Parsed,
) -> Result<Addition, Error> {
    let select = if args.filter.select.is_empty() {
        parsed.select()
    } else {
        args.filter.select.clone()
    };
    let folders = crate::glob::folders_of(&select);
    let listing =
        engine::resolve(&context.output, &parsed.reference, net, folders.as_deref()).await?;
    engine::tell_resolved(&context.output, &listing);
    let reference = stored_reference(&args.link, parsed);
    // patterns from the command line are checked before any byte moves, and no line of
    // data.toml is to blame for them
    let alone = Manifest::parse(&manifest::with_dataset(
        "",
        "new",
        &reference,
        &select,
        &args.filter.exclude,
        !args.no_extract,
    )?)?;
    let mut entry = alone.datasets["new"].clone();
    entry.lines.clear();
    engine::filter(&parsed.reference.shown(), &entry, &listing)?;
    Ok(Addition {
        listing,
        reference,
        select,
        entry,
    })
}

/// data.toml with the new dataset, as text and as read.
fn added(
    text: &str,
    name: &str,
    args: &Add,
    addition: &Addition,
) -> Result<(String, Manifest), Error> {
    let updated = manifest::with_dataset(
        text,
        name,
        &addition.reference,
        &addition.select,
        &args.filter.exclude,
        !args.no_extract,
    )?;
    let manifest = Manifest::parse(&updated)?;
    Ok((updated, manifest))
}

/// `add --dry-run`: what adding the dataset would change, with data.toml as it would be, told
/// from `here`.
async fn preview_add(
    work: &Loop<'_>,
    manifest: &Manifest,
    old: Option<Lock>,
    name: String,
    listing: Listing,
    here: &Path,
) -> Result<Report, Error> {
    let given = Some((name.clone(), listing));
    let outcome = preview_all(work, manifest, old, false, false, given, None).await?;
    let output = &work.context.output;
    preview_ignore(work, manifest)?;
    output.step(
        "Would add",
        &format!(
            "{name} to data.toml, its files in {}",
            from_here(work.project, here, &plan::folder(manifest, &name))
        ),
    );
    dry_run_done(output, None);
    Ok(Report::dry(work.project.root.clone(), vec![name], outcome))
}

/// brings the project in line with `manifest`, an add's data.toml, whose text is `updated`
/// when the add changes data.toml, with data.lock as read, `old`. `dataset` names the dataset
/// added and gives its listing when that was resolved already. every byte the project needs goes
/// into the cache first, and only then are data.toml and data.lock written and the files placed,
/// so an add cut off while it downloads, even killed, has changed nothing. a failure or a stop
/// once the project is changing puts data.toml and data.lock back.
async fn sync_add(
    work: &Loop<'_>,
    adding: &AddProject,
    manifest: &Manifest,
    updated: Option<&str>,
    old: Option<Lock>,
    dataset: (&str, Option<Listing>),
) -> Result<Outcome<Placement>, Error> {
    let (name, listing) = dataset;
    let project = work.project;
    let mut survey = Survey::read(project, &manifest.data_dir, work.context.threads);
    survey.check_doubted(&project.root, &work.cache)?;
    disk::audit(project, &survey, &work.cache)?;
    let given = listing.map(|listing| (name.to_owned(), listing));
    let (lock, before) = work
        .relock(manifest, old, &Upgrade::Nothing, given)
        .instrument(tracing::debug_span!("relock"))
        .await?;
    // a lock whose files cannot all land is refused before anything is written, so data.lock
    // never pins what no sync can place
    let plan = plan::from_cache(manifest, &lock, &work.cache)?;
    let placing = work.prepare(manifest, &lock, plan, false, survey).await?;
    // every byte is in the cache now, and a stop asked meanwhile ends the add before anything in
    // the project changed
    work.context.stop.check()?;
    // an edit made meanwhile stays, since writing what the add read would lose it
    let changed = adding.changed();
    if !changed.is_empty() {
        return Err(changed_meanwhile(&changed));
    }
    let relocked = (!before.holds(&lock)).then(|| lock.render());
    let mut saved = if updated.is_some() || relocked.is_some() {
        Some(BeforeAdd::save(
            project,
            (adding.text.as_deref(), adding.lock_text.as_deref()),
        )?)
    } else {
        None
    };
    // the recovery copies are flushed to disk, which takes a while, so it looks again right
    // before the first write
    let changed = adding.changed();
    if !changed.is_empty() {
        return Err(changed_meanwhile(&changed));
    }
    let manifest_text = updated.or(adding.text.as_deref()).unwrap_or_default();
    let committed = (|| -> Result<Outcome<Placement>, Error> {
        if let Some(updated) = updated {
            if let Some(saved) = saved.as_mut() {
                saved.writing(manifest::FILE, updated.as_bytes());
            }
            project.write(manifest::FILE, updated.as_bytes())?;
            #[cfg(test)]
            tests::reached("wrote data.toml", &project.root);
            // writing data.lock would lose a change made to it meanwhile, so the add stops
            // instead, and its failure puts data.toml back
            if relocked.is_some() && adding.differs(lock::FILE, adding.lock_text.as_deref()) {
                return Err(lock_changed_meanwhile());
            }
        }
        let written = match relocked {
            Some(text) => {
                if let Some(saved) = saved.as_mut() {
                    saved.writing(lock::FILE, text.as_bytes());
                }
                Some(work.write_rendered(&before, &lock, text)?)
            }
            None => None,
        };
        let lock_text = written.as_deref().or(adding.lock_text.as_deref());
        let synced = lock_text.map(|text| Synced {
            manifest: digest::sha256(manifest_text.as_bytes()),
            lock: digest::sha256(text.as_bytes()),
            complete: false,
        });
        // a stop while files are placed fails the placement, which takes back the files it
        // placed for the new dataset, so the add is undone whole. once every file is in, the
        // add is complete, and a later stop finds it so
        let placement = work.apply(placing, synced)?;
        work.context.stop.finish();
        Ok(Outcome {
            lock_changed: written.is_some(),
            pins: engine::repinned(&before, &lock),
            placed: placement,
        })
    })();
    match committed {
        Ok(outcome) => {
            work.drop_unpacked(&lock);
            // parts the old data.lock named stayed until the add could no longer go back to it
            if outcome.lock_changed {
                project.sweep_lists(&lock);
            }
            register(work);
            Ok(outcome)
        }
        Err(error) => Err(match saved.as_mut() {
            Some(saved) => {
                let error = restore(project, saved, name, error);
                if saved.keep {
                    adding.unrestored.store(true, Ordering::Relaxed);
                }
                error
            }
            None => error,
        }),
    }
}

/// the error of an add that found data.toml or data.lock, as `changed` names them, changed once
/// its downloads were done: it writes neither, so the change stays.
fn changed_meanwhile(changed: &[&str]) -> Error {
    let files = changed.join(" and ");
    Error::new(
        Kind::ProjectChanged,
        format!(
            "{files} changed while the add was downloading, so the add stopped and wrote nothing"
        ),
    )
    .help("your change is kept. run the same add again; what it downloaded is in the cache")
}

/// the error of an add that found data.lock changed once it wrote data.toml: it leaves data.lock
/// as it is, and puts data.toml back as it found it.
fn lock_changed_meanwhile() -> Error {
    Error::new(
        Kind::ProjectChanged,
        "data.lock changed while the add wrote data.toml, so the add stopped before writing data.lock",
    )
    .help("your change is kept. run the same add again; what it downloaded is in the cache")
}

fn render_add(
    work: &Loop<'_>,
    manifest: &Manifest,
    name: &str,
    here: &Path,
    starting: bool,
) -> Result<(), Error> {
    if starting {
        work.context.output.step(
            "Started",
            &format!("a project in {}", work.project.root.display()),
        );
    }
    ignore_data(work, manifest)?;
    tell_added(work, manifest, name, here)
}

/// restores both project files after a failed add, including their original absence. a file the
/// add never wrote needs nothing restored, so a failure to write it again is no failure. a file
/// someone changed after the add wrote it stays as they left it, with the steps that put it back
/// by hand, since restoring it would lose their change.
fn restore(project: &Project, saved: &mut BeforeAdd<'_>, name: &str, mut failure: Error) -> Error {
    let mut complete = true;
    let mut restored = Vec::new();
    for file in &saved.files {
        let now = std::fs::read(project.root.join(file.name));
        let unchanged = match (&file.before, &now) {
            (BeforeFile::Saved { text, .. }, Ok(bytes)) => bytes == text.as_bytes(),
            (BeforeFile::Absent, Err(err)) => err.kind() == std::io::ErrorKind::NotFound,
            _ => false,
        };
        // a file the add never wrote holds nothing of it, whoever changed it
        let Some(writing) = file.writing.filter(|_| !unchanged) else {
            continue;
        };
        let theirs = !now
            .as_ref()
            .is_ok_and(|bytes| digest::sha256(bytes) == writing);
        if theirs {
            failure = failure.detail(format!(
                "{} changed after the add wrote it, so it was left as it is",
                file.name
            ));
        } else {
            let put_back = match file.before {
                BeforeFile::Saved { text, .. } => project.write(file.name, text.as_bytes()),
                BeforeFile::Absent => match std::fs::remove_file(project.root.join(file.name)) {
                    Ok(()) => Ok(()),
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
                    Err(err) => Err(Error::io(format!("could not remove {}", file.name), &err)),
                },
            };
            let Err(err) = put_back else {
                restored.push(file.name);
                continue;
            };
            failure = failure.detail(format!("could not restore {}: {err}", file.name));
            if let Some(help) = err.help {
                failure = failure.detail(help);
            }
        }
        complete = false;
        failure = match &file.before {
            BeforeFile::Saved { backup, .. } => failure.detail(format!(
                "restore {} from the recovery copy at {}",
                file.name,
                backup.display()
            )),
            BeforeFile::Absent => failure.detail(format!(
                "remove the {name} dataset entry from {0}, or remove {0} if it holds nothing else",
                file.name
            )),
        };
    }
    saved.keep = !complete;
    if !complete {
        return failure.detail(
            "could not finish rolling back data.toml and data.lock; follow the recovery steps above",
        );
    }
    match restored.as_slice() {
        [] => failure,
        [file] => failure.detail(format!("restored {file} to its state before add")),
        files => failure.detail(format!(
            "restored {} to their state before add",
            files.join(" and ")
        )),
    }
}

/// the dataset in data.toml that already pins what was just resolved, under any spelling of its
/// reference.
fn present(
    manifest: &Manifest,
    old: Option<&Lock>,
    wanted: &manifest::Entry,
    listing: &Listing,
) -> Option<String> {
    let resolved = listing.resolved.to_string();
    manifest
        .datasets
        .iter()
        .filter(|(name, entry)| {
            entry.reference == wanted.reference
                || old.and_then(|old| old.get(name)).is_some_and(|locked| {
                    locked.reference == entry.reference && locked.resolved == resolved
                })
        })
        .min_by_key(|(_, entry)| {
            (
                entry.select != wanted.select
                    || entry.exclude != wanted.exclude
                    || entry.extract != wanted.extract,
                entry.reference != wanted.reference,
            )
        })
        .map(|(name, _)| name.clone())
}

/// `add` of a dataset data.toml already has, as `present`. with the same settings nothing
/// changes, so running the same `add` twice is safe; other settings need a choice only the person
/// can make. paths are told from `here`.
async fn again(
    work: &Loop<'_>,
    adding: &AddProject,
    old: Option<Lock>,
    present: &Present<'_>,
    dry_run: bool,
    here: &Path,
) -> Result<Report, Error> {
    let Present {
        name,
        entry,
        listing,
    } = *present;
    let manifest = &adding.manifest;
    let existing = &manifest.datasets[name];
    let differs = if existing.select != entry.select || existing.exclude != entry.exclude {
        Some(("other patterns", "the patterns of"))
    } else if existing.extract != entry.extract {
        Some(("another extract setting", "extract on"))
    } else {
        None
    };
    if let Some((differs, setting)) = differs {
        return Err(Error::new(
            Kind::Usage,
            format!(
                "{} is already in data.toml as {name}, with {differs}",
                listing.resolved.shown()
            ),
        )
        .help(format!(
            "change {setting} {name} in data.toml, or pass --as <name> to add it a second time"
        )));
    }
    if dry_run {
        let outcome = preview_all(work, manifest, old, false, false, None, None).await?;
        preview_ignore(work, manifest)?;
        dry_run_done(
            &work.context.output,
            Some(&format!("{name} is already in data.toml")),
        );
        return Ok(Report::dry(work.project.root.clone(), Vec::new(), outcome));
    }
    let outcome = sync_add(work, adding, manifest, None, old, (name, None)).await?;
    // a project that became a git repository after its first add gets its ignore rule now
    ignore_data(work, manifest)?;
    let contents = if has_files(work, manifest, name)? {
        format!(
            "its files are in {}",
            from_here(work.project, here, &plan::folder(manifest, name))
        )
    } else {
        "its archives contain no regular files".to_owned()
    };
    work.context
        .output
        .done("Ok", &format!("{name} is already in data.toml, {contents}"));
    Ok(Report {
        project: work.project.root.clone(),
        datasets: Vec::new(),
        lock_changed: outcome.lock_changed,
        placement: Some(outcome.placed),
        ..Report::default()
    })
}

fn tell_added(work: &Loop<'_>, manifest: &Manifest, name: &str, here: &Path) -> Result<(), Error> {
    if has_files(work, manifest, name)? {
        let folder = from_here(work.project, here, &plan::folder(manifest, name));
        work.context
            .output
            .done("Added", &format!("{name}, its files are in {folder}"));
        work.context
            .output
            .hint(&format!("scripts can find it with `fetchloom path {name}`"));
    } else {
        work.context.output.done(
            "Added",
            &format!("{name}, its archives contain no regular files"),
        );
    }
    work.context.output.next(&format!(
        "commit {}; anyone who clones runs `fetchloom sync`",
        committed(work.project)
    ));
    Ok(())
}

/// `path`, relative to the project's folder, as it reads from `here`, a folder inside it.
fn from_here(project: &Project, here: &Path, path: &str) -> String {
    let depth = here
        .strip_prefix(&project.root)
        .map_or(0, |inside| inside.components().count());
    format!("{}{path}", "../".repeat(depth))
}

fn has_files(work: &Loop<'_>, manifest: &Manifest, name: &str) -> Result<bool, Error> {
    let lock = work.project.lock()?.unwrap_or_default();
    Ok(plan::from_cache(manifest, &lock, &work.cache)?
        .targets
        .iter()
        .any(|target| target.dataset == name))
}

/// downloads a lone file whose name only the server knows, so the dataset can be named after it.
async fn learn_name(
    context: &Context,
    net: &Arc<Net>,
    cache: &crate::cache::Cache,
    listing: &mut Listing,
    extract: bool,
) -> Result<(), Error> {
    let [remote] = listing.files.as_mut_slice() else {
        return Ok(());
    };
    if !remote.named_by_server {
        return Ok(());
    }

    let job = fetch::listed_job(remote, &remote.path, extract);
    let started = std::time::Instant::now();
    // the add changes nothing before its downloads are done
    let fetched = fetch::all(context, net, cache, vec![job])
        .await
        .map_err(Error::unchanged)?;
    if let Some(fetched) = fetched.first() {
        if let Some(given) = fetched.name.as_deref().and_then(name::sanitize) {
            remote.path = given;
        }
        if fetched.origin == crate::net::Origin::Network {
            context.output.step(
                "Downloaded",
                &format!(
                    "{} · {}",
                    remote.path,
                    crate::units::downloaded([(fetched.size, fetched.moved)], started.elapsed())
                ),
            );
        }
        remote.sha256 = Some(fetched.digest);
        remote.size = Some(fetched.size);
        remote.named_by_server = false;
    }
    Ok(())
}

/// refuses a name that is not a valid dataset name or that data.toml already has, as `--as`
/// gives it, saying why and suggesting one the next run accepts.
fn usable(asked: &str, manifest: &Manifest) -> Result<(), Error> {
    if let Some(problem) = name::dataset_name_problem(asked) {
        // a name long enough to be refused for its length would fill the screen
        let shown: String = if asked.chars().count() > 40 {
            asked.chars().take(40).chain(['\u{2026}']).collect()
        } else {
            asked.to_owned()
        };
        let error = Error::new(
            Kind::Usage,
            format!("\"{shown}\" is not a valid dataset name"),
        )
        .detail(problem);
        let rule = format!(
            "use lowercase letters a to z, digits, - and _, starting with a letter or a digit, at most {} characters",
            name::DATASET_NAME_MOST
        );
        return Err(match name::dataset_name(asked) {
            Some(suggested) => {
                error.help(format!("{rule}, such as --as {}", unreserved(suggested)))
            }
            None => error.help(rule),
        });
    }
    // a dataset's name is its folder, and data.toml travels to windows machines too
    if name::is_reserved_on_windows(asked) {
        return Err(Error::new(
            Kind::Usage,
            format!("\"{asked}\" is a name windows keeps for devices, so it cannot be a folder"),
        )
        .help(format!("pick another name, such as --as {asked}-data")));
    }
    unused(asked, manifest)
}

/// `name`, or `name-data` where windows keeps `name` for a device, such as con.
fn unreserved(name: String) -> String {
    if name::is_reserved_on_windows(&name) {
        format!("{name}-data")
    } else {
        name
    }
}

/// the name a new dataset gets without `--as`: one made from its file or its title.
fn choose_name(
    output: &Output,
    listing: &Listing,
    parsed: &Parsed,
    manifest: &Manifest,
) -> Result<String, Error> {
    let single = match (listing.files.as_slice(), &parsed.file) {
        ([only], _) => Some(
            only.path
                .rsplit('/')
                .next()
                .unwrap_or(&only.path)
                .to_owned(),
        ),
        (_, Some(file)) => Some(file.rsplit('/').next().unwrap_or(file).to_owned()),
        _ => None,
    };
    let name = single
        .and_then(|file| name::dataset_name(&name::folder_for(&file)))
        .or_else(|| listing.naming_title().and_then(name::title_name))
        .or_else(|| name::reference_name(&parsed.reference.to_string()))
        .unwrap_or_else(|| "dataset".to_owned());
    // data.toml travels to windows machines, where a folder named con or aux cannot exist
    let name = unreserved(name);
    // a clash with --as is refused, but a name fetchloom chose can simply take the next number
    if !manifest.datasets.contains_key(&name) {
        return Ok(name);
    }
    let numbered = (2..100)
        .map(|number| format!("{name}-{number}"))
        .find(|numbered| !manifest.datasets.contains_key(numbered));
    let Some(numbered) = numbered else {
        return Err(taken(&name));
    };
    output.warn_with(
        &format!("{name} was taken, so this one is {numbered}"),
        "choose a name with --as <name>",
    );
    Ok(numbered)
}

/// refuses a name data.toml already has.
fn unused(name: &str, manifest: &Manifest) -> Result<(), Error> {
    if manifest.datasets.contains_key(name) {
        return Err(taken(name));
    }
    Ok(())
}

fn taken(name: &str) -> Error {
    Error::new(
        Kind::Usage,
        format!("data.toml already has a dataset called {name}"),
    )
    .help(format!(
        "give this one another name with --as <name>, or run `fetchloom remove {name}` first"
    ))
}
/// how data.toml records a reference: the canonical id where a source has one, and the link as
/// pasted for drive and dropbox, and the raw endpoint for plain file links.
fn stored_reference(typed: &str, parsed: &Parsed) -> String {
    match parsed.reference {
        Reference::Drive(_) | Reference::Dropbox(_) if typed.contains("://") => {
            typed.trim().to_owned()
        }
        _ => parsed.reference.to_string(),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn raw_references_are_written_instead_of_repository_pages() {
        let typed = "https://github.com/org/repo/blob/main/iris.csv";
        let parsed = crate::source::Reference::parse(typed).unwrap();
        assert_eq!(
            super::stored_reference(typed, &parsed),
            "https://raw.githubusercontent.com/org/repo/main/iris.csv"
        );
    }

    use super::*;
    use crate::cache::Cache;
    use crate::cli::project::tests::{listed, offline_context, pinned, pinned_file, stated};
    use crate::output::{Level, Output, Style};
    use crate::source::recorded::block_on;

    /// a named point of an add, and an edit made in the project folder there.
    type EditAt = (&'static str, fn(&Path));

    thread_local! {
        /// an edit a test makes at a point of an add, as a person's edit landing there would.
        static EDIT_AT: std::cell::Cell<Option<EditAt>> = const { std::cell::Cell::new(None) };
    }

    /// makes the edit a test set for `point`, once, in the project folder `root`.
    pub(super) fn reached(point: &str, root: &Path) {
        match EDIT_AT.take() {
            Some((at, edit)) if at == point => edit(root),
            other => EDIT_AT.set(other),
        }
    }

    /// a rollback puts back only what the add changed. one that failed before changing anything
    /// claims no rollback at all, and no recovery copy is kept.
    #[test]
    fn a_rollback_restores_only_what_the_add_changed() {
        let dir = tempfile::tempdir().unwrap();
        let project = Project {
            root: dir.path().to_path_buf(),
        };
        let manifest_path = project.root.join(manifest::FILE);
        std::fs::write(&manifest_path, "[datasets]\n").unwrap();
        let failure = || Error::new(Kind::LocalIo, "could not write data.toml");
        let mut saved = BeforeAdd::save(&project, (Some("[datasets]\n"), None)).unwrap();
        let untouched = restore(&project, &mut saved, "iris", failure());
        assert!(untouched.details.is_empty(), "{:?}", untouched.details);
        assert!(!saved.keep);
        drop(saved);
        std::fs::write(&manifest_path, "[datasets.iris]\nref = 'x'\n").unwrap();
        std::fs::write(project.root.join(lock::FILE), "version = 1\n").unwrap();
        let mut saved = BeforeAdd::save(&project, (Some("[datasets]\n"), None)).unwrap();
        saved.writing(manifest::FILE, b"[datasets.iris]\nref = 'x'\n");
        saved.writing(lock::FILE, b"version = 1\n");
        let restored = restore(&project, &mut saved, "iris", failure());
        assert_eq!(
            restored.details,
            ["restored data.toml and data.lock to their state before add"]
        );
        assert_eq!(
            std::fs::read_to_string(&manifest_path).unwrap(),
            "[datasets]\n"
        );
        assert!(!project.root.join(lock::FILE).exists());
        drop(saved);
        let left: Vec<_> = std::fs::read_dir(project.own_folder())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(left, [std::ffi::OsString::from(".gitignore")]);
    }

    /// a file someone changed after the add wrote it stays as they left it, since putting it back
    /// would lose their change: the rollback says so and keeps the recovery copy, and still puts
    /// back the other file, which only the add changed.
    #[test]
    fn a_rollback_keeps_a_change_made_after_the_add_wrote_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let project = Project {
            root: dir.path().to_path_buf(),
        };
        let manifest_path = project.root.join(manifest::FILE);
        std::fs::write(&manifest_path, "[datasets]\n").unwrap();
        let mut saved = BeforeAdd::save(&project, (Some("[datasets]\n"), None)).unwrap();
        let added = "[datasets.iris]\nref = 'x'\n";
        saved.writing(manifest::FILE, added.as_bytes());
        std::fs::write(&manifest_path, added).unwrap();
        saved.writing(lock::FILE, b"version = 1\n");
        std::fs::write(project.root.join(lock::FILE), "version = 1\n").unwrap();
        let theirs = format!("{added}# a note made meanwhile\n");
        std::fs::write(&manifest_path, &theirs).unwrap();
        let failure = Error::new(Kind::LocalIo, "could not place a file");
        let restored = restore(&project, &mut saved, "iris", failure);
        assert_eq!(std::fs::read_to_string(&manifest_path).unwrap(), theirs);
        assert!(!project.root.join(lock::FILE).exists());
        assert!(saved.keep);
        let details = restored.details.join("\n");
        assert!(
            details.contains("data.toml changed after the add wrote it")
                && details.contains("recovery copy"),
            "{details}"
        );
        drop(saved);
        let copies = std::fs::read_dir(project.own_folder())
            .unwrap()
            .filter(|entry| {
                entry
                    .as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with("data.toml.recovery.")
            })
            .count();
        assert_eq!(copies, 1);
    }

    /// a rollback that cannot put a file back names the recovery copy, which holds the exact
    /// bytes, readable by the owner only, and never shows what the file holds, which may be a
    /// credential.
    #[cfg(unix)]
    #[test]
    fn a_rollback_that_cannot_finish_names_a_private_copy_without_its_text() {
        use std::os::unix::fs::PermissionsExt;
        if rustix::process::geteuid().is_root() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let project = Project {
            root: dir.path().to_path_buf(),
        };
        let manifest_path = project.root.join(manifest::FILE);
        let before = "# https://example.org/?token=EM_CANARY\n[datasets]\n";
        std::fs::write(&manifest_path, before).unwrap();
        let mut saved = BeforeAdd::save(&project, (Some(before), None)).unwrap();
        let added = format!("{before}[datasets.a]\nref = 'x'\n");
        saved.writing(manifest::FILE, added.as_bytes());
        std::fs::write(&manifest_path, &added).unwrap();
        // the project folder takes no new file, so data.toml cannot be put back
        let mode = |mode| std::fs::Permissions::from_mode(mode);
        std::fs::set_permissions(&project.root, mode(0o555)).unwrap();
        let failure = Error::new(Kind::LocalIo, "could not place a file");
        let restored = restore(&project, &mut saved, "a", failure);
        std::fs::set_permissions(&project.root, mode(0o755)).unwrap();
        let shown = format!("{}\n{}", restored.message, restored.details.join("\n"));
        assert!(
            shown.contains("could not restore data.toml")
                && shown.contains("restore data.toml from the recovery copy"),
            "{shown}"
        );
        assert!(!shown.contains("EM_CANARY"), "{shown}");
        assert!(saved.keep);
        drop(saved);
        let copies: Vec<PathBuf> = std::fs::read_dir(project.own_folder())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("data.toml.recovery."))
            })
            .collect();
        assert_eq!(copies.len(), 1);
        assert_eq!(std::fs::read_to_string(&copies[0]).unwrap(), before);
        assert_eq!(
            std::fs::metadata(&copies[0]).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    /// a project in the home folder or at a disk's root takes in every folder below it, so
    /// starting one there asks first, and with nobody to ask it needs --yes.
    #[test]
    fn starting_a_project_at_a_disk_root_or_in_home_asks_first() {
        let style = Style {
            color: false,
            unicode: false,
            interactive: false,
            watched: false,
            links: false,
        };
        let mut context = Context::new(Output::new(style, Level::Quiet, false, false));
        let root = std::path::Path::new(std::path::MAIN_SEPARATOR_STR);
        let refused = start_here(&context, root, None).unwrap_err();
        assert_eq!(refused.kind, Kind::Unconfirmed);
        assert!(refused.message.contains("the root of a disk"), "{refused}");
        assert!(refused.help.unwrap().contains("--yes"));
        let dir = tempfile::tempdir().unwrap();
        assert!(start_here(&context, dir.path(), None).is_ok());
        let home = start_here(&context, dir.path(), Some(dir.path())).unwrap_err();
        assert!(home.message.contains("your home folder"), "{home}");
        context.yes = true;
        assert!(start_here(&context, root, None).is_ok());
        assert!(start_here(&context, dir.path(), Some(dir.path())).is_ok());
    }

    #[test]
    fn the_question_before_a_project_in_home_fits_an_80_column_terminal() {
        for (root, what) in [
            ("/home/ada", "your home folder"),
            ("/", "the root of a disk"),
        ] {
            let question = start_question(std::path::Path::new(root), what);
            for line in format!("{question} [y/N] ").lines() {
                assert!(
                    line.chars().count() < 80,
                    "{} columns: {line}",
                    line.chars().count()
                );
            }
        }
    }

    /// a project whose data.toml and data.lock pin dataset a, as an add of dataset b holds and
    /// reads it, with both files in the cache. returns data.toml with b added, and b's listing.
    fn adding_b(dir: &Path) -> (AddProject, Cache, String, Listing) {
        let project = Project {
            root: dir.join("project"),
        };
        std::fs::create_dir(&project.root).unwrap();
        let text = "[datasets.a]\nref = \"https://example.org/a\"\n";
        let manifest = Manifest::parse(text).unwrap();
        let old = Lock {
            datasets: vec![pinned(
                "a",
                lock::spec(&manifest.datasets["a"]),
                vec![pinned_file("a", "https://example.org/a", b"a")],
            )],
        };
        project.write(manifest::FILE, text.as_bytes()).unwrap();
        project.write(lock::FILE, old.render().as_bytes()).unwrap();
        let cache = Cache::open(dir.join("cache")).unwrap();
        for bytes in [b"a", b"b"] {
            crate::cache::tests::store(&cache, bytes);
        }
        let mut adding = AddProject {
            project,
            held: None,
            text: None,
            manifest: Manifest::parse("").unwrap(),
            old: None,
            lock_text: None,
            unrestored: AtomicBool::new(false),
        };
        adding.read().unwrap();
        let updated = format!("{text}\n[datasets.b]\nref = \"https://example.org/b\"\n");
        let listing = listed(
            "https://example.org/b",
            vec![stated("b", "https://example.org/b", b"b")],
        );
        (adding, cache, updated, listing)
    }

    /// runs the add of b that [`adding_b`] readies, with `edit` made at `point` of it, and says
    /// how it ended.
    fn add_b_editing(dir: &Path, point: &'static str, edit: fn(&Path)) -> (AddProject, Error) {
        let (mut adding, cache, updated, listing) = adding_b(dir);
        let context = offline_context();
        let manifest = Manifest::parse(&updated).unwrap();
        let old = adding.old.take();
        let work = Loop::new(&context, &adding.project, Arc::new(context.net()), cache);
        EDIT_AT.set(Some((point, edit)));
        let outcome = block_on(sync_add(
            &work,
            &adding,
            &manifest,
            Some(&updated),
            old,
            ("b", Some(listing)),
        ));
        EDIT_AT.set(None);
        drop(work);
        let error = outcome.err();
        (
            adding,
            error.unwrap_or_else(|| Error::new(Kind::LocalIo, "the add went through")),
        )
    }

    /// an edit to data.toml made while the add saves its recovery copies, which it flushes to
    /// disk, stays: the add looks again right before its first write, and writes neither file.
    #[test]
    fn an_add_keeps_an_edit_made_while_it_saves_its_recovery_copies() {
        let dir = tempfile::tempdir().unwrap();
        let (adding, error) = add_b_editing(dir.path(), "saved", |root| {
            std::fs::write(root.join(manifest::FILE), "# a note made meanwhile\n").unwrap();
        });
        let root = &adding.project.root;
        assert_eq!(
            std::fs::read_to_string(root.join(manifest::FILE)).unwrap(),
            "# a note made meanwhile\n"
        );
        assert_eq!(
            std::fs::read_to_string(root.join(lock::FILE)).unwrap(),
            adding.lock_text.clone().unwrap()
        );
        assert_eq!(error.kind, Kind::ProjectChanged, "{error:?}");
        assert!(!root.join("data").join("b").exists());
    }

    /// an edit to data.lock made once the add wrote data.toml stays: the add does not write
    /// data.lock, and its failure puts data.toml back as it found it.
    #[test]
    fn an_add_keeps_an_edit_to_data_lock_made_after_it_wrote_data_toml() {
        let dir = tempfile::tempdir().unwrap();
        let (adding, error) = add_b_editing(dir.path(), "wrote data.toml", |root| {
            std::fs::write(root.join(lock::FILE), "# a note made meanwhile\n").unwrap();
        });
        let root = &adding.project.root;
        assert_eq!(
            std::fs::read_to_string(root.join(lock::FILE)).unwrap(),
            "# a note made meanwhile\n"
        );
        assert_eq!(
            std::fs::read_to_string(root.join(manifest::FILE)).unwrap(),
            adding.text.clone().unwrap()
        );
        assert_eq!(error.kind, Kind::ProjectChanged, "{error:?}");
        assert!(
            error
                .details
                .contains(&"restored data.toml to its state before add".to_owned()),
            "{error:?}"
        );
        assert!(!root.join("data").join("b").exists());
    }
}
