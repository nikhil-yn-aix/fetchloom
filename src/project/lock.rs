//! `data.lock`: what each dataset resolved to and every file it downloaded. only fetchloom writes
//! it, in a fixed order, so the same inputs always give the same bytes and a diff shows exactly
//! what changed.

mod canonical;
pub(crate) mod lists;
#[cfg(test)]
mod model_tests;

use std::fmt::Write as _;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::de::Error as _;
use serde::{Deserialize, Deserializer};

use super::manifest::Entry;
use crate::digest::{self, Md5, Sha256};
use crate::error::{Error, Kind};
use crate::glob::Glob;
use crate::name;

pub(crate) const FILE: &str = "data.lock";
/// the one format of data.lock. a dataset of more files than it lists in itself keeps them in
/// [`lists::FOLDER`], in the same format.
const VERSION: u32 = 1;
/// how to recover a data.lock this fetchloom cannot use. `fetchloom lock` alone reads the same
/// file first and fails the same way.
pub(crate) const BROKEN: &str = "restore data.lock from git, or delete it and run `fetchloom lock`, which resolves every dataset again and may pin newer data";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Lock {
    /// sorted by name.
    pub(crate) datasets: Vec<Locked>,
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
    /// what changed in `entry` since this pin was made, or nothing when it still answers it.
    pub(crate) fn drift(&self, entry: &Entry) -> Option<Drift> {
        if self.spec != spec(entry) {
            Some(Drift::Spec)
        } else if self.extract != entry.extract {
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
    /// a dataset fetched from a source, with its files, or with none yet and the list that holds
    /// them.
    Source(Locked, Option<lists::Listed>),
}

impl Parsed {
    fn name(&self) -> &str {
        match self {
            Parsed::Source(locked, _) => &locked.name,
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

/// a dataset as data.lock holds it: its files, or where a list of them is kept.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredDataset {
    name: String,
    #[serde(rename = "ref")]
    reference: String,
    spec: String,
    extract: Option<bool>,
    resolved: String,
    title: Option<String>,
    license: Option<String>,
    doi: Option<String>,
    #[serde(rename = "locked")]
    timestamp: String,
    files: Option<Vec<LockedFile>>,
    files_in: Option<Vec<String>>,
    file_count: Option<u64>,
    file_bytes: Option<u64>,
    files_sha256: Option<String>,
}

impl StoredDataset {
    /// the dataset, with its files or the list that holds them.
    fn parsed(self) -> Result<Parsed, Error> {
        let listing = (
            self.files_in,
            self.file_count,
            self.file_bytes,
            self.files_sha256,
        );
        let (files, listed) = match (self.files, listing) {
            (Some(files), (None, None, None, None)) => (files, None),
            (None, (Some(parts), Some(count), Some(bytes), Some(sha256))) => {
                let sha256 = prefixed(&sha256).ok_or_else(|| {
                    broken(format!(
                        "data.lock gives {} a files_sha256 that is not sha256: and 64 hex digits",
                        self.name
                    ))
                })?;
                let listed = lists::Listed {
                    parts,
                    count,
                    bytes,
                    sha256,
                };
                (Vec::new(), Some(listed))
            }
            _ => {
                return Err(broken(format!(
                    "data.lock gives {} neither its files nor all of files_in, file_count, file_bytes and files_sha256",
                    self.name
                )));
            }
        };
        let locked = Locked {
            name: self.name,
            reference: self.reference,
            spec: self.spec,
            extract: self.extract.unwrap_or(true),
            resolved: self.resolved,
            title: self.title,
            license: self.license,
            doi: self.doi,
            timestamp: self.timestamp,
            files,
        };
        Ok(Parsed::Source(locked, listed))
    }
}

#[derive(Deserialize)]
struct VersionOnly {
    version: Option<u32>,
}

impl Lock {
    /// the lock `text` holds, when it lists every file in itself. runs read through
    /// [`Lock::read`], which reads the lists a lock keeps beside it too.
    #[cfg(test)]
    pub(crate) fn parse(text: &str) -> Result<Lock, Error> {
        resolve(parse(text)?, None)
    }

    /// the lock `text` holds, with the file lists it keeps in [`lists::FOLDER`] below `root`
    /// read and checked.
    pub(crate) fn read(text: &str, root: &Path) -> Result<Lock, Error> {
        resolve(parse(text)?, Some(root))
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

    /// the lock's text, the same bytes for the same contents. a dataset of more than
    /// [`lists::INLINE`] files names the list [`Lock::lists`] gives instead of its files.
    pub(crate) fn render(&self) -> String {
        self.render_with(lists::inline())
    }

    /// [`Lock::render`], with a dataset of more than `inline` files kept in a list.
    fn render_with(&self, inline: usize) -> String {
        let mut datasets: Vec<&Locked> = self.datasets.iter().collect();
        datasets.sort_by(|a, b| a.name.cmp(&b.name));
        let mut out = format!("version = {VERSION}\n");
        for dataset in datasets {
            write_source(&mut out, dataset, inline);
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
    /// the parts of every file list the lock keeps beside it, each with its path below the
    /// project and its text, as [`Lock::render`] names them.
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
        parts
    }
}

/// `files` sorted by path, as the lock keeps them.
fn sorted(files: &[LockedFile]) -> Vec<LockedFile> {
    let mut sorted = files.to_vec();
    sorted.sort_by(|a, b| a.path.cmp(&b.path));
    sorted
}

/// the datasets the text of a lock gives, in name order: through the narrow reader when the text
/// is in the writer's form, otherwise through the general TOML reader.
fn parse(text: &str) -> Result<Vec<Parsed>, Error> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    match canonical::read(text, false).or_else(|| canonical::read(text, true)) {
        Some(parsed) => Ok(parsed),
        None => parse_toml(text),
    }
}

/// the datasets any valid TOML of the lock's format gives, sorted by name, each source's files
/// sorted by path.
fn parse_toml(text: &str) -> Result<Vec<Parsed>, Error> {
    let stored = match toml::from_str::<Stored>(text) {
        Ok(stored) if stored.version == VERSION => stored,
        Ok(stored) => return Err(other_version(stored.version)),
        Err(err) => {
            return Err(match toml::from_str::<VersionOnly>(text) {
                Ok(VersionOnly {
                    version: Some(version),
                }) if version != VERSION => other_version(version),
                _ => broken(format!("data.lock cannot be read: {}", err.message())),
            });
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
        let Parsed::Source(dataset, _) = entry;
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
    Ok(parsed)
}

/// the lock `parsed` gives, with each list it keeps below `root` read and checked.
fn resolve(parsed: Vec<Parsed>, root: Option<&Path>) -> Result<Lock, Error> {
    let mut lock = Lock::default();
    for entry in parsed {
        match entry {
            Parsed::Source(mut dataset, listed) => {
                if let Some(listed) = listed {
                    let Some(root) = root else {
                        return Err(broken(format!(
                            "data.lock keeps the files of {} in {}, which only a project can read",
                            dataset.name,
                            lists::FOLDER
                        )));
                    };
                    dataset.files = lists::read(root, &dataset.name, &listed)?;
                    check_paths(&dataset)?;
                }
                lock.datasets.push(dataset);
            }
        }
    }
    Ok(lock)
}

/// the hash of everything in a data.toml entry that decides what is downloaded. `copy` and
/// `path` only change where files go, and `extract` what they unpack to, so they are left out.
pub(crate) fn spec(entry: &Entry) -> String {
    let mut text = format!("ref={}\n", entry.reference);
    for (key, globs) in [("select", &entry.select), ("exclude", &entry.exclude)] {
        for glob in globs.iter().map(Glob::as_str) {
            let _ = writeln!(text, "{key}={glob}");
        }
    }
    format!("sha256:{}", digest::sha256(text.as_bytes()))
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

fn other_version(version: u32) -> Error {
    let error = Error::new(
        Kind::ProjectInvalid,
        format!("data.lock is in format {version}, and this fetchloom reads format {VERSION}"),
    );
    // no fetchloom ever wrote an older format, so one is a broken file
    if version > VERSION {
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
    /// the datasets as a reader gives them when the lock lists every file in itself.
    fn parsed(&self) -> Vec<Parsed> {
        self.datasets
            .iter()
            .map(|dataset| Parsed::Source(dataset.clone(), None))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
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
        }
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
        let mut sorted = sample();
        sorted.datasets.sort_by(|a, b| a.name.cmp(&b.name));
        for dataset in &mut sorted.datasets {
            dataset.files.sort_by(|a, b| a.path.cmp(&b.path));
        }
        assert_eq!(read, sorted);
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
        assert!(
            BROKEN.contains("delete it and run `fetchloom lock`, which resolves every dataset")
        );
        assert_eq!(Lock::parse("version = 1\n").unwrap(), Lock::default());
    }

    /// a dataset of more files than data.lock lists in itself keeps them in parts beside it, in
    /// the same format 1 as every other lock.
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

    #[test]
    fn the_spec_follows_what_decides_the_download() {
        let entry = |extra: &str| {
            Manifest::parse(&format!("[datasets.x]\nref = \"zenodo:1\"\n{extra}"))
                .unwrap()
                .datasets["x"]
                .clone()
        };
        let plain = spec(&entry(""));
        assert_eq!(
            plain,
            spec(&entry("copy = true\npath = \"elsewhere\"\n")),
            "copy and path do not change what is downloaded"
        );
        assert_ne!(plain, spec(&entry("select = \"*.csv\"\n")));
        assert_eq!(
            plain,
            spec(&entry("extract = false\n")),
            "extract is compared on its own, so a switch keeps the files"
        );
        assert!(plain.starts_with("sha256:") && plain.len() == 71);
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
            spec: spec(&entry("")),
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
