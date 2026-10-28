//! the names inside an archive: made portable, checked, and compared the way disks compare them.

use std::collections::HashMap;
use std::collections::hash_map::Entry;

use unicode_normalization::{UnicodeNormalization, is_nfc};

use super::{refused, refused_member};
use crate::error::Error;
use crate::name;

/// what an entry turned out to be, once its name passed every check.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Named {
    /// a file to unpack, at this path: NFC, with `/` between folders.
    File(String),
    /// a folder. folders are implied by the paths of the files inside them.
    Folder,
}

/// every name one archive used so far, compared the way a case-insensitive disk compares them.
#[derive(Default)]
pub(super) struct Names {
    seen: HashMap<String, Seen>,
}

struct Seen {
    /// the path in NFC, when it differs from the key.
    path: Option<Box<str>>,
    /// the name as the archive spelled it, when that was not NFC.
    raw: Option<Box<str>>,
    is_folder: bool,
}

impl Names {
    /// checks an entry's name. `is_folder` is what the archive says the entry is; a name that
    /// ends with `/` is a folder too.
    pub(super) fn add(&mut self, raw: &str, is_folder: bool) -> Result<Named, Error> {
        let mut path = raw;
        while let Some(rest) = path.strip_prefix("./") {
            path = rest;
        }
        let is_folder = is_folder || path.ends_with('/');
        let path = path.strip_suffix('/').unwrap_or(path);
        if path.is_empty() || path == "." {
            return Ok(Named::Folder);
        }
        let nfc = if is_nfc(path) {
            path.to_owned()
        } else {
            path.nfc().collect()
        };
        if nfc.chars().any(char::is_control) {
            return Err(refused_member(
                &raw.escape_debug().to_string(),
                "it holds a control character, which no list of files can record safely",
            ));
        }
        if let Some(reason) = name::escapes(&nfc) {
            return Err(refused_member(raw, reason));
        }
        if let Some(reason) = name::unstorable(&nfc) {
            return Err(refused(format!(
                "the archive holds \"{raw}\", which this machine cannot store: {reason}"
            )));
        }
        for (at, _) in nfc.match_indices('/') {
            self.insert(&nfc[..at], None, true)?;
        }
        let raw = (raw != nfc).then_some(raw);
        self.insert(&nfc, raw, is_folder)?;
        Ok(if is_folder {
            Named::Folder
        } else {
            Named::File(nfc)
        })
    }

    fn insert(&mut self, path: &str, raw: Option<&str>, is_folder: bool) -> Result<(), Error> {
        let key = name::fold(path);
        let seen = match self.seen.entry(key) {
            Entry::Vacant(vacant) => {
                let path = (vacant.key() != path).then(|| path.into());
                vacant.insert(Seen {
                    path,
                    raw: raw.map(Into::into),
                    is_folder,
                });
                return Ok(());
            }
            Entry::Occupied(occupied) => occupied,
        };
        let old = seen.get();
        let old_path = old.path.as_deref().unwrap_or(seen.key());
        if old_path != path {
            return Err(refused(format!(
                "the archive holds \"{old_path}\" and \"{path}\", which are one name on disks that ignore case"
            )));
        }
        match (old.is_folder, is_folder) {
            (true, true) => Ok(()),
            (false, false) if old.raw.as_deref().unwrap_or(old_path) == raw.unwrap_or(path) => {
                Err(refused(format!("the archive holds \"{path}\" twice")))
            }
            (false, false) => Err(refused(format!(
                "the archive holds \"{path}\" twice, spelled in two unicode forms"
            ))),
            _ => Err(refused(format!(
                "the archive holds \"{path}\" both as a file and as a folder"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn add_all(names: &[&str]) -> Result<Vec<Named>, String> {
        let mut seen = Names::default();
        names
            .iter()
            .map(|name| seen.add(name, false).map_err(|err| err.message))
            .collect()
    }

    fn file(path: &str) -> Named {
        Named::File(path.to_owned())
    }

    #[test]
    fn a_leading_dot_slash_is_dropped() {
        assert_eq!(
            add_all(&["./", "./a.csv", "././b/c.csv"]),
            Ok(vec![Named::Folder, file("a.csv"), file("b/c.csv")])
        );
    }

    #[test]
    fn a_trailing_slash_makes_a_folder() {
        assert_eq!(
            add_all(&["data/", "data/a.csv"]),
            Ok(vec![Named::Folder, file("data/a.csv")])
        );
    }

    #[test]
    fn names_become_nfc() {
        assert_eq!(
            add_all(&["cafe\u{301}.txt"]),
            Ok(vec![file("caf\u{e9}.txt")])
        );
    }

    #[test]
    fn names_that_escape_are_refused() {
        for bad in [
            "/etc/passwd",
            "../x",
            "a/../../x",
            "a\\b.txt",
            "C:x",
            "a//b",
        ] {
            let err = add_all(&[bad]).unwrap_err();
            assert!(err.contains(bad), "{err}");
        }
    }

    #[test]
    fn control_characters_are_refused() {
        let err = add_all(&["a\tb.txt"]).unwrap_err();
        assert!(err.contains("a\\tb.txt"), "{err}");
        assert!(add_all(&["a\nb"]).is_err());
    }

    #[test]
    fn names_this_machine_cannot_store_are_refused() {
        assert!(add_all(&[&"x".repeat(256)]).is_err());
        assert_eq!(add_all(&["data/aux.csv"]).is_err(), cfg!(windows));
        assert_eq!(add_all(&["notes."]).is_err(), cfg!(windows));
    }

    #[test]
    fn a_duplicate_is_refused() {
        let err = add_all(&["a.csv", "b.csv", "a.csv"]).unwrap_err();
        assert_eq!(err, "the archive holds \"a.csv\" twice");
    }

    #[test]
    fn names_that_differ_only_in_case_are_refused() {
        let err = add_all(&["Data.csv", "data.csv"]).unwrap_err();
        assert!(err.contains("\"Data.csv\" and \"data.csv\""), "{err}");
        assert!(add_all(&["\u{3c3}.txt", "\u{3c2}.txt"]).is_err(), "sigmas");
        assert!(add_all(&["K.txt", "\u{212a}.txt"]).is_err(), "kelvin");
    }

    #[test]
    fn folders_that_differ_only_in_case_are_refused() {
        assert!(add_all(&["Data/a.csv", "data/b.csv"]).is_err());
        assert!(add_all(&["data/a.csv", "data/b.csv"]).is_ok());
    }

    #[test]
    fn names_equal_after_nfc_are_refused() {
        let err = add_all(&["caf\u{e9}.txt", "cafe\u{301}.txt"]).unwrap_err();
        assert!(err.contains("two unicode forms"), "{err}");
        assert!(
            add_all(&["caf\u{e9}/a.txt", "cafe\u{301}/b.txt"]).is_ok(),
            "one folder however it is spelled"
        );
    }

    #[test]
    fn a_name_cannot_be_a_file_and_a_folder() {
        for names in [&["a", "a/b"], &["a/b", "a"], &["a/", "a"], &["A", "a/b"]] {
            assert!(add_all(names).is_err(), "{names:?}");
        }
    }
}
