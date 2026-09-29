//! Events: what happened during a command, in one stream that every reporter renders.

use std::time::Duration;

use serde::Serialize;

use crate::error::ErrorKind;
use crate::name::Name;
use crate::reference::Reference;

/// One thing that happened, in the order it happened.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Event {
    /// A dataset's reference resolved to a listing.
    Resolved {
        /// The dataset.
        dataset: Name,
        /// The exact version it resolved to.
        resolved: Reference,
        /// The title the source states.
        title: Option<String>,
        /// How many files were selected.
        files: u64,
        /// Their total size in bytes.
        bytes: u64,
        /// The license the source states.
        license: Option<String>,
    },
    /// A transfer began.
    TransferStarted {
        /// Identifies the transfer within the command.
        id: u64,
        /// The host the bytes come from.
        host: String,
        /// The size, when known before the transfer.
        bytes: Option<u64>,
    },
    /// A transfer received more bytes.
    TransferAdvanced {
        /// Identifies the transfer within the command.
        id: u64,
        /// Bytes received so far.
        bytes: u64,
    },
    /// A transfer completed.
    TransferFinished {
        /// Identifies the transfer within the command.
        id: u64,
        /// Bytes received in all.
        bytes: u64,
        /// How long it took.
        elapsed: Duration,
    },
    /// `data.lock` was written.
    Locked {
        /// How many files it lists.
        files: u64,
    },
    /// A dataset was placed in the project.
    Linked {
        /// The dataset.
        dataset: Name,
        /// Where it was placed, relative to the project root.
        path: String,
        /// How long it took.
        elapsed: Duration,
        /// Disk space used beyond the store, in bytes.
        extra_bytes: u64,
    },
    /// A dataset needed no work.
    Unchanged {
        /// The dataset.
        dataset: Name,
    },
    /// Something the user should know that did not stop the command.
    Warning(String),
    /// A tip teaching something the user has not done yet.
    Hint(String),
    /// Something that stopped part or all of the command.
    Failed(Problem),
}

/// An error as reporters show it and as JSON documents carry it.
#[derive(Clone, PartialEq, Eq, Debug, Serialize)]
pub struct Problem {
    /// Its kind.
    pub kind: ErrorKind,
    /// What happened, in one sentence.
    pub message: String,
    /// The exact next step, when there is one.
    pub help: Option<String>,
    /// The path or dataset it concerns.
    pub subject: Option<String>,
}
