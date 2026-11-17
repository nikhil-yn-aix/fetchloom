//! the .gitignore fetchloom keeps in each dataset folder it places files in, holding `*`, so git
//! ignores that folder and leaves everything beside it to the person: made only where nothing
//! stands, written to the state before it is made, and deleted only by that record, once no file
//! fetchloom placed needs the folder. a .gitignore that only looks like one is never taken for it.

use std::collections::{BTreeSet, HashSet};
use std::io::{self, Read as _, Write as _};
use std::path::Path;

use super::super::disk::{self, Mark, Survey};
use super::{Action, Scope, Unsure};
use crate::output::{Output, Warning};
use crate::place::{self, Placer};
use crate::plan::{self, Target};
use crate::project::manifest::Manifest;
use crate::project::native;
use crate::project::state::State;

/// what a marker holds: git ignores every entry of its folder, the marker too.
const TEXT: &[u8] = b"*\n";

/// the dataset folders of the targets `chosen` picks that a marker can keep out of git: none
/// whose marker's path is a target's, a .gitignore the dataset places itself.
fn folders_of(
    manifest: &Manifest,
    targets: &[Target],
    chosen: impl Fn(usize) -> bool,
) -> BTreeSet<String> {
    let mut datasets = BTreeSet::new();
    let mut ignores = HashSet::new();
    for (index, target) in targets.iter().enumerate() {
        if target.path.ends_with("/.gitignore") {
            ignores.insert(target.path.as_str());
        }
        if chosen(index) {
            datasets.insert(target.dataset);
        }
    }
    datasets
        .into_iter()
        .map(|dataset| plan::folder(manifest, dataset))
        .filter(|folder| !ignores.contains(disk::marker(folder).as_str()))
        .collect()
}

/// the dataset folders of the run's datasets, as `actions` select them, whose markers it makes.
pub(super) fn wanted(
    manifest: &Manifest,
    targets: &[Target],
    actions: &[Action],
) -> BTreeSet<String> {
    folders_of(manifest, targets, |index| {
        !matches!(actions[index], Action::Unselected)
    })
}

/// the dataset folders of every dataset `targets` places, and of those whose files are not
/// known yet, as `unsure` names them, whose markers stay.
fn needed(manifest: &Manifest, targets: &[Target], unsure: Unsure<'_>) -> BTreeSet<String> {
    let (unknown, waiting) = unsure;
    let mut needed = folders_of(manifest, targets, |_| true);
    let unknown = unknown.iter().map(|(dataset, _)| *dataset);
    needed.extend(
        unknown
            .chain(waiting.iter().copied())
            .map(|dataset| plan::folder(manifest, dataset)),
    );
    needed
}

/// whether the state records a file fetchloom placed in `folder` or below it.
fn holds_placed(state: &State, folder: &str) -> bool {
    let below = format!("{folder}/");
    state
        .files
        .range(below.clone()..)
        .next()
        .is_some_and(|(path, _)| path.starts_with(&below))
}

/// forgets the marker of each folder the state records that no dataset of `targets` or `unsure`
/// places files in and that holds no file fetchloom placed, as after a dataset's removal or move,
/// and deletes it while it holds what fetchloom wrote, with the folders it leaves empty. one a
/// person changed stays theirs, and so does one outside the data folder, where a sync deletes
/// nothing. says whether the state changed.
pub(super) fn retire(
    root: &Path,
    manifest: &Manifest,
    placer: &mut Placer,
    survey: &mut Survey,
    (targets, unsure): (&[Target], Unsure<'_>),
) -> bool {
    let needed = needed(manifest, targets, unsure);
    let going: Vec<String> = survey
        .state
        .marked
        .iter()
        .filter(|folder| !needed.contains(*folder) && !holds_placed(&survey.state, folder))
        .cloned()
        .collect();
    for folder in &going {
        survey.state.marked.remove(folder);
        if let Some(Mark::Made(_)) = survey.marks.remove(folder) {
            delete(root, placer, folder);
        }
    }
    !going.is_empty()
}

/// deletes the marker of `folder` while it is a plain file holding what fetchloom wrote, and the
/// folders it leaves empty.
fn delete(root: &Path, placer: &mut Placer, folder: &str) {
    let file = native(root, &disk::marker(folder));
    let mut held = Vec::new();
    let unchanged = placer.open_read(&file).is_ok_and(|opened| {
        opened
            .take(TEXT.len() as u64 + 1)
            .read_to_end(&mut held)
            .is_ok()
            && held == TEXT
    });
    if !unchanged {
        return;
    }
    if let Err(err) = placer.remove(&file, true, None) {
        tracing::debug!(%folder, %err, "could not delete the .gitignore fetchloom made");
    }
}

/// what a run does about the markers of `wanted`: the folders to make one in, each with nothing
/// at the marker's path or whose recorded marker is gone, and the records to forget, whose path a
/// link, a folder or a special file took.
pub(super) fn planned(survey: &Survey, wanted: &BTreeSet<String>) -> (Vec<String>, Vec<String>) {
    let (mut make, mut forget) = (Vec::new(), Vec::new());
    for folder in wanted {
        match survey.marks.get(folder) {
            Some(Mark::Made(_)) => {}
            Some(Mark::Gone) => make.push(folder.clone()),
            Some(Mark::Taken) => forget.push(folder.clone()),
            None => {
                let path = disk::marker(folder);
                if !survey.disk.contains_key(&path) && !survey.entries.contains_key(&path) {
                    make.push(folder.clone());
                }
            }
        }
    }
    (make, forget)
}

/// records the markers of `wanted` a run makes, before it makes them, so the state written ahead
/// of placing names each before it exists, and forgets those something else took. the folders
/// to make, and whether the state changed.
pub(super) fn to_make(survey: &mut Survey, wanted: &BTreeSet<String>) -> (Vec<String>, bool) {
    let (make, forget) = planned(survey, wanted);
    let state = &mut survey.state;
    let mut changed = false;
    for folder in forget {
        changed |= state.marked.remove(&folder);
    }
    for folder in &make {
        changed |= state.marked.insert(folder.clone());
    }
    (make, changed)
}

/// makes the marker of each of `folders`, which the state records already, and the folders on
/// its way. one that cannot be made is forgotten, so a later run tries again; one whose path
/// something took meanwhile is that something's. the folders it made.
pub(super) fn make(
    root: &Path,
    placer: &mut Placer,
    state: &mut State,
    folders: Vec<String>,
) -> Vec<String> {
    folders
        .into_iter()
        .filter(|folder| match make_one(root, placer, folder) {
            Ok(()) => true,
            Err(err) => {
                tracing::debug!(%folder, %err, "could not make the folder's .gitignore");
                state.marked.remove(folder);
                false
            }
        })
        .collect()
}

/// writes the marker of `folder` under a hidden name and flushes it before naming it, never
/// replacing anything, so its name never holds less than all of it.
fn make_one(root: &Path, placer: &mut Placer, folder: &str) -> io::Result<()> {
    let marker = native(root, &disk::marker(folder));
    placer.check_folders(&marker, true)?;
    let hidden = marker.with_file_name(place::copy_temporary_name());
    let written = placer.create_file(&hidden, false).and_then(|mut file| {
        file.write_all(TEXT)?;
        file.sync_all()
    });
    let named = written.and_then(|()| placer.move_aside_to(&hidden, &marker));
    if named.is_err()
        && let Err(err) = placer.delete_file(&hidden)
    {
        tracing::debug!(%folder, %err, "could not delete an unnamed .gitignore");
    }
    named
}

/// takes back the markers of `made` that a rolled back add leaves in folders holding no file
/// fetchloom placed. says whether the state changed.
pub(super) fn take_back(
    root: &Path,
    placer: &mut Placer,
    state: &mut State,
    made: &[String],
) -> bool {
    let mut changed = false;
    for folder in made {
        if !holds_placed(state, folder) && state.marked.remove(folder) {
            delete(root, placer, folder);
            changed = true;
        }
    }
    changed
}

/// whether every dataset folder of `scope` holding files fetchloom placed has its marker, or an
/// entry of someone else's where the marker would go, and no record waits to be forgotten, as
/// a run that changes nothing leaves the markers.
pub(crate) fn settled(manifest: &Manifest, survey: &Survey, scope: &Scope) -> bool {
    let state = &survey.state;
    let placed: BTreeSet<String> = manifest
        .datasets
        .iter()
        .filter(|(_, entry)| entry.place)
        .map(|(name, _)| plan::folder(manifest, name))
        .collect();
    let marked = manifest
        .datasets
        .iter()
        .filter(|(name, entry)| entry.place && scope.includes(name))
        .map(|(name, _)| plan::folder(manifest, name))
        .filter(|folder| holds_placed(state, folder))
        .all(|folder| match survey.marks.get(&folder) {
            Some(Mark::Made(_)) => true,
            Some(Mark::Gone | Mark::Taken) => false,
            None => {
                let path = disk::marker(&folder);
                survey.disk.contains_key(&path) || survey.entries.contains_key(&path)
            }
        });
    marked
        && survey
            .marks
            .keys()
            .all(|folder| placed.contains(folder) || holds_placed(state, folder))
}

/// the dataset folders whose own .gitignore, at the top, `actions` place, so no marker keeps
/// them out of git, and that no line of the .gitignore of the project at `root` hides.
pub(super) fn unmarkable(
    root: &Path,
    manifest: &Manifest,
    targets: &[Target],
    actions: &[Action],
) -> BTreeSet<String> {
    targets
        .iter()
        .zip(actions)
        .filter(|(_, action)| {
            matches!(
                action,
                Action::Place
                    | Action::Replace
                    | Action::Freed(_)
                    | Action::Unblock(_)
                    | Action::Overwrite(_)
            )
        })
        .map(|(target, _)| (target, plan::folder(manifest, target.dataset)))
        .filter(|(target, folder)| target.path == disk::marker(folder))
        .map(|(_, folder)| folder)
        .filter(|folder| hiding_line(root, folder, false).is_none())
        .collect()
}

/// a line of the project's .gitignore at `root` that hides `folder` from git whole: the folder,
/// one holding it, or all that one holds, as `/data/`, `data` or `/data/*`. its number from 1 and
/// its text. none when the file cannot be read, holds no such line, or, with `deliberate`,
/// takes any path below the top of `folder` back with `!`, as a person who means it writes.
fn hiding_line(root: &Path, folder: &str, deliberate: bool) -> Option<(usize, String)> {
    let text = crate::project::read_gitignore(&root.join(".gitignore")).ok()?;
    let top = folder.split('/').next().unwrap_or(folder);
    let taken_back = |line: &str| {
        line.strip_prefix('!').is_some_and(|rule| {
            rule.trim_start_matches('/')
                .starts_with(&format!("{}/", escaped(top)))
        })
    };
    if deliberate && text.lines().any(taken_back) {
        return None;
    }
    let mut rules = Vec::new();
    let mut at = Some(folder);
    while let Some(current) = at {
        let literal = escaped(current);
        for form in [
            "/{}/", "{}/", "/{}", "{}", "/{}/*", "{}/*", "/{}/**", "{}/**",
        ] {
            rules.push(form.replace("{}", &literal));
        }
        at = current.rsplit_once('/').map(|(parent, _)| parent);
    }
    text.lines().enumerate().find_map(|(index, line)| {
        // git drops spaces that end a line, and a file saved on windows ends each with \r
        let rule = line.trim_end();
        rules
            .iter()
            .any(|form| form == rule)
            .then(|| (index + 1, rule.to_owned()))
    })
}

/// `folder` as a .gitignore line names it and nothing else: git reads `[`, `*`, `?` and `\` in a
/// line as a pattern.
fn escaped(folder: &str) -> String {
    folder
        .chars()
        .flat_map(|c| {
            let escape = matches!(c, '[' | ']' | '*' | '?' | '\\').then_some('\\');
            escape.into_iter().chain([c])
        })
        .collect()
}

/// once markers keep dataset folders out of git, says once of a line of the project's .gitignore
/// that still hides the whole data folder, the person's own files there too, which fetchloom
/// never edits: the line to delete, when the person wants git to see them. says whether the state
/// changed.
pub(super) fn tell_hiding_line(
    output: &Output,
    root: &Path,
    data_dir: &str,
    state: &mut State,
) -> bool {
    let Some((number, rule)) = hiding_line(root, data_dir, true) else {
        return false;
    };
    if state.rule_told.as_ref() == Some(&rule) {
        return false;
    }
    output.warn_with(
        Warning::GitDataFolderIgnored,
        &format!(
            "line {number} of .gitignore, {rule}, keeps all of {data_dir}/ out of git, your own files there too"
        ),
        &format!(
            "each dataset's folder keeps itself out of git now, so delete that line for git to see the rest of {data_dir}/. fetchloom never edits .gitignore"
        ),
    );
    state.rule_told = Some(rule);
    true
}

/// warns of each of `folders`, as [`unmarkable`] finds them: git sees the dataset's files.
pub(super) fn warn_unmarkable(output: &Output, folders: &BTreeSet<String>) {
    for folder in folders {
        output.warn_with(
            Warning::GitDatasetUnignored,
            &format!(
                "{folder} holds a .gitignore of the dataset's own, so fetchloom cannot keep {folder} out of git"
            ),
            &format!(
                "to keep it out, add the line /{}/ to .gitignore",
                escaped(folder)
            ),
        );
    }
}

/// the line for the folders a run marked, `made`: what git ignores now, and that the person's own
/// files there are hidden too. outside a git repository, `in_git` false, a detail.
pub(super) fn tell_made(output: &Output, made: &[String], in_git: bool, verb: &str) {
    let (said, files) = match made {
        [] => return,
        [one] => (format!("{one} in git with {}", disk::marker(one)), false),
        many => (
            format!(
                "{} dataset folders in git, each with a .gitignore in it",
                crate::units::grouped(many.len() as u64)
            ),
            true,
        ),
    };
    let line = format!("{said}, which also hides files you put there");
    if in_git {
        output.step(verb, &line);
    } else {
        output.detail(&format!("{verb} {line}"));
    }
    if files {
        for folder in made {
            output.detail(&disk::marker(folder));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::hiding_line;

    /// a line hides a folder whole as git reads it: the folder or one holding it, with or without
    /// its slashes, or all they hold. spaces that end a line and windows' line ends count for
    /// nothing, spaces before it do, and a line of a folder inside it hides only part of it. a
    /// line taking a path below the folder's top back means the person wrote the rules on
    /// purpose, which `deliberate` respects.
    #[test]
    fn a_line_hides_a_folder_whole_as_git_reads_it() {
        let root = tempfile::tempdir().unwrap();
        let found = |text: &str, folder: &str, deliberate: bool| {
            std::fs::write(root.path().join(".gitignore"), text).unwrap();
            hiding_line(root.path(), folder, deliberate)
        };
        for line in [
            "/data/", "data/", "/data", "data", "/data/*", "data/*", "/data/**", "data/**",
        ] {
            let text = format!("# raw files\n*.tmp\n{line}  \r\n");
            assert_eq!(
                found(&text, "data", true),
                Some((3, line.to_owned())),
                "{line}"
            );
        }
        assert_eq!(
            found("/inputs/\n", "inputs/raw", true),
            Some((1, "/inputs/".to_owned()))
        );
        assert_eq!(
            found("/data\\[1\\]/\n", "data[1]", true),
            Some((1, "/data\\[1\\]/".to_owned()))
        );
        for text in [
            "/data/raw/\n",
            "  data/\n",
            "# /data/\n",
            "/database/\n",
            "",
        ] {
            assert_eq!(found(text, "data", true), None, "{text:?}");
        }
        let taken_back = "/data/*\n!/data/processed/\n";
        assert_eq!(found(taken_back, "data", true), None);
        assert_eq!(
            found(taken_back, "data/iris", false),
            Some((1, "/data/*".to_owned()))
        );
    }
}
