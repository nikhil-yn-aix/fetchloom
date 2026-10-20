//! The record of an unfinished download, kept beside its bytes so a later run can continue it.

use serde::{Deserialize, Serialize};

/// What an unfinished download received, and what identified its bytes.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct PartialRecord {
    /// Where the bytes came from, in the redacted form logs show.
    pub location: String,
    /// The length of the whole file, when the server stated it.
    pub length: Option<u64>,
    /// What the server said identifies the bytes.
    pub validator: Validator,
    /// The bytes received and written from the start of the file. Anything after them is
    /// discarded.
    pub received: u64,
}

/// What a server said identifies the bytes it sent.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Validator {
    /// Nothing.
    None,
    /// A strong `ETag`: the bytes change whenever it does.
    StrongEtag(String),
    /// A weak `ETag`, which may stay the same when the bytes change.
    WeakEtag(String),
    /// Only `Last-Modified`.
    LastModified(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_record_keeps_its_stored_shape() {
        let record = PartialRecord {
            location: "https://example.org/a.bin?sig=***".to_owned(),
            length: Some(10),
            validator: Validator::StrongEtag("\"v1\"".to_owned()),
            received: 4,
        };
        let text = serde_json::to_string(&record).unwrap();
        assert_eq!(
            text,
            r#"{"location":"https://example.org/a.bin?sig=***","length":10,"validator":{"kind":"strong_etag","value":"\"v1\""},"received":4}"#
        );
        assert_eq!(
            serde_json::from_str::<PartialRecord>(&text).unwrap(),
            record
        );
        let none: PartialRecord = serde_json::from_str(
            r#"{"location":"x","length":null,"validator":{"kind":"none"},"received":0}"#,
        )
        .unwrap();
        assert_eq!(none.validator, Validator::None);
    }
}
