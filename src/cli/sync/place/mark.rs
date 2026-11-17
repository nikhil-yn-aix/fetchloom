//! the .gitignore fetchloom keeps in each dataset folder it places files in, so git ignores what
//! it placed and leaves everything beside it to the person: `*` in a folder only datasets fill,
//! and in one the person shares, the data folder itself or one holding another dataset's folder,
//! the datasets' own entries, each anchored, as `/train.csv` and `/images/`. made only where
//! nothing stands, written to the state before it is made, and rewritten or deleted only while it
//! holds what fetchloom last wrote, by that record. one that only looks like it is never taken.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::{self, Read as _, Write as _};
use std::path::Path;

use super::super::disk::{self, Mark, Survey};
use super::{Action, Scope, Unsure};
use crate::digest;
use crate::output::{Output, Warning};
use crate::place::{self, Placer};
use crate::plan::{self, Target};
use crate::project::manifest::Manifest;
use crate::project::native;
use crate::project::state::State;

/// what a marker of a folder only datasets fill holds: git ignores every entry, the marker too.
const TEXT: &[u8] = b"*\n";

/// the first line of a marker that names the datasets' entries.
const NAMES: &str = "# fetchloom keeps the dataset files placed here out of git, and rewrites this list as they change\n";

/// the most entries a marker names one by one: git matches every line against every path it
/// looks at, so a folder with more takes `*`.
const MOST_NAMED: usize = 1000;

/// the most of a marker read to tell whether it holds what fetchloom wrote, far past what it
/// writes.
const READ_MOST: u64 = 1 << 20;

/// what each dataset folder of `targets` should have for a marker: `*`, or for one the person
/// shares, the data folder itself or one holding another dataset's folder, the top entries of
/// the datasets placed there, anchored, and the marker itself. the files `state` records for a
/// dataset of `unsure` count too, since they stay while nothing tells which it needs.
pub(super) fn contents(
    manifest: &Manifest,
    targets: &[Target],
    (state, unsure): (&State, Unsure<'_>),
) -> BTreeMap<String, Vec<u8>> {
    let mut folder_of: HashMap<&str, String> = HashMap::new();
    let unsure = super::unsure(unsure);
    for dataset in targets
        .iter()
        .map(|target| target.dataset)
        .chain(unsure.iter().copied())
    {
        if !folder_of.contains_key(dataset) {
            folder_of.insert(dataset, plan::folder(manifest, dataset));
        }
    }
    let placing: BTreeSet<&str> = targets
        .iter()
        .map(|target| folder_of[target.dataset].as_str())
        .collect();
    let mut named: BTreeMap<&str, BTreeSet<(&str, bool)>> = placing
        .iter()
        .filter(|folder| shared(manifest, folder))
        .map(|folder| (*folder, BTreeSet::new()))
        .collect();
    let staying = state
        .files
        .iter()
        .filter(|(_, placed)| unsure.contains(placed.dataset.as_str()))
        .map(|(path, placed)| (placed.dataset.as_str(), path.as_str()));
    let paths = targets
        .iter()
        .map(|target| (target.dataset, target.path.as_str()))
        .chain(staying);
    for (dataset, path) in paths {
        let folder = folder_of[dataset].as_str();
        let Some(entries) = named.get_mut(folder) else {
            continue;
        };
        let Some(inside) = path
            .strip_prefix(folder)
            .and_then(|rest| rest.strip_prefix('/'))
        else {
            continue;
        };
        entries.insert(match inside.split_once('/') {
            Some((top, _)) => (top, true),
            None => (inside, false),
        });
    }
    placing
        .into_iter()
        .map(|folder| {
            let text = match named.get(folder) {
                Some(entries) if entries.len() <= MOST_NAMED => names(entries),
                _ => TEXT.to_vec(),
            };
            (folder.to_owned(), text)
        })
        .collect()
}

/// whether the person shares `folder`, a dataset's, with its datasets: the data folder itself, or
/// one holding another dataset's folder, where `*` would hide the person's files too.
fn shared(manifest: &Manifest, folder: &str) -> bool {
    folder == manifest.data_dir
        || manifest.datasets.keys().any(|name| {
            let other = plan::folder(manifest, name);
            other != folder && disk::is_inside(&other, folder)
        })
}

/// a marker naming `entries`, each a name at the top of its folder and whether it is a folder,
/// anchored there, after the marker itself.
fn names(entries: &BTreeSet<(&str, bool)>) -> Vec<u8> {
    let mut text = String::from(NAMES);
    text.push_str("/.gitignore\n");
    for (name, folder) in entries {
        text.push('/');
        text.push_str(&literal(name));
        if *folder {
            text.push('/');
        }
        text.push('\n');
    }
    text.into_bytes()
}

/// `name` as a .gitignore line names it: git reads `[`, `*`, `?` and `\` as a pattern, and drops
/// the spaces that end a line unless each is escaped. no line can hold a line break, so each
/// control character becomes `?`, which matches it, and a few names beside it.
fn literal(name: &str) -> String {
    let kept = name.trim_end_matches(' ');
    let mut line: String = escaped(kept)
        .chars()
        .map(|c| if c.is_control() { '?' } else { c })
        .collect();
    for _ in kept.len()..name.len() {
        line.push_str("\\ ");
    }
    line
}

/// the SHA-256 of what fetchloom last wrote in the marker of `folder`.
fn written(state: &State, folder: &str) -> String {
    state
        .listed
        .get(folder)
        .cloned()
        .unwrap_or_else(|| digest::sha256(TEXT).hex())
}

/// records that the marker of `folder` holds `text`.
fn note(state: &mut State, folder: &str, text: &[u8]) {
    if text == TEXT {
        state.listed.remove(folder);
    } else {
        state
            .listed
            .insert(folder.to_owned(), digest::sha256(text).hex());
    }
}

/// forgets the marker of `folder`, which is the person's now, or gone.
fn forget(state: &mut State, folder: &str) -> bool {
    state.listed.remove(folder);
    state.marked.remove(folder)
}

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
        let wrote = written(&survey.state, folder);
        forget(&mut survey.state, folder);
        if let Some(Mark::Made(_)) = survey.marks.remove(folder) {
            delete(root, placer, folder, &wrote);
        }
    }
    !going.is_empty()
}

/// whether the marker of `folder` is a plain file holding what fetchloom wrote, `wrote` its
/// SHA-256.
fn holds(root: &Path, placer: &Placer, folder: &str, wrote: &str) -> bool {
    let file = native(root, &disk::marker(folder));
    let mut held = Vec::new();
    placer.open_read(&file).is_ok_and(|opened| {
        opened.take(READ_MOST + 1).read_to_end(&mut held).is_ok()
            && held.len() as u64 <= READ_MOST
            && digest::sha256(&held).hex() == wrote
    })
}

/// deletes the marker of `folder` while it holds what fetchloom wrote, `wrote` its SHA-256, and
/// the folders it leaves empty.
fn delete(root: &Path, placer: &mut Placer, folder: &str, wrote: &str) {
    if !holds(root, placer, folder, wrote) {
        return;
    }
    let file = native(root, &disk::marker(folder));
    if let Err(err) = placer.remove(&file, true, None) {
        tracing::debug!(%folder, %err, "could not delete the .gitignore fetchloom made");
    }
}

/// what a run does about the markers of `wanted`, and of every folder whose text `contents` gives:
/// the folders of `wanted` to make one in, each with nothing at the marker's path or whose recorded
/// marker is gone, the folders whose marker should hold another text now, as after a dataset there
/// came or went, and the records of `wanted` to forget, whose path a link, a folder or a special
/// file took.
pub(super) fn planned(
    survey: &Survey,
    wanted: &BTreeSet<String>,
    contents: &BTreeMap<String, Vec<u8>>,
) -> Planned {
    let mut planned = Planned::default();
    for (folder, text) in contents {
        if let Some(Mark::Made(_)) = survey.marks.get(folder)
            && digest::sha256(text).hex() != written(&survey.state, folder)
        {
            planned.rewrite.push(folder.clone());
        }
    }
    for folder in wanted {
        match survey.marks.get(folder) {
            Some(Mark::Made(_)) => {}
            Some(Mark::Gone) => planned.make.push(folder.clone()),
            Some(Mark::Taken) => planned.forget.push(folder.clone()),
            None => {
                let path = disk::marker(folder);
                if !survey.disk.contains_key(&path) && !survey.entries.contains_key(&path) {
                    planned.make.push(folder.clone());
                }
            }
        }
    }
    planned
}

/// what a run does about the markers of the folders it places files in.
#[derive(Debug, Default)]
pub(super) struct Planned {
    pub(super) make: Vec<String>,
    pub(super) rewrite: Vec<String>,
    pub(super) forget: Vec<String>,
}

/// records the markers of `wanted` a run makes, with the texts `contents` gives them, before it
/// makes them, so the state written ahead of placing names each before it exists, and forgets
/// those something else took. the plan, and whether the state changed.
pub(super) fn to_make(
    survey: &mut Survey,
    wanted: &BTreeSet<String>,
    contents: &BTreeMap<String, Vec<u8>>,
) -> (Planned, bool) {
    let planned = planned(survey, wanted, contents);
    let state = &mut survey.state;
    let mut changed = false;
    for folder in &planned.forget {
        changed |= forget(state, folder);
    }
    for folder in &planned.make {
        changed |= state.marked.insert(folder.clone());
        note(
            state,
            folder,
            contents.get(folder).map_or(TEXT, Vec::as_slice),
        );
    }
    (planned, changed)
}

/// makes the marker of each of `folders`, which the state records already, with the text
/// `contents` gives it, and the folders on its way. one that cannot be made is forgotten, so a
/// later run tries again; one whose path something took meanwhile is that something's. the
/// folders it made, each with whether its marker names the datasets' entries.
pub(super) fn make(
    root: &Path,
    placer: &mut Placer,
    state: &mut State,
    folders: Vec<String>,
    contents: &BTreeMap<String, Vec<u8>>,
) -> Vec<(String, bool)> {
    folders
        .into_iter()
        .filter_map(|folder| {
            let text = contents.get(&folder).map_or(TEXT, Vec::as_slice);
            match make_one(root, placer, &folder, text) {
                Ok(()) => Some((folder, text != TEXT)),
                Err(err) => {
                    tracing::debug!(%folder, %err, "could not make the folder's .gitignore");
                    forget(state, &folder);
                    None
                }
            }
        })
        .collect()
}

/// writes `text` in the marker of `folder` under a hidden name and flushes it before naming it,
/// never replacing anything, so its name never holds less than all of it.
fn make_one(root: &Path, placer: &mut Placer, folder: &str, text: &[u8]) -> io::Result<()> {
    let marker = native(root, &disk::marker(folder));
    placer.check_folders(&marker, true)?;
    let hidden = written_beside(placer, &marker, text)?;
    let named = placer.move_aside_to(&hidden, &marker);
    if named.is_err()
        && let Err(err) = placer.delete_file(&hidden)
    {
        tracing::debug!(%folder, %err, "could not delete an unnamed .gitignore");
    }
    named
}

/// `text` written and flushed in a new file under a hidden name beside `marker`, readable as the
/// person's own files are, so git under another account reads it too.
fn written_beside(placer: &Placer, marker: &Path, text: &[u8]) -> io::Result<std::path::PathBuf> {
    let hidden = marker.with_file_name(place::copy_temporary_name());
    let filled = placer.create_shared_file(&hidden).and_then(|mut file| {
        file.write_all(text)?;
        file.sync_all()
    });
    match filled {
        Ok(()) => Ok(hidden),
        Err(err) => {
            if let Err(err) = placer.delete_file(&hidden) {
                tracing::debug!(%err, "could not delete an unfinished .gitignore");
            }
            Err(err)
        }
    }
}

/// gives the marker of each of `folders` the text `contents` has for it now, as when a dataset's
/// entries changed, while it holds what fetchloom last wrote; one the person changed is theirs,
/// and its record goes. one holding the new text already, as a run cut short after writing it
/// leaves it, is only recorded. whether the state changed, and the folders whose marker went
/// from `*` to naming the datasets' entries or back, each with whether it names them now.
pub(super) fn rewrite(
    root: &Path,
    placer: &mut Placer,
    state: &mut State,
    folders: &[String],
    contents: &BTreeMap<String, Vec<u8>>,
) -> (bool, Vec<(String, bool)>) {
    let mut changed = false;
    let mut turned = Vec::new();
    for folder in folders {
        let text = contents.get(folder).map_or(TEXT, Vec::as_slice);
        let was_named = state.listed.contains_key(folder);
        if holds(root, placer, folder, &digest::sha256(text).hex()) {
            note(state, folder, text);
            changed = true;
            continue;
        }
        if !holds(root, placer, folder, &written(state, folder)) {
            changed |= forget(state, folder);
            continue;
        }
        let marker = native(root, &disk::marker(folder));
        let replaced = written_beside(placer, &marker, text).and_then(|hidden| {
            let named = placer.rename_file(&hidden, &marker);
            if named.is_err()
                && let Err(err) = placer.delete_file(&hidden)
            {
                tracing::debug!(%folder, %err, "could not delete an unnamed .gitignore");
            }
            named
        });
        match replaced {
            Ok(()) => {
                note(state, folder, text);
                changed = true;
                let named = text != TEXT;
                if named != was_named {
                    turned.push((folder.clone(), named));
                }
            }
            Err(err) => tracing::debug!(%folder, %err, "could not rewrite the folder's .gitignore"),
        }
    }
    (changed, turned)
}

/// takes back the markers of `made` that a rolled back add leaves in folders holding no file
/// fetchloom placed. says whether the state changed.
pub(super) fn take_back(
    root: &Path,
    placer: &mut Placer,
    state: &mut State,
    made: &[(String, bool)],
) -> bool {
    let mut changed = false;
    for (folder, _) in made {
        let wrote = written(state, folder);
        if !holds_placed(state, folder) && forget(state, folder) {
            delete(root, placer, folder, &wrote);
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

/// once markers keep dataset folders out of git, says once to a person at a terminal of a line of
/// the project's .gitignore that still hides the whole data folder, the person's own files there
/// too, which fetchloom never edits: theirs to keep, or to take out for git to see those files.
/// says whether the state changed.
pub(super) fn tell_hiding_line(
    output: &Output,
    root: &Path,
    data_dir: &str,
    state: &mut State,
) -> bool {
    // a choice for a person at a terminal: a script, or a fresh clone in CI, has none to make
    if !output.watched_story() || output.json() {
        return false;
    }
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
            "each dataset's folder keeps itself out of git, so the line now only hides your own files in {data_dir}/: keep it if that is what you want, or take it out for git to see them. fetchloom never edits .gitignore"
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

/// the folders of `wanted` a run would give a marker, each with whether its marker names the
/// datasets' entries, as [`planned`] and `contents` decide.
pub(super) fn would_make(
    survey: &Survey,
    wanted: &BTreeSet<String>,
    contents: &BTreeMap<String, Vec<u8>>,
) -> Vec<(String, bool)> {
    planned(survey, wanted, contents)
        .make
        .into_iter()
        .map(|folder| {
            let named = contents.get(&folder).is_some_and(|text| text != TEXT);
            (folder, named)
        })
        .collect()
}

/// the lines for the folders a run marked, `made`, each with whether its marker names the
/// datasets' entries: what git ignores now, and whether the person's own files there are hidden
/// too. outside a git repository, `in_git` false, details.
pub(super) fn tell_made(output: &Output, made: &[(String, bool)], in_git: bool, verb: &str) {
    for named in [false, true] {
        let folders: Vec<&str> = made
            .iter()
            .filter(|(_, kind)| *kind == named)
            .map(|(folder, _)| folder.as_str())
            .collect();
        let count = crate::units::grouped(folders.len() as u64);
        let line = match (folders.as_slice(), named) {
            ([], _) => continue,
            ([one], false) => format!(
                "{one} in git with {}, which also hides files you put there",
                disk::marker(one)
            ),
            (_, false) => format!(
                "{count} dataset folders in git, each with a .gitignore in it, which also hides files you put there"
            ),
            ([one], true) => format!(
                "the dataset files in {one} in git with {}, which names them, so files you put there stay in git",
                disk::marker(one)
            ),
            (_, true) => format!(
                "the dataset files in {count} folders in git, each with a .gitignore naming them, so files you put there stay in git"
            ),
        };
        if in_git {
            output.step(verb, &line);
        } else {
            output.detail(&format!("{verb} {line}"));
        }
        if folders.len() > 1 {
            for folder in folders {
                output.detail(&disk::marker(folder));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{hiding_line, names};

    /// git, as a marker naming entries reads it, ignores each entry it names, whatever its name
    /// holds, and nothing the person keeps beside them. none of these names exists on windows.
    #[cfg(unix)]
    #[test]
    fn git_ignores_each_entry_a_marker_names_and_nothing_beside() {
        use std::process::Command;
        let root = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(root.path())
                .output()
                .ok()
                .filter(|output| output.status.success())
        };
        if git(&["init", "-q"]).is_none() {
            return;
        }
        let named = [
            ("[a]", false),
            ("x*", false),
            ("end ", false),
            ("two\nlines", false),
            ("#notes", false),
            ("!keep", false),
            ("sub?", true),
        ];
        let theirs = ["a", "xy", "end", "two", "notes", "keep", "subs/f", "x*y"];
        let entries: BTreeSet<(&str, bool)> = named.into_iter().collect();
        std::fs::write(root.path().join(".gitignore"), names(&entries)).unwrap();
        for (name, folder) in named {
            let file = if folder {
                std::fs::create_dir(root.path().join(name)).unwrap();
                root.path().join(name).join("f")
            } else {
                root.path().join(name)
            };
            std::fs::write(file, "dataset").unwrap();
        }
        for name in theirs {
            let file = root.path().join(name);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, "mine").unwrap();
        }
        let seen = git(&["ls-files", "--others", "--exclude-standard", "-z"]).unwrap();
        let mut seen: Vec<String> = String::from_utf8(seen.stdout)
            .unwrap()
            .split_terminator('\0')
            .map(str::to_owned)
            .collect();
        seen.sort();
        let mut expected: Vec<String> = theirs.iter().map(|name| (*name).to_owned()).collect();
        expected.sort();
        assert_eq!(seen, expected);
    }

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
