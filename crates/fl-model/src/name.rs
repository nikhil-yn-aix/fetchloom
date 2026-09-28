//! Names of datasets and steps.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::text;

/// The name of a dataset or a step: one or more of `a-z`, `0-9`, `-` and `_`.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct Name(String);

impl Name {
    /// Validates `text` as a name.
    ///
    /// # Errors
    ///
    /// Returns [`NameError`] when `text` is empty or holds another character.
    pub fn new(text: &str) -> Result<Self, NameError> {
        if text.is_empty() {
            return Err(NameError::Empty);
        }
        let allowed = |byte: &u8| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_');
        if !text.as_bytes().iter().all(allowed) {
            return Err(NameError::Characters(text.to_owned()));
        }
        Ok(Self(text.to_owned()))
    }

    /// The name as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Name {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for Name {
    type Err = NameError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::new(text)
    }
}

impl Serialize for Name {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Name {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        text::deserialize(deserializer)
    }
}

/// Why text is not a name.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NameError {
    /// The text is empty.
    #[error("a name cannot be empty")]
    Empty,
    /// The text holds a character outside `a-z0-9-_`.
    #[error("name `{0}` may hold only a-z, 0-9, `-` and `_`")]
    Characters(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_lowercase_digits_dash_and_underscore() {
        for text in ["eeg", "grch38", "motor-imagery", "a_b", "0", "-", "_x"] {
            let name: Name = text.parse().unwrap();
            assert_eq!(name.as_str(), text);
            assert_eq!(name.to_string(), text);
        }
    }

    #[test]
    fn refuses_anything_else() {
        assert_eq!(
            Name::new("").unwrap_err().to_string(),
            "a name cannot be empty"
        );
        for text in ["EEG", "a b", "a.b", "a/b", "caf\u{e9}", "a:b"] {
            assert_eq!(
                Name::new(text).unwrap_err().to_string(),
                format!("name `{text}` may hold only a-z, 0-9, `-` and `_`")
            );
        }
    }
}
