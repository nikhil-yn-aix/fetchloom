//! datasets data.toml renamed: a name data.lock lacks whose table pins what an entry pins under a
//! name data.toml no longer has. relocking carries that entry over to the new name, so a rename
//! never asks the source again and never pins other bytes than the old name had.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashSet};

use super::{Scope, Upgrade};
use crate::project::lock::derived::Derived;
use crate::project::lock::{Lock, Locked};
use crate::project::manifest::{Manifest, Recipe};

/// an entry of data.lock carried over to the name data.toml gives it now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::cli) struct Rename {
    pub(in crate::cli) old: String,
    pub(in crate::cli) new: String,
    /// whether the old entry went: a run of named datasets that leaves the old name out keeps
    /// its entry until a run of every dataset.
    pub(super) moved: bool,
}

/// `lock` as a relock of every dataset would carry its entries over to the names data.toml gives
/// them now, with those renames, so a forecast that writes nothing, as status, agrees with what
/// a sync keeps. results and entries of names data.toml does not have stay as they are.
pub(in crate::cli) fn forecast(manifest: &Manifest, lock: Lock) -> (Lock, Vec<Rename>) {
    let (mut derived, dropped, mut renamed) = results(manifest, lock.derived);
    derived.extend(dropped);
    derived.sort_by(|a, b| a.name.cmp(&b.name));
    let mut datasets = Vec::new();
    let mut others = lock.datasets;
    renamed.extend(carry(
        manifest,
        &Scope::All,
        &Upgrade::Nothing,
        &HashSet::new(),
        &mut datasets,
        &mut others,
    ));
    datasets.extend(others);
    datasets.sort_by(|a, b| a.name.cmp(&b.name));
    (Lock { datasets, derived }, renamed)
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

/// carries over the entries of fetched datasets data.toml renamed, from `others`, the entries
/// relocking replaces or drops, or from `kept`, which holds those a run of named datasets leaves
/// alone, into `kept` under their new names. a new name is one in `scope` that data.lock lacks,
/// that no command `given` already and that no upgrade asks to resolve again; an old entry
/// answers it when its spec and `extract` are the ones its table asks for now. an old entry in
/// `others` moves, and one in `kept` is copied, since a run of named datasets changes no other
/// entry. both vectors keep data.lock's name order.
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
        .filter(|locked| !manifest.datasets.contains_key(&locked.name))
        .map(|locked| {
            (
                (Cow::Borrowed(locked.spec.as_str()), locked.extract),
                locked.name.as_str(),
            )
        });
    let new = manifest.datasets.iter().filter_map(|(name, entry)| {
        let source = entry.source()?;
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

#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct RecipeShape<'a> {
    from: Vec<&'a str>,
    run: Vec<&'a str>,
    code: Vec<&'a str>,
    env: &'a BTreeMap<String, String>,
}

impl<'a> RecipeShape<'a> {
    fn pinned(result: &'a Derived) -> Self {
        Self {
            from: result
                .from
                .iter()
                .map(|input| input.name.as_str())
                .collect(),
            run: result.run.iter().map(String::as_str).collect(),
            code: result.code.iter().map(|code| code.path.as_str()).collect(),
            env: &result.env,
        }
    }

    fn asked(recipe: &'a Recipe) -> Self {
        let mut code: Vec<&str> = recipe.code.iter().map(String::as_str).collect();
        code.sort_unstable();
        Self {
            from: recipe.from.iter().map(String::as_str).collect(),
            run: recipe
                .run
                .iter()
                .map(crate::recipe::Template::text)
                .collect(),
            code,
            env: &recipe.env,
        }
    }
}

/// data.lock's recipe results `derived` as relocking sorts them: those it keeps, those it drops,
/// and the renames it made. only `fetchloom build` pins a recipe's result, so one stays while
/// data.toml has the recipe, moves with a recipe data.toml renamed, and otherwise goes as a
/// dropped dataset does.
pub(super) fn results(
    manifest: &Manifest,
    derived: Vec<Derived>,
) -> (Vec<Derived>, Vec<Derived>, Vec<Rename>) {
    let (mut kept, mut dropped): (Vec<Derived>, Vec<Derived>) =
        derived.into_iter().partition(|result| {
            manifest
                .datasets
                .get(&result.name)
                .is_some_and(|entry| entry.recipe().is_some())
        });
    if dropped.is_empty() {
        return (kept, dropped, Vec::new());
    }
    let old = dropped
        .iter()
        .filter(|result| !manifest.datasets.contains_key(&result.name))
        .map(|result| (RecipeShape::pinned(result), result.name.as_str()));
    let new = manifest.datasets.iter().filter_map(|(name, entry)| {
        let recipe = entry.recipe()?;
        kept.binary_search_by(|result| result.name.cmp(name))
            .is_err()
            .then(|| (RecipeShape::asked(recipe), name.as_str()))
    });
    let matches = pairs_by_key(old, new);
    let mut renamed = Vec::with_capacity(matches.len());
    for mut result in dropped.extract_if(.., |result| matches.contains_key(&result.name)) {
        let new = &matches[&result.name];
        renamed.push(rename_to(&mut result.name, new, true));
        kept.push(result);
    }
    kept.sort_by(|a, b| a.name.cmp(&b.name));
    renamed.sort_by(|a, b| a.new.cmp(&b.new));
    (kept, dropped, renamed)
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
            spec: crate::project::lock::spec(manifest.datasets["input"].source().unwrap()),
            extract,
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
            [
                Rename {
                    old: "old_local".into(),
                    new: "alpha".into(),
                    moved: true
                },
                Rename {
                    old: "before".into(),
                    new: "zeta".into(),
                    moved: true
                },
            ]
        );
        let mut alpha = imported;
        alpha.name = "alpha".into();
        let mut zeta = before;
        zeta.name = "zeta".into();
        assert_eq!(kept, [alpha, stay, zeta]);
        assert_eq!(others, []);
    }

    #[test]
    fn named_rename_copies_the_pin_and_excludes_other_new_names() {
        let manifest = Manifest::parse(
            "[datasets.chosen]\nref = \"local\"\n[datasets.other]\nref = \"local\"\n",
        )
        .unwrap();
        let old = fetched("old", "local", true);
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
                writeln!(text, "[datasets.{name}]\nref = \"local\"").unwrap();
            }
            let manifest = Manifest::parse(&text).unwrap();
            let mut kept = Vec::new();
            let mut others: Vec<_> = old_names
                .iter()
                .map(|name| fetched(name, "local", extract))
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
        let manifest = Manifest::parse("[datasets.new]\nref = \"local\"\n").unwrap();
        for (upgrade, given) in [
            (Upgrade::Nothing, HashSet::from(["new"])),
            (Upgrade::All, HashSet::new()),
            (Upgrade::Only(["new".to_owned()].into()), HashSet::new()),
        ] {
            let mut kept = Vec::new();
            let mut others = vec![fetched("old", "local", true)];
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

    fn recipe_manifest(name: &str) -> String {
        format!(
            "[datasets.raw]\nref = \"local\"\n[datasets.labels]\nref = \"https://example.org/labels\"\n[datasets.{name}]\nfrom = [\"raw\", \"labels\"]\nrun = [\"run\", \"{{raw}}\", \"{{labels}}\", \"{{out}}\"]\ncode = [\"b.py\", \"a.py\"]\nenv = {{ MODE = \"one\" }}\n"
        )
    }

    fn result(name: &str) -> Derived {
        let hash = crate::digest::sha256(b"old");
        Derived {
            name: name.to_owned(),
            from: ["raw", "labels"]
                .into_iter()
                .map(|name| crate::project::lock::derived::Input {
                    name: name.to_owned(),
                    content: hash,
                })
                .collect(),
            run: ["run", "{raw}", "{labels}", "{out}"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            code: ["a.py", "b.py"]
                .into_iter()
                .map(|path| crate::project::lock::derived::Code {
                    path: path.to_owned(),
                    sha256: hash,
                    folder: false,
                })
                .collect(),
            env: [("MODE".to_owned(), "one".to_owned())].into(),
            key: hash,
            timestamp: "2026-11-12T00:00:00Z".to_owned(),
            tree: hash,
            files: vec![crate::cache::Member {
                path: "result.csv".into(),
                size: 3,
                sha256: hash,
            }],
        }
    }

    #[test]
    fn recipe_renames_match_the_ordered_recipe_not_its_old_hashes() {
        let text = recipe_manifest("next");
        let manifest = Manifest::parse(&text).unwrap();
        let original = result("old");
        let (kept, dropped, renamed) = results(&manifest, vec![original.clone()]);
        let mut expected = original.clone();
        expected.name = "next".into();
        assert_eq!(kept, [expected]);
        assert_eq!(dropped, []);
        assert_eq!(
            renamed,
            [Rename {
                old: "old".into(),
                new: "next".into(),
                moved: true
            }]
        );
        for changed in [
            text.replace("[\"raw\", \"labels\"]", "[\"labels\", \"raw\"]"),
            text.replace("[\"run\",", "[\"other\","),
            text.replace("\"b.py\"", "\"c.py\""),
            text.replace("MODE = \"one\"", "MODE = \"two\""),
        ] {
            let manifest = Manifest::parse(&changed).unwrap();
            let (kept, dropped, renamed) = results(&manifest, vec![original.clone()]);
            assert_eq!(kept, []);
            assert_eq!(dropped.as_slice(), std::slice::from_ref(&original));
            assert_eq!(renamed, []);
        }
    }

    #[test]
    fn recipe_renames_refuse_ambiguous_old_or_new_shapes() {
        let original = result("old");
        let one = Manifest::parse(&recipe_manifest("next")).unwrap();
        let (kept, dropped, renamed) = results(&one, vec![original.clone(), result("other_old")]);
        assert_eq!(kept, []);
        assert_eq!(dropped.len(), 2);
        assert_eq!(renamed, []);
        let mut two = one.clone();
        two.datasets
            .insert("other_new".into(), one.datasets["next"].clone());
        let (kept, dropped, renamed) = results(&two, vec![original.clone()]);
        assert_eq!(kept, []);
        assert_eq!(dropped, [original]);
        assert_eq!(renamed, []);
    }
}
