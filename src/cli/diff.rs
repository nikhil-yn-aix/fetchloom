//! `fetchloom diff`: what changed in data.lock, its file lists included, between two git commits,
//! or a commit and the project folder as it is now, since git shows the lists in data.lock.d only
//! as compressed bytes. it reads git's objects alone and changes nothing.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;

use super::{Context, args, shell_word};
use crate::cache::Member;
use crate::digest::{Md5, Sha256};
use crate::error::{Error, Kind};
use crate::output::plain;
use crate::project::git::{self, Unnamed};
use crate::project::lock::derived::Derived;
use crate::project::lock::{self, Lock, Locked, LockedFile, Stated, StatedFile, lists};
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
    /// `fetched` or `recipe`, as `to` pins it, else as `from` did.
    kind: &'static str,
    /// each field of the pin that differs, apart from its files: all of them for a dataset
    /// added or removed.
    fields: Vec<Field>,
    added: u64,
    removed: u64,
    changed: u64,
    /// each file added, removed or changed, by its path in the dataset, sorted.
    files: Vec<File>,
    /// the sides, `from` or `to`, whose commit lacks the dataset's file list, so its files were
    /// not compared.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    unlisted: Vec<&'static str>,
    /// what changed, in words, for the line that tells it.
    #[serde(skip)]
    said: Vec<String>,
    /// the name `from` gave the dataset, when only its name changed, as a rename in data.toml
    /// moves its pin.
    #[serde(skip_serializing_if = "Option::is_none")]
    renamed_from: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Change {
    Added,
    Removed,
    Changed,
}

/// a field of a pin, as data.lock writes it, before and after; none on a side without it.
#[derive(Debug, PartialEq, Eq, Serialize)]
struct Field {
    name: &'static str,
    before: Option<String>,
    after: Option<String>,
}

/// a file whose pin differs: its size and digests before and after, none on a side without it.
#[derive(Debug, PartialEq, Eq, Serialize)]
struct File {
    path: String,
    change: Change,
    before: Option<Pinned>,
    after: Option<Pinned>,
}

/// a file's pin: its size and SHA-256, or for a dataset not downloaded yet, the SHA-256 or MD5
/// its source states.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
struct Pinned {
    size: u64,
    sha256: Option<Sha256>,
    #[serde(skip_serializing_if = "Option::is_none")]
    md5: Option<Md5>,
}

impl Pinned {
    /// whether `other` pins the same bytes: the same size, and the same SHA-256 where both have
    /// one, else the same MD5 where both have one, as a sync checks a stated pin.
    fn same(self, other: Pinned) -> bool {
        self.size == other.size
            && match (self.sha256, other.sha256, self.md5, other.md5) {
                (Some(this), Some(that), _, _) => this == that,
                (_, _, Some(this), Some(that)) => this == that,
                _ => true,
            }
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
    let project = super::project::find(&context.output)?;
    let root = &project.root;
    let git = std::env::var_os("PATH")
        .and_then(|path| git::find(Some(&path), root))
        .ok_or_else(|| {
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
    let datasets = compared(&before, &after);
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
    let pinned = |name: &str| {
        now.get(name).is_some() || now.stated(name).is_some() || now.result(name).is_some()
    };
    // a source's pin comes from lock, a recipe's from build
    for (made, command) in [(false, "fetchloom lock"), (true, "fetchloom build")] {
        let unpinned: Vec<&str> = manifest
            .datasets
            .iter()
            .filter(|(name, entry)| {
                matches!(entry.origin, crate::project::manifest::Origin::Derived(_)) == made
                    && !pinned(name)
            })
            .map(|(name, _)| name.as_str())
            .collect();
        let Some(first) = unpinned.first() else {
            continue;
        };
        let (named, them) = if unpinned.len() == 1 {
            (plain(first).into_owned(), "it")
        } else {
            (
                units::count(unpinned.len() as u64, "dataset", "datasets"),
                "them",
            )
        };
        context.output.hint(&format!(
            "data.toml names {named} that data.lock does not pin yet, so diff cannot show {them}: `{command}` pins {them}"
        ));
    }
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
    Made(&'a Derived),
}

impl<'a> Pin<'a> {
    fn of(lock: &'a Lock, name: &str) -> Option<Pin<'a>> {
        lock.get(name)
            .map(Pin::Fetched)
            .or_else(|| lock.stated(name).map(Pin::Stated))
            .or_else(|| lock.result(name).map(Pin::Made))
    }

    fn kind(self) -> &'static str {
        match self {
            Pin::Fetched(_) | Pin::Stated(_) => "fetched",
            Pin::Made(_) => "recipe",
        }
    }

    /// what data.lock records of it apart from its files, each field by its name there, in the
    /// order data.lock writes them.
    fn fields(self) -> Vec<(&'static str, Option<String>)> {
        let (locked, downloaded) = match self {
            Pin::Fetched(locked) => (locked, true),
            Pin::Stated(stated) => (&stated.pin, false),
            Pin::Made(derived) => return recipe(derived),
        };
        let masked = |text: &str| crate::shown::mask_query(text).into_owned();
        vec![
            ("ref", Some(masked(&locked.reference))),
            ("spec", Some(locked.spec.clone())),
            ("extract", Some(locked.extract.to_string())),
            ("web_page", Some(locked.web_page.to_string())),
            ("resolved", Some(masked(&locked.resolved))),
            ("title", locked.title.clone()),
            ("license", locked.license.clone()),
            ("doi", locked.doi.clone()),
            ("access", Some(locked.access.word().to_owned())),
            ("downloaded", Some(downloaded.to_string())),
        ]
    }

    /// each file it pins, sorted by path.
    fn files(self) -> Vec<(&'a str, Pinned)> {
        match self {
            Pin::Fetched(locked) => locked
                .files
                .iter()
                .map(|file: &'a LockedFile| {
                    let pinned = Pinned {
                        size: file.size,
                        sha256: Some(file.sha256),
                        md5: file.md5,
                    };
                    (file.path.as_str(), pinned)
                })
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
            Pin::Made(derived) => derived
                .files
                .iter()
                .map(|file: &'a Member| {
                    let pinned = Pinned {
                        size: file.size,
                        sha256: Some(file.sha256),
                        md5: None,
                    };
                    (file.path.as_str(), pinned)
                })
                .collect(),
        }
    }
}

/// what data.lock records of a recipe's result apart from its files, as [`Pin::fields`] gives it.
fn recipe(derived: &Derived) -> Vec<(&'static str, Option<String>)> {
    let from: Vec<String> = derived
        .from
        .iter()
        .map(|input| format!("{} sha256:{}", input.name, input.content))
        .collect();
    let code: Vec<String> = derived
        .code
        .iter()
        .map(|code| format!("{} sha256:{}", code.path, code.sha256))
        .collect();
    let env: Vec<String> = derived
        .env
        .iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect();
    vec![
        ("from", Some(from.join(", "))),
        ("run", Some(derived.run.join(" "))),
        ("code", Some(code.join(", "))),
        ("env", Some(env.join(" "))),
        ("key", Some(format!("sha256:{}", derived.key))),
    ]
}

/// a dataset removed and another added with the same fields and files, which a rename of its
/// table in data.toml makes, as one dataset renamed. the others stay as they are.
fn renamed(datasets: Vec<Dataset>) -> Vec<Dataset> {
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
        let Some(at) = gone
            .iter()
            .position(|old| old.kind == dataset.kind && pinned(old) == same)
        else {
            continue;
        };
        let old = gone.remove(at);
        *dataset = Dataset {
            name: dataset.name.clone(),
            change: Change::Changed,
            kind: dataset.kind,
            fields: Vec::new(),
            added: 0,
            removed: 0,
            changed: 0,
            files: Vec::new(),
            unlisted: Vec::new(),
            said: Vec::new(),
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
                .chain(lock.derived.iter().map(|derived| derived.name.as_str()))
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
    renamed(datasets)
}

/// how the pin of `name` differs from `before` to `after`, none when it does not. the files of a
/// dataset whose list a side's commit lacks, as `unlisted` names the sides, are not compared.
fn dataset(
    name: &str,
    before: Option<Pin<'_>>,
    after: Option<Pin<'_>>,
    unlisted: Vec<&'static str>,
) -> Option<Dataset> {
    let (change, kind) = match (before, after) {
        (None, None) => return None,
        (None, Some(after)) => (Change::Added, after.kind()),
        (Some(before), None) => (Change::Removed, before.kind()),
        (Some(_), Some(after)) => (Change::Changed, after.kind()),
    };
    let fields = fields(before, after);
    let files = if unlisted.is_empty() {
        files(
            before.map(Pin::files).unwrap_or_default(),
            after.map(Pin::files).unwrap_or_default(),
        )
    } else {
        Vec::new()
    };
    if change == Change::Changed && fields.is_empty() && files.is_empty() && unlisted.is_empty() {
        return None;
    }
    let said = match (before, after) {
        (Some(Pin::Made(was)), Some(Pin::Made(now))) => recipe_said(was, now),
        _ => fields_said(&fields),
    };
    let count = |change: Change| files.iter().filter(|file| file.change == change).count() as u64;
    Some(Dataset {
        name: name.to_owned(),
        change,
        kind,
        added: count(Change::Added),
        removed: count(Change::Removed),
        changed: count(Change::Changed),
        fields,
        files,
        unlisted,
        said,
        renamed_from: None,
    })
}

/// each field that differs, in words: a version that is its link again is left to the link's
/// line, and a spec, a hash of what data.toml asks, is said as what it stands for.
fn fields_said(fields: &[Field]) -> Vec<String> {
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
            "spec" => "select or exclude changed in data.toml".to_owned(),
            "resolved"
                if field("ref").is_some_and(|reference| {
                    reference.before == changed.before && reference.after == changed.after
                }) =>
            {
                continue;
            }
            "resolved" => format!(
                "version {} \u{2192} {}",
                value(&changed.before),
                value(&changed.after)
            ),
            "downloaded" if changed.after.as_deref() == Some("true") => "now downloaded".to_owned(),
            "downloaded" => "not downloaded yet".to_owned(),
            name => format!(
                "{name} {} \u{2192} {}",
                value(&changed.before),
                value(&changed.after)
            ),
        });
    }
    said
}

/// what changed in how a recipe made its result, in words: the inputs it names, or each input
/// whose bytes changed, its command, each file of its code and each variable, with its values.
fn recipe_said(was: &Derived, now: &Derived) -> Vec<String> {
    let mut said = Vec::new();
    let names = |derived: &Derived| -> Vec<String> {
        derived
            .from
            .iter()
            .map(|input| plain(&input.name).into_owned())
            .collect()
    };
    let listed = |names: Vec<String>| {
        if names.is_empty() {
            "none".to_owned()
        } else {
            names.join(", ")
        }
    };
    if names(was) != names(now) {
        said.push(format!(
            "from {} \u{2192} {}",
            listed(names(was)),
            listed(names(now))
        ));
    }
    for input in &now.from {
        if was
            .from
            .iter()
            .any(|old| old.name == input.name && old.content != input.content)
        {
            said.push(format!("{} changed", plain(&input.name)));
        }
    }
    if was.run != now.run {
        said.push("run changed".to_owned());
    }
    for code in &now.code {
        match was.code.iter().find(|old| old.path == code.path) {
            Some(old) if old.sha256 != code.sha256 => {
                said.push(format!("{} changed", plain(&code.path)));
            }
            None => said.push(format!("{} added to code", plain(&code.path))),
            Some(_) => {}
        }
    }
    for old in &was.code {
        if !now.code.iter().any(|code| code.path == old.path) {
            said.push(format!("{} taken out of code", plain(&old.path)));
        }
    }
    let variables: std::collections::BTreeSet<&String> =
        was.env.keys().chain(now.env.keys()).collect();
    for name in variables {
        let name_shown = plain(name);
        // a value may hold a key, so only the name of what changed shows
        match (was.env.get(name), now.env.get(name)) {
            (Some(before), Some(after)) if before != after => {
                said.push(format!("env {name_shown} changed"));
            }
            (None, Some(_)) => said.push(format!("env {name_shown} added")),
            (Some(_), None) => said.push(format!("env {name_shown} taken out")),
            _ => {}
        }
    }
    if said.is_empty() && was.key != now.key {
        said.push("how it is built changed".to_owned());
    }
    said
}

/// the fields that differ: every field of a dataset added or removed, the kind alone of one now
/// fetched where a recipe made it or the other way round, else each field with another value.
fn fields(before: Option<Pin<'_>>, after: Option<Pin<'_>>) -> Vec<Field> {
    let listed = |pin: Option<Pin<'_>>| pin.map(Pin::fields).unwrap_or_default();
    if let (Some(was), Some(now)) = (before, after)
        && was.kind() != now.kind()
    {
        return vec![Field {
            name: "kind",
            before: Some(was.kind().to_owned()),
            after: Some(now.kind().to_owned()),
        }];
    }
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
        .map(|field| match field.name {
            // compared whole, then shown by name alone, as merge and data.toml's errors show them
            "env" => Field {
                before: before.and_then(variables),
                after: after.and_then(variables),
                ..field
            },
            _ => field,
        })
        .collect()
}

/// a recipe's variables by their names alone, as `SEED=***`, since a value may hold a key.
fn variables(pin: Pin<'_>) -> Option<String> {
    let Pin::Made(derived) = pin else {
        return None;
    };
    let named: Vec<String> = derived
        .env
        .keys()
        .map(|name| format!("{name}=***"))
        .collect();
    Some(named.join(" "))
}

/// the files added, removed and changed from `before` to `after`, each sorted by path, as data.lock
/// keeps them: a file changes when its bytes do, as [`Pinned::same`] tells, not when its link does.
fn files(before: Vec<(&str, Pinned)>, after: Vec<(&str, Pinned)>) -> Vec<File> {
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
        });
    }
    found
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
        let mut text = summary(dataset, from, to);
        if output.shows_items() {
            text.push_str(&file_lines(dataset, most, again));
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
fn file_lines(dataset: &Dataset, most: usize, again: &str) -> String {
    let mut text = String::new();
    for file in dataset.files.iter().take(most) {
        let _ = write!(text, "\n{}", line(&dataset.name, file));
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

/// the line that says what changed in `dataset`: its fields, then how many files changed, or
/// which of the sides, `from` and `to` as shown, lack its file list.
fn summary(dataset: &Dataset, from: &str, to: &str) -> String {
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
            let what = match (dataset.kind, shown("downloaded").as_deref()) {
                ("recipe", _) => "made by a recipe".to_owned(),
                (_, Some("false")) => format!(
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
        Change::Changed => {
            let mut said = dataset.said.clone();
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
        Change::Changed => text,
    }
}

/// a file's line: A, D or M, its path below the dataset's name, and its size, or for one that
/// changed its size before and after.
fn line(dataset: &str, file: &File) -> String {
    let path = plain(&file.path);
    let dataset = plain(dataset);
    match (file.change, file.before, file.after) {
        (Change::Changed, Some(before), Some(after)) if before.size == after.size => format!(
            "M  {dataset}/{path}  {}, other bytes",
            units::size(after.size)
        ),
        (Change::Changed, Some(before), Some(after)) => format!(
            "M  {dataset}/{path}  {} \u{2192} {}",
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
