//! `.fetchloom/state`: every file fetchloom placed and how it looked right after, so `status` and
//! `sync` notice hand edits without hashing anything.

use std::collections::BTreeMap;
use std::fs::Metadata;
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};

use crate::digest::Sha256;
use crate::place::Method;

const VERSION: u32 = 1;

#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct State {
    version: u32,
    /// keyed by the file's path in the project, with `/` between folders.
    pub(crate) files: BTreeMap<String, Placed>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Placed {
    pub(crate) dataset: String,
    pub(crate) sha256: Sha256,
    pub(crate) method: Method,
    pub(crate) look: Look,
    /// a file of its own that may be written to, as `copy = true` asks. a state written before
    /// this was recorded reads as false, so such a copy is placed once more.
    #[serde(default)]
    pub(crate) writable: bool,
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
        }
        let state = Written {
            version: VERSION,
            files: &self.files,
        };
        serde_json::to_vec(&state).unwrap_or_default()
    }
}

impl Look {
    pub(crate) fn of(metadata: &Metadata) -> Look {
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |since| {
                u64::try_from(since.as_nanos()).unwrap_or(u64::MAX)
            });
        Look {
            size: metadata.len(),
            modified,
        }
    }
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
            },
        );
        let read = State::parse(&state.render());
        assert_eq!(read.files, state.files);
        assert!(State::parse(b"not json").files.is_empty());
        assert!(
            State::parse(br#"{"version": 9, "files": {}}"#)
                .files
                .is_empty()
        );
    }

    #[test]
    fn a_state_from_before_writable_copies_reads_them_as_read_only() {
        let text = format!(
            r#"{{"version":1,"files":{{"data/a.csv":{{"dataset":"a","sha256":"{}","method":"copy","look":{{"size":1,"modified":2}}}}}}}}"#,
            digest::sha256(b"x")
        );
        let read = State::parse(text.as_bytes());
        assert!(!read.files["data/a.csv"].writable);
    }
}
