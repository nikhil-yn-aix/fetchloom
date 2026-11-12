//! `.fetchloom/state`: every file fetchloom placed and how it looked right after, so `status` and
//! `sync` notice hand edits without hashing, except for a file as new as the state itself, whose
//! size and time a coarse clock cannot vouch for. it also keeps each rebuild of a recipe that
//! differs from data.lock until a person accepts it or data.toml drops the recipe.

use std::collections::BTreeMap;
use std::fs::Metadata;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::cache::Member;
use crate::digest::Sha256;
use crate::place::Method;
use crate::project::lock::derived::Derived;
use crate::project::manifest::Manifest;

const VERSION: u32 = 1;

#[derive(Debug, Default, Deserialize)]
pub(crate) struct State {
    version: u32,
    /// keyed by the file's path in the project, with `/` between folders.
    pub(crate) files: BTreeMap<String, Placed>,
    /// data.toml and data.lock as the last sync left them.
    #[serde(default)]
    pub(crate) synced: Option<Synced>,
    /// the rebuild of each recipe that made other files than data.lock pins for its key, kept
    /// until `fetchloom build --accept` pins it, a build pins something else or data.toml no
    /// longer has the recipe.
    #[serde(default)]
    pub(crate) candidates: BTreeMap<String, Candidate>,
}

/// a rebuild of a recipe that made other files than data.lock pins for the same key. its files
/// are in the cache, but it is no result a build may reuse: only a person pins it, with
/// `fetchloom build --accept`.
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

    pub(crate) fn render(&self) -> Vec<u8> {
        let state = Written {
            version: VERSION,
            files: &self.files,
            synced: self.synced,
            candidates: &self.candidates,
        };
        serde_json::to_vec(&state).unwrap_or_default()
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

#[derive(Serialize)]
struct Written<'a> {
    version: u32,
    files: &'a BTreeMap<String, Placed>,
    #[serde(skip_serializing_if = "Option::is_none")]
    synced: Option<Synced>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    candidates: &'a BTreeMap<String, Candidate>,
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
            },
        );
        let read = State::parse(&state.render());
        assert_eq!(read.files, state.files);
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

    /// a read of some records keeps them, and the rest of the state, as a read of all would.
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
            br#"{"version": 1}"#,
            br#"{"version": 1, "files": {}, "files": {}}"#,
            br#"{"version": 1, "files": {}} trailing"#,
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
            },
        );
        state.synced = Some(Synced {
            manifest: digest::sha256(b"manifest"),
            lock: digest::sha256(b"lock"),
            complete: true,
        });
        let mut stored: serde_json::Value = serde_json::from_slice(&state.render()).unwrap();
        for version in [0, VERSION + 1, u32::MAX] {
            stored["version"] = serde_json::json!(version);
            let read = State::parse(&serde_json::to_vec(&stored).unwrap());
            assert!(
                read.files.is_empty(),
                "version {version} cannot vouch for files"
            );
            assert!(
                read.synced.is_none(),
                "version {version} cannot claim a complete sync"
            );
        }
    }
}
