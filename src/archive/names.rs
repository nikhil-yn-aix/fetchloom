//! the names inside an archive: made portable, checked, and compared the way disks compare them.

use std::collections::HashMap;
use std::collections::hash_map::Entry;

use icu_normalizer::ComposingNormalizerBorrowed;

use super::{refused, refused_member};
use crate::error::Error;
use crate::{name, units};

/// the longest path an entry may have, in bytes, both as the archive spells it and in NFC. linux
/// takes no longer path either.
pub(super) const LONGEST_PATH: usize = 4096;

/// the most one archive's names may take: each entry counts the length of its path, and each
/// folder or file seen for the first time its own name plus [`PER_NAME`].
const NAME_BYTES: u64 = 256 << 20;

/// roughly what keeping one folder or file costs besides the bytes of its name.
const PER_NAME: u64 = 64;

/// what an entry turned out to be, once its name passed every check.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Named {
    /// a file to unpack, at this path: NFC, with `/` between folders.
    File(String),
    /// a file a system writes by itself, such as finder's `.DS_Store` or one under `__MACOSX`,
    /// at this path. it is no data, so it is left out, as a folder import leaves it out.
    System(String),
    /// a folder. folders are implied by the paths of the files inside them.
    Folder,
}

/// every name one archive used so far, compared the way a case-insensitive disk compares them.
/// folders form a tree, so a path keeps each of its names once rather than every folder above it
/// whole.
pub(super) struct Names {
    /// every folder and file, by the id of the folder holding it and its own name folded. the
    /// archive's root is folder 0.
    seen: HashMap<(usize, Box<str>), Seen>,
    /// what the names took so far, counted as [`NAME_BYTES`] says.
    kept: u64,
    budget: u64,
}

struct Seen {
    id: usize,
    /// the name in NFC, when it differs from the key.
    name: Option<Box<str>>,
    /// the whole path as the archive spelled it, when that was not NFC.
    raw: Option<Box<str>>,
    is_folder: bool,
}

impl Default for Names {
    fn default() -> Names {
        Names::with_budget(NAME_BYTES)
    }
}

impl Names {
    fn with_budget(budget: u64) -> Names {
        Names {
            seen: HashMap::new(),
            kept: 0,
            budget,
        }
    }

    /// checks an entry's name. `is_folder` is what the archive says the entry is; a name that
    /// ends with `/` is a folder too. a name a system writes by itself takes no place in the tree,
    /// since nothing is unpacked at it.
    pub(super) fn add(&mut self, raw: &str, is_folder: bool) -> Result<Named, Error> {
        if raw.len() > LONGEST_PATH {
            return Err(too_long(raw));
        }
        let mut path = raw;
        while let Some(rest) = path.strip_prefix("./") {
            path = rest;
        }
        let is_folder = is_folder || path.ends_with('/');
        let path = path.strip_suffix('/').unwrap_or(path);
        if path.is_empty() || path == "." {
            return Ok(Named::Folder);
        }
        let nfc = ComposingNormalizerBorrowed::new_nfc()
            .normalize(path)
            .into_owned();
        if nfc.len() > LONGEST_PATH {
            return Err(too_long(raw));
        }
        if nfc.chars().any(name::is_control) {
            return Err(refused_member(
                &raw.escape_debug().to_string(),
                "it holds a control character, which no list of files can record safely",
            ));
        }
        if let Some(reason) = name::escapes(&nfc) {
            return Err(refused_member(raw, reason));
        }
        if nfc.split('/').any(name::made_by_system) {
            self.kept = self.kept.saturating_add(nfc.len() as u64);
            self.check_budget()?;
            return Ok(if is_folder {
                Named::Folder
            } else {
                Named::System(nfc)
            });
        }
        if let Some(reason) = name::unstorable(&nfc) {
            return Err(refused(format!(
                "the archive holds \"{}\", which this machine cannot store: {reason}",
                name::abridged(raw)
            )));
        }
        self.kept = self.kept.saturating_add(nfc.len() as u64);
        self.check_budget()?;
        let mut folder = 0;
        let mut start = 0;
        for (end, _) in nfc.match_indices('/') {
            folder = self.insert(folder, &nfc[..end], start, None, true)?;
            start = end + 1;
        }
        let raw = (raw != nfc).then_some(raw);
        self.insert(folder, &nfc, start, raw, is_folder)?;
        Ok(if is_folder {
            Named::Folder
        } else {
            Named::File(nfc)
        })
    }

    /// the path in NFC of the file an earlier entry named `raw`, a hardlink's target as the
    /// archive spells it, when one did, spelled the same.
    pub(super) fn earlier_file(&self, raw: &str) -> Option<String> {
        let mut path = raw;
        while let Some(rest) = path.strip_prefix("./") {
            path = rest;
        }
        let nfc = ComposingNormalizerBorrowed::new_nfc()
            .normalize(path)
            .into_owned();
        let mut folder = 0;
        let mut parts = nfc.split('/').peekable();
        while let Some(part) = parts.next() {
            let (key, seen) = self
                .seen
                .get_key_value(&(folder, name::fold(part).into_boxed_str()))?;
            if seen.name.as_deref().unwrap_or(&key.1) != part {
                return None;
            }
            if parts.peek().is_none() {
                return (!seen.is_folder).then_some(nfc);
            }
            folder = seen.id;
        }
        None
    }

    /// adds the last name in `path`, which starts at byte `at`, to the folder `folder`, and
    /// returns its id.
    fn insert(
        &mut self,
        folder: usize,
        path: &str,
        at: usize,
        raw: Option<&str>,
        is_folder: bool,
    ) -> Result<usize, Error> {
        let name = &path[at..];
        let id = self.seen.len() + 1;
        let key = (folder, name::fold(name).into_boxed_str());
        let seen = match self.seen.entry(key) {
            Entry::Vacant(vacant) => {
                let spelled = (*vacant.key().1 != *name).then(|| name.into());
                let cost = PER_NAME + (name.len() + raw.map_or(0, str::len)) as u64;
                self.kept = self.kept.saturating_add(cost);
                vacant.insert(Seen {
                    id,
                    name: spelled,
                    raw: raw.map(Into::into),
                    is_folder,
                });
                return self.check_budget().map(|()| id);
            }
            Entry::Occupied(occupied) => occupied,
        };
        let old = seen.get();
        let old_name = old.name.as_deref().unwrap_or(&*seen.key().1);
        if old_name != name {
            let old_path = format!("{}{old_name}", &path[..at]);
            return Err(refused(format!(
                "the archive holds \"{old_path}\" and \"{path}\", which are one name on disks that ignore case"
            )));
        }
        match (old.is_folder, is_folder) {
            (true, true) => Ok(old.id),
            (false, false) if old.raw.as_deref().unwrap_or(path) == raw.unwrap_or(path) => {
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

    fn check_budget(&self) -> Result<(), Error> {
        if self.kept > self.budget {
            return Err(refused(format!(
                "the archive's names take more than {}, the most fetchloom keeps for one archive",
                units::size(self.budget)
            )));
        }
        Ok(())
    }
}

/// a path too long to take, shown by its start only.
fn too_long(raw: &str) -> Error {
    let start: String = raw.chars().take(60).collect();
    refused(format!(
        "the archive holds \"{}…\", a path longer than {}, the longest fetchloom takes",
        start.escape_debug(),
        units::count(LONGEST_PATH as u64, "byte", "bytes")
    ))
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

    /// what a system makes is named apart only once the checks every name passes let it through,
    /// and takes no place in the tree, so it clashes with nothing.
    #[test]
    fn what_a_system_makes_is_named_apart_after_the_checks() {
        assert_eq!(
            add_all(&["__MACOSX/", "__MACOSX/a/._b.csv", "a/.DS_Store", "a/b.csv"]),
            Ok(vec![
                Named::Folder,
                Named::System("__MACOSX/a/._b.csv".to_owned()),
                Named::System("a/.DS_Store".to_owned()),
                file("a/b.csv"),
            ])
        );
        assert!(add_all(&["__MACOSX/../../x"]).is_err());
        assert!(add_all(&["a/.DS_Store", "a/.DS_Store", "A/x.csv"]).is_ok());
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
    fn bidi_controls_are_refused_in_members() {
        let result = add_all(&["report\u{202e}txt.exe"]);
        assert!(result.is_err(), "accepted bidi archive member");
        let error = result.unwrap_err();
        assert!(error.contains("control character"), "{error}");
        assert!(error.contains("report\\u{202e}txt.exe"), "{error}");
        assert!(!error.contains('\u{202e}'), "{error}");
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

    impl Names {
        /// the bytes of text kept for every name added so far.
        fn stored(&self) -> usize {
            self.seen
                .iter()
                .map(|((_, key), seen)| {
                    key.len()
                        + seen.name.as_deref().map_or(0, str::len)
                        + seen.raw.as_deref().map_or(0, str::len)
                })
                .sum()
        }
    }

    #[test]
    fn what_names_keep_grows_with_their_length() {
        let mut names = Names::default();
        let mut added = 0;
        for top in ["a", "b", "c", "d"] {
            let deep = format!("{top}/{}", vec!["x"; 2000].join("/"));
            added += deep.len();
            names.add(&deep, false).unwrap();
        }
        assert!(
            names.stored() <= 2 * added,
            "{} bytes kept for {added} bytes of names",
            names.stored()
        );
    }

    #[test]
    fn a_path_longer_than_4096_bytes_is_refused() {
        let longest = format!("{}x", vec!["x"; 2048].join("/"));
        assert_eq!(longest.len(), 4096);
        assert!(add_all(&[&longest]).is_ok(), "4,096 bytes fit");
        let err = add_all(&[&format!("{longest}y")]).unwrap_err();
        assert!(err.contains("longer than 4,096 bytes"), "{err}");
        let dotted = format!("{}a", "./".repeat(2048));
        assert!(
            add_all(&[&dotted]).is_err(),
            "the name as the archive spells it"
        );
    }

    /// the limit holds for a name as it unpacks too: U+0958 takes 3 bytes as spelled and 6 once
    /// NFC splits it in two, so 33 parts of 40 fit as spelled, in 3,992 bytes, and unpacked, in
    /// 7,952, do not.
    #[test]
    fn a_path_that_nfc_lengthens_past_4096_bytes_is_refused() {
        let part = "\u{958}".repeat(40);
        let grows = vec![part.as_str(); 33].join("/");
        assert_eq!(grows.len(), 3992);
        let Err(err) = add_all(&[&grows]) else {
            panic!("a path of 7,952 bytes once unpacked was taken");
        };
        assert!(err.contains("longer than 4,096 bytes"), "{err}");
    }

    #[test]
    fn names_past_their_budget_refuse_the_archive() {
        let mut names = Names::with_budget(1000);
        names.add("data/a.csv", false).unwrap();
        for _ in 0..100 {
            names.add("data/", true).unwrap();
        }
        let err = (0..100)
            .find_map(|n| names.add(&format!("data/{n}.csv"), false).err())
            .unwrap();
        assert_eq!(err.kind, crate::error::Kind::SourceInvalid);
        assert_eq!(
            err.message,
            "the archive's names take more than 1000 B, the most fetchloom keeps for one archive"
        );
    }

    #[test]
    fn a_name_cannot_be_a_file_and_a_folder() {
        for names in [&["a", "a/b"], &["a/b", "a"], &["a/", "a"], &["A", "a/b"]] {
            assert!(add_all(names).is_err(), "{names:?}");
        }
    }

    /// any names, in any order: every file taken is a safe path in NFC that reads back as itself,
    /// and no two files, or a file and a folder, are one name on a disk that ignores case.
    #[test]
    fn fuzz_member_names() {
        bolero::check!()
            .with_type::<Vec<(Vec<u8>, bool)>>()
            .for_each(|entries| {
                let mut names = Names::default();
                let mut files = HashMap::new();
                let mut folders = std::collections::HashSet::new();
                for (raw, is_folder) in entries {
                    let raw = String::from_utf8_lossy(raw);
                    let Ok(Named::File(path)) = names.add(&raw, *is_folder) else {
                        continue;
                    };
                    assert_eq!(name::escapes(&path), None, "{raw:?} became {path:?}");
                    assert_eq!(name::unstorable(&path), None, "{raw:?} became {path:?}");
                    assert!(!path.contains(name::is_control), "{path:?}");
                    assert_eq!(
                        ComposingNormalizerBorrowed::new_nfc().normalize(&path),
                        path
                    );
                    assert_eq!(
                        Names::default()
                            .add(&path, false)
                            .map_err(|err| err.message),
                        Ok(Named::File(path.clone()))
                    );
                    let key = name::fold(&path);
                    assert!(!folders.contains(&key), "{path:?} is also a folder");
                    assert_eq!(files.insert(key, path.clone()), None, "{path:?}");
                    for (at, _) in path.match_indices('/') {
                        let folder = name::fold(&path[..at]);
                        assert!(!files.contains_key(&folder), "{path:?} is inside a file");
                        folders.insert(folder);
                    }
                }
            });
    }
}
