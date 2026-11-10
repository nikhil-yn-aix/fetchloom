//! the project commands: add, remove, sync, lock, status and path. each one reads the project,
//! takes its lock when it changes something, and leaves the order of work to the sync loop.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::Serialize;
use tracing::Instrument as _;

use super::args::{self, Add, Remove};
use super::sync::disk::{self, Survey};
use super::sync::place::{self, Changes, Download, Placement, Preview, Seen};
use super::sync::{self as engine, Loop, Upgrade};
use super::{Context, fetch, open_cache, peek_cache};
use crate::cache::Cache;
use crate::digest::{self, Sha256};
use crate::error::{Error, Kind};
use crate::name;
use crate::net::Net;
use crate::output::Output;
use crate::plan;
use crate::project::lock::{self, Lock};
use crate::project::manifest::{self, Manifest};
use crate::project::state::{Look, Synced};
use crate::project::{Held, Hold, Project};
use crate::source::{Listing, Parsed, Reference};
use crate::units;

/// what a project command did, or with `--dry-run` would do, as `--json` prints it.
#[derive(Default, Serialize)]
pub(super) struct Report {
    project: PathBuf,
    /// datasets added, removed or upgraded.
    datasets: Vec<String>,
    lock_changed: bool,
    placement: Option<Placement>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    dry_run: bool,
    /// with `--dry-run`, the paths that would change.
    #[serde(skip_serializing_if = "Option::is_none")]
    changes: Option<Changes>,
}

impl Report {
    /// what a dry run found: whether data.lock would change and what placing would do.
    fn dry(project: PathBuf, datasets: Vec<String>, outcome: Outcome<Preview>) -> Self {
        Report {
            project,
            datasets,
            lock_changed: outcome.lock_changed,
            placement: Some(outcome.placed.placement),
            dry_run: true,
            changes: Some(outcome.placed.changes),
        }
    }
}

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
    let net = Arc::new(net(context, manifest));
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
    let listing = engine::resolve(&context.output, &parsed.reference, net).await?;
    engine::tell_resolved(&context.output, &listing);
    let select = if args.filter.select.is_empty() {
        parsed.select()
    } else {
        args.filter.select.clone()
    };
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

fn preview_ignore(work: &Loop<'_>, manifest: &Manifest) -> Result<(), Error> {
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

fn ignore_data(work: &Loop<'_>, manifest: &Manifest) -> Result<(), Error> {
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
    work.context
        .output
        .next("commit data.toml and data.lock; anyone who clones runs `fetchloom sync`");
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

pub(super) async fn remove(context: &Context, args: Remove) -> Result<Report, Error> {
    let project = find(&context.output)?;
    let dry_run = args.dry.dry_run;
    let _held = if dry_run {
        None
    } else {
        Some(hold(context, &project, "remove").await?)
    };
    let (text, manifest) = project.manifest()?;
    known(&manifest, &args.name, "remove")?;
    let updated = manifest::without_dataset(&text, &args.name)?;
    let new_manifest = Manifest::parse(&updated)?;
    let mut lock = project.lock()?.unwrap_or_default();
    lock.datasets.retain(|dataset| dataset.name != args.name);
    let cache = if dry_run {
        peek_cache()?
    } else {
        open_cache()?
    };
    let survey = Survey::read(&project, &new_manifest.data_dir, context.threads);
    let placing = place::Placing::new(&project, &new_manifest, &lock, &cache, false, survey)?;
    if dry_run {
        let preview = placing.preview_removal();
        context.output.step(
            "Would drop",
            &format!("{} from data.toml and data.lock", args.name),
        );
        place::tell_preview(&context.output, &new_manifest, &preview);
        dry_run_done(&context.output, None);
        let outcome = Outcome {
            lock_changed: true,
            pins: Vec::new(),
            placed: preview,
        };
        return Ok(Report::dry(project.root, vec![args.name], outcome));
    }
    // a stop asked while the data folder was read leaves the project as it was; once the files
    // change, the removal runs to its end
    context.stop.check()?;
    context.stop.finish();
    project.write(manifest::FILE, updated.as_bytes())?;
    project.write(lock::FILE, lock.render().as_bytes())?;
    let placement = placing.remove_only(&context.output).map_err(|error| {
        error.detail("data.toml and data.lock were updated; after fixing the error, `fetchloom sync` finishes the removal")
    })?;
    context.output.done("Removed", &args.name);
    context.output.next(
        "commit data.toml and data.lock; after pulling, other clones remove unchanged files with `fetchloom sync`",
    );
    Ok(Report {
        project: project.root,
        datasets: vec![args.name],
        lock_changed: true,
        placement: Some(placement),
        ..Report::default()
    })
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
    let read = project.lock_and_text()?;
    if args.locked {
        let Some((lock, _)) = &read else {
            return Err(Error::new(Kind::Outdated, "there is no data.lock yet")
                .help("run `fetchloom lock` and commit data.lock, or sync without --locked"));
        };
        check(&project, &manifest, Some(lock)).map_err(|error| {
            error.help("run `fetchloom lock` and commit data.lock, or sync without --locked")
        })?;
    }
    let seen = Seen {
        manifest: digest::sha256(text.as_bytes()),
        lock: read
            .as_ref()
            .map(|(_, text)| digest::sha256(text.as_bytes())),
    };
    if dry_run {
        return preview_sync(context, &project, &manifest, read, &seen, &args).await;
    }
    let old = read.map(|(lock, _)| lock);
    let work = Loop::new(
        context,
        &project,
        Arc::new(net(context, &manifest)),
        open_cache()?,
    );
    let Outcome {
        lock_changed,
        pins,
        placed: placement,
    } = sync_all(
        &work,
        &manifest,
        old,
        &Upgrade::Nothing,
        args.force,
        args.verify,
        None,
        Read::Settled(&seen),
    )
    .await?;
    // a project that became a git repository after its add gets its ignore rule now
    ignore_data(&work, &manifest)?;
    tell_synced(context, &manifest, lock_changed, &placement, args.verify);
    Ok(Report {
        project: project.root,
        datasets: pins,
        lock_changed,
        placement: Some(placement),
        ..Report::default()
    })
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
    let Some((lock, lock_text)) = project.lock_and_text()? else {
        return Ok(None);
    };
    if args.locked {
        check(project, &manifest, Some(&lock)).map_err(|error| {
            error.help("run `fetchloom lock` and commit data.lock, or sync without --locked")
        })?;
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
    survey.check_doubted(&project.root, &cache)?;
    if args.verify {
        survey.verify(&project.root)?;
    }
    let Some(placement) = place::untouched(project, &manifest, &survey, &seen, false)? else {
        return Ok(None);
    };
    if project.ignored_in_git(&manifest.data_dir)?.is_some() {
        return Ok(None);
    }
    if held && let Err(err) = cache.register(&project.root) {
        tracing::debug!(%err, "could not list a read-only project in the cache");
    }
    tell_synced(context, &manifest, false, &placement, args.verify);
    Ok(Some(Report {
        project: project.root.clone(),
        datasets: Vec::new(),
        lock_changed: false,
        placement: Some(placement),
        ..Report::default()
    }))
}

/// the last lines of a sync: nothing to do, or what it synced and what to do next.
fn tell_synced(
    context: &Context,
    manifest: &Manifest,
    lock_changed: bool,
    placement: &Placement,
    verified: bool,
) {
    if !lock_changed
        && placement.placed + placement.replaced + placement.removed == 0
        && placement.kept.is_empty()
    {
        context.output.done(
            "Ok",
            if verified {
                "all pinned files are in place, checked byte for byte; nothing to do"
            } else {
                "all pinned files are in place by size and time, nothing to do"
            },
        );
    } else {
        let datasets = crate::units::count(manifest.datasets.len() as u64, "dataset", "datasets");
        if placement.kept.is_empty() {
            context
                .output
                .done("Synced", &format!("{datasets} into {}", manifest.data_dir));
        } else {
            let kept = crate::units::count(
                placement.kept.len() as u64,
                "file left unchanged",
                "files left unchanged",
            );
            context.output.done(
                "Synced",
                &format!(
                    "{datasets} into {}, {kept}; see the warnings above",
                    manifest.data_dir
                ),
            );
        }
        if lock_changed {
            context
                .output
                .next("commit data.toml and data.lock, so other clones use these pins");
        } else if let Some(name) = manifest.datasets.keys().next()
            && placement.placed + placement.replaced > 0
        {
            context.output.next(&format!(
                "`fetchloom path {name}` tells your scripts where {name} is"
            ));
        }
    }
}

/// `sync --dry-run`: what syncing would change, given data.lock as `read` and both files as
/// `seen`.
async fn preview_sync(
    context: &Context,
    project: &Project,
    manifest: &Manifest,
    read: Option<(Lock, String)>,
    seen: &Seen,
    args: &args::Sync,
) -> Result<Report, Error> {
    let old = read.map(|(lock, _)| lock);
    let work = Loop::new(
        context,
        project,
        Arc::new(net(context, manifest)),
        peek_cache()?,
    );
    let outcome = preview_all(
        &work,
        manifest,
        old,
        args.force,
        args.verify,
        None,
        Some(seen),
    )
    .await?;
    preview_ignore(&work, manifest)?;
    let nothing = !outcome.lock_changed && outcome.placed.changes_nothing();
    dry_run_done(
        &context.output,
        nothing.then_some(if args.verify {
            "all pinned files are in place, checked byte for byte"
        } else {
            "all pinned files are in place by size and time"
        }),
    );
    let pins = outcome.pins.clone();
    Ok(Report::dry(project.root.clone(), pins, outcome))
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
    let (_, manifest) = project.manifest()?;
    let old = project.lock()?;
    if args.check {
        return check(&project, &manifest, old.as_ref());
    }
    let upgrade = match args.upgrade {
        None => Upgrade::Nothing,
        Some(names) if names.is_empty() => Upgrade::All,
        Some(names) => {
            for name in &names {
                known(&manifest, name, "lock --upgrade")?;
            }
            Upgrade::Only(names.into_iter().collect::<BTreeSet<_>>())
        }
    };
    if args.dry.dry_run {
        return preview_lock(context, &project, &manifest, old, &upgrade).await;
    }
    let work = Loop::new(
        context,
        &project,
        Arc::new(net(context, &manifest)),
        open_cache()?,
    );
    let (lock, before) = work.relock(&manifest, old, &upgrade, None).await?;
    // a stop asked while the datasets were resolved leaves data.lock as it was
    context.stop.check()?;
    context.stop.finish();
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
            "run `fetchloom sync` to update {}/, then commit data.toml and data.lock",
            manifest.data_dir
        ));
    } else {
        context
            .output
            .done("Ok", "data.lock is already up to date, nothing to do");
    }
    Ok(Report {
        project: project.root,
        datasets: pins,
        lock_changed: changed,
        placement: None,
        ..Report::default()
    })
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
    let work = Loop::new(
        context,
        project,
        Arc::new(net(context, manifest)),
        peek_cache()?,
    );
    let dry = work.relock_dry(manifest, old, upgrade, None).await?;
    let output = &context.output;
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
    dry_run_done(output, nothing.then_some("data.lock is already up to date"));
    Ok(Report {
        project: project.root.clone(),
        datasets: engine::repinned(&dry.before, &dry.lock),
        lock_changed: changed,
        placement: None,
        dry_run: true,
        changes: Some(Changes {
            download,
            unpack: dry.unpack,
            ..Changes::default()
        }),
    })
}

/// `lock --check`: compares without the network and without writing.
fn check(project: &Project, manifest: &Manifest, old: Option<&Lock>) -> Result<Report, Error> {
    let empty = Lock::default();
    let old = old.unwrap_or(&empty);
    let mut stale: Vec<String> = Vec::new();
    for (name, entry) in &manifest.datasets {
        match old.get(name) {
            None => stale.push(format!("{name} is not in data.lock yet")),
            Some(locked) if locked.spec != lock::spec(entry) => {
                stale.push(format!("{name} changed in data.toml"));
            }
            Some(_) => {}
        }
    }
    for dataset in &old.datasets {
        if !manifest.datasets.contains_key(&dataset.name) {
            stale.push(format!(
                "{} is in data.lock but no longer in data.toml",
                dataset.name
            ));
        }
    }
    if stale.is_empty() {
        return Ok(Report {
            project: project.root.clone(),
            ..Report::default()
        });
    }
    let mut error = Error::new(Kind::Outdated, "data.lock is out of date with data.toml");
    for line in stale {
        error = error.detail(line);
    }
    Err(error.help("run `fetchloom lock` and commit data.lock"))
}

pub(super) async fn path(context: &Context, args: args::Path) -> Result<PathBuf, Error> {
    let project = find(&context.output)?;
    let before = state_stamp(&project);
    let (_, manifest) = project.manifest()?;
    let (name, file) = match args.target.split_once('/') {
        Some((name, file)) => (name, Some(file.trim_matches('/'))),
        None => (args.target.as_str(), None),
    };
    known(&manifest, name, "path")?;
    let lock = project.lock()?;
    let mut cache = None;
    let path = if let Some(path) = ready_path(
        context,
        &project,
        &manifest,
        lock.as_ref(),
        &mut cache,
        name,
        file,
    )? {
        path
    } else {
        drop(lock);
        drop(manifest);
        match hold_path(context, &project, &mut cache, name, file, before).await? {
            Waited::Ready(path) => path,
            Waited::Held(held) => {
                let (text, manifest) = project.manifest()?;
                known(&manifest, name, "path")?;
                let read = project.lock_and_text()?;
                if let Some(path) = ready_path(
                    context,
                    &project,
                    &manifest,
                    read.as_ref().map(|(lock, _)| lock),
                    &mut cache,
                    name,
                    file,
                )? {
                    path
                } else {
                    let cache = match cache {
                        Some(cache) => cache,
                        None => open_cache()?,
                    };
                    let work =
                        Loop::new(context, &project, Arc::new(net(context, &manifest)), cache);
                    let seen = Seen {
                        manifest: digest::sha256(text.as_bytes()),
                        lock: read
                            .as_ref()
                            .map(|(_, text)| digest::sha256(text.as_bytes())),
                    };
                    sync_all(
                        &work,
                        &manifest,
                        read.map(|(lock, _)| lock),
                        &Upgrade::Nothing,
                        false,
                        false,
                        None,
                        Read::Planned(&seen),
                    )
                    .await?;
                    drop(held);
                    let lock = project.lock()?.unwrap_or_default();
                    let planned = plan::from_cache(&manifest, &lock, &work.cache)?;
                    planned_path(&project, &manifest, &planned, name, file)?
                }
            }
        }
    };
    context.output.answer(&path.display().to_string());
    context.output.detail(
        "a placed file is a link to its file in the cache folder, or a copy of it; `fetchloom cache` shows the cache",
    );
    Ok(path)
}

/// validates the answer against the same plan used to check readiness.
fn planned_path(
    project: &Project,
    manifest: &Manifest,
    planned: &plan::Plan<'_>,
    name: &str,
    file: Option<&str>,
) -> Result<PathBuf, Error> {
    if !planned.targets.iter().any(|target| target.dataset == name) {
        return Err(Error::new(
            Kind::Usage,
            format!("{name} has no regular files after extraction"),
        )
        .help(format!(
            "set extract = false on {name} in data.toml, then run `fetchloom sync` to keep the packed archive"
        )));
    }
    let folder = plan::folder(manifest, name);
    let relative = match file.filter(|file| !file.is_empty()) {
        None => folder,
        Some(file) => {
            let wanted = format!("{folder}/{file}");
            let prefix = format!("{wanted}/");
            if !planned.targets.iter().any(|target| {
                target.dataset == name
                    && (target.path == wanted || target.path.starts_with(&prefix))
            }) {
                let prefix = format!("{folder}/");
                let inside: Vec<&str> = planned
                    .targets
                    .iter()
                    .filter(|target| target.dataset == name)
                    .filter_map(|target| target.path.strip_prefix(&prefix))
                    .collect();
                return Err(not_in(name, file, &inside));
            }
            wanted
        }
    };
    Ok(project
        .root
        .join(relative.replace('/', std::path::MAIN_SEPARATOR_STR)))
}

/// a path `fetchloom path` did not find in dataset `name`, with the paths `inside` it nearest to
/// what was asked.
fn not_in(name: &str, file: &str, inside: &[&str]) -> Error {
    let error = Error::new(
        Kind::Usage,
        format!("{name} has no file or folder called {file}"),
    );
    if let Some(best) = name::closest(file, inside.iter().copied()) {
        return error.help(format!("did you mean `fetchloom path {name}/{best}`?"));
    }
    let near = name::nearest(file, inside.iter().copied(), 3);
    let shown = match near.as_slice() {
        [] => return error.help(format!("{name} holds no files")),
        [only] => (*only).to_owned(),
        [first @ .., last] => format!("{} and {last}", first.join(", ")),
    };
    let holds = if inside.len() > near.len() {
        let files = crate::units::count(inside.len() as u64, "file", "files");
        format!("{name} holds {files}, such as {shown}")
    } else {
        format!("{name} holds {shown}")
    };
    error.detail(holds).help(format!(
        "name a path it holds, such as `fetchloom path {name}/{}`",
        near[0]
    ))
}

/// a ready answer from one lock snapshot and one plan, without a mutation hold.
fn ready_path(
    context: &Context,
    project: &Project,
    manifest: &Manifest,
    lock: Option<&Lock>,
    cache: &mut Option<Cache>,
    name: &str,
    file: Option<&str>,
) -> Result<Option<PathBuf>, Error> {
    let Some(lock) = lock else {
        return Ok(None);
    };
    let fresh = lock.get(name).is_some_and(|locked| {
        manifest
            .datasets
            .get(name)
            .is_some_and(|entry| locked.spec == lock::spec(entry))
    });
    if !fresh {
        return Ok(None);
    }
    let cache = match cache {
        Some(cache) => cache,
        slot @ None => slot.insert(open_cache()?),
    };
    let planned = plan::from_cache(manifest, lock, cache)?;
    if planned.unknown.iter().any(|(dataset, _)| *dataset == name) {
        return Ok(None);
    }
    // reject a known impossible member before a missing file can trigger downloads.
    let answer = planned_path(project, manifest, &planned, name, file)?;
    let survey = Survey::read(project, &manifest.data_dir, context.threads);
    let doubted = survey.doubted(&project.root, cache)?;
    let ready = planned
        .targets
        .iter()
        .filter(|target| target.dataset == name)
        .all(|target| {
            !doubted.contains(&target.path)
                && survey.state.files.get(&target.path).is_some_and(|placed| {
                    placed.sha256 == target.sha256
                        && survey.disk.get(&target.path) == Some(&placed.look)
                        && !survey.edited.contains(&target.path)
                        && (!target.copy || placed.writable)
                })
        });
    Ok(ready.then_some(answer))
}

/// data.toml and data.lock as a run read them, which the state records as the run leaves them.
#[derive(Clone, Copy)]
enum Read<'a> {
    /// a data folder still as a complete sync left them needs no plan, as `sync` finds.
    Settled(&'a Seen),
    /// the run plans even so, as `add` and `path` must, since they answer from what planning
    /// learns, such as what an archive holds.
    Planned(&'a Seen),
}

/// what a run did, or with `--dry-run` would do, to data.lock and the data folder.
#[derive(Debug)]
struct Outcome<T> {
    lock_changed: bool,
    /// the datasets whose pins changed: pinned for the first time, pinned to other data or files,
    /// or no longer pinned.
    pins: Vec<String>,
    /// what placing did, or would do.
    placed: T,
}

/// the lock, the cache and the data folder brought in line, in that order, and the state left
/// recording data.toml and data.lock as `read` and as this run leaves them.
#[expect(
    clippy::too_many_arguments,
    reason = "each is one independent choice a command makes for the run"
)]
async fn sync_all(
    work: &Loop<'_>,
    manifest: &Manifest,
    old: Option<Lock>,
    upgrade: &Upgrade,
    force: bool,
    verify: bool,
    given: Option<(String, Listing)>,
    read: Read<'_>,
) -> Result<Outcome<Placement>, Error> {
    let mut survey = Survey::read(work.project, &manifest.data_dir, work.context.threads);
    survey.check_doubted(&work.project.root, &work.cache)?;
    if verify {
        survey.verify(&work.project.root)?;
    }
    disk::audit(work.project, &survey, &work.cache)?;
    let seen = match read {
        Read::Settled(seen) => {
            if let Some(placement) = place::untouched(work.project, manifest, &survey, seen, true)?
            {
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
        .relock(manifest, old, upgrade, given)
        .instrument(tracing::debug_span!("relock"))
        .await
        .map_err(|error| work.before_placing(error))?;
    // a lock whose files cannot all land is refused before it is written, so data.lock never
    // pins what no sync can place. archives not unpacked here yet are checked once they are
    let plan = plan::from_cache(manifest, &lock, &work.cache)?;
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
    let placement = work
        .place(manifest, &lock, plan, force, survey, synced)
        .await?;
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
/// the downloads and unpacking it would need, then what placing would change. it stops before
/// anything [`sync_all`] changes: no download, unpacking, lock, state or registration, and cached
/// objects are read without being sealed or marked. given `seen`, it takes the settled-sync
/// shortcut as [`sync_all`] does, so a settled project needs neither the cache nor the network.
async fn preview_all(
    work: &Loop<'_>,
    manifest: &Manifest,
    old: Option<Lock>,
    force: bool,
    verify: bool,
    given: Option<(String, Listing)>,
    seen: Option<&Seen>,
) -> Result<Outcome<Preview>, Error> {
    let mut survey = Survey::read(work.project, &manifest.data_dir, work.context.threads);
    survey.check_doubted(&work.project.root, &work.cache)?;
    if verify {
        survey.verify(&work.project.root)?;
    }
    let damaged = work.damaged_objects(&survey)?;
    if let Some(seen) = seen
        && let Some(placement) = place::untouched(work.project, manifest, &survey, seen, false)?
    {
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
        lock, before, jobs, ..
    } = work
        .relock_dry(manifest, old, &Upgrade::Nothing, given)
        .await?;
    let changed = !before.holds(&lock);
    let output = &work.context.output;
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
    let preview = work.preview(manifest, &lock, force, survey, jobs, &damaged)?;
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
    place::tell_preview(output, manifest, &preview);
    Ok(Outcome {
        lock_changed: changed,
        pins: engine::repinned(&before, &lock),
        placed: preview,
    })
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
fn register(work: &Loop<'_>) {
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
    let project = Project::find(&here).ok_or_else(|| {
        Error::new(
            Kind::ProjectInvalid,
            "there is no data.toml in this folder or any folder above it",
        )
        .help("run `fetchloom add <link>` to start a project here")
    })?;
    tracing::debug!(root = %project.root.display(), "found data.toml");
    if project.root != here {
        output.aside(&format!("using the project in {}", project.root.display()));
    }
    Ok(project)
}

/// takes the project lock, waiting while another fetchloom changes the project.
enum Waited<T> {
    Held(Held),
    Ready(T),
}

async fn hold(context: &Context, project: &Project, command: &str) -> Result<Held, Error> {
    match hold_until(context, project, command, || {
        Ok(None::<std::convert::Infallible>)
    })
    .await?
    {
        Waited::Held(held) => Ok(held),
        Waited::Ready(never) => match never {},
    }
}

fn state_stamp(project: &Project) -> Option<Look> {
    let metadata = std::fs::symlink_metadata(project.own_folder().join("state")).ok()?;
    metadata.is_file().then(|| Look::of(&metadata))
}

async fn hold_path(
    context: &Context,
    project: &Project,
    cache: &mut Option<Cache>,
    name: &str,
    file: Option<&str>,
    mut before: Option<Look>,
) -> Result<Waited<PathBuf>, Error> {
    hold_until(context, project, "path", || {
        let current = state_stamp(project);
        if current == before {
            return Ok(None);
        }
        before = current;
        let (_, manifest) = project.manifest()?;
        known(&manifest, name, "path")?;
        let lock = project.lock()?;
        ready_path(
            context,
            project,
            &manifest,
            lock.as_ref(),
            cache,
            name,
            file,
        )
    })
    .await
}

async fn hold_until<T>(
    context: &Context,
    project: &Project,
    command: &str,
    mut ready: impl FnMut() -> Result<Option<T>, Error>,
) -> Result<Waited<T>, Error> {
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

/// refuses a dataset name data.toml does not have, suggesting the closest one as the `command`
/// that names it, such as `path` in `fetchloom path iris`.
pub(super) fn known(manifest: &Manifest, name: &str, command: &str) -> Result<(), Error> {
    if manifest.datasets.contains_key(name) {
        return Ok(());
    }
    let error = Error::new(
        Kind::Usage,
        format!("data.toml has no dataset called {name}"),
    );
    Err(
        match name::closest(name, manifest.datasets.keys().map(String::as_str)) {
            Some(best) => error.help(format!("did you mean `fetchloom {command} {best}`?")),
            None if manifest.datasets.is_empty() => {
                error.help("data.toml has no datasets yet. add one with `fetchloom add <link>`")
            }
            None => error.help(format!(
                "name one of {}, as in `fetchloom {command} {}`",
                manifest
                    .datasets
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", "),
                manifest.datasets.keys().next().map_or("", String::as_str)
            )),
        },
    )
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

/// the network, trusting every host data.toml names, since a person wrote them.
fn net(context: &Context, manifest: &Manifest) -> Net {
    let net = context.net();
    net.trust(manifest.datasets.values().filter_map(|entry| {
        Reference::parse(&entry.reference)
            .ok()?
            .reference
            .written_host()
    }));
    net
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

    fn waiting_project() -> (tempfile::TempDir, Project, Context) {
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

    fn waiter_state(
        project: &Project,
        stamp: std::time::SystemTime,
    ) -> crate::project::state::State {
        use crate::project::state::{Placed, State};
        let text = "[datasets.a]\nref = 'https://example.org/a'\n";
        project.write(manifest::FILE, text.as_bytes()).unwrap();
        let manifest = Manifest::parse(text).unwrap();
        let digest = digest::sha256(b"data");
        let lock = Lock {
            datasets: vec![pinned(
                "a",
                lock::spec(&manifest.datasets["a"]),
                vec![lock::LockedFile {
                    path: "x".to_owned(),
                    size: 4,
                    sha256: digest,
                    md5: None,
                    url: "https://example.org/a".to_owned(),
                    unpacks_to: None,
                }],
            )],
        };
        project.write(lock::FILE, lock.render().as_bytes()).unwrap();
        let mut state = State::default();
        state.files.insert(
            "data/a/x".to_owned(),
            Placed {
                dataset: "a".to_owned(),
                sha256: digest,
                method: crate::place::Method::Copy,
                look: Look {
                    size: 4,
                    modified: crate::project::state::nanos(stamp),
                },
                writable: true,
                checked: 1,
            },
        );
        state.synced = Some(Synced {
            manifest: digest::sha256(text.as_bytes()),
            lock: digest::sha256(lock.render().as_bytes()),
            complete: true,
        });
        state
    }

    #[tokio::test(start_paused = true)]
    async fn path_waiters_recheck_changed_state_without_claiming_the_owner() {
        let (dir, project, context) = waiting_project();
        let owner = project.own_folder().join("owner");
        let owner_before = std::fs::read(&owner).unwrap();
        let stamp = std::time::UNIX_EPOCH + Duration::from_secs(1_600_000_000);
        let state = waiter_state(&project, stamp);
        let mut cache = Some(Cache::open(dir.path().join("cache")).unwrap());
        let mut waiting = Box::pin(hold_path(
            &context,
            &project,
            &mut cache,
            "a",
            Some("x"),
            None,
        ));
        std::future::poll_fn(|task| {
            assert!(std::future::Future::poll(waiting.as_mut(), task).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        let state_path = project.own_folder().join("state");
        project.write_state(&state).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&state_path)
            .unwrap()
            .set_modified(stamp + Duration::from_secs(4))
            .unwrap();
        tokio::time::advance(Duration::from_millis(200)).await;
        std::future::poll_fn(|task| {
            assert!(
                std::future::Future::poll(waiting.as_mut(), task).is_pending(),
                "state alone cannot vouch for a missing file"
            );
            std::task::Poll::Ready(())
        })
        .await;
        let target = project.root.join("data/a/x");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, b"data").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&target)
            .unwrap()
            .set_modified(stamp)
            .unwrap();
        project.write_state(&state).unwrap();
        // whole even seconds apart, so a clock that keeps two second steps tells the writes apart
        std::fs::File::options()
            .write(true)
            .open(&state_path)
            .unwrap()
            .set_modified(stamp + Duration::from_secs(8))
            .unwrap();
        tokio::time::advance(Duration::from_millis(200)).await;
        // a waiter that never sees the project ready fails here, saying what it saw, rather than
        // waiting for ever
        let waited = tokio::time::timeout(Duration::from_secs(60), waiting).await;
        let look = |path: &Path| std::fs::metadata(path).map(|metadata| Look::of(&metadata));
        let Ok(Ok(Waited::Ready(answer))) = waited else {
            panic!(
                "the waiter never answered: state {:?}, file {:?}, recorded {:?}",
                look(&state_path),
                look(&target),
                state.files.get("data/a/x").map(|placed| placed.look),
            );
        };
        assert_eq!(answer, target);
        assert_eq!(std::fs::read(owner).unwrap(), owner_before);
    }

    fn offline_context() -> Context {
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
    fn stated(path: &str, url: &str, bytes: &[u8]) -> crate::source::Remote {
        crate::source::Remote {
            path: path.to_owned(),
            size: Some(bytes.len() as u64),
            sha256: Some(digest::sha256(bytes)),
            md5: None,
            url: url::Url::parse(url).unwrap(),
            named_by_server: false,
        }
    }

    fn listed(url: &str, files: Vec<crate::source::Remote>) -> Listing {
        Listing {
            resolved: Reference::Url(url::Url::parse(url).unwrap()),
            title: None,
            license: None,
            doi: None,
            files,
            unavailable: Vec::new(),
            warnings: Vec::new(),
        }
    }

    fn pinned(name: &str, spec: String, files: Vec<lock::LockedFile>) -> lock::Locked {
        lock::Locked {
            name: name.to_owned(),
            reference: format!("https://example.org/{name}"),
            spec,
            resolved: format!("https://example.org/{name}"),
            title: None,
            license: None,
            doi: None,
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

    fn pinned_file(path: &str, url: &str, bytes: &[u8]) -> lock::LockedFile {
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
            "[datasets.a]\nref = \"https://example.org/a\"\npath = \"shared\"\n\n[datasets.b]\nref = \"https://example.org/b\"\npath = \"shared/inner\"\n",
        )
        .unwrap();
        let a = pinned(
            "a",
            lock::spec(&manifest.datasets["a"]),
            vec![pinned_file("a", "https://example.org/a", b"a")],
        );
        let old = Lock { datasets: vec![a] };
        std::fs::write(project.root.join(lock::FILE), old.render()).unwrap();
        let work = Loop::new(&context, &project, Arc::new(context.net()), cache);
        let given = (
            "b".to_owned(),
            listed(
                "https://example.org/b",
                vec![stated("b", "https://example.org/b", b"b")],
            ),
        );

        let error = block_on(sync_all(
            &work,
            &manifest,
            Some(old.clone()),
            &Upgrade::Nothing,
            false,
            false,
            Some(given),
            Read::Planned(&unread()),
        ))
        .unwrap_err();

        assert!(error.message.contains("would share files"), "{error:?}");
        assert_eq!(
            std::fs::read_to_string(project.root.join(lock::FILE)).unwrap(),
            old.render(),
            "data.lock changed though nothing could be placed"
        );
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
            &Upgrade::Nothing,
            false,
            false,
            Some(given),
            Read::Planned(&unread()),
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
        let synced = sync_all(
            &work,
            &manifest,
            None,
            &Upgrade::Nothing,
            false,
            false,
            None,
            Read::Planned(&unlocked),
        );
        assert!(super::super::block_on(&first, synced).is_ok());
        let (lock, text) = project.lock_and_text().unwrap().unwrap();
        let seen = Seen {
            manifest: digest::sha256(b""),
            lock: Some(digest::sha256(text.as_bytes())),
        };
        let survey = Survey::read(&project, &manifest.data_dir, first.threads);
        assert!(
            place::untouched(&project, &manifest, &survey, &seen, false)
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
                    &Upgrade::Nothing,
                    false,
                    false,
                    None,
                    Read::Settled(&seen),
                )
                .await;
                context.stop.ask();
                synced.map(|_| ())
            });
            stopped.push(synced.err().map(|error| error.kind));
        }
        assert_eq!(stopped, [None, Some(Kind::Interrupted)]);
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
