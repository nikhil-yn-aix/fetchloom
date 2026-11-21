//! `fetchloom diff`: what changed in data.lock, its file lists included, between two git commits,
//! or a commit and the project folder as it is now, since git shows the lists in data.lock.d only
//! as compressed bytes. it reads git's objects alone and changes nothing.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;

use super::{Context, args, shell_word};
use crate::digest::{Md5, Sha256};
use crate::error::{Error, Kind};
use crate::output::plain;
use crate::project::git::{self, Unnamed};
use crate::project::lock::{self, Lock, Locked, LockedFile, Stated, StatedFile, lists};
use crate::project::manifest::{self, Manifest};
use crate::units;

/// what `fetchloom diff` found, as `--json` reports it.
#[derive(Serialize)]
pub(super) struct Report {
    project: PathBuf,
    from: Side,
    to: Side,
    /// each dataset data.lock pins otherwise at `to` than at `from`, by name.
    datasets: Vec<Dataset>,
}

/// a side of the diff: a revision as it was named and the commit it names, both null for the
/// project folder as it is now, and whether data.lock is there.
#[derive(Serialize)]
struct Side {
    revision: Option<String>,
    commit: Option<String>,
    has_lock: bool,
}

/// data.lock as one side has it: none there reads as an empty lock.
struct Read {
    lock: Lock,
    found: bool,
    /// the datasets whose file list the side's commit lacks, kept with no files.
    unlisted: Vec<String>,
}

/// a dataset whose pin differs between the two sides.
#[derive(Serialize)]
struct Dataset {
    name: String,
    change: Change,
    /// each field of the pin that differs, apart from its files: all of them for a dataset
    /// added or removed.
    fields: Vec<Field>,
    added: u64,
    removed: u64,
    changed: u64,
    /// files pinned with the same bytes at another link.
    relinked: u64,
    /// each file added, removed, changed or relinked, by its path in the dataset, sorted.
    files: Vec<File>,
    /// the sides, `from` or `to`, whose commit lacks the dataset's file list, so its files were
    /// not compared.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    unlisted: Vec<&'static str>,
    /// the `select` and `exclude` patterns data.toml asks of it on each side, when its spec
    /// changed and each side's data.toml tells them, for the line that tells it.
    #[serde(skip)]
    patterns: Option<Patterns>,
    /// the name `from` gave the dataset, when only its name changed, as a rename in data.toml
    /// moves its pin.
    #[serde(skip_serializing_if = "Option::is_none")]
    renamed_from: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(in crate::cli) enum Change {
    Added,
    Removed,
    Changed,
    /// the same bytes, at another link: where a clone connects is part of what data.lock says.
    Relinked,
}

/// a field of a pin, as data.lock writes it, before and after; none on a side without it.
#[derive(Debug, PartialEq, Eq, Serialize)]
struct Field {
    name: &'static str,
    before: Option<String>,
    after: Option<String>,
}

/// a file whose pin differs: its size and digests before and after, none on a side without it,
/// and its link before and after where both sides pin it at other links.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub(in crate::cli) struct File {
    pub(in crate::cli) path: String,
    pub(in crate::cli) change: Change,
    pub(in crate::cli) before: Option<Pinned>,
    pub(in crate::cli) after: Option<Pinned>,
    #[serde(skip_serializing_if = "Option::is_none")]
    link: Option<Link>,
}

/// a file's link before and after, as data.lock records them, a secret's value masked.
#[derive(Debug, PartialEq, Eq, Serialize)]
struct Link {
    before: String,
    after: String,
}

impl Link {
    /// the host before and after, as a line names them with `arrow` between, when the link goes
    /// to another host now.
    fn moved(&self, arrow: &str) -> Option<String> {
        let (before, after) = (host(&self.before), host(&self.after));
        (before != after).then(|| {
            let shown = |host: Option<String>| host.unwrap_or_else(|| "no host".to_owned());
            format!("link {} {arrow} {}", shown(before), shown(after))
        })
    }
}

/// the host `link` goes to, as a line shows it.
fn host(link: &str) -> Option<String> {
    url::Url::parse(link)
        .ok()
        .and_then(|url| url.host_str().map(|host| plain(host).into_owned()))
}

/// a file's pin: its size and SHA-256, or for a dataset not downloaded yet, the SHA-256 or MD5
/// its source states.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub(in crate::cli) struct Pinned {
    pub(in crate::cli) size: u64,
    sha256: Option<Sha256>,
    #[serde(skip_serializing_if = "Option::is_none")]
    md5: Option<Md5>,
}

impl Pinned {
    /// the pin data.lock records of `file`, a downloaded one's.
    pub(in crate::cli) fn of(file: &LockedFile) -> Pinned {
        Pinned {
            size: file.size,
            sha256: Some(file.sha256),
            md5: file.md5,
        }
    }

    /// whether `other` pins the same bytes: the same size, and the same SHA-256 where both have
    /// one, else the same MD5 where both have one, as a sync checks a stated pin. pins that share
    /// no checksum never show the same bytes.
    fn same(self, other: Pinned) -> bool {
        self.size == other.size
            && match (self.sha256, other.sha256, self.md5, other.md5) {
                (Some(this), Some(that), _, _) => this == that,
                (_, _, Some(this), Some(that)) => this == that,
                _ => false,
            }
    }

    /// whether `other` has a checksum of the kind this one has, so the two can be compared.
    fn comparable(self, other: Pinned) -> bool {
        (self.sha256.is_some() && other.sha256.is_some())
            || (self.md5.is_some() && other.md5.is_some())
    }
}

/// [`report`] on a thread that may block, which a stop leaves at once, since it only reads.
pub(super) fn run(context: &Arc<Context>, args: args::Diff) -> Result<Report, Error> {
    super::blocking(context, super::Effect::Reads, move |context| {
        report(context, &args)
    })
}

/// what changed in data.lock between the sides `args` names, told on stdout.
fn report(context: &Context, args: &args::Diff) -> Result<Report, Error> {
    let project = super::project::find(context)?;
    let root = &project.root;
    let git = git::on_path().ok_or_else(|| {
        Error::new(
            Kind::Usage,
            "fetchloom diff reads data.lock's history from git, and git is not on PATH",
        )
        .help("install git, or add the folder that holds it to PATH")
    })?;
    let from = args.from.as_deref().unwrap_or("HEAD");
    let from_commit = commit(&git, root, from)?;
    let to_commit = match args.to.as_deref() {
        Some(to) => Some(commit(&git, root, to)?),
        None => None,
    };
    // the two locks are held at once, so the second is read within what the first leaves
    let (before, after) = if let (Some(to), Some(named)) = (args.to.as_deref(), &to_commit) {
        let before = at(&git, root, &from_commit, from, 0)?;
        let after = at(&git, root, named, to, before.lock.taken())?;
        (before, after)
    } else {
        let mut now = project.lock()?;
        // a list the project folder lacks leaves its dataset's files uncompared, as a commit's does
        let unlisted = now.as_mut().map(Lock::unwait).unwrap_or_default();
        let after = Read {
            found: now.is_some(),
            lock: now.unwrap_or_default(),
            unlisted,
        };
        (
            at(&git, root, &from_commit, from, after.lock.taken())?,
            after,
        )
    };
    let mut datasets = compared(&before, &after);
    let asked = |commit: Option<&str>| match commit {
        Some(commit) => manifest_at(&git, root, commit),
        None => project.manifest().ok().map(|(_, manifest)| manifest),
    };
    name_patterns(
        &mut datasets,
        || asked(Some(&from_commit)),
        || asked(to_commit.as_deref()),
    );
    let to_shown = args.to.as_deref().map_or_else(
        || "the project folder".to_owned(),
        |to| plain(to).into_owned(),
    );
    // the same comparison with -v, which lists every file a terminal leaves out
    let mut again = vec!["diff".to_owned()];
    again.extend(
        [args.from.as_deref(), args.to.as_deref()]
            .into_iter()
            .flatten()
            .map(|revision| super::shell_word(revision).into_owned()),
    );
    again.push("-v".to_owned());
    let again = super::project::command(&again.join(" "));
    tell(context, &datasets, (&plain(from), &to_shown), &again);
    tell_sides(context, root, &before, &after, &plain(from), &to_shown);
    if to_commit.is_none() {
        tell_unpinned(context, &project, &after.lock);
    }
    Ok(Report {
        project: root.clone(),
        from: Side {
            revision: Some(from.to_owned()),
            commit: Some(from_commit),
            has_lock: before.found,
        },
        to: Side {
            revision: args.to.clone(),
            commit: to_commit,
            has_lock: after.found,
        },
        datasets,
    })
}

/// says when a side has no data.lock, so every dataset counts as added or removed, and when a
/// commit lacks the file list of a dataset, which a commit made with `git commit -a` leaves out.
fn tell_sides(context: &Context, root: &Path, before: &Read, after: &Read, from: &str, to: &str) {
    let output = &context.output;
    let lock = lock::FILE;
    if !before.found && after.found {
        output.hint(&format!(
            "{from} has no {lock} in this project's folder, so every dataset counts as added"
        ));
    }
    if before.found && !after.found {
        let to = if to == "the project folder" {
            format!("the project folder has no {lock}")
        } else {
            format!("{to} has no {lock} in this project's folder")
        };
        output.hint(&format!("{to}, so every dataset counts as removed"));
    }
    if !(before.unlisted.is_empty() && after.unlisted.is_empty()) {
        output.hint(&format!(
            "`git commit -a` leaves out new files, as the lists in {}: `{}` before committing keeps them",
            lists::FOLDER,
            crate::suggested::git(root, &format!("add {}", lists::FOLDER))
        ));
    }
}

/// says which datasets data.toml names that data.lock in the project folder does not pin yet,
/// which diff cannot show until `fetchloom lock` pins them.
fn tell_unpinned(context: &Context, project: &crate::project::Project, now: &Lock) {
    let Ok((_, manifest)) = project.manifest() else {
        return;
    };
    let unpinned: Vec<&str> = manifest
        .datasets
        .keys()
        .filter(|name| now.get(name).is_none() && now.stated(name).is_none())
        .map(String::as_str)
        .collect();
    let Some(first) = unpinned.first() else {
        return;
    };
    let (named, them) = if unpinned.len() == 1 {
        (plain(first).into_owned(), "it")
    } else {
        (
            units::count(unpinned.len() as u64, "dataset", "datasets"),
            "them",
        )
    };
    let command = super::project::command("lock");
    context.output.hint(&format!(
        "data.toml names {named} that data.lock does not pin yet, so diff cannot show {them}: `{command}` pins {them}"
    ));
}

/// the commit `revision` names in the repository holding the project at `root`.
fn commit(git: &Path, root: &Path, revision: &str) -> Result<String, Error> {
    git::resolve(git, root, revision).map_err(|unnamed| match unnamed {
        Unnamed::NoRepository => Error::new(
            Kind::Usage,
            format!(
                "this project, {}, is not in a git repository, and diff compares its commits",
                root.display()
            ),
        )
        .help(format!(
            "run it in a project git keeps; `{}` says whether git reads the repository, which it refuses to while another user owns it",
            crate::suggested::git(root, "status")
        )),
        Unnamed::NoCommit => {
            let error = Error::new(
                Kind::Usage,
                format!("\"{}\" names no commit in this repository", plain(revision)),
            );
            if let Some((from, to)) = revision.split_once("..") {
                let (from, to) = (from.trim_start_matches('.'), to.trim_start_matches('.'));
                let from = if from.is_empty() { "HEAD" } else { from };
                let to = if to.is_empty() { "HEAD" } else { to };
                return error.help(format!(
                    "diff takes two commits, not a range: `{}`",
                    super::project::command(&format!(
                        "diff {} {}",
                        shell_word(from),
                        shell_word(to)
                    ))
                ));
            }
            if git::shallow(git, root) {
                return error.help(format!(
                    "this clone is shallow, so it has only the latest commits: `{}` brings in one more, `{}` all of them",
                    crate::suggested::git(root, "fetch --deepen=1"),
                    crate::suggested::git(root, "fetch --unshallow")
                ));
            }
            error.help("name a commit, a branch or a tag, such as HEAD~1 or main; a repository with no commit yet has no HEAD")
        }
    })
}

/// data.lock as `commit`, which `revision` names, has it, and the lists it names as the commit
/// has them, read within the limits of any lock beside one that takes `taken` bytes; an empty
/// lock where the commit has none.
fn at(git: &Path, root: &Path, commit: &str, revision: &str, taken: u64) -> Result<Read, Error> {
    let shown = plain(revision);
    let unread = || {
        Error::new(
            Kind::LocalIo,
            format!("git could not give data.lock and its lists as {shown} has them"),
        )
        .help(format!(
            "a partial clone lacks the files of old commits, and fetchloom never fetches them: check that `{}` works here",
            crate::suggested::git(
                root,
                &format!("show {}", shell_word(&format!("{revision}:./{}", lock::FILE)))
            )
        ))
    };
    let blobs = git::blobs(git, root, commit, &[lock::FILE, lists::FOLDER]).ok_or_else(unread)?;
    let Some(object) = blobs.get(lock::FILE) else {
        return Ok(Read {
            lock: Lock::default(),
            found: false,
            unlisted: Vec::new(),
        });
    };
    let text = git::blob(git, root, object).ok_or_else(unread)?;
    let text = String::from_utf8(text).map_err(|_| {
        Error::new(
            Kind::ProjectInvalid,
            format!("data.lock as {shown} has it is not UTF-8 text"),
        )
    })?;
    let part = |path: &str| -> Result<Vec<u8>, Error> {
        let object = blobs.get(path).ok_or_else(unread)?;
        git::blob(git, root, object).ok_or_else(unread)
    };
    let has = |path: &str| blobs.contains_key(path);
    let (lock, unlisted) = Lock::read_with(&text, &part, &has, taken).map_err(|error| {
        let error = error.detail(format!("in data.lock as {shown} has it"));
        if error.kind != Kind::ProjectInvalid {
            return error;
        }
        // history keeps the commit as it is, so the help for the project's own data.lock is no way on
        error.help("that commit's data.lock stays as it was committed, so compare other commits")
    })?;
    Ok(Read {
        lock,
        found: true,
        unlisted,
    })
}

/// a dataset as one side pins it.
#[derive(Clone, Copy)]
enum Pin<'a> {
    Fetched(&'a Locked),
    /// fetched, but pinned by what its source states until a sync downloads it.
    Stated(&'a Stated),
}

impl<'a> Pin<'a> {
    fn of(lock: &'a Lock, name: &str) -> Option<Pin<'a>> {
        lock.get(name)
            .map(Pin::Fetched)
            .or_else(|| lock.stated(name).map(Pin::Stated))
    }

    /// what data.lock records of it apart from its files, each field by its name there, in the
    /// order data.lock writes them.
    fn fields(self) -> Vec<(&'static str, Option<String>)> {
        let (locked, downloaded) = match self {
            Pin::Fetched(locked) => (locked, true),
            Pin::Stated(stated) => (&stated.pin, false),
        };
        let masked = |text: &str| crate::shown::mask_query(text).into_owned();
        vec![
            ("ref", Some(masked(&locked.reference))),
            ("spec", Some(locked.spec.clone())),
            ("at", locked.at.clone()),
            ("extract", Some(locked.extract.to_string())),
            ("decompress", Some(locked.decompress.to_string())),
            ("web_page", Some(locked.web_page.to_string())),
            ("resolved", Some(masked(&locked.resolved))),
            ("title", locked.title.clone()),
            ("license", locked.license.clone()),
            ("doi", locked.doi.clone()),
            ("access", Some(locked.access.word().to_owned())),
            ("downloaded", Some(downloaded.to_string())),
        ]
    }

    /// each file it pins with its link, sorted by path.
    fn links(self) -> Vec<(&'a str, &'a str)> {
        match self {
            Pin::Fetched(locked) => locked
                .files
                .iter()
                .map(|file| (file.path.as_str(), file.url.as_str()))
                .collect(),
            Pin::Stated(stated) => stated
                .files
                .iter()
                .map(|file| (file.path.as_str(), file.url.as_str()))
                .collect(),
        }
    }

    /// each file it pins, sorted by path.
    fn files(self) -> Vec<(&'a str, Pinned)> {
        match self {
            Pin::Fetched(locked) => locked
                .files
                .iter()
                .map(|file: &'a LockedFile| (file.path.as_str(), Pinned::of(file)))
                .collect(),
            Pin::Stated(stated) => stated
                .files
                .iter()
                .map(|file: &'a StatedFile| {
                    let pinned = Pinned {
                        size: file.size,
                        sha256: file.sha256,
                        md5: file.md5,
                    };
                    (file.path.as_str(), pinned)
                })
                .collect(),
        }
    }
}

/// a dataset removed and another added with the same fields, files and links, which a rename of
/// its table in data.toml makes, as one dataset renamed, `before` and `after` being the locks the
/// two come from. the others stay as they are.
fn renamed(datasets: Vec<Dataset>, before: &Lock, after: &Lock) -> Vec<Dataset> {
    type Pins<'a> = (
        Vec<(&'a str, &'a Option<String>)>,
        Vec<(&'a str, Option<Pinned>)>,
    );
    /// a dataset's fields and files as its one side pins them.
    fn pinned(dataset: &Dataset) -> Pins<'_> {
        let fields = dataset
            .fields
            .iter()
            .map(|field| {
                let value = if field.before.is_some() {
                    &field.before
                } else {
                    &field.after
                };
                (field.name, value)
            })
            .collect();
        let files = dataset
            .files
            .iter()
            .map(|file| (file.path.as_str(), file.before.or(file.after)))
            .collect();
        (fields, files)
    }
    let mut gone: Vec<Dataset> = Vec::new();
    let mut kept: Vec<Dataset> = Vec::new();
    for dataset in datasets {
        match dataset.change {
            Change::Removed if dataset.unlisted.is_empty() => gone.push(dataset),
            _ => kept.push(dataset),
        }
    }
    for dataset in &mut kept {
        if dataset.change != Change::Added || !dataset.unlisted.is_empty() {
            continue;
        }
        let same = pinned(dataset);
        let links = |lock, name| Pin::of(lock, name).map(Pin::links).unwrap_or_default();
        let linked = links(after, &dataset.name);
        let Some(at) = gone
            .iter()
            .position(|old| pinned(old) == same && links(before, &old.name) == linked)
        else {
            continue;
        };
        let old = gone.remove(at);
        *dataset = Dataset {
            name: dataset.name.clone(),
            change: Change::Changed,
            fields: Vec::new(),
            added: 0,
            removed: 0,
            changed: 0,
            relinked: 0,
            files: Vec::new(),
            unlisted: Vec::new(),
            patterns: None,
            renamed_from: Some(old.name),
        };
    }
    kept.extend(gone);
    kept.sort_by(|a, b| a.name.cmp(&b.name));
    kept
}

/// each dataset `after` pins otherwise than `before`, by name.
fn compared(before: &Read, after: &Read) -> Vec<Dataset> {
    let mut names: Vec<&str> = [&before.lock, &after.lock]
        .iter()
        .flat_map(|lock| {
            lock.datasets
                .iter()
                .map(|locked| locked.name.as_str())
                .chain(lock.stated.iter().map(Stated::name))
        })
        .collect();
    names.sort_unstable();
    names.dedup();
    let datasets = names
        .into_iter()
        .filter_map(|name| {
            let unlisted = [("from", before), ("to", after)]
                .into_iter()
                .filter(|(_, side)| side.unlisted.iter().any(|unlisted| unlisted == name))
                .map(|(side, _)| side)
                .collect();
            dataset(
                name,
                Pin::of(&before.lock, name),
                Pin::of(&after.lock, name),
                unlisted,
            )
        })
        .collect();
    renamed(datasets, &before.lock, &after.lock)
}

/// how the pin of `name` differs from `before` to `after`, none when it does not. the files of a
/// dataset whose list a side's commit lacks, as `unlisted` names the sides, are not compared.
fn dataset(
    name: &str,
    before: Option<Pin<'_>>,
    after: Option<Pin<'_>>,
    unlisted: Vec<&'static str>,
) -> Option<Dataset> {
    let change = match (before, after) {
        (None, None) => return None,
        (None, Some(_)) => Change::Added,
        (Some(_), None) => Change::Removed,
        (Some(_), Some(_)) => Change::Changed,
    };
    let fields = fields(before, after);
    let files = if unlisted.is_empty() {
        let changed = files(
            before.map(Pin::files).unwrap_or_default(),
            after.map(Pin::files).unwrap_or_default(),
        );
        match (before, after) {
            (Some(before), Some(after)) => with_links(changed, before, after),
            _ => changed,
        }
    } else {
        Vec::new()
    };
    if change == Change::Changed && fields.is_empty() && files.is_empty() && unlisted.is_empty() {
        return None;
    }
    let count = |change: Change| files.iter().filter(|file| file.change == change).count() as u64;
    Some(Dataset {
        name: name.to_owned(),
        change,
        added: count(Change::Added),
        removed: count(Change::Removed),
        changed: count(Change::Changed),
        relinked: count(Change::Relinked),
        fields,
        files,
        unlisted,
        patterns: None,
        renamed_from: None,
    })
}

/// each field that differs, in words: a version that is its link again is left to the link's
/// line, and a spec, a hash of what data.toml asks, is said as what it stands for: the
/// `patterns` each side asks, when they are known. `arrow` stands between a value and the one
/// that replaced it.
fn fields_said(fields: &[Field], patterns: Option<&Patterns>, arrow: &str) -> Vec<String> {
    let field = |name: &str| fields.iter().find(|field| field.name == name);
    let value = |value: &Option<String>| {
        value
            .as_deref()
            .map_or_else(|| "none".to_owned(), |value| plain(value).into_owned())
    };
    let mut said = Vec::new();
    for changed in fields {
        said.push(match changed.name {
            "spec" if field("ref").is_some() || field("web_page").is_some() => continue,
            "spec" if let Some(patterns) = patterns.filter(|patterns| patterns.differ()) => {
                said.extend(patterns.said(arrow));
                continue;
            }
            "spec" => "select or exclude changed in data.toml".to_owned(),
            // an output never unpacks, so becoming one or no longer says it
            "extract" if field("at").is_some() => continue,
            "at" => match (&changed.before, &changed.after) {
                (None, Some(after)) => format!("now an output, at {}", plain(after)),
                (Some(before), None) => format!("no longer an output, was at {}", plain(before)),
                _ => format!(
                    "at {} {arrow} {}",
                    value(&changed.before),
                    value(&changed.after)
                ),
            },
            "resolved"
                if field("ref").is_some_and(|reference| {
                    reference.before == changed.before && reference.after == changed.after
                }) =>
            {
                continue;
            }
            "resolved" => format!(
                "version {} {arrow} {}",
                value(&changed.before),
                value(&changed.after)
            ),
            "downloaded" if changed.after.as_deref() == Some("true") => "now downloaded".to_owned(),
            "downloaded" => "not downloaded yet".to_owned(),
            name => format!(
                "{name} {} {arrow} {}",
                value(&changed.before),
                value(&changed.after)
            ),
        });
    }
    said
}

/// the `select` and `exclude` patterns data.toml asks of a dataset on each side, as it pinned them.
struct Patterns {
    before: [Vec<String>; 2],
    after: [Vec<String>; 2],
}

impl Patterns {
    /// the patterns `manifest` asks of `name`, when they hash, with its `ref`, to `spec`, the spec
    /// that side's data.lock pins, so a data.toml edited since its lock tells nothing.
    fn asked(
        manifest: Option<&Manifest>,
        name: &str,
        spec: Option<&str>,
    ) -> Option<[Vec<String>; 2]> {
        let source = &manifest?.datasets.get(name)?.source;
        let patterns = |globs: &[crate::glob::Glob]| {
            globs.iter().map(|glob| glob.as_str().to_owned()).collect()
        };
        (Some(lock::spec(source).as_str()) == spec)
            .then(|| [patterns(&source.select), patterns(&source.exclude)])
    }

    fn differ(&self) -> bool {
        self.before != self.after
    }

    /// a line for each key whose patterns differ, as `select ["*.csv"] → ["*.csv", "README"]`,
    /// with `arrow` between the sides.
    fn said(&self, arrow: &str) -> Vec<String> {
        let shown = |patterns: &[String]| {
            if patterns.is_empty() {
                return "none".to_owned();
            }
            let quoted: Vec<String> = patterns
                .iter()
                .map(|pattern| format!("{pattern:?}"))
                .collect();
            format!("[{}]", quoted.join(", "))
        };
        ["select", "exclude"]
            .into_iter()
            .zip(self.before.iter().zip(&self.after))
            .filter(|(_, (before, after))| before != after)
            .map(|(key, (before, after))| {
                format!("{key} {} {arrow} {}", shown(before), shown(after))
            })
            .collect()
    }
}

/// tells, for each of `datasets` whose spec alone changed, the patterns data.toml asks of it on
/// each side, as `before` and `after` read data.toml there, each read once and only when a
/// dataset needs it. a side whose patterns are not what its data.lock pinned leaves the line as
/// it was.
fn name_patterns(
    datasets: &mut [Dataset],
    before: impl Fn() -> Option<Manifest>,
    after: impl Fn() -> Option<Manifest>,
) {
    let (was, now) = (std::cell::OnceCell::new(), std::cell::OnceCell::new());
    for dataset in datasets
        .iter_mut()
        .filter(|dataset| dataset.change == Change::Changed)
    {
        let Some(spec) = dataset.fields.iter().find(|field| field.name == "spec") else {
            continue;
        };
        let asked = |manifest: &std::cell::OnceCell<Option<Manifest>>,
                     read: &dyn Fn() -> Option<Manifest>,
                     spec: &Option<String>| {
            Patterns::asked(
                manifest.get_or_init(read).as_ref(),
                &dataset.name,
                spec.as_deref(),
            )
        };
        let Some(before) = asked(&was, &before, &spec.before) else {
            continue;
        };
        let Some(after) = asked(&now, &after, &spec.after) else {
            continue;
        };
        dataset.patterns = Some(Patterns { before, after });
    }
}

/// data.toml as `commit` has it in the project at `root`, none when it has none or it does not
/// read.
fn manifest_at(git: &Path, root: &Path, commit: &str) -> Option<Manifest> {
    let blobs = git::blobs(git, root, commit, &[manifest::FILE])?;
    let text = git::blob(git, root, blobs.get(manifest::FILE)?)?;
    Manifest::parse(std::str::from_utf8(&text).ok()?).ok()
}

/// the fields that differ: every field of a dataset added or removed, else each field with another
/// value.
fn fields(before: Option<Pin<'_>>, after: Option<Pin<'_>>) -> Vec<Field> {
    let listed = |pin: Option<Pin<'_>>| pin.map(Pin::fields).unwrap_or_default();
    let (was, now) = (listed(before), listed(after));
    let value = |fields: &[(&'static str, Option<String>)], name: &str| {
        fields
            .iter()
            .find(|(field, _)| *field == name)
            .and_then(|(_, value)| value.clone())
    };
    let every = if was.is_empty() { &now } else { &was };
    every
        .iter()
        .map(|&(name, _)| Field {
            name,
            before: value(&was, name),
            after: value(&now, name),
        })
        .filter(|field| field.before != field.after)
        .collect()
}

/// the files added, removed and changed from `before` to `after`, each sorted by path, as data.lock
/// keeps them: a file changes when its bytes do, as [`Pinned::same`] tells. [`with_links`] adds
/// what a link changed for diff.
pub(in crate::cli) fn files(before: Vec<(&str, Pinned)>, after: Vec<(&str, Pinned)>) -> Vec<File> {
    let mut found = Vec::new();
    let mut was = before.into_iter().peekable();
    let mut now = after.into_iter().peekable();
    loop {
        let order = match (was.peek(), now.peek()) {
            (None, None) => break,
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (Some((before, _)), Some((after, _))) => before.cmp(after),
        };
        let (path, change, before, after) = match order {
            std::cmp::Ordering::Less => {
                let Some((path, pin)) = was.next() else { break };
                (path, Change::Removed, Some(pin), None)
            }
            std::cmp::Ordering::Greater => {
                let Some((path, pin)) = now.next() else { break };
                (path, Change::Added, None, Some(pin))
            }
            std::cmp::Ordering::Equal => {
                let (Some((path, before)), Some((_, after))) = (was.next(), now.next()) else {
                    break;
                };
                if before.same(after) {
                    continue;
                }
                (path, Change::Changed, Some(before), Some(after))
            }
        };
        found.push(File {
            path: path.to_owned(),
            change,
            before,
            after,
            link: None,
        });
    }
    found
}

/// `changed`, the files `before` and `after` pin otherwise, each with its link before and after
/// where the two pin it at other links, and with each file pinned with the same bytes at another
/// link, relinked, all sorted by path: a pull request may move a file's link alone, and a clone
/// then asks that host first.
fn with_links(mut changed: Vec<File>, before: Pin<'_>, after: Pin<'_>) -> Vec<File> {
    let masked = |link: &str| crate::shown::mask_query(link).into_owned();
    let now: std::collections::HashMap<&str, &str> = after.links().into_iter().collect();
    let pins: std::collections::HashMap<&str, (Pinned, Pinned)> = {
        let was: std::collections::HashMap<&str, Pinned> = before.files().into_iter().collect();
        after
            .files()
            .into_iter()
            .filter_map(|(path, pin)| Some((path, (*was.get(path)?, pin))))
            .collect()
    };
    let mut relinked = Vec::new();
    for (path, was) in before.links() {
        let Some(&is) = now.get(path).filter(|is| **is != was) else {
            continue;
        };
        let link = Some(Link {
            before: masked(was),
            after: masked(is),
        });
        if let Ok(at) = changed.binary_search_by(|file| file.path.as_str().cmp(path)) {
            changed[at].link = link;
        } else if let Some(&(before, after)) = pins.get(path) {
            relinked.push(File {
                path: path.to_owned(),
                change: Change::Relinked,
                before: Some(before),
                after: Some(after),
                link,
            });
        }
    }
    changed.extend(relinked);
    changed.sort_by(|a, b| a.path.cmp(&b.path));
    changed
}

/// the most file lines a terminal shows of one dataset; a pipe, or `-v`, has each.
const SHOWN: usize = 20;

/// tells each dataset that differs on stdout: a line of what changed in it, then, unless `-q`
/// asked for those lines alone, a line for each file as `git diff --name-status` gives one, at a
/// terminal at most [`SHOWN`] of them, with `again`, the command that lists every one.
fn tell(context: &Context, datasets: &[Dataset], (from, to): (&str, &str), again: &str) {
    let output = &context.output;
    let most = output
        .answer_width()
        .filter(|_| !output.shows_details())
        .map_or(usize::MAX, |_| SHOWN);
    for dataset in datasets {
        let mut text = summary(dataset, from, to, output.arrow());
        if output.shows_items() {
            text.push_str(&file_lines(dataset, most, again, output.arrow()));
        }
        output.answer(&text);
    }
    if datasets.is_empty() {
        output.done(
            "Ok",
            &format!("nothing differs in data.lock between {from} and {to}"),
        );
    } else {
        let verb = if datasets.len() == 1 {
            "differs"
        } else {
            "differ"
        };
        output.done(
            "Ok",
            &format!(
                "{} {verb} between {from} and {to}",
                units::count(datasets.len() as u64, "dataset", "datasets")
            ),
        );
    }
}

/// a line for each of `dataset`'s files, at most `most` of them, then one counting the rest that
/// names `again`, which lists them all.
fn file_lines(dataset: &Dataset, most: usize, again: &str, arrow: &str) -> String {
    let mut text = String::new();
    for file in dataset.files.iter().take(most) {
        let _ = write!(text, "\n{}", line(&dataset.name, file, arrow));
    }
    let hidden = dataset.files.len().saturating_sub(most);
    if hidden > 0 {
        let _ = write!(
            text,
            "\nand {} more \u{b7} `{again}` names each",
            units::grouped(hidden as u64)
        );
    }
    text
}

/// the line that says what changed in `dataset`: its fields, each value then `arrow` then the one
/// that replaced it, then how many files changed, or which of the sides, `from` and `to` as shown,
/// lack its file list.
fn summary(dataset: &Dataset, from: &str, to: &str, arrow: &str) -> String {
    let name = plain(&dataset.name);
    let files = match dataset.unlisted.as_slice() {
        [] => files_counted(dataset),
        [side] => format!(
            "its file list is not in {}, so its files were not compared",
            if *side == "from" { from } else { to }
        ),
        _ => format!("its file list is in neither {from} nor {to}, so its files were not compared"),
    };
    match dataset.change {
        Change::Added | Change::Removed => {
            let shown = |name: &str| {
                dataset
                    .fields
                    .iter()
                    .find(|field| field.name == name)
                    .and_then(|field| field.after.as_ref().or(field.before.as_ref()))
                    .map(|value| plain(value).into_owned())
            };
            let what = match shown("downloaded").as_deref() {
                Some("false") => format!(
                    "{}, not downloaded yet",
                    shown("resolved").unwrap_or_default()
                ),
                _ => shown("resolved").unwrap_or_default(),
            };
            let change = if dataset.change == Change::Added {
                "added"
            } else {
                "removed"
            };
            format!("{name} {change}: {what}; {files}")
        }
        Change::Changed if let Some(before) = &dataset.renamed_from => {
            format!("{} renamed to {name}; the same files", plain(before))
        }
        Change::Changed | Change::Relinked => {
            let mut said = fields_said(&dataset.fields, dataset.patterns.as_ref(), arrow);
            said.push(files);
            format!("{name} changed: {}", said.join("; "))
        }
    }
}

/// how many files were added, removed and changed, or that the files are the same.
fn files_counted(dataset: &Dataset) -> String {
    let parts: Vec<(u64, &str)> = [
        (dataset.added, "added"),
        (dataset.removed, "removed"),
        (dataset.changed, "changed"),
        (
            dataset.relinked,
            if dataset.relinked == 1 {
                "at another link"
            } else {
                "at other links"
            },
        ),
    ]
    .into_iter()
    .filter(|(count, _)| *count > 0)
    .collect();
    let Some(((first, how), rest)) = parts.split_first() else {
        return "the same files".to_owned();
    };
    let mut text = format!("{} {how}", units::count(*first, "file", "files"));
    for (count, how) in rest {
        let _ = write!(text, ", {} {how}", units::grouped(*count));
    }
    let bytes = |pin: fn(&File) -> Option<Pinned>| {
        dataset
            .files
            .iter()
            .filter_map(pin)
            .map(|pinned| pinned.size)
            .fold(0, u64::saturating_add)
    };
    match dataset.change {
        Change::Added => format!("{text}, {}", units::size(bytes(|file| file.after))),
        Change::Removed => format!("{text}, {}", units::size(bytes(|file| file.before))),
        Change::Changed | Change::Relinked => text,
    }
}

/// a file's line: A, D or M, its path below the dataset's name, and its size, or for one that
/// changed its size before and after, `arrow` between them.
fn line(dataset: &str, file: &File, arrow: &str) -> String {
    let path = plain(&file.path);
    let dataset = plain(dataset);
    let moved = file.link.as_ref().and_then(|link| link.moved(arrow));
    let link = moved
        .as_ref()
        .map(|moved| format!(", {moved}"))
        .unwrap_or_default();
    match (file.change, file.before, file.after) {
        (Change::Relinked, _, Some(after)) => {
            let at = match (
                &moved,
                file.link.as_ref().and_then(|link| host(&link.after)),
            ) {
                (Some(moved), _) => format!(", {moved}"),
                (None, Some(host)) => format!(" at another link on {host}"),
                (None, None) => " at another link".to_owned(),
            };
            format!(
                "M  {dataset}/{path}  {}, the same bytes{at}",
                units::size(after.size)
            )
        }
        (Change::Changed, Some(before), Some(after)) if before.size == after.size => format!(
            "M  {dataset}/{path}  {}, {}{link}",
            units::size(after.size),
            if before.comparable(after) {
                "other bytes"
            } else {
                "pinned by another checksum"
            }
        ),
        (Change::Changed, Some(before), Some(after)) => format!(
            "M  {dataset}/{path}  {} {arrow} {}{link}",
            units::size(before.size),
            units::size(after.size)
        ),
        (Change::Added, _, Some(after)) => {
            format!("A  {dataset}/{path}  {}", units::size(after.size))
        }
        (_, Some(before), _) => format!("D  {dataset}/{path}  {}", units::size(before.size)),
        _ => format!("?  {dataset}/{path}"),
    }
}

#[cfg(test)]
mod tests;
