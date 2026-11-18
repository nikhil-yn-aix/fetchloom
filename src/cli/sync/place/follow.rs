//! the first follow of a data folder link: what the folder it leads to holds that a sync, or the
//! `sync --force` its help offers, could harm.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;

use super::{Action, Held, Reason, edited, held, local, mark, passed_over, reads_as_target};
use crate::cli::sync::disk::{Blocker, Entry, Survey, is_inside};
use crate::error::{Error, Kind};
use crate::place;
use crate::plan::Target;
use crate::project::manifest::Manifest;
use crate::project::state::{Look, Once};
use crate::project::{Followed, Project, native};

/// refuses the first follow of `link` before anything is written through it when the folder it
/// leads to holds what a run could harm: a .gitignore in the form of fetchloom's markers that the
/// state does not record, a dataset folder of another fetchloom project; a file at a path
/// `targets` place holding other bytes, a record of the path there or not, as one could be of the
/// folder the link led to before, or a file where they place a folder, which `sync --force`
/// replaces; or a file in an output's folder that no pin places and the state does not record,
/// which `sync --force` deletes. `actions` are those decided for `targets`. a file of the pinned
/// bytes is taken as placed, an edit the state told of, by its path, size and time, is the
/// person's own moved with their folder, what the folder an output's capture pins holds is the
/// person's claim, and everything else there is theirs, as anywhere in a data folder.
pub(super) fn refuse_harm(
    project: &Project,
    manifest: &Manifest,
    (targets, actions): (&[Target], &[Action]),
    survey: &Survey,
    link: &Followed,
) -> Result<(), Error> {
    let inside = |path: &str| {
        is_inside(path, &link.path)
            && !project
                .captured
                .as_deref()
                .is_some_and(|at| is_inside(path, at))
    };
    let named = |paths: &BTreeSet<&str>| {
        let below: Vec<&str> = paths
            .iter()
            .map(|path| path.get(link.path.len() + 1..).unwrap_or(path))
            .collect();
        crate::units::some_of(&below, 3)
    };
    let marked = marked(project, survey, &inside);
    if !marked.is_empty() {
        let (why, those) = if marked.len() == 1 {
            (
                "marks a dataset folder of another fetchloom project",
                "that .gitignore",
            )
        } else {
            (
                "mark dataset folders of other fetchloom projects",
                "those .gitignore files",
            )
        };
        return Err(refused(
            link,
            &format!("{} {why}", named(&marked)),
            &format!(
                "point the link at a folder of this project's own, or delete {those} if no other project places files there now"
            ),
        ));
    }
    let (replaced, blockers) = replaced(project, (targets, actions), survey, &inside)?;
    let in_the_way: BTreeSet<&str> = blockers.iter().map(String::as_str).collect();
    let move_away = |files: &BTreeSet<&str>| {
        if files.len() == 1 {
            "move that file away if it is yours"
        } else {
            "move those files away if they are yours"
        }
    };
    if !replaced.is_empty() {
        let holds = if replaced.len() == 1 { "holds" } else { "hold" };
        let help = match outputs_holding(manifest, &replaced) {
            Some(outputs) => keeping(project, &outputs, replaced.len()),
            None => format!(
                "point the link at a folder of this project's own, or {}: a file holding the pinned bytes is taken as placed",
                move_away(&replaced)
            ),
        };
        return Err(refused(
            link,
            &format!(
                "{} {holds} other bytes than data.lock pins, which `sync --force` would replace",
                named(&replaced)
            ),
            &help,
        ));
    }
    if !in_the_way.is_empty() {
        let stands = if in_the_way.len() == 1 {
            "stands"
        } else {
            "stand"
        };
        return Err(refused(
            link,
            &format!(
                "{} {stands} where data.lock pins a folder, which `sync --force` would remove",
                named(&in_the_way)
            ),
            &format!(
                "point the link at a folder of this project's own, or {}",
                move_away(&in_the_way)
            ),
        ));
    }
    let deleted = deleted(manifest, targets, survey, &inside);
    if deleted.is_empty() {
        return Ok(());
    }
    let files = named(&deleted.keys().copied().collect());
    Err(unpinned(project, link, &deleted, &files))
}

/// the refusal of the first follow of `link` for `deleted`, files in an output's folder that no
/// pin places, each with its output's name and folder, `files` as the refusal lists them, with the
/// add that pins the folder again.
fn unpinned(
    project: &Project,
    link: &Followed,
    deleted: &BTreeMap<&str, (&str, &str)>,
    files: &str,
) -> Error {
    let (is, it) = if deleted.len() == 1 {
        ("is", "it")
    } else {
        ("are", "them")
    };
    let outputs: BTreeSet<(&str, &str)> = deleted.values().copied().collect();
    refused(
        link,
        &format!(
            "{files} {is} in an output's folder though no pin places {it}, which `sync --force` would delete"
        ),
        &keeping(project, &outputs, deleted.len()),
    )
}

/// the name and folder of each output of `manifest` holding one of `paths`, or none when one is in
/// no output's folder.
fn outputs_holding<'m>(
    manifest: &'m Manifest,
    paths: &BTreeSet<&str>,
) -> Option<BTreeSet<(&'m str, &'m str)>> {
    paths
        .iter()
        .map(|path| {
            manifest.datasets.iter().find_map(|(name, entry)| {
                let at = entry.at.as_deref()?;
                is_inside(path, at).then_some((name.as_str(), at))
            })
        })
        .collect()
}

/// the help of a refusal of `count` files in the folders of `outputs`, each an output's name and
/// folder: keep them by moving them out, or by pinning the folder again, which claims them.
fn keeping(project: &Project, outputs: &BTreeSet<(&str, &str)>, count: usize) -> String {
    let (it, those) = if count == 1 {
        ("it", "that file")
    } else {
        ("them", "those files")
    };
    let again = match outputs.iter().next() {
        Some((name, at)) if outputs.len() == 1 => {
            crate::suggested::command(&crate::source::local::output_pin(
                name,
                &crate::cli::folder_from_here(&project.root, at),
            ))
        }
        _ => crate::suggested::command("add <folder> --as <name>"),
    };
    format!(
        "point the link at a folder of this project's own, or keep {those} by moving {it} out of the output's folder or by pinning the folder again with `{again}`"
    )
}

/// the .gitignore files `inside` takes that are in the form of fetchloom's markers, which the
/// survey lists as the state records none of them.
fn marked<'s>(
    project: &Project,
    survey: &'s Survey,
    inside: &impl Fn(&str) -> bool,
) -> BTreeSet<&'s str> {
    survey
        .disk
        .keys()
        .filter(|path| inside(path) && path.ends_with("/.gitignore"))
        .filter(|path| mark::is_marker_form(&native(&project.root, path)))
        .map(String::as_str)
        .collect()
}

/// the files `inside` takes at paths `targets` place that hold other bytes than pinned, as
/// `actions` decided them or, for a dataset the run leaves alone, which a later run places
/// trusting the folder, as a placing would, but for an edit the state told of while its file
/// looked as it does now; and the files where they place a folder, which `sync --force` always
/// removes.
fn replaced<'s>(
    project: &Project,
    (targets, actions): (&'s [Target], &'s [Action]),
    survey: &Survey,
    inside: &impl Fn(&str) -> bool,
) -> Result<(BTreeSet<&'s str>, BTreeSet<String>), Error> {
    let mut replaced = BTreeSet::new();
    let mut in_the_way = BTreeSet::new();
    let unrecorded_file = |blocker: &Blocker| {
        blocker.found == Entry::File && !survey.state.files.contains_key(&blocker.path)
    };
    for (target, action) in targets.iter().zip(actions) {
        if !inside(&target.path) {
            continue;
        }
        match action {
            Action::Overwrite(Reason::Foreign | Reason::Changed | Reason::Shared)
            | Action::Leave(Reason::Foreign | Reason::Changed | Reason::Shared)
                if !told(survey, &target.path) =>
            {
                replaced.insert(target.path.as_str());
            }
            Action::Unblock(blocker) | Action::Blocked(blocker) if unrecorded_file(blocker) => {
                in_the_way.insert(blocker.path.clone());
            }
            Action::Unselected => match survey.disk.get(&target.path) {
                Some(look) => {
                    let path = native(&project.root, &target.path);
                    if !told(survey, &target.path) && other_bytes(target, look, survey, &path)? {
                        replaced.insert(target.path.as_str());
                    }
                }
                None => {
                    if let Some(blocker) = survey.blocker(&target.path).filter(unrecorded_file) {
                        in_the_way.insert(blocker.path);
                    }
                }
            },
            _ => {}
        }
    }
    Ok((replaced, in_the_way))
}

/// whether the state told of the file at `path` as an edit while it looked as it does now, which
/// makes it the person's own, moved with their folder.
fn told(survey: &Survey, path: &str) -> bool {
    survey
        .disk
        .get(path)
        .is_some_and(|look| survey.state.told.has(Once::Edit, &edited(path, look)))
}

/// whether the file at `path`, seen as `look`, holds other bytes than `target` pins, and than those
/// the state records placing there when it looks as recorded no more, as a placing decides it.
fn other_bytes(target: &Target, look: &Look, survey: &Survey, path: &Path) -> Result<bool, Error> {
    match survey.state.files.get(&target.path) {
        Some(placed) if placed.look == *look && !survey.edited.contains(&target.path) => Ok(false),
        Some(placed) => Ok(held(target, placed, look, path) == Held::Other),
        None => reads_as_target(target, look, path)
            .map(|same| !same)
            .map_err(|err| local(path, &err)),
    }
}

/// the files `inside` takes in the folder of an output of `manifest` that no pin of `targets`
/// places, the state does not record and no run told of as an edit while it looked as it does now,
/// which `sync --force` deletes as the output's snapshot lacks them, each with its output's name and
/// folder. what a system, git or a tool puts there, and what the output's patterns leave out, is
/// no file of the output.
fn deleted<'m, 's>(
    manifest: &'m Manifest,
    targets: &[Target],
    survey: &'s Survey,
    inside: &impl Fn(&str) -> bool,
) -> BTreeMap<&'s str, (&'m str, &'m str)> {
    let wanted: HashSet<&str> = targets.iter().map(|target| target.path.as_str()).collect();
    let outputs: Vec<(&str, &str)> = manifest
        .datasets
        .iter()
        .filter_map(|(name, entry)| Some((name.as_str(), entry.at.as_deref()?)))
        .collect();
    survey
        .disk
        .keys()
        .filter(|path| {
            inside(path)
                && !wanted.contains(path.as_str())
                && !survey.state.files.contains_key(*path)
                && !survey.state.aside.contains_key(*path)
                && !told(survey, path)
                && !survey.made_by_system(path)
                && !place::is_copy_temporary(path.rsplit('/').next().unwrap_or(path))
                && !passed_over(manifest, survey, path)
        })
        .filter_map(|path| {
            let output = outputs.iter().find(|(_, at)| is_inside(path, at))?;
            Some((path.as_str(), *output))
        })
        .collect()
}

/// the refusal of the first follow of `link`, where `why` says what a run could harm, with `help`.
fn refused(link: &Followed, why: &str, help: &str) -> Error {
    Error::new(
        Kind::ProjectInvalid,
        format!(
            "{} is a link to {}, where {why}, so fetchloom places nothing through it",
            link.at.display(),
            crate::machine::plain(&link.found).display()
        ),
    )
    .help(help)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::outputs_holding;
    use crate::project::manifest::Manifest;

    /// a refusal names the pin of an output's folder again only when every file it names lies in
    /// an output's folder: a fetched dataset's file among them keeps the help that fits it.
    #[test]
    fn only_files_all_in_outputs_take_the_pin_again() {
        let manifest = Manifest::parse(
            "[datasets.processed]\nref = \"local\"\nat = \"data/processed\"\n\n[datasets.iris]\nref = \"https://example.org/iris.csv\"\n",
        )
        .unwrap();
        let rerun = BTreeSet::from(["data/processed/clean.csv"]);
        assert_eq!(
            outputs_holding(&manifest, &rerun),
            Some(BTreeSet::from([("processed", "data/processed")]))
        );
        let mixed = BTreeSet::from(["data/processed/clean.csv", "data/iris/iris.csv"]);
        assert_eq!(outputs_holding(&manifest, &mixed), None);
    }
}
