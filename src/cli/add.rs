//! `fetchloom add`: a dataset into data.toml, pinned in data.lock and placed. every byte is cached
//! before either file is written, and a failure after that puts back what the add wrote.

mod several;
mod stated;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tracing::Instrument as _;

use super::args::Add;
use super::import::path_held;
use super::links;
use super::project::{Outcome, Report, committed, dry_run_done, hold, preview_all, register};
use super::sync::disk::{self, Folders, Survey};
use super::sync::place::{self, Placement, tell_marked};
use super::sync::{self as engine, Given, Loop, Upgrade};
use super::{Context, fetch, open_cache, peek_cache};
use crate::digest::{self, Sha256};
use crate::error::{Error, Kind};
use crate::name;
use crate::net::Net;
use crate::output::{Output, Warning};
use crate::plan;
use crate::project::lock::{self, Lock};
use crate::project::manifest::{self, Manifest, Source};
use crate::project::state::Synced;
use crate::project::{Held, Project};
use crate::source::folder::PAGE_FLAG;
use crate::source::{Listing, Parsed, Reference, Remote, local};
use crate::units;
use several::add_several_held;

pub(super) struct AddProject {
    pub(super) project: Project,
    /// none for a dry run, which changes nothing and so needs no hold.
    held: Option<Held>,
    /// data.toml as read, or none when there was none.
    pub(super) text: Option<String>,
    pub(super) manifest: Manifest,
    /// data.lock as read, until the add takes it to lock again.
    pub(super) old: Option<Lock>,
    lock_text: Option<String>,
    /// a failed add could not put back what it changed in data.toml or data.lock, so its error
    /// holds the steps that do.
    unrestored: AtomicBool,
    /// the records of an output's files the add read where they are, which the state takes as
    /// placed once the add's survey is read.
    seeded: std::sync::Mutex<Vec<(String, crate::project::state::Placed)>>,
    /// at a pin of an output, its folder and whether git tracks the person's files there, which the
    /// add's survey takes once it is read.
    git_keeps: std::sync::Mutex<Option<(String, bool)>>,
}

impl AddProject {
    /// gives the add's survey `records`: an output's files, read where they are, as placed.
    pub(super) fn seed(&self, records: Vec<(String, crate::project::state::Placed)>) {
        self.seeded
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend(records);
    }

    /// gives the add's survey what the pin of the output at `at` found: whether git tracks the
    /// person's files there, so git keeps the folder and no marker hides it.
    pub(super) fn git_keeps(&self, at: &str, kept: bool) {
        *self
            .git_keeps
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((at.to_owned(), kept));
    }

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
        match self.project.read_own(name) {
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

/// what an add adds: what one link or search words name, the files a list in the project names,
/// a dataset for each of several links typed together, or the files at a path on this machine.
enum Request {
    Link(String),
    /// the list `-i` reads, by its path as typed, until the project it must be in is known.
    ListFile(PathBuf),
    Several(Several),
    Path(String),
}

/// several links typed for one add, each checked, with what it reads as.
struct Several {
    links: Vec<(String, Parsed)>,
}

pub(super) async fn add(context: &Context, mut args: Add) -> Result<Report, Error> {
    let dry_run = args.dry.dry_run;
    let here =
        std::env::current_dir().map_err(|err| Error::io("cannot read the current folder", &err))?;
    let found = Project::find(&here);
    let starting = found.is_none();
    if starting {
        // the run's start found no project here, so the one this add makes is distrusted now
        crate::machine::distrust(here.clone());
    }
    let mut project = found.unwrap_or_else(|| Project::at(here.clone()));
    project.data_links = super::project::data_links(context);
    let mirrored = !context.mirrors.mirrors.list.is_empty();
    let mut request = request(&mut args, &project, mirrored)?;
    if let Request::ListFile(path) = &request {
        request = Request::Link(list_in_project(path, &project.root, &here)?);
    }
    if project.root != here {
        context
            .output
            .aside(&format!("using the project in {}", project.root.display()));
    }
    if starting && dry_run {
        tell_would_start(context, &project.root, std::env::home_dir().as_deref());
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
        seeded: std::sync::Mutex::default(),
        git_keeps: std::sync::Mutex::default(),
    };
    let read = adding.read();
    // another add may have started the project while this one waited to hold it, and then this
    // one neither started it nor may take it back
    let starting = starting && adding.text.is_none();
    let fresh = fresh.filter(|_| starting);
    if let Some(fresh) = fresh.filter(|fresh| fresh.own && adding.held.is_some()) {
        let project = Project::at(adding.project.root.clone());
        let data = fresh.data.then(|| project.root.join(manifest::DATA_DIR));
        context
            .stop
            .undo_at_quit(move || project.unstart_quitting(data.as_deref()));
    }
    let added = match (read, request) {
        (Ok(()), Request::Link(link)) => {
            add_held(context, &mut adding, args, &link, &here, starting).await
        }
        (Ok(()), Request::Several(several)) => {
            add_several_held(context, &mut adding, &args, several, &here, starting).await
        }
        // an import waits on the heap, so the add's own future stays small
        (Ok(()), Request::Path(typed)) => {
            Box::pin(path_held(
                context,
                &mut adding,
                &args,
                &typed,
                &here,
                starting,
            ))
            .await
        }
        (Ok(()), Request::ListFile(_)) => unreachable!("a list file is named by its link first"),
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

/// what `args` ask to add in `project`: a list of links read or typed, checked whole before any
/// request, a path, or one link or search words, refused when they name something on this machine.
/// `mirrored` says a mirror is set, which may hold the files of an output whose folder is gone.
fn request(args: &mut Add, project: &Project, mirrored: bool) -> Result<Request, Error> {
    if let Some(path) = args.input_file.take() {
        return Ok(Request::ListFile(path));
    }
    let mut typed = std::mem::take(&mut args.links);
    if let Some(asked) = args.name.as_deref() {
        named_as(asked, &typed)?;
    }
    if let Some(words) = one_search(&typed, true)? {
        return Ok(Request::Link(words));
    }
    if typed.len() > 1 {
        if args.no_sync {
            return Err(stated::refused_several());
        }
        return several(args, typed).map(Request::Several);
    }
    let Some(link) = typed.pop() else {
        return Err(
            Error::new(Kind::Usage, "add needs a link, or a list of links with -i").help(format!(
                "run `{}`, or `{}`",
                super::project::command("add <link>"),
                super::project::command("add -i links.txt")
            )),
        );
    };
    if let Some(refused) = output_gone(&link, project, mirrored) {
        return Err(refused);
    }
    // with --as, a folder of the project a bare word names is pinned where it is, as `add results
    // --as results` reads; without it, a bare word may be search words too
    if local::is_path(&link)
        || (args.name.is_some() && names_project_folder(&link, project))
        || written_here(&link)
    {
        if args.no_sync {
            // a mistyped path is told first, as an add without --no-sync tells it
            let here = std::env::current_dir()
                .map_err(|err| Error::io("cannot read the current folder", &err))?;
            super::import::located(&link, &here, std::env::home_dir().as_deref())?;
            return Err(stated::refused_path(&link, args.name.as_deref()));
        }
        return Ok(Request::Path(link));
    }
    super::search::refuse_local(&link, true)?;
    Ok(Request::Link(link))
}

/// whether `word`, which is no path as typed, names a file or folder here and holds what no
/// dataset's name can, a `/` or `\` between names or a file type, so no search could mean it:
/// `data/processed` is `./data/processed`, while a bare `iris` beside a folder may be search words.
fn written_here(word: &str) -> bool {
    let word = word.trim();
    let written = word.contains(['/', '\\']) || local::reads_as_path(word);
    written
        && !word.contains(':')
        && std::env::current_dir().is_ok_and(|here| local::here(word, &here).is_some())
}

/// whether `word`, which is no path as typed, names a folder inside `project`, the one the add
/// runs in, or below the folder it runs in when there is none yet: its data folder too, reached
/// through the link the project follows there.
fn names_project_folder(word: &str, project: &Project) -> bool {
    let Ok(here) = std::env::current_dir() else {
        return false;
    };
    let Ok(folder) = std::fs::canonicalize(here.join(word.trim())) else {
        return false;
    };
    let data_dir = project
        .manifest()
        .map_or_else(|_| manifest::DATA_DIR.to_owned(), |(_, read)| read.data_dir);
    folder.is_dir()
        && matches!(
            super::import::in_project(project, &data_dir, &folder),
            Ok(Some(at)) if !at.is_empty()
        )
}

/// the refusal of `word`, typed here, when it names the folder of an output of `project` with
/// nothing there, as after a move or a failed run: an add of it says so, whatever its spelling,
/// rather than send the folder's name off as search words. its ways on offer no `sync --force`
/// when no cache here holds the files it pins and no mirror set may, as `mirrored` says and the
/// state records of a sync that asked them.
fn output_gone(word: &str, project: &Project, mirrored: bool) -> Option<Error> {
    if word.contains(':') && !local::is_path(word) {
        return None;
    }
    let here = std::env::current_dir().ok()?;
    let below = here.join(word.trim());
    let mut names: Vec<&str> = Vec::new();
    for part in below.strip_prefix(&project.root).ok()?.components() {
        match part {
            std::path::Component::Normal(part) => names.push(part.to_str()?),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                names.pop()?;
            }
            _ => return None,
        }
    }
    let folded = name::fold(&names.join("/"));
    let (_, manifest) = project.manifest().ok()?;
    let (output, at) = manifest.datasets.iter().find_map(|(output, entry)| {
        let at = entry.at.as_deref()?;
        (name::fold(at) == folded).then_some((output, at))
    })?;
    match std::fs::symlink_metadata(crate::project::native(&project.root, at)) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        _ => return None,
    }
    let (state, _) = project.state();
    let lock = project.lock().ok().flatten();
    let pinned = lock
        .iter()
        .flat_map(|lock| &lock.datasets)
        .flat_map(|locked| {
            let name = locked.name.as_str();
            locked.files.iter().map(move |file| (name, file))
        });
    let left = crate::cli::sync::place::left_at(&project.root, (output, at), pinned, &state)
        .map(|from| super::folder_from_here(&project.root, &from));
    // nothing is there, so a `sync --force` would put back every file it pins
    let files = lock
        .as_ref()
        .map_or(&[][..], |lock| place::pinned_files(lock, output));
    let written = place::output_targets(output, at, files);
    let reach = super::peek_cache().map_or(local::Reach::Reachable, |cache| {
        let mirrors = place::mirror_reach(mirrored, &state.told, |_| files);
        place::unsupplied(&written, (&cache, mirrors))
            .get(output.as_str())
            .map_or(local::Reach::Reachable, |(reach, _)| *reach)
    });
    let since = match left.as_deref() {
        Some(from) => local::Since::Left { from, reach },
        None => local::Since::Gone { reach },
    };
    Some(
        Error::new(
            Kind::Usage,
            format!(
                "{} is the folder of the output {output}, and it is not there",
                word.trim()
            ),
        )
        .help(local::ways_on(output, word.trim(), since)),
    )
}

/// the reference data.toml gives the list at `path`, typed in `here`: `list:` and its path in the
/// project at `root`, where whoever clones the project finds it again. a list from stdin or from
/// outside the project could not be found again, so it is refused.
fn list_in_project(path: &Path, root: &Path, here: &Path) -> Result<String, Error> {
    let typed = crate::shown::mask_query(&path.display().to_string()).into_owned();
    let elsewhere = |what: String| {
        Error::new(Kind::Usage, what).help(format!(
            "put the list in the project folder, {}, then run `{}`, so data.toml can name it for whoever clones the project",
            units::path(root),
            super::project::command("add -i <file> --as <name>")
        ))
    };
    if path == Path::new("-") {
        return Err(elsewhere(
            "add -i reads a list from a file in the project, and stdin is none".to_owned(),
        ));
    }
    let found = std::fs::canonicalize(here.join(path))
        .map_err(|err| super::links::unread(&typed, path, here, &err))?;
    if !found.is_file() {
        return Err(Error::new(
            Kind::Usage,
            format!("{typed} is not a file, and -i reads a list of files from one"),
        ));
    }
    let inside = std::fs::canonicalize(root)
        .ok()
        .and_then(|root| found.strip_prefix(root).ok().map(Path::to_path_buf));
    let Some(inside) = inside else {
        return Err(elsewhere(format!("{typed} is outside the project folder")));
    };
    let parts: Option<Vec<&str>> = inside
        .components()
        .map(|part| match part {
            std::path::Component::Normal(name) => name.to_str(),
            _ => None,
        })
        .collect();
    let reference = format!("list:{}", parts.unwrap_or_default().join("/"));
    Reference::parse(&reference)?;
    Ok(reference)
}

/// refuses an `--as` name no dataset can have before anything is searched or fetched. a link
/// there, beside one word that could be the name, was most likely given the other way round.
fn named_as(asked: &str, typed: &[String]) -> Result<(), Error> {
    if name::dataset_name_problem(asked).is_none() {
        return Ok(());
    }
    if let [word] = typed
        && name::dataset_name_problem(word).is_none()
        && (asked.contains(':') || local::is_path(asked))
    {
        let shown = crate::shown::mask_reference(asked);
        let instead =
            super::project::command(&format!("add {} --as {word}", super::shell_word(&shown)));
        let line = super::typed::meant(&["--as", asked, word], &[asked, "--as", word], instead);
        return Err(Error::new(
            Kind::Usage,
            format!("--as takes the new dataset's name, and \"{shown}\" looks like its link"),
        )
        .help(format!("did you mean `{line}`?")));
    }
    usable(asked, &Manifest::parse("")?)
}

/// the words of one search, when `typed` holds several plain words and no link, id, DOI or path.
/// a word naming something here is refused, never sent; `pins` says the command can import it.
pub(super) fn one_search(typed: &[String], pins: bool) -> Result<Option<String>, Error> {
    if typed.len() < 2
        || !typed
            .iter()
            .all(|word| crate::source::search::is_search(word))
    {
        return Ok(None);
    }
    for word in typed {
        super::search::refuse_local(word, pins)?;
    }
    Ok(Some(typed.join(" ")))
}

/// the links typed for one add, `links`, which add a dataset each: `--as` and the patterns that
/// choose files are each for one dataset, so they are refused.
fn several(args: &Add, links: Vec<String>) -> Result<Several, Error> {
    let count = units::grouped(links.len() as u64);
    let what = units::count(links.len() as u64, "link", "links");
    if args.name.is_some() {
        return Err(Error::new(
            Kind::Usage,
            format!("--as names one dataset, and {what} add {count}"),
        )
        .help(format!(
            "add the link to name on its own, as in `{}`",
            super::project::command("add <link> --as <name>")
        )));
    }
    if !args.filter.select.is_empty() || !args.filter.exclude.is_empty() {
        return Err(Error::new(
            Kind::Usage,
            format!(
                "--select and --exclude choose the files of one dataset, and {what} add {count}"
            ),
        )
        .help(format!(
            "add the link whose files to choose on its own, as in `{}`",
            super::project::command("add <link> --select \"*.csv\"")
        )));
    }
    let links = links::typed(links)?
        .into_iter()
        .map(|(link, mut parsed)| {
            parsed.reference = parsed.reference.taking_page(args.web_page);
            (link, parsed)
        })
        .collect();
    Ok(Several { links })
}

/// what an add settles before it holds the project: whether to start one in `root`, and which
/// dataset search words name, so a no, q at the list or ctrl-c leaves nothing behind.
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

/// why an add asks before starting a project in `root`, and its question, or none when it
/// starts one unasked: `root` is the home folder or a disk's root, which take in far more than
/// one project's folders, or folders below hold projects, `below`, which the new one would wrap.
fn start_asked(
    root: &Path,
    home: Option<&Path>,
    below: &[Option<String>],
) -> Option<(String, String)> {
    let what = if root.parent().is_none() {
        Some("the root of a disk")
    } else if home.is_some_and(|home| same_file::is_same_file(home, root).unwrap_or(false)) {
        Some("your home folder")
    } else {
        None
    };
    match what {
        None if below.is_empty() => None,
        Some(what) => Some((
            format!(
                "{} is {what}, and a project there takes in every folder below it",
                root.display()
            ),
            start_question(root, what),
        )),
        None => {
            let (named, held, them) = held_below(below);
            Some((
                format!(
                    "{named} below {} {held} already, so fetchloom did not start another project around {them}",
                    root.display()
                ),
                format!(
                    "{named} below this folder {held} already.\nstart another project here, around {them}?"
                ),
            ))
        }
    }
}

/// `add --dry-run`'s word that the add would start a project in `root`, around the projects
/// below it if any, and the question the add asks first, unless `--yes` answers it.
fn tell_would_start(context: &Context, root: &Path, home: Option<&Path>) {
    let output = &context.output;
    let below = super::project::projects_below(root);
    let around = if below.is_empty() {
        String::new()
    } else {
        let named = super::project::named_below(&below);
        let projects = if below.len() == 1 {
            "the project"
        } else {
            "the projects"
        };
        format!(", around {projects} in {}", units::listed(&named, "and"))
    };
    output.step(
        "Would start",
        &format!("a project in {}{around}", units::path(root)),
    );
    if let Some((_, question)) = start_asked(root, home, &below).filter(|_| !context.yes) {
        output.hint(&format!(
            "without --dry-run, the add asks first: {} [y/N]",
            question.replace('\n', " ")
        ));
    }
}

/// asks before starting a project in `root` when it is the home folder or a disk's root, which
/// take in far more than one project's folders, or when a folder below holds a project already,
/// which the new one would wrap. with nobody to ask it refuses, unless `--yes`.
fn start_here(context: &Context, root: &Path, home: Option<&Path>) -> Result<(), Error> {
    if context.yes {
        return Ok(());
    }
    let below = super::project::projects_below(root);
    let Some((why, question)) = start_asked(root, home, &below) else {
        return Ok(());
    };
    // the command runs again from that new folder, so a relative path in it starts one further up
    let down = super::typed::one_folder_down();
    let again = down.as_ref().map_or_else(
        || "run the command again".to_owned(),
        |line| format!("run `{line}` again"),
    );
    // a line naming paths from here would miss them from a folder further down
    let there = |folder: &str| {
        down.as_ref().filter(|_| !folder.contains('/')).map_or_else(
            || "add from there".to_owned(),
            |line| format!("run `{line}` again"),
        )
    };
    let first = below.iter().flatten().next();
    let elsewhere = match (below.len(), first) {
        (0, _) => format!(
            "make a folder for the project and go into it, such as with `mkdir my-project` and `cd my-project`, then {again}"
        ),
        (1, Some(one)) => format!(
            "to add to the project in {one}/, go into its folder with `cd {}`, then {}",
            crate::suggested::shell_folder(one),
            there(one)
        ),
        (_, Some(first)) => format!(
            "to add to one of those projects, go into its folder, as with `cd {}`, then {}",
            crate::suggested::shell_folder(first),
            there(first)
        ),
        (1, None) => {
            "to add to the project below, go into its folder and add from there".to_owned()
        }
        (_, None) => {
            "to add to one of the projects below, go into its folder and add from there".to_owned()
        }
    };
    let cancel = format!(
        "starting a project in {}, so nothing changed",
        root.display()
    );
    match context
        .output
        .ask_yes_no(&question, false)
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

fn start_question(root: &Path, what: &str) -> String {
    format!(
        "{} is {what}.\na project there takes in every folder below it. start one anyway?",
        root.display()
    )
}

/// how a question names `below`, the folders below that hold a project: the folders, what they
/// hold, and the word for them.
fn held_below(below: &[Option<String>]) -> (String, &'static str, &'static str) {
    let named = super::project::named_below(below);
    match (below.len(), named.as_slice()) {
        (1, [one]) => (one.clone(), "holds a project", "it"),
        (_, many) => (units::listed(many, "and"), "hold projects", "them"),
    }
}

/// `error`, saying what state the project is in when a stop ended the add. `restored` says whether
/// the add left data.toml and data.lock with no change of its own.
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
        context.output.warn(
            Warning::ProjectNotRestored,
            "stopped, but data.toml and data.lock could not be put back as they were",
        );
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

/// resolves `link`, typed for a new dataset, with help to fix the command line when nothing is
/// there and a guess when a source may not show it. a link `manifest` has already keeps its help.
async fn resolve_typed(
    context: &Context,
    net: &Net,
    args: &Add,
    (link, parsed): (&str, &Parsed),
    (manifest, root): (&Manifest, &Path),
) -> Result<Addition, Error> {
    let reference = stored_reference(link, parsed);
    let known = manifest
        .datasets
        .values()
        .any(|entry| entry.source.reference == reference);
    match resolve_add(context, net, args, (link, parsed), root).await {
        Ok(addition) => Ok(addition),
        Err(error) if known => Err(error),
        Err(error) => Err(near_miss(context, net, link, parsed, typed(error, false)).await),
    }
}

/// `error`, a source showing no dataset of that name, which hugging face and kaggle say of a
/// private one too, with the command meant when the owner has a public dataset one typo away. a
/// source whose owners' datasets fetchloom cannot list is searched for nothing, and says nothing.
async fn near_miss(
    context: &Context,
    net: &Net,
    link: &str,
    parsed: &Parsed,
    mut error: Error,
) -> Error {
    if !error.unseen
        || context.offline.enabled()
        || crate::source::search::Near::asks(&parsed.reference).is_none()
    {
        return error;
    }
    let near = {
        let _searching = context.output.wait(
            "Searching",
            format!(
                "for a public dataset named like {}",
                parsed.reference.shown()
            ),
        );
        crate::source::search::near(net, &parsed.reference).await
    };
    let Some(near) = near else {
        return error;
    };
    // a public dataset one typo away makes a typo likelier than a missing sign-in
    error.kind = Kind::ReferenceUnresolved;
    error.need = None;
    error.help(near_help(link, &near))
}

/// the help for a `link` a source would not show, whose owner has the public dataset `near` one
/// typo away: the command meant, quoted for a shell, or only the dataset's name when the link may
/// hold a secret, which no command repeats.
fn near_help(link: &str, near: &crate::source::search::Near) -> String {
    // the link as typed, with the name fixed, keeps a link's file or folder
    let fixed = if link.contains(&near.given) {
        link.replacen(&near.given, &near.id, 1)
    } else {
        near.reference.to_string()
    };
    let alone = ["add", fixed.as_str()].map(std::ffi::OsString::from);
    match super::command_line(alone.into_iter()) {
        Some(instead) => format!(
            "did you mean `{}`?",
            super::meant(&[link], &[&fixed], instead)
        ),
        None => format!("did you mean {}?", near.reference.shown()),
    }
}

/// the loop of an add, which its questions and warnings name as one.
fn adding_loop<'a>(
    context: &'a Context,
    project: &'a Project,
    net: Arc<Net>,
    cache: crate::cache::Cache,
) -> Loop<'a> {
    Loop::new(context, project, net, cache).running("this add")
}

#[expect(
    clippy::too_many_lines,
    reason = "each step guards the next write, so they read best in order"
)]
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
    let link = &super::ftp::over_https(context, link).await?;
    let net = Arc::new(context.net());
    let parsed = validate_add(context, manifest, &net, &args, link)?;
    let cache = if dry_run {
        peek_cache()?
    } else {
        open_cache()?
    };
    // a name --as gives that waits for its list is refused before its source is asked
    if let (Some(asked), Some(old)) = (&args.name, &old)
        && manifest.datasets.contains_key(asked)
    {
        old.refuse_waiting([asked.as_str()])?;
    }
    let mut addition = resolve_typed(
        context,
        &net,
        &args,
        (link, &parsed),
        (manifest, &project.root),
    )
    .await?;
    // a name --as gives that data.toml has got this far only for the same link
    let present = match &args.name {
        Some(asked) => manifest.datasets.contains_key(asked).then(|| asked.clone()),
        None => Sources::new(manifest, old.as_ref()).present(&addition.entry, &addition.listing),
    };
    if let Some(name) = present {
        let work = adding_loop(context, project, net, cache).adding(&name);
        let present = Present {
            name: &name,
            entry: &addition.entry,
            listing: &addition.listing,
        };
        if args.no_sync {
            return stated::again(&work, adding, old, &present, (&parsed, link), dry_run);
        }
        return again(&work, adding, old, &present, dry_run, here).await;
    }
    // a dry run asks for each lone file's first byte where the add downloads it, so it names the
    // dataset as the add does and fails where the add would. patterns were written against the
    // link's own name, which the server's name would replace before they apply
    let patterns = !args.filter.select.is_empty() || !args.filter.exclude.is_empty();
    if dry_run && !args.no_sync {
        foresee_names(
            context,
            &net,
            &cache,
            vec![&mut addition.listing],
            (!args.no_extract, args.web_page),
            !patterns,
        )
        .await
        .map_err(|error| typed(error, false))?;
    } else if args.name.is_none() && !dry_run && !patterns && !args.no_sync {
        // these downloads run before relock, so the data folder a size their server states is
        // weighed against is noted here
        context.placing_into(&project.root, &manifest.data_dir);
        learn_names(
            context,
            &net,
            &cache,
            vec![&mut addition.listing],
            (!args.no_extract, args.web_page),
        )
        .await
        .map_err(|error| typed(error, false))?;
    }
    let name = new_name(context, &mut args, &addition, &parsed, manifest)?;
    let text = adding.text.as_deref().unwrap_or_default();
    let (updated, new_manifest) = added(text, &name, &args, &addition)?;
    let work = adding_loop(context, project, net, cache).adding(&name);
    if args.no_sync {
        let shown = parsed.reference.shown();
        let pinned = stated::pinned(&name, &addition.entry, &addition.listing, (&shown, link))?;
        return stated::write(&work, adding, &updated, old, pinned, (starting, dry_run));
    }
    if dry_run {
        let given = vec![Given::Listing(name.clone(), addition.listing)];
        return preview_add(&work, &new_manifest, old, (vec![name], given), here, None).await;
    }
    let names = [name];
    let typed_line = FromLine {
        shown: parsed.reference.shown(),
        dataset: Some(names[0].clone()),
    };
    let kept = kept_files(&new_manifest, &names[0], &addition.listing);
    let tree = matches!(addition.listing.resolved, Reference::GitFolder(_))
        .then(|| addition.listing.resolved.shown_short());
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
    )
    .await
    .map_err(|error| {
        let error = some_missing(error, &names[0], kept, tree.as_deref());
        page_by_flag(error, &names[0])
    })
    .map_err(|error| match &parsed.reference {
        Reference::List(list) => moved_in(error, list),
        _ => on_lines(error, std::slice::from_ref(&typed_line)),
    })?;
    let [name] = names;
    let private = private_origins(&work, [&parsed.reference]);
    let has_files = outcome.placed.planned.contains(&name);
    let list = match &parsed.reference {
        Reference::List(list) => list.in_project(),
        _ => None,
    };
    render_add(
        &work,
        &new_manifest,
        (&name, has_files),
        here,
        (starting, &outcome.placed),
        (&private, list),
    );
    Ok(Report {
        project: project.root.clone(),
        datasets: vec![name],
        lock_changed: outcome.lock_changed,
        placement: Some(outcome.placed),
        ..Report::default()
    })
}

/// the name of the dataset `addition` adds: the one --as gives, else one made from its listing.
fn new_name(
    context: &Context,
    args: &mut Add,
    addition: &Addition,
    parsed: &Parsed,
    manifest: &Manifest,
) -> Result<String, Error> {
    match args.name.take() {
        Some(asked) => Ok(asked),
        None => choose_name(
            &context.output,
            &addition.listing,
            parsed,
            &|name| manifest.datasets.contains_key(name),
            AS,
        ),
    }
}

/// a typed link as messages name it, and the dataset it became, so a failure met later says which
/// link to fix.
struct FromLine {
    shown: String,
    dataset: Option<String>,
}

impl FromLine {
    /// each link of `several`, before any became a dataset.
    fn each(several: &Several) -> Vec<FromLine> {
        several
            .links
            .iter()
            .map(|(_, parsed)| FromLine {
                shown: parsed.reference.shown(),
                dataset: None,
            })
            .collect()
    }
}

/// how many files of `listing` the new dataset `name` keeps, after its select and exclude.
fn kept_files(manifest: &Manifest, name: &str, listing: &Listing) -> usize {
    let Some(source) = manifest.datasets.get(name).map(|entry| &entry.source) else {
        return listing.files.len();
    };
    listing
        .files
        .iter()
        .filter(|file| {
            (source.select.is_empty() || source.select.iter().any(|glob| glob.matches(&file.path)))
                && !source.exclude.iter().any(|glob| glob.matches(&file.path))
        })
        .count()
}

/// `error` from adding `name`, which keeps `kept` files. when some but not all of them had nothing
/// at their links, the link did not move: the help leaves those files out, each by its pattern.
/// `tree` is a git folder at its commit, whose tree just listed them: its own commit links cannot
/// have moved, so the help says its server has no bytes for them, whether or not others are left.
/// a private repository's files git-lfs keeps cannot be downloaded at all, which it says alike.
fn some_missing(mut error: Error, name: &str, kept: usize, tree: Option<&str>) -> Error {
    let prefix = format!("{name}/");
    let failed = |kind: fn(&str) -> bool| -> Vec<&str> {
        error
            .failures
            .iter()
            .filter(|failure| failure.help.as_deref().is_some_and(kind))
            .filter_map(|failure| failure.file.strip_prefix(&prefix))
            .collect()
    };
    let moved = failed(is_moved);
    let private = failed(is_private_lfs);
    let missing: Vec<&str> = moved.iter().chain(&private).copied().collect();
    let alone = missing.len() == error.failures.len();
    let mut why = Vec::new();
    if let Some(tree) = tree.filter(|_| !moved.is_empty()) {
        why.push(format!(
            "{tree} lists {}, but its server has no bytes for {}, as for a git-lfs file whose object was never uploaded",
            units::some_of(&moved, MOST_FLAGS),
            if moved.len() == 1 { "it" } else { "them" }
        ));
    }
    if !private.is_empty() {
        why.push(format!(
            "git-lfs keeps {}, and {}",
            units::some_of(&private, MOST_FLAGS),
            crate::source::PRIVATE_LFS
        ));
    }
    if missing.is_empty() || (why.is_empty() && (!alone || missing.len() >= kept)) {
        return error;
    }
    let them = if missing.len() == 1 { "it" } else { "them" };
    let leave = (alone && missing.len() < kept).then(|| {
        let flags: Vec<String> = missing
            .iter()
            .take(MOST_FLAGS)
            .map(|path| crate::glob::exclusion_flag(path))
            .collect();
        let flags = if missing.len() > MOST_FLAGS {
            format!("an --exclude for each, such as {}", flags[0])
        } else {
            flags.join(" ")
        };
        format!(
            "the other files are in the cache. to add {name} without {them}, run the add again with {flags}"
        )
    });
    let why = why.join(". ");
    let help = match leave {
        Some(leave) if why.is_empty() => leave,
        Some(leave) => format!("{why}. {leave}"),
        None if alone => format!("{why}, and no other file is left to pin"),
        None => why,
    };
    let left = |said: &str| is_moved(said) || is_private_lfs(said);
    for detail in &mut error.details {
        if detail.strip_prefix("help: ").is_some_and(left) {
            *detail = format!("help: {help}");
        }
    }
    for failure in &mut error.failures {
        if alone || failure.help.as_deref().is_some_and(left) {
            failure.help = Some(help.clone());
        }
    }
    if alone || error.help.as_deref().is_some_and(left) {
        error = error.help(help);
    }
    error
}

/// whether `help` is a download's for a link with nothing at it, which may have moved, or which
/// is at a commit, whose server has no bytes for its file.
fn is_moved(help: &str) -> bool {
    help == crate::net::MOVED_HELP || crate::source::is_unserved(help)
}

/// whether `help` is a download's for a private repository's file git-lfs keeps.
fn is_private_lfs(help: &str) -> bool {
    help.starts_with(crate::source::PRIVATE_LFS)
}

/// the most `--exclude` flags a help spells out.
const MOST_FLAGS: usize = 5;

/// `error` from downloading the files `list` names, with help that fixes a file with nothing at its
/// link where the link is: on its line of a list in the project, or with the owner of one at a link.
fn moved_in(mut error: Error, list: &crate::source::list::List) -> Error {
    let help = match list.in_project() {
        Some(path) => format!(
            "check each link in a browser. if it moved, put the new link on its line of {path}"
        ),
        None => "check each link in a browser. the list may name files its folder no longer holds, which only its owner can fix".to_owned(),
    };
    let moved = format!("help: {}", crate::net::MOVED_HELP);
    for detail in &mut error.details {
        if *detail == moved {
            *detail = format!("help: {help}");
        }
    }
    for failure in &mut error.failures {
        if failure.help.as_deref() == Some(crate::net::MOVED_HELP) {
            failure.help = Some(help.clone());
        }
    }
    if error.help.as_deref() == Some(crate::net::MOVED_HELP) {
        error = error.help(help);
    }
    error
}

/// `error`, about a link or id typed for a new dataset, `several` with others, with help in the
/// words of an add: data.toml has no entry to fix, so the add runs again with the right one.
fn typed(mut error: Error, several: bool) -> Error {
    if let Some(help) = error
        .help
        .as_deref()
        .and_then(|help| typed_help(help, several))
    {
        error = error.help(help);
    }
    for failure in &mut error.failures {
        if let Some(help) = failure
            .help
            .as_deref()
            .and_then(|help| typed_help(help, several))
        {
            failure.help = Some(help);
        }
    }
    error
}

/// `error` of the add's downloads with its help saying to ask for a web page through `name`'s
/// data.toml entry, which this add did not write: the add's own flag asks for it instead.
fn page_by_flag(mut error: Error, name: &str) -> Error {
    let entry = super::fetch::page_entry(name);
    let flagged = |help: &str| {
        help.contains(&entry)
            .then(|| help.replace(&entry, PAGE_FLAG))
    };
    if let Some(help) = error.help.as_deref().and_then(flagged) {
        error = error.help(help);
    }
    for failure in &mut error.failures {
        if let Some(help) = failure.help.as_deref().and_then(flagged) {
            failure.help = Some(help);
        }
    }
    error
}

/// [`typed`]'s words for `help`, which a source or server gives a link or id with nothing at it.
/// none for any other help.
fn typed_help(help: &str, several: bool) -> Option<String> {
    let moved = help == crate::net::MOVED_HELP;
    if !moved && help != crate::source::UNKNOWN_HELP {
        return None;
    }
    Some(match (moved, several) {
        (true, false) => format!(
            "check the link in a browser. if it moved, run `{}`",
            super::project::command("add <new link>")
        ),
        (true, true) => {
            "check the link in a browser. if it moved, run the add again with the new link in its place"
                .to_owned()
        }
        (false, false) => format!(
            "check the id in a browser, then run `{}` with the right one. a dataset that was removed cannot be fetched",
            super::project::command("add <id>")
        ),
        (false, true) => {
            "check the id in a browser, then run the add again with the right one in its place. a dataset that was removed cannot be fetched"
                .to_owned()
        }
    })
}

/// `error` from downloading what the typed links of `lines` name, each failure of a link's dataset
/// with [`typed`]'s help; one of no typed link stays as is.
fn on_lines(mut error: Error, lines: &[FromLine]) -> Error {
    let from = |file: &str, message: &str| {
        lines.iter().find(|line| {
            message.contains(&line.shown)
                || line.dataset.as_deref().is_some_and(|name| {
                    file.strip_prefix(name)
                        .is_some_and(|rest| rest.starts_with('/'))
                })
        })
    };
    for failure in &mut error.failures {
        if from(&failure.file, &failure.message).is_none() {
            continue;
        }
        if let Some(help) = failure
            .help
            .as_deref()
            .and_then(|help| typed_help(help, lines.len() > 1))
        {
            failure.help = Some(help);
        }
    }
    match &*error.failures {
        // the one failed file is the error itself
        [failure] => {
            error.message.clone_from(&failure.message);
            error.help = failure.help.as_deref().map(Into::into);
            error
        }
        [] if from("", &error.message).is_some() => typed(error, lines.len() > 1),
        // failures of typed links and of datasets data.toml has may share a help, which then
        // stays as it is
        _ => error,
    }
}

fn validate_add(
    context: &Context,
    manifest: &Manifest,
    net: &Net,
    args: &Add,
    link: &str,
) -> Result<Parsed, Error> {
    let mut parsed =
        Reference::parse(link).map_err(|error| super::typed::scheme_meant(error, link, "add"))?;
    parsed.reference = parsed.reference.taking_page(args.web_page);
    engine::tell_raw(&context.output, link, &parsed.reference);
    parsed.check_shareable()?;
    // no answer from the source changes whether the name --as gives is usable. the dataset of
    // that name added again from the same link is a repeat, which `again` checks
    if let Some(asked) = args.name.as_deref()
        && manifest
            .datasets
            .get(asked)
            .is_none_or(|entry| entry.source.reference != stored_reference(link, &parsed))
    {
        usable(asked, manifest)?;
    }
    // the origin the person typed, such as a dataverse install on a university network
    net.trust(parsed.reference.written_origin());
    Ok(parsed)
}

/// resolves `link`, which reads as `parsed`, as an add of it chooses files; a list in the project
/// at `root` is read there.
async fn resolve_add(
    context: &Context,
    net: &Net,
    args: &Add,
    (link, parsed): (&str, &Parsed),
    root: &Path,
) -> Result<Addition, Error> {
    let select = if args.filter.select.is_empty() {
        parsed.select()
    } else {
        args.filter.select.clone()
    };
    let patterns = crate::glob::patterns_of(&select);
    let pruning = crate::glob::patterns_of(&args.filter.exclude);
    let listing = match &parsed.reference {
        Reference::List(list) if list.in_project().is_some() => {
            list.read_in(root, crate::net::Api::most_files(net))?
        }
        reference => engine::resolve(&context.output, reference, net, &patterns, &pruning).await?,
    };
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
    entry.source.decompress = args.decompress;
    let kept = engine::filter(&parsed.reference.shown(), &entry, &listing);
    // what the patterns keep is what gets pinned, so the line counts that
    let told = kept.as_deref().ok();
    engine::tell_kept(&context.output, &listing, told.unwrap_or(&listing.files));
    kept?;
    engine::tell_unmatched_excludes(
        &context.output,
        (
            &parsed.reference.shown(),
            crate::plan::Patterns::OnCommandLine,
        ),
        (&entry.source.exclude, entry.source.extract),
        &listing,
    );
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
    let mut updated = manifest::with_dataset(
        text,
        name,
        &addition.reference,
        &addition.select,
        &args.filter.exclude,
        !args.no_extract,
    )?;
    for (asked, key) in [(args.decompress, "decompress"), (args.web_page, "web_page")] {
        if asked {
            updated = manifest::with_set(&updated, name, key)?;
        }
    }
    let manifest = Manifest::parse(&updated)?;
    Ok((updated, manifest))
}

/// `add --dry-run`: what adding `names` would change, `manifest` being data.toml as it would be and
/// `given` what their sources listed. `nothing` is said when there is nothing to add.
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

/// whether the datasets an add of `names` leaves alone were in place before it, as `survey` read
/// the state, so placing those it adds settles the project: the last complete sync left the
/// data.toml and data.lock the add began from, or data.toml, `lock` and the state name no other.
fn settled_before(
    adding: &AddProject,
    (manifest, lock): (&Manifest, &Lock),
    names: &[String],
    survey: &Survey,
) -> bool {
    let adds = |name: &str| names.iter().any(|added| added == name);
    let alone = manifest.datasets.keys().all(|name| adds(name))
        && lock.datasets.iter().all(|dataset| adds(&dataset.name))
        && lock.stated.iter().all(|stated| adds(stated.name()))
        && survey
            .state
            .files
            .values()
            .all(|placed| adds(&placed.dataset));
    let hash = |text: Option<&str>| text.map(|text| digest::sha256(text.as_bytes()));
    alone
        || survey.state.synced.is_some_and(|synced| {
            synced.complete
                && Some(synced.manifest) == hash(adding.text.as_deref())
                && Some(synced.lock) == hash(adding.lock_text.as_deref())
        })
}

/// brings the project in line with `manifest`, the add's data.toml, `updated` its text when the add
/// changes it. every byte goes into the cache before data.toml or data.lock is written, so an add
/// cut off while it downloads has changed nothing; a later failure or stop puts both back.
#[expect(
    clippy::too_many_lines,
    reason = "each step guards the next write, so they read best in order"
)]
pub(super) async fn sync_add(
    work: &Loop<'_>,
    adding: &AddProject,
    manifest: &Manifest,
    updated: Option<&str>,
    old: Option<Lock>,
    datasets: (&[String], Vec<Given>),
) -> Result<Outcome<Placement>, Error> {
    let (names, given) = datasets;
    let project = work.project;
    // pinning many files takes a while, which a first add sat through untold. downloads tell
    // their own lines, so this one starts once every byte is in, or at once for an import
    let imported = !given.is_empty() && given.iter().all(|given| matches!(given, Given::Locked(_)));
    let mut pinning = imported.then(|| pinning_line(work, names)).flatten();
    let stop = &work.context.stop;
    let mut survey = Survey::read(project, &Folders::of(manifest), work.context.threads, stop)?;
    let seeded = std::mem::take(
        &mut *adding
            .seeded
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    );
    for (path, mut placed) in seeded {
        placed.checked = survey.started;
        survey.state.files.insert(path, placed);
        survey.needs_writing = true;
    }
    survey.git_keeps.extend(
        adding
            .git_keeps
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take(),
    );
    let restored = place::restore_aside(project, &manifest.data_dir, &mut survey, false);
    place::restored_aside(&work.context.output, &restored, false);
    survey.check_doubted(&project.root, &work.cache)?;
    disk::audit(project, &survey, &work.cache)?;
    // a dataset dropped here that the add names comes back, as `sync <name>` brings it back
    super::drop::brought_back(&mut survey, &names.iter().cloned().collect());
    work.placing_next();
    let (lock, before) = work
        .relock(manifest, old, &Upgrade::Nothing, given)
        .instrument(tracing::debug_span!("relock"))
        .await?;
    work.tell_versions_kept(manifest, &before, &lock, &Upgrade::Nothing);
    // a lock whose files cannot all land is refused before anything is written, so data.lock
    // never pins what no sync can place
    let placed = super::drop::placed_adding(manifest, names, &survey);
    let plan = plan::from_cache_adding(&placed, &lock, &work.cache, names)?;
    let settled = settled_before(adding, (&placed, &lock), names, &survey);
    let mut placing = work.prepare(&placed, &lock, plan, false, survey).await?;
    placing.vouch_rest(settled);
    if pinning.is_none() {
        pinning = pinning_line(work, names);
    }
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
    // from the first write on, a project folder the add started stays, even if it quits at once
    work.context.stop.keep_at_quit();
    // data.lock goes first: a stop between the two writes, as a kill or a second ctrl-c makes,
    // leaves a pin data.toml does not name yet, which the next sync drops, never a dataset in
    // data.toml without its pin, which no sync can make again for an import
    let committed = (|| -> Result<Outcome<Placement>, Error> {
        let written = match relocked {
            Some(text) => {
                if let Some(saved) = saved.as_mut() {
                    saved.writing(lock::FILE, text.as_bytes());
                }
                Some(work.write_rendered(&before, &lock, text)?)
            }
            None => None,
        };
        if let Some(updated) = updated {
            #[cfg(test)]
            tests::reached("between its writes", &project.root);
            // writing data.toml would lose a change made to it meanwhile, as while a big list
            // was written, so the add stops instead, and its failure puts data.lock back
            if adding.differs(manifest::FILE, adding.text.as_deref()) {
                return Err(manifest_changed_meanwhile());
            }
            if let Some(saved) = saved.as_mut() {
                saved.writing(manifest::FILE, updated.as_bytes());
            }
            project.write(manifest::FILE, updated.as_bytes())?;
        }
        // placing tells its own line
        drop(pinning);
        let lock_text = written.as_deref().or(adding.lock_text.as_deref());
        let synced = lock_text.map(|text| Synced {
            manifest: digest::sha256(manifest_text.as_bytes()),
            lock: digest::sha256(text.as_bytes()),
            complete: false,
        });
        // a stop while placing takes back the files placed for the new dataset, so the add is
        // undone whole. once every file is in, the add is complete, and a later stop finds it so
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
                unwrite_lists(work.context, project);
                if saved.keep {
                    adding.unrestored.store(true, Ordering::Relaxed);
                }
                error
            }
            None => error,
        }),
    }
}

/// removes the parts of file lists this run wrote that data.lock, as a failed add left it, does
/// not name, and data.lock.d once that leaves it empty: the folder is as the add found it.
fn unwrite_lists(context: &Context, project: &Project) {
    let written: Vec<String> = context
        .lists
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .filter(|(root, _)| *root == project.root)
        .map(|(_, part)| part.clone())
        .collect();
    if written.is_empty() {
        return;
    }
    let text = project.own_text(lock::FILE).unwrap_or_default();
    let named: std::collections::HashSet<String> = lock::listed(&text)
        .into_iter()
        .flat_map(|(_, parts)| parts)
        .collect();
    for part in written.iter().filter(|part| !named.contains(*part)) {
        let path = project
            .root
            .join(part.replace('/', std::path::MAIN_SEPARATOR_STR));
        if let Err(err) = std::fs::remove_file(&path) {
            tracing::debug!(%err, part, "could not remove a file list the add wrote");
        }
    }
    if let Ok(folder) = lock::lists::folder(&project.root) {
        // only an empty folder goes
        let _ = std::fs::remove_dir(folder);
    }
}

/// a line, once the work lasts, that the add pins the datasets `names`, or none for none.
fn pinning_line(work: &Loop<'_>, names: &[String]) -> Option<crate::output::Lasting> {
    let what = match names {
        [] => return None,
        [name] => name.clone(),
        many => units::count(many.len() as u64, "dataset", "datasets"),
    };
    Some(work.context.output.lasting("Pinning", what))
}

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

fn manifest_changed_meanwhile() -> Error {
    Error::new(
        Kind::ProjectChanged,
        "data.toml changed while the add wrote data.lock, so the add stopped before writing data.toml",
    )
    .help("your change is kept. run the same add again; what it downloaded is in the cache")
}

/// the last lines of an add of the dataset `added`, a name and whether it has files, in a project
/// it is `starting`, after its placement `placed`. `list` is the path of the list in the project
/// it was added from, which a clone reads to lock it again.
pub(super) fn render_add(
    work: &Loop<'_>,
    manifest: &Manifest,
    added: (&str, bool),
    here: &Path,
    (starting, placed): (bool, &Placement),
    (private, list): (&[url::Origin], Option<&str>),
) {
    tell_started(work, starting);
    tell_marked(&work.context.output, placed);
    tell_added(
        work,
        manifest,
        added,
        here,
        (starting, placed.copied.is_some()),
        (private, list),
    );
}

fn tell_started(work: &Loop<'_>, starting: bool) {
    if starting {
        work.context.output.step(
            "Started",
            &format!("a project in {}", units::path(&work.project.root)),
        );
    }
}

/// the origins of `references`, links an add pinned, that are on a private address, such as a lab
/// server's, which a later run reaches only once `allowed_origins` lists them.
fn private_origins<'r>(
    work: &Loop<'_>,
    references: impl IntoIterator<Item = &'r Reference>,
) -> Vec<url::Origin> {
    let mut private: Vec<url::Origin> = Vec::new();
    for origin in references.into_iter().filter_map(Reference::written_origin) {
        if work.net.on_private_address(&origin) && !private.contains(&origin) {
            private.push(origin);
        }
    }
    private
}

/// the last lines of an add: a warning for each `private` origin the settings do not list, since
/// typing its link trusted it for this run alone, then what to commit, the `list` in the project
/// the add read among it, and what a clone runs. the files of `imported`, a dataset of files from
/// this machine, reach a clone only through a mirror, so the line names the push that fills one.
fn close(work: &Loop<'_>, private: &[url::Origin], list: Option<&str>, imported: Option<&str>) {
    let context = work.context;
    let unlisted: Vec<&url::Origin> = private
        .iter()
        .filter(|origin| !context.settings.allowed_origins.value.contains(origin))
        .collect();
    if let Some((last, rest)) = unlisted.split_last() {
        let named: Vec<String> = unlisted
            .iter()
            .map(|origin| origin.ascii_serialization())
            .collect();
        let (what, them) = if rest.is_empty() {
            ("a private address", "it")
        } else {
            ("private addresses", "them")
        };
        let mut allowed = context.allowed();
        allowed
            .origins
            .extend(rest.iter().map(|origin| (*origin).clone()));
        let help = if unlisted
            .iter()
            .any(|origin| work.net.on_metadata_address(origin))
        {
            crate::net::METADATA_HELP.to_owned()
        } else {
            allowed.remedy(last)
        };
        context.output.warn_detailed(
            Warning::NetworkPrivateOrigin,
            &format!(
                "a later run downloads from {}, {what}, only once allowed_origins lists {them}",
                units::listed(&named, "and")
            ),
            Some(&help),
            &named,
        );
    }
    let committed = match list {
        Some(path) => format!(
            "{} and {path}",
            committed(work.project).replacen(" and ", ", ", 1)
        ),
        None => committed(work.project).to_owned(),
    };
    if let Some(name) = imported {
        let to = if context.settings.push_to.value.is_some() {
            ""
        } else {
            " --to <folder>"
        };
        context.output.next(&format!(
            "commit {committed}; the files of {name} are in this machine's cache only, so to share them, push them to a mirror with `{}`",
            super::project::command(&format!("push {name}{to}"))
        ));
        return;
    }
    // a clone is never told to paste a command that lets a link-local or metadata address in
    let pasted: Vec<&url::Origin> = private
        .iter()
        .filter(|origin| !work.net.on_metadata_address(origin))
        .collect();
    if pasted.is_empty() {
        context.output.next(&format!(
            "commit {committed}; anyone who clones runs `fetchloom sync`"
        ));
        return;
    }
    let words: Vec<String> = pasted
        .iter()
        .map(|origin| crate::suggested::shell_word(&origin.ascii_serialization()).into_owned())
        .collect();
    context.output.next(&format!(
        "commit {committed}; anyone who clones runs `fetchloom settings set allowed_origins {}`, with any origins they list already, then `fetchloom sync`",
        words.join(" ")
    ));
}

/// restores both project files after a failed add of `names`, absent ones included. a file the add
/// never wrote needs nothing. one changed since the add wrote it stays, with the steps to put it
/// back by hand, since restoring it would lose that change.
fn restore(
    project: &Project,
    saved: &mut BeforeAdd<'_>,
    names: &[String],
    mut failure: Error,
) -> Error {
    let entries = match names {
        [] => "the new dataset entries".to_owned(),
        [name] => format!("the {name} dataset entry"),
        many => format!("the {} dataset entries", units::listed(many, "and")),
    };
    let mut complete = true;
    let mut restored = Vec::new();
    for file in &saved.files {
        let now = project.read_own(file.name);
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

/// data.toml's sourced datasets by their reference and by what data.lock resolved it to, so a link
/// finds the dataset pinning it without a look at every other.
struct Sources<'a> {
    by_reference: HashMap<&'a str, Vec<(&'a str, &'a Source)>>,
    by_resolved: HashMap<&'a str, Vec<(&'a str, &'a Source)>>,
}

impl<'a> Sources<'a> {
    fn new(manifest: &'a Manifest, old: Option<&'a Lock>) -> Sources<'a> {
        let mut sources = Sources {
            by_reference: HashMap::new(),
            by_resolved: HashMap::new(),
        };
        for (name, entry) in &manifest.datasets {
            let source = &entry.source;
            sources
                .by_reference
                .entry(&source.reference)
                .or_default()
                .push((name, source));
            if let Some(locked) = old
                .and_then(|old| old.get(name))
                .filter(|locked| locked.reference == source.reference)
            {
                sources
                    .by_resolved
                    .entry(&locked.resolved)
                    .or_default()
                    .push((name, source));
            }
        }
        sources
    }

    /// the dataset in data.toml that pins what was just resolved, under any spelling of its
    /// reference: one with the same settings first, then the same reference, then the first name.
    fn present(&self, wanted: &manifest::Entry, listing: &Listing) -> Option<String> {
        let wanted = &wanted.source;
        let resolved = listing.resolved.to_string();
        let found: BTreeMap<&str, &Source> = self
            .by_reference
            .get(wanted.reference.as_str())
            .into_iter()
            .chain(self.by_resolved.get(resolved.as_str()))
            .flatten()
            .copied()
            .collect();
        found
            .into_iter()
            .min_by_key(|(_, source)| {
                (
                    !source.same_settings(wanted),
                    source.reference != wanted.reference,
                )
            })
            .map(|(name, _)| name.to_owned())
    }
}

/// refuses `present`, a dataset data.toml has already, when the add asks for other settings, which
/// need the person's choice.
fn same_settings(manifest: &Manifest, present: &Present<'_>) -> Result<(), Error> {
    let Present {
        name,
        entry,
        listing,
    } = *present;
    let (existing, wanted) = (&manifest.datasets[name].source, &entry.source);
    if existing.same_settings(wanted) {
        return Ok(());
    }
    let (differs, setting) =
        if existing.select != wanted.select || existing.exclude != wanted.exclude {
            ("other patterns", "the patterns of")
        } else if existing.extract != wanted.extract {
            ("another extract setting", "extract on")
        } else {
            ("another decompress setting", "decompress on")
        };
    Err(Error::new(
        Kind::Usage,
        format!(
            "{} is already in data.toml as {name}, with {differs}",
            listing.resolved.shown()
        ),
    )
    .help(format!(
        "change {setting} {name} in data.toml, or pass --as <name> to add it a second time"
    )))
}

/// `add` of a dataset data.toml has already, as [`Sources::present`] finds it. with the same
/// settings nothing changes, so adding twice is safe; other settings need the person's choice.
async fn again(
    work: &Loop<'_>,
    adding: &AddProject,
    old: Option<Lock>,
    present: &Present<'_>,
    dry_run: bool,
    here: &Path,
) -> Result<Report, Error> {
    let name = present.name;
    let manifest = &adding.manifest;
    same_settings(manifest, present)?;
    // nothing of it can be placed until its list is back, as `sync <name>` says
    if let Some(old) = &old {
        old.refuse_waiting([name])?;
    }
    let back = work.project.dropped().contains(name);
    if dry_run {
        let outcome = preview_all(work, manifest, old, false, false, Vec::new(), None).await?;
        let back = if back {
            " and would be back on this machine"
        } else {
            ""
        };
        dry_run_done(
            &work.context.output,
            Some(&format!("{name} is already in data.toml{back}")),
        );
        return Ok(Report::dry(work.project.root.clone(), Vec::new(), outcome));
    }
    let names = [name.to_owned()];
    let outcome = sync_add(work, adding, manifest, None, old, (&names, Vec::new())).await?;
    tell_marked(&work.context.output, &outcome.placed);
    let contents = if outcome.placed.planned.contains(name) {
        format!(
            "its files are in {}",
            from_here(work.project, here, &plan::folder(manifest, name))
        )
    } else {
        "its archives contain no regular files".to_owned()
    };
    let back = if back {
        " and back on this machine"
    } else {
        ""
    };
    work.context.output.done(
        "Ok",
        &format!("{name} is already in data.toml{back}, {contents}"),
    );
    Ok(Report {
        project: work.project.root.clone(),
        datasets: Vec::new(),
        lock_changed: outcome.lock_changed,
        placement: Some(outcome.placed),
        ..Report::default()
    })
}

fn tell_added(
    work: &Loop<'_>,
    manifest: &Manifest,
    (name, has_files): (&str, bool),
    here: &Path,
    (starting, copied): (bool, bool),
    (private, list): (&[url::Origin], Option<&str>),
) {
    if has_files {
        let folder = from_here(work.project, here, &plan::folder(manifest, name));
        work.context
            .output
            .done("Added", &format!("{name}, its files are in {folder}"));
        work.context.output.hint(&format!(
            "scripts can find it with `{}`",
            super::project::command(&format!("path {name}"))
        ));
        if starting {
            tell_read_only(&work.context.output, manifest, name, copied);
        }
    } else {
        work.context.output.done(
            "Added",
            &format!("{name}, its archives contain no regular files"),
        );
    }
    let imported = manifest
        .datasets
        .get(name)
        .is_some_and(manifest::Entry::is_import);
    close(work, private, list, imported.then_some(name));
}

/// why the files of `name` are read-only, and how to edit or delete them, for a person at a
/// terminal, who meets them as an editor's permission denied or `rm -r` asking about each file.
/// a script needs no telling, and the first add of a project tells it once. files `copied`
/// rather than linked share nothing with the cache, so they are read-only only to stay as pinned.
fn tell_read_only(output: &Output, manifest: &Manifest, name: &str, copied: bool) {
    let writable = manifest
        .datasets
        .get(name)
        .is_none_or(|entry| entry.copy || entry.is_output());
    if writable || !output.shows_next() {
        return;
    }
    let why = if copied {
        "its files are read-only copies, so they stay as data.lock pins them"
    } else {
        "its files are read-only, which protects the cache's copy"
    };
    output.hint(&format!(
        "{why}: to edit them, set copy = true for {name} in data.toml; to delete them, run `{}`",
        super::project::command(&format!("remove {name}"))
    ));
}

/// `path`, relative to the project's folder, as it reads from `here`, a folder inside it.
pub(super) fn from_here(project: &Project, here: &Path, path: &str) -> String {
    format!("{}{path}", super::up_to(&project.root, here))
}

/// downloads each lone file whose name only its server knows, so its dataset is named after it.
async fn learn_names(
    context: &Context,
    net: &Arc<Net>,
    cache: &crate::cache::Cache,
    listings: Vec<&mut Listing>,
    (extract, page): (bool, bool),
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
    let jobs: Vec<fetch::Job> = lone
        .iter()
        .map(|remote| {
            let mut job = fetch::listed_job(remote, &remote.path, extract);
            job.fetch.page = page;
            job
        })
        .collect();
    let lacking: Vec<&fetch::Job> = jobs
        .iter()
        .filter(|job| job.fetch.cached(cache).is_none())
        .collect();
    let planned = fetch::planned(
        context,
        lacking
            .iter()
            .map(|job| (fetch::host_of(job), job.fetch.expect.size)),
    );
    let called = fetch::called(lacking.iter().map(|job| job.label.as_str()), planned.files);
    let plan = fetch::planned_line(&context.output, &called, &planned);
    let started = std::time::Instant::now();
    // the add changes nothing before its downloads are done
    let fetched = fetch::all(context, net, cache, jobs, plan)
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
        [] => {
            fetch::tell_found(&context.output, &called, &planned, &fetched);
            return Ok(());
        }
        [(path, _)] => path.clone(),
        several => units::count(several.len() as u64, "file", "files"),
    };
    let moved = units::downloaded(
        downloaded.iter().map(|(_, moved)| *moved),
        context
            .output
            .took(crate::net::Fetched::transfer(&fetched, started)),
    );
    context
        .output
        .step("Downloaded", &format!("{what} · {moved}"));
    Ok(())
}

/// asks the server of each lone file whose name only its server knows for the file's first byte,
/// where an add or a get downloads it to learn that name: a dry run then names the dataset and
/// its file as the command would, with the size the server states, and fails where it would.
/// offline, the last download of the link the cache keeps answers instead, as it does for the
/// command. with `rename` false, as patterns written against the link's own name ask, the file
/// keeps that name.
pub(in crate::cli) async fn foresee_names(
    context: &Context,
    net: &Net,
    cache: &crate::cache::Cache,
    listings: Vec<&mut Listing>,
    (extract, page): (bool, bool),
    rename: bool,
) -> Result<(), Error> {
    use crate::net::Probed;
    for listing in listings {
        let [remote] = listing.files.as_mut_slice() else {
            continue;
        };
        if !remote.named_by_server {
            continue;
        }
        let mut job = fetch::listed_job(remote, &remote.path, extract);
        job.fetch.page = page;
        if context.offline.enabled() {
            // offline, the add takes the link's last download the cache keeps, or stops
            let Some(record) = job.fetch.remembered(cache) else {
                return Err(crate::net::offline_missing(
                    &remote.url,
                    context.offline,
                    false,
                ));
            };
            if rename {
                if let Some(given) = record.name.as_deref().and_then(name::sanitize) {
                    remote.path = given;
                }
                remote.named_by_server = false;
            }
            remote.sha256 = Some(record.sha256);
            remote.size = Some(record.size);
            continue;
        }
        let asked = {
            let _asking = context.output.wait(
                "Asking",
                format!(
                    "{} for the file's name and size",
                    crate::net::host(&remote.url)
                ),
            );
            net.probe(&job.fetch).await?
        };
        match asked {
            Probed::File {
                total,
                name,
                page,
                slashed,
                ..
            } => {
                // a web page where a file was asked is judged as the download judges it, by its
                // first bytes where its name promised data
                if page {
                    let given = name.as_deref().and_then(name::sanitize);
                    match fetch::page_call(&job, given.as_deref()) {
                        fetch::PageCall::Refused => {
                            return Err(fetch::page_refused(&job, false, slashed));
                        }
                        fetch::PageCall::ByBytes => {
                            if let Probed::File { head, .. } =
                                net.peek(&job.fetch, fetch::HEAD).await?
                                && fetch::reads_as_html(&head)
                            {
                                return Err(fetch::page_refused(&job, true, false));
                            }
                        }
                        fetch::PageCall::Taken | fetch::PageCall::Warned => {}
                    }
                }
                if rename {
                    if let Some(given) = name.as_deref().and_then(name::sanitize) {
                        remote.path = given;
                    }
                    remote.named_by_server = false;
                }
                remote.size = remote.size.or(total);
            }
            Probed::Answered { error, .. } | Probed::Unanswered(error) => return Err(error),
        }
    }
    Ok(())
}

/// refuses a name that is not a valid dataset name or that data.toml already has, as `--as`
/// gives it, saying why and suggesting one the next run accepts.
pub(super) fn usable(asked: &str, manifest: &Manifest) -> Result<(), Error> {
    if let Some(problem) = name::dataset_name_problem(asked) {
        // a link given as the name may hold a login, which neither the name nor its suggestion shows
        let typed = crate::shown::mask_reference(asked);
        // a name long enough to be refused for its length would fill the screen
        let shown = if units::columns(&typed) > 40 {
            format!("{}\u{2026}", units::within(&typed, 40))
        } else {
            typed.clone().into_owned()
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
        return Err(match name::dataset_name(&typed) {
            // lowercase letters cannot fix a name windows keeps for a device
            Some(suggested) if name::is_reserved_on_windows(asked) => {
                error.help(format!("pick another name, such as --as {suggested}"))
            }
            Some(suggested) => error.help(format!("{rule}, such as --as {suggested}")),
            None => error.help(rule),
        });
    }
    unused(asked, manifest)
}

/// how to name a dataset of one add yourself.
pub(super) const AS: &str = "choose a name with --as <name>";

/// how to name a dataset of a list yourself, which takes no `--as`.
const RENAME: &str = "to name it yourself, rename its table in data.toml, and the next sync moves its pin to the new name";

/// the name a new dataset gets without `--as`: one made from its file or its title, unless
/// `is_taken`. `rename` is how a person names it otherwise, should that name be taken.
fn choose_name(
    output: &Output,
    listing: &Listing,
    parsed: &Parsed,
    is_taken: &dyn Fn(&str) -> bool,
    rename: &str,
) -> Result<String, Error> {
    let single = match (listing.files.as_slice(), &parsed.file) {
        // the one file of a folder a select had the source list alone is still the dataset's
        ([only], _) if listing.names_its_file() && listing.within.is_empty() => Some(
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
        .or_else(|| name::reference_name(&parsed.reference.naming()))
        .unwrap_or_else(|| "dataset".to_owned());
    free_name(output, name, is_taken, rename)
}

/// `name`, which fetchloom chose, with the next number when `is_taken` says it is taken, told with
/// `rename`. it never names a windows device, as [`name::dataset_name`] made it.
pub(super) fn free_name(
    output: &Output,
    name: String,
    is_taken: &dyn Fn(&str) -> bool,
    rename: &str,
) -> Result<String, Error> {
    // a clash with --as is refused, but a name fetchloom chose takes the next number
    if !is_taken(&name) {
        return Ok(name);
    }
    let numbered = (2..100)
        .map(|number| format!("{name}-{number}"))
        .find(|numbered| !is_taken(numbered));
    let Some(numbered) = numbered else {
        return Err(taken(&name));
    };
    output.warn_with(
        Warning::NameTaken,
        &format!("{name} was taken, so this one is {numbered}"),
        rename,
    );
    Ok(numbered)
}

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
        "give this one another name with --as <name>, or run `{}` first",
        super::project::command(&format!("remove {name}"))
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
    use super::*;
    use crate::cache::Cache;
    use crate::cli::import::{Walked, tell_left_out};
    use crate::cli::project::tests::{listed, offline_context, pinned, pinned_file, stated};
    use crate::output::screen::Screen;
    use crate::output::{Level, Output, Style};
    use crate::source::recorded::block_on;

    #[test]
    fn raw_references_are_written_instead_of_repository_pages() {
        let typed = "https://github.com/org/repo/blob/main/iris.csv";
        let parsed = Reference::parse(typed).unwrap();
        assert_eq!(
            stored_reference(typed, &parsed),
            "https://raw.githubusercontent.com/org/repo/main/iris.csv"
        );
    }

    /// a file with nothing at its link is fixed where its link is: on its line of a list in the
    /// project, or by the owner of a list at a link.
    #[test]
    fn a_moved_link_of_a_list_is_fixed_in_the_list() {
        let moved = || {
            Error::new(
                Kind::ReferenceUnresolved,
                "example.org has nothing at https://example.org/gone.csv (404)",
            )
            .help(crate::net::MOVED_HELP)
        };
        for (list, help) in [
            (
                "list:lists/links.txt",
                "check each link in a browser. if it moved, put the new link on its line of lists/links.txt",
            ),
            (
                "https://example.org/SHA256SUMS",
                "check each link in a browser. the list may name files its folder no longer holds, which only its owner can fix",
            ),
        ] {
            let Reference::List(list) = Reference::parse(list).unwrap().reference else {
                panic!("{list} is no list");
            };
            assert_eq!(moved_in(moved(), &list).help.as_deref(), Some(help));
        }
    }

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

    /// a refused name is cut to 40 columns, so one in chinese shows 20 of its characters.
    #[test]
    fn a_long_refused_name_is_cut_by_the_columns_it_shows() {
        let manifest = Manifest::parse("").unwrap();
        for (asked, shown) in [
            ("x".repeat(70), format!("{}…", "x".repeat(40))),
            ("鸢".repeat(30), format!("{}…", "鸢".repeat(20))),
        ] {
            let error = usable(&asked, &manifest).unwrap_err();
            assert_eq!(
                error.message,
                format!("\"{shown}\" is not a valid dataset name")
            );
        }
    }

    /// a near miss's link is what a person typed, often pasted from a page, so the add it names
    /// reads back as a shell reads it, and a link that may hold a secret is in no command at all.
    #[test]
    fn a_near_miss_names_a_quoted_add_and_never_a_secret() {
        let near = crate::source::search::Near {
            given: "scikit-learn/irsi".to_owned(),
            id: "scikit-learn/iris".to_owned(),
            reference: Reference::parse("hf:datasets/scikit-learn/iris")
                .unwrap()
                .reference,
        };
        assert_eq!(
            near_help("hf:datasets/scikit-learn/irsi", &near),
            "did you mean `fetchloom add hf:datasets/scikit-learn/iris`?"
        );
        let help = near_help(
            "https://huggingface.co/datasets/scikit-learn/irsi/tree/main/a&b$(id)",
            &near,
        );
        // no command on windows holds `&` or `$` safely, so the help names the link apart there
        if cfg!(windows) {
            let (shown, named) = crate::suggested::withheld(&help);
            assert_eq!(shown, r#"did you mean `fetchloom add "<name>"`?"#);
            assert_eq!(
                named,
                [
                    "<name> is https://huggingface.co/datasets/scikit-learn/iris/tree/main/a&b$(id), which no command here can hold safely"
                ]
            );
        } else {
            let meant = help
                .strip_prefix("did you mean `")
                .and_then(|rest| rest.strip_suffix("`?"))
                .unwrap_or_default();
            assert_eq!(
                crate::suggested::command_words(meant, false),
                Some(
                    [
                        "fetchloom",
                        "add",
                        "https://huggingface.co/datasets/scikit-learn/iris/tree/main/a&b$(id)",
                    ]
                    .map(str::to_owned)
                    .to_vec()
                ),
                "{help}"
            );
        }
        assert_eq!(
            near_help(
                "https://huggingface.co/datasets/scikit-learn/irsi?rlkey=hunter2",
                &near
            ),
            "did you mean hf:datasets/scikit-learn/iris?"
        );
    }

    /// an add that failed before changing anything claims no rollback and keeps no recovery copy.
    #[test]
    fn a_rollback_restores_only_what_the_add_changed() {
        let dir = tempfile::tempdir().unwrap();
        let project = Project::at(dir.path().to_path_buf());
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

    /// the rollback says it kept that change, keeps the recovery copy, and still puts back the
    /// other file, which only the add changed.
    #[test]
    fn a_rollback_keeps_a_change_made_after_the_add_wrote_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let project = Project::at(dir.path().to_path_buf());
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

    /// the copy holds the exact bytes, readable by its owner alone; the text may be a credential.
    #[cfg(unix)]
    #[test]
    fn a_rollback_that_cannot_finish_names_a_private_copy_without_its_text() {
        use std::os::unix::fs::PermissionsExt;
        if rustix::process::geteuid().is_root() {
            return crate::test_skips::skip("unprivileged", "root ignores file permissions");
        }
        let dir = tempfile::tempdir().unwrap();
        let project = Project::at(dir.path().to_path_buf());
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

    /// with nobody to ask, starting a project at a disk root or in home needs --yes.
    #[test]
    fn starting_a_project_at_a_disk_root_or_in_home_asks_first() {
        let style = Style {
            color: false,
            unicode: false,
            interactive: false,
            watched: false,
            links: false,
            accessible: false,
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

    /// a new project would wrap the one in a folder below, which the person more likely meant, so
    /// with nobody to ask the add refuses and names that folder, unless --yes.
    #[test]
    fn starting_a_project_around_another_asks_first() {
        let style = Style {
            color: false,
            unicode: false,
            interactive: false,
            watched: false,
            links: false,
            accessible: false,
        };
        let mut context = Context::new(Output::new(style, Level::Quiet, false, false));
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("myproj")).unwrap();
        assert!(start_here(&context, dir.path(), None).is_ok());
        std::fs::write(dir.path().join("myproj").join(manifest::FILE), "").unwrap();
        let refused = start_here(&context, dir.path(), None).unwrap_err();
        assert_eq!(refused.kind, Kind::Unconfirmed);
        assert_eq!(
            refused.message,
            format!(
                "myproj/ below {} holds a project already, so fetchloom did not start another project around it",
                dir.path().display()
            )
        );
        assert_eq!(
            refused.help.as_deref(),
            Some(
                "to add to the project in myproj/, go into its folder with `cd myproj`, then add from there, or add --yes to start the project there anyway"
            )
        );
        context.yes = true;
        assert!(start_here(&context, dir.path(), None).is_ok());
    }

    /// the counts are of system files an import skips and links one add with `--as` was given.
    #[test]
    fn counts_in_add_lines_have_their_thousands_apart() {
        use clap::Parser as _;
        let style = Style {
            color: false,
            unicode: false,
            interactive: false,
            watched: false,
            links: false,
            accessible: false,
        };
        let output = Output::new(style, Level::Normal, false, false);
        let mut walked = Walked::default();
        walked.system = (0..1001).map(|n| format!("f{n}/.DS_Store")).collect();
        tell_left_out(&output, "../photos", &walked);
        assert_eq!(
            output.said(),
            ["    Skipped f0/.DS_Store and 1,000 more the system makes in ../photos"]
        );
        let links: Vec<String> = (0..1200)
            .map(|n| format!("https://a.example/{n}"))
            .collect();
        let line = ["fetchloom", "add", "--as", "x"].map(str::to_owned);
        let parsed =
            super::super::args::Args::try_parse_from(line.into_iter().chain(links.clone()));
        let Some(super::super::args::Command::Add(add)) = parsed.unwrap().command else {
            panic!("not an add");
        };
        let Err(refused) = several(&add, links) else {
            panic!("--as with several links was taken");
        };
        assert_eq!(
            refused.message,
            "--as names one dataset, and 1,200 links add 1,200"
        );
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
        let project = Project::at(dir.join("project"));
        std::fs::create_dir(&project.root).unwrap();
        let text = "[datasets.a]\nref = \"https://example.org/a\"\n";
        let manifest = Manifest::parse(text).unwrap();
        let old = Lock {
            datasets: vec![pinned(
                "a",
                lock::spec(&manifest.datasets["a"].source),
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
            seeded: std::sync::Mutex::default(),
            git_keeps: std::sync::Mutex::default(),
        };
        adding.read().unwrap();
        let updated = format!("{text}\n[datasets.b]\nref = \"https://example.org/b\"\n");
        let listing = listed(
            "https://example.org/b",
            vec![stated("b", "https://example.org/b", b"b")],
        );
        (adding, cache, updated, listing)
    }

    /// runs the add of b that [`adding_b`] readies, with `edit` made at `point` of it, or at
    /// `start` before it runs, and says how it ended.
    fn add_b_editing(dir: &Path, point: &'static str, edit: fn(&Path)) -> (AddProject, Error) {
        let (mut adding, cache, updated, listing) = adding_b(dir);
        let context = offline_context();
        let manifest = Manifest::parse(&updated).unwrap();
        let old = adding.old.take();
        let work = Loop::new(&context, &adding.project, Arc::new(context.net()), cache);
        EDIT_AT.set(Some((point, edit)));
        reached("start", &adding.project.root);
        let names = ["b".to_owned()];
        let outcome = block_on(sync_add(
            &work,
            &adding,
            &manifest,
            Some(&updated),
            old,
            (&names, vec![Given::Listing("b".to_owned(), listing)]),
        ));
        EDIT_AT.set(None);
        drop(work);
        let error = outcome.err();
        (
            adding,
            error.unwrap_or_else(|| Error::new(Kind::LocalIo, "the add went through")),
        )
    }

    /// the add looks again right before its first write, and writes neither file.
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

    /// the copies fail here because the metadata folder takes no new files.
    #[cfg(unix)]
    #[test]
    fn an_add_that_cannot_save_its_recovery_copies_writes_neither_file() {
        use std::os::unix::fs::PermissionsExt as _;
        if rustix::process::geteuid().is_root() {
            return crate::test_skips::skip("unprivileged", "root writes into a read-only folder");
        }
        let dir = tempfile::tempdir().unwrap();
        let (adding, error) = add_b_editing(dir.path(), "start", |root| {
            let folder = root.join(".fetchloom");
            std::fs::create_dir_all(&folder).unwrap();
            std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o555)).unwrap();
        });
        let root = &adding.project.root;
        std::fs::set_permissions(
            root.join(".fetchloom"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        assert_eq!(error.kind, Kind::LocalIo, "{error:?}");
        assert_eq!(
            error.message,
            "could not save a recovery copy of data.toml: permission was denied"
        );
        assert_eq!(
            std::fs::read_to_string(root.join(manifest::FILE)).unwrap(),
            adding.text.clone().unwrap()
        );
        assert_eq!(
            std::fs::read_to_string(root.join(lock::FILE)).unwrap(),
            adding.lock_text.clone().unwrap()
        );
        assert!(!root.join("data").join("b").exists());
    }

    /// the add does not write data.toml, and its failure puts data.lock back as it found it.
    #[test]
    fn an_add_keeps_an_edit_to_data_toml_made_after_it_wrote_data_lock() {
        let dir = tempfile::tempdir().unwrap();
        let (adding, error) = add_b_editing(dir.path(), BETWEEN, |root| {
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
        assert!(
            error
                .details
                .contains(&"restored data.lock to its state before add".to_owned()),
            "{error:?}"
        );
        assert!(!root.join("data").join("b").exists());
    }

    /// the point of an add between its writes of data.lock and data.toml.
    const BETWEEN: &str = "between its writes";

    /// a run killed between its two writes, as a kill or a second ctrl-c ends it, leaves no
    /// dataset in data.toml that data.lock does not pin: an import's pin could not be made again.
    #[test]
    fn an_add_killed_between_its_writes_leaves_no_entry_without_its_pin() {
        let dir = tempfile::tempdir().unwrap();
        let killed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            add_b_editing(dir.path(), BETWEEN, |_| panic!("killed"))
        }));
        assert!(
            killed.is_err(),
            "the add was not stopped between its writes"
        );
        let root = dir.path().join("project");
        let read = |name| std::fs::read_to_string(root.join(name)).unwrap();
        let manifest = Manifest::parse(&read(manifest::FILE)).unwrap();
        let lock = Lock::parse(&read(lock::FILE)).unwrap();
        for name in manifest.datasets.keys() {
            assert!(lock.get(name).is_some(), "data.toml names {name} unpinned");
        }
    }

    thread_local! {
        /// the terminal an add a test runs draws on.
        static SCREEN: std::cell::RefCell<Option<Screen>> = const { std::cell::RefCell::new(None) };
    }

    /// a first add of many files once sat through relocking and planning untold after `Imported`.
    #[test]
    fn an_add_tells_what_it_pins_before_it_places() {
        let dir = tempfile::tempdir().unwrap();
        let (mut adding, cache, updated, listing) = adding_b(dir.path());
        let screen = Screen::default();
        SCREEN.set(Some(screen.clone()));
        let mut context = Context::new(Output::on_screen(screen.clone()));
        context.offline = crate::net::Offline::Flag;
        let manifest = Manifest::parse(&updated).unwrap();
        let old = adding.old.take();
        let work = Loop::new(&context, &adding.project, Arc::new(context.net()), cache).adding("b");
        // the add goes on once the line shows, as one of many files would still be pinning
        EDIT_AT.set(Some(("saved", |_| {
            let screen = SCREEN.with_borrow(Clone::clone).unwrap();
            screen.shows("Pinning b");
        })));
        let names = ["b".to_owned()];
        block_on(sync_add(
            &work,
            &adding,
            &manifest,
            Some(&updated),
            old,
            (&names, vec![Given::Listing("b".to_owned(), listing)]),
        ))
        .unwrap();
        EDIT_AT.set(None);
        assert!(screen.shows("Pinning b"));
    }
}
