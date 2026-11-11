//! `fetchloom add`: a dataset into data.toml, pinned in data.lock and placed with the rest of the
//! project. every byte goes into the cache before data.toml or data.lock is written, and a failure
//! after that puts back what the add wrote.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::task::JoinSet;
use tracing::Instrument as _;

use super::args::Add;
use super::import::{self, Walked};
use super::links::{self, Line};
use super::project::{
    Outcome, Report, committed, dry_run_done, hold, ignore_data, preview_all, preview_ignore,
    register,
};
use super::sync::disk::{self, Survey};
use super::sync::place::{Changes, Placement};
use super::sync::{self as engine, Given, Loop, Upgrade};
use super::{Context, fetch, open_cache, peek_cache};
use crate::archive;
use crate::digest::{self, Sha256};
use crate::error::{Error, Kind};
use crate::name;
use crate::net::Net;
use crate::output::Output;
use crate::plan;
use crate::project::lock::{self, Lock};
use crate::project::manifest::{self, Manifest, Source};
use crate::project::state::Synced;
use crate::project::{Held, Project};
use crate::source::{Listing, Parsed, Reference, Remote, local};
use crate::units;

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

/// what an add adds: what one link or search words name, a dataset for each link of a list, or
/// the files at a path on this machine, as typed.
enum Request {
    Link(String),
    List(List),
    Path(String),
}

/// a list of links `add -i` read, each line checked.
struct List {
    /// the name messages give the list, such as `links.txt`.
    name: String,
    lines: Vec<(Line, Parsed)>,
}

pub(super) async fn add(context: &Context, mut args: Add) -> Result<Report, Error> {
    let dry_run = args.dry.dry_run;
    let here =
        std::env::current_dir().map_err(|err| Error::io("cannot read the current folder", &err))?;
    let mut request = request(context, &mut args).await?;
    let found = Project::find(&here);
    let starting = found.is_none();
    let project = found.unwrap_or_else(|| Project { root: here.clone() });
    if project.root != here {
        context
            .output
            .aside(&format!("using the project in {}", project.root.display()));
    }
    before_holding(context, &mut request, &project.root, starting && !dry_run)
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
    let added = match (adding.read(), request) {
        (Ok(()), Request::Link(link)) => {
            add_held(context, &mut adding, args, &link, &here, starting).await
        }
        (Ok(()), Request::List(list)) => {
            add_list_held(context, &mut adding, &args, list, &here, starting).await
        }
        (Ok(()), Request::Path(typed)) => {
            import_held(context, &mut adding, &args, &typed, &here, starting).await
        }
        (Err(error), _) => Err(error),
    };
    added.map_err(|error| {
        let restored = !adding.unrestored.load(Ordering::Relaxed);
        if restored && let Some(fresh) = fresh {
            adding.unstart(fresh);
        }
        stopped(context, error, restored)
    })
}

/// what `args` ask to add: a list of links, read and checked whole before any request, or one
/// link or search words, refused when they name something on this machine.
async fn request(context: &Context, args: &mut Add) -> Result<Request, Error> {
    if let Some(path) = args.input_file.take() {
        let (name, text) = links::read(&path, &context.stop).await?;
        let lines = links::checked(&name, links::parse(&name, &text)?, true)?;
        return Ok(Request::List(List { name, lines }));
    }
    let Some(link) = args.link.take() else {
        return Err(
            Error::new(Kind::Usage, "add needs a link, or a list of links with -i")
                .help("run `fetchloom add <link>`, or `fetchloom add -i links.txt`"),
        );
    };
    if local::is_path(&link) {
        return Ok(Request::Path(link));
    }
    super::search::refuse_local(&link, true)?;
    Ok(Request::Link(link))
}

/// what an add settles before it holds the project: whether to start a project in `root`, and
/// which dataset search words name. a question answered no, q at the list or ctrl-c then leaves
/// nothing behind.
async fn before_holding(
    context: &Context,
    request: &mut Request,
    root: &Path,
    starting: bool,
) -> Result<(), Error> {
    if starting {
        start_here(context, root, std::env::home_dir().as_deref())?;
    }
    if let Request::Link(link) = request
        && crate::source::search::is_search(link)
    {
        *link = super::search::pick(context, &context.net(), link, "add").await?;
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
    link: &str,
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
    let parsed = validate_add(context, manifest, &net, &args, link)?;
    let cache = if dry_run {
        peek_cache()?
    } else {
        open_cache()?
    };
    let mut addition = resolve_add(context, &net, &args, link, &parsed).await?;
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
        learn_names(
            context,
            &net,
            &cache,
            vec![&mut addition.listing],
            !args.no_extract,
        )
        .await?;
    }
    let name = match args.name.take() {
        Some(asked) => asked,
        None => choose_name(&context.output, &addition.listing, &parsed, manifest, AS)?,
    };
    let text = adding.text.as_deref().unwrap_or_default();
    let (updated, new_manifest) = added(text, &name, &args, &addition)?;
    let work = Loop::new(context, project, net, cache).telling(&name);
    if dry_run {
        let given = vec![Given::Listing(name.clone(), addition.listing)];
        return preview_add(&work, &new_manifest, old, (vec![name], given), here, None).await;
    }
    let names = [name];
    let outcome = sync_add(
        &work,
        adding,
        &new_manifest,
        Some(&updated),
        old,
        (
            &names,
            vec![Given::Listing(names[0].clone(), addition.listing)],
        ),
        &[],
    )
    .await?;
    let [name] = names;
    render_add(&work, &new_manifest, &name, here, starting)?;
    Ok(Report {
        project: project.root.clone(),
        datasets: vec![name],
        lock_changed: outcome.lock_changed,
        placement: Some(outcome.placed),
        ..Report::default()
    })
}

/// `add <path>`: the files of a folder, or one file, on this machine, pinned by their bytes. with
/// `--as` naming a dataset data.toml has as an import, the files supply it instead: they must be
/// the ones data.lock pins, and then only the data folder changes. the files are only read.
async fn import_held(
    context: &Context,
    adding: &mut AddProject,
    args: &Add,
    typed: &str,
    here: &Path,
    starting: bool,
) -> Result<Report, Error> {
    let dry_run = args.dry.dry_run;
    let shown = typed.trim();
    let old = adding.old.take();
    let adding = &*adding;
    let project = &adding.project;
    let manifest = &adding.manifest;
    let start = import::located(shown, here, std::env::home_dir().as_deref())?;
    if !dry_run {
        project.check_writable()?;
    }
    let cache = if dry_run {
        peek_cache()?
    } else {
        open_cache()?
    };
    import::refuse_overlap(
        &start,
        shown,
        &project.root,
        &project.root.join(&manifest.data_dir),
        cache.root(),
    )?;
    let (supplied, source, walked) = walk_import(context, manifest, args, &start, shown)?;
    let pinned = supplied
        .as_ref()
        .and_then(|name| current(old.as_ref(), name, &manifest.datasets[name.as_str()]));
    if let (Some(name), Some(pinned)) = (&supplied, pinned)
        && let Some(difference) = import::first_difference(&walked.files, &pinned.files)
    {
        return Err(other_files(shown, name, difference));
    }
    let own = match (&supplied, args.name.as_deref()) {
        (Some(name), _) => name.clone(),
        (None, Some(asked)) => asked.to_owned(),
        (None, None) => unreserved(
            import::dataset_name(&start, start.is_file()).unwrap_or_else(|| "dataset".to_owned()),
        ),
    };
    let net = Arc::new(context.net());
    if dry_run {
        let supplying = supplied.is_some();
        let name = if supplying || args.name.is_some() {
            own
        } else {
            free_name(&context.output, own, manifest, AS)?
        };
        let work = Loop::new(context, project, net, cache).telling(&name);
        let found = (&walked, shown, supplying);
        return Ok(preview_import(&work, manifest, found, &name, here));
    }
    import::room(context, &cache, &walked, shown)?;
    let copied = import::into_cache(context, &cache, &walked)?;
    let how = if copied.cloned { "cloned" } else { "copied" };
    let told = format!(
        "{} · {}, {how} into the cache; {shown} is unchanged",
        units::count(walked.files.len() as u64, "file", "files"),
        units::size(walked.bytes())
    );
    if let Some(name) = supplied {
        let work = Loop::new(context, project, net, cache).telling(&name);
        let imported = Imported {
            name: &name,
            source: &source,
            files: copied.files,
        };
        return supply(&work, adding, old, imported, (shown, &told), here).await;
    }
    // the same files imported again under their own name change nothing, as the same link does
    if args.name.is_none() && same_import(manifest, old.as_ref(), &own, &source, &copied.files) {
        let work = Loop::new(context, project, net, cache).telling(&own);
        context.output.step("Imported", &format!("{own} · {told}"));
        return already_imported(&work, adding, old, &own, here).await;
    }
    let name = if args.name.is_some() {
        own
    } else {
        free_name(&context.output, own, manifest, AS)?
    };
    context.output.step("Imported", &format!("{name} · {told}"));
    let work = Loop::new(context, project, net, cache).telling(&name);
    let imported = Imported {
        name: &name,
        source: &source,
        files: copied.files,
    };
    import_new(&work, adding, old, imported, here, starting).await
}

/// files of a folder copied into the cache, pinned for the dataset `name` as data.toml's `source`
/// would have them.
struct Imported<'a> {
    name: &'a str,
    source: &'a Source,
    files: Vec<lock::LockedFile>,
}

/// the source of data.toml's `entry` of `name`, which an import has and a recipe does not.
fn import_source<'e>(entry: &'e manifest::Entry, name: &str) -> Result<&'e Source, Error> {
    entry.source().ok_or_else(|| {
        Error::new(
            Kind::Usage,
            format!("{name} is made by a recipe in data.toml, so no files are imported for it"),
        )
    })
}

/// the source an import is checked against: data.toml's for `supplied`, an import `--as` names,
/// whose patterns and extract setting stay as data.toml has them, or else one made from `args`,
/// once `--as` gives a usable name.
fn import_entry(manifest: &Manifest, supplied: Option<&str>, args: &Add) -> Result<Source, Error> {
    if let Some(name) = supplied {
        return supplied_entry(manifest, name, args);
    }
    if let Some(asked) = args.name.as_deref() {
        usable(asked, manifest)?;
    }
    let text = manifest::with_dataset(
        "",
        "new",
        local::IMPORTED,
        &args.filter.select,
        &args.filter.exclude,
        !args.no_extract,
    )?;
    import_source(&Manifest::parse(&text)?.datasets["new"], "new").cloned()
}

/// data.lock's pin of `name`, when `old` has one that still answers data.toml's `entry`.
fn current<'a>(
    old: Option<&'a Lock>,
    name: &str,
    entry: &manifest::Entry,
) -> Option<&'a lock::Locked> {
    old.and_then(|lock| lock.get(name))
        .filter(|locked| locked.drift(entry).is_none())
}

/// whether data.toml has `name` as an import with the settings of `source`, pinned in `old` to
/// exactly the files `copied` holds.
fn same_import(
    manifest: &Manifest,
    old: Option<&Lock>,
    name: &str,
    source: &Source,
    copied: &[lock::LockedFile],
) -> bool {
    let Some(existing) = manifest.datasets.get(name) else {
        return false;
    };
    let Some(had) = existing.source() else {
        return false;
    };
    let pin = |file: &lock::LockedFile| (file.path.clone(), file.size, file.sha256);
    local::is_imported(&had.reference)
        && had.select == source.select
        && had.exclude == source.exclude
        && had.extract == source.extract
        && current(old, name, existing)
            .is_some_and(|locked| locked.files.iter().map(pin).eq(copied.iter().map(pin)))
}

/// an import whose files data.toml and data.lock pin already: it syncs, and says so.
async fn already_imported(
    work: &Loop<'_>,
    adding: &AddProject,
    old: Option<Lock>,
    name: &str,
    here: &Path,
) -> Result<Report, Error> {
    let manifest = &adding.manifest;
    let names = [name.to_owned()];
    let outcome = sync_add(work, adding, manifest, None, old, (&names, Vec::new()), &[]).await?;
    ignore_data(work, manifest)?;
    let folder = from_here(work.project, here, &plan::folder(manifest, name));
    work.context.output.done(
        "Ok",
        &format!("{name} is already in data.toml, its files are in {folder}"),
    );
    Ok(Report {
        project: work.project.root.clone(),
        datasets: Vec::new(),
        lock_changed: outcome.lock_changed,
        placement: Some(outcome.placed),
        ..Report::default()
    })
}

/// adds `imported` to data.toml and pins it in data.lock, then places it, as any add does.
async fn import_new(
    work: &Loop<'_>,
    adding: &AddProject,
    old: Option<Lock>,
    imported: Imported<'_>,
    here: &Path,
    starting: bool,
) -> Result<Report, Error> {
    let Imported {
        name,
        source,
        files,
    } = imported;
    let text = adding.text.as_deref().unwrap_or_default();
    let patterns = |globs: &[crate::glob::Glob]| -> Vec<String> {
        globs.iter().map(|glob| glob.as_str().to_owned()).collect()
    };
    let updated = manifest::with_dataset(
        text,
        name,
        local::IMPORTED,
        &patterns(&source.select),
        &patterns(&source.exclude),
        source.extract,
    )?;
    let manifest = Manifest::parse(&updated)?;
    let pinned = import::pinned(name, import_source(&manifest.datasets[name], name)?, files);
    let names = [name.to_owned()];
    let outcome = sync_add(
        work,
        adding,
        &manifest,
        Some(&updated),
        old,
        (&names, vec![Given::Locked(pinned)]),
        &[],
    )
    .await?;
    render_add(work, &manifest, name, here, starting)?;
    Ok(Report {
        project: work.project.root.clone(),
        datasets: names.to_vec(),
        lock_changed: outcome.lock_changed,
        placement: Some(outcome.placed),
        ..Report::default()
    })
}

/// the entry of `name`, an import in data.toml that `add <path> --as <name>` supplies. its
/// patterns and extract setting stay as data.toml has them.
fn supplied_entry(manifest: &Manifest, name: &str, args: &Add) -> Result<Source, Error> {
    if !args.filter.select.is_empty() || !args.filter.exclude.is_empty() || args.no_extract {
        return Err(Error::new(
            Kind::Usage,
            format!("{name} is in data.toml already, so its patterns and extract setting come from there"),
        )
        .help(format!(
            "leave out --select, --exclude and --no-extract, or change datasets.{name} in data.toml"
        )));
    }
    import_source(&manifest.datasets[name], name).cloned()
}

/// the error of a supply whose folder, `shown`, holds other files than data.lock pins for `name`,
/// as `difference` says first.
fn other_files(shown: &str, name: &str, difference: String) -> Error {
    Error::new(
        Kind::LocalIo,
        format!("{shown} does not hold the files data.lock pins for {name}"),
    )
    .detail(difference)
    .help(format!(
        "supply the files {name} was imported from, or run `fetchloom remove {name}` and add these as a new import"
    ))
}

/// supplies an import from its files copied into the cache: when data.lock pins them, nothing
/// but the data folder changes; when it holds no pin of them, they are pinned as data.toml's
/// entry asks. other bytes than data.lock pins change nothing. `shown` names the folder, and
/// `told` what was copied.
async fn supply(
    work: &Loop<'_>,
    adding: &AddProject,
    old: Option<Lock>,
    imported: Imported<'_>,
    (shown, told): (&str, &str),
    here: &Path,
) -> Result<Report, Error> {
    let Imported {
        name,
        source,
        files,
    } = imported;
    let manifest = &adding.manifest;
    let given = match current(old.as_ref(), name, &manifest.datasets[name]) {
        Some(pinned) => {
            if let Some(difference) = import::first_other_bytes(&files, &pinned.files) {
                return Err(other_files(shown, name, difference));
            }
            Vec::new()
        }
        None => vec![Given::Locked(import::pinned(name, source, files))],
    };
    let pins = !given.is_empty();
    let output = &work.context.output;
    output.step("Supplied", &format!("{name} · {told}"));
    let names = [name.to_owned()];
    let outcome = sync_add(work, adding, manifest, None, old, (&names, given), &[]).await?;
    ignore_data(work, manifest)?;
    let folder = from_here(work.project, here, &plan::folder(manifest, name));
    if pins {
        output.done(
            "Pinned",
            &format!("{name} in data.lock, its files are in {folder}"),
        );
    } else {
        output.done(
            "Ok",
            &format!("{name} is in data.toml and data.lock as it was, its files are in {folder}"),
        );
    }
    Ok(Report {
        project: work.project.root.clone(),
        datasets: if pins { names.to_vec() } else { Vec::new() },
        lock_changed: outcome.lock_changed,
        placement: Some(outcome.placed),
        ..Report::default()
    })
}

/// `add <path> --dry-run`: what importing the files `walked` found in `shown` as `name` would
/// change, or supplying them when `supplying`, found without reading a byte of them, told from
/// `here`.
fn preview_import(
    work: &Loop<'_>,
    manifest: &Manifest,
    (walked, shown, supplying): (&Walked, &str, bool),
    name: &str,
    here: &Path,
) -> Report {
    let output = &work.context.output;
    let folder = plan::folder(manifest, name);
    let files = units::count(walked.files.len() as u64, "file", "files");
    let size = units::size(walked.bytes());
    if supplying {
        output.step(
            "Would supply",
            &format!("{name} · {files} · {size} into the cache; {shown} stays unchanged"),
        );
    } else {
        output.step(
            "Would import",
            &format!("{name} · {files} · {size} into the cache; {shown} stays unchanged"),
        );
        output.step(
            "Would add",
            &format!(
                "{name} to data.toml, its files in {}",
                from_here(work.project, here, &folder)
            ),
        );
    }
    dry_run_done(output, None);
    Report {
        project: work.project.root.clone(),
        selected: None,
        datasets: if supplying {
            Vec::new()
        } else {
            vec![name.to_owned()]
        },
        lock_changed: !supplying,
        placement: None,
        dry_run: true,
        changes: Some(Changes {
            place: walked
                .files
                .iter()
                .map(|file| format!("{folder}/{}", file.path))
                .collect(),
            ..Changes::default()
        }),
        merge: None,
    }
}

/// what an import of the folder at `start`, typed as `shown`, takes: the dataset it supplies
/// files for, when `--as` names an import data.toml has, the entry it makes, and the files a walk
/// of the folder found. it says what the walk left out, and for a new dataset that unpacks, hints
/// keeping numbered tar shards packed, as a bucket of them hints, before anything is copied.
fn walk_import(
    context: &Context,
    manifest: &Manifest,
    args: &Add,
    start: &Path,
    shown: &str,
) -> Result<(Option<String>, Source, Walked), Error> {
    let supplied = args
        .name
        .as_deref()
        .filter(|name| {
            manifest
                .datasets
                .get(*name)
                .is_some_and(manifest::Entry::is_import)
        })
        .map(str::to_owned);
    let source = import_entry(manifest, supplied.as_deref(), args)?;
    let walked = import::walk(start, shown, &source, &context.stop)?;
    let output = &context.output;
    tell_skipped(output, shown, &walked.skipped);
    tell_system(output, shown, &walked.system);
    if supplied.is_none()
        && source.extract
        && output.shows_hints()
        && let Some(shards) = archive::shards(walked.files.iter().map(|file| file.path.as_str()))
    {
        output.hint(&shards.hint(shown, "--no-extract, or extract = false in data.toml"));
    }
    Ok((supplied, source, walked))
}

/// warns about the links and special files below `shown` that an import left out.
fn tell_skipped(output: &Output, shown: &str, skipped: &[String]) {
    match skipped {
        [] => {}
        [one] => output.warn(&format!(
            "left out {one} in {shown}, a link or a special file, which fetchloom never follows or imports"
        )),
        [first, ..] => output.warn(&format!(
            "left out {} in {shown}, such as {first}, which fetchloom never follows or imports",
            units::count(skipped.len() as u64, "link or special file", "links and special files")
        )),
    }
}

/// says what an import left out as the system's own, such as finder's `.DS_Store`.
fn tell_system(output: &Output, shown: &str, system: &[String]) {
    match system {
        [] => {}
        [one] => output.step(
            "Skipped",
            &format!("{one} in {shown}, which the system makes"),
        ),
        [first, rest @ ..] => output.step(
            "Skipped",
            &format!(
                "{first} and {} more the system makes in {shown}",
                rest.len()
            ),
        ),
    }
}

/// `add -i`: a dataset for each link of a list, resolved, downloaded and placed together, with
/// data.toml and data.lock each written once. a link data.toml has already stays as it is.
async fn add_list_held(
    context: &Context,
    adding: &mut AddProject,
    args: &Add,
    list: List,
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
    let extract = !args.no_extract;
    // a list is a file anyone could have written, as data.toml is, so no host it names is trusted
    let net = Arc::new(context.net());
    let cache = if dry_run {
        peek_cache()?
    } else {
        open_cache()?
    };
    let mut listings = resolve_lines(context, &net, &list).await?;
    let entries = list
        .lines
        .iter()
        .zip(&listings)
        .map(|((line, parsed), listing)| listed_entry(&list.name, line, parsed, listing, extract))
        .collect::<Result<Vec<_>, _>>()?;
    if !dry_run {
        learn_names(
            context,
            &net,
            &cache,
            listings.iter_mut().collect(),
            extract,
        )
        .await?;
    }
    let mut text = adding.text.clone().unwrap_or_default();
    let mut manifest = adding.manifest.clone();
    let (mut names, mut given, mut promised, mut already) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for (((line, parsed), listing), listed) in list.lines.iter().zip(listings).zip(entries) {
        let at = line.named(&list.name);
        if let Some(name) = present(&adding.manifest, old.as_ref(), &listed.entry, &listing) {
            same_settings(&adding.manifest.datasets[&name], &listed.entry, &name, &at)?;
            already.push(name);
            continue;
        }
        let name = choose_name(&context.output, &listing, parsed, &manifest, RENAME)?;
        text = manifest::with_dataset(
            &text,
            &name,
            &listed.reference,
            &listed.select,
            &[],
            extract,
        )?;
        manifest = Manifest::parse(&text)?;
        if let Some(sha256) = line.sha256 {
            promised.push(Promised {
                dataset: name.clone(),
                sha256,
                line: at,
            });
        }
        given.push(Given::Listing(name.clone(), listing));
        names.push(name);
    }
    let work = Loop::new(context, project, net, cache);
    if dry_run {
        let nothing = format!("each link of {} is in data.toml already", list.name);
        return preview_add(&work, &manifest, old, (names, given), here, Some(&nothing)).await;
    }
    let updated = (!names.is_empty()).then_some(text.as_str());
    let outcome = sync_add(
        &work,
        adding,
        &manifest,
        updated,
        old,
        (&names, given),
        &promised,
    )
    .await?;
    if starting {
        context.output.step(
            "Started",
            &format!("a project in {}", project.root.display()),
        );
    }
    ignore_data(&work, &manifest)?;
    tell_list(&work, &manifest, &names, &already, here)?;
    Ok(Report {
        project: project.root.clone(),
        datasets: names,
        lock_changed: outcome.lock_changed,
        placement: Some(outcome.placed),
        ..Report::default()
    })
}

/// a dataset a line of a list names, as data.toml records it.
struct Listed {
    reference: String,
    select: Vec<String>,
    /// the entry as data.toml reads it back.
    entry: manifest::Entry,
}

/// how data.toml records what `line` of the list `list` names, once the files its listing keeps
/// are checked: a file or folder the link points at is selected, and a SHA-256 on the line must
/// name the one file left, unless its source states another.
fn listed_entry(
    list: &str,
    line: &Line,
    parsed: &Parsed,
    listing: &Listing,
    extract: bool,
) -> Result<Listed, Error> {
    let at = line.named(list);
    let select = parsed.select();
    let reference = stored_reference(&line.link, parsed);
    let alone = Manifest::parse(&manifest::with_dataset(
        "",
        "new",
        &reference,
        &select,
        &[],
        extract,
    )?)?;
    let mut entry = alone.datasets["new"].clone();
    entry.lines.clear();
    let files = match entry.source() {
        Some(source) => engine::filter(&parsed.reference.shown(), &entry, source, listing),
        None => Ok(listing.files.clone()),
    }
    .map_err(|error| links::on_line(error, &at))?;
    if let Some(sha256) = line.sha256 {
        let [file] = files.as_slice() else {
            return Err(Error::new(
                Kind::Usage,
                format!(
                    "the SHA-256 on {at} checks one file, and {} has {}",
                    parsed.reference.shown(),
                    units::count(files.len() as u64, "file", "files")
                ),
            )
            .help("list a link to one file there, or leave its SHA-256 off"));
        };
        if let Some(stated) = file.sha256.filter(|stated| *stated != sha256) {
            return Err(Error::new(
                Kind::IntegrityMismatch,
                format!("{} is not the file the SHA-256 on {at} names", file.path),
            )
            .detail(format!(
                "expected sha256 {}, the source states {}",
                sha256.hex(),
                stated.hex()
            ))
            .help(format!(
                "check the hash on {at}, or ask the file's owner which bytes are right"
            )));
        }
    }
    Ok(Listed {
        reference,
        select,
        entry,
    })
}

/// refuses a line of a list, `at`, that names `name`, a dataset data.toml has already, with other
/// settings than the line's own: adding it again would place the same data twice.
fn same_settings(
    existing: &manifest::Entry,
    wanted: &manifest::Entry,
    name: &str,
    at: &str,
) -> Result<(), Error> {
    let alike = existing
        .source()
        .zip(wanted.source())
        .is_some_and(|(existing, wanted)| {
            existing.select == wanted.select
                && existing.exclude == wanted.exclude
                && existing.extract == wanted.extract
        });
    if alike {
        return Ok(());
    }
    Err(Error::new(
        Kind::Usage,
        format!("{at} names {name}, which data.toml has already with other settings"),
    )
    .help("leave that line out, or add its link on its own with --as <name> to have it twice"))
}

/// resolves every line of `list` at once, within the network's limits, in the lines' order. the
/// first line that fails stops the add, so a list adds all its datasets or none.
async fn resolve_lines(
    context: &Context,
    net: &Arc<Net>,
    list: &List,
) -> Result<Vec<Listing>, Error> {
    let mut tasks = JoinSet::new();
    let mut asked = 0_u64;
    for (index, (line, parsed)) in list.lines.iter().enumerate() {
        engine::tell_raw(&context.output, &line.link, &parsed.reference);
        if !engine::resolves_locally(&parsed.reference) {
            asked += 1;
        }
        let reference = parsed.reference.clone();
        let folders = crate::glob::folders_of(&parsed.select());
        let net = net.clone();
        tasks.spawn(async move {
            let listing = reference.resolve_in(&*net, folders.as_deref()).await;
            (index, listing)
        });
    }
    let _waiting = (asked > 0).then(|| {
        context
            .output
            .wait("Resolving", units::count(asked, "link", "links"))
    });
    let mut listings: Vec<Option<Listing>> = list.lines.iter().map(|_| None).collect();
    let mut failed: Option<(usize, Error)> = None;
    while let Some(joined) = tasks.join_next().await {
        let (index, listing) = joined.map_err(|err| {
            Error::new(
                Kind::LocalIo,
                format!("resolving stopped unexpectedly: {err}"),
            )
        })?;
        match listing {
            Ok(listing) => {
                engine::tell_resolved(&context.output, &listing);
                listings[index] = Some(listing);
            }
            Err(error) if error.kind == Kind::Interrupted => {
                tasks.abort_all();
                return Err(error);
            }
            Err(error) => {
                if failed.as_ref().is_none_or(|(first, _)| index < *first) {
                    failed = Some((index, error));
                }
            }
        }
    }
    if let Some((index, error)) = failed {
        let at = list.lines[index].0.named(&list.name);
        return Err(links::on_line(error, &at));
    }
    Ok(listings.into_iter().flatten().collect())
}

/// a SHA-256 a line of a list gave for its dataset's one file, checked once relocking knows the
/// bytes and before anything is written.
struct Promised {
    dataset: String,
    sha256: Sha256,
    /// the line that gave it, as `line 2 of links.txt`.
    line: String,
}

impl Promised {
    /// refuses `lock` when it pins other bytes for the dataset than the hash names.
    fn kept(&self, lock: &Lock) -> Result<(), Error> {
        let files = lock
            .get(&self.dataset)
            .map_or(&[][..], |locked| locked.files.as_slice());
        match files {
            [file] if file.sha256 == self.sha256 => Ok(()),
            [file] => Err(Error::new(
                Kind::IntegrityMismatch,
                format!(
                    "{}/{} does not match the SHA-256 on {}",
                    self.dataset, file.path, self.line
                ),
            )
            .detail(format!(
                "expected sha256 {}, got {}",
                self.sha256.hex(),
                file.sha256.hex()
            ))
            .help(format!(
                "check the hash on {}, or ask the file's owner which bytes are right",
                self.line
            ))),
            files => Err(Error::new(
                Kind::Usage,
                format!(
                    "the SHA-256 on {} checks one file, and {} has {}",
                    self.line,
                    self.dataset,
                    units::count(files.len() as u64, "file", "files")
                ),
            )
            .help("list a link to one file there, or leave its SHA-256 off")),
        }
    }
}

/// the last lines of `add -i`: each dataset added and where its files are, from `here`, then
/// each one data.toml had already.
fn tell_list(
    work: &Loop<'_>,
    manifest: &Manifest,
    names: &[String],
    already: &[String],
    here: &Path,
) -> Result<(), Error> {
    let output = &work.context.output;
    let lock = work.project.lock()?.unwrap_or_default();
    let placed: std::collections::BTreeSet<String> =
        plan::from_cache(manifest, &lock, &work.cache)?
            .targets
            .iter()
            .map(|target| target.dataset.to_owned())
            .collect();
    for name in names {
        if placed.contains(name) {
            let folder = from_here(work.project, here, &plan::folder(manifest, name));
            output.done("Added", &format!("{name}, its files are in {folder}"));
        } else {
            output.done(
                "Added",
                &format!("{name}, its archives contain no regular files"),
            );
        }
    }
    for name in already {
        output.done("Ok", &format!("{name} is already in data.toml"));
    }
    match names {
        [] => return Ok(()),
        [name] => output.hint(&format!("scripts can find it with `fetchloom path {name}`")),
        _ => output.hint("scripts can find each with `fetchloom path <name>`"),
    }
    output.next(&format!(
        "commit {}; anyone who clones runs `fetchloom sync`",
        committed(work.project)
    ));
    Ok(())
}

fn validate_add(
    context: &Context,
    manifest: &Manifest,
    net: &Net,
    args: &Add,
    link: &str,
) -> Result<Parsed, Error> {
    let parsed = Reference::parse(link)?;
    engine::tell_raw(&context.output, link, &parsed.reference);
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
    link: &str,
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
    let reference = stored_reference(link, parsed);
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
    if let Some(source) = entry.source() {
        engine::filter(&parsed.reference.shown(), &entry, source, &listing)?;
    }
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

/// `add --dry-run`: what adding the datasets `names` would change, with data.toml as it would
/// be, told from `here`. `given` holds what their sources listed, and `nothing` what to say when
/// there is nothing to add.
async fn preview_add(
    work: &Loop<'_>,
    manifest: &Manifest,
    old: Option<Lock>,
    (names, given): (Vec<String>, Vec<Given>),
    here: &Path,
    nothing: Option<&str>,
) -> Result<Report, Error> {
    let outcome = preview_all(work, manifest, old, false, false, given, None).await?;
    let output = &work.context.output;
    preview_ignore(work, manifest)?;
    for name in &names {
        output.step(
            "Would add",
            &format!(
                "{name} to data.toml, its files in {}",
                from_here(work.project, here, &plan::folder(manifest, name))
            ),
        );
    }
    let nothing = nothing.filter(|_| names.is_empty() && !outcome.lock_changed);
    dry_run_done(output, nothing);
    Ok(Report::dry(work.project.root.clone(), names, outcome))
}

/// brings the project in line with `manifest`, an add's data.toml, whose text is `updated`
/// when the add changes data.toml, with data.lock as read, `old`. `datasets` names the datasets
/// added, with what the add already has of them. every byte the project needs goes into the cache
/// first, and only then are data.toml and data.lock written and the files placed, so an add cut
/// off while it downloads, even killed, has changed nothing. a hash the person `promised` for a
/// dataset's one file is checked before anything is written. a failure or a stop once the project
/// is changing puts data.toml and data.lock back.
async fn sync_add(
    work: &Loop<'_>,
    adding: &AddProject,
    manifest: &Manifest,
    updated: Option<&str>,
    old: Option<Lock>,
    datasets: (&[String], Vec<Given>),
    promised: &[Promised],
) -> Result<Outcome<Placement>, Error> {
    let (names, given) = datasets;
    let project = work.project;
    let mut survey = Survey::read(project, &manifest.data_dir, work.context.threads);
    survey.check_doubted(&project.root, &work.cache)?;
    disk::audit(project, &survey, &work.cache)?;
    let (lock, before) = work
        .relock(manifest, old, &Upgrade::Nothing, given)
        .instrument(tracing::debug_span!("relock"))
        .await?;
    for promise in promised {
        promise.kept(&lock)?;
    }
    // a lock whose files cannot all land is refused before anything is written, so data.lock
    // never pins what no sync can place
    let plan = plan::from_cache_adding(manifest, &lock, &work.cache, names)?;
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
        let placement = work.apply(manifest, &lock, placing, synced)?;
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
                let error = restore(project, saved, names, error);
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

/// restores both project files after a failed add of the datasets `names`, including their
/// original absence. a file the add never wrote needs nothing restored, so a failure to write it
/// again is no failure. a file someone changed after the add wrote it stays as they left it, with
/// the steps that put it back by hand, since restoring it would lose their change.
fn restore(
    project: &Project,
    saved: &mut BeforeAdd<'_>,
    names: &[String],
    mut failure: Error,
) -> Error {
    let entries = match names {
        [name] => format!("the {name} dataset entry"),
        [first @ .., last] if !first.is_empty() => {
            format!("the {} and {last} dataset entries", first.join(", "))
        }
        _ => "the new dataset entries".to_owned(),
    };
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
                "remove {entries} from {0}, or remove {0} if it holds nothing else",
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
    let wanted = wanted.source()?;
    let resolved = listing.resolved.to_string();
    manifest
        .datasets
        .iter()
        .filter_map(|(name, entry)| Some((name, entry.source()?)))
        .filter(|(name, source)| {
            source.reference == wanted.reference
                || old.and_then(|old| old.get(name)).is_some_and(|locked| {
                    locked.reference == source.reference && locked.resolved == resolved
                })
        })
        .min_by_key(|(_, source)| {
            (
                source.select != wanted.select
                    || source.exclude != wanted.exclude
                    || source.extract != wanted.extract,
                source.reference != wanted.reference,
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
    let differs = match (manifest.datasets[name].source(), entry.source()) {
        (Some(existing), Some(wanted))
            if existing.select != wanted.select || existing.exclude != wanted.exclude =>
        {
            Some(("other patterns", "the patterns of"))
        }
        (Some(existing), Some(wanted)) if existing.extract != wanted.extract => {
            Some(("another extract setting", "extract on"))
        }
        _ => None,
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
        let outcome = preview_all(work, manifest, old, false, false, Vec::new(), None).await?;
        preview_ignore(work, manifest)?;
        dry_run_done(
            &work.context.output,
            Some(&format!("{name} is already in data.toml")),
        );
        return Ok(Report::dry(work.project.root.clone(), Vec::new(), outcome));
    }
    let names = [name.to_owned()];
    let outcome = sync_add(work, adding, manifest, None, old, (&names, Vec::new()), &[]).await?;
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
    format!("{}{path}", super::up_to(&project.root, here))
}

fn has_files(work: &Loop<'_>, manifest: &Manifest, name: &str) -> Result<bool, Error> {
    let lock = work.project.lock()?.unwrap_or_default();
    Ok(plan::from_cache(manifest, &lock, &work.cache)?
        .targets
        .iter()
        .any(|target| target.dataset == name))
}

/// downloads each lone file whose name only its server knows, so each dataset can be named after
/// its file. they download together.
async fn learn_names(
    context: &Context,
    net: &Arc<Net>,
    cache: &crate::cache::Cache,
    listings: Vec<&mut Listing>,
    extract: bool,
) -> Result<(), Error> {
    let mut lone: Vec<&mut Remote> = listings
        .into_iter()
        .filter_map(|listing| match listing.files.as_mut_slice() {
            [remote] if remote.named_by_server => Some(remote),
            _ => None,
        })
        .collect();
    if lone.is_empty() {
        return Ok(());
    }
    let jobs = lone
        .iter()
        .map(|remote| fetch::listed_job(remote, &remote.path, extract))
        .collect();
    let started = std::time::Instant::now();
    // the add changes nothing before its downloads are done
    let fetched = fetch::all(context, net, cache, jobs)
        .await
        .map_err(Error::unchanged)?;
    let mut downloaded = Vec::new();
    for (remote, fetched) in lone.iter_mut().zip(&fetched) {
        if let Some(given) = fetched.name.as_deref().and_then(name::sanitize) {
            remote.path = given;
        }
        if fetched.origin == crate::net::Origin::Network {
            downloaded.push((remote.path.clone(), (fetched.size, fetched.moved)));
        }
        remote.sha256 = Some(fetched.digest);
        remote.size = Some(fetched.size);
        remote.named_by_server = false;
    }
    let what = match downloaded.as_slice() {
        [] => return Ok(()),
        [(path, _)] => path.clone(),
        several => units::count(several.len() as u64, "file", "files"),
    };
    let moved = units::downloaded(
        downloaded.iter().map(|(_, moved)| *moved),
        crate::net::Fetched::transfer(&fetched, started),
    );
    context
        .output
        .step("Downloaded", &format!("{what} · {moved}"));
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

/// how to name a dataset of one add yourself.
const AS: &str = "choose a name with --as <name>";

/// how to name a dataset of a list yourself, which takes no `--as`.
const RENAME: &str = "to name it yourself, rename its table in data.toml, and the next sync moves its pin to the new name";

/// the name a new dataset gets without `--as`: one made from its file or its title. `rename` is
/// how a person names it otherwise, should that name be taken.
fn choose_name(
    output: &Output,
    listing: &Listing,
    parsed: &Parsed,
    manifest: &Manifest,
    rename: &str,
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
    free_name(output, name, manifest, rename)
}

/// `name`, a name fetchloom chose, made usable: with `-data` added where windows keeps it for a
/// device, and the next number when data.toml has it already, told with `rename`.
fn free_name(
    output: &Output,
    name: String,
    manifest: &Manifest,
    rename: &str,
) -> Result<String, Error> {
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
        rename,
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
        let untouched = restore(&project, &mut saved, &["iris".to_owned()], failure());
        assert!(untouched.details.is_empty(), "{:?}", untouched.details);
        assert!(!saved.keep);
        drop(saved);
        std::fs::write(&manifest_path, "[datasets.iris]\nref = 'x'\n").unwrap();
        std::fs::write(project.root.join(lock::FILE), "version = 1\n").unwrap();
        let mut saved = BeforeAdd::save(&project, (Some("[datasets]\n"), None)).unwrap();
        saved.writing(manifest::FILE, b"[datasets.iris]\nref = 'x'\n");
        saved.writing(lock::FILE, b"version = 1\n");
        let restored = restore(&project, &mut saved, &["iris".to_owned()], failure());
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
        let restored = restore(&project, &mut saved, &["iris".to_owned()], failure);
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
        let restored = restore(&project, &mut saved, &["a".to_owned()], failure);
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
                lock::spec(manifest.datasets["a"].source().unwrap()),
                vec![pinned_file("a", "https://example.org/a", b"a")],
            )],
            ..Lock::default()
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
        let names = ["b".to_owned()];
        let outcome = block_on(sync_add(
            &work,
            &adding,
            &manifest,
            Some(&updated),
            old,
            (&names, vec![Given::Listing("b".to_owned(), listing)]),
            &[],
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
