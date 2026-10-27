//! failures, in a form both people and scripts can read.

use std::fmt;

/// a stable name for each kind of failure, and the exit code it ends with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Usage,
}

impl Kind {
    /// the exit code a run ends with when this kind stops it.
    pub(crate) fn exit_code(self) -> u8 {
        match self {
            Kind::Usage => 2,
        }
    }
}

/// a failure: what happened in one sentence, supporting lines, and what to do next.
#[derive(Debug)]
pub(crate) struct Error {
    pub(crate) kind: Kind,
    pub(crate) message: String,
    pub(crate) details: Vec<String>,
    pub(crate) help: Option<String>,
}

impl Error {
    pub(crate) fn new(kind: Kind, message: impl Into<String>) -> Error {
        Error {
            kind,
            message: message.into(),
            details: Vec::new(),
            help: None,
        }
    }

    pub(crate) fn help(mut self, help: impl Into<String>) -> Error {
        self.help = Some(help.into());
        self
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}
