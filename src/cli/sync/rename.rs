//! datasets data.toml renamed: a name data.lock lacks whose table pins what an entry pins under a
//! name data.toml no longer has. relocking carries that entry over to the new name, so a rename
//! never asks the source again and never pins other bytes than the old name had.

use super::{Scope, Upgrade};
use crate::project::lock::Locked;
use crate::project::lock::derived::Derived;
use crate::project::manifest::{Manifest, Recipe};

/// an entry of data.lock carried over to the name data.toml gives it now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Rename {
    pub(super) old: String,
    pub(super) new: String,
    /// whether the old entry went: a run of named datasets that leaves the old name out keeps
    /// its entry until a run of every dataset.
    pub(super) moved: bool,
}

/// pairs each name of `new` with the one name of `old` it renames, as `(old, new)`: `answers`
/// says whether the old entry pins what a new name's table asks. a pair needs each to answer only
/// the other, so several equal candidates rename nothing.
fn pairs<'a>(
    new: &[&'a str],
    old: &[&'a str],
    answers: impl Fn(&str, &str) -> bool,
) -> Vec<(&'a str, &'a str)> {
    let mut found = Vec::new();
    for &name in new {
        let mut matching = old.iter().filter(|was| answers(was, name));
        let (Some(&was), None) = (matching.next(), matching.next()) else {
            continue;
        };
        if new.iter().filter(|other| answers(was, other)).count() == 1 {
            found.push((was, name));
        }
    }
    found
}

/// carries over the entries of fetched datasets data.toml renamed, from `others`, the entries
/// relocking replaces or drops, or from `kept`, which holds those a run of named datasets leaves
/// alone, into `kept` under their new names. a new name is one in `scope` that data.lock lacks,
/// that no command `given` already and that no upgrade asks to resolve again; an old entry
/// answers it when its spec and `extract` are the ones its table asks for now. an old entry in
/// `others` moves, and one in `kept` is copied, since a run of named datasets changes no other
/// entry.
pub(super) fn carry(
    manifest: &Manifest,
    scope: &Scope,
    upgrade: &Upgrade,
    given: &[&str],
    kept: &mut Vec<Locked>,
    others: &mut Vec<Locked>,
) -> Vec<Rename> {
    let pinned = |name: &str| {
        kept.iter()
            .chain(others.iter())
            .any(|locked| locked.name == name)
    };
    let new: Vec<&str> = manifest
        .datasets
        .iter()
        .filter(|(name, entry)| {
            entry.source().is_some()
                && scope.includes(name)
                && !upgrade.includes(name)
                && !given.contains(&name.as_str())
                && !pinned(name)
        })
        .map(|(name, _)| name.as_str())
        .collect();
    if new.is_empty() {
        return Vec::new();
    }
    let gone: Vec<&Locked> = kept
        .iter()
        .chain(others.iter())
        .filter(|locked| !manifest.datasets.contains_key(&locked.name))
        .collect();
    let old: Vec<&str> = gone.iter().map(|locked| locked.name.as_str()).collect();
    let answers = |was: &str, name: &str| {
        gone.iter()
            .find(|locked| locked.name == was)
            .is_some_and(|locked| locked.drift(&manifest.datasets[name]).is_none())
    };
    let found: Vec<(String, String)> = pairs(&new, &old, answers)
        .into_iter()
        .map(|(was, name)| (was.to_owned(), name.to_owned()))
        .collect();
    let mut renames = Vec::with_capacity(found.len());
    for (was, name) in found {
        let moved = others
            .iter()
            .position(|locked| locked.name == was)
            .map(|at| others.remove(at));
        let entry = match &moved {
            Some(locked) => Some(locked.clone()),
            None => kept.iter().find(|locked| locked.name == was).cloned(),
        };
        let Some(mut entry) = entry else {
            continue;
        };
        entry.name.clone_from(&name);
        kept.push(entry);
        renames.push(Rename {
            old: was,
            new: name,
            moved: moved.is_some(),
        });
    }
    kept.sort_by(|a, b| a.name.cmp(&b.name));
    renames
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
    let renamed = carry_results(manifest, &mut kept, &mut dropped);
    (kept, dropped, renamed)
}

/// carries over the pinned results of recipes data.toml renamed, from `dropped`, the results of
/// recipes data.toml no longer has, into `results` under their new names. a result answers a
/// recipe whose table is the one it was built from: the same `from`, `run`, `code` and `env`. a
/// key never held a recipe's own name, so the result still answers the recipe as it stands.
fn carry_results(
    manifest: &Manifest,
    results: &mut Vec<Derived>,
    dropped: &mut Vec<Derived>,
) -> Vec<Rename> {
    let new: Vec<&str> = manifest
        .datasets
        .iter()
        .filter(|(name, entry)| {
            entry.recipe().is_some() && !results.iter().any(|result| result.name == **name)
        })
        .map(|(name, _)| name.as_str())
        .collect();
    let old: Vec<&str> = dropped
        .iter()
        .filter(|result| !manifest.datasets.contains_key(&result.name))
        .map(|result| result.name.as_str())
        .collect();
    let answers = |was: &str, name: &str| {
        let recipe = manifest.datasets[name].recipe();
        dropped
            .iter()
            .find(|result| result.name == was)
            .zip(recipe)
            .is_some_and(|(result, recipe)| built_from(result, recipe))
    };
    let found: Vec<(String, String)> = pairs(&new, &old, answers)
        .into_iter()
        .map(|(was, name)| (was.to_owned(), name.to_owned()))
        .collect();
    let mut renames = Vec::with_capacity(found.len());
    for (was, name) in found {
        let Some(at) = dropped.iter().position(|result| result.name == was) else {
            continue;
        };
        let mut result = dropped.remove(at);
        result.name.clone_from(&name);
        results.push(result);
        renames.push(Rename {
            old: was,
            new: name,
            moved: true,
        });
    }
    results.sort_by(|a, b| a.name.cmp(&b.name));
    renames
}

/// whether `result` was built from the table `recipe` has: its inputs by name and in order, its
/// command as written, its code by path and its variables.
fn built_from(result: &Derived, recipe: &Recipe) -> bool {
    let mut code: Vec<&str> = recipe.code.iter().map(String::as_str).collect();
    code.sort_unstable();
    result
        .from
        .iter()
        .map(|input| &input.name)
        .eq(recipe.from.iter())
        && result
            .run
            .iter()
            .map(String::as_str)
            .eq(recipe.run.iter().map(crate::recipe::Template::text))
        && result.code.iter().map(|code| code.path.as_str()).eq(code)
        && result.env == recipe.env
}

#[cfg(test)]
mod tests {
    use super::*;

    /// a name pairs with the one old name that answers it, and with none when either side has
    /// several that answer.
    #[test]
    fn only_one_to_one_answers_pair() {
        let answers = |was: &str, name: &str| {
            matches!(
                (was, name),
                ("iris", "flowers") | ("a" | "b", "c") | ("d", "e" | "f")
            )
        };
        assert_eq!(
            pairs(
                &["flowers", "c", "e", "f", "lone"],
                &["iris", "a", "b", "d"],
                answers
            ),
            [("iris", "flowers")]
        );
        assert_eq!(pairs(&["flowers"], &[], answers), []);
    }
}
