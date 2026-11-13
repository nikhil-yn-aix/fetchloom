//! `.fetchloom/state`: every file fetchloom placed and how it looked right after, so `status` and
//! `sync` notice hand edits without hashing, but for a file as new as the state, which a coarse
//! clock cannot vouch for. it also keeps each rebuild of a recipe that differs from data.lock.
//!
//! its records come in path order, in blocks of about [`BLOCK`] bytes indexed at its end, so a
//! command that names a dataset reads only the blocks that can hold its folder's paths.

use std::collections::BTreeMap;
use std::fs::Metadata;
use std::io::{self, Read, Seek, SeekFrom};
use std::ops::Range;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::cache::Member;
use crate::digest::Sha256;
use crate::place::Method;
use crate::project::lock::derived::Derived;
use crate::project::manifest::Manifest;

const VERSION: u32 = 2;

/// a block of records ends once it holds this many bytes, so a read within a folder reads
/// about this much beyond the records it keeps.
const BLOCK: usize = 64 << 10;

/// how every state starts, up to the number `at` holds: where the records end.
const HEAD: &[u8] = br#"{"version":2,"at":"#;

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
    /// the rebuild of each recipe that made other files than data.lock pins for its key, kept until
    /// `fetchloom build --accept` pins it, a build pins another or data.toml drops the recipe.
    #[serde(default)]
    pub(crate) candidates: BTreeMap<String, Candidate>,
}

/// a rebuild of a recipe that made other files than data.lock pins for the same key. its files are
/// cached, but no build may reuse it: only a person pins it, with `fetchloom build --accept`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Candidate {
    pub(crate) key: Sha256,
    pub(crate) tree: Sha256,
    /// the tree data.lock pinned for `key` when the rebuild was kept.
    pub(crate) pinned: Sha256,
    /// sorted by path.
    pub(crate) files: Vec<Member>,
}

impl Candidate {
    /// whether the rebuild answers the recipe and the pin as they are now, which `build --accept`
    /// asks: `key` is the recipe's key now, the result data.lock pins now, `pinned`, must be the
    /// one it was kept beside, and its files must hash to its tree.
    pub(crate) fn answers(&self, key: Option<Sha256>, pinned: Option<&Derived>) -> bool {
        key == Some(self.key)
            && pinned.is_some_and(|pinned| pinned.key == self.key && pinned.tree == self.pinned)
            && crate::cache::tree_hash(&self.files) == self.tree
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
    pub(crate) fn parse_only(bytes: &[u8], keep: &dyn Fn(&str) -> bool) -> State {
        let mut deserializer = serde_json::Deserializer::from_slice(bytes);
        let state = serde::de::DeserializeSeed::deserialize(Only(keep), &mut deserializer)
            .and_then(|state| deserializer.end().map(|()| state));
        match state {
            Ok(state) if state.version == VERSION => state,
            _ => State::default(),
        }
    }

    /// [`State::parse_only`] of `file`, reading only the parts that can hold a record at or below
    /// `folders`, with how many bytes it read; a state whose index does not fit is read whole, so
    /// the records are always a whole read's. fails when `file` cannot be read.
    pub(crate) fn read_within(
        file: &mut (impl Read + Seek),
        folders: &[String],
        keep: &dyn Fn(&str) -> bool,
    ) -> io::Result<(State, u64)> {
        let mut reading = Reading { file, bytes: 0 };
        match indexed(&mut reading, folders, keep) {
            Ok(state) => return Ok((state, reading.bytes)),
            Err(reason) => tracing::debug!(reason, "the state is read whole"),
        }
        let bytes = reading.rest(0)?;
        Ok((State::parse_only(&bytes, keep), reading.bytes))
    }

    /// drops the kept rebuild of each recipe `manifest` no longer has, as relocking drops the
    /// recipe's pin, and says whether any went.
    pub(crate) fn forget_removed(&mut self, manifest: &Manifest) -> bool {
        let before = self.candidates.len();
        self.candidates.retain(|name, _| {
            manifest
                .datasets
                .get(name)
                .is_some_and(|entry| entry.recipe().is_some())
        });
        self.candidates.len() != before
    }

    /// the state as one json document: the records in path order, then where each block of them
    /// lies in the document, which [`State::read_within`] reads by.
    pub(crate) fn render(&self) -> Vec<u8> {
        self.render_in(BLOCK)
    }

    /// [`State::render`] with blocks of at least `block` bytes, the last one aside.
    fn render_in(&self, block: usize) -> Vec<u8> {
        // where serde_json::to_vec starts: doubling from 128 ends a large state's buffer at a
        // power of two, as large as before the state had an index
        let mut bytes = Vec::with_capacity(128);
        match self.encode(&mut bytes, block) {
            Ok(()) => bytes,
            Err(_) => Vec::new(),
        }
    }

    fn encode(&self, out: &mut Vec<u8>, block: usize) -> serde_json::Result<()> {
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
        if !self.candidates.is_empty() {
            out.extend_from_slice(br#","candidates":"#);
            serde_json::to_writer(&mut *out, &self.candidates)?;
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
        let (mut version, mut files, mut synced, mut candidates) = (None, false, false, false);
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
                "candidates" => {
                    state.candidates = map.next_value()?;
                    std::mem::replace(&mut candidates, true)
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

    fn visit_string<E>(self, path: String) -> Result<Self::Value, E> {
        Ok((self.0)(&path).then_some(path))
    }
}

/// a block of records: the path of its first record, and where its bytes start and end.
type Block = (String, u64, u64);

/// what follows the records: the settled sync, the kept rebuilds and the blocks.
#[derive(Deserialize)]
struct Tail {
    #[serde(default)]
    synced: Option<Synced>,
    #[serde(default)]
    candidates: BTreeMap<String, Candidate>,
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

    /// everything from `start` on.
    fn rest(&mut self, start: u64) -> io::Result<Vec<u8>> {
        self.file.seek(SeekFrom::Start(start))?;
        let mut bytes = Vec::new();
        self.bytes += self.file.read_to_end(&mut bytes)? as u64;
        Ok(bytes)
    }
}

/// the records at or below `folders` that `keep` accepts, read through the state's index, or why
/// it declines when anything it reads does not fit the index, which a whole read ignores.
fn indexed(
    reading: &mut Reading<'_, impl Read + Seek>,
    folders: &[String],
    keep: &dyn Fn(&str) -> bool,
) -> Result<State, &'static str> {
    let mut head = Vec::new();
    reading
        .append(0, START as u64, &mut head)
        .map_err(|_| "it is shorter than its header")?;
    let at = records_end(&head).ok_or("its header is not this version's")?;
    let mut tail = reading
        .rest(at)
        .map_err(|_| "what follows its records cannot be read")?;
    // the comma that ends the records opens what follows them as an object of its own
    match tail.first_mut() {
        Some(comma @ b',') => *comma = b'{',
        _ => return Err("its records do not end where its header says"),
    }
    let Tail {
        synced,
        candidates,
        blocks,
    } = serde_json::from_slice(&tail).map_err(|_| "what follows its records is not its index")?;
    drop(tail);
    if !chained(&blocks, at) {
        return Err("its blocks do not cover its records");
    }
    let mut files = BTreeMap::new();
    for run in runs(&blocks, folders) {
        let (first, start, _) = &blocks[run.start];
        let end = blocks[run.end - 1].2;
        let next = blocks.get(run.end).map(|(first, ..)| first.as_str());
        // the bytes right after the run, read along with it: the next block's first path, or
        // the end of the records
        let after = match next {
            Some(next) => {
                let mut key = vec![b','];
                serde_json::to_writer(&mut key, next).map_err(|_| "a path cannot be written")?;
                key.push(b':');
                key
            }
            None => b"}".to_vec(),
        };
        let mut bytes = vec![b'{'];
        reading
            .append(*start, end - start + after.len() as u64, &mut bytes)
            .map_err(|_| "a block cannot be read")?;
        if !bytes.ends_with(&after) {
            return Err("a block does not end where its index says");
        }
        bytes.truncate(bytes.len() - after.len());
        bytes.push(b'}');
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
        candidates,
    })
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

impl Look {
    pub(crate) fn of(metadata: &Metadata) -> Look {
        Look {
            size: metadata.len(),
            modified: metadata.modified().map_or(0, nanos),
        }
    }
}

/// `time` as the state records times: nanoseconds since 1970, zero before then, and capped at the
/// largest unsigned 64-bit integer.
pub(crate) fn nanos(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH).map_or(0, |since| {
        u64::try_from(since.as_nanos()).unwrap_or(u64::MAX)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::digest;

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
        let (read, _) =
            State::read_within(&mut file, &folders, &|path| path.starts_with("data/tiny/"))
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
        let kept = Candidate {
            key: digest::sha256(b"key"),
            tree: digest::sha256(b"tree"),
            pinned: digest::sha256(b"pinned"),
            files: vec![Member {
                path: "random.bin".to_owned(),
                size: 8,
                sha256: digest::sha256(b"8 bytes!"),
            }],
        };
        state.candidates.insert("clean".to_owned(), kept);
        assert_eq!(State::parse(&state.render()).candidates, state.candidates);
        state.candidates.clear();
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
                    racy_intact: std::sync::OnceLock::new(),
                },
            );
        }
        state.synced = Some(Synced {
            manifest: digest::sha256(b"manifest"),
            lock: digest::sha256(b"lock"),
            complete: true,
        });
        let kept = Candidate {
            key: digest::sha256(b"key"),
            tree: digest::sha256(b"tree"),
            pinned: digest::sha256(b"pinned"),
            files: Vec::new(),
        };
        state.candidates.insert("clean".to_owned(), kept);
        let bytes = state.render();
        let inside = |path: &str| path.starts_with("data/a/");
        let read = State::parse_only(&bytes, &inside);
        let mut expected = State::parse(&bytes);
        expected.files.retain(|path, _| inside(path));
        assert_eq!(read.files.len(), 2);
        assert_eq!(read.files, expected.files);
        assert_eq!(read.synced, state.synced);
        assert_eq!(read.candidates, state.candidates);
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
                State::read_within(&mut io::Cursor::new(&bytes), &folders, &|_| true).unwrap();
            assert!(within.files.is_empty() && within.synced.is_none());
        }
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
        assert_eq!(read.candidates, whole.candidates, "{case}");
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
            let bytes = state.render_in(block);
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

    /// a read within folders goes through the index, at any block size.
    #[test]
    fn a_read_within_folders_answers_as_a_whole_read() {
        let state = crowded();
        for block in [1, 300, 4096, usize::MAX] {
            let bytes = state.render_in(block);
            for folders in FOLDERS {
                let folders: Vec<String> = folders.iter().map(|&folder| folder.into()).collect();
                let case = format!("{folders:?} in blocks of {block}");
                let keep = |path: &str| inside(path, &folders);
                let mut file = io::Cursor::new(&bytes);
                let mut reading = Reading {
                    file: &mut file,
                    bytes: 0,
                };
                let read = indexed(&mut reading, &folders, &keep).expect(&case);
                assert_same(&read, &whole_within(&bytes, &folders), &case);
            }
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
        let bytes = crowded().render_in(300);
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
            for folders in &sets {
                let keep = |path: &str| inside(path, folders);
                let (read, _) =
                    State::read_within(&mut io::Cursor::new(state), folders, &keep).unwrap();
                let case = format!("change {number}, {folders:?}");
                assert_same(&read, &whole_within(state, folders), &case);
            }
        }
    }
}
