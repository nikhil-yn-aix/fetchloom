//! Paths of files inside a dataset.

use std::borrow::Cow;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use unicode_normalization::{UnicodeNormalization, is_nfc};

use crate::text;

/// The path of a file inside a dataset.
///
/// It is relative, separated by `/`, in unicode NFC form, and has no empty, `.` or `..`
/// component, no drive prefix, no backslash and no control character. The only way to make one
/// is through that validation, so a `DataPath` is always safe to join below a dataset directory
/// and compares equal on every platform. Paths order by their UTF-8 bytes.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DataPath(String);

impl DataPath {
    /// Validates `text` as a dataset path, refusing text that is not already NFC.
    ///
    /// # Errors
    ///
    /// Returns the [`PathError`] naming the first rule `text` breaks.
    pub fn new(text: &str) -> Result<Self, PathError> {
        validate(text)?;
        if !is_nfc(text) {
            return Err(PathError::NotNfc(text.to_owned()));
        }
        Ok(Self(text.to_owned()))
    }

    /// Converts `text` to NFC, then validates it, for paths as sources list them.
    ///
    /// # Errors
    ///
    /// Returns the [`PathError`] naming the first rule the normalized text breaks.
    pub fn normalize(text: &str) -> Result<Self, PathError> {
        let normal = if is_nfc(text) {
            Cow::Borrowed(text)
        } else {
            Cow::Owned(text.nfc().collect())
        };
        validate(&normal)?;
        Ok(Self(normal.into_owned()))
    }

    /// The path as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn validate(text: &str) -> Result<(), PathError> {
    let owned = || text.to_owned();
    if text.is_empty() {
        return Err(PathError::Empty);
    }
    if text.starts_with('/') {
        return Err(PathError::Absolute(owned()));
    }
    if let [letter, b':', ..] = text.as_bytes()
        && letter.is_ascii_alphabetic()
    {
        return Err(PathError::Drive(owned()));
    }
    if text.chars().any(char::is_control) {
        return Err(PathError::Control(owned()));
    }
    if text.contains('\\') {
        return Err(PathError::Backslash(owned()));
    }
    for component in text.split('/') {
        match component {
            "" => return Err(PathError::EmptyComponent(owned())),
            "." => return Err(PathError::Dot(owned())),
            ".." => return Err(PathError::DotDot(owned())),
            _ => {}
        }
    }
    Ok(())
}

impl fmt::Display for DataPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for DataPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)
    }
}

impl FromStr for DataPath {
    type Err = PathError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::new(text)
    }
}

impl Serialize for DataPath {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for DataPath {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        text::deserialize(deserializer)
    }
}

/// Why text is not a dataset path.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PathError {
    /// The text is empty.
    #[error("a dataset path cannot be empty")]
    Empty,
    /// The text starts with `/`.
    #[error("dataset path `{0}` is absolute")]
    Absolute(String),
    /// The text starts with a drive letter and a colon.
    #[error("dataset path `{0}` starts with a drive")]
    Drive(String),
    /// The text holds a backslash, which is a separator on windows.
    #[error("dataset path `{0}` contains a backslash")]
    Backslash(String),
    /// Two separators in a row, or one at the end.
    #[error("dataset path `{0}` has an empty component")]
    EmptyComponent(String),
    /// A `.` component.
    #[error("dataset path `{0}` has a `.` component")]
    Dot(String),
    /// A `..` component.
    #[error("dataset path `{0}` has a `..` component")]
    DotDot(String),
    /// A NUL, newline or other control character.
    #[error("dataset path {0:?} contains a control character")]
    Control(String),
    /// The text is not in unicode NFC form.
    #[error("dataset path `{0}` is not in unicode NFC form")]
    NotNfc(String),
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn accepts_relative_slash_separated_paths() {
        for text in [
            "a",
            "participants.tsv",
            "sub-01/eeg/sub-01_task-mi_eeg.edf",
            ".hidden/a.edf",
            "a..b/c",
            "...",
            "a/b c/d",
            "caf\u{e9}.txt",
            "cd:/x",
        ] {
            let path = DataPath::new(text).unwrap();
            assert_eq!(path.as_str(), text);
            assert_eq!(path.to_string(), text);
            assert_eq!(format!("{path:?}"), format!("{text:?}"));
            assert_eq!(text.parse::<DataPath>().unwrap(), path);
        }
    }

    #[test]
    fn refuses_each_unsafe_form_with_its_reason() {
        let cases = [
            ("", "a dataset path cannot be empty"),
            ("/a", "dataset path `/a` is absolute"),
            ("C:/a", "dataset path `C:/a` starts with a drive"),
            ("c:", "dataset path `c:` starts with a drive"),
            ("d:x", "dataset path `d:x` starts with a drive"),
            ("a\\b", "dataset path `a\\b` contains a backslash"),
            ("a//b", "dataset path `a//b` has an empty component"),
            ("a/", "dataset path `a/` has an empty component"),
            ("./a", "dataset path `./a` has a `.` component"),
            ("a/./b", "dataset path `a/./b` has a `.` component"),
            ("../a", "dataset path `../a` has a `..` component"),
            ("a/..", "dataset path `a/..` has a `..` component"),
            (
                "a\u{0}b",
                "dataset path \"a\\0b\" contains a control character",
            ),
            (
                "a\nb",
                "dataset path \"a\\nb\" contains a control character",
            ),
            (
                "a\u{7f}",
                "dataset path \"a\\u{7f}\" contains a control character",
            ),
            (
                "a\u{85}",
                "dataset path \"a\\u{85}\" contains a control character",
            ),
            (
                "cafe\u{301}.txt",
                "dataset path `cafe\u{301}.txt` is not in unicode NFC form",
            ),
        ];
        for (text, message) in cases {
            let err = DataPath::new(text).unwrap_err();
            assert_eq!(err.to_string(), message, "{text:?}");
        }
    }

    #[test]
    fn normalize_composes_to_nfc_before_validating() {
        let path = DataPath::normalize("cafe\u{301}.txt").unwrap();
        assert_eq!(path.as_str(), "caf\u{e9}.txt");
        assert_eq!(DataPath::normalize("a/b").unwrap().as_str(), "a/b");
        assert!(DataPath::normalize("../a").is_err());
    }

    proptest! {
        #[test]
        fn normalized_paths_always_validate(text in "\\PC{1,40}") {
            if let Ok(path) = DataPath::normalize(&text) {
                prop_assert_eq!(DataPath::new(path.as_str()), Ok(path.clone()));
            }
        }

        #[test]
        fn valid_paths_round_trip(text in "[a-z0-9._ -]{1,8}(/[a-z0-9_ -][a-z0-9._ -]{0,7}){0,4}") {
            if let Ok(path) = DataPath::new(&text) {
                prop_assert_eq!(path.to_string().parse::<DataPath>(), Ok(path));
            }
        }
    }
}
