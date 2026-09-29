//! The JSON document every command prints with `--json`.

use std::collections::BTreeMap;

use serde::{Serialize, Serializer};

use crate::event::Problem;

/// A command's JSON document, schema 1. Keys are never renamed within a schema.
#[derive(Clone, PartialEq, Eq, Debug, Serialize)]
pub struct Document<T> {
    schema: Schema,
    /// The command that ran, such as `sync`.
    pub command: String,
    /// How it ended.
    pub status: Status,
    /// The command's own result.
    pub result: T,
    /// Warnings, in the order they were raised.
    pub warnings: Vec<String>,
    /// Errors, in the order they were raised.
    pub errors: Vec<Problem>,
    /// What the command did.
    pub work: Work,
}

impl<T> Document<T> {
    /// A document with no warnings, errors or work yet.
    pub fn new(command: impl Into<String>, status: Status, result: T) -> Self {
        Self {
            schema: Schema,
            command: command.into(),
            status,
            result,
            warnings: Vec::new(),
            errors: Vec::new(),
            work: Work::default(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Schema;

impl Serialize for Schema {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u8(1)
    }
}

/// How a command ended.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// Everything asked for was done.
    Ok,
    /// Some of it was done.
    Partial,
    /// It stopped with an error.
    Error,
}

/// Counters of the work a command did.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize)]
pub struct Work {
    /// Network requests made.
    pub requests: u64,
    /// Bytes received.
    pub bytes_down: u64,
    /// Bytes sent.
    pub bytes_up: u64,
    /// Files placed in the project.
    pub files_linked: u64,
    /// Objects found, per tier.
    pub tier_hits: BTreeMap<String, u64>,
    /// Requests retried.
    pub retries: u64,
    /// Milliseconds spent, per phase of the command.
    pub phase_ms: BTreeMap<String, u64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorKind;

    #[test]
    fn serializes_in_the_documented_key_order() {
        let mut document = Document::new("sync", Status::Error, BTreeMap::<String, u64>::new());
        document.warnings.push("slow mirror".to_owned());
        document.errors.push(Problem {
            kind: ErrorKind::SelectionEmpty,
            message: "`*.edf` matched none of 12 files".to_owned(),
            help: None,
            subject: Some("eeg".to_owned()),
        });
        document.work.tier_hits.insert("local".to_owned(), 3);
        assert_eq!(
            serde_json::to_string(&document).unwrap(),
            concat!(
                r#"{"schema":1,"command":"sync","status":"error","result":{},"#,
                r#""warnings":["slow mirror"],"#,
                r#""errors":[{"kind":"selection.empty","message":"`*.edf` matched none of 12 files","help":null,"subject":"eeg"}],"#,
                r#""work":{"requests":0,"bytes_down":0,"bytes_up":0,"files_linked":0,"tier_hits":{"local":3},"retries":0,"phase_ms":{}}}"#,
            )
        );
    }
}
