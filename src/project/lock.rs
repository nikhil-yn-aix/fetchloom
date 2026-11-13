//! `data.lock`: what each dataset resolved to and the files it pins, or what a recipe made it
//! from. only fetchloom writes it, in a fixed order, so the same inputs give the same bytes.

pub(crate) mod archives;
mod canonical;
pub(crate) mod derived;
pub(crate) mod lists;
pub(crate) mod merge;
#[cfg(test)]
mod model_tests;

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::de::Error as _;
use serde::{Deserialize, Deserializer};
use url::Url;

use self::derived::Derived;
use super::manifest::{Entry, Source};
use crate::digest::{self, Md5, Sha256};
use crate::error::{Error, Kind};
use crate::glob::Glob;
use crate::name;
use crate::source::Access;

pub(crate) const FILE: &str = "data.lock";
/// the one format of data.lock, a lock that keeps lists in [`lists::FOLDER`] included.
const VERSION: u32 = 1;
/// a lower bound on the memory a read takes per pinned file: a status or settled sync peaked at
/// 76.7 MiB on glibc and 92.5 MiB on musl for 100,000 files, and 690 and 668 MiB for 1,000,001.
const PER_FILE: u64 = 650;
/// what a run holds before it reads a project, which the musl build's allocator starts larger.
const BASE: u64 = if cfg!(target_env = "musl") {
    20 << 20
} else {
    8 << 20
};

/// the memory a container's limit leaves this run, past which the kernel ends it with no word.
/// what a system without such a limit has available is no such wall: it can page out instead.
static ROOM: AtomicU64 = AtomicU64::new(u64::MAX);

/// sets [`ROOM`] for every later read of a lock; none means no container limit.
pub(crate) fn set_room(room: Option<u64>) {
    ROOM.store(room.unwrap_or(u64::MAX), Ordering::Relaxed);
}

/// refuses, before any list is read, a lock whose files take more memory than `room`, so a run
/// too big for its container says so instead of being killed partway.
fn fits(parsed: &[Parsed], room: u64) -> Result<(), Error> {
    let files: u64 = parsed.iter().map(Parsed::files).sum();
    let need = BASE.saturating_add(files.saturating_mul(PER_FILE));
    if need <= room {
        return Ok(());
    }
    Err(Error::new(
        Kind::LocalIo,
        format!(
            "reading this project's {} takes about {} of memory, more than the {} this container leaves fetchloom",
            crate::units::count(files, "file", "files"),
            crate::units::size(need),
            crate::units::size(room)
        ),
    )
    .help(format!(
        "raise the container's memory limit by {} or more, or name one dataset, as in `fetchloom path <name>`, which reads only its files",
        crate::units::size(need - room)
    )))
}

/// how to recover a data.lock this fetchloom cannot use. `fetchloom lock` alone reads the same
/// file first and fails the same way.
pub(crate) const BROKEN: &str = "restore data.lock from git, or delete it and run `fetchloom lock`, which resolves every dataset again and may pin newer data";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Lock {
    /// the datasets fetched from a source, sorted by name.
    pub(crate) datasets: Vec<Locked>,
    /// the datasets a recipe made, sorted by name. no name is in both lists.
    pub(crate) derived: Vec<Derived>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Locked {
    pub(crate) name: String,
    pub(crate) reference: String,
    /// the hash of the data.toml keys that decide what is downloaded. while it matches, the
    /// dataset is not resolved again.
    pub(crate) spec: String,
    /// whether archives unpack, as data.toml asked when the dataset was pinned. it is compared
    /// apart from `spec`, since a switch keeps the files and changes only what they unpack to.
    pub(crate) extract: bool,
    pub(crate) resolved: String,
    pub(crate) title: Option<String>,
    pub(crate) license: Option<String>,
    pub(crate) doi: Option<String>,
    /// who may read the data, as its source said when it was pinned: a hint for sharing it.
    pub(crate) access: Access,
    /// when the pinned data last changed, in UTC.
    pub(crate) timestamp: String,
    /// sorted by path.
    pub(crate) files: Vec<LockedFile>,
}

/// what changed in a data.toml entry since its dataset was pinned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Drift {
    /// only `extract`: the pinned files stand, and only what their archives unpack to changes.
    Extract,
    /// what is downloaded, so the dataset is resolved again.
    Spec,
}

impl Locked {
    /// what changed in `entry` since this pin was made, or nothing when it still answers it. a
    /// dataset a recipe now makes is no longer the one fetched.
    pub(crate) fn drift(&self, entry: &Entry) -> Option<Drift> {
        let Some(source) = entry.source() else {
            return Some(Drift::Spec);
        };
        if self.spec != spec(source) {
            Some(Drift::Spec)
        } else if self.extract != source.extract {
            Some(Drift::Extract)
        } else {
            None
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LockedFile {
    pub(crate) path: String,
    pub(crate) size: u64,
    pub(crate) sha256: Sha256,
    pub(crate) md5: Option<Md5>,
    pub(crate) url: String,
    /// for an archive, the tree hash of the files it unpacks to.
    #[serde(default, deserialize_with = "tree")]
    pub(crate) unpacks_to: Option<Sha256>,
}

/// a dataset as the text of data.lock gives it, before a list it keeps beside data.lock is read.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Parsed {
    /// a fetched dataset with its files, or with none yet and the list that holds them.
    Source(Locked, Option<lists::Listed>),
    /// a dataset a recipe made, with none of its files yet and the list that holds them.
    Derived(Derived, lists::Listed),
}

impl Parsed {
    /// the files it pins, in data.lock or in its list.
    fn files(&self) -> u64 {
        match self {
            Parsed::Source(locked, None) => locked.files.len() as u64,
            Parsed::Source(_, Some(listed)) | Parsed::Derived(_, listed) => listed.count,
        }
    }

    fn name(&self) -> &str {
        match self {
            Parsed::Source(locked, _) => &locked.name,
            Parsed::Derived(derived, _) => &derived.name,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Stored {
    version: u32,
    #[serde(default)]
    dataset: Vec<StoredDataset>,
}

/// one `[[dataset]]` table, a source's or a recipe's, so each key only one kind has is optional.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredDataset {
    name: String,
    #[serde(rename = "ref")]
    reference: Option<String>,
    spec: Option<String>,
    extract: Option<bool>,
    resolved: Option<String>,
    title: Option<String>,
    license: Option<String>,
    doi: Option<String>,
    access: Option<Access>,
    from: Option<Vec<StoredInput>>,
    run: Option<Vec<String>>,
    code: Option<Vec<StoredCode>>,
    env: Option<BTreeMap<String, String>>,
    key: Option<String>,
    #[serde(rename = "locked")]
    timestamp: String,
    tree: Option<String>,
    files: Option<Vec<LockedFile>>,
    files_in: Option<Vec<String>>,
    file_count: Option<u64>,
    file_bytes: Option<u64>,
    files_sha256: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredInput {
    name: String,
    content: String,
}

/// a file of code with its hash, or a folder with its tree.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredCode {
    path: String,
    sha256: Option<Sha256>,
    tree: Option<String>,
}

impl StoredDataset {
    fn parsed(mut self) -> Result<Parsed, Error> {
        let listed = self.listed()?;
        let recipe = self.from.is_some()
            || self.run.is_some()
            || self.code.is_some()
            || self.env.is_some()
            || self.key.is_some()
            || self.tree.is_some();
        if recipe {
            return self.derived(listed);
        }
        let name = self.name;
        let (Some(reference), Some(spec), Some(resolved)) =
            (self.reference, self.spec, self.resolved)
        else {
            return Err(broken(format!(
                "data.lock gives {name} neither ref, spec and resolved, nor the from, run, key and tree of a recipe"
            )));
        };
        let files = match (self.files, &listed) {
            (Some(files), None) => files,
            (None, Some(_)) => Vec::new(),
            _ => {
                return Err(broken(format!(
                    "data.lock gives {name} neither its files nor all of files_in, file_count, file_bytes and files_sha256"
                )));
            }
        };
        let locked = Locked {
            name,
            reference,
            spec,
            extract: self.extract.unwrap_or(true),
            resolved,
            title: self.title,
            license: self.license,
            doi: self.doi,
            access: self.access.unwrap_or_default(),
            timestamp: self.timestamp,
            files,
        };
        Ok(Parsed::Source(locked, listed))
    }

    /// the list the dataset names beside data.lock, when it names all of one.
    fn listed(&mut self) -> Result<Option<lists::Listed>, Error> {
        let listing = (
            self.files_in.take(),
            self.file_count,
            self.file_bytes,
            self.files_sha256.take(),
        );
        match listing {
            (None, None, None, None) => Ok(None),
            (Some(parts), Some(count), Some(bytes), Some(sha256)) => {
                let sha256 = prefixed(&sha256).ok_or_else(|| {
                    broken(format!(
                        "data.lock gives {} a files_sha256 that is not sha256: and 64 hex digits",
                        self.name
                    ))
                })?;
                Ok(Some(lists::Listed {
                    parts,
                    count,
                    bytes,
                    sha256,
                }))
            }
            _ => Err(broken(format!(
                "data.lock gives {} only some of files_in, file_count, file_bytes and files_sha256",
                self.name
            ))),
        }
    }

    /// the dataset a recipe made, refused with a key only a fetched one has, `files` among them.
    fn derived(self, listed: Option<lists::Listed>) -> Result<Parsed, Error> {
        let name = self.name;
        let fetched = [
            ("ref", self.reference.is_some()),
            ("spec", self.spec.is_some()),
            ("extract", self.extract.is_some()),
            ("resolved", self.resolved.is_some()),
            ("title", self.title.is_some()),
            ("license", self.license.is_some()),
            ("doi", self.doi.is_some()),
            ("access", self.access.is_some()),
            ("files", self.files.is_some()),
        ];
        if let Some((key, _)) = fetched.iter().find(|(_, set)| *set) {
            return Err(broken(format!(
                "data.lock gives {name} the {key} of a fetched dataset beside the keys of a recipe"
            )));
        }
        let missing = |key: &str| {
            broken(format!(
                "data.lock gives {name}, made by a recipe, no {key}"
            ))
        };
        let hash = |key: &str, text: &str| {
            prefixed(text).ok_or_else(|| {
                broken(format!(
                    "data.lock gives {name} a {key} that is not sha256: and 64 hex digits"
                ))
            })
        };
        let from = self
            .from
            .ok_or_else(|| missing("from"))?
            .into_iter()
            .map(|input| {
                Ok(derived::Input {
                    content: hash("content", &input.content)?,
                    name: input.name,
                })
            })
            .collect::<Result<_, Error>>()?;
        let run = self.run.ok_or_else(|| missing("run"))?;
        let key = hash("key", &self.key.ok_or_else(|| missing("key"))?)?;
        let tree = hash("tree", &self.tree.ok_or_else(|| missing("tree"))?)?;
        let listed = listed.ok_or_else(|| missing("files_in"))?;
        let code = self
            .code
            .unwrap_or_default()
            .into_iter()
            .map(|code| {
                let path = quoted(&code.path);
                let (sha256, folder) = match (code.sha256, code.tree) {
                    (Some(sha256), None) => (sha256, false),
                    (None, Some(tree)) => (
                        prefixed(&tree).ok_or_else(|| {
                            broken(format!("data.lock gives {name} a tree for the code {path} that is not sha256: and 64 hex digits"))
                        })?,
                        true,
                    ),
                    (sha256, _) => {
                        let what = if sha256.is_some() {
                            "both a sha256 and a tree"
                        } else {
                            "neither a sha256 nor a tree"
                        };
                        return Err(broken(format!(
                            "data.lock gives the code {path} of {name} with {what}"
                        )));
                    }
                };
                Ok(derived::Code {
                    path: code.path,
                    sha256,
                    folder,
                })
            })
            .collect::<Result<_, Error>>()?;
        let derived = Derived {
            name,
            from,
            run,
            code,
            env: self.env.unwrap_or_default(),
            key,
            timestamp: self.timestamp,
            tree,
            files: Vec::new(),
        };
        Ok(Parsed::Derived(derived, listed))
    }
}

/// a lock's `version` alone, of any type: a lock in another format may not read as this one.
#[derive(Deserialize)]
struct VersionOnly {
    version: Option<toml::Value>,
}

impl Lock {
    /// a test's lock that lists every file in itself; runs read through [`Lock::read`].
    #[cfg(test)]
    pub(crate) fn parse(text: &str) -> Result<Lock, Error> {
        resolve(parse(text)?, None)
    }

    /// the lock `text` holds, its lists in [`lists::FOLDER`] below `root` read and checked.
    pub(crate) fn read(text: &str, root: &Path) -> Result<Lock, Error> {
        resolve(parse(text)?, Some(root))
    }

    /// [`Lock::read`] of only the datasets `keep` accepts by name: the others are left out, and
    /// their lists beside data.lock are not read.
    pub(crate) fn read_only(
        text: &str,
        root: &Path,
        keep: &dyn Fn(&str) -> bool,
    ) -> Result<Lock, Error> {
        let mut parsed = parse_only(text, keep)?;
        parsed.retain(|entry| keep(entry.name()));
        resolve(parsed, Some(root))
    }

    /// the dataset named `name`, found by halving, since every lock keeps its datasets sorted by
    /// name: as read, in either reader, and as a sync settles it.
    pub(crate) fn get(&self, name: &str) -> Option<&Locked> {
        let index = self
            .datasets
            .binary_search_by(|dataset| dataset.name.as_str().cmp(name))
            .ok()?;
        self.datasets.get(index)
    }

    /// the result of the recipe named `name`, found by halving as [`Lock::get`] is.
    pub(crate) fn result(&self, name: &str) -> Option<&Derived> {
        let index = self
            .derived
            .binary_search_by(|derived| derived.name.as_str().cmp(name))
            .ok()?;
        self.derived.get(index)
    }

    /// the lock's text, the same bytes for the same contents. a source past [`lists::INLINE`]
    /// files, and every recipe result, names its list from [`Lock::lists`] instead of its files.
    pub(crate) fn render(&self) -> String {
        self.render_with(lists::inline())
    }

    /// [`Lock::render`], with a dataset of more than `inline` files kept in a list.
    fn render_with(&self, inline: usize) -> String {
        enum Table<'a> {
            Source(&'a Locked),
            Derived(&'a Derived),
        }
        let mut tables: Vec<(&str, Table)> = self
            .datasets
            .iter()
            .map(|dataset| (dataset.name.as_str(), Table::Source(dataset)))
            .chain(
                self.derived
                    .iter()
                    .map(|derived| (derived.name.as_str(), Table::Derived(derived))),
            )
            .collect();
        tables.sort_by(|a, b| a.0.cmp(b.0));
        let mut out = format!("version = {VERSION}\n");
        for (_, table) in tables {
            match table {
                Table::Source(dataset) => write_source(&mut out, dataset, inline),
                Table::Derived(derived) => derived::write(&mut out, derived),
            }
        }
        out
    }
}

/// a source's table in the order data.lock keeps its keys, optional ones only when set.
fn write_source(out: &mut String, dataset: &Locked, inline: usize) {
    out.push_str("\n[[dataset]]\n");
    for (key, value) in [
        ("name", &dataset.name),
        ("ref", &dataset.reference),
        ("spec", &dataset.spec),
    ] {
        let _ = writeln!(out, "{key} = {}", quoted(value));
    }
    if !dataset.extract {
        out.push_str("extract = false\n");
    }
    let _ = writeln!(out, "resolved = {}", quoted(&dataset.resolved));
    for (key, value) in [
        ("title", &dataset.title),
        ("license", &dataset.license),
        ("doi", &dataset.doi),
    ] {
        if let Some(value) = value {
            let _ = writeln!(out, "{key} = {}", quoted(value));
        }
    }
    if dataset.access != Access::Unknown {
        let _ = writeln!(out, "access = \"{}\"", dataset.access.word());
    }
    let _ = writeln!(out, "locked = {}", quoted(&dataset.timestamp));
    if dataset.files.len() > inline {
        let texts = lists::texts(&sorted(&dataset.files));
        write_listed(out, &lists::listed(&dataset.name, &dataset.files, &texts));
        return;
    }
    let mut files: Vec<&LockedFile> = dataset.files.iter().collect();
    files.sort_by(|a, b| a.path.cmp(&b.path));
    if files.is_empty() {
        out.push_str("files = []\n");
        return;
    }
    out.push_str("files = [\n");
    for file in files {
        let _ = write!(
            out,
            "  {{ path = {}, size = {}, sha256 = \"{}\"",
            quoted(&file.path),
            file.size,
            file.sha256
        );
        if let Some(md5) = file.md5 {
            let _ = write!(out, ", md5 = \"{md5}\"");
        }
        let _ = write!(out, ", url = {}", quoted(&file.url));
        if let Some(tree) = file.unpacks_to {
            let _ = write!(out, ", unpacks_to = \"sha256:{tree}\"");
        }
        out.push_str(" },\n");
    }
    out.push_str("]\n");
}

/// the keys that name a list kept beside data.lock in place of `files`.
fn write_listed(out: &mut String, listed: &lists::Listed) {
    let parts: Vec<String> = listed.parts.iter().map(|part| quoted(part)).collect();
    let _ = writeln!(out, "files_in = [{}]", parts.join(", "));
    let _ = writeln!(out, "file_count = {}", listed.count);
    let _ = writeln!(out, "file_bytes = {}", listed.bytes);
    let _ = writeln!(out, "files_sha256 = \"sha256:{}\"", listed.sha256);
}

impl Lock {
    /// each part of the lists [`Lock::render`] names, as its path below the project and its text.
    pub(crate) fn lists(&self) -> Vec<(String, String)> {
        self.lists_with(lists::inline())
    }

    /// [`Lock::lists`], as [`Lock::render_with`] names them for `inline`.
    fn lists_with(&self, inline: usize) -> Vec<(String, String)> {
        let mut parts = Vec::new();
        for dataset in &self.datasets {
            if dataset.files.len() <= inline {
                continue;
            }
            let texts = lists::texts(&sorted(&dataset.files));
            let listed = lists::listed(&dataset.name, &dataset.files, &texts);
            parts.extend(listed.parts.into_iter().zip(texts));
        }
        for derived in &self.derived {
            parts.extend(derived::parts(derived));
        }
        parts
    }
}

/// the list parts data.lock's `text` names, as paths below the project; none if it does not read.
pub(crate) fn parts(text: &str) -> Vec<String> {
    parse(text)
        .unwrap_or_default()
        .into_iter()
        .flat_map(|parsed| match parsed {
            Parsed::Source(_, Some(listed)) | Parsed::Derived(_, listed) => listed.parts,
            Parsed::Source(_, None) => Vec::new(),
        })
        .collect()
}

/// `files` sorted by path, as the lock keeps them.
fn sorted(files: &[LockedFile]) -> Vec<LockedFile> {
    let mut sorted = files.to_vec();
    sorted.sort_by(|a, b| a.path.cmp(&b.path));
    sorted
}

/// the datasets a lock's text gives in name order, by the narrow reader when it is the writer's.
fn parse(text: &str) -> Result<Vec<Parsed>, Error> {
    parse_only(text, &|_| true)
}

/// [`parse`], where the files of a source `keep` refuses by name may be left unread.
fn parse_only(text: &str, keep: &dyn Fn(&str) -> bool) -> Result<Vec<Parsed>, Error> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let read = |comments| canonical::read_only(text, comments, keep);
    let parsed = match read(false).or_else(|| read(true)) {
        Some(parsed) => parsed,
        None => parse_toml(text)?,
    };
    // a name becomes a path in data.lock.d, and a cloned lock is untrusted
    if let Some(entry) = parsed
        .iter()
        .find(|entry| !crate::name::is_dataset_name(entry.name()))
    {
        return Err(broken(format!(
            "data.lock names a dataset \"{}\", which is not a valid dataset name",
            entry.name()
        )));
    }
    Ok(parsed)
}

/// the datasets any TOML of the lock's format gives, sorted by name, a source's files by path.
fn parse_toml(text: &str) -> Result<Vec<Parsed>, Error> {
    let stored = match toml::from_str::<Stored>(text) {
        Ok(stored) if stored.version == VERSION => stored,
        Ok(stored) => return Err(other_version(stored.version.into())),
        Err(err) => {
            return Err(unread_version(text).unwrap_or_else(|| {
                broken(format!("data.lock cannot be read: {}", err.message()))
            }));
        }
    };
    let mut parsed = stored
        .dataset
        .into_iter()
        .map(StoredDataset::parsed)
        .collect::<Result<Vec<_>, _>>()?;
    parsed.sort_by(|a, b| a.name().cmp(b.name()));
    if let Some(pair) = parsed
        .windows(2)
        .find(|pair| pair[0].name() == pair[1].name())
    {
        return Err(broken(format!(
            "data.lock lists the dataset {} twice",
            pair[0].name()
        )));
    }
    for entry in &mut parsed {
        match entry {
            Parsed::Source(dataset, _) => {
                dataset.files.sort_by(|a, b| a.path.cmp(&b.path));
                if let Some(pair) = dataset
                    .files
                    .windows(2)
                    .find(|pair| pair[0].path == pair[1].path)
                {
                    return Err(broken(format!(
                        "data.lock lists {} twice in {}",
                        pair[0].path, dataset.name
                    )));
                }
                check_paths(dataset)?;
            }
            Parsed::Derived(derived, _) => {
                derived.code.sort_by(|a, b| a.path.cmp(&b.path));
                derived::check(derived)?;
            }
        }
    }
    Ok(parsed)
}

/// the lock `parsed` gives, with each list it keeps below `root` read and checked. every reader's
/// datasets pass through here, so the links they give are checked here once.
fn resolve(parsed: Vec<Parsed>, root: Option<&Path>) -> Result<Lock, Error> {
    fits(&parsed, ROOM.load(Ordering::Relaxed))?;
    let mut lock = Lock::default();
    for entry in parsed {
        match entry {
            Parsed::Source(mut dataset, listed) => {
                check_resolved(&dataset)?;
                let is_listed = listed.is_some();
                if let Some(listed) = listed {
                    let root = project(root, &dataset.name)?;
                    dataset.files = lists::read(root, &dataset.name, &listed)?;
                    check_paths(&dataset)?;
                }
                check_links(&dataset, is_listed)?;
                lock.datasets.push(dataset);
            }
            Parsed::Derived(mut derived, listed) => {
                let root = project(root, &derived.name)?;
                derived.files = lists::read(root, &derived.name, &listed)?;
                derived::check_files(&derived)?;
                lock.derived.push(derived);
            }
        }
    }
    Ok(lock)
}

/// the project folder whose [`lists::FOLDER`] holds the files of `name`.
fn project<'a>(root: Option<&'a Path>, name: &str) -> Result<&'a Path, Error> {
    root.ok_or_else(|| {
        broken(format!(
            "data.lock keeps the files of {name} in {}, which only a project can read",
            lists::FOLDER
        ))
    })
}

/// the hash of everything in a data.toml entry that decides what is downloaded. `copy` and
/// `path` only change where files go, and `extract` what they unpack to, so they are left out.
pub(crate) fn spec(source: &Source) -> String {
    let mut text = format!("ref={}\n", source.reference);
    for (key, globs) in [("select", &source.select), ("exclude", &source.exclude)] {
        for glob in globs.iter().map(Glob::as_str) {
            let _ = writeln!(text, "{key}={glob}");
        }
    }
    // only a page asked for is the data, so taking the line away asks the link again
    if source.web_page {
        text.push_str("web_page=true\n");
    }
    format!("sha256:{}", digest::sha256(text.as_bytes()))
}

/// whether `locked` was pinned while data.toml asked for its link's web page, which `source` no
/// longer does: such a pin counts for nothing then, since only a page asked for is the data.
pub(crate) fn page_no_longer_asked(locked: &Locked, source: &Source) -> bool {
    !source.web_page
        && locked.spec
            == spec(&Source {
                web_page: true,
                ..source.clone()
            })
}

/// the current time as `data.lock` records it: `2026-10-27T17:30:00Z`.
pub(crate) fn timestamp(now: SystemTime) -> String {
    let seconds = now
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let days = i64::try_from(seconds / 86_400).unwrap_or(0);
    let rest = seconds % 86_400;
    // days since 1970 to a civil date, as in Howard Hinnant's chrono algorithms
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

/// a TOML basic string.
fn quoted(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{:04X}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// the error for a lock whose `version` is another format or no whole number, which says so
/// without the rest of the lock; none when it is this format or missing, or the text is no TOML.
fn unread_version(text: &str) -> Option<Error> {
    let what = match toml::from_str::<VersionOnly>(text).ok()?.version? {
        toml::Value::Integer(version) if version == i64::from(VERSION) => return None,
        toml::Value::Integer(version) => return Some(other_version(version)),
        toml::Value::String(_) => "text in quotes",
        toml::Value::Float(_) => "a decimal number",
        toml::Value::Boolean(_) => "true or false",
        toml::Value::Datetime(_) => "a date",
        toml::Value::Array(_) => "a list",
        toml::Value::Table(_) => "a table",
    };
    Some(broken(format!(
        "data.lock cannot be read: version is {what}, not a whole number such as {VERSION}"
    )))
}

fn other_version(version: i64) -> Error {
    let error = Error::new(
        Kind::ProjectInvalid,
        format!("data.lock is in format {version}, and this fetchloom reads format {VERSION}"),
    );
    // no fetchloom ever wrote an older format, so one is a broken file
    if version > i64::from(VERSION) {
        error.help("install a newer fetchloom, then run the same command again")
    } else {
        error.help(BROKEN)
    }
}

fn broken(message: String) -> Error {
    Error::new(Kind::ProjectInvalid, message).help(BROKEN)
}

/// the hash in `sha256:` and 64 hex digits, as data.lock writes a hash of a list or a tree.
fn prefixed(text: &str) -> Option<Sha256> {
    text.strip_prefix("sha256:").and_then(Sha256::from_hex)
}

/// refuses a lock that would place a file outside its dataset's folder. fetchloom never writes
/// such a path, so the lock was edited or made by someone else.
fn check_paths(dataset: &Locked) -> Result<(), Error> {
    for file in &dataset.files {
        if let Some(reason) = name::escapes(&file.path) {
            return Err(broken(format!(
                "data.lock lists {} in {}, which fetchloom refuses because {reason}",
                quoted(&file.path),
                dataset.name
            )));
        }
    }
    Ok(())
}

/// refuses a file link carrying a login or a secret: fetchloom never pins one, so the lock was
/// edited, and a download would send it on. a listed link's help is to restore from git.
fn check_links(dataset: &Locked, is_listed: bool) -> Result<(), Error> {
    // parsing each of a million links doubled the time a lock took to read. links mostly differ
    // in the values of the same keys, so a cleared link's keys are reused where keys alone decide
    let mut cleared: Option<Vec<&str>> = None;
    for file in &dataset.files {
        let text = file.url.as_str();
        let marks = Marks::of(text);
        if !marks.at && !marks.split {
            continue;
        }
        let is_plain = !marks.at && !marks.dropped;
        if is_plain
            && cleared
                .as_ref()
                .is_some_and(|keys| keys.iter().copied().eq(raw_keys(text)))
        {
            continue;
        }
        let Ok(url) = Url::parse(text) else {
            continue;
        };
        if let Some(carried) = Carried::by(&url) {
            let help = if is_listed {
                lists::BROKEN.to_owned()
            } else {
                carried.mend()
            };
            let subject = format!("the link data.lock gives {}/{}", dataset.name, file.path);
            return Err(carried.refused(&subject).help(help));
        }
        if is_plain && !raw_keys(text).any(crate::net::depends_on_path) {
            cleared = Some(raw_keys(text).collect());
        }
    }
    Ok(())
}

/// the keys of `link`'s query, then of its fragment, as written: where the URL parser and
/// `form_urlencoded` split them, before any decoding.
fn raw_keys(link: &str) -> impl Iterator<Item = &str> {
    let (head, fragment) = match link.split_once('#') {
        Some((head, fragment)) => (head, Some(fragment)),
        None => (link, None),
    };
    let query = head.split_once('?').map(|(_, query)| query);
    query
        .into_iter()
        .chain(fragment)
        .flat_map(|part| part.split('&'))
        .map(|pair| pair.split_once('=').map_or(pair, |(key, _)| key))
}

/// whether a text holds what a link needs for a login (`@`) or a query or fragment (`?`, `#`),
/// and a tab or line break, which the URL parser drops before it splits the link.
struct Marks {
    at: bool,
    split: bool,
    dropped: bool,
}

impl Marks {
    fn of(text: &str) -> Marks {
        let (mut at, mut split, mut dropped) = (false, false, false);
        // every byte without an early stop, so the compiler compares many at once
        for &byte in text.as_bytes() {
            at |= byte == b'@';
            split |= byte == b'?' || byte == b'#';
            dropped |= matches!(byte, b'\t' | b'\n' | b'\r');
        }
        Marks { at, split, dropped }
    }
}

/// refuses a resolved web link with a login or a secret, as [`check_links`] does: a sync may
/// request it again. a reference like `drive:<id>?resourcekey=<key>` is never sent as written.
fn check_resolved(dataset: &Locked) -> Result<(), Error> {
    let marks = Marks::of(&dataset.resolved);
    let web = (marks.at || marks.split)
        .then(|| Url::parse(&dataset.resolved).ok())
        .flatten()
        .filter(|url| matches!(url.scheme(), "http" | "https"));
    match web.as_ref().and_then(Carried::by) {
        Some(carried) => {
            let subject = format!("the resolved link data.lock gives {}", dataset.name);
            Err(carried.refused(&subject).help(carried.mend()))
        }
        None => Ok(()),
    }
}

/// what a link carries that lets anyone holding it in.
enum Carried {
    Login,
    /// the key of the query or fragment value [`crate::net::secret`] names, such as `token`.
    Secret(String),
}

impl Carried {
    fn by(url: &Url) -> Option<Carried> {
        if !url.username().is_empty() || url.password().is_some() {
            return Some(Carried::Login);
        }
        crate::net::secret(url).map(Carried::Secret)
    }

    /// the error for `subject`, such as "the link data.lock gives x/a.csv", carrying this.
    fn refused(&self, subject: &str) -> Error {
        let message = match self {
            Carried::Login => format!("{subject} carries a login, which fetchloom never pins"),
            Carried::Secret(key) => {
                format!("{subject} carries {key}, a secret fetchloom never pins")
            }
        };
        Error::new(Kind::ProjectInvalid, message)
    }

    /// how to take it out of data.lock by hand.
    fn mend(&self) -> String {
        let what = match self {
            Carried::Login => "the login".to_owned(),
            Carried::Secret(key) => format!("{key} and its value"),
        };
        format!("remove {what} from that link in data.lock, or restore data.lock from git")
    }
}

fn tree<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Sha256>, D::Error> {
    let Some(text) = Option::<String>::deserialize(deserializer)? else {
        return Ok(None);
    };
    prefixed(&text)
        .map(Some)
        .ok_or_else(|| D::Error::custom("unpacks_to is sha256: and 64 hex digits"))
}

#[cfg(test)]
impl Lock {
    /// the datasets as a reader gives them when every source lists its files inline: by name,
    /// each recipe's result without its files and with the list that holds them.
    fn parsed(&self) -> Vec<Parsed> {
        let mut parsed: Vec<Parsed> = self
            .datasets
            .iter()
            .map(|dataset| Parsed::Source(dataset.clone(), None))
            .collect();
        for derived in &self.derived {
            let mut files = derived.files.clone();
            files.sort_by(|a, b| a.path.cmp(&b.path));
            let texts = lists::texts(&files);
            let listed = lists::listed(&derived.name, &files, &texts);
            let unread = Derived {
                files: Vec::new(),
                ..derived.clone()
            };
            parsed.push(Parsed::Derived(unread, listed));
        }
        parsed.sort_by(|a, b| a.name().cmp(b.name()));
        parsed
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::cache::Member;
    use crate::project::manifest::Manifest;

    fn sample() -> Lock {
        let file = |path: &str, bytes: &[u8]| LockedFile {
            path: path.to_owned(),
            size: bytes.len() as u64,
            sha256: digest::sha256(bytes),
            md5: None,
            url: format!("https://example.org/{path}"),
            unpacks_to: None,
        };
        Lock {
            datasets: vec![
                Locked {
                    name: "zeta".to_owned(),
                    reference: "https://example.org/z.csv".to_owned(),
                    spec: "sha256:00".to_owned(),
                    extract: false,
                    resolved: "https://example.org/z.csv".to_owned(),
                    title: None,
                    license: None,
                    doi: None,
                    access: Access::Unknown,
                    timestamp: "2026-10-27T17:30:00Z".to_owned(),
                    files: vec![file("z.csv", b"z")],
                },
                Locked {
                    name: "iris".to_owned(),
                    reference: "zenodo:3608990".to_owned(),
                    spec: "sha256:11".to_owned(),
                    extract: true,
                    resolved: "zenodo:3608990".to_owned(),
                    title: Some("Iris \"classic\"\tset".to_owned()),
                    license: Some("CC-BY-4.0".to_owned()),
                    doi: Some("10.5281/zenodo.3608990".to_owned()),
                    access: Access::Public,
                    timestamp: "2026-10-27T17:30:00Z".to_owned(),
                    files: vec![
                        LockedFile {
                            md5: Md5::from_hex("900150983cd24fb0d6963f7d28e17f72"),
                            unpacks_to: Some(digest::sha256(b"tree")),
                            ..file("b/iris.zip", b"abc")
                        },
                        file("a.csv", b"a"),
                    ],
                },
            ],
            ..Lock::default()
        }
    }

    /// [`sample`] as a lock keeps it, its datasets sorted by name and their files by path.
    fn sample_sorted() -> Lock {
        let mut sorted = sample();
        sorted.datasets.sort_by(|a, b| a.name.cmp(&b.name));
        for dataset in &mut sorted.datasets {
            dataset.files.sort_by(|a, b| a.path.cmp(&b.path));
        }
        sorted
    }

    #[test]
    fn duplicate_dataset_names_are_refused_with_lock_recovery_help() {
        let mut lock = sample();
        lock.datasets.push(lock.datasets[1].clone());
        let canonical = lock.render();
        let edited = format!("# edited lock\n{canonical}");
        for text in [canonical, edited] {
            let error = Lock::parse(&text).unwrap_err();
            assert_eq!(error.kind, Kind::ProjectInvalid);
            assert_eq!(error.message, "data.lock lists the dataset iris twice");
            assert_eq!(error.help.as_deref(), Some(BROKEN));
        }
    }

    #[test]
    fn a_file_listed_twice_is_refused_by_both_readers() {
        let mut lock = sample();
        let again = lock.datasets[0].files[0].clone();
        lock.datasets[0].files.push(again);
        let canonical = lock.render();
        let edited = format!("# edited lock\n{canonical}");
        for text in [canonical, edited] {
            let error = Lock::parse(&text).unwrap_err();
            assert_eq!(error.kind, Kind::ProjectInvalid);
            assert!(error.message.contains("twice"), "{}", error.message);
            assert_eq!(error.help.as_deref(), Some(BROKEN));
        }
    }

    #[test]
    fn a_name_data_toml_refuses_is_refused_by_both_readers() {
        for name in ["../outside", "a/b", "a\\b"] {
            let mut lock = sample();
            lock.datasets[0].name = name.to_owned();
            let canonical = lock.render();
            let edited = format!("# edited lock\n{canonical}");
            for text in [canonical, edited] {
                let error = Lock::parse(&text).unwrap_err();
                assert_eq!(error.kind, Kind::ProjectInvalid, "{name}");
                assert_eq!(
                    error.message,
                    format!(
                        "data.lock names a dataset \"{name}\", which is not a valid dataset name"
                    ),
                );
                assert_eq!(error.help.as_deref(), Some(BROKEN));
            }
        }
    }

    #[test]
    fn the_same_contents_always_give_the_same_bytes() {
        let text = sample().render();
        assert!(
            text.starts_with("version = 1\n\n[[dataset]]\nname = \"iris\"\n"),
            "{text}"
        );
        assert!(
            text.contains("title = \"Iris \\\"classic\\\"\\tset\"\n"),
            "{text}"
        );
        assert!(
            text.find("path = \"a.csv\"") < text.find("path = \"b/iris.zip\""),
            "files are sorted"
        );
        assert!(text.contains(", md5 = \"900150983cd24fb0d6963f7d28e17f72\", url = \"https://example.org/b/iris.zip\", unpacks_to = \"sha256:"), "{text}");
        let read = Lock::parse(&text).unwrap();
        assert_eq!(read, sample_sorted());
        assert_eq!(
            read.render(),
            text,
            "reading and writing again changes nothing"
        );
    }

    #[test]
    fn a_newer_format_asks_for_a_newer_fetchloom_and_zero_is_broken() {
        for text in ["version = 2\n", "version = 2\n[[dataset]]\nwhatever = 1\n"] {
            let newer = Lock::parse(text).unwrap_err();
            assert_eq!(newer.kind, Kind::ProjectInvalid);
            assert_eq!(
                newer.message,
                "data.lock is in format 2, and this fetchloom reads format 1"
            );
            assert_eq!(
                newer.help.as_deref(),
                Some("install a newer fetchloom, then run the same command again")
            );
        }
        for text in ["version = 0\n", "version = 1\n[[dataset]]\nname = 3\n"] {
            let broken = Lock::parse(text).unwrap_err();
            assert_eq!(broken.kind, Kind::ProjectInvalid);
            assert_eq!(broken.help.as_deref(), Some(BROKEN), "{text}");
        }
        for (version, help) in [
            (
                "4294967296",
                "install a newer fetchloom, then run the same command again",
            ),
            ("-1", BROKEN),
        ] {
            let error = Lock::parse(&format!("version = {version}\n")).unwrap_err();
            assert_eq!(error.kind, Kind::ProjectInvalid);
            assert_eq!(
                error.message,
                format!("data.lock is in format {version}, and this fetchloom reads format 1")
            );
            assert_eq!(error.help.as_deref(), Some(help));
        }
        assert!(
            BROKEN.contains("delete it and run `fetchloom lock`, which resolves every dataset")
        );
        assert_eq!(Lock::parse("version = 1\n").unwrap(), Lock::default());
    }

    /// a version error says what the field holds and leaves the rest of the lock out.
    #[test]
    fn a_version_that_is_no_whole_number_says_what_it_takes() {
        let rest = sample().render().replacen("version = 1\n", "", 1);
        for (version, what) in [
            ("\"1\"", "text in quotes"),
            ("1.0", "a decimal number"),
            ("true", "true or false"),
            ("1979-05-27", "a date"),
            ("[1]", "a list"),
            ("{ format = 1 }", "a table"),
        ] {
            let error = Lock::parse(&format!("version = {version}\n{rest}")).unwrap_err();
            assert_eq!(error.kind, Kind::ProjectInvalid, "{version}");
            assert_eq!(
                error.message,
                format!(
                    "data.lock cannot be read: version is {what}, not a whole number such as 1"
                )
            );
            assert_eq!(error.help.as_deref(), Some(BROKEN), "{version}");
        }
    }

    /// the result `clean` of a recipe made from `iris`, holding two files.
    fn clean() -> Derived {
        let files = vec![
            Member {
                path: "iris.csv".to_owned(),
                size: 3,
                sha256: digest::sha256(b"abc"),
            },
            Member {
                path: "notes/read me.txt".to_owned(),
                size: 1,
                sha256: digest::sha256(b"x"),
            },
        ];
        Derived {
            name: "clean".to_owned(),
            from: vec![derived::Input {
                name: "iris".to_owned(),
                content: digest::sha256(b"iris"),
            }],
            run: vec![
                "python3".to_owned(),
                "clean.py".to_owned(),
                "{iris}".to_owned(),
                "{out}".to_owned(),
            ],
            code: vec![derived::Code {
                path: "clean.py".to_owned(),
                sha256: digest::sha256(b"print()"),
                folder: false,
            }],
            env: BTreeMap::from([("SOURCE_DATE_EPOCH".to_owned(), "315532800".to_owned())]),
            key: digest::sha256(b"key"),
            timestamp: "2026-11-10T00:00:00Z".to_owned(),
            tree: crate::cache::tree_hash(&files),
            files,
        }
    }

    /// `lock` written into a new project folder, its lists beside it, and its text.
    fn written(lock: &Lock) -> (tempfile::TempDir, String) {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join(lists::FOLDER)).unwrap();
        for (part, text) in lock.lists() {
            std::fs::write(root.path().join(part), lists::compress(&text).unwrap()).unwrap();
        }
        let text = lock.render();
        (root, text)
    }

    /// the datasets left out have their file lines passed over unread where no string among them
    /// could hide a line of the lock.
    #[test]
    fn a_read_of_some_datasets_reads_them_as_a_whole_read_does() {
        let mut lock = Lock {
            derived: vec![clean()],
            ..sample()
        };
        let (root, text) = written(&lock);
        let whole = Lock::read(&text, root.path()).unwrap();
        for keep in [&["iris"][..], &["zeta", "clean"], &[]] {
            let read = Lock::read_only(&text, root.path(), &|name| keep.contains(&name)).unwrap();
            let kept: Vec<&Locked> = whole
                .datasets
                .iter()
                .filter(|dataset| keep.contains(&dataset.name.as_str()))
                .collect();
            assert_eq!(read.datasets.iter().collect::<Vec<_>>(), kept);
            assert_eq!(read.derived.len(), usize::from(keep.contains(&"clean")));
        }
        let files = |text: &str, name: &str| {
            canonical::read_only(text, false, &|kept| kept == "iris")
                .unwrap()
                .into_iter()
                .find_map(|parsed| match parsed {
                    Parsed::Source(dataset, _) if dataset.name == name => Some(dataset.files.len()),
                    _ => None,
                })
                .unwrap()
        };
        assert_eq!(files(&text, "iris"), 2);
        assert_eq!(files(&text, "zeta"), 0, "passed over unread");
        let hiding = text.replace("path = \"z.csv\"", "path = \"\"\"z.csv\"");
        assert_eq!(
            canonical::read_only(&hiding, false, &|kept| kept == "iris"),
            None,
            "three quotes could start a string, so the line is read and refused"
        );
        let quoted = LockedFile {
            path: "it's.csv".to_owned(),
            ..lock.datasets[0].files[0].clone()
        };
        lock.datasets[0].files.push(quoted);
        let (_root, text) = written(&lock);
        assert_eq!(
            files(&text, "zeta"),
            2,
            "a quote could start a string, so it is read"
        );
    }

    #[test]
    fn a_derived_entry_round_trips_through_both_readers() {
        let lock = Lock {
            derived: vec![clean()],
            ..sample()
        };
        let (root, text) = written(&lock);
        let parts = lock.lists();
        assert_eq!(parts.len(), 1, "a result's files always live in a list");
        assert!(
            parts[0].1.starts_with("path\tsize\tsha256\n"),
            "{}",
            parts[0].1
        );
        let expected = format!(
            "\n[[dataset]]\nname = \"clean\"\nfrom = [\n  {{ name = \"iris\", content = \"sha256:{}\" }},\n]\nrun = [\"python3\", \"clean.py\", \"{{iris}}\", \"{{out}}\"]\ncode = [\n  {{ path = \"clean.py\", sha256 = \"{}\" }},\n]\nenv = {{ SOURCE_DATE_EPOCH = \"315532800\" }}\nkey = \"sha256:{}\"\nlocked = \"2026-11-10T00:00:00Z\"\ntree = \"sha256:{}\"\nfiles_in = [\"{}\"]\nfile_count = 2\nfile_bytes = 4\nfiles_sha256 = \"sha256:",
            digest::sha256(b"iris"),
            digest::sha256(b"print()"),
            digest::sha256(b"key"),
            clean().tree,
            parts[0].0
        );
        assert!(text.contains(&expected), "{text}");
        assert!(
            text.find("name = \"clean\"") < text.find("name = \"iris\""),
            "tables of both kinds go by name"
        );
        let mut sorted = sample_sorted();
        sorted.derived = vec![clean()];
        assert_eq!(Lock::read(&text, root.path()).unwrap(), sorted);
        assert_eq!(canonical::read(&text, false), Some(sorted.parsed()));
        assert_eq!(parse_toml(&text).unwrap(), sorted.parsed());
        let error = Lock::parse(&text).unwrap_err();
        assert!(error.message.contains("which only a project can read"));
    }

    /// data.lock keeps the tree prefixed, as every tree hash, beside a file's plain hash.
    #[test]
    fn a_code_folder_is_pinned_by_its_tree() {
        let mut result = clean();
        result.code.push(derived::Code {
            path: "lib".to_owned(),
            sha256: digest::sha256(b"tree of lib"),
            folder: true,
        });
        let lock = Lock {
            derived: vec![result.clone()],
            ..Lock::default()
        };
        let (root, text) = written(&lock);
        let lines = format!(
            "code = [\n  {{ path = \"clean.py\", sha256 = \"{}\" }},\n  {{ path = \"lib\", tree = \"sha256:{}\" }},\n]\n",
            digest::sha256(b"print()"),
            digest::sha256(b"tree of lib")
        );
        assert!(text.contains(&lines), "{text}");
        assert_eq!(Lock::read(&text, root.path()).unwrap(), lock);
        assert_eq!(canonical::read(&text, false), Some(lock.parsed()));
        assert_eq!(parse_toml(&text).unwrap(), lock.parsed());
        let tree = format!("tree = \"sha256:{}\"", digest::sha256(b"tree of lib"));
        for (changed, says) in [
            (
                text.replacen(
                    &tree,
                    &format!("{tree}, sha256 = \"{}\"", digest::sha256(b"x")),
                    1,
                ),
                "the code \"lib\" of clean with both a sha256 and a tree",
            ),
            (
                text.replacen(&format!(", {tree}"), "", 1),
                "the code \"lib\" of clean with neither a sha256 nor a tree",
            ),
            (
                text.replacen("tree = \"sha256:", "tree = \"", 1),
                "a tree for the code \"lib\" that is not sha256: and 64 hex digits",
            ),
        ] {
            let error = Lock::read(&changed, root.path()).unwrap_err();
            assert_eq!(error.kind, Kind::ProjectInvalid, "{changed}");
            assert!(error.message.contains(says), "{says}: {}", error.message);
            assert_eq!(canonical::read(&changed, false), None, "{changed}");
        }
    }

    #[test]
    fn a_derived_tree_must_match_its_listing() {
        let mut lock = Lock {
            derived: vec![clean()],
            ..Lock::default()
        };
        let (root, _) = written(&lock);
        lock.derived[0].tree = digest::sha256(b"another tree");
        let error = Lock::read(&lock.render(), root.path()).unwrap_err();
        assert_eq!(error.kind, Kind::ProjectInvalid);
        assert_eq!(
            error.message,
            "the files of clean in data.lock.d are not the tree data.lock records"
        );
        assert!(error.help.unwrap().contains("data.lock.d from git"));
    }

    #[test]
    fn a_malformed_derived_entry_is_refused() {
        let lock = Lock {
            derived: vec![clean()],
            ..Lock::default()
        };
        let (root, text) = written(&lock);
        let content = format!("content = \"sha256:{}\"", digest::sha256(b"iris"));
        let code = format!("sha256 = \"{}\"", digest::sha256(b"print()"));
        for (changed, says) in [
            (
                text.replacen("from = [", "ref = \"zenodo:1\"\nfrom = [", 1),
                "the ref of a fetched dataset beside the keys of a recipe",
            ),
            (
                text.replacen("files_in = [", "files = []\nfiles_in = [", 1),
                "the files of a fetched dataset beside the keys of a recipe",
            ),
            (
                text.replacen("key = \"sha256:", "key = \"", 1),
                "a key that is not sha256: and 64 hex digits",
            ),
            (
                text.replacen("content = \"sha256:", "content = \"", 1),
                "a content that is not sha256: and 64 hex digits",
            ),
            (
                text.replacen("\ntree = ", "\nwhatever = 1\ntree = ", 1),
                "unknown field `whatever`",
            ),
            (text.replacen("tree = ", "branch = ", 1), "unknown field"),
            (
                text.replacen("\nkey = ", "\nkey_was = ", 1),
                "unknown field",
            ),
            (
                text.replacen(&format!("  {{ name = \"iris\", {content} }},\n"), "", 1),
                "no dataset it is made from",
            ),
            (
                text.replacen("name = \"iris\"", "name = \"clean\"", 1),
                "the input \"clean\" twice, as itself or with no name",
            ),
            (
                text.replacen("run = [\"python3\"", "run = [\"\"", 1),
                "no program to run",
            ),
            (
                text.replacen("path = \"clean.py\"", "path = \"../clean.py\"", 1),
                "the code \"../clean.py\" of clean, which fetchloom refuses",
            ),
            (
                text.replacen(
                    &format!("  {{ path = \"clean.py\", {code} }},\n"),
                    &format!("  {{ path = \"clean.py\", {code} }},\n  {{ path = \"clean.py\", {code} }},\n"),
                    1,
                ),
                "the code \"clean.py\" of clean twice",
            ),
            (
                text.replacen("SOURCE_DATE_EPOCH =", "\"A=B\" =", 1),
                "the variable \"A=B\", which no program can be given",
            ),
            (
                text.replacen("\nfiles_in = ", "\nfiles_listed = ", 1),
                "unknown field",
            ),
        ] {
            let error = Lock::read(&changed, root.path()).unwrap_err();
            assert_eq!(error.kind, Kind::ProjectInvalid, "{changed}");
            assert!(error.message.contains(says), "{says}: {}", error.message);
            assert_eq!(canonical::read(&changed, false), None, "{changed}");
        }
        let unlisted = text
            .lines()
            .filter(|line| !line.starts_with("file"))
            .collect::<Vec<_>>()
            .join("\n");
        let error = Lock::read(&unlisted, root.path()).unwrap_err();
        assert!(
            error.message.ends_with("made by a recipe, no files_in"),
            "{}",
            error.message
        );
    }

    /// the memory check counts a dataset's files whether data.lock or its list holds them; a read
    /// of some datasets counts only theirs.
    #[test]
    fn a_lock_too_big_for_its_room_is_refused_before_its_lists() {
        let text = sample_sorted()
            .render_with(1)
            .replace("file_count = 2\n", "file_count = 100000\n");
        let parsed = parse(&text).unwrap();
        // iris's 100,000 in its list, which is not on disk, and zeta's one file inline
        let need = BASE + 100_001 * PER_FILE;
        assert!(fits(&parsed, need).is_ok());
        let error = fits(&parsed, need - 1).unwrap_err();
        assert_eq!(error.kind, Kind::LocalIo);
        assert!(
            error
                .message
                .starts_with("reading this project's 100,001 files takes about "),
            "{}",
            error.message
        );
        let zeta = parse_only(&text, &|name| name == "zeta").unwrap();
        let zeta: Vec<Parsed> = zeta
            .into_iter()
            .filter(|entry| entry.name() == "zeta")
            .collect();
        assert!(fits(&zeta, BASE + PER_FILE).is_ok());
        assert!(fits(&zeta, BASE + PER_FILE - 1).is_err());
    }

    #[test]
    fn a_version_1_lock_reads_a_big_dataset_from_its_parts() {
        let root = tempfile::tempdir().unwrap();
        let files = sorted(&sample().datasets[1].files);
        let texts = lists::texts(&files);
        let listed = lists::listed("iris", &files, &texts);
        std::fs::create_dir(root.path().join(lists::FOLDER)).unwrap();
        for (part, text) in listed.parts.iter().zip(&texts) {
            std::fs::write(root.path().join(part), lists::compress(text).unwrap()).unwrap();
        }
        let text = format!(
            "version = 1\n\n[[dataset]]\nname = \"iris\"\nref = \"zenodo:3608990\"\nspec = \"sha256:11\"\nresolved = \"zenodo:3608990\"\nlocked = \"2026-10-27T17:30:00Z\"\nfiles_in = [\"{}\"]\nfile_count = 2\nfile_bytes = {}\nfiles_sha256 = \"sha256:{}\"\n",
            listed.parts[0], listed.bytes, listed.sha256
        );
        let read = Lock::read(&text, root.path()).unwrap();
        assert_eq!(read.datasets[0].files, files);
        assert_eq!(read.render_with(1), text, "the writer keeps format 1");
        let error = Lock::parse(&text).unwrap_err();
        assert_eq!(error.kind, Kind::ProjectInvalid);
        assert!(
            error.message.contains("only a project can read"),
            "{}",
            error.message
        );
    }

    #[test]
    fn a_large_lock_lookup_finds_every_dataset() {
        let template = sample().datasets.swap_remove(0);
        let names: Vec<String> = (0..50_000).map(|index| format!("f{index:05}")).collect();
        let lock = Lock {
            datasets: names
                .iter()
                .map(|name| Locked {
                    name: name.clone(),
                    ..template.clone()
                })
                .collect(),
            ..Lock::default()
        };
        for name in &names {
            assert_eq!(lock.get(name).map(|locked| &locked.name), Some(name));
        }
        assert!(lock.get("f50000").is_none());
        assert!(lock.get("").is_none());
    }

    #[test]
    fn a_path_that_leaves_its_folder_is_refused() {
        for path in [
            "../../outside.txt",
            "a/../../b",
            "..\\\\..\\\\outside.txt",
            "/etc/passwd",
            "C:/Windows/x",
            "a//b",
        ] {
            let text = format!(
                "version = 1\n[[dataset]]\nname = \"x\"\nref = \"https://example.org/x\"\nspec = \"sha256:00\"\nresolved = \"https://example.org/x\"\nlocked = \"2026-10-27T17:30:00Z\"\nfiles = [\n  {{ path = \"{path}\", size = 1, sha256 = \"{}\", url = \"https://example.org/x\" }},\n]\n",
                digest::sha256(b"x")
            );
            let err = Lock::parse(&text).unwrap_err();
            assert_eq!(err.kind, Kind::ProjectInvalid, "{path}");
            assert!(err.message.contains("data.lock"), "{}", err.message);
            assert_eq!(err.help.as_deref(), Some(BROKEN), "{path}");
        }
    }

    /// `lock` read in each form a reader takes: as its writer leaves it, as other TOML of the same
    /// contents, and with every source's files in a list beside it.
    fn every_form(lock: &Lock) -> [Result<Lock, Error>; 3] {
        let canonical = lock.render();
        assert!(canonical::read(&canonical, false).is_some(), "{canonical}");
        let general = canonical.replacen("version = 1\n", "version=1\n", 1);
        assert!(canonical::read(&general, true).is_none(), "{general}");
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join(lists::FOLDER)).unwrap();
        for (part, text) in lock.lists_with(0) {
            std::fs::write(root.path().join(part), lists::compress(&text).unwrap()).unwrap();
        }
        let listed = lock.render_with(0);
        assert!(listed.contains("\nfiles_in = "), "{listed}");
        [
            Lock::parse(&canonical),
            Lock::parse(&general),
            Lock::read(&listed, root.path()),
        ]
    }

    /// each reader's refusal names the key and never its value.
    #[test]
    fn a_link_with_a_login_or_a_secret_is_refused_by_every_reader() {
        const CANARY: &str = "canary-value";
        let token =
            "remove token and its value from that link in data.lock, or restore data.lock from git";
        for (url, says, help) in [
            (
                format!("https://example.org/a.csv?token={CANARY}"),
                "carries token, a secret fetchloom never pins",
                token,
            ),
            (
                format!("https://example.org/a.csv?download=1#Access-Token={CANARY}"),
                "carries access-token, a secret fetchloom never pins",
                "remove access-token and its value from that link in data.lock, or restore data.lock from git",
            ),
            (
                format!("https://ada:{CANARY}@example.org/a.csv"),
                "carries a login, which fetchloom never pins",
                "remove the login from that link in data.lock, or restore data.lock from git",
            ),
        ] {
            let mut lock = sample();
            lock.datasets[1].files[1].url = url;
            for (form, read) in every_form(&lock).into_iter().enumerate() {
                let error = read.unwrap_err();
                assert_eq!(error.kind, Kind::ProjectInvalid, "{says}");
                assert_eq!(
                    error.message,
                    format!("the link data.lock gives iris/a.csv {says}")
                );
                let listed = form == 2;
                assert_eq!(
                    error.help.as_deref(),
                    Some(if listed { lists::BROKEN } else { help })
                );
                assert!(!format!("{error:?}").contains(CANARY), "{error:?}");
            }
        }
        let mut lock = sample();
        lock.datasets[1].resolved = format!("https://example.org/iris.zip?token={CANARY}");
        for read in every_form(&lock) {
            let error = read.unwrap_err();
            assert_eq!(error.kind, Kind::ProjectInvalid);
            assert_eq!(
                error.message,
                "the resolved link data.lock gives iris carries token, a secret fetchloom never pins"
            );
            assert_eq!(error.help.as_deref(), Some(token));
            assert!(!format!("{error:?}").contains(CANARY), "{error:?}");
        }
    }

    /// whether a key is a secret can turn on the link's path, as dataverse's `key` does, so a
    /// link cleared before vouches for no other with the same keys.
    #[test]
    fn a_dataverse_key_is_refused_after_a_link_with_the_same_keys() {
        const CANARY: &str = "canary-value";
        let mut lock = sample();
        lock.datasets[1].files[1].url = "https://example.org/a.csv?key=1".to_owned();
        lock.datasets[1].files[0].url =
            format!("https://dataverse.example.edu/api/access/datafile/2?key={CANARY}");
        for read in every_form(&lock) {
            let error = read.unwrap_err();
            assert_eq!(
                error.message,
                "the link data.lock gives iris/b/iris.zip carries key, a secret fetchloom never pins"
            );
            assert!(!format!("{error:?}").contains(CANARY), "{error:?}");
        }
    }

    /// links fetchloom pins carry queries but no secret, and an import's is empty; a reference
    /// that is no web link, such as `drive:1?resourcekey=key`, is never sent as written.
    #[test]
    fn a_link_without_a_secret_is_read_as_written() {
        for (resolved, url) in [
            ("zenodo:3608990", "https://example.org/a.csv?download=1"),
            ("hf:datasets/org/x@3c1f0e2", "https://example.org/a@2x.csv"),
            (
                "drive:1?resourcekey=key",
                "https://drive.google.com/uc?id=1&resourcekey=key",
            ),
            (
                "s3:https://minio.lab.example:9000/scans/",
                "https://minio.lab.example:9000/scans/a.csv?versionId=3",
            ),
            (
                "https://www.dropbox.com/s/x/a.csv?rlkey=key&dl=1",
                "https://www.dropbox.com/s/x/a.csv?rlkey=key&dl=1",
            ),
            (
                "https://example.org/a.csv",
                "https://example.org/a.csv#page=2",
            ),
            ("local", ""),
        ] {
            let mut lock = sample();
            lock.datasets[1].resolved = resolved.to_owned();
            lock.datasets[1].files[1].url = url.to_owned();
            for read in every_form(&lock) {
                let read = read.unwrap();
                let iris = read.get("iris").unwrap();
                assert_eq!(iris.resolved, resolved);
                assert_eq!(iris.files[0].url, url);
            }
        }
    }

    #[test]
    fn the_spec_follows_what_decides_the_download() {
        let entry = |extra: &str| {
            Manifest::parse(&format!("[datasets.x]\nref = \"zenodo:1\"\n{extra}"))
                .unwrap()
                .datasets["x"]
                .clone()
        };
        let plain = spec(entry("").source().unwrap());
        assert_eq!(
            plain,
            spec(
                entry("copy = true\npath = \"elsewhere\"\n")
                    .source()
                    .unwrap()
            ),
            "copy and path do not change what is downloaded"
        );
        assert_ne!(plain, spec(entry("select = \"*.csv\"\n").source().unwrap()));
        assert_eq!(
            plain,
            spec(entry("extract = false\n").source().unwrap()),
            "extract is compared on its own, so a switch keeps the files"
        );
        assert!(plain.starts_with("sha256:") && plain.len() == 71);
    }

    #[test]
    fn access_is_written_unless_unknown_and_refused_when_misspelled() {
        let text = sample().render();
        assert!(
            text.contains("doi = \"10.5281/zenodo.3608990\"\naccess = \"public\"\nlocked = "),
            "{text}"
        );
        assert_eq!(text.matches("access").count(), 1, "{text}");
        assert_eq!(
            canonical::read(&text, false),
            Some(sample_sorted().parsed())
        );
        let unknown = text.replace("access = \"public\"", "access = \"unknown\"");
        assert_eq!(canonical::read(&unknown, false), None);
        let read = Lock::parse(&unknown).unwrap();
        assert!(
            read.datasets
                .iter()
                .all(|dataset| dataset.access == Access::Unknown)
        );
        assert!(!read.render().contains("access"));
        let error = Lock::parse(&text.replace("\"public\"", "\"secret\"")).unwrap_err();
        assert_eq!(error.kind, Kind::ProjectInvalid);
        assert_eq!(error.help.as_deref(), Some(BROKEN));
    }

    #[test]
    fn extract_is_written_only_when_false_and_drifts_apart_from_the_spec() {
        let text = sample().render();
        assert!(
            text.contains("spec = \"sha256:00\"\nextract = false\nresolved = "),
            "{text}"
        );
        assert_eq!(text.matches("extract").count(), 1, "{text}");
        let written = Lock::parse(&text.replace("extract = false", "extract = true")).unwrap();
        assert!(written.datasets.iter().all(|dataset| dataset.extract));
        assert!(!written.render().contains("extract"));

        let entry = |extra: &str| {
            Manifest::parse(&format!("[datasets.x]\nref = \"zenodo:1\"\n{extra}"))
                .unwrap()
                .datasets["x"]
                .clone()
        };
        let locked = Locked {
            spec: spec(entry("").source().unwrap()),
            extract: true,
            ..sample().datasets[1].clone()
        };
        assert_eq!(locked.drift(&entry("")), None);
        assert_eq!(locked.drift(&entry("copy = true\n")), None);
        assert_eq!(
            locked.drift(&entry("extract = false\n")),
            Some(Drift::Extract)
        );
        assert_eq!(
            locked.drift(&entry("extract = false\nselect = \"*.csv\"\n")),
            Some(Drift::Spec)
        );
    }

    /// a dataset that asks for no web page keeps the spec it always had, so data.lock files
    /// written before stay up to date; taking `web_page = true` away changes it, and the pin made
    /// with it counts for nothing.
    #[test]
    fn web_page_joins_the_spec_only_when_asked() {
        let entry = |extra: &str| {
            Manifest::parse(&format!(
                "[datasets.x]\nref = \"https://example.org/notes\"\n{extra}"
            ))
            .unwrap()
            .datasets["x"]
                .clone()
        };
        let plain = entry("select = \"*\"\n");
        let plain = plain.source().unwrap();
        assert_eq!(
            spec(plain),
            format!(
                "sha256:{}",
                digest::sha256(b"ref=https://example.org/notes\nselect=*\n")
            )
        );
        let asked = entry("select = \"*\"\nweb_page = true\n");
        let asked = asked.source().unwrap();
        let pinned = |source: &Source| Locked {
            spec: spec(source),
            ..sample().datasets[1].clone()
        };
        assert_eq!(
            pinned(asked).drift(&entry("select = \"*\"\n")),
            Some(Drift::Spec)
        );
        assert!(page_no_longer_asked(&pinned(asked), plain));
        assert!(!page_no_longer_asked(&pinned(asked), asked));
        assert!(!page_no_longer_asked(&pinned(plain), plain));
    }

    #[test]
    #[expect(
        clippy::duration_suboptimal_units,
        reason = "unix timestamps count seconds"
    )]
    fn timestamps_are_utc_and_exact() {
        assert_eq!(timestamp(UNIX_EPOCH), "1970-01-01T00:00:00Z");
        assert_eq!(
            timestamp(UNIX_EPOCH + Duration::from_secs(1_793_122_200)),
            "2026-10-27T17:30:00Z"
        );
        assert_eq!(
            timestamp(UNIX_EPOCH + Duration::from_secs(951_782_400)),
            "2000-02-29T00:00:00Z"
        );
    }

    #[test]
    fn control_characters_are_escaped() {
        assert_eq!(quoted("a\u{1}b\u{7f}"), "\"a\\u0001b\\u007F\"");
    }
}
