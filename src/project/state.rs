//! `.fetchloom/state`: every file fetchloom placed and how it looked right after, so `status` and
//! `sync` notice hand edits without hashing, but for a file as new as the state, which a coarse
//! clock cannot vouch for.
//!
//! its records come in path order, in blocks of about [`BLOCK`] bytes indexed at its end, so a
//! command that names a dataset reads only the blocks that can hold its folder's paths.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::Metadata;
use std::io::{self, Read, Seek, SeekFrom};
use std::ops::Range;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::digest::Sha256;
use crate::place::Method;
use crate::project::manifest::Manifest;

const VERSION: u32 = 3;

/// a block of records ends once it holds this many bytes, so a read within a folder reads
/// about this much beyond the records it keeps.
const BLOCK: usize = 64 << 10;

/// how every state starts, up to the number `at` holds: where the records end.
const HEAD: &[u8] = br#"{"version":3,"at":"#;

/// room for `at`: the twenty digits of the largest 64-bit number. the writer pads a shorter one
/// with spaces, which json allows after a number.
const SLOT: usize = 20;

const FILES: &[u8] = br#","files":{"#;

/// where the first record starts.
const START: usize = HEAD.len() + SLOT + FILES.len();

#[derive(Debug, Default, Deserialize)]
pub(crate) struct State {
    version: u32,
    /// keyed by the file's path in the project, with `/` between folders.
    pub(crate) files: BTreeMap<String, Placed>,
    /// data.toml and data.lock as the last sync left them.
    #[serde(default)]
    pub(crate) synced: Option<Synced>,
    /// the datasets `fetchloom drop` freed on this machine, which a plain sync leaves out until a
    /// command that names one brings it back.
    #[serde(default)]
    pub(crate) dropped: BTreeSet<String>,
    /// those of `dropped` a drop marked before any file went and has not freed from the cache yet:
    /// one cut short, which the same drop finishes.
    #[serde(default)]
    pub(crate) freeing: BTreeSet<String>,
    /// the project folder the state was written for, which a clone or a copy is not.
    #[serde(default)]
    pub(crate) folder: Option<Folder>,
    /// files `sync --force` moves or moved out of the way, by the hidden name each is kept under,
    /// with the path it had: written before any is moved, so a run cut off at once leaves each
    /// findable, and kept while one cannot go back, so each sync and status names it.
    #[serde(default)]
    pub(crate) aside: BTreeMap<String, String>,
    /// files fetchloom placed that a sync removes to place the pinned ones, written before it
    /// removes any, so a run cut off at once leaves no later run saying someone deleted them.
    #[serde(default)]
    pub(crate) replacing: BTreeSet<String>,
    /// that a sync found the cache holding none of the objects of this project's copies of cached
    /// files, so a settled sync need not look for them again while nothing it notes has changed.
    #[serde(default)]
    pub(crate) uncached: Option<Uncached>,
    /// the dataset folders fetchloom put a .gitignore of its own in, by path in the project, each
    /// with the SHA-256 of what it last wrote there: recorded before the file is made, so no such
    /// file goes unrecorded, and only one holding that is fetchloom's to rewrite or delete.
    #[serde(default)]
    pub(crate) markers: BTreeMap<String, String>,
    /// what runs told a person once, so later runs say it again only of a new subject.
    #[serde(default)]
    pub(crate) told: Told,
}

/// what a run tells a person once, said again only once its subject changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Once {
    /// an edit sync keeps of a file fetchloom placed, with the ways to keep it: the path and how
    /// the file looks, so a new edit is told of again.
    Edit,
    /// a dataset folder placed again after it went whole, with how to keep it away: the folder.
    Refill,
    /// the data folder placed again after it went whole, as when it was moved: the folder.
    Moved,
    /// a line of the project's .gitignore that hides the whole data folder: the line.
    Rule,
    /// files copied rather than linked, and why: the number of the disk the data folder is on.
    Copy,
    /// a folder the person shares with datasets that put more entries there than a marker names,
    /// so git sees them: the folder.
    Unnamed,
    /// an add into the data folder of an R package, which R keeps for its own datasets: the folder.
    RPackage,
}

impl Once {
    fn kind(self) -> &'static str {
        match self {
            Once::Edit => "edit",
            Once::Refill => "refill",
            Once::Moved => "moved",
            Once::Rule => "rule",
            Once::Copy => "copy",
            Once::Unnamed => "unnamed",
            Once::RPackage => "r_package",
        }
    }
}

/// what runs told once, each as its kind and its subject, as `refill:data/iris`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct Told(BTreeSet<String>);

impl Told {
    /// whether a run told `once` of `subject`.
    pub(crate) fn has(&self, once: Once, subject: &str) -> bool {
        self.0.contains(&format!("{}:{subject}", once.kind()))
    }

    /// records that a run told `once` of `subject`; whether none had.
    pub(crate) fn note(&mut self, once: Once, subject: &str) -> bool {
        self.0.insert(format!("{}:{subject}", once.kind()))
    }

    /// forgets each subject told of as `once` that `keep` declines; whether any went.
    pub(crate) fn retain(&mut self, once: Once, keep: impl Fn(&str) -> bool) -> bool {
        let before = self.0.len();
        let kind = once.kind();
        self.0.retain(|key| {
            key.strip_prefix(kind)
                .and_then(|rest| rest.strip_prefix(':'))
                .is_none_or(&keep)
        });
        self.0.len() != before
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// what a sync noted when it found a cache holding none of the objects of a project's copies of
/// cached files. while the cache, the copies and the folders their objects would sit in are as
/// noted, the cache took none of those objects since: placing one changes its folder's time.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Uncached {
    /// the cache's folder.
    pub(crate) cache: String,
    /// the copies, summed up from their SHA-256 in path order.
    pub(crate) copies: u64,
    /// the newest change of the folders their objects would sit in, in nanoseconds since 1970.
    pub(crate) newest: u64,
}

/// which folder a state was written for: its file number, the inode on unix and the file index
/// on windows, and its creation time where the disk keeps one finer than the second. a rename or a
/// move within the disk keeps the number, and a move on fat or exfat, which keep no lasting
/// numbers, keeps the time. hfs+ keeps whole seconds, which a copy made in the same second shares,
/// so its time is not kept. the disk's own number is left out, since network disks number it apart
/// on each machine. a clone, a copy or a state from git matches neither, so the records of a state
/// that came with it, which anyone could have written, are not used.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Folder {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    inode: Option<u64>,
    /// nanoseconds since 1970.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    created: Option<u64>,
}

impl Folder {
    /// the folder at `root`, none when the system can tell neither its number nor its creation
    /// time.
    pub(crate) fn of(root: &std::path::Path) -> Option<Folder> {
        let metadata = std::fs::metadata(root).ok();
        let created = metadata
            .as_ref()
            .and_then(|metadata| metadata.created().ok())
            .and_then(|created| created.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|since| u64::try_from(since.as_nanos()).ok())
            .filter(|_| crate::space::Filesystem::of(root) != crate::space::Filesystem::Hfs);
        #[cfg(unix)]
        let inode = metadata.as_ref().map(std::os::unix::fs::MetadataExt::ino);
        #[cfg(windows)]
        let inode = winapi_util::Handle::from_path_any(root)
            .and_then(|handle| winapi_util::file::information(&handle))
            .ok()
            .map(|information| information.file_index());
        #[cfg(not(any(unix, windows)))]
        let inode = None;
        (inode.is_some() || created.is_some()).then_some(Folder { inode, created })
    }

    /// whether a state written for this folder is one for `here`: the same number, or the same
    /// creation time. what either side cannot tell matches nothing.
    pub(crate) fn matches(&self, here: &Folder) -> bool {
        let same =
            |recorded: Option<u64>, found: Option<u64>| recorded.is_some() && recorded == found;
        same(self.inode, here.inode) || same(self.created, here.created)
    }
}

/// the SHA-256 of data.toml and data.lock as a sync left them, and whether that sync left every
/// file they ask for in place. while both stay the same byte for byte after a complete sync, the
/// files recorded here are exactly the files they ask for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Synced {
    pub(crate) manifest: Sha256,
    pub(crate) lock: Sha256,
    pub(crate) complete: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Placed {
    pub(crate) dataset: String,
    pub(crate) sha256: Sha256,
    pub(crate) method: Method,
    pub(crate) look: Look,
    /// a file of its own that may be written to, as `copy = true` asks.
    pub(crate) writable: bool,
    /// the start of the scan that last placed, adopted or vouched for the file, in nanoseconds
    /// since 1970. marks written while placing other files still put it in doubt.
    pub(crate) checked: u64,
    /// whether the file was already at its place with the pinned bytes before fetchloom took it
    /// as placed, so a removal of it can say so.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) adopted: bool,
    /// whether a sync found this file, a copy of its own, cannot be linked to the cache's copy:
    /// reading it found other bytes than the pinned ones, or the disk refused the link. no later
    /// sync reads it or tries again while its record stays.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) unlinkable: bool,
    /// this survey's intact racy hash, kept inline rather than in another set of paths.
    #[serde(skip)]
    pub(crate) racy_intact: OnceLock<()>,
}

/// how a file looks on disk without reading it: its size and modification time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Look {
    pub(crate) size: u64,
    /// nanoseconds since 1970, as the disk reports them.
    pub(crate) modified: u64,
}

impl State {
    /// whether this state was read from a file of this version, rather than standing for none.
    pub(crate) fn was_read(&self) -> bool {
        self.version == VERSION
    }

    /// the state on disk. an unreadable state counts as empty: files then look unplaced, and
    /// fetchloom hashes them before it trusts or replaces any.
    pub(crate) fn parse(bytes: &[u8]) -> State {
        match serde_json::from_slice::<State>(bytes) {
            Ok(state) if state.version == VERSION => state,
            _ => State::default(),
        }
    }

    /// [`State::parse`] keeping only the records of the paths `keep` accepts. the others are
    /// passed over without being built, so a few files of a large state cost little to read.
    #[cfg(test)]
    pub(crate) fn parse_only(bytes: &[u8], keep: &dyn Fn(&str) -> bool) -> State {
        let mut deserializer = serde_json::Deserializer::from_slice(bytes);
        let state = serde::de::DeserializeSeed::deserialize(Only(keep), &mut deserializer)
            .and_then(|state| deserializer.end().map(|()| state));
        match state {
            Ok(state) if state.version == VERSION => state,
            _ => State::default(),
        }
    }

    /// the state in `file` with only the records `keep` accepts, passing the others over without
    /// building them, reading only the parts that can hold a record at or below `folders`, with
    /// how many bytes it read; a state whose index does not fit is read whole, so
    /// the records are always a whole read's. one `ours` refuses by the folder it was written
    /// for, as a clone or a copy brings it, gives none of its records. fails when `file` cannot
    /// be read.
    pub(crate) fn read_within(
        file: &mut (impl Read + Seek),
        folders: &[String],
        keep: &dyn Fn(&str) -> bool,
        ours: &dyn Fn(Option<&Folder>) -> bool,
    ) -> io::Result<(State, u64)> {
        let mut reading = Reading { file, bytes: 0 };
        match indexed(&mut reading, folders, keep, ours) {
            Ok(state) => return Ok((state, reading.bytes)),
            Err(reason) => tracing::debug!(reason, "the state is read whole"),
        }
        reading.streamed(keep, ours)
    }

    /// [`State::parse`] of `file`, `length` bytes from its start, with how many bytes it read. a
    /// state of one block is one read; a larger one is read [`State::read_blocks`].
    pub(crate) fn read_whole(
        file: &mut (impl Read + Seek),
        length: u64,
        ours: &dyn Fn(Option<&Folder>) -> bool,
    ) -> io::Result<(State, u64)> {
        // through the index, its header, tail and block would take three reads and three seeks
        if length > BLOCK as u64 {
            return State::read_blocks(file, ours);
        }
        let mut bytes = vec![0; usize::try_from(length).unwrap_or_default()];
        let mut filled = 0;
        while filled < bytes.len() {
            match file.read(&mut bytes[filled..]) {
                Ok(0) => break,
                Ok(read) => filled += read,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(err) => return Err(err),
            }
        }
        bytes.truncate(filled);
        Ok((State::parse(&bytes), filled as u64))
    }

    /// [`State::parse`] of `file`, read a block at a time through its index, so the document is
    /// never held whole, with how many bytes it read; a state whose index does not fit is read
    /// whole, and one `ours` refuses gives none of its records.
    fn read_blocks(
        file: &mut (impl Read + Seek),
        ours: &dyn Fn(Option<&Folder>) -> bool,
    ) -> io::Result<(State, u64)> {
        let mut reading = Reading { file, bytes: 0 };
        match by_blocks(&mut reading, ours) {
            Ok(state) => return Ok((state, reading.bytes)),
            Err(reason) => tracing::debug!(reason, "the state is read whole"),
        }
        reading.streamed(&|_| true, ours)
    }

    /// drops the mark of each dataset `manifest` no longer has, and says whether any went.
    pub(crate) fn forget_removed(&mut self, manifest: &Manifest) -> bool {
        let before = (self.dropped.len(), self.freeing.len());
        // a dataset added again under a dropped one's name starts out placed
        self.dropped
            .retain(|name| manifest.datasets.contains_key(name));
        self.freeing.retain(|name| self.dropped.contains(name));
        (self.dropped.len(), self.freeing.len()) != before
    }

    /// [`State::render_for`] the folder the state records, for tests.
    #[cfg(test)]
    pub(crate) fn render(&self) -> Vec<u8> {
        self.render_for(self.folder.as_ref())
    }

    /// the state as one json document, written for the project folder `folder`: the records in
    /// path order, then where each block of them lies in the document, which
    /// [`State::read_within`] reads by.
    pub(crate) fn render_for(&self, folder: Option<&Folder>) -> Vec<u8> {
        self.render_in(BLOCK, folder)
    }

    /// [`State::render_for`] with blocks of at least `block` bytes, the last one aside.
    fn render_in(&self, block: usize, folder: Option<&Folder>) -> Vec<u8> {
        // where serde_json::to_vec starts: doubling from 128 ends a large state's buffer at a
        // power of two, as large as before the state had an index
        let mut bytes = Vec::with_capacity(128);
        match self.encode(&mut bytes, block, folder) {
            Ok(()) => bytes,
            Err(_) => Vec::new(),
        }
    }

    fn encode(
        &self,
        out: &mut Vec<u8>,
        block: usize,
        folder: Option<&Folder>,
    ) -> serde_json::Result<()> {
        out.extend_from_slice(HEAD);
        out.resize(out.len() + SLOT, b' ');
        out.extend_from_slice(FILES);
        let mut blocks: Vec<(&str, usize, usize)> = Vec::new();
        let mut open = None;
        for (index, (path, placed)) in self.files.iter().enumerate() {
            if index > 0 {
                out.push(b',');
            }
            let (first, start) = *open.get_or_insert((path.as_str(), out.len()));
            serde_json::to_writer(&mut *out, path)?;
            out.push(b':');
            serde_json::to_writer(&mut *out, placed)?;
            if out.len() - start >= block {
                blocks.push((first, start, out.len()));
                open = None;
            }
        }
        blocks.extend(open.map(|(first, start)| (first, start, out.len())));
        out.push(b'}');
        let at = out.len().to_string();
        if let Some(synced) = &self.synced {
            out.extend_from_slice(br#","synced":"#);
            serde_json::to_writer(&mut *out, synced)?;
        }
        if !self.dropped.is_empty() {
            out.extend_from_slice(br#","dropped":"#);
            serde_json::to_writer(&mut *out, &self.dropped)?;
        }
        if !self.freeing.is_empty() {
            out.extend_from_slice(br#","freeing":"#);
            serde_json::to_writer(&mut *out, &self.freeing)?;
        }
        if let Some(folder) = folder {
            out.extend_from_slice(br#","folder":"#);
            serde_json::to_writer(&mut *out, folder)?;
        }
        if !self.aside.is_empty() {
            out.extend_from_slice(br#","aside":"#);
            serde_json::to_writer(&mut *out, &self.aside)?;
        }
        if !self.replacing.is_empty() {
            out.extend_from_slice(br#","replacing":"#);
            serde_json::to_writer(&mut *out, &self.replacing)?;
        }
        if let Some(uncached) = &self.uncached {
            out.extend_from_slice(br#","uncached":"#);
            serde_json::to_writer(&mut *out, uncached)?;
        }
        if !self.markers.is_empty() {
            out.extend_from_slice(br#","markers":"#);
            serde_json::to_writer(&mut *out, &self.markers)?;
        }
        if !self.told.is_empty() {
            out.extend_from_slice(br#","told":"#);
            serde_json::to_writer(&mut *out, &self.told)?;
        }
        out.extend_from_slice(br#","blocks":"#);
        serde_json::to_writer(&mut *out, &blocks)?;
        out.push(b'}');
        out[HEAD.len()..HEAD.len() + at.len()].copy_from_slice(at.as_bytes());
        Ok(())
    }
}

/// reads a state as its derived reader does, keeping only the records of the paths it accepts.
struct Only<'a>(&'a dyn Fn(&str) -> bool);

impl<'de> serde::de::DeserializeSeed<'de> for Only<'_> {
    type Value = State;

    fn deserialize<D: serde::Deserializer<'de>>(self, deserializer: D) -> Result<State, D::Error> {
        deserializer.deserialize_map(self)
    }
}

impl<'de> serde::de::Visitor<'de> for Only<'_> {
    type Value = State;

    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str("fetchloom's state")
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<State, A::Error> {
        use serde::de::Error;
        let mut state = State::default();
        let (mut version, mut files, mut synced) = (None, false, false);
        let (mut dropped, mut freeing, mut folder, mut aside, mut replacing, mut uncached) =
            (false, false, false, false, false, false);
        let (mut markers, mut told) = (false, false);
        while let Some(key) = map.next_key::<String>()? {
            let twice = match key.as_str() {
                "version" => version.replace(map.next_value()?).is_some(),
                "files" => {
                    state.files = map.next_value_seed(Records(self.0))?;
                    std::mem::replace(&mut files, true)
                }
                "synced" => {
                    state.synced = map.next_value()?;
                    std::mem::replace(&mut synced, true)
                }
                "dropped" => {
                    state.dropped = map.next_value()?;
                    std::mem::replace(&mut dropped, true)
                }
                "freeing" => {
                    state.freeing = map.next_value()?;
                    std::mem::replace(&mut freeing, true)
                }
                "folder" => {
                    state.folder = map.next_value()?;
                    std::mem::replace(&mut folder, true)
                }
                "aside" => {
                    state.aside = map.next_value()?;
                    std::mem::replace(&mut aside, true)
                }
                "replacing" => {
                    state.replacing = map.next_value()?;
                    std::mem::replace(&mut replacing, true)
                }
                "uncached" => {
                    state.uncached = map.next_value()?;
                    std::mem::replace(&mut uncached, true)
                }
                "markers" => {
                    state.markers = map.next_value()?;
                    std::mem::replace(&mut markers, true)
                }
                "told" => {
                    state.told = map.next_value()?;
                    std::mem::replace(&mut told, true)
                }
                _ => {
                    map.next_value::<serde::de::IgnoredAny>()?;
                    false
                }
            };
            if twice {
                return Err(A::Error::custom(format!("duplicate field `{key}`")));
            }
        }
        if !files {
            return Err(A::Error::missing_field("files"));
        }
        state.version = version.ok_or_else(|| A::Error::missing_field("version"))?;
        Ok(state)
    }
}

/// the state's records of the paths [`Only`] accepts.
struct Records<'a>(&'a dyn Fn(&str) -> bool);

impl<'de> serde::de::DeserializeSeed<'de> for Records<'_> {
    type Value = BTreeMap<String, Placed>;

    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_map(self)
    }
}

impl<'de> serde::de::Visitor<'de> for Records<'_> {
    type Value = BTreeMap<String, Placed>;

    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str("the files fetchloom placed")
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut records = BTreeMap::new();
        while let Some(path) = map.next_key_seed(Kept(self.0))? {
            match path {
                Some(path) => {
                    records.insert(path, map.next_value()?);
                }
                None => {
                    map.next_value::<serde::de::IgnoredAny>()?;
                }
            }
        }
        Ok(records)
    }
}

/// a record's path, when [`Only`] accepts it.
struct Kept<'a>(&'a dyn Fn(&str) -> bool);

impl<'de> serde::de::DeserializeSeed<'de> for Kept<'_> {
    type Value = Option<String>;

    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_str(self)
    }
}

impl serde::de::Visitor<'_> for Kept<'_> {
    type Value = Option<String>;

    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str("a path")
    }

    fn visit_str<E>(self, path: &str) -> Result<Self::Value, E> {
        Ok((self.0)(path).then(|| path.to_owned()))
    }
}

/// a block of records: the path of its first record, and where its bytes start and end.
type Block = (String, u64, u64);

/// what follows the records: the settled sync, the dropped datasets and those
/// whose drop has not freed the cache yet, the folder, the files kept aside, those being replaced,
/// what the cache lacked of the copies, the folders fetchloom marked for git, what runs told once
/// and the blocks.
#[derive(Deserialize)]
struct Tail {
    #[serde(default)]
    synced: Option<Synced>,
    #[serde(default)]
    dropped: BTreeSet<String>,
    #[serde(default)]
    freeing: BTreeSet<String>,
    #[serde(default)]
    folder: Option<Folder>,
    #[serde(default)]
    aside: BTreeMap<String, String>,
    #[serde(default)]
    replacing: BTreeSet<String>,
    #[serde(default)]
    uncached: Option<Uncached>,
    #[serde(default)]
    markers: BTreeMap<String, String>,
    #[serde(default)]
    told: Told,
    blocks: Vec<Block>,
}

/// reads parts of a state, counting the bytes.
struct Reading<'a, R> {
    file: &'a mut R,
    bytes: u64,
}

impl<R: Read + Seek> Reading<'_, R> {
    /// the `length` bytes at `start`, added to `into`.
    fn append(&mut self, start: u64, length: u64, into: &mut Vec<u8>) -> io::Result<()> {
        self.file.seek(SeekFrom::Start(start))?;
        into.reserve(usize::try_from(length).unwrap_or_default());
        let read = self.file.by_ref().take(length).read_to_end(into)? as u64;
        self.bytes += read;
        if read < length {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        Ok(())
    }

    /// what follows the records that end at `at`, parsed as it is read: the comma that ends the
    /// records opens it as an object of its own.
    fn tail<T: serde::de::DeserializeOwned>(&mut self, at: u64) -> Result<T, &'static str> {
        let unread = |_| "what follows its records cannot be read";
        self.file.seek(SeekFrom::Start(at)).map_err(unread)?;
        let mut comma = [0];
        self.file
            .read_exact(&mut comma)
            .map_err(|_| "its records do not end where its header says")?;
        self.bytes += 1;
        if comma != *b"," {
            return Err("its records do not end where its header says");
        }
        let mut counted = Counted {
            inner: &mut *self.file,
            bytes: 0,
        };
        let parsed =
            serde_json::from_reader(io::Cursor::new(b"{").chain(io::BufReader::new(&mut counted)));
        self.bytes += counted.bytes;
        parsed.map_err(|_| "what follows its records is not its index")
    }

    /// the state read whole from its start as it is parsed, of the records `keep` accepts, for
    /// one whose index does not fit: a file that is no state, as one of zeros a clone can hold,
    /// stops at its first byte rather than being held whole, and one `ours` refuses by the
    /// folder it names gives none of its records.
    fn streamed(
        &mut self,
        keep: &dyn Fn(&str) -> bool,
        ours: &dyn Fn(Option<&Folder>) -> bool,
    ) -> io::Result<(State, u64)> {
        let owner: Option<Owner> =
            self.parsed(|deserializer| serde::Deserialize::deserialize(deserializer))?;
        let state = match owner {
            Some(owner) if owner.version != Some(VERSION) => State::default(),
            Some(owner) if !ours(owner.folder.as_ref()) => foreign(owner.folder, owner.markers),
            Some(_) => self
                .parsed(|deserializer| {
                    serde::de::DeserializeSeed::deserialize(Only(keep), deserializer)
                })?
                .filter(|state| state.version == VERSION)
                .unwrap_or_default(),
            None => State::default(),
        };
        Ok((state, self.bytes))
    }

    /// what `parse` makes of the whole file read from its start, none when it is no such json.
    fn parsed<T>(
        &mut self,
        parse: impl FnOnce(
            &mut serde_json::Deserializer<
                serde_json::de::IoRead<io::BufReader<&mut Counted<&mut R>>>,
            >,
        ) -> serde_json::Result<T>,
    ) -> io::Result<Option<T>> {
        self.file.seek(SeekFrom::Start(0))?;
        let mut counted = Counted {
            inner: &mut *self.file,
            bytes: 0,
        };
        let mut deserializer =
            serde_json::Deserializer::from_reader(io::BufReader::new(&mut counted));
        let parsed = parse(&mut deserializer).and_then(|value| deserializer.end().map(|()| value));
        drop(deserializer);
        self.bytes += counted.bytes;
        Ok(parsed.ok())
    }
}

/// the records at or below `folders` that `keep` accepts, read through the state's index, or why
/// it declines when anything it reads does not fit the index, which a whole read ignores.
fn indexed(
    reading: &mut Reading<'_, impl Read + Seek>,
    folders: &[String],
    keep: &dyn Fn(&str) -> bool,
    ours: &dyn Fn(Option<&Folder>) -> bool,
) -> Result<State, &'static str> {
    let Tail {
        synced,
        dropped,
        freeing,
        folder,
        aside,
        replacing,
        uncached,
        markers,
        told,
        blocks,
    } = match index(reading, ours)? {
        Indexed::Tail(tail) => *tail,
        Indexed::Foreign(folder, marks) => return Ok(foreign(folder, marks)),
    };
    let mut files = BTreeMap::new();
    let mut bytes = Vec::new();
    for run in runs(&blocks, folders) {
        let (first, next) = framed(reading, &blocks, run, &mut bytes)?;
        let mut deserializer = serde_json::Deserializer::from_slice(&bytes);
        let run = Run { first, next, keep };
        let kept = serde::de::DeserializeSeed::deserialize(run, &mut deserializer)
            .and_then(|kept| deserializer.end().map(|()| kept))
            .map_err(|_| "a block does not hold the paths its index says")?;
        files.extend(kept);
    }
    Ok(State {
        version: VERSION,
        files,
        synced,
        dropped,
        freeing,
        folder,
        aside,
        replacing,
        uncached,
        markers,
        told,
    })
}

/// every record, read a block at a time through the state's index into one list in path order,
/// which builds the map at once, or why it declines as [`indexed`] does.
fn by_blocks(
    reading: &mut Reading<'_, impl Read + Seek>,
    ours: &dyn Fn(Option<&Folder>) -> bool,
) -> Result<State, &'static str> {
    let Tail {
        synced,
        dropped,
        freeing,
        folder,
        aside,
        replacing,
        uncached,
        markers,
        told,
        blocks,
    } = match index(reading, ours)? {
        Indexed::Tail(tail) => *tail,
        Indexed::Foreign(folder, marks) => return Ok(foreign(folder, marks)),
    };
    let mut records = Vec::new();
    let mut bytes = Vec::new();
    for at in 0..blocks.len() {
        let (first, next) = framed(reading, &blocks, at..at + 1, &mut bytes)?;
        let mut deserializer = serde_json::Deserializer::from_slice(&bytes);
        let block = OneBlock {
            first,
            next,
            into: &mut records,
        };
        serde::de::DeserializeSeed::deserialize(block, &mut deserializer)
            .and_then(|()| deserializer.end())
            .map_err(|_| "a block does not hold the paths its index says")?;
    }
    Ok(State {
        version: VERSION,
        files: records.into_iter().collect(),
        synced,
        dropped,
        freeing,
        folder,
        aside,
        replacing,
        uncached,
        markers,
        told,
    })
}

/// what follows a state's records, once its blocks are found to cover them.
fn index(
    reading: &mut Reading<'_, impl Read + Seek>,
    ours: &dyn Fn(Option<&Folder>) -> bool,
) -> Result<Indexed, &'static str> {
    let mut head = Vec::new();
    reading
        .append(0, START as u64, &mut head)
        .map_err(|_| "it is shorter than its header")?;
    let at = records_end(&head).ok_or("its header is not this version's")?;
    // the folder first, so nothing more of another folder's state, as a clone brings it, is read
    let owner: Owner = reading.tail(at)?;
    if !ours(owner.folder.as_ref()) {
        return Ok(Indexed::Foreign(owner.folder, owner.markers));
    }
    let found: Tail = reading.tail(at)?;
    if !chained(&found.blocks, at) {
        return Err("its blocks do not cover its records");
    }
    Ok(Indexed::Tail(Box::new(found)))
}

/// what [`index`] found after a state's records: the index of one written for this folder, or
/// the folder another one names, with the folders it marked for git.
enum Indexed {
    Tail(Box<Tail>),
    Foreign(Option<Folder>, BTreeMap<String, String>),
}

/// the version and the folder a state records, read before anything else of it, and the folders
/// it marked for git, every other field passed over unheld.
#[derive(Deserialize)]
struct Owner {
    #[serde(default)]
    version: Option<u32>,
    #[serde(default)]
    folder: Option<Folder>,
    #[serde(default)]
    markers: BTreeMap<String, String>,
}

/// a state written for another folder, `folder`, with none of its records but the folders it
/// marked for git, `markers`: the reader drops the rest.
fn foreign(folder: Option<Folder>, markers: BTreeMap<String, String>) -> State {
    State {
        version: VERSION,
        folder,
        markers,
        ..State::default()
    }
}

/// a reader that counts the bytes read through it.
struct Counted<R> {
    inner: R,
    bytes: u64,
}

impl<R: Read> Read for Counted<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(buf)?;
        self.bytes += read as u64;
        Ok(read)
    }
}

/// the records of the blocks `run` read into `bytes` as one json object, with the first path of
/// the run and of the block after it.
fn framed<'b>(
    reading: &mut Reading<'_, impl Read + Seek>,
    blocks: &'b [Block],
    run: Range<usize>,
    bytes: &mut Vec<u8>,
) -> Result<(&'b str, Option<&'b str>), &'static str> {
    let (first, start, _) = &blocks[run.start];
    let end = blocks[run.end - 1].2;
    let next = blocks.get(run.end).map(|(first, ..)| first.as_str());
    // the bytes right after the run, read along with it: the next block's first path, or the end
    // of the records
    let after = match next {
        Some(next) => {
            let mut key = vec![b','];
            serde_json::to_writer(&mut key, next).map_err(|_| "a path cannot be written")?;
            key.push(b':');
            key
        }
        None => b"}".to_vec(),
    };
    bytes.clear();
    bytes.push(b'{');
    reading
        .append(*start, end - start + after.len() as u64, bytes)
        .map_err(|_| "a block cannot be read")?;
    if !bytes.ends_with(&after) {
        return Err("a block does not end where its index says");
    }
    bytes.truncate(bytes.len() - after.len());
    bytes.push(b'}');
    Ok((first, next))
}

/// the number in a state's first bytes: where its records end, padded with spaces.
fn records_end(head: &[u8]) -> Option<u64> {
    let (slot, files) = head.strip_prefix(HEAD)?.split_at_checked(SLOT)?;
    if files != FILES {
        return None;
    }
    let digits = &slot[..=slot.iter().rposition(|byte| *byte != b' ')?];
    if !digits.iter().all(u8::is_ascii_digit) || digits.len() > 1 && digits[0] == b'0' {
        return None;
    }
    std::str::from_utf8(digits).ok()?.parse().ok()
}

/// whether `blocks` cover the records from [`START`] to `at` without a gap, a comma between two
/// of them, with their first paths in order.
fn chained(blocks: &[Block], at: u64) -> bool {
    let mut next = START as u64;
    let mut last: Option<&str> = None;
    for (first, start, end) in blocks {
        if *start != next || end <= start || last.is_some_and(|last| last >= first.as_str()) {
            return false;
        }
        let Some(after) = end.checked_add(1) else {
            return false;
        };
        next = after;
        last = Some(first);
    }
    // the records' closing brace stands where the next block would start, or at the start
    let close = if blocks.is_empty() { next } else { next - 1 };
    close.checked_add(1) == Some(at)
}

/// the blocks that can hold a path at or below one of `folders`, consecutive ones joined, in
/// order. a block holds the paths from its first up to the next block's first.
fn runs(blocks: &[Block], folders: &[String]) -> Vec<Range<usize>> {
    let mut wanted: Vec<Range<usize>> = folders
        .iter()
        .map(|folder| {
            // every path at or below `folder` sorts from it up to `folder` and a `0`, the
            // character after `/`
            let past = format!("{folder}0");
            let from = blocks
                .partition_point(|(first, ..)| first <= folder)
                .saturating_sub(1);
            let to = blocks.partition_point(|(first, ..)| *first < past);
            from..to
        })
        .filter(|range| !range.is_empty())
        .collect();
    wanted.sort_unstable_by_key(|range| range.start);
    let mut runs: Vec<Range<usize>> = Vec::new();
    for range in wanted {
        match runs.last_mut() {
            Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
            _ => runs.push(range),
        }
    }
    runs
}

/// the records of a run of blocks that `keep` accepts, checked against the index: the first is the
/// run's first block's, each past the one before and before `next`, the block after's first path.
struct Run<'a> {
    first: &'a str,
    next: Option<&'a str>,
    keep: &'a dyn Fn(&str) -> bool,
}

impl<'de> serde::de::DeserializeSeed<'de> for Run<'_> {
    type Value = BTreeMap<String, Placed>;

    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_map(self)
    }
}

impl<'de> serde::de::Visitor<'de> for Run<'_> {
    type Value = BTreeMap<String, Placed>;

    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str("a block of fetchloom's state")
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        use serde::de::Error;
        let mut records = BTreeMap::new();
        let mut last: Option<String> = None;
        while let Some(path) = map.next_key::<String>()? {
            let rising = match &last {
                None => path == self.first,
                Some(last) => *last < path,
            };
            if !rising || self.next.is_some_and(|next| path.as_str() >= next) {
                return Err(A::Error::custom("a path outside its block"));
            }
            if (self.keep)(&path) {
                records.insert(path.clone(), map.next_value()?);
            } else {
                map.next_value::<serde::de::IgnoredAny>()?;
            }
            last = Some(path);
        }
        if last.is_none() {
            return Err(A::Error::custom("an empty block"));
        }
        Ok(records)
    }
}

/// the records of one block, checked against the index as [`Run`] checks them, pushed onto `into`
/// in path order without a copy of each path.
struct OneBlock<'a, 'v> {
    first: &'a str,
    next: Option<&'a str>,
    into: &'v mut Vec<(String, Placed)>,
}

impl<'de> serde::de::DeserializeSeed<'de> for OneBlock<'_, '_> {
    type Value = ();

    fn deserialize<D: serde::Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        deserializer.deserialize_map(self)
    }
}

impl<'de> serde::de::Visitor<'de> for OneBlock<'_, '_> {
    type Value = ();

    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str("a block of fetchloom's state")
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        use serde::de::Error;
        let start = self.into.len();
        while let Some(path) = map.next_key::<String>()? {
            let rising = match self.into[start..].last() {
                None => path == self.first,
                Some((last, _)) => *last < path,
            };
            if !rising || self.next.is_some_and(|next| path.as_str() >= next) {
                return Err(A::Error::custom("a path outside its block"));
            }
            let placed = map.next_value()?;
            self.into.push((path, placed));
        }
        if self.into.len() == start {
            return Err(A::Error::custom("an empty block"));
        }
        Ok(())
    }
}

impl Look {
    pub(crate) fn of(metadata: &Metadata) -> Look {
        Look {
            size: metadata.len(),
            modified: metadata.modified().map_or(0, crate::units::nanos),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::digest;

    /// a state is its folder's when the number or the creation time match, so a move on a disk of
    /// no lasting numbers keeps it, and a copy, whose both differ, does not. what either side
    /// cannot tell matches nothing: without a creation time, as on hfs+, the number alone decides.
    /// a time that falls on a whole second still matches, as one in a hundred do on fat's 10 ms.
    #[test]
    fn a_folder_matches_by_its_number_or_its_creation_time() {
        let folder = |inode, created| Folder { inode, created };
        for (recorded, here, matches) in [
            (folder(Some(7), Some(100)), folder(Some(7), Some(100)), true),
            (folder(Some(7), Some(100)), folder(Some(8), Some(100)), true),
            (folder(Some(7), Some(100)), folder(Some(7), Some(200)), true),
            (
                folder(Some(7), Some(100)),
                folder(Some(8), Some(200)),
                false,
            ),
            (folder(Some(7), None), folder(Some(7), Some(100)), true),
            (folder(Some(7), Some(100)), folder(Some(7), None), true),
            (folder(Some(7), None), folder(Some(8), Some(100)), false),
            (folder(Some(7), Some(100)), folder(Some(8), None), false),
            (folder(None, None), folder(None, None), false),
            (
                folder(Some(7), Some(5_000_000_000)),
                folder(Some(8), Some(5_000_000_000)),
                true,
            ),
            (
                folder(Some(7), Some(5_000_000_000)),
                folder(Some(7), Some(5_000_000_000)),
                true,
            ),
            (
                folder(Some(7), Some(5_010_000_000)),
                folder(Some(8), Some(5_010_000_000)),
                true,
            ),
        ] {
            assert_eq!(recorded.matches(&here), matches, "{recorded:?} {here:?}");
        }
    }

    /// a record of `dataset` holding `size` bytes.
    fn placed(dataset: &str, size: u64) -> Placed {
        Placed {
            dataset: dataset.to_owned(),
            sha256: digest::sha256(&size.to_le_bytes()),
            method: Method::Hardlink,
            look: Look { size, modified: 42 },
            writable: false,
            checked: 43,
            adopted: false,
            unlinkable: false,
            racy_intact: OnceLock::new(),
        }
    }

    /// a reader that counts the bytes read through it.
    struct Counted<R> {
        inner: R,
        bytes: usize,
    }

    impl<R: Read> Read for Counted<R> {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let read = self.inner.read(buffer)?;
            self.bytes += read;
            Ok(read)
        }
    }

    impl<R: Seek> Seek for Counted<R> {
        fn seek(&mut self, to: io::SeekFrom) -> io::Result<u64> {
            self.inner.seek(to)
        }
    }

    /// the mark of a dataset data.toml no longer has goes, so one added again is placed, and only
    /// the call that drops it says so.
    #[test]
    fn marks_go_with_their_datasets_once() {
        let manifest = Manifest::parse("[datasets.raw]\nref = \"zenodo:1\"\n").unwrap();
        let mut state = State {
            dropped: ["raw".to_owned(), "removed".to_owned()].into(),
            ..State::default()
        };
        assert!(state.forget_removed(&manifest));
        assert_eq!(state.dropped.iter().collect::<Vec<_>>(), ["raw"]);
        assert!(!state.forget_removed(&manifest));
    }

    /// a small folder beside a large one reads little past its records, however large the state.
    #[test]
    fn a_read_within_a_folder_reads_little_beyond_its_records() {
        let mut state = State::default();
        for number in 0..4_000 {
            let path = format!("data/large/{number:05}.bin");
            state.files.insert(path, placed("large", number));
        }
        state
            .files
            .insert("data/tiny/payload.bin".to_owned(), placed("tiny", 14));
        let bytes = state.render();
        let mut file = Counted {
            inner: io::Cursor::new(&bytes),
            bytes: 0,
        };
        let folders = ["data/tiny".to_owned()];
        let (read, _) = State::read_within(
            &mut file,
            &folders,
            &|path| path.starts_with("data/tiny/"),
            &|_| true,
        )
        .unwrap();
        assert_eq!(
            read.files.keys().collect::<Vec<_>>(),
            ["data/tiny/payload.bin"]
        );
        assert!(
            file.bytes * 4 < bytes.len(),
            "read {} of {} bytes",
            file.bytes,
            bytes.len()
        );
    }

    #[test]
    fn state_round_trips_and_anything_else_reads_as_empty() {
        let mut state = State::default();
        state.files.insert(
            "data/iris/iris.csv".to_owned(),
            Placed {
                dataset: "iris".to_owned(),
                sha256: digest::sha256(b"x"),
                method: Method::Copy,
                look: Look {
                    size: 1,
                    modified: 42,
                },
                writable: true,
                checked: 43,
                adopted: false,
                unlinkable: false,
                racy_intact: std::sync::OnceLock::new(),
            },
        );
        let read = State::parse(&state.render());
        assert_eq!(read.files, state.files);
        let bytes = state.render();
        state.files["data/iris/iris.csv"]
            .racy_intact
            .set(())
            .unwrap();
        assert_eq!(state.render(), bytes);
        let mut untrusted: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        untrusted["files"]["data/iris/iris.csv"]["racy_intact"] = true.into();
        let bytes = serde_json::to_vec(&untrusted).unwrap();
        assert_eq!(State::parse(&bytes).files, read.files);
        assert_eq!(State::parse_only(&bytes, &|_| true).files, read.files);
        assert_eq!(read.synced, None, "no sync settled yet");
        state.synced = Some(Synced {
            manifest: digest::sha256(b"data.toml"),
            lock: digest::sha256(b"data.lock"),
            complete: true,
        });
        assert_eq!(State::parse(&state.render()).synced, state.synced);
        state.dropped.insert("pets".to_owned());
        state.freeing.insert("pets".to_owned());
        let read = State::parse(&state.render());
        assert_eq!(read.dropped, state.dropped);
        assert_eq!(read.freeing, state.freeing);
        state.dropped.clear();
        state.freeing.clear();
        state.synced = None;
        assert!(State::parse(b"not json").files.is_empty());
        assert!(
            State::parse(br#"{"version": 9, "files": {}}"#)
                .files
                .is_empty()
        );
        let unrecorded = String::from_utf8(state.render())
            .unwrap()
            .replace(r#","writable":true"#, "");
        assert!(State::parse(unrecorded.as_bytes()).files.is_empty());
    }

    #[test]
    fn a_read_of_some_records_matches_a_read_of_all() {
        let mut state = State::default();
        for (path, size) in [("data/a/x", 1), ("data/a/\"q\u{e9}\"", 2), ("data/ab/y", 3)] {
            state.files.insert(
                path.to_owned(),
                Placed {
                    dataset: "a".to_owned(),
                    sha256: digest::sha256(path.as_bytes()),
                    method: Method::Hardlink,
                    look: Look { size, modified: 42 },
                    writable: false,
                    checked: 43,
                    adopted: size == 2,
                    unlinkable: false,
                    racy_intact: std::sync::OnceLock::new(),
                },
            );
        }
        state.synced = Some(Synced {
            manifest: digest::sha256(b"manifest"),
            lock: digest::sha256(b"lock"),
            complete: true,
        });
        let bytes = state.render();
        let inside = |path: &str| path.starts_with("data/a/");
        let read = State::parse_only(&bytes, &inside);
        let mut expected = State::parse(&bytes);
        expected.files.retain(|path, _| inside(path));
        assert_eq!(read.files.len(), 2);
        assert_eq!(read.files, expected.files);
        assert_eq!(read.synced, state.synced);
        assert_eq!(read.version, VERSION);
        for unread in [
            &b"not json"[..],
            br#"{"version": 9, "files": {}}"#,
            br#"{"version": 2}"#,
            br#"{"version": 2, "files": {}, "files": {}}"#,
            br#"{"version": 2, "files": {}} trailing"#,
        ] {
            let read = State::parse_only(unread, &|_| true);
            assert!(read.files.is_empty() && read.synced.is_none(), "{unread:?}");
            assert_eq!(read.version, 0);
        }
    }

    #[test]
    fn an_unknown_version_drops_placement_and_sync_claims() {
        let mut state = State::default();
        state.files.insert(
            "data/iris/iris.csv".to_owned(),
            Placed {
                dataset: "iris".to_owned(),
                sha256: digest::sha256(b"good"),
                method: Method::Hardlink,
                look: Look {
                    size: 4,
                    modified: 42,
                },
                writable: false,
                checked: 43,
                adopted: false,
                unlinkable: false,
                racy_intact: std::sync::OnceLock::new(),
            },
        );
        state.synced = Some(Synced {
            manifest: digest::sha256(b"manifest"),
            lock: digest::sha256(b"lock"),
            complete: true,
        });
        let mut stored: serde_json::Value = serde_json::from_slice(&state.render()).unwrap();
        for version in [0, VERSION - 1, VERSION + 1, u32::MAX] {
            stored["version"] = serde_json::json!(version);
            let bytes = serde_json::to_vec(&stored).unwrap();
            let read = State::parse(&bytes);
            assert!(
                read.files.is_empty(),
                "version {version} cannot vouch for files"
            );
            assert!(
                read.synced.is_none(),
                "version {version} cannot claim a complete sync"
            );
            let folders = ["data".to_owned()];
            let (within, _) =
                State::read_within(&mut io::Cursor::new(&bytes), &folders, &|_| true, &|_| true)
                    .unwrap();
            assert!(within.files.is_empty() && within.synced.is_none());
        }
    }

    /// a state another folder wrote, as a clone or a copy brings it, is dropped from its index
    /// alone: none of its records is read, however many it holds. its own folder reads them all.
    #[test]
    fn a_state_written_for_another_folder_has_no_record_read() {
        let mut state = crowded();
        for number in 0..2_000 {
            let path = format!("data/big/{number:05}");
            state.files.insert(path, placed("big", number));
        }
        let there = Folder {
            inode: Some(7),
            created: None,
        };
        let bytes = state.render_for(Some(&there));
        assert!(bytes.len() > 4 * BLOCK, "{} bytes", bytes.len());
        let here = Folder {
            inode: Some(8),
            created: None,
        };
        for (folder, all) in [(&here, false), (&there, true)] {
            let ours = |recorded: Option<&Folder>| {
                recorded.is_some_and(|recorded| recorded.matches(folder))
            };
            let mut file = Counted {
                inner: io::Cursor::new(&bytes),
                bytes: 0,
            };
            let (read, _) = State::read_whole(&mut file, bytes.len() as u64, &ours).unwrap();
            assert_eq!(read.folder.as_ref(), Some(&there));
            if all {
                assert_eq!(read.files.len(), state.files.len());
            } else {
                assert!(read.files.is_empty());
                assert!(
                    file.bytes < BLOCK,
                    "{} of {} bytes",
                    file.bytes,
                    bytes.len()
                );
            }
        }
    }

    /// a state that is no state at all, as a file of zeros a clone can hold, is found so at its
    /// first bytes rather than read whole.
    #[test]
    fn a_state_of_zeros_is_never_read_whole() {
        let zeros = vec![0_u8; 8 * BLOCK];
        let mut file = Counted {
            inner: io::Cursor::new(&zeros),
            bytes: 0,
        };
        let (read, _) = State::read_whole(&mut file, zeros.len() as u64, &|_| true).unwrap();
        assert!(!read.was_read());
        assert!(
            file.bytes < BLOCK,
            "{} of {} bytes",
            file.bytes,
            zeros.len()
        );
    }

    /// paths in a folder, beside it and in folders whose names start alike, around block edges.
    fn crowded() -> State {
        let mut state = State::default();
        let paths = [
            "data/a",
            "data/a-x/f",
            "data/a.b/f",
            "data/a/x",
            "data/a/y/z",
            "data/ab/w",
            "data/caf\u{e9}/\u{e9}",
            "data/caf\u{e9}-/x",
            "data/\"q\"/x",
            "data/z/inner/x",
        ];
        for (size, path) in paths.into_iter().enumerate() {
            state
                .files
                .insert(path.to_owned(), placed("a", size as u64));
        }
        for number in 0..200 {
            let path = format!("data/m/{number:04}");
            state.files.insert(path, placed("m", number));
        }
        state.synced = Some(Synced {
            manifest: digest::sha256(b"manifest"),
            lock: digest::sha256(b"lock"),
            complete: true,
        });
        state.dropped.insert("pets".to_owned());
        state.freeing.insert("pets".to_owned());
        state.aside.insert(
            "data/b/.fetchloom-0123456789abcdef.old".to_owned(),
            "data/b/c".to_owned(),
        );
        state.replacing.insert("data/b/d".to_owned());
        state.uncached = Some(Uncached {
            cache: "/cache".to_owned(),
            copies: 7,
            newest: 42,
        });
        state.markers.insert("data/a".to_owned(), "0".repeat(64));
        state.markers.insert("data".to_owned(), "1".repeat(64));
        state.told.note(Once::Rule, "/data/");
        state.told.note(Once::Refill, "data/a");
        state
    }

    fn inside(path: &str, folders: &[String]) -> bool {
        folders.iter().any(|folder| {
            path.strip_prefix(folder.as_str())
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
        })
    }

    /// what a whole read of `bytes` keeps at or below `folders`.
    fn whole_within(bytes: &[u8], folders: &[String]) -> State {
        let mut state = State::parse(bytes);
        state.files.retain(|path, _| inside(path, folders));
        state
    }

    fn assert_same(read: &State, whole: &State, case: &str) {
        assert_eq!(read.files, whole.files, "{case}");
        assert_eq!(read.synced, whole.synced, "{case}");
        assert_eq!(read.dropped, whole.dropped, "{case}");
        assert_eq!(read.freeing, whole.freeing, "{case}");
        assert_eq!(read.aside, whole.aside, "{case}");
        assert_eq!(read.replacing, whole.replacing, "{case}");
        assert_eq!(read.uncached, whole.uncached, "{case}");
        assert_eq!(read.markers, whole.markers, "{case}");
        assert_eq!(read.told, whole.told, "{case}");
        assert_eq!(read.version, whole.version, "{case}");
    }

    const FOLDERS: [&[&str]; 13] = [
        &[],
        &["data/a"],
        &["data/a", "data/a-x"],
        &["data/a-x", "data/a"],
        &["data/a/y"],
        &["data/caf\u{e9}"],
        &["data/\"q\""],
        &["data/m"],
        &["data/m/0100"],
        &["data/m", "data/z/inner"],
        &["data/zz"],
        &["data/0"],
        &["data"],
    ];

    /// the records are those serde writes for the map, `at` is a plain number padded with
    /// spaces, and each block starts at the record its index names.
    #[test]
    fn the_state_is_one_json_document_with_its_index() {
        let empty = State::default();
        let crowded = crowded();
        for (state, block) in [(&empty, BLOCK), (&crowded, BLOCK), (&crowded, 300)] {
            let bytes = state.render_in(block, None);
            let document: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            let files = serde_json::to_vec(&state.files).unwrap();
            let at = START - 1 + files.len();
            assert_eq!(document["at"], serde_json::json!(at));
            assert_eq!(bytes[START - 1..at], files);
            let digits = at.to_string();
            let slot = &bytes[HEAD.len()..HEAD.len() + SLOT];
            assert!(slot.starts_with(digits.as_bytes()), "{slot:?}");
            assert!(slot[digits.len()..].iter().all(|byte| *byte == b' '));
            let blocks: Vec<Block> = serde_json::from_value(document["blocks"].clone()).unwrap();
            for (index, (first, start, end)) in blocks.iter().enumerate() {
                let mut key = serde_json::to_vec(first).unwrap();
                key.push(b':');
                assert!(bytes[usize::try_from(*start).unwrap()..].starts_with(&key));
                if index + 1 < blocks.len() {
                    assert!(end - start >= block as u64, "a block ends early");
                }
            }
            let first = blocks.first().map(|(first, ..)| first);
            assert_eq!(first, state.files.keys().next());
            assert!(chained(&blocks, at as u64));
            assert_eq!(State::parse(&bytes).files, state.files);
        }
    }

    /// a read within folders goes through the index, at any block size, and so does a read of
    /// every record, a block at a time.
    #[test]
    fn a_read_within_folders_answers_as_a_whole_read() {
        let state = crowded();
        for block in [1, 300, 4096, usize::MAX] {
            let bytes = state.render_in(block, None);
            let mut file = io::Cursor::new(&bytes);
            let mut reading = Reading {
                file: &mut file,
                bytes: 0,
            };
            let read = by_blocks(&mut reading, &|_| true).expect("its index fits");
            assert_same(&read, &State::parse(&bytes), &format!("blocks of {block}"));
            assert_eq!(read.aside, state.aside, "blocks of {block}");
            assert_eq!(read.replacing, state.replacing, "blocks of {block}");
            assert_eq!(read.uncached, state.uncached, "blocks of {block}");
            assert_eq!(read.markers, state.markers, "blocks of {block}");
            assert_eq!(read.told, state.told, "blocks of {block}");
            for folders in FOLDERS {
                let folders: Vec<String> = folders.iter().map(|&folder| folder.into()).collect();
                let case = format!("{folders:?} in blocks of {block}");
                let keep = |path: &str| inside(path, &folders);
                let mut file = io::Cursor::new(&bytes);
                let mut reading = Reading {
                    file: &mut file,
                    bytes: 0,
                };
                let read = indexed(&mut reading, &folders, &keep, &|_| true).expect(&case);
                assert_same(&read, &whole_within(&bytes, &folders), &case);
            }
        }
    }

    /// a reader that counts the calls a read makes of it.
    struct Calls<'a> {
        bytes: io::Cursor<&'a [u8]>,
        reads: usize,
        seeks: usize,
    }

    impl Read for Calls<'_> {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            self.reads += 1;
            self.bytes.read(buffer)
        }
    }

    impl Seek for Calls<'_> {
        fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
            self.seeks += 1;
            self.bytes.seek(to)
        }
    }

    /// a state of one block is read in one go, where its index would take three reads and three
    /// seeks; a torn one reads as the whole read finds it.
    #[test]
    fn a_state_of_one_block_is_one_read() {
        let bytes = crowded().render();
        assert!(bytes.len() <= BLOCK, "{} bytes", bytes.len());
        for length in [bytes.len(), bytes.len() / 2] {
            let mut file = Calls {
                bytes: io::Cursor::new(&bytes[..length]),
                reads: 0,
                seeks: 0,
            };
            let (read, counted) = State::read_whole(&mut file, length as u64, &|_| true).unwrap();
            assert_same(
                &read,
                &State::parse(&bytes[..length]),
                &format!("{length} bytes"),
            );
            assert_eq!(counted, length as u64);
            assert_eq!((file.reads, file.seeks), (1, 0), "{length} bytes");
        }
    }

    /// `bytes` with its index replaced by `blocks`, the records and `at` untouched.
    fn with_index(bytes: &[u8], blocks: &serde_json::Value) -> Vec<u8> {
        let index = bytes
            .windows(10)
            .rposition(|window| window == br#","blocks":"#)
            .unwrap();
        let mut mangled = bytes[..index + 10].to_vec();
        mangled.extend(serde_json::to_vec(blocks).unwrap());
        mangled.push(b'}');
        mangled
    }

    /// a changed `at` changes no answer either, since a whole read ignores it and the index.
    #[test]
    fn a_read_within_a_changed_index_answers_as_a_whole_read() {
        let bytes = crowded().render_in(300, None);
        let document: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let blocks = document["blocks"].as_array().unwrap().clone();
        assert_eq!(with_index(&bytes, &blocks.clone().into()), bytes);
        let count = blocks.len();
        // a block said to start just past its first path, still in order, so a read of that path
        // would pass over the block that holds it
        let first = blocks[20][0].as_str().unwrap().to_owned();
        let later = format!("{first}1");
        let changed = |change: &dyn Fn(&mut Vec<serde_json::Value>)| {
            let mut blocks = blocks.clone();
            change(&mut blocks);
            with_index(&bytes, &serde_json::Value::Array(blocks))
        };
        let mut states = vec![
            changed(&|blocks| blocks[5][0] = blocks[6][0].clone()),
            changed(&|blocks| blocks[5][0] = "data/m/0050".into()),
            changed(&|blocks| blocks[5][0] = "data/a/y".into()),
            changed(&|blocks| blocks.swap(3, 4)),
            changed(&|blocks| {
                blocks.remove(7);
            }),
            changed(&|blocks| {
                blocks.pop();
            }),
            changed(&Vec::clear),
            changed(&|blocks| {
                let end = blocks[8][2].as_u64().unwrap() + 1;
                blocks[8][2] = end.into();
                blocks[9][1] = (end + 1).into();
            }),
            changed(&|blocks| {
                let next = blocks.remove(9);
                blocks[8][2] = next[2].clone();
            }),
            changed(&|blocks| blocks[20][0] = later.clone().into()),
            changed(&|blocks| blocks[0][1] = 0.into()),
            changed(&|blocks| blocks[count - 1][2] = u64::MAX.into()),
            with_index(&bytes, &"blocks".into()),
            bytes[..bytes.len() - 2].to_vec(),
        ];
        let mut unnamed = bytes.clone();
        let index = unnamed
            .windows(10)
            .rposition(|window| window == br#","blocks":"#)
            .unwrap();
        unnamed[index + 7] = b'z';
        states.push(unnamed);
        let records_end = document["at"].as_u64().unwrap();
        for slot in [
            String::new(),
            "0".into(),
            "1".into(),
            format!("0{records_end}"),
            (records_end - 1).to_string(),
            (records_end + 1).to_string(),
            "x".into(),
        ] {
            let mut moved = bytes.clone();
            moved[HEAD.len()..HEAD.len() + SLOT]
                .copy_from_slice(format!("{slot:<SLOT$}").as_bytes());
            states.push(moved);
        }
        let mut sets: Vec<Vec<String>> = FOLDERS
            .iter()
            .map(|folders| folders.iter().map(|&folder| folder.into()).collect())
            .collect();
        sets.push(vec![first]);
        for (number, state) in states.iter().enumerate() {
            let (read, _) = State::read_blocks(&mut io::Cursor::new(state), &|_| true).unwrap();
            assert_same(
                &read,
                &State::parse(state),
                &format!("change {number}, every block"),
            );
            for folders in &sets {
                let keep = |path: &str| inside(path, folders);
                let (read, _) =
                    State::read_within(&mut io::Cursor::new(state), folders, &keep, &|_| true)
                        .unwrap();
                let case = format!("change {number}, {folders:?}");
                assert_same(&read, &whole_within(state, folders), &case);
            }
        }
    }
}
