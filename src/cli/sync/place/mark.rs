//! the .gitignore fetchloom keeps in each dataset folder it places files in, so git ignores what
//! it placed and leaves everything beside it to the person: `*` in a folder only datasets fill,
//! and in one the person shares, the data folder itself or one holding another dataset's folder,
//! the datasets' own entries, each anchored, as `/train.csv` and `/images/`. one writer makes and
//! rewrites them all, and one record in the state, the SHA-256 of what fetchloom last wrote in
//! each, says which are fetchloom's: made only where nothing stands, recorded before it is made,
//! and rewritten or deleted only while it holds what the record says. one that only looks like it
//! is never taken.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::{self, Read as _, Write as _};
use std::path::Path;

use super::super::disk::{self, Mark, Survey};
use super::{Action, Placement, Scope, Unsure};
use crate::digest;
use crate::output::{Output, Warning};
use crate::place::{self, Placer};
use crate::plan::{self, Target};
use crate::project::manifest::Manifest;
use crate::project::state::{Once, State, Told};
use crate::project::{Project, gitignore_escaped, native};

/// what a marker of a folder only datasets fill holds: git ignores every entry, the marker too.
const TEXT: &[u8] = b"*\n";

/// the first line of a marker that names the datasets' entries.
const NAMES: &str = "# fetchloom keeps the dataset files placed here out of git, and rewrites this list as they change\n";

/// the most entries a marker names one by one: git matches every line against every path it
/// looks at, so a folder the person shares with more gets no marker.
const MOST_NAMED: usize = 1000;

/// the most of a marker read to tell whether it holds what fetchloom wrote, far past what it
/// writes.
const READ_MOST: u64 = 1 << 20;

/// what the marker of a dataset folder holds, as [`contents`] decides it.
#[derive(Debug)]
pub(super) enum Content {
    /// the marker's text.
    Text(Vec<u8>),
    /// no marker: the person shares the folder, and its datasets put this many entries at its
    /// top, more than a marker names one by one, while `*` would hide the person's files too.
    Unnamed(usize),
    /// no marker: git keeps the folder, an output's, as [`kept_in_git`] finds.
    Tracked,
}

/// what each dataset folder of `targets` should have for a marker: `*`, or for one the person
/// shares, the data folder itself or one holding another dataset's folder, the top entries of
/// the datasets placed there, anchored, and the marker itself, or none past [`MOST_NAMED`] of
/// them, and none in an output's folder git keeps. the files the survey's state records for a
/// dataset of `unsure` count too, since they stay while nothing tells which it needs.
pub(super) fn contents(
    manifest: &Manifest,
    targets: &[Target],
    (survey, unsure): (&Survey, Unsure<'_>),
) -> BTreeMap<String, Content> {
    let state = &survey.state;
    let unsure = super::unsure(unsure);
    // the plan lists each dataset's targets together, so each run of them is looked at once
    let runs: Vec<&[Target]> = targets
        .chunk_by(|one, next| one.dataset == next.dataset)
        .collect();
    let folder_of: HashMap<&str, String> = runs
        .iter()
        .map(|run| run[0].dataset)
        .chain(unsure.iter().copied())
        .map(|dataset| (dataset, plan::folder(manifest, dataset)))
        .collect();
    let placing: BTreeSet<&str> = runs
        .iter()
        .map(|run| folder_of[run[0].dataset].as_str())
        .collect();
    let mut named: HashMap<&str, Entries> = placing
        .iter()
        .filter(|folder| shared(manifest, folder))
        .map(|folder| (*folder, Entries::default()))
        .collect();
    if !named.is_empty() {
        for run in &runs {
            let folder = folder_of[run[0].dataset].as_str();
            if let Some(entries) = named.get_mut(folder) {
                for target in *run {
                    entries.add(folder, &target.path);
                }
            }
        }
        // a dataset whose files are not all known keeps those placed, which targets may name too
        let staying = state
            .files
            .iter()
            .filter(|(_, placed)| unsure.contains(placed.dataset.as_str()));
        if !unsure.is_empty() {
            for (path, placed) in staying {
                let folder = folder_of[placed.dataset.as_str()].as_str();
                if let Some(entries) = named.get_mut(folder) {
                    entries.add(folder, path);
                    entries.again = true;
                }
            }
        }
    }
    placing
        .into_iter()
        .map(|folder| {
            let content = match named.remove(folder) {
                _ if kept_in_git(manifest, survey, folder) => Content::Tracked,
                None => Content::Text(TEXT.to_vec()),
                Some(entries) => entries.content(),
            };
            (folder.to_owned(), content)
        })
        .collect()
}

/// the names at the top of a folder the person shares that its datasets place: files, which
/// targets name once each, and folders, which many files share.
#[derive(Default)]
struct Entries<'a> {
    files: Vec<&'a str>,
    folders: HashSet<&'a str>,
    /// whether a name may have come twice, as from both a target and a record.
    again: bool,
}

impl<'a> Entries<'a> {
    /// notes the top name of `path`, a path in `folder`.
    fn add(&mut self, folder: &str, path: &'a str) {
        let Some(inside) = path
            .strip_prefix(folder)
            .and_then(|rest| rest.strip_prefix('/'))
        else {
            return;
        };
        match inside.split_once('/') {
            Some((top, _)) => {
                self.folders.insert(top);
            }
            None => self.files.push(inside),
        }
    }

    /// the marker naming each entry, or none past [`MOST_NAMED`] of them.
    fn content(mut self) -> Content {
        if self.again {
            self.files.sort_unstable();
            self.files.dedup();
        }
        let count = self.files.len() + self.folders.len();
        if count > MOST_NAMED {
            return Content::Unnamed(count);
        }
        let entries: BTreeSet<(&str, bool)> = self
            .files
            .iter()
            .map(|file| (*file, false))
            .chain(self.folders.iter().map(|folder| (*folder, true)))
            .collect();
        Content::Text(names(&entries))
    }
}

/// the folders of `wanted` that `contents` gives no marker, as too full to name, each with how
/// many entries its datasets put at its top.
pub(super) fn unnamed(
    wanted: &BTreeSet<String>,
    contents: &BTreeMap<String, Content>,
) -> Vec<(String, usize)> {
    wanted
        .iter()
        .filter_map(|folder| match contents.get(folder) {
            Some(Content::Unnamed(entries)) => Some((folder.clone(), *entries)),
            _ => None,
        })
        .collect()
}

/// whether git keeps `folder`, an output's, tracking the person's files there: as the pin this run
/// makes found, or else as the state records the last pin found. a marker would hide their next
/// file there from git.
fn kept_in_git(manifest: &Manifest, survey: &Survey, folder: &str) -> bool {
    let kept = match &survey.git_keeps {
        Some((at, kept)) if at == folder => *kept,
        _ => survey.state.told.has(Once::Tracked, folder),
    };
    kept && manifest
        .datasets
        .values()
        .any(|entry| entry.at.as_deref() == Some(folder))
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
        text.push_str(&gitignore_escaped(name));
        if *folder {
            text.push('/');
        }
        text.push('\n');
    }
    text.into_bytes()
}

/// records that the marker of `folder` holds `text`.
fn note(state: &mut State, folder: &str, text: &[u8]) {
    state
        .markers
        .insert(folder.to_owned(), digest::sha256(text).hex());
}

/// forgets the marker of `folder`, which is the person's now, or gone.
fn forget(state: &mut State, folder: &str) -> bool {
    state.markers.remove(folder).is_some()
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

/// the dataset folders of every dataset `targets` places, of those whose files are not known yet,
/// as `unsure` names them, and of every output, whose markers stay: a dropped output's folder
/// still holds the files data.lock pins.
fn needed(manifest: &Manifest, targets: &[Target], unsure: Unsure<'_>) -> BTreeSet<String> {
    let (unknown, waiting) = unsure;
    let mut needed = folders_of(manifest, targets, |_| true);
    let unknown = unknown.iter().map(|(dataset, _)| *dataset);
    let outputs = manifest
        .datasets
        .iter()
        .filter(|(_, entry)| entry.is_output())
        .map(|(name, _)| name.as_str());
    needed.extend(
        unknown
            .chain(waiting.iter().copied())
            .chain(outputs)
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
/// places files in, that is none of `kept`, and that holds no file fetchloom placed, as after a
/// dataset's removal or move, and deletes it while it holds what fetchloom wrote, with the folders
/// it leaves empty. one a person changed stays theirs, and so does one outside the folders the run
/// lists, where a sync deletes nothing. says whether the state changed.
pub(super) fn retire(
    root: &Path,
    manifest: &Manifest,
    placer: &mut Placer,
    survey: &mut Survey,
    (targets, unsure): (&[Target], Unsure<'_>),
    kept: &[String],
) -> bool {
    let mut needed = needed(manifest, targets, unsure);
    needed.extend(kept.iter().cloned());
    let going: Vec<String> = survey
        .state
        .markers
        .keys()
        .filter(|folder| !needed.contains(*folder) && !holds_placed(&survey.state, folder))
        .cloned()
        .collect();
    for folder in &going {
        let wrote = survey.state.markers.remove(folder);
        if let (Some(wrote), Some(Mark::Made(_))) = (wrote, survey.marks.remove(folder)) {
            delete(root, placer, folder, &wrote);
        }
    }
    !going.is_empty()
}

/// forgets the marker of each of `folders`, outputs' folders a removal took out of data.toml that
/// lie outside every folder the run lists, once none holds a file fetchloom placed, and deletes it
/// as [`retire`] does, through a placer rooted at the folder, which no link leads to and which is
/// apart from the cache. says whether the state changed.
pub(super) fn retire_removed(project: &Project, state: &mut State, folders: &[String]) -> bool {
    let mut changed = false;
    for folder in folders {
        if holds_placed(state, folder) {
            continue;
        }
        let Some(wrote) = state.markers.remove(folder) else {
            continue;
        };
        changed = true;
        let apart = crate::cli::cache_folder()
            .is_none_or(|cache| project.refuse_cache_overlap(folder, &cache, true).is_ok());
        if apart && project.refuse_linked_data_dir(folder).is_ok() {
            let mut placer = Placer::new(&native(&project.root, folder));
            delete(&project.root, &mut placer, folder, &wrote);
        }
    }
    changed
}

/// whether the marker of `folder` is a plain file holding what fetchloom wrote, `wrote` its
/// SHA-256, in the form fetchloom writes, so a record that came with a copied state takes no
/// other .gitignore.
fn holds(root: &Path, placer: &Placer, folder: &str, wrote: &str) -> bool {
    let file = native(root, &disk::marker(folder));
    let mut held = Vec::new();
    placer.open_read(&file).is_ok_and(|opened| {
        opened.take(READ_MOST + 1).read_to_end(&mut held).is_ok()
            && in_marker_form(&held)
            && digest::sha256(&held).hex() == wrote
    })
}

/// whether `text`, a .gitignore's, is in the form fetchloom writes a marker in: `*`, or the
/// datasets' entries under its first line, no longer than a marker is read.
fn in_marker_form(text: &[u8]) -> bool {
    text.len() as u64 <= READ_MOST && (text == TEXT || text.starts_with(NAMES.as_bytes()))
}

/// whether `file`, a .gitignore, holds a marker in the form fetchloom writes, whatever the state
/// records of it, as one a folder took along when it was renamed: fetchloom's own, so never part
/// of an output, nor a change to one. a link is not followed.
pub(crate) fn is_marker_form(file: &Path) -> bool {
    let mut held = Vec::new();
    crate::volume::read_unfollowed(file).is_ok_and(|opened| {
        opened.take(READ_MOST + 1).read_to_end(&mut held).is_ok() && in_marker_form(&held)
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

/// what a run does about the markers of `wanted`, and of every folder `contents` decides on: the
/// folders whose marker it writes, those of `wanted` with nothing at the marker's path or whose
/// recorded marker is gone, and those whose marker of fetchloom's should hold another text now, as
/// after a dataset there came or went, or none, and the records of `wanted` to forget, whose path
/// a link, a folder or a special file took, or whose folder gets no marker now.
pub(super) fn planned(
    survey: &Survey,
    wanted: &BTreeSet<String>,
    contents: &BTreeMap<String, Content>,
) -> Planned {
    let mut planned = Planned::default();
    for folder in contents.keys() {
        let text = text_of(contents, folder);
        let wrote = survey.state.markers.get(folder);
        if let Some(Mark::Made(_)) = survey.marks.get(folder)
            && text.is_none_or(|text| wrote != Some(&digest::sha256(text).hex()))
        {
            planned.write.push((folder.clone(), true));
        }
    }
    for folder in wanted {
        let none = text_of(contents, folder).is_none();
        match survey.marks.get(folder) {
            Some(Mark::Made(_)) => {}
            Some(Mark::Gone) if none => planned.forget.push(folder.clone()),
            Some(Mark::Gone) => planned.write.push((folder.clone(), false)),
            Some(Mark::Taken) => planned.forget.push(folder.clone()),
            None if none => {}
            None => {
                let path = disk::marker(folder);
                if !survey.disk.contains_key(&path) && !survey.entries.contains_key(&path) {
                    planned.write.push((folder.clone(), false));
                }
            }
        }
    }
    planned
}

/// what a run does about the markers of the folders it places files in: the folders whose
/// marker it writes, each with whether fetchloom's own stands there to rewrite, or to delete when
/// the folder gets none now, and the records it forgets.
#[derive(Debug, Default)]
pub(super) struct Planned {
    pub(super) write: Vec<(String, bool)>,
    pub(super) forget: Vec<String>,
}

/// plans the markers of `wanted` and of the folders `contents` gives texts for, records each new
/// one with its text before it is made, so the state written ahead of placing names it before it
/// exists, and forgets those something else took. the plan, and whether the state changed.
pub(super) fn to_write(
    survey: &mut Survey,
    wanted: &BTreeSet<String>,
    contents: &BTreeMap<String, Content>,
) -> (Planned, bool) {
    let planned = planned(survey, wanted, contents);
    let state = &mut survey.state;
    let mut changed = false;
    for folder in &planned.forget {
        changed |= forget(state, folder);
    }
    for (folder, _) in planned.write.iter().filter(|(_, there)| !there) {
        if let Some(text) = text_of(contents, folder) {
            note(state, folder, text);
            changed = true;
        }
    }
    (planned, changed)
}

/// the folders of a plan a run would make a marker in, each with whether its marker names the
/// datasets' entries.
pub(super) fn would_make(
    planned: &Planned,
    contents: &BTreeMap<String, Content>,
) -> Vec<(String, bool)> {
    planned
        .write
        .iter()
        .filter(|(_, there)| !there)
        .filter_map(|(folder, _)| Some((folder.clone(), text_of(contents, folder)? != TEXT)))
        .collect()
}

/// those of `made`, folders whose marker a run made or changed, each with whether it names the
/// datasets' entries, whose line no run said to a person in git yet in that kind, as `state`
/// records, so a folder a pipeline writes again is told of once.
pub(super) fn untold(state: &State, made: &[(String, bool)]) -> Vec<(String, bool)> {
    made.iter()
        .filter(|(folder, named)| !state.told.has(said_as(*named), folder))
        .cloned()
        .collect()
}

/// records that a person in git heard the lines of `said`, each a folder with whether its marker
/// names the datasets' entries, and forgets the other kind's there, so a change back is told
/// again. says whether the record changed.
pub(super) fn note_said(told: &mut Told, said: &[(String, bool)]) -> bool {
    let mut changed = false;
    for (folder, named) in said {
        changed |= told.note(said_as(*named), folder);
        changed |= told.retain(said_as(!named), |other| other != folder);
    }
    changed
}

/// forgets the lines said of folders no dataset of `manifest` has now, so a dataset that lands
/// there later is told of its marker again. says whether the record changed.
pub(super) fn forget_left(told: &mut Told, manifest: &Manifest) -> bool {
    let folders: HashSet<String> = manifest
        .datasets
        .keys()
        .map(|name| plan::folder(manifest, name))
        .collect();
    let mut changed = false;
    for said in [Once::Hidden, Once::Named] {
        changed |= told.retain(said, |folder| folders.contains(folder));
    }
    changed
}

/// the kind a marker's line is said once as.
fn said_as(named: bool) -> Once {
    if named { Once::Named } else { Once::Hidden }
}

/// the text `contents` gives the marker of `folder`, none when it gets no marker.
fn text_of<'c>(contents: &'c BTreeMap<String, Content>, folder: &str) -> Option<&'c [u8]> {
    match contents.get(folder)? {
        Content::Text(text) => Some(text),
        Content::Unnamed(_) | Content::Tracked => None,
    }
}

/// writes the marker of each folder `planned` names, with the text `contents` gives it: a new one,
/// which the state records already, never over anything, and fetchloom's own while it holds what
/// the state records, or the new text already, as a run cut short after writing it leaves it. one
/// the person changed is theirs, and its record goes, as does that of a new one that cannot be
/// made, so a later run tries again. fetchloom's own in a folder that gets none now is deleted.
/// whether the state changed, and the folders a person hears of, each with whether its marker
/// names the datasets' entries: those made, and those whose marker went from `*` to naming them or
/// back.
pub(super) fn write(
    root: &Path,
    placer: &mut Placer,
    state: &mut State,
    planned: &[(String, bool)],
    contents: &BTreeMap<String, Content>,
) -> (bool, Vec<(String, bool)>) {
    let star = digest::sha256(TEXT).hex();
    let mut changed = false;
    let mut told = Vec::new();
    for (folder, there) in planned {
        let before = state.markers.get(folder).cloned();
        let Some(text) = text_of(contents, folder) else {
            if let Some(wrote) = &before {
                delete(root, placer, folder, wrote);
            }
            changed |= forget(state, folder);
            continue;
        };
        let named = text != TEXT;
        match put(root, placer, state, folder, text, *there) {
            Ok(Put::Written) => {
                let was_named = before.as_ref().is_some_and(|wrote| *wrote != star);
                if !there || named != was_named {
                    told.push((folder.clone(), named));
                }
                note(state, folder, text);
            }
            Ok(Put::Held) => note(state, folder, text),
            Ok(Put::Theirs) => {
                forget(state, folder);
            }
            Err(err) => {
                tracing::debug!(%folder, %err, "could not write the folder's .gitignore");
                // a marker that could not be rewritten stays fetchloom's, as it was
                if !there {
                    forget(state, folder);
                }
            }
        }
        changed |= state.markers.get(folder) != before.as_ref();
    }
    (changed, told)
}

/// what [`put`] found at a marker's path.
enum Put {
    /// the new text took the marker's name.
    Written,
    /// fetchloom's marker held the new text already.
    Held,
    /// the person's file stands there, or changed fetchloom's.
    Theirs,
}

/// writes `text` under a hidden name beside the marker of `folder` and flushes it before it takes
/// the marker's name: over nothing when fetchloom's own does not stand `there`, so a file someone
/// made meanwhile stays, else over fetchloom's own while it holds what the state records.
fn put(
    root: &Path,
    placer: &mut Placer,
    state: &State,
    folder: &str,
    text: &[u8],
    there: bool,
) -> io::Result<Put> {
    let marker = native(root, &disk::marker(folder));
    if there {
        if holds(root, placer, folder, &digest::sha256(text).hex()) {
            return Ok(Put::Held);
        }
        let ours = state
            .markers
            .get(folder)
            .is_some_and(|wrote| holds(root, placer, folder, wrote));
        if !ours {
            return Ok(Put::Theirs);
        }
    } else {
        placer.check_folders(&marker, true)?;
    }
    let hidden = written_beside(placer, &marker, text)?;
    let named = if there {
        placer.rename_file(&hidden, &marker)
    } else {
        placer.move_aside_to(&hidden, &marker)
    };
    if named.is_err()
        && let Err(err) = placer.delete_file(&hidden)
    {
        tracing::debug!(%folder, %err, "could not delete an unnamed .gitignore");
    }
    named.map(|()| Put::Written)
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
        if !holds_placed(state, folder)
            && let Some(wrote) = state.markers.remove(folder)
        {
            delete(root, placer, folder, &wrote);
            changed = true;
        }
    }
    changed
}

/// whether every dataset folder of `scope` holding files fetchloom placed has its marker, an
/// entry of someone else's where the marker would go, or none as too full to name or as git's,
/// and no record waits to be forgotten, as a run that changes nothing leaves the markers.
pub(crate) fn settled(manifest: &Manifest, survey: &Survey, scope: &Scope) -> bool {
    let state = &survey.state;
    let placed: BTreeSet<String> = manifest
        .datasets
        .iter()
        .filter(|(_, entry)| entry.place || entry.is_output())
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
                survey.disk.contains_key(&path)
                    || survey.entries.contains_key(&path)
                    || too_full(manifest, state, &folder)
                    || kept_in_git(manifest, survey, &folder)
            }
        });
    marked
        && survey
            .marks
            .keys()
            .all(|folder| placed.contains(folder) || holds_placed(state, folder))
}

/// whether the files `state` records in `folder`, one the person shares, put more entries at its
/// top than a marker names, as [`contents`] counts them, so it gets none.
fn too_full(manifest: &Manifest, state: &State, folder: &str) -> bool {
    if !shared(manifest, folder) {
        return false;
    }
    let below = format!("{folder}/");
    let mut here: HashMap<&str, bool> = HashMap::new();
    let mut entries = HashSet::new();
    for (path, placed) in state
        .files
        .range(below.clone()..)
        .take_while(|(path, _)| path.starts_with(&below))
    {
        let theirs = *here
            .entry(placed.dataset.as_str())
            .or_insert_with(|| plan::folder(manifest, &placed.dataset) == folder);
        let inside = &path[below.len()..];
        if theirs
            && entries.insert(inside.split('/').next().unwrap_or(inside))
            && entries.len() > MOST_NAMED
        {
            return true;
        }
    }
    false
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
                .starts_with(&format!("{}/", gitignore_escaped(top)))
        })
    };
    if deliberate && text.lines().any(taken_back) {
        return None;
    }
    let mut rules = Vec::new();
    let mut at = Some(folder);
    while let Some(current) = at {
        let literal = gitignore_escaped(current);
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
    if !state.told.note(Once::Rule, &rule) {
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
    true
}

/// says once to a person at a terminal of the .gitignore line that keeps the data folder link at
/// `link`, its path in the project, out of git, when no line of the project's .gitignore hides the
/// link itself: git keeps a link as a file, which a line ending in `/`, as cookiecutter's `/data/`,
/// never hides, so a commit takes it and each clone then refuses it. fetchloom never edits
/// .gitignore. says whether the state changed.
pub(super) fn tell_link_line(output: &Output, root: &Path, link: &str, state: &mut State) -> bool {
    if !output.watched_story() || output.json() {
        return false;
    }
    let rule = format!("/{}", gitignore_escaped(link));
    if link_hidden(root, link) || !state.told.note(Once::Link, &rule) {
        return false;
    }
    output.warn_with(
        Warning::GitDataLinkUnignored,
        &format!(
            "git sees {link}, the link fetchloom follows to the data folder, so a commit takes it and each clone of this project then refuses it"
        ),
        &format!(
            "add the line {rule} to the project's .gitignore: a line ending in /, as {rule}/, hides folders alone, never a link. fetchloom never edits .gitignore"
        ),
    );
    true
}

/// whether a line of the project's .gitignore at `root` hides the link at `link` itself: its own
/// path, with or without a leading `/`, or a folder holding it, as [`hiding_line`] finds one.
fn link_hidden(root: &Path, link: &str) -> bool {
    let Ok(text) = crate::project::read_gitignore(&root.join(".gitignore")) else {
        return false;
    };
    let literal = gitignore_escaped(link);
    let own = [format!("/{literal}"), literal];
    text.lines()
        .any(|line| own.iter().any(|form| form == line.trim_end()))
        || link
            .rsplit_once('/')
            .is_some_and(|(folder, _)| hiding_line(root, folder, false).is_some())
}

/// warns of the dataset folders `placement` leaves with no marker of fetchloom's, whose files git
/// sees.
pub(super) fn warn_unmarked(output: &Output, manifest: &Manifest, placement: &Placement) {
    warn_unmarkable(output, &placement.unmarkable);
    warn_unnamed(output, manifest, &placement.unnamed);
}

/// warns of each of `folders`, as [`unnamed`] finds them, with how many entries its datasets put
/// there: git sees them, and hiding the whole folder is the person's choice, never fetchloom's.
fn warn_unnamed(output: &Output, manifest: &Manifest, folders: &[(String, usize)]) {
    for (folder, entries) in folders {
        let datasets: Vec<&str> = manifest
            .datasets
            .iter()
            .filter(|(name, entry)| entry.place && plan::folder(manifest, name) == *folder)
            .map(|(name, _)| name.as_str())
            .collect();
        let own = match datasets.as_slice() {
            [] => continue,
            _ if *folder != manifest.data_dir => {
                format!("keep the folders of other datasets out of {folder}")
            }
            [one] => format!("give {one} a folder of its own, as path = \"{one}\" in data.toml"),
            [first, ..] => format!(
                "give each a folder of its own, as path = \"{first}\" for {first} in data.toml"
            ),
        };
        output.warn_with(
            Warning::GitDatasetUnignored,
            &format!(
                "git sees the {} entries {} placed in {folder}/, more than a .gitignore names one by one, and `*` would hide your own files there too",
                crate::units::grouped(*entries as u64),
                crate::units::listed(&datasets, "and")
            ),
            &format!(
                "to keep them out of git, {own}, where fetchloom's .gitignore needs no list; or add the line /{}/ to the project's .gitignore, which hides your own files in {folder}/ too",
                gitignore_escaped(folder)
            ),
        );
    }
}

/// warns of each of `folders`, as [`unmarkable`] finds them: git sees the dataset's files.
fn warn_unmarkable(output: &Output, folders: &BTreeSet<String>) {
    for folder in folders {
        output.warn_with(
            Warning::GitDatasetUnignored,
            &format!(
                "{folder} holds a .gitignore of the dataset's own, so fetchloom cannot keep {folder} out of git"
            ),
            &format!(
                "to keep it out, add the line /{}/ to .gitignore",
                gitignore_escaped(folder)
            ),
        );
    }
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

    use super::{MOST_NAMED, TEXT, hiding_line, holds, names, settled};
    use crate::cli::sync::Scope;
    use crate::cli::sync::disk::{Folders, Survey};
    use crate::digest;
    use crate::place::{Method, Placer};
    use crate::project::Project;
    use crate::project::manifest::Manifest;
    use crate::project::state::{Look, Once, Placed, State};

    /// a folder the person shares, filled past what a marker names, settles with no marker of
    /// fetchloom's, so a sync with nothing to do stays quick; one at the limit needs its marker.
    #[test]
    fn a_shared_folder_too_full_to_name_settles_with_no_marker() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("data")).unwrap();
        let project = Project::at(dir.path().to_path_buf());
        let manifest = Manifest::parse("[datasets.many]\nref = \"x\"\npath = \".\"\n").unwrap();
        for (count, quiet) in [(MOST_NAMED, false), (MOST_NAMED + 1, true)] {
            let mut survey = Survey::read(
                &project,
                &Folders::data("data"),
                crate::threads::Threads::default(),
                &crate::stop::Stop::default(),
            )
            .unwrap();
            for at in 0..count {
                survey
                    .state
                    .files
                    .insert(format!("data/f{at:04}.csv"), placed("many"));
            }
            assert_eq!(settled(&manifest, &survey, &Scope::All), quiet, "{count}");
        }
    }

    /// a file fetchloom placed for `dataset`.
    fn placed(dataset: &str) -> Placed {
        Placed {
            dataset: dataset.to_owned(),
            sha256: digest::sha256(b"1\n"),
            method: Method::Copy,
            look: Look {
                size: 2,
                modified: 1,
            },
            writable: false,
            checked: 0,
            adopted: false,
            output: false,
            unlinkable: false,
            racy_intact: std::sync::OnceLock::new(),
        }
    }

    /// an output's folder git keeps settles with no marker, so a sync with nothing to do stays
    /// quick there, while the same folder needs its marker without the record, or as a dataset's
    /// that is no output.
    #[test]
    fn an_output_git_keeps_settles_with_no_marker() {
        let dir = tempfile::tempdir().unwrap();
        let project = Project::at(dir.path().to_path_buf());
        std::fs::create_dir_all(project.root.join("data/iris")).unwrap();
        let output = "[datasets.iris]\nref = \"local\"\nat = \"data/iris\"\n";
        let output = Manifest::parse(output).unwrap();
        let dataset = Manifest::parse("[datasets.iris]\nref = \"x\"\n").unwrap();
        let mut survey = Survey::read(
            &project,
            &Folders::of(&output),
            crate::threads::Threads::default(),
            &crate::stop::Stop::default(),
        )
        .unwrap();
        let file = "data/iris/iris.csv".to_owned();
        survey.state.files.insert(file, placed("iris"));
        assert!(!settled(&output, &survey, &Scope::All));
        survey.state.told.note(Once::Tracked, "data/iris");
        assert!(settled(&output, &survey, &Scope::All));
        assert!(!settled(&dataset, &survey, &Scope::All));
    }

    /// a record names fetchloom's marker only while the file holds what the record says, in the
    /// form fetchloom writes, so a record that came with a copied state takes no other .gitignore.
    #[test]
    fn a_record_takes_only_a_gitignore_in_fetchlooms_form() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        std::fs::create_dir_all(data.join("iris")).unwrap();
        let placer = Placer::new(&data);
        let named = names(&BTreeSet::from([("iris.csv", false)]));
        for (text, ours) in [
            (TEXT, true),
            (named.as_slice(), true),
            (b"*.tmp\n".as_slice(), false),
        ] {
            std::fs::write(data.join("iris/.gitignore"), text).unwrap();
            let wrote = digest::sha256(text).hex();
            assert_eq!(
                holds(root.path(), &placer, "data/iris", &wrote),
                ours,
                "{}",
                String::from_utf8_lossy(text)
            );
        }
    }

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

    /// a dropped output's folder still holds the files data.lock pins, so its marker stays and
    /// leaves the project settled, as a run that changes nothing needs to take its quick way.
    #[test]
    fn a_dropped_outputs_marker_leaves_the_project_settled() {
        let dir = tempfile::tempdir().unwrap();
        let project = Project::at(dir.path().to_path_buf());
        std::fs::create_dir_all(project.root.join("results")).unwrap();
        std::fs::write(project.root.join("results/.gitignore"), TEXT).unwrap();
        let mut state = State::default();
        let wrote = digest::sha256(TEXT).hex();
        state.markers.insert("results".to_owned(), wrote);
        project.write_state(&state).unwrap();
        let text = "[datasets.results]\nref = \"local\"\nat = \"results\"\n";
        let mut manifest = Manifest::parse(text).unwrap();
        manifest.datasets.get_mut("results").unwrap().place = false;
        let survey = Survey::read(
            &project,
            &Folders::of(&manifest),
            crate::threads::Threads::default(),
            &crate::stop::Stop::default(),
        )
        .unwrap();
        assert!(survey.marks.contains_key("results"));
        assert!(settled(&manifest, &survey, &Scope::All));
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
