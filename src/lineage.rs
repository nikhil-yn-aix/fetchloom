//! lineage: what a dataset was made from. data.toml's `from` names the datasets, and data.lock
//! keeps, with the pin of the dataset made from them, each one's tree identity ([`crate::tree`])
//! as it was when an add pinned it. it is a record a person keeps: nothing here runs a pipeline
//! or checks that the files came from those inputs.
//!
//! only an add records, for the datasets it names: `--from` replaces what data.toml says, and an
//! add without it keeps that and records the inputs anew, even when the dataset's own files are
//! the same. every other change of a pin keeps the record as it was.

use crate::cache::Cache;
use crate::project::lock::{Lock, Recorded};
use crate::project::manifest::Manifest;
use crate::tree::{self, Identity};
use crate::units;

/// the identity of `name`'s files as data.toml and `lock` have them now, or none when this machine
/// cannot tell: data.toml has no such dataset, `lock` does not pin it as data.toml asks, or one of
/// its archives was never unpacked here.
pub(crate) fn current(
    manifest: &Manifest,
    lock: &Lock,
    name: &str,
    cache: &Cache,
) -> Option<Identity> {
    let entry = manifest.datasets.get(name)?;
    let locked = lock
        .get(name)
        .filter(|locked| locked.drift(entry).is_none())?;
    tree::effective(entry, locked, cache)
}

/// records, in `lock`, each of `names` as made from the datasets data.toml's `from` gives it, with
/// their files as `lock` pins them now; the names whose record changed, with it.
pub(crate) fn record(
    lock: &mut Lock,
    manifest: &Manifest,
    names: &[String],
    cache: &Cache,
) -> Vec<(String, Vec<Recorded>)> {
    let mut changed = Vec::new();
    for name in names {
        let Some(entry) = manifest.datasets.get(name) else {
            continue;
        };
        let mut inputs: Vec<Recorded> = entry
            .from
            .iter()
            .map(|input| Recorded {
                name: input.clone(),
                tree: current(manifest, lock, input, cache),
            })
            .collect();
        inputs.sort_by(|a, b| a.name.cmp(&b.name));
        let Some(locked) = lock.datasets.iter_mut().find(|locked| locked.name == *name) else {
            continue;
        };
        if locked.from != inputs {
            locked.from.clone_from(&inputs);
            changed.push((name.clone(), inputs));
        }
    }
    changed
}

/// what an add says of `name` once it recorded `inputs`: what it was made from, and whether status
/// can tell when their files change.
pub(crate) fn told_record(name: &str, inputs: &[Recorded]) -> String {
    let names: Vec<&str> = inputs.iter().map(|input| input.name.as_str()).collect();
    let unknown: Vec<&str> = inputs
        .iter()
        .filter(|input| input.tree.is_none())
        .map(|input| input.name.as_str())
        .collect();
    let made = format!("{name} as made from {}", units::listed(&names, "and"));
    match unknown.as_slice() {
        [] if names.len() == 1 => format!("{made}; status says when {}'s files change", names[0]),
        [] => format!("{made}; status says when their files change"),
        [one] => format!(
            "{made}; this machine does not know {one}'s files, so status cannot tell when they change"
        ),
        some => format!(
            "{made}; this machine does not know the files of {}, so status cannot tell when they change",
            units::listed(some, "and")
        ),
    }
}
