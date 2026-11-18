//! the first follow of a data folder link: what the folder it leads to holds that a sync, or the
//! `sync --force` its help offers, could harm.

use std::collections::{BTreeMap, BTreeSet, HashSet};
#[cfg(unix)]
use std::path::Path;

use super::mark::{self, Whose};
use super::{Action, Reason, decide, passed_over};
use crate::cli::sync::disk::{Blocker, Entry, Mark, Survey, is_inside};
use crate::error::Error;
use crate::place;
use crate::plan::Target;
use crate::project::manifest::Manifest;
use crate::project::{Followed, Project, native};

/// refuses the first follow of `link` before anything is written through it when the folder it
/// leads to holds what a run could harm: a .gitignore in the form of fetchloom's markers that names
/// another project than this one, or none, whatever the state records of its folder, as it marks
/// a dataset folder of another project or another copy, or of the project this one was copied or
/// moved from; a file at a path `targets` place holding other bytes, a record of the path there or
/// not, as one could be of the folder the link led to before, or a file where they place a folder,
/// which `sync --force` replaces; or a file in an output's folder that no pin places and the state
/// does not record, which `sync --force` deletes. `actions` are those decided for `targets`. a file of the pinned
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
    #[cfg(unix)]
    if let Some((entry, owner)) = not_owned(
        &project.root,
        survey,
        link,
        crate::cache::run_as(),
        &crate::cache::entry_owner,
    ) {
        return Err(theirs(project, link, entry, owner));
    }
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
    refuse_marked(project, survey, link, (&inside, &named))?;
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
            project,
            link,
            &format!(
                "where {} {holds} other bytes than data.lock pins, which `sync --force` would replace",
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
            project,
            link,
            &format!(
                "where {} {stands} where data.lock pins a folder, which `sync --force` would remove",
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
        project,
        link,
        &format!(
            "where {files} {is} in an output's folder though no pin places {it}, which `sync --force` would delete"
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

/// refuses the first follow of `link` when the folder it leads to holds a marker of fetchloom's that
/// names another project than this one, or none, as [`marked`] finds them, `named` listing them:
/// one of the project this one was copied or moved from, as the copied state's id says, with the
/// way out of each reading, or of another project, another copy or this project before its
/// .fetchloom folder was lost.
fn refuse_marked(
    project: &Project,
    survey: &Survey,
    link: &Followed,
    (inside, named): (&impl Fn(&str) -> bool, &impl Fn(&BTreeSet<&str>) -> String),
) -> Result<(), Error> {
    let marked = marked(project, survey, inside);
    if marked.is_empty() {
        return Ok(());
    }
    let paths: BTreeSet<&str> = marked.keys().map(String::as_str).collect();
    let copied = survey.state.copied.id.as_deref();
    let one = marked.len() == 1;
    if marked
        .values()
        .all(|whose| matches!(whose, Whose::Project(id) if Some(id.as_str()) == copied))
    {
        let why = if one {
            "marks a dataset folder of the project this one was copied or moved from"
        } else {
            "mark dataset folders of the project this one was copied or moved from"
        };
        return Err(refused(
            project,
            link,
            &format!("where {} {why}", named(&paths)),
            "if this is a copy, give it a data folder of its own: the cache keeps each file's bytes once. if you moved the project here and no other copy uses that folder, delete those .gitignore files, and sync makes them again",
        ));
    }
    // a project that lost its .fetchloom folder lost its id too, so its own markers read alike
    let (why, those, again) = if one {
        (
            "marks another project's or another copy's dataset folder",
            "that .gitignore",
            "if it was this project's own, sync makes it again",
        )
    } else {
        (
            "mark other projects' or other copies' dataset folders",
            "those .gitignore files",
            "if they were this project's own, sync makes them again",
        )
    };
    Err(refused(
        project,
        link,
        &format!(
            "where {} {why}, or this project's own, from before its .fetchloom folder was lost",
            named(&paths)
        ),
        &format!(
            "point the link at a folder of this project's own, or delete {those} if no other project or copy places files there now: {again}"
        ),
    ))
}

/// the .gitignore files `inside` takes, those the state records as markers and those it does not,
/// that are in the form of fetchloom's markers and name another project than this one, or none,
/// each with the id it names: the state records markers by their folder in the project, which a
/// copy's state or a link pointed elsewhere shares with another project's.
fn marked(
    project: &Project,
    survey: &Survey,
    inside: &impl Fn(&str) -> bool,
) -> BTreeMap<String, Whose> {
    let id = survey.state.id.as_deref();
    let recorded = survey
        .marks
        .iter()
        .filter(|(_, found)| matches!(found, Mark::Made(_)))
        .map(|(folder, _)| crate::cli::sync::disk::marker(folder));
    let unrecorded = survey
        .disk
        .keys()
        .filter(|path| path.ends_with("/.gitignore"))
        .cloned();
    recorded
        .chain(unrecorded)
        .filter(|path| inside(path))
        .filter_map(|path| {
            let whose = mark::marker_of(&native(&project.root, &path))?;
            let ours = matches!(&whose, Whose::Project(named) if Some(named.as_str()) == id);
            (!ours).then_some((path, whose))
        })
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
        let decided;
        let action = if let Action::Unselected = action {
            let path = native(&project.root, &target.path);
            decided = decide(target, survey, false, &path)?.readable(&path)?;
            &decided
        } else {
            action
        };
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
            _ => {}
        }
    }
    Ok((replaced, in_the_way))
}

/// whether the state told of the file at `path` as an edit while it looked as it does now, which
/// makes it the person's own, moved with their folder.
fn told(survey: &Survey, path: &str) -> bool {
    survey.told_edit(path, &survey.state.told)
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

/// the folder `link` leads to when another user than `me` owns it, else the first entry `survey`
/// lists there, by its path below that folder, that another user owns, with that user, as
/// `owner` tells who owns an entry: a first follow takes only a folder this user owns, whole, as
/// others share bytes through a cache rather than a data folder. windows keeps no user id, so
/// there a folder anyone owns is followed.
#[cfg(unix)]
fn not_owned<'s>(
    root: &Path,
    survey: &'s Survey,
    link: &Followed,
    me: u32,
    owner: &dyn Fn(&Path) -> Option<u32>,
) -> Option<(Option<&'s str>, u32)> {
    let theirs = |path: &Path| owner(path).filter(|owner| *owner != me);
    if let Some(owner) = theirs(&link.found) {
        return Some((None, owner));
    }
    let entries: BTreeSet<&str> = survey
        .disk
        .keys()
        .chain(survey.entries.keys())
        .map(String::as_str)
        .filter(|path| *path != link.path && is_inside(path, &link.path))
        .collect();
    entries.into_iter().find_map(|path| {
        let owner = theirs(&native(root, path))?;
        Some((path.get(link.path.len() + 1..), owner))
    })
}

/// the refusal of the first follow of `link` into a folder of the user `owner`, or one holding
/// `entry` of theirs.
#[cfg(unix)]
fn theirs(project: &Project, link: &Followed, entry: Option<&str>, owner: u32) -> Error {
    let owner = crate::cache::said_user(owner);
    let why = match entry {
        Some(entry) => format!("where {entry} belongs to {owner}"),
        None => format!("which belongs to {owner}"),
    };
    refused(
        project,
        link,
        &why,
        "point the link at a folder of your own; to share bytes with others, share a cache (FETCHLOOM_CACHE) rather than a data folder",
    )
}

/// the refusal of the first follow of `link`, which `why` says what a run could harm, with `help`
/// and, for a link git tracks, how it came with the clone.
fn refused(project: &Project, link: &Followed, why: &str, help: &str) -> Error {
    project.cloned_link(
        &link.path,
        crate::project::link_refused(&link.at, &link.found, why).help(help),
    )
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

    /// a first follow takes only a folder this user owns whole: another user's folder is named
    /// first, then the first entry of theirs the survey lists there by path, a file or a folder,
    /// as its path below the folder.
    #[cfg(unix)]
    #[test]
    fn a_first_follow_names_the_folder_or_the_first_entry_of_another_user() {
        use std::fs;
        use std::path::Path;

        use super::not_owned;
        use crate::cli::sync::disk::{Folders, Survey};
        use crate::project::{Followed, Project};
        use crate::stop::Stop;
        use crate::threads::Threads;

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("p");
        let data = root.join("data");
        fs::create_dir_all(data.join("iris")).unwrap();
        fs::create_dir(data.join("raw")).unwrap();
        fs::write(data.join("iris").join("iris.csv"), b"a\n").unwrap();
        fs::write(data.join("notes.txt"), b"mine\n").unwrap();
        let project = Project::at(root.clone());
        let survey = Survey::read(
            &project,
            &Folders::data("data"),
            Threads::default(),
            &Stop::default(),
        )
        .unwrap();
        let found = fs::canonicalize(&data).unwrap();
        let link = Followed {
            at: data.clone(),
            path: "data".to_owned(),
            found: found.clone(),
            first: true,
        };
        let (me, other) = (1000, 1001);
        let check = |theirs: &[&Path]| {
            not_owned(&root, &survey, &link, me, &|path: &Path| {
                Some(if theirs.contains(&path) { other } else { me })
            })
        };
        let (notes, raw, iris) = (
            data.join("notes.txt"),
            data.join("raw"),
            data.join("iris").join("iris.csv"),
        );
        assert_eq!(check(&[]), None);
        assert_eq!(check(&[&raw, &notes]), Some((Some("notes.txt"), other)));
        assert_eq!(check(&[&raw]), Some((Some("raw"), other)));
        assert_eq!(check(&[&iris]), Some((Some("iris/iris.csv"), other)));
        assert_eq!(check(&[&notes, &found]), Some((None, other)));
    }
}
