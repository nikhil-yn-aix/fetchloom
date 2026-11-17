//! datasets data.toml renamed: a new name whose table pins what a gone name's entry pins takes that
//! entry over, so a rename never asks the source again or pins other bytes.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashSet};

use super::{Scope, Upgrade};
use crate::project::lock::{Lock, Locked};
use crate::project::manifest::Manifest;

/// an entry of data.lock carried over to the name data.toml gives it now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::cli) struct Rename {
    pub(in crate::cli) old: String,
    pub(in crate::cli) new: String,
    /// whether the old entry went: a run of named datasets that leaves the old name out keeps
    /// its entry until a run of every dataset.
    pub(super) moved: bool,
}

/// `lock` with its entries carried to the names data.toml gives them now, as a relock of every
/// dataset would, so status agrees with what a sync keeps; other entries stay as they are.
pub(in crate::cli) fn forecast(manifest: &Manifest, lock: Lock) -> (Lock, Vec<Rename>) {
    let mut datasets = Vec::new();
    let mut others = lock.datasets;
    // a dataset waiting for its list keeps its own pin
    let waiting: HashSet<&str> = lock
        .waiting
        .iter()
        .map(|waiting| waiting.name.as_str())
        .collect();
    let renamed = carry(
        manifest,
        &Scope::All,
        &Upgrade::Nothing,
        &waiting,
        &mut datasets,
        &mut others,
    );
    datasets.extend(others);
    datasets.sort_by(|a, b| a.name.cmp(&b.name));
    let stated = lock.stated;
    (
        Lock {
            datasets,
            stated,
            waiting: lock.waiting,
        },
        renamed,
    )
}

struct Match<'a> {
    old: Option<&'a str>,
    new: Option<&'a str>,
    repeated: bool,
}

fn pairs_by_key<'a, K: Ord>(
    old: impl Iterator<Item = (K, &'a str)>,
    new: impl Iterator<Item = (K, &'a str)>,
) -> BTreeMap<String, String> {
    let mut new = new.peekable();
    if new.peek().is_none() {
        return BTreeMap::new();
    }
    let mut groups = BTreeMap::<K, Match<'a>>::new();
    for (key, name) in old {
        groups
            .entry(key)
            .and_modify(|pair| pair.old = None)
            .or_insert(Match {
                old: Some(name),
                new: None,
                repeated: false,
            });
    }
    if groups.is_empty() {
        return BTreeMap::new();
    }
    for (key, name) in new {
        if let Some(pair) = groups.get_mut(&key) {
            pair.repeated |= pair.new.replace(name).is_some();
        }
    }
    groups
        .into_values()
        .filter_map(|pair| {
            if pair.repeated {
                return None;
            }
            Some((pair.old?.to_owned(), pair.new?.to_owned()))
        })
        .collect()
}

fn rename_to(name: &mut String, new: &str, moved: bool) -> Rename {
    Rename {
        old: std::mem::replace(name, new.to_owned()),
        new: new.to_owned(),
        moved,
    }
}

/// carries the entries of renamed fetched datasets into `kept` under their new names, for new names
/// the run may pin, matched one to one by spec and `extract`: one from `others` moves, and one from
/// `kept`, which a named run leaves alone, is copied. both keep data.lock's name order. an import
/// stays: every import's table reads `ref = "local"`, so a match tells no rename from a new table.
pub(super) fn carry(
    manifest: &Manifest,
    scope: &Scope,
    upgrade: &Upgrade,
    given: &HashSet<&str>,
    kept: &mut Vec<Locked>,
    others: &mut Vec<Locked>,
) -> Vec<Rename> {
    if kept.is_empty() && others.is_empty() {
        return Vec::new();
    }
    let pinned = |name: &str| {
        [kept.as_slice(), others.as_slice()]
            .into_iter()
            .any(|pins| {
                pins.binary_search_by(|locked| locked.name.as_str().cmp(name))
                    .is_ok()
            })
    };
    let old = kept
        .iter()
        .chain(others.iter())
        .filter(|locked| {
            !manifest.datasets.contains_key(&locked.name)
                && !crate::source::local::is_imported(&locked.reference)
        })
        .map(|locked| {
            (
                (Cow::Borrowed(locked.spec.as_str()), locked.extract),
                locked.name.as_str(),
            )
        });
    let new = manifest.datasets.iter().filter_map(|(name, entry)| {
        let source = &entry.source;
        (scope.includes(name)
            && !upgrade.includes(name)
            && !given.contains(name.as_str())
            && !pinned(name))
        .then(|| {
            (
                (
                    Cow::Owned(crate::project::lock::spec(source)),
                    source.extract,
                ),
                name.as_str(),
            )
        })
    });
    let matches = pairs_by_key(old, new);
    if matches.is_empty() {
        return Vec::new();
    }
    let mut renamed = Vec::with_capacity(matches.len());
    for index in 0..kept.len() {
        if let Some(new) = matches.get(&kept[index].name) {
            let mut copied = kept[index].clone();
            renamed.push(rename_to(&mut copied.name, new, false));
            kept.push(copied);
        }
    }
    for mut locked in others.extract_if(.., |locked| matches.contains_key(&locked.name)) {
        let new = &matches[&locked.name];
        renamed.push(rename_to(&mut locked.name, new, true));
        kept.push(locked);
    }
    kept.sort_by(|a, b| a.name.cmp(&b.name));
    renamed.sort_by(|a, b| a.new.cmp(&b.new));
    renamed
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt::Write as _;

    /// a name pairs with the one old name that answers it, and with none when either side has
    /// several that answer.
    #[test]
    fn only_one_to_one_answers_pair() {
        let old = [("one", "iris"), ("old", "a"), ("old", "b"), ("new", "d")];
        let new = [
            ("one", "flowers"),
            ("old", "c"),
            ("new", "e"),
            ("new", "f"),
            ("none", "lone"),
        ];
        assert_eq!(
            pairs_by_key(old.into_iter(), new.into_iter()),
            BTreeMap::from([("iris".to_owned(), "flowers".to_owned())])
        );
        assert_eq!(
            pairs_by_key([].into_iter(), new.into_iter()),
            BTreeMap::new()
        );
    }

    fn fetched(name: &str, reference: &str, extract: bool) -> Locked {
        let manifest = Manifest::parse(&format!(
            "[datasets.input]\nref = {reference:?}\nextract = {extract}\n"
        ))
        .unwrap();
        Locked {
            name: name.to_owned(),
            reference: reference.to_owned(),
            spec: crate::project::lock::spec(&manifest.datasets["input"].source),
            at: None,
            extract,
            decompress: false,
            web_page: false,
            resolved: reference.to_owned(),
            title: Some("the original title".to_owned()),
            license: None,
            doi: None,
            access: crate::source::Access::Unknown,
            timestamp: "2026-11-12T00:00:00Z".to_owned(),
            files: vec![crate::project::lock::LockedFile {
                path: "original.csv".to_owned(),
                size: 3,
                sha256: crate::digest::sha256(b"old"),
                md5: None,
                url: String::new(),
                unpacks_to: None,
            }],
        }
    }

    /// an import that went is not carried to the new import, whose table could be any import's.
    #[test]
    fn fetched_renames_keep_bytes_and_report_new_name_order() {
        let manifest = Manifest::parse(
            "[datasets.alpha]\nref = \"local\"\n[datasets.stay]\nref = \"https://example.org/stay\"\n[datasets.zeta]\nref = \"https://example.org/source\"\n",
        )
        .unwrap();
        let before = fetched("before", "https://example.org/source", true);
        let imported = fetched("old_local", "local", true);
        let stay = fetched("stay", "https://example.org/stay", true);
        let mut kept = vec![stay.clone()];
        let mut others = vec![before.clone(), imported.clone()];
        let renamed = carry(
            &manifest,
            &Scope::All,
            &Upgrade::Nothing,
            &HashSet::new(),
            &mut kept,
            &mut others,
        );
        assert_eq!(
            renamed,
            [Rename {
                old: "before".into(),
                new: "zeta".into(),
                moved: true
            }]
        );
        let mut zeta = before;
        zeta.name = "zeta".into();
        assert_eq!(kept, [stay, zeta]);
        assert_eq!(others, [imported]);
    }

    #[test]
    fn named_rename_copies_the_pin_and_excludes_other_new_names() {
        let manifest = Manifest::parse(
            "[datasets.chosen]\nref = \"https://example.org/source\"\n[datasets.other]\nref = \"https://example.org/source\"\n",
        )
        .unwrap();
        let old = fetched("old", "https://example.org/source", true);
        let mut kept = vec![old.clone()];
        let mut others = Vec::new();
        let renamed = carry(
            &manifest,
            &Scope::Only(["chosen".to_owned()].into()),
            &Upgrade::Nothing,
            &HashSet::new(),
            &mut kept,
            &mut others,
        );
        assert_eq!(
            renamed,
            [Rename {
                old: "old".into(),
                new: "chosen".into(),
                moved: false
            }]
        );
        let mut chosen = old.clone();
        chosen.name = "chosen".into();
        assert_eq!(kept, [chosen, old]);
        assert_eq!(others, []);
    }

    #[test]
    fn fetched_matches_require_unique_specs_and_the_same_extraction() {
        for (new_names, old_names, extract) in [
            (&["new"][..], &["a", "b"][..], true),
            (&["first", "second"][..], &["old"][..], true),
            (&["new"][..], &["old"][..], false),
        ] {
            let mut text = String::new();
            for name in new_names {
                writeln!(
                    text,
                    "[datasets.{name}]\nref = \"https://example.org/source\""
                )
                .unwrap();
            }
            let manifest = Manifest::parse(&text).unwrap();
            let mut kept = Vec::new();
            let mut others: Vec<_> = old_names
                .iter()
                .map(|name| fetched(name, "https://example.org/source", extract))
                .collect();
            let before = others.clone();
            assert_eq!(
                carry(
                    &manifest,
                    &Scope::All,
                    &Upgrade::Nothing,
                    &HashSet::new(),
                    &mut kept,
                    &mut others
                ),
                []
            );
            assert_eq!(kept, []);
            assert_eq!(others, before);
        }
    }

    #[test]
    fn given_and_upgraded_names_are_not_rename_candidates() {
        let manifest =
            Manifest::parse("[datasets.new]\nref = \"https://example.org/source\"\n").unwrap();
        for (upgrade, given) in [
            (Upgrade::Nothing, HashSet::from(["new"])),
            (Upgrade::All, HashSet::new()),
            (Upgrade::Only(["new".to_owned()].into()), HashSet::new()),
        ] {
            let mut kept = Vec::new();
            let mut others = vec![fetched("old", "https://example.org/source", true)];
            let before = others.clone();
            assert_eq!(
                carry(
                    &manifest,
                    &Scope::All,
                    &upgrade,
                    &given,
                    &mut kept,
                    &mut others
                ),
                []
            );
            assert_eq!(kept, []);
            assert_eq!(others, before);
        }
    }
}
