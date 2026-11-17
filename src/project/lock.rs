//! `data.lock`: what each dataset resolved to and the files it pins. only fetchloom writes it, in
//! a fixed order, so the same inputs give the same bytes.

pub(crate) mod archives;
mod canonical;
pub(crate) mod lists;
pub(crate) mod merge;
#[cfg(test)]
mod model_tests;

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer};
use url::Url;

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
/// it is the process's own, since the limit holds the whole process, read once at start. unset,
/// as in tests, means no limit.
static ROOM: OnceLock<u64> = OnceLock::new();

/// the most files one dataset may list in this run, which a lock's list may hold no more of. it
/// is the process's own, set once at start; unset, as in tests, it is [`crate::net::MOST_FILES`].
static MOST: OnceLock<usize> = OnceLock::new();

/// sets [`MOST`] for every later read of a lock. set once at start; a second set is a bug, which
/// debug builds stop at.
pub(crate) fn set_most_files(most: usize) {
    let first = MOST.set(most.max(1)).is_ok();
    debug_assert!(
        first,
        "the most files a dataset may list is set once, at start"
    );
}

/// the most files a lock's list of one dataset may hold in this run.
pub(super) fn most_files() -> usize {
    MOST.get().copied().unwrap_or(crate::net::MOST_FILES)
}

/// sets [`ROOM`] for every later read of a lock; none means no container limit. set once at
/// start; a second set is a bug, which debug builds stop at.
pub(crate) fn set_room(room: Option<u64>) {
    let first = ROOM.set(room.unwrap_or(u64::MAX)).is_ok();
    debug_assert!(first, "the container's room is set once, at start");
}

/// refuses, before any list is read, a lock whose files take more memory than `room`, so a run
/// too big for its container says so instead of being killed partway.
fn fits<'a>(parsed: impl IntoIterator<Item = &'a Parsed>, room: u64) -> Result<(), Error> {
    Budget::of(parsed, room).map(|_| ())
}

/// the memory reading a lock's files takes, against the `room` a container leaves: [`PER_FILE`]
/// a file and [`BASE`] counted before any list is read, then what each row read from a list
/// holds past that, so a list of long rows is refused as it is read rather than filling the room.
pub(crate) struct Budget {
    files: u64,
    need: u64,
    room: u64,
    /// what a lock read before and still held takes of the room, as [`Lock::read_with`] says.
    taken: u64,
}

impl Budget {
    /// the budget of the files `parsed` pins, refused as [`fits`] says when they do not fit.
    fn of<'a>(parsed: impl IntoIterator<Item = &'a Parsed>, room: u64) -> Result<Budget, Error> {
        let files: u64 = parsed.into_iter().map(Parsed::files).sum();
        let need = BASE.saturating_add(files.saturating_mul(PER_FILE));
        if need > room {
            return Err(too_big(
                &crate::units::count(files, "file", "files"),
                need,
                room,
            ));
        }
        Ok(Budget {
            files,
            need,
            room,
            taken: 0,
        })
    }

    /// takes the memory a row of `name`'s list holding `text` bytes of path and link needs past
    /// [`PER_FILE`]: a status held about twice the text of a list of long rows.
    pub(super) fn take(&mut self, name: &str, text: u64) -> Result<(), Error> {
        let past = text.saturating_sub(PER_FILE).saturating_mul(2);
        self.need = self.need.saturating_add(past);
        if self.need <= self.room {
            return Ok(());
        }
        // what is not read yet would take more still
        let error = Error::new(
            Kind::LocalIo,
            format!(
                "reading this project's {} takes at least {} of memory, more than the {} this container leaves fetchloom",
                crate::units::count(self.files, "file", "files"),
                crate::units::size(self.need),
                crate::units::size(self.room)
            ),
        )
        .detail(format!(
            "the file list of {name} holds longer paths and links than most"
        ));
        let error = if self.taken == 0 {
            error
        } else {
            error.detail(format!(
                "the other data.lock, read already, takes about {} of it",
                crate::units::size(self.taken)
            ))
        };
        Err(error.help(raise(self.need - self.room)))
    }
}

/// refuses, before it is parsed, the text of a lock, or of `what` as messages name it, which is
/// `bytes` long: the text and what it parses into take twice its size at least, so a text too big
/// for `room` says so before parsing it fills the container.
fn fits_text(what: &str, bytes: u64, room: u64) -> Result<(), Error> {
    let need = bytes.saturating_mul(2);
    if need <= room {
        return Ok(());
    }
    // naming one dataset reads the whole text all the same
    Err(too_big(what, need, room).help(format!(
        "raise the container's memory limit by {} or more",
        crate::units::size(need - room)
    )))
}

/// refuses, before it is read, a data.lock of `bytes` too big for the room a container leaves, as
/// [`fits_text`] refuses its text.
pub(crate) fn fits_file(bytes: u64) -> Result<(), Error> {
    fits_text(
        &format!("data.lock, which is {},", crate::units::size(bytes)),
        bytes,
        ROOM.get().copied().unwrap_or(u64::MAX),
    )
}

/// the error for reading `what` of the project, which takes about `need` bytes of memory, more
/// than the `room` its container leaves.
fn too_big(what: &str, need: u64, room: u64) -> Error {
    Error::new(
        Kind::LocalIo,
        format!(
            "reading this project's {what} takes about {} of memory, more than the {} this container leaves fetchloom",
            crate::units::size(need),
            crate::units::size(room)
        ),
    )
    .help(raise(need - room))
}

/// how to give a read that is `short` bytes short the memory it takes.
fn raise(short: u64) -> String {
    format!(
        "raise the container's memory limit by {} or more, or name one dataset, as in `{}`, which reads only its files",
        crate::units::size(short),
        crate::suggested::command("path <name>")
    )
}

/// what to do about a data.lock holding a field or a value this fetchloom does not know.
const NEWER: &str = "install a newer fetchloom to read it; if someone edited data.lock by hand, restore it from git";

/// how to recover a data.lock this fetchloom cannot use. `fetchloom lock` alone reads the same
/// file first and fails the same way.
pub(crate) fn recovery() -> String {
    format!(
        "restore data.lock from git, or delete it and run `{}`, which resolves every dataset again and may pin newer data",
        crate::suggested::command("lock")
    )
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Lock {
    /// the datasets fetched from a source, sorted by name.
    pub(crate) datasets: Vec<Locked>,
    /// the datasets pinned by what their sources state, not downloaded yet, sorted by name. no
    /// name is in two lists.
    pub(crate) stated: Vec<Stated>,
    /// the datasets whose list data.lock names and data.lock.d lacks, sorted by name: none of
    /// their files is known, and data.lock is written with each entry as it was read. one the other
    /// lists pin again is written as they pin it.
    pub(crate) waiting: Vec<Waiting>,
}

/// a dataset `add --no-sync` pinned by what its source states of each file, its size and SHA-256
/// or MD5, before anything downloaded it. only a sync, which downloads it and pins it whole, uses
/// its files; every other command that needs them refuses it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Stated {
    /// the pin as a downloaded dataset's says it, but its `files`, which stay empty.
    pub(crate) pin: Locked,
    /// sorted by path.
    pub(crate) files: Vec<StatedFile>,
    /// the title, license and doi as data.lock writes them. a reader escapes the control
    /// characters in `pin`'s for display; writing and filling the pin keep these instead.
    pub(crate) written: Words,
}

/// a pin's title, license and doi.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Words {
    pub(crate) title: Option<String>,
    pub(crate) license: Option<String>,
    pub(crate) doi: Option<String>,
}

impl Words {
    pub(crate) fn of(locked: &Locked) -> Words {
        Words {
            title: locked.title.clone(),
            license: locked.license.clone(),
            doi: locked.doi.clone(),
        }
    }
}

/// a dataset data.lock pins in a list that is not beside it, as [`Lock::read_waiting`] finds it:
/// it waits while every other dataset is used, its entry kept as data.lock gives it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Waiting {
    pub(crate) name: String,
    /// the first part of its list that is not there, by its path below the project.
    pub(crate) part: String,
    /// the project data.lock is in, where a part deleted comes back from git.
    root: PathBuf,
    /// its pin as data.lock gives it, without its files.
    pin: Locked,
    /// the list that holds its files.
    listed: lists::Listed,
}

impl Waiting {
    /// what a command that needs it says: the part that is not there, and how to bring it back.
    pub(crate) fn error(&self) -> Error {
        lists::not_there(&self.name, &self.part, Some(&self.root))
    }

    /// its pin, as data.lock gives it, without its files.
    pub(crate) fn pin(&self) -> &Locked {
        &self.pin
    }

    /// the parts of its list, each a path below the project, the missing ones among them.
    pub(crate) fn parts(&self) -> &[String] {
        &self.listed.parts
    }

    /// whether `entry`, data.toml's table of its name if it has one, still asks for this pin:
    /// nothing changed that decides what it downloads or what its archives unpack to. a pin that
    /// no longer answers goes, and its name is pinned as one data.lock does not pin.
    pub(crate) fn answers(&self, entry: Option<&Entry>) -> bool {
        entry.is_some_and(|entry| self.pin.drift(entry).is_none())
    }

    /// `entry` waiting for `part` of its list, which is not there below `root`; `entry` back when
    /// it keeps no list beside data.lock.
    fn of(entry: Parsed, part: String, root: &Path) -> Result<Waiting, Box<Parsed>> {
        match entry {
            Parsed::Source(pin, Some(listed)) => Ok(Waiting {
                name: pin.name.clone(),
                part,
                root: root.to_path_buf(),
                pin,
                listed,
            }),
            other @ (Parsed::Source(_, None) | Parsed::Stated(_)) => Err(Box::new(other)),
        }
    }
}

/// the first part below `root` that is not there of the list `entry` names, when it names one as
/// fetchloom writes it.
fn missing_part(entry: &Parsed, root: &Path) -> Option<String> {
    entry
        .written()?
        .parts
        .iter()
        .find(|part| lists::missing(root, part))
        .cloned()
}

/// the datasets whose list data.lock's `text` names as fetchloom writes it and the project at
/// `root` lacks a part of, as [`Lock::read_waiting`] keeps them. none if it does not read; a
/// list's lines are not read.
pub(crate) fn missing_lists(text: &str, root: &Path) -> Vec<Waiting> {
    // no list, nothing to miss, without parsing a lock of only inline files
    if !text.contains("files_in") {
        return Vec::new();
    }
    parse_within(text, &|_| false, ROOM.get().copied().unwrap_or(u64::MAX))
        .unwrap_or_default()
        .into_iter()
        .filter_map(|parsed| {
            let part = missing_part(&parsed, root)?;
            Waiting::of(parsed, part, root).ok()
        })
        .collect()
}

/// the SHA-256 of each file list data.lock's `text` names as fetchloom writes it, none if it
/// does not read; a list's lines are not read.
pub(crate) fn named_lists(text: &str) -> std::collections::HashSet<Sha256> {
    if !text.contains("files_in") {
        return std::collections::HashSet::new();
    }
    parse_within(text, &|_| false, ROOM.get().copied().unwrap_or(u64::MAX))
        .unwrap_or_default()
        .iter()
        .filter_map(|parsed| parsed.written().map(|listed| listed.sha256))
        .collect()
}

/// each dataset data.lock's `text` pins, by name, with the parts of its file list, none if it
/// does not read; a list's lines are not read.
pub(crate) fn listed(text: &str) -> Vec<(String, Vec<String>)> {
    parse_within(text, &|_| false, ROOM.get().copied().unwrap_or(u64::MAX))
        .unwrap_or_default()
        .iter()
        .map(|parsed| {
            let parts = parsed
                .listed()
                .map(|listed| listed.parts.clone())
                .unwrap_or_default();
            (parsed.name().to_owned(), parts)
        })
        .collect()
}

/// a file as its source states it: a SHA-256, an MD5 or both.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StatedFile {
    pub(crate) path: String,
    pub(crate) size: u64,
    pub(crate) sha256: Option<Sha256>,
    pub(crate) md5: Option<Md5>,
    pub(crate) url: String,
}

impl Stated {
    pub(crate) fn name(&self) -> &str {
        &self.pin.name
    }

    /// what pins its files, as messages say it: each file's SHA-256 where its source states one,
    /// else its MD5.
    pub(crate) fn digests(&self) -> &'static str {
        let sha256 = self
            .files
            .iter()
            .filter(|file| file.sha256.is_some())
            .count();
        match sha256 {
            0 => "MD5",
            all if all == self.files.len() => "SHA-256",
            _ => "SHA-256 and MD5",
        }
    }

    /// the bytes of its files.
    pub(crate) fn bytes(&self) -> u64 {
        self.files
            .iter()
            .map(|file| file.size)
            .fold(0, u64::saturating_add)
    }
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
    /// whether single compressed files decompress, as data.toml asked when the dataset was
    /// pinned, compared apart from `spec` as `extract` is.
    pub(crate) decompress: bool,
    /// whether data.toml asked for the link's web page when the dataset was pinned, so its pins
    /// may be pages: a relock once it no longer asks never carries one.
    pub(crate) web_page: bool,
    pub(crate) resolved: String,
    pub(crate) title: Option<String>,
    pub(crate) license: Option<String>,
    pub(crate) doi: Option<String>,
    /// who may read the data, as its source said when it was pinned: a hint for sharing it.
    pub(crate) access: Access,
    /// when the pinned data last changed, in UTC.
    pub(crate) timestamp: String,
    /// the identity of the files a pipeline sees in it ([`crate::tree::identity`]), when the
    /// machine that wrote it knew them: always for plain files, for archives once unpacked there.
    pub(crate) tree: Option<Sha256>,
    /// the datasets this one was made from, as an add recorded them, by name. only an add
    /// writes them; every other change of the pin keeps them as they were.
    pub(crate) from: Vec<Recorded>,
    /// sorted by path.
    pub(crate) files: Vec<LockedFile>,
}

/// a dataset another was made from, as data.lock keeps it in that one's `from`: its name, and
/// the identity of its files when that one was pinned from them ([`crate::tree::identity`]),
/// none when they were not known.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Recorded {
    pub(crate) name: String,
    pub(crate) tree: Option<Sha256>,
}

/// what data.lock writes for a tree that was not known.
pub(crate) const UNKNOWN: &str = "unknown";

impl Recorded {
    /// the tree as data.lock writes it.
    pub(crate) fn tree_text(&self) -> String {
        self.tree
            .map_or_else(|| UNKNOWN.to_owned(), |tree| format!("sha256:{tree}"))
    }
}

/// what changed in a data.toml entry since its dataset was pinned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Drift {
    /// only `extract` or `decompress`: the pinned files stand, and only what they unpack to
    /// changes.
    Extract,
    /// what is downloaded, so the dataset is resolved again.
    Spec,
}

impl Locked {
    /// what changed in `entry` since this pin was made, or nothing when it still answers it.
    pub(crate) fn drift(&self, entry: &Entry) -> Option<Drift> {
        let source = &entry.source;
        if self.spec != spec(source) {
            Some(Drift::Spec)
        } else if self.extract != source.extract || self.decompress != source.decompress {
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
    /// a dataset pinned by what its source states, its files always in data.lock.
    Stated(Stated),
}

impl Parsed {
    /// the files it pins, in data.lock or in its list.
    fn files(&self) -> u64 {
        match self {
            Parsed::Source(locked, None) => locked.files.len() as u64,
            Parsed::Source(_, Some(listed)) => listed.count,
            Parsed::Stated(stated) => stated.files.len() as u64,
        }
    }

    /// the list beside data.lock that holds its files, if one does.
    fn listed(&self) -> Option<&lists::Listed> {
        match self {
            Parsed::Source(_, listed) => listed.as_ref(),
            Parsed::Stated(_) => None,
        }
    }

    /// [`Parsed::listed`] when data.lock names its parts as fetchloom writes them.
    fn written(&self) -> Option<&lists::Listed> {
        let listed = self.listed()?;
        lists::named_as_written(self.name(), listed).then_some(listed)
    }

    fn name(&self) -> &str {
        match self {
            Parsed::Source(locked, _) => &locked.name,
            Parsed::Stated(stated) => stated.name(),
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

/// one `[[dataset]]` table, a downloaded dataset's or one its source's digests pin, so each key
/// only one kind has is optional.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredDataset {
    name: String,
    #[serde(rename = "ref")]
    reference: Option<String>,
    spec: Option<String>,
    extract: Option<bool>,
    decompress: Option<bool>,
    web_page: Option<bool>,
    resolved: Option<String>,
    title: Option<String>,
    license: Option<String>,
    doi: Option<String>,
    access: Option<Access>,
    #[serde(rename = "locked")]
    timestamp: String,
    tree: Option<String>,
    from: Option<Vec<StoredRecord>>,
    downloaded: Option<bool>,
    files: Option<Vec<StoredFile>>,
    files_in: Option<Vec<String>>,
    file_count: Option<u64>,
    file_bytes: Option<u64>,
    files_sha256: Option<String>,
}

/// one dataset of a `from` list, its tree as data.lock writes it.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredRecord {
    name: String,
    tree: String,
}

/// one file of a `files` list: a downloaded dataset's with its SHA-256, or one its source states
/// with a SHA-256, an MD5 or both.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredFile {
    path: String,
    size: u64,
    sha256: Option<Sha256>,
    md5: Option<Md5>,
    url: String,
    #[serde(default, deserialize_with = "tree")]
    unpacks_to: Option<Sha256>,
}

impl StoredDataset {
    fn parsed(mut self) -> Result<Parsed, Error> {
        let listed = self.listed()?;
        let name = self.name;
        let from = recorded(&name, self.from.unwrap_or_default())?;
        let tree = match self.tree {
            None => None,
            Some(text) => Some(prefixed(&text).ok_or_else(|| {
                broken(format!(
                    "data.lock gives {name} a tree that is not sha256: and 64 hex digits"
                ))
            })?),
        };
        let (Some(reference), Some(spec), Some(resolved)) =
            (self.reference, self.spec, self.resolved)
        else {
            return Err(broken(format!(
                "data.lock lacks the ref, spec or resolved of {name}"
            )));
        };
        let stated = self.downloaded == Some(false);
        if stated && listed.is_some() {
            return Err(broken(format!(
                "data.lock keeps the files of {name}, which is not downloaded yet, in {}, where fetchloom never keeps them",
                lists::FOLDER
            )));
        }
        let stored = match (self.files, &listed) {
            (Some(files), None) => files,
            (None, Some(_)) => Vec::new(),
            _ => {
                return Err(broken(format!(
                    "data.lock gives {name} neither its files nor all of files_in, file_count, file_bytes and files_sha256"
                )));
            }
        };
        let mut locked = Locked {
            name,
            reference,
            spec,
            extract: self.extract.unwrap_or(true),
            decompress: self.decompress.unwrap_or(false),
            web_page: self.web_page.unwrap_or(false),
            resolved,
            title: self.title,
            license: self.license,
            doi: self.doi,
            access: self.access.unwrap_or_default(),
            timestamp: self.timestamp,
            tree,
            from,
            files: Vec::new(),
        };
        if stated {
            let files = stored
                .into_iter()
                .map(|file| stated_file(&locked.name, file))
                .collect::<Result<_, _>>()?;
            let written = Words::of(&locked);
            return Ok(Parsed::Stated(Stated {
                pin: locked,
                files,
                written,
            }));
        }
        locked.files = stored
            .into_iter()
            .map(|file| {
                let Some(sha256) = file.sha256 else {
                    return Err(broken(format!(
                        "data.lock gives {} the file {} without its sha256",
                        locked.name,
                        quoted(&file.path)
                    )));
                };
                Ok(LockedFile {
                    path: file.path,
                    size: file.size,
                    sha256,
                    md5: file.md5,
                    url: file.url,
                    unpacks_to: file.unpacks_to,
                })
            })
            .collect::<Result<_, _>>()?;
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
}

/// a file of `name`, a dataset not downloaded yet, as its source states it: with a SHA-256, an MD5
/// or both, and never what it unpacks to, which only its bytes tell.
fn stated_file(name: &str, file: StoredFile) -> Result<StatedFile, Error> {
    let path = quoted(&file.path);
    if file.unpacks_to.is_some() {
        return Err(broken(format!(
            "data.lock gives the file {path} of {name}, which is not downloaded yet, what it unpacks to, which only its bytes tell"
        )));
    }
    if file.sha256.is_none() && file.md5.is_none() {
        return Err(broken(format!(
            "data.lock gives the file {path} of {name}, which is not downloaded yet, neither a sha256 nor an md5"
        )));
    }
    Ok(StatedFile {
        path: file.path,
        size: file.size,
        sha256: file.sha256,
        md5: file.md5,
        url: file.url,
    })
}

/// a lock's `version` alone, of any type: a lock in another format may not read as this one.
#[derive(Deserialize)]
struct VersionOnly {
    version: Option<toml::Value>,
}

impl Lock {
    /// a test's lock that lists every file in itself; runs read through [`Lock::read_waiting`].
    #[cfg(test)]
    pub(crate) fn parse(text: &str) -> Result<Lock, Error> {
        resolve(parse(text)?, None)
    }

    /// a test's [`Lock::read_waiting`] of every dataset, where a list that is not there stops the
    /// read too.
    #[cfg(test)]
    pub(crate) fn read(text: &str, root: &Path) -> Result<Lock, Error> {
        resolve(parse(text)?, Some(root))
    }

    /// [`Lock::read_waiting`] of a lock kept elsewhere, as in a git commit, within the same limits: `part`
    /// gives a part of a list by its path below the project, once the path is one fetchloom
    /// writes for the list data.lock names. a dataset with a part `has` lacks, as a commit made
    /// without data.lock.d lacks it, is kept with no files and named in what comes back. `taken`
    /// is the memory a lock read before and still held takes, which this one fits beside.
    pub(crate) fn read_with(
        text: &str,
        part: &dyn Fn(&str) -> Result<Vec<u8>, Error>,
        has: &dyn Fn(&str) -> bool,
        taken: u64,
    ) -> Result<(Lock, Vec<String>), Error> {
        let room = ROOM
            .get()
            .copied()
            .unwrap_or(u64::MAX)
            .saturating_sub(taken);
        let beside = |error: Error| {
            if taken == 0 {
                return error;
            }
            error.detail(format!(
                "the other data.lock, read already, takes about {} of it",
                crate::units::size(taken)
            ))
        };
        let bytes = text.len() as u64;
        fits_text(
            &format!("data.lock, which is {},", crate::units::size(bytes)),
            bytes,
            room,
        )
        .map_err(beside)?;
        let parsed = parse_only(text, &|_| true)?;
        let mut budget = Budget::of(&parsed, room).map_err(beside)?;
        budget.taken = taken;
        // a part fetchloom would not write is refused as a read refuses it, never passed over
        let (listed, unlisted): (Vec<Parsed>, Vec<Parsed>) =
            parsed.into_iter().partition(|entry| {
                entry.listed().is_none_or(|listed| {
                    !lists::named_as_written(entry.name(), listed)
                        || listed.parts.iter().all(|part| has(part))
                })
            });
        let mut lock = resolve_from(listed, Some(lists::Parts::Given(part)), &mut budget)?;
        let mut names = Vec::with_capacity(unlisted.len());
        for entry in unlisted {
            names.push(entry.name().to_owned());
            if let Parsed::Source(dataset, _) = &entry {
                check_resolved(dataset)?;
            }
            lock.put_unlisted(entry);
        }
        Ok((lock, names))
    }

    /// puts `entry`, whose list is not read, among the datasets with no files.
    fn put_unlisted(&mut self, entry: Parsed) {
        match entry {
            Parsed::Source(dataset, _) => {
                let at = self
                    .datasets
                    .partition_point(|known| known.name < dataset.name);
                self.datasets.insert(at, dataset);
            }
            Parsed::Stated(_) => {}
        }
    }

    /// moves each dataset that waits for its list among the datasets with no files, as
    /// [`Lock::read_with`] gives a commit's that lacks one, and gives their names.
    pub(crate) fn unwait(&mut self) -> Vec<String> {
        let waiting = std::mem::take(&mut self.waiting);
        let mut names = Vec::with_capacity(waiting.len());
        for waiting in waiting {
            if !self.pins(&waiting.name) {
                names.push(waiting.name);
                self.put_unlisted(Parsed::Source(waiting.pin, Some(waiting.listed)));
            }
        }
        names
    }

    /// the memory a lock with as many files as this one takes as it is read.
    pub(crate) fn taken(&self) -> u64 {
        let files = self
            .datasets
            .iter()
            .map(|locked| locked.files.len())
            .chain(self.stated.iter().map(|stated| stated.files.len()))
            .map(|count| count as u64)
            .fold(0, u64::saturating_add);
        BASE.saturating_add(files.saturating_mul(PER_FILE))
    }

    /// the lock `text` holds of the datasets `keep` accepts by name, their lists in
    /// [`lists::FOLDER`] below `root` read and checked; the others are left out, their lists not
    /// read. a dataset whose list is not beside data.lock, as in a clone of a commit that left
    /// data.lock.d out, waits: its entry is kept in [`Lock::waiting`] as read, so every other
    /// dataset can be used and data.lock written. a list that is there but does not read, one
    /// named otherwise than fetchloom writes it, and a data.lock.d that is no real folder still
    /// stop the read.
    pub(crate) fn read_waiting(
        text: &str,
        root: &Path,
        keep: &dyn Fn(&str) -> bool,
    ) -> Result<Lock, Error> {
        Lock::read_waiting_within(text, root, keep, ROOM.get().copied().unwrap_or(u64::MAX))
    }

    /// [`Lock::read_waiting`] with its text held to `memory`, the room a container leaves.
    fn read_waiting_within(
        text: &str,
        root: &Path,
        keep: &dyn Fn(&str) -> bool,
        memory: u64,
    ) -> Result<Lock, Error> {
        let mut parsed = parse_within(text, keep, memory)?;
        parsed.retain(|entry| keep(entry.name()));
        // a part looked for through a linked data.lock.d would be looked for outside the project
        if parsed.iter().any(|entry| entry.written().is_some()) {
            lists::folder(root)?;
        }
        let mut waiting = Vec::new();
        let mut listed = Vec::with_capacity(parsed.len());
        for entry in parsed {
            let Some(part) = missing_part(&entry, root) else {
                listed.push(entry);
                continue;
            };
            match Waiting::of(entry, part, root) {
                // written back as read, so its link is held to what any read allows
                Ok(waits) => {
                    check_resolved(&waits.pin)?;
                    waiting.push(waits);
                }
                Err(entry) => listed.push(*entry),
            }
        }
        let mut budget = Budget::of(&listed, memory)?;
        let mut lock = resolve_from(listed, Some(lists::Parts::Folder(root)), &mut budget)?;
        lock.waiting = waiting;
        Ok(lock)
    }

    /// the dataset named `name` that waits for its list, as [`Lock::read_waiting`] found it,
    /// unless another list pins it again.
    pub(crate) fn waits(&self, name: &str) -> Option<&Waiting> {
        let index = self
            .waiting
            .binary_search_by(|waiting| waiting.name.as_str().cmp(name))
            .ok()?;
        self.waiting.get(index).filter(|_| !self.pins(name))
    }

    /// the datasets that wait for their lists, as [`Lock::waits`] tells each.
    pub(crate) fn still_waiting(&self) -> impl Iterator<Item = &Waiting> {
        self.waiting
            .iter()
            .filter(|waiting| !self.pins(&waiting.name))
    }

    /// refuses with what [`Waiting::error`] says when one of `names` waits for its list.
    pub(crate) fn refuse_waiting<'n>(
        &self,
        names: impl IntoIterator<Item = &'n str>,
    ) -> Result<(), Error> {
        match names.into_iter().find_map(|name| self.waits(name)) {
            Some(waiting) => Err(waiting.error()),
            None => Ok(()),
        }
    }

    /// whether a list other than [`Lock::waiting`] pins `name`.
    pub(crate) fn pins(&self, name: &str) -> bool {
        self.get(name).is_some() || self.stated(name).is_some()
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

    /// the dataset named `name` that its source's digests pin before a sync downloads it.
    pub(crate) fn stated(&self, name: &str) -> Option<&Stated> {
        let index = self
            .stated
            .binary_search_by(|stated| stated.name().cmp(name))
            .ok()?;
        self.stated.get(index)
    }

    /// refuses the datasets of `names` that only their sources' digests pin, since a command
    /// that needs their files can use them only once a sync downloaded them.
    ///
    /// # Errors
    ///
    /// `data.not_ready`, naming each such dataset and the sync that downloads it.
    pub(crate) fn refuse_stated<'n>(
        &self,
        names: impl IntoIterator<Item = &'n str>,
    ) -> Result<(), Error> {
        let mut found: Vec<&Stated> = names
            .into_iter()
            .filter_map(|name| self.stated(name))
            .collect();
        found.sort_by(|a, b| a.name().cmp(b.name()));
        found.dedup_by(|a, b| a.name() == b.name());
        match stated_error(&found) {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

/// the error for `found`, datasets only their sources' digests pin, which a sync downloads.
pub(crate) fn stated_error(found: &[&Stated]) -> Option<Error> {
    let message = match found {
        [] => return None,
        [one] => format!(
            "{} is pinned by its source's {} until a sync downloads it",
            one.name(),
            one.digests()
        ),
        many => {
            let names: Vec<&str> = many.iter().map(|stated| stated.name()).collect();
            format!(
                "{} are pinned by their sources' digests until a sync downloads them",
                crate::units::listed(&names, "and")
            )
        }
    };
    let names: Vec<&str> = found.iter().map(|stated| stated.name()).collect();
    Some(Error::new(Kind::NotReady, message).help(format!(
        "run `{}` where the data can download, then commit data.lock",
        crate::suggested::command(&format!("sync {}", names.join(" ")))
    )))
}

impl Lock {
    /// the lock's text, the same bytes for the same contents. a source past [`lists::INLINE`]
    /// files names its list from [`Lock::lists`] instead of its files.
    pub(crate) fn render(&self) -> String {
        self.render_with(lists::inline())
    }

    /// [`Lock::render`], with a dataset of more than `inline` files kept in a list.
    fn render_with(&self, inline: usize) -> String {
        enum Table<'a> {
            Source(&'a Locked),
            Stated(&'a Stated),
            Waiting(&'a Waiting),
        }
        let mut tables: Vec<(&str, Table)> = self
            .datasets
            .iter()
            .map(|dataset| (dataset.name.as_str(), Table::Source(dataset)))
            .chain(
                self.stated
                    .iter()
                    .map(|stated| (stated.name(), Table::Stated(stated))),
            )
            .chain(
                self.still_waiting()
                    .map(|waiting| (waiting.name.as_str(), Table::Waiting(waiting))),
            )
            .collect();
        tables.sort_by(|a, b| a.0.cmp(b.0));
        let mut out = format!("version = {VERSION}\n");
        for (_, table) in tables {
            match table {
                Table::Source(dataset) => write_source(&mut out, dataset, inline),
                Table::Stated(stated) => write_stated(&mut out, stated),
                Table::Waiting(waiting) => {
                    write_head(&mut out, &waiting.pin);
                    write_listed(&mut out, &waiting.listed);
                }
            }
        }
        out
    }
}

/// the keys of a fetched dataset's table up to `locked`, in the order data.lock keeps them,
/// optional ones only when set.
fn write_head(out: &mut String, dataset: &Locked) {
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
    if dataset.decompress {
        out.push_str("decompress = true\n");
    }
    if dataset.web_page {
        out.push_str("web_page = true\n");
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
    if let Some(tree) = dataset.tree {
        let _ = writeln!(out, "tree = \"sha256:{tree}\"");
    }
    if !dataset.from.is_empty() {
        let mut inputs: Vec<&Recorded> = dataset.from.iter().collect();
        inputs.sort_by(|a, b| a.name.cmp(&b.name));
        out.push_str("from = [\n");
        for input in inputs {
            let _ = writeln!(
                out,
                "  {{ name = {}, tree = \"{}\" }},",
                quoted(&input.name),
                input.tree_text()
            );
        }
        out.push_str("]\n");
    }
}

/// a source's table in the order data.lock keeps its keys, optional ones only when set.
fn write_source(out: &mut String, dataset: &Locked, inline: usize) {
    write_head(out, dataset);
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

/// the table of a dataset not downloaded yet: a downloaded one's keys, `downloaded = false`, then
/// each file inline with the digests its source states, in the order a downloaded file's are kept.
fn write_stated(out: &mut String, stated: &Stated) {
    let Words {
        title,
        license,
        doi,
    } = stated.written.clone();
    write_head(
        out,
        &Locked {
            title,
            license,
            doi,
            ..stated.pin.clone()
        },
    );
    out.push_str("downloaded = false\n");
    if stated.files.is_empty() {
        out.push_str("files = []\n");
        return;
    }
    let mut files: Vec<&StatedFile> = stated.files.iter().collect();
    files.sort_by(|a, b| a.path.cmp(&b.path));
    out.push_str("files = [\n");
    for file in files {
        let _ = write!(
            out,
            "  {{ path = {}, size = {}",
            quoted(&file.path),
            file.size
        );
        if let Some(sha256) = file.sha256 {
            let _ = write!(out, ", sha256 = \"{sha256}\"");
        }
        if let Some(md5) = file.md5 {
            let _ = write!(out, ", md5 = \"{md5}\"");
        }
        let _ = writeln!(out, ", url = {} }},", quoted(&file.url));
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

    /// refuses a lock whose lists would hold a file whose path or link is longer than a list
    /// keeps, as [`lists::listable`] says.
    pub(crate) fn listable(&self) -> Result<(), Error> {
        let inline = lists::inline();
        for dataset in self
            .datasets
            .iter()
            .filter(|dataset| dataset.files.len() > inline)
        {
            lists::listable(&dataset.name, &dataset.files).map_err(|error| {
                error.help(format!(
                    "leave that file out with an `exclude` pattern for {} in data.toml",
                    dataset.name
                ))
            })?;
        }
        Ok(())
    }

    /// [`Lock::lists`], as [`Lock::render_with`] names them for `inline`.
    fn lists_with(&self, inline: usize) -> Vec<(String, String)> {
        self.hashed_lists_with(inline)
            .into_iter()
            .map(|(part, text, _)| (part, text))
            .collect()
    }

    /// [`Lock::lists`], each part with the SHA-256 of the whole list it is a part of.
    pub(crate) fn hashed_lists(&self) -> Vec<(String, String, Sha256)> {
        self.hashed_lists_with(lists::inline())
    }

    fn hashed_lists_with(&self, inline: usize) -> Vec<(String, String, Sha256)> {
        let mut parts = Vec::new();
        for dataset in &self.datasets {
            if dataset.files.len() <= inline {
                continue;
            }
            let texts = lists::texts(&sorted(&dataset.files));
            let listed = lists::listed(&dataset.name, &dataset.files, &texts);
            let sha256 = listed.sha256;
            parts.extend(
                listed
                    .parts
                    .into_iter()
                    .zip(texts)
                    .map(|(part, text)| (part, text, sha256)),
            );
        }
        parts
    }
}

/// the list parts data.lock's `text` names, as paths below the project; none if it does not read.
/// a list named otherwise than fetchloom writes it, as a cloned lock may, gives none.
pub(crate) fn parts(text: &str) -> Vec<String> {
    parse(text)
        .unwrap_or_default()
        .into_iter()
        .flat_map(|parsed| match parsed {
            Parsed::Source(locked, Some(listed)) => written(&locked.name, listed),
            Parsed::Source(_, None) | Parsed::Stated(_) => Vec::new(),
        })
        .collect()
}

/// the parts of `name`'s list `listed`, when they are named as fetchloom writes them.
fn written(name: &str, listed: lists::Listed) -> Vec<String> {
    if lists::named_as_written(name, &listed) {
        listed.parts
    } else {
        Vec::new()
    }
}

/// `files` sorted by path, as the lock keeps them.
fn sorted(files: &[LockedFile]) -> Vec<LockedFile> {
    let mut sorted = files.to_vec();
    sorted.sort_by(|a, b| a.path.cmp(&b.path));
    sorted
}

/// the datasets a lock's text gives in name order, by the narrow reader when it is the writer's.
fn parse(text: &str) -> Result<Vec<Parsed>, Error> {
    parse_within(text, &|_| true, ROOM.get().copied().unwrap_or(u64::MAX))
}

/// [`parse_only`] within `room`: a text too big for it is refused before it is parsed, as
/// [`fits_text`] says, and [`fits`] weighs the files once it is.
fn parse_within(text: &str, keep: &dyn Fn(&str) -> bool, room: u64) -> Result<Vec<Parsed>, Error> {
    let bytes = text.len() as u64;
    fits_text(
        &format!("data.lock, which is {},", crate::units::size(bytes)),
        bytes,
        room,
    )?;
    parse_only(text, keep)
}

/// [`parse`], where the files of a source `keep` refuses by name may be left unread.
fn parse_only(text: &str, keep: &dyn Fn(&str) -> bool) -> Result<Vec<Parsed>, Error> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let read = |comments| canonical::read_only(text, comments, keep);
    let mut parsed = match read(false).or_else(|| read(true)) {
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
    // a part holds one file at least, but the one part of an empty list, so more parts are no
    // list fetchloom wrote, and comparing their names would first make as many again
    for entry in &parsed {
        let Parsed::Source(_, Some(listed)) = entry else {
            continue;
        };
        let most = listed.count.max(1).min(most_files() as u64);
        if listed.parts.len() as u64 > most {
            return Err(broken(format!(
                "data.lock keeps the files of {} in {} parts, more than fetchloom writes for {}",
                entry.name(),
                crate::units::grouped(listed.parts.len() as u64),
                crate::units::count(listed.count, "file", "files")
            )));
        }
    }
    // a downloaded dataset's words stay as written, which writing data.lock again keeps, and the
    // output shows escaped; a dataset not downloaded keeps its words as written apart
    for entry in &mut parsed {
        if let Parsed::Stated(Stated { pin: locked, .. }) = entry {
            for words in [&mut locked.title, &mut locked.license, &mut locked.doi]
                .into_iter()
                .flatten()
            {
                escape_controls(words);
            }
        }
    }
    Ok(parsed)
}

/// writes each control character in `words`, a title, license or doi, as its escape, but a tab
/// or a line break, so a cloned lock's words cannot steer the terminal that shows them.
fn escape_controls(words: &mut String) {
    let steers = |c: char| crate::name::is_control(c) && !matches!(c, '\t' | '\n');
    if !words.contains(steers) {
        return;
    }
    let mut escaped = String::with_capacity(words.len() + 8);
    for c in words.chars() {
        if steers(c) {
            escaped.extend(c.escape_default());
        } else {
            escaped.push(c);
        }
    }
    *words = escaped;
}

/// the datasets any TOML of the lock's format gives, sorted by name, a source's files by path.
fn parse_toml(text: &str) -> Result<Vec<Parsed>, Error> {
    let stored = match toml::from_str::<Stored>(text) {
        Ok(stored) if stored.version == VERSION => stored,
        Ok(stored) => return Err(other_version(stored.version.into())),
        Err(err) => return Err(unread_version(text).unwrap_or_else(|| unreadable(text, &err))),
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
                check_paths(&dataset.name, dataset.files.iter().map(|file| &file.path))?;
            }
            Parsed::Stated(stated) => {
                stated.files.sort_by(|a, b| a.path.cmp(&b.path));
                if let Some(pair) = stated
                    .files
                    .windows(2)
                    .find(|pair| pair[0].path == pair[1].path)
                {
                    return Err(broken(format!(
                        "data.lock lists {} twice in {}",
                        pair[0].path,
                        stated.name()
                    )));
                }
                check_paths(stated.name(), stated.files.iter().map(|file| &file.path))?;
            }
        }
    }
    Ok(parsed)
}

/// the lock `parsed` gives, with each list it keeps below `root` read and checked within the room
/// a container leaves.
#[cfg(test)]
fn resolve(parsed: Vec<Parsed>, root: Option<&Path>) -> Result<Lock, Error> {
    let mut budget = Budget::of(&parsed, ROOM.get().copied().unwrap_or(u64::MAX))?;
    resolve_from(parsed, root.map(lists::Parts::Folder), &mut budget)
}

/// the lock `parsed` gives, with each list it keeps read from `parts` and checked, weighed against
/// `budget`. every reader's datasets pass through here, so the links they give are checked here
/// once.
fn resolve_from(
    parsed: Vec<Parsed>,
    parts: Option<lists::Parts<'_>>,
    budget: &mut Budget,
) -> Result<Lock, Error> {
    let mut lock = Lock::default();
    for entry in parsed {
        match entry {
            Parsed::Source(mut dataset, listed) => {
                check_resolved(&dataset)?;
                let is_listed = listed.is_some();
                if let Some(listed) = listed {
                    let parts = project(parts, &dataset.name)?;
                    dataset.files = lists::read(parts, &dataset.name, &listed, budget)?;
                    check_paths(&dataset.name, dataset.files.iter().map(|file| &file.path))?;
                }
                let links = dataset.files.iter().map(|file| (&file.path, &file.url));
                check_links(&dataset.name, links, is_listed)?;
                lock.datasets.push(dataset);
            }
            Parsed::Stated(stated) => {
                check_resolved(&stated.pin)?;
                check_stated_count(&stated, lists::inline().min(most_files()))?;
                let links = stated.files.iter().map(|file| (&file.path, &file.url));
                check_links(stated.name(), links, false)?;
                lock.stated.push(stated);
            }
        }
    }
    Ok(lock)
}

/// refuses a dataset not downloaded yet with more than `most` files, the most data.lock keeps
/// inline, since fetchloom never writes one: a list beside data.lock holds only downloaded files.
fn check_stated_count(stated: &Stated, most: usize) -> Result<(), Error> {
    if stated.files.len() <= most {
        return Ok(());
    }
    Err(broken(format!(
        "data.lock gives {}, which is not downloaded yet, {} files, more than the {} fetchloom writes",
        stated.name(),
        crate::units::grouped(stated.files.len() as u64),
        crate::units::grouped(most as u64)
    )))
}

/// where the list of the files of `name` is.
fn project<'a>(parts: Option<lists::Parts<'a>>, name: &str) -> Result<lists::Parts<'a>, Error> {
    parts.ok_or_else(|| {
        broken(format!(
            "data.lock keeps the files of {name} in {}, which only a project can read",
            lists::FOLDER
        ))
    })
}

/// the hash of everything in a data.toml entry that decides what is downloaded. `copy` and
/// `path` only change where files go, and `extract` and `decompress` what they unpack to, so they
/// are left out.
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
/// longer does, whatever else changed: such a pin counts for nothing then, since only a page
/// asked for is the data.
pub(crate) fn page_no_longer_asked(locked: &Locked, source: &Source) -> bool {
    locked.web_page && !source.web_page
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
    // no fetchloom ever wrote an older format, so one is a broken file
    if version > i64::from(VERSION) {
        return Error::new(
            Kind::ProjectInvalid,
            format!(
                "data.lock is in format {version}, which a newer fetchloom wrote, and this one reads format {VERSION}"
            ),
        )
        .help(format!(
            "update fetchloom, as {} shows, then run the same command again",
            super::manifest::INSTALL
        ));
    }
    Error::new(
        Kind::ProjectInvalid,
        format!("data.lock is in format {version}, and this fetchloom reads format {VERSION}"),
    )
    .help(recovery())
}

/// the error for a data.lock the toml reader refused: the line it stopped at, in plain words, with
/// the reader's own reason after them.
fn unreadable(text: &str, err: &toml::de::Error) -> Error {
    if let Some(error) = newer(text, err) {
        return error;
    }
    let reason = err.message();
    // serde names the integer type it could not fill, as "as i128, expected u64"
    let what = if reason.starts_with("invalid type: integer") || reason.contains("out of range") {
        "holds a number too large for fetchloom to store"
    } else {
        "is damaged"
    };
    let message = match err.span() {
        Some(span) => {
            let line = text[..span.start.min(text.len())].matches('\n').count() + 1;
            format!("data.lock cannot be read: line {line} {what}")
        }
        None => format!("data.lock cannot be read: it {what}"),
    };
    broken(message).detail(format!("the toml reader says: {reason}"))
}

/// the error for a data.lock the toml reader refused for a field or a value this fetchloom does
/// not know, as a newer fetchloom may write one, none for any other reason: such a lock is not
/// damaged, and pinning anew would throw away what it pins. the reader's words follow as a detail.
fn newer(text: &str, err: &toml::de::Error) -> Option<Error> {
    let reason = err.message();
    let (rest, what) = match reason.strip_prefix("unknown field `") {
        Some(rest) => (rest, "field"),
        None => (reason.strip_prefix("unknown variant `")?, "value"),
    };
    // the name is the lock's own text, so only a plain word is repeated
    let word = rest.split_once('`').map(|(word, _)| word).filter(|word| {
        !word.is_empty()
            && word.len() <= 40
            && word
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    });
    let at = err.span().map_or_else(String::new, |span| {
        let line = text[..span.start.min(text.len())].matches('\n').count() + 1;
        format!(" on line {line}")
    });
    let message = match word {
        Some(word) => format!(
            "data.lock holds {word}{at}, a {what} this fetchloom does not know, so a newer fetchloom likely wrote it"
        ),
        None => format!(
            "data.lock holds a {what} this fetchloom does not know{at}, so a newer fetchloom likely wrote it"
        ),
    };
    Some(
        Error::new(Kind::ProjectInvalid, message)
            .detail(format!("the toml reader says: {reason}"))
            .help(NEWER),
    )
}

fn broken(message: String) -> Error {
    Error::new(Kind::ProjectInvalid, message).help(recovery())
}

/// the hash in `sha256:` and 64 hex digits, as data.lock writes a hash of a list or a tree.
fn prefixed(text: &str) -> Option<Sha256> {
    text.strip_prefix("sha256:").and_then(Sha256::from_hex)
}

/// the datasets `stored` records `dataset` was made from, sorted by name, or the refusal of a
/// `from` fetchloom never writes: a name no dataset can have, a tree neither a hash nor unknown,
/// or a name twice.
fn recorded(dataset: &str, stored: Vec<StoredRecord>) -> Result<Vec<Recorded>, Error> {
    let mut inputs = Vec::with_capacity(stored.len());
    for input in stored {
        if name::dataset_name_problem(&input.name).is_some() {
            return Err(broken(format!(
                "data.lock says {dataset} was made from {}, which is no dataset's name",
                quoted(&input.name)
            )));
        }
        let tree = match input.tree.as_str() {
            UNKNOWN => None,
            text => Some(prefixed(text).ok_or_else(|| {
                broken(format!(
                    "data.lock gives {} in the from of {dataset} a tree that is neither sha256: and 64 hex digits nor unknown",
                    input.name
                ))
            })?),
        };
        inputs.push(Recorded {
            name: input.name,
            tree,
        });
    }
    inputs.sort_by(|a, b| a.name.cmp(&b.name));
    if let Some(pair) = inputs.windows(2).find(|pair| pair[0].name == pair[1].name) {
        return Err(broken(format!(
            "data.lock says {dataset} was made from {} twice",
            pair[0].name
        )));
    }
    Ok(inputs)
}

/// refuses a lock that would place a file outside its dataset's folder. fetchloom never writes
/// such a path, so the lock was edited or made by someone else.
fn check_paths<'a>(dataset: &str, paths: impl Iterator<Item = &'a String>) -> Result<(), Error> {
    for path in paths {
        if let Some(reason) = name::escapes(path) {
            return Err(broken(format!(
                "data.lock lists {} in {dataset}, which fetchloom refuses because {reason}",
                quoted(path)
            )));
        }
    }
    Ok(())
}

/// refuses a file link carrying a login or a secret: fetchloom never pins one, so the lock was
/// edited, and a download would send it on. a listed link's help is to restore from git.
fn check_links<'a>(
    dataset: &str,
    files: impl Iterator<Item = (&'a String, &'a String)>,
    is_listed: bool,
) -> Result<(), Error> {
    // parsing each of a million links doubled the time a lock took to read. links mostly differ
    // in the values of the same keys, so a cleared link's keys are reused where keys alone decide
    let mut cleared: Option<Vec<&str>> = None;
    for (path, url) in files {
        let text = url.as_str();
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
                lists::recovery()
            } else {
                carried.mend()
            };
            let subject = format!("the link data.lock gives {dataset}/{path}");
            return Err(carried.refused(&subject).help(help));
        }
        if is_plain && !raw_keys(text).any(crate::shown::depends_on_path) {
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
    /// the key of the query or fragment value [`crate::shown::secret`] names, such as `token`.
    Secret(String),
}

impl Carried {
    fn by(url: &Url) -> Option<Carried> {
        if !url.username().is_empty() || url.password().is_some() {
            return Some(Carried::Login);
        }
        crate::shown::secret(url).map(Carried::Secret)
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
    /// the datasets as a reader gives them when every source lists its files inline, by name.
    fn parsed(&self) -> Vec<Parsed> {
        let mut parsed: Vec<Parsed> = self
            .datasets
            .iter()
            .map(|dataset| Parsed::Source(dataset.clone(), None))
            .collect();
        parsed.sort_by(|a, b| a.name().cmp(b.name()));
        parsed
    }
}

#[cfg(test)]
mod tests {
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
                    decompress: false,
                    web_page: false,
                    resolved: "https://example.org/z.csv".to_owned(),
                    title: None,
                    license: None,
                    doi: None,
                    access: Access::Unknown,
                    timestamp: "2026-10-27T17:30:00Z".to_owned(),
                    tree: None,
                    from: Vec::new(),
                    files: vec![file("z.csv", b"z")],
                },
                Locked {
                    name: "iris".to_owned(),
                    reference: "zenodo:3608990".to_owned(),
                    spec: "sha256:11".to_owned(),
                    extract: true,
                    decompress: false,
                    web_page: false,
                    resolved: "zenodo:3608990".to_owned(),
                    title: Some("Iris \"classic\"\tset".to_owned()),
                    license: Some("CC-BY-4.0".to_owned()),
                    doi: Some("10.5281/zenodo.3608990".to_owned()),
                    access: Access::Public,
                    timestamp: "2026-10-27T17:30:00Z".to_owned(),
                    tree: None,
                    from: Vec::new(),
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

    /// `mnist`, pinned by what its source states: an MD5 alone, a SHA-256 alone, and both.
    fn stated() -> Stated {
        let file = |path: &str, sha256: Option<&[u8]>, md5: Option<&str>| StatedFile {
            path: path.to_owned(),
            size: 7,
            sha256: sha256.map(digest::sha256),
            md5: md5.and_then(Md5::from_hex),
            url: format!("https://zenodo.org/api/records/1/files/{path}/content"),
        };
        let pin = Locked {
            name: "mnist".to_owned(),
            reference: "zenodo:1".to_owned(),
            spec: "sha256:22".to_owned(),
            extract: true,
            decompress: false,
            web_page: false,
            resolved: "zenodo:1".to_owned(),
            title: Some("MNIST".to_owned()),
            license: None,
            doi: None,
            access: Access::Public,
            timestamp: "2026-11-15T07:00:00Z".to_owned(),
            tree: None,
            from: Vec::new(),
            files: Vec::new(),
        };
        Stated {
            written: Words::of(&pin),
            pin,
            files: vec![
                file("train.zip", None, Some("d41d8cd98f00b204e9800998ecf8427e")),
                file("labels.csv", Some(b"labels"), None),
                file(
                    "test.csv",
                    Some(b"test"),
                    Some("0cc175b9c0f1b6a831c399e269772661"),
                ),
            ],
        }
    }

    /// [`sample`] with [`stated`], as a lock keeps them.
    fn with_stated() -> Lock {
        let mut lock = sample_sorted();
        let mut stated = stated();
        stated.files.sort_by(|a, b| a.path.cmp(&b.path));
        lock.stated.push(stated);
        lock
    }

    /// a dataset not downloaded yet is written inline with the digests its source states, in a
    /// downloaded file's order of keys, and every reader gives it back as written.
    #[test]
    fn a_dataset_not_downloaded_round_trips_through_every_reader() {
        let lock = with_stated();
        let text = lock.render();
        let expected = format!(
            "\n[[dataset]]\nname = \"mnist\"\nref = \"zenodo:1\"\nspec = \"sha256:22\"\nresolved = \"zenodo:1\"\ntitle = \"MNIST\"\naccess = \"public\"\nlocked = \"2026-11-15T07:00:00Z\"\ndownloaded = false\nfiles = [\n  {{ path = \"labels.csv\", size = 7, sha256 = \"{}\", url = \"https://zenodo.org/api/records/1/files/labels.csv/content\" }},\n  {{ path = \"test.csv\", size = 7, sha256 = \"{}\", md5 = \"0cc175b9c0f1b6a831c399e269772661\", url = \"https://zenodo.org/api/records/1/files/test.csv/content\" }},\n  {{ path = \"train.zip\", size = 7, md5 = \"d41d8cd98f00b204e9800998ecf8427e\", url = \"https://zenodo.org/api/records/1/files/train.zip/content\" }},\n]\n",
            digest::sha256(b"labels"),
            digest::sha256(b"test")
        );
        assert!(text.contains(&expected), "{text}");
        assert!(
            text.find("name = \"iris\"") < text.find("name = \"mnist\"")
                && text.find("name = \"mnist\"") < text.find("name = \"zeta\""),
            "tables of every kind go by name"
        );
        assert!(
            lock.lists_with(0)
                .iter()
                .all(|(part, _)| !part.contains("mnist"))
        );
        for read in every_form(&lock) {
            let read = read.unwrap();
            assert_eq!(read.stated, lock.stated);
            assert_eq!(read.stated("mnist").unwrap().digests(), "SHA-256 and MD5");
            assert!(
                read.get("mnist").is_none(),
                "no command takes it for a downloaded one"
            );
        }
        assert_eq!(Lock::parse(&text).unwrap().render(), text);
        let kept = Lock::read_waiting(&text, Path::new("."), &|name| name == "zeta").unwrap();
        assert_eq!(kept.stated, []);
        let only = Lock::read_waiting(&text, Path::new("."), &|name| name == "mnist").unwrap();
        assert_eq!(only.stated, lock.stated);
    }

    /// a cloned lock is untrusted: an entry not downloaded yet gets every check another gets, and
    /// those only its kind needs.
    #[test]
    fn a_hostile_dataset_not_downloaded_is_refused_by_every_reader() {
        let text = with_stated().render();
        let md5 = "md5 = \"d41d8cd98f00b204e9800998ecf8427e\"";
        for (changed, says) in [
            (
                text.replacen("path = \"train.zip\"", "path = \"../train.zip\"", 1),
                "data.lock lists \"../train.zip\" in mnist, which fetchloom refuses",
            ),
            (
                text.replacen(&format!(", {md5}"), "", 1),
                "the file \"train.zip\" of mnist, which is not downloaded yet, neither a sha256 nor an md5",
            ),
            (
                text.replacen(md5, &format!("{md5}, unpacks_to = \"sha256:{}\"", digest::sha256(b"t")), 1),
                "what it unpacks to, which only its bytes tell",
            ),
            (
                text.replacen("train.zip/content\"", "train.zip/content?token=canary\"", 1),
                "the link data.lock gives mnist/train.zip carries token",
            ),
            (
                text.replacen("path = \"test.csv\"", "path = \"labels.csv\"", 1),
                "data.lock lists labels.csv twice in mnist",
            ),
            (
                text.replacen(
                    "downloaded = false\nfiles = [",
                    &format!(
                        "downloaded = false\nfiles_in = [\"data.lock.d/mnist.tsv.zst\"]\nfile_count = 1\nfile_bytes = 7\nfiles_sha256 = \"sha256:{}\"\nfiles = [",
                        digest::sha256(b"list")
                    ),
                    1,
                ),
                "data.lock keeps the files of mnist, which is not downloaded yet, in data.lock.d",
            ),
        ] {
            let error = Lock::parse(&changed).unwrap_err();
            assert_eq!(error.kind, Kind::ProjectInvalid, "{says}");
            assert!(error.message.contains(says), "{says}: {}", error.message);
            // links are checked once either reader is done
            if !says.contains("carries") {
                assert_eq!(canonical::read(&changed, false), None, "{changed}");
            }
            assert!(!format!("{error:?}").contains("canary"), "{error:?}");
        }
        let error = check_stated_count(&stated(), 2).unwrap_err();
        assert_eq!(
            error.message,
            "data.lock gives mnist, which is not downloaded yet, 3 files, more than the 2 fetchloom writes"
        );
        assert!(check_stated_count(&stated(), 3).is_ok());
        let parsed = parse(&text).unwrap();
        // iris's 2, zeta's 1 and mnist's 3
        assert!(fits(&parsed, BASE + 6 * PER_FILE).is_ok());
        assert!(fits(&parsed, BASE + 6 * PER_FILE - 1).is_err());
    }

    /// a title with control characters is shown escaped, but written back byte for byte, so
    /// data.lock's bytes never depend on which command wrote them.
    #[test]
    fn a_dataset_not_downloaded_keeps_its_words_as_written() {
        let mut lock = with_stated();
        lock.stated[0].pin.title = Some("MNIST\u{1b}]0;x\u{7}".to_owned());
        lock.stated[0].written.title = lock.stated[0].pin.title.clone();
        let text = lock.render();
        assert!(
            text.contains("title = \"MNIST\\u001B]0;x\\u0007\"\n"),
            "{text}"
        );
        for read in every_form(&lock) {
            let read = read.unwrap();
            let mnist = read.stated("mnist").unwrap();
            assert_eq!(mnist.pin.title.as_deref(), Some("MNIST\\u{1b}]0;x\\u{7}"));
            assert_eq!(mnist.written.title.as_deref(), Some("MNIST\u{1b}]0;x\u{7}"));
            assert_eq!(read.render(), text);
        }
    }

    /// a command that needs a dataset's files refuses one only its source's digests pin, saying
    /// which sync downloads it.
    #[test]
    fn a_dataset_not_downloaded_is_refused_where_its_files_are_needed() {
        let lock = with_stated();
        assert!(lock.refuse_stated(["iris", "zeta"]).is_ok());
        let error = lock.refuse_stated(["iris", "mnist"]).unwrap_err();
        assert_eq!(error.kind, Kind::NotReady);
        assert_eq!(
            error.message,
            "mnist is pinned by its source's SHA-256 and MD5 until a sync downloads it"
        );
        assert_eq!(
            error.help.as_deref(),
            Some("run `fetchloom sync mnist` where the data can download, then commit data.lock")
        );
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
            assert_eq!(error.help.as_deref(), Some(recovery().as_str()));
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
            assert_eq!(error.help.as_deref(), Some(recovery().as_str()));
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
                assert_eq!(error.help.as_deref(), Some(recovery().as_str()));
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
        let update = "update fetchloom, as https://nikhil-yn-aix.github.io/fetchloom/user/start/install/ shows, then run the same command again";
        for text in ["version = 2\n", "version = 2\n[[dataset]]\nwhatever = 1\n"] {
            let newer = Lock::parse(text).unwrap_err();
            assert_eq!(newer.kind, Kind::ProjectInvalid);
            assert_eq!(
                newer.message,
                "data.lock is in format 2, which a newer fetchloom wrote, and this one reads format 1"
            );
            assert_eq!(newer.help.as_deref(), Some(update));
        }
        for text in ["version = 0\n", "version = 1\n[[dataset]]\nname = 3\n"] {
            let broken = Lock::parse(text).unwrap_err();
            assert_eq!(broken.kind, Kind::ProjectInvalid);
            assert_eq!(broken.help.as_deref(), Some(recovery().as_str()), "{text}");
        }
        for (version, message, help) in [
            (
                "4294967296",
                "data.lock is in format 4294967296, which a newer fetchloom wrote, and this one reads format 1",
                update,
            ),
            (
                "-1",
                "data.lock is in format -1, and this fetchloom reads format 1",
                recovery().as_str(),
            ),
        ] {
            let error = Lock::parse(&format!("version = {version}\n")).unwrap_err();
            assert_eq!(error.kind, Kind::ProjectInvalid);
            assert_eq!(error.message, message);
            assert_eq!(error.help.as_deref(), Some(help));
        }
        assert!(
            recovery().contains("delete it and run `fetchloom lock`, which resolves every dataset")
        );
        assert_eq!(Lock::parse("version = 1\n").unwrap(), Lock::default());
    }

    /// a lock the toml reader refuses names the line it stopped at in plain words, as data.toml's
    /// errors do, and keeps the reader's own words as a detail.
    #[test]
    fn an_unreadable_lock_names_its_line() {
        let text = sample().render();
        let cut = &text[..text.rfind('}').unwrap()];
        let error = Lock::parse(cut).unwrap_err();
        let line = cut.lines().count();
        assert_eq!(error.kind, Kind::ProjectInvalid);
        assert_eq!(
            error.message,
            format!("data.lock cannot be read: line {line} is damaged")
        );
        assert_eq!(error.help.as_deref(), Some(recovery().as_str()));
        assert!(
            error.details[0].starts_with("the toml reader says: "),
            "{:?}",
            error.details
        );
        let (before, after) = text.split_once("size = ").unwrap();
        let digits = after.find(|c: char| !c.is_ascii_digit()).unwrap();
        let huge = format!(
            "{before}size = 99999999999999999999999999{}",
            &after[digits..]
        );
        let line = before.lines().count();
        assert_eq!(
            Lock::parse(&huge).unwrap_err().message,
            format!(
                "data.lock cannot be read: line {line} holds a number too large for fetchloom to store"
            )
        );
    }

    /// a field or a value this fetchloom does not know, as a newer fetchloom may write, is no
    /// damage: the error asks for that newer fetchloom, or a restore from git of a hand edit,
    /// never to delete data.lock and pin anew, which would throw its pins away. reading stays
    /// strict.
    #[test]
    fn a_field_a_newer_fetchloom_writes_asks_for_that_fetchloom() {
        let text = sample().render();
        let line = |changed: &str, word: &str| {
            changed[..changed.find(word).unwrap()].matches('\n').count() + 1
        };
        for (changed, word, what) in [
            (
                text.replacen("locked = ", "citation = \"x\"\nlocked = ", 1),
                "citation",
                "field",
            ),
            (
                text.replacen("version = 1\n", "version = 1\nrequires = \"0.2\"\n", 1),
                "requires",
                "field",
            ),
            (
                text.replacen("access = \"public\"", "access = \"embargoed\"", 1),
                "embargoed",
                "value",
            ),
        ] {
            let error = Lock::parse(&changed).unwrap_err();
            assert_eq!(error.kind, Kind::ProjectInvalid, "{changed}");
            assert_eq!(
                error.message,
                format!(
                    "data.lock holds {word} on line {}, a {what} this fetchloom does not know, so a newer fetchloom likely wrote it",
                    line(&changed, word)
                )
            );
            assert_eq!(error.help.as_deref(), Some(NEWER));
        }
        // a field inside a file's table, and one whose name is no plain word, are not named
        let inline = text.replacen(", url = ", ", citation = \"x\", url = ", 1);
        let error = Lock::parse(&inline).unwrap_err();
        assert!(
            error
                .message
                .starts_with("data.lock holds citation on line ")
        );
        let odd = text.replacen("locked = ", "\"cit\\u001b[2Jation\" = \"x\"\nlocked = ", 1);
        let error = Lock::parse(&odd).unwrap_err();
        assert!(
            error
                .message
                .starts_with("data.lock holds a field this fetchloom does not know on line "),
            "{}",
            error.message
        );
        assert!(!NEWER.contains("delete") && !NEWER.contains("fetchloom lock"));
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
            assert_eq!(
                error.help.as_deref(),
                Some(recovery().as_str()),
                "{version}"
            );
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

    /// data.lock as a git commit holds it is text anyone may have written: a part it names that
    /// fetchloom would not write is refused before anything is read for it.
    #[test]
    fn a_lock_read_elsewhere_reads_only_the_parts_fetchloom_writes() {
        let lock = sample_sorted();
        let text = lock.render_with(1);
        let parts = lock.lists_with(1);
        let asked = std::cell::RefCell::new(Vec::new());
        let given = |path: &str| -> Result<Vec<u8>, Error> {
            asked.borrow_mut().push(path.to_owned());
            let (_, part) = parts.iter().find(|(part, _)| part == path).unwrap();
            Ok(lists::compress(part).unwrap())
        };
        let (read, unlisted) = Lock::read_with(&text, &given, &|_| true, 0).unwrap();
        assert_eq!((read, unlisted), (lock, Vec::new()));
        assert_eq!(*asked.borrow(), [parts[0].0.clone()]);
        asked.borrow_mut().clear();
        for elsewhere in ["data.toml", "data.lock.d/../../secret.tsv.zst"] {
            let error =
                Lock::read_with(&text.replace(&parts[0].0, elsewhere), &given, &|_| false, 0)
                    .unwrap_err();
            assert_eq!(error.kind, Kind::ProjectInvalid, "{elsewhere}");
            assert!(
                error.message.ends_with("in files fetchloom does not write"),
                "{}",
                error.message
            );
        }
        assert!(asked.borrow().is_empty(), "{:?}", asked.borrow());
    }

    /// a commit made without the list of a dataset keeps that dataset, with no files, named as
    /// such, and the rest of the lock is read; a lock read beside another fits the room with it.
    #[test]
    fn a_lock_read_elsewhere_keeps_a_dataset_whose_list_is_missing() {
        let lock = sample_sorted();
        let text = lock.render_with(1);
        let parts = lock.lists_with(1);
        let given = |path: &str| -> Result<Vec<u8>, Error> {
            let (_, part) = parts.iter().find(|(part, _)| part == path).unwrap();
            Ok(lists::compress(part).unwrap())
        };
        let (whole, _) = Lock::read_with(&text, &given, &|_| true, 0).unwrap();
        let (read, unlisted) = Lock::read_with(&text, &given, &|_| false, 0).unwrap();
        assert_eq!(unlisted, ["iris"]);
        let kept = read.get("iris").unwrap();
        assert_eq!(kept.files, []);
        assert_eq!(kept.resolved, whole.get("iris").unwrap().resolved);
        assert_eq!(read.get("zeta"), whole.get("zeta"));
        let error = Lock::read_with(&text, &given, &|_| true, u64::MAX).unwrap_err();
        assert!(
            error.details[0].starts_with("the other data.lock, read already, takes about "),
            "{error:?}"
        );
    }

    /// the datasets left out have their file lines passed over unread where no string among them
    /// could hide a line of the lock.
    #[test]
    fn a_read_of_some_datasets_reads_them_as_a_whole_read_does() {
        let mut lock = sample();
        let (root, text) = written(&lock);
        let whole = Lock::read(&text, root.path()).unwrap();
        for keep in [&["iris"][..], &["zeta"], &[]] {
            let read =
                Lock::read_waiting(&text, root.path(), &|name| keep.contains(&name)).unwrap();
            assert_eq!(read.waiting, []);
            let kept: Vec<&Locked> = whole
                .datasets
                .iter()
                .filter(|dataset| keep.contains(&dataset.name.as_str()))
                .collect();
            assert_eq!(read.datasets.iter().collect::<Vec<_>>(), kept);
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

    /// a lock's text is held against the room before it is parsed, since the text and what it
    /// parses into take twice its size at least: one too big says so before any parsing, which
    /// text that does not read as a lock shows.
    #[test]
    fn a_lock_text_too_big_for_its_room_is_refused_before_it_is_parsed() {
        let text = sample_sorted().render_with(1);
        let need = 2 * text.len() as u64;
        let all = |_: &str| true;
        assert!(parse_within(&text, &all, need).is_ok());
        let garbage = "x".repeat(text.len());
        let error = parse_within(&garbage, &all, need - 1).unwrap_err();
        assert_eq!(error.kind, Kind::LocalIo);
        assert!(
            error
                .message
                .starts_with("reading this project's data.lock, which is "),
            "{}",
            error.message
        );
        assert_eq!(
            parse_within(&garbage, &all, need).unwrap_err().kind,
            Kind::ProjectInvalid
        );
    }

    /// a read of a lock beside one read already holds its text to the room the other leaves, and
    /// a read of some datasets holds it to the room as a whole read does, both before parsing.
    #[test]
    fn a_lock_text_fits_beside_a_lock_read_already_and_in_a_named_read() {
        let root = tempfile::tempdir().unwrap();
        let garbage = "x".repeat(1000);
        let none = |_: &str| -> Result<Vec<u8>, Error> { Err(broken("no part".to_owned())) };
        let error = Lock::read_with(&garbage, &none, &|_| true, u64::MAX - 1999).unwrap_err();
        assert_eq!(error.kind, Kind::LocalIo, "{error:?}");
        assert!(
            error.details[0].starts_with("the other data.lock, read already, takes about "),
            "{error:?}"
        );
        assert_eq!(
            Lock::read_with(&garbage, &none, &|_| true, u64::MAX - 2000)
                .unwrap_err()
                .kind,
            Kind::ProjectInvalid
        );
        let error = Lock::read_waiting_within(&garbage, root.path(), &|_| true, 1999).unwrap_err();
        assert_eq!(error.kind, Kind::LocalIo, "{error:?}");
        assert_eq!(
            error.help.as_deref(),
            Some("raise the container's memory limit by 1 B or more")
        );
    }

    /// a list's part holds one file at least, so a lock naming more parts than its files fill is
    /// refused as it is read, before the names fetchloom would write are made to compare.
    #[test]
    fn a_list_of_more_parts_than_files_is_refused_as_it_is_read() {
        let text = sample_sorted().render_with(1);
        let named = parts(&text);
        let many = format!("{0}\", \"{0}\", \"{0}", named[0]);
        let error = parse(&text.replace(&named[0], &many)).unwrap_err();
        assert_eq!(error.kind, Kind::ProjectInvalid);
        assert_eq!(
            error.message,
            "data.lock keeps the files of iris in 3 parts, more than fetchloom writes for 2 files"
        );
    }

    /// the parts a lock names are only those fetchloom writes for each list, as a read takes them,
    /// so another path in a cloned lock never reaches git or a folder as a part.
    #[test]
    fn parts_are_only_the_names_fetchloom_writes() {
        let text = sample_sorted().render_with(1);
        let named = parts(&text);
        assert_eq!(named.len(), 1, "{named:?}");
        assert!(named[0].starts_with("data.lock.d/iris."), "{named:?}");
        let elsewhere = text.replace(&named[0], "../outside.tsv.zst");
        assert_eq!(parts(&elsewhere), Vec::<String>::new());
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

    /// a list is weighed as it is read: a row whose path and link run past what [`PER_FILE`]
    /// holds takes the rest twice, so a cloned list of long rows, small once compressed, is
    /// refused within the container's room rather than filling it.
    #[test]
    fn a_list_of_long_rows_is_weighed_against_the_room() {
        let mut lock = sample_sorted();
        for file in &mut lock.datasets[0].files {
            file.url = format!("https://example.org/{}", "a".repeat(60 << 10));
        }
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join(lists::FOLDER)).unwrap();
        for (part, text) in lock.lists_with(1) {
            std::fs::write(root.path().join(part), lists::compress(&text).unwrap()).unwrap();
        }
        let text = lock.render_with(1);
        // iris's two files in its list and zeta's one, as the count alone weighs them
        let counted = BASE + 3 * PER_FILE;
        let read = |room| Lock::read_waiting_within(&text, root.path(), &|_| true, room);
        let error = read(counted + (100 << 10)).unwrap_err();
        assert_eq!(error.kind, Kind::LocalIo);
        assert!(
            error
                .message
                .starts_with("reading this project's 3 files takes at least "),
            "{}",
            error.message
        );
        assert_eq!(
            error.details,
            ["the file list of iris holds longer paths and links than most"]
        );
        assert_eq!(read(counted + (1 << 20)).unwrap().datasets, lock.datasets);
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

    /// a list that is not there, as in a clone of a commit that left data.lock.d out, leaves its
    /// dataset waiting with its entry as read, written back unchanged, and the others read. a list
    /// named otherwise than fetchloom writes it, a part that is a link and a data.lock.d that is a
    /// link still stop the read.
    #[test]
    fn a_dataset_whose_list_is_not_there_waits_alone() {
        let lock = sample();
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join(lists::FOLDER)).unwrap();
        let parts = lock.lists_with(0);
        for (part, text) in &parts {
            std::fs::write(root.path().join(part), lists::compress(text).unwrap()).unwrap();
        }
        let text = lock.render_with(0);
        let gone = lock.datasets[1].name.clone();
        let part_of = |name: &str| {
            let prefix = format!("{}/{name}.", lists::FOLDER);
            parts
                .iter()
                .map(|(part, _)| part.clone())
                .find(|part| part.starts_with(&prefix))
                .unwrap()
        };
        let part = part_of(&gone);
        std::fs::remove_file(root.path().join(&part)).unwrap();
        let read = Lock::read_waiting(&text, root.path(), &|_| true).unwrap();
        let waiting: Vec<(&str, &str)> = read
            .waiting
            .iter()
            .map(|waiting| (waiting.name.as_str(), waiting.part.as_str()))
            .collect();
        assert_eq!(waiting, [(gone.as_str(), part.as_str())]);
        assert_eq!(read.render_with(0), text);
        assert_eq!(
            read.waits(&gone).unwrap().pin().resolved,
            lock.datasets[1].resolved
        );
        let names = |lock: &Lock| -> Vec<String> {
            lock.datasets
                .iter()
                .map(|locked| locked.name.clone())
                .filter(|name| *name != gone)
                .collect()
        };
        assert_eq!(read.datasets.len(), lock.datasets.len() - 1);
        assert_eq!(names(&read), names(&lock));
        // a pin made again is written in place of the entry that waited
        let mut pinned = read.clone();
        let at = pinned.datasets.partition_point(|known| known.name < gone);
        pinned.datasets.insert(at, lock.datasets[1].clone());
        assert!(pinned.waits(&gone).is_none());
        assert_eq!(pinned.render_with(0), text);
        let renamed = text.replace(part.as_str(), &format!("{}/{gone}.tsv.zst", lists::FOLDER));
        let error = Lock::read_waiting(&renamed, root.path(), &|_| true).unwrap_err();
        assert!(
            error.message.contains("in files fetchloom does not write"),
            "{error:?}"
        );
        #[cfg(unix)]
        {
            let elsewhere = root.path().join("elsewhere");
            std::os::unix::fs::symlink(&elsewhere, root.path().join(part)).unwrap();
            let error = Lock::read_waiting(&text, root.path(), &|_| true).unwrap_err();
            assert!(
                error.message.contains("is a link or a special file"),
                "{error:?}"
            );
            let folder = root.path().join(lists::FOLDER);
            std::fs::rename(&folder, root.path().join("moved")).unwrap();
            std::os::unix::fs::symlink(&elsewhere, &folder).unwrap();
            let error = Lock::read_waiting(&text, root.path(), &|_| true).unwrap_err();
            assert!(error.message.contains("only in a real folder"), "{error:?}");
        }
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
            assert_eq!(err.help.as_deref(), Some(recovery().as_str()), "{path}");
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

    /// every reader takes a downloaded dataset's title, license and doi as data.lock writes them,
    /// control characters too: the output escapes them, as it does all text it did not write.
    #[test]
    fn a_title_license_or_doi_is_read_as_written() {
        let mut lock = sample();
        let iris = lock
            .datasets
            .iter_mut()
            .find(|dataset| dataset.name == "iris")
            .unwrap();
        iris.title = Some("Iris\tset\n\u{1b}]0;x\u{7}".to_owned());
        iris.license = Some("\u{9b}31mCC-BY-4.0".to_owned());
        iris.doi = Some("10.5281/\u{202e}zenodo".to_owned());
        for read in every_form(&lock) {
            let read = read.unwrap();
            let iris = read.get("iris").unwrap();
            assert_eq!(iris.title.as_deref(), Some("Iris\tset\n\u{1b}]0;x\u{7}"));
            assert_eq!(iris.license.as_deref(), Some("\u{9b}31mCC-BY-4.0"));
            assert_eq!(iris.doi.as_deref(), Some("10.5281/\u{202e}zenodo"));
        }
    }

    /// a downloaded dataset's title, license and doi are shown escaped, but written back byte
    /// for byte when data.lock is written again for another dataset, as a not-downloaded one's
    /// are, so its bytes never depend on which command wrote them.
    #[test]
    fn a_downloaded_dataset_keeps_its_words_as_written() {
        let mut lock = sample();
        let iris = lock
            .datasets
            .iter_mut()
            .find(|dataset| dataset.name == "iris")
            .unwrap();
        iris.title = Some("Iris\u{1b}]0;x\u{7}".to_owned());
        iris.license = Some("\u{9b}31mCC-BY-4.0".to_owned());
        iris.doi = Some("10.5281/\u{202e}zenodo".to_owned());
        let text = lock.render();
        for read in every_form(&lock) {
            let read = read.unwrap();
            let iris = read.get("iris").unwrap();
            assert_eq!(iris.title.as_deref(), Some("Iris\u{1b}]0;x\u{7}"));
            assert_eq!(read.render(), text);
        }
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
                    Some(
                        if listed {
                            lists::recovery()
                        } else {
                            help.to_owned()
                        }
                        .as_str()
                    )
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
        let plain = spec(&entry("").source);
        assert_eq!(
            plain,
            spec(&entry("copy = true\npath = \"elsewhere\"\n").source),
            "copy and path do not change what is downloaded"
        );
        assert_ne!(plain, spec(&entry("select = \"*.csv\"\n").source));
        assert_eq!(
            plain,
            spec(&entry("extract = false\n").source),
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
        // a value a newer fetchloom may write, which a hand edit's restore from git mends too
        let error = Lock::parse(&text.replace("\"public\"", "\"secret\"")).unwrap_err();
        assert_eq!(error.kind, Kind::ProjectInvalid);
        assert_eq!(error.help.as_deref(), Some(NEWER));
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
            spec: spec(&entry("").source),
            extract: true,
            decompress: false,
            web_page: false,
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

    /// a dataset that asks for no web page has the spec it has without the key, so its pin stays
    /// up to date; taking `web_page = true` away changes it, and the pin made with it counts for
    /// nothing.
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
        let plain = &plain.source;
        assert_eq!(
            spec(plain),
            format!(
                "sha256:{}",
                digest::sha256(b"ref=https://example.org/notes\nselect=*\n")
            )
        );
        let asked = entry("select = \"*\"\nweb_page = true\n");
        let asked = &asked.source;
        let pinned = |source: &Source| Locked {
            spec: spec(source),
            web_page: source.web_page,
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

    /// a pin made while data.toml asked for a page says so in data.lock, written only then, so a
    /// relock that no longer asks never carries it, whatever else changed with it.
    #[test]
    fn a_page_pin_is_marked_and_never_carried_once_unasked() {
        let entry = |extra: &str| {
            Manifest::parse(&format!(
                "[datasets.x]\nref = \"https://example.org/notes\"\n{extra}"
            ))
            .unwrap()
            .datasets["x"]
                .clone()
        };
        let asked = entry("web_page = true\n");
        let mut pinned = Locked {
            spec: spec(&asked.source),
            web_page: true,
            ..sample().datasets[1].clone()
        };
        pinned.files.sort_by(|a, b| a.path.cmp(&b.path));
        for unasked in ["", "exclude = \"*.csv\"\n", "select = \"notes\"\n"] {
            let unasked = entry(unasked);
            assert!(
                page_no_longer_asked(&pinned, &unasked.source),
                "{unasked:?}"
            );
        }
        let still = entry("web_page = true\nexclude = \"*.csv\"\n");
        assert!(!page_no_longer_asked(&pinned, &still.source));
        let lock = Lock {
            datasets: vec![pinned.clone()],
            ..sample()
        };
        let text = lock.render();
        assert!(
            text.contains("spec = \"sha256:") && text.contains("\nweb_page = true\nresolved = "),
            "{text}"
        );
        assert_eq!(text.matches("web_page").count(), 1, "{text}");
        assert_eq!(Lock::parse(&text).unwrap().datasets, [pinned]);
        assert_eq!(
            canonical::read(&text, false),
            Some(parse_toml(&text).unwrap())
        );
        assert!(!sample().render().contains("web_page"));
    }

    #[test]
    fn control_characters_are_escaped() {
        assert_eq!(quoted("a\u{1}b\u{7f}"), "\"a\\u0001b\\u007F\"");
    }

    #[test]
    fn from_reads_back_as_written_and_refuses_what_fetchloom_never_writes() {
        let mut lock = sample_sorted();
        let tree = digest::sha256(b"tree");
        lock.datasets[1].tree = Some(digest::sha256(b"zeta's tree"));
        lock.datasets[0].from = vec![
            Recorded {
                name: "raw".to_owned(),
                tree: Some(tree),
            },
            Recorded {
                name: "zeta".to_owned(),
                tree: None,
            },
        ];
        let text = lock.render();
        let written = format!(
            "locked = \"2026-10-27T17:30:00Z\"\nfrom = [\n  {{ name = \"raw\", tree = \"sha256:{tree}\" }},\n  {{ name = \"zeta\", tree = \"unknown\" }},\n]\nfiles = ["
        );
        assert!(text.contains(&written), "{text}");
        let own = format!(
            "locked = \"2026-10-27T17:30:00Z\"\ntree = \"sha256:{}\"\nfiles = [",
            digest::sha256(b"zeta's tree")
        );
        assert!(text.contains(&own), "{text}");
        assert_eq!(Lock::parse(&text).unwrap(), lock);
        let unreadable = text.replace(
            &own,
            "locked = \"2026-10-27T17:30:00Z\"\ntree = \"sha256:12\"\nfiles = [",
        );
        assert_eq!(
            Lock::parse(&unreadable).unwrap_err().message,
            "data.lock gives zeta a tree that is not sha256: and 64 hex digits"
        );
        let swapped = text.replace(
            &written,
            &format!(
                "locked = \"2026-10-27T17:30:00Z\"\nfrom = [{{ name = \"zeta\", tree = \"unknown\" }}, {{ name = \"raw\", tree = \"sha256:{tree}\" }}]\nfiles = ["
            ),
        );
        assert_ne!(swapped, text);
        assert_eq!(
            Lock::parse(&swapped).unwrap(),
            lock,
            "any order reads sorted"
        );
        for (from, message) in [
            (
                "[{ name = \"Raw Data\", tree = \"unknown\" }]",
                "data.lock says iris was made from \"Raw Data\", which is no dataset's name",
            ),
            (
                "[{ name = \"raw\", tree = \"sha256:12\" }]",
                "data.lock gives raw in the from of iris a tree that is neither sha256: and 64 hex digits nor unknown",
            ),
            (
                "[{ name = \"raw\", tree = \"unknown\" }, { name = \"raw\", tree = \"unknown\" }]",
                "data.lock says iris was made from raw twice",
            ),
        ] {
            let edited = text.replace(
                &written,
                &format!("locked = \"2026-10-27T17:30:00Z\"\nfrom = {from}\nfiles = ["),
            );
            let error = Lock::parse(&edited).unwrap_err();
            assert_eq!(error.kind, Kind::ProjectInvalid);
            assert_eq!(error.message, message);
        }
    }
}
