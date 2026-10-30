//! `.fetchloom/state`: every file fetchloom placed and how it looked right after, so `status` and
//! `sync` notice hand edits without hashing, except for a file as new as the state itself, whose
//! size and time a coarse clock cannot vouch for.

use std::collections::BTreeMap;
use std::fs::Metadata;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::digest::Sha256;
use crate::place::Method;

const VERSION: u32 = 1;

#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct State {
    version: u32,
    /// keyed by the file's path in the project, with `/` between folders.
    pub(crate) files: BTreeMap<String, Placed>,
    /// data.toml and data.lock as the last sync left them.
    #[serde(default)]
    pub(crate) synced: Option<Synced>,
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

    pub(crate) fn render(&self) -> Vec<u8> {
        #[derive(Serialize)]
        struct Written<'a> {
            version: u32,
            files: &'a BTreeMap<String, Placed>,
            #[serde(skip_serializing_if = "Option::is_none")]
            synced: Option<Synced>,
        }
        let state = Written {
            version: VERSION,
            files: &self.files,
            synced: self.synced,
        };
        serde_json::to_vec(&state).unwrap_or_default()
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
            },
        );
        let read = State::parse(&state.render());
        assert_eq!(read.files, state.files);
        assert_eq!(read.synced, None, "a state from before syncs were recorded");
        state.synced = Some(Synced {
            manifest: digest::sha256(b"data.toml"),
            lock: digest::sha256(b"data.lock"),
            complete: true,
        });
        assert_eq!(State::parse(&state.render()).synced, state.synced);
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
}
