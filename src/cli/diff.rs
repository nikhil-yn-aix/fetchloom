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
/// project folder as it is now.
#[derive(Serialize)]
struct Side {
    revision: Option<String>,
    commit: Option<String>,
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
    let before = at(&git, root, &from_commit, from)?;
    let (to_commit, after) = match args.to.as_deref() {
        Some(to) => {
            let named = commit(&git, root, to)?;
            let lock = at(&git, root, &named, to)?;
            (Some(named), lock)
        }
        None => (None, project.lock()?.unwrap_or_default()),
    };
    let datasets = compared(&before, &after);
    let to_shown = args.to.as_deref().map_or_else(
        || "the project folder".to_owned(),
        |to| plain(to).into_owned(),
    );
    tell(context, &datasets, &plain(from), &to_shown);
    Ok(Report {
        project: root.clone(),
        from: Side {
            revision: Some(from.to_owned()),
            commit: Some(from_commit),
        },
        to: Side {
            revision: args.to.clone(),
            commit: to_commit,
        },
        datasets,
    })
}

/// the commit `revision` names in the repository holding the project at `root`.
fn commit(git: &Path, root: &Path, revision: &str) -> Result<String, Error> {
    git::resolve(git, root, revision).map_err(|unnamed| match unnamed {
        Unnamed::NoRepository => Error::new(
            Kind::Usage,
            format!(
                "{} is in no git repository git reads, so it has no commits to compare",
                root.display()
            ),
        )
        .help("run fetchloom diff in the project's git repository. git also refuses one another user owns until you trust it: `git status` there says so"),
        Unnamed::NoCommit => Error::new(
            Kind::Usage,
            format!("{} names no commit in this repository", plain(revision)),
        )
        .help("name a commit, a branch or a tag, such as HEAD~1 or main; a repository with no commit yet has no HEAD"),
    })
}

/// data.lock as `commit`, which `revision` names, has it, and the lists it names as the commit
/// has them, read within the limits of any lock; an empty lock where the commit has none.
fn at(git: &Path, root: &Path, commit: &str, revision: &str) -> Result<Lock, Error> {
    let shown = plain(revision);
    let unread = || {
        Error::new(
            Kind::LocalIo,
            format!("git could not give data.lock and its lists as {shown} has them"),
        )
        .help(format!(
            "a partial clone lacks the files of old commits, and fetchloom never fetches them: check that `git show {}` works here",
            shell_word(&format!("{revision}:./{}", lock::FILE))
        ))
    };
    let blobs = git::blobs(git, root, commit, &[lock::FILE, lists::FOLDER]).ok_or_else(unread)?;
    let Some(object) = blobs.get(lock::FILE) else {
        return Ok(Lock::default());
    };
    let text = git::blob(git, root, object).ok_or_else(unread)?;
    let text = String::from_utf8(text).map_err(|_| {
        Error::new(
            Kind::ProjectInvalid,
            format!("data.lock as {shown} has it is not UTF-8 text"),
        )
    })?;
    let part = |path: &str| -> Result<Vec<u8>, Error> {
        let object = blobs.get(path).ok_or_else(|| {
            Error::new(
                Kind::ProjectInvalid,
                format!(
                    "data.lock as {shown} has it names {}, which that commit does not have",
                    plain(path)
                ),
            )
            .help(format!(
                "{} was probably not committed with data.lock there; compare other commits",
                lists::FOLDER
            ))
        })?;
        git::blob(git, root, object).ok_or_else(unread)
    };
    Lock::read_with(&text, &part)
        .map_err(|error| error.detail(format!("in data.lock as {shown} has it")))
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

/// each dataset `after` pins otherwise than `before`, by name.
fn compared(before: &Lock, after: &Lock) -> Vec<Dataset> {
    let mut names: Vec<&str> = [before, after]
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
    names
        .into_iter()
        .filter_map(|name| dataset(name, Pin::of(before, name), Pin::of(after, name)))
        .collect()
}

/// how the pin of `name` differs from `before` to `after`, none when it does not.
fn dataset(name: &str, before: Option<Pin<'_>>, after: Option<Pin<'_>>) -> Option<Dataset> {
    let (change, kind) = match (before, after) {
        (None, None) => return None,
        (None, Some(after)) => (Change::Added, after.kind()),
        (Some(before), None) => (Change::Removed, before.kind()),
        (Some(_), Some(after)) => (Change::Changed, after.kind()),
    };
    let fields = fields(before, after);
    let files = files(
        before.map(Pin::files).unwrap_or_default(),
        after.map(Pin::files).unwrap_or_default(),
    );
    if change == Change::Changed && fields.is_empty() && files.is_empty() {
        return None;
    }
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
    })
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
        .collect()
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

/// tells each dataset that differs on stdout: a line of what changed in it, then, unless `-q`
/// asked for those lines alone, a line for each file as `git diff --name-status` gives one.
fn tell(context: &Context, datasets: &[Dataset], from: &str, to: &str) {
    let output = &context.output;
    for dataset in datasets {
        let mut text = summary(dataset);
        if output.shows_items() {
            for file in &dataset.files {
                let _ = write!(text, "\n{}", line(&dataset.name, file));
            }
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

/// the line that says what changed in `dataset`: its fields, then how many files changed.
fn summary(dataset: &Dataset) -> String {
    let name = plain(&dataset.name);
    let files = files_counted(dataset);
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
        Change::Changed => {
            let mut said: Vec<String> = Vec::new();
            let named = |name: &str| dataset.fields.iter().any(|field| field.name == name);
            for field in &dataset.fields {
                let value = |value: &Option<String>| {
                    value
                        .as_deref()
                        .map_or_else(|| "none".to_owned(), |value| plain(value).into_owned())
                };
                said.push(match field.name {
                    // a spec is a hash of ref, select, exclude and web_page in data.toml
                    "spec" if named("ref") || named("web_page") => continue,
                    "spec" => "select or exclude changed in data.toml".to_owned(),
                    "resolved" => format!(
                        "version {} \u{2192} {}",
                        value(&field.before),
                        value(&field.after)
                    ),
                    "key" => format!(
                        "key {} \u{2192} {}",
                        short(field.before.as_deref()),
                        short(field.after.as_deref())
                    ),
                    "from" | "run" | "code" | "env" => format!("{} changed", field.name),
                    "downloaded" if field.after.as_deref() == Some("true") => {
                        "now downloaded".to_owned()
                    }
                    "downloaded" => "not downloaded yet".to_owned(),
                    name => format!(
                        "{name} {} \u{2192} {}",
                        value(&field.before),
                        value(&field.after)
                    ),
                });
            }
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

/// a key as `sha256:` and its first 12 hex digits, enough to tell two apart in a line.
fn short(key: Option<&str>) -> String {
    key.map_or_else(
        || "none".to_owned(),
        |key| key.chars().take("sha256:".len() + 12).collect(),
    )
}

#[cfg(test)]
mod tests;
