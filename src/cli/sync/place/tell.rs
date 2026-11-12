//! the lines a placement prints: what each dataset cost, what was removed, and every file left
//! alone or put back, with the reason.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use super::put::Tally;
use super::{
    Blocker, ChangedCopy, Entry, LeftBehind, Placement, Preview, Reason, Scope, Undroppable,
};
use crate::output::Output;
use crate::place::{Because, Placer, Unlinked};
use crate::plan;
use crate::project::manifest::Manifest;
use crate::units;

pub(super) fn tell(
    output: &Output,
    manifest: &Manifest,
    placer: &Placer,
    tallies: &BTreeMap<&str, Tally>,
    placement: &Placement,
    told: Option<&str>,
    scope: &Scope,
) {
    for (path, found) in &placement.cleared {
        output.step(
            "Removed",
            &format!("{path} · {} that was in the way", entry_name(*found)),
        );
    }
    for overwritten in &placement.overwritten {
        output.step(
            "Replaced",
            &format!(
                "{} · discarding {}",
                overwritten.path,
                overwritten_what(overwritten.reason)
            ),
        );
    }
    // datasets that share a folder share its line, where the first of them comes
    let mut folders: Vec<(String, Tally)> = Vec::new();
    for (dataset, tally) in tallies
        .iter()
        .filter(|(dataset, _)| told != Some(**dataset))
    {
        let folder = plan::folder(manifest, dataset);
        let at = folders
            .iter()
            .position(|(seen, _)| *seen == folder)
            .unwrap_or_else(|| {
                folders.push((folder, Tally::default()));
                folders.len() - 1
            });
        let sum = &mut folders[at].1;
        sum.files += tally.files;
        sum.links += tally.links;
        sum.copied += tally.copied;
    }
    for (folder, tally) in &folders {
        let how = match (tally.links, tally.files - tally.links) {
            (_, 0) => "linked to the cache".to_owned(),
            (0, _) => format!("copied, {}", units::size(tally.copied)),
            (_, _) => format!("partly copied, {}", units::size(tally.copied)),
        };
        let fresh = if placement.refilled.contains(folder) {
            " · the folder was missing"
        } else {
            ""
        };
        output.step(
            "Placed",
            &format!(
                "{folder} · {} {how}{fresh}",
                units::count(tally.files, "file", "files")
            ),
        );
    }
    for (folder, paths) in by_folder(manifest, &placement.adopted) {
        let (linked, kept): (Vec<&String>, Vec<&String>) = paths
            .into_iter()
            .partition(|path| placement.relinked.contains(*path));
        tell_adopted(output, &folder, &linked, true);
        tell_adopted(output, &folder, &kept, false);
    }
    tell_removed(output, manifest, ["Retired", "Removed"], placement);
    warn_adopted_removed(output, &placement.removed_adopted, "removed");
    if tallies.values().any(|tally| tally.unlinked > 0)
        && let Some(why) = placer.unlinked()
    {
        let because = copy_warning(why, &manifest.data_dir);
        match &because.help {
            Some(help) => output.warn_with(&because.said, help),
            None => output.warn(&because.said),
        }
    }
    warn_kept(output, manifest, scope, placement);
    warn_retired(output, placement);
    warn_put_back(output, manifest, placement, "put back");
    warn_unreached(output, &placement.unreached);
    for path in &placement.restored {
        if placement.repaired.contains(path) {
            output.warn(&format!(
                "{path} was put back as pinned: its bytes had changed though its size and time had not"
            ));
        } else {
            let said = match changed_copy(placement, path) {
                Some(copy) => format!(
                    "{path} was put back as pinned: it is linked to the cache's copy, which was changed. the changed copy is kept as {}",
                    copy.kept_as
                ),
                None => format!(
                    "{path} was put back as pinned: it is linked to the cache's copy, which was changed"
                ),
            };
            output.warn_with(
                &said,
                "to edit a dataset's files, set copy = true for it in data.toml",
            );
        }
    }
}

/// where the cache's changed copy linked at `path` was kept, if sync kept it.
fn changed_copy<'p>(placement: &'p Placement, path: &str) -> Option<&'p ChangedCopy> {
    placement
        .changed_copies
        .iter()
        .find(|copy| copy.path == path)
}

/// a warning for each dataset exclude that names files inside an archive, which only the
/// archive's members table reaches, with the drop that leaves them out. `-v` names each file.
fn warn_unreached(output: &Output, unreached: &[plan::Unreached]) {
    for found in unreached {
        let (said, help) = unreached_said(found);
        output.warn_with(&said, &help);
        for path in &found.paths {
            output.detail(path);
        }
    }
}

/// what a dataset exclude that names files inside an archive leaves undone, and the drop that
/// does it.
fn unreached_said(found: &plan::Unreached) -> (String, String) {
    let inside = match found.archives.as_slice() {
        [one] => one
            .rsplit_once('/')
            .map_or(one.as_str(), |(_, name)| name)
            .to_owned(),
        many => format!("{} archives", many.len()),
    };
    let first = found.paths.first().map_or("", String::as_str);
    let command = crate::cli::project::command(&format!("drop {}", crate::cli::shell_word(first)));
    let (what, help) = match found.paths.len() {
        1 => ("a file".to_owned(), format!("run `{command}`")),
        count => (
            format!("{count} files"),
            format!("run `fetchloom drop` with their paths, as `{command}`"),
        ),
    };
    (
        format!(
            "exclude \"{}\" in datasets.{} names {what} inside {inside}, which exclude does not reach",
            found.pattern, found.dataset
        ),
        help,
    )
}

/// the most files a warning about files put back names; `-v` names each one.
const NAMED: usize = 5;

/// a warning for the files fetchloom placed that someone deleted or moved, which a sync puts
/// back, as `verb` says, since data.lock still pins them: how to leave each kind of them out for
/// good, at most [`NAMED`] ways, where a person's own files stay out of the way, and, when no
/// file of the data folder was left, how to keep that folder elsewhere. a folder deleted whole
/// is named once, and a dataset's own folder deleted whole is a fresh start, which needs no
/// warning.
fn warn_put_back(output: &Output, manifest: &Manifest, placement: &Placement, verb: &str) {
    let fresh = |path: &String| {
        placement
            .emptied
            .get(path)
            .is_some_and(|folder| placement.refilled.contains(folder))
    };
    let paths: Vec<&String> = placement
        .put_back
        .iter()
        .filter(|path| !fresh(path))
        .collect();
    if paths.is_empty() {
        return;
    }
    // each folder deleted whole, with its files, or a file alone
    let mut items: Vec<(&str, u64, &String)> = Vec::new();
    let mut folders: BTreeMap<&str, usize> = BTreeMap::new();
    for path in &paths {
        match placement.emptied.get(*path) {
            Some(folder) => {
                let at = *folders.entry(folder).or_insert_with(|| {
                    items.push((folder, 0, path));
                    items.len() - 1
                });
                items[at].1 += 1;
            }
            None => items.push((path, 1, path)),
        }
    }
    let said = put_back_said(verb, &paths, &items, &folders);
    let mut leave = Vec::new();
    if let Some((name, _, _)) = items
        .iter()
        .find(|(_, _, path)| !placement.undroppable.contains_key(*path))
    {
        let what = if folders.contains_key(name) {
            "folder"
        } else {
            "file"
        };
        leave.push(format!(
            "to leave a {what} out for good, run `{}`",
            crate::cli::project::command(&format!("drop {}", crate::cli::shell_word(name)))
        ));
    }
    let mut told = BTreeSet::new();
    for (path, why) in paths
        .iter()
        .filter_map(|path| Some((*path, placement.undroppable.get(*path)?)))
    {
        if leave.len() == NAMED {
            break;
        }
        match why {
            Undroppable::Made(recipe) if told.insert(recipe) => leave.push(format!(
                "{recipe} makes {path}, so to leave it out for good, choose the files inside {recipe}'s step"
            )),
            Undroppable::Only(dataset) if told.insert(dataset) => leave.push(format!(
                "{path} is the only file of {dataset}, so to leave it out for good, run `{}`",
                crate::cli::project::command(&format!("remove {dataset}"))
            )),
            _ => {}
        }
    }
    let data = &manifest.data_dir;
    leave.push(format!(
        "keep files of your own in a folder outside {data}/"
    ));
    // nothing of the data folder was left, as when the folder itself was moved
    if paths.len() > 1 && placement.unchanged == 0 && paths.len() as u64 == placement.placed {
        leave.push(format!(
            "to keep the data folder somewhere else than {data}/, set data_dir in data.toml"
        ));
    }
    output.warn_with(&said, &leave.join("; "));
    for path in paths {
        output.detail(path);
    }
}

/// what a warning about `paths`, put back as `verb` says, names: the one file, the one folder
/// deleted whole, or the first [`NAMED`] of `items`, each folder of `folders` with how many files
/// it held, and how many files the rest hold.
fn put_back_said(
    verb: &str,
    paths: &[&String],
    items: &[(&str, u64, &String)],
    folders: &BTreeMap<&str, usize>,
) -> String {
    let shown = |(name, files, _): &(&str, u64, &String)| {
        if folders.contains_key(name) {
            format!("the {} in {name}/", units::grouped(*files))
        } else {
            (*name).to_owned()
        }
    };
    match (paths.len(), items) {
        (1, _) => format!(
            "{verb} {}, which was deleted or moved, since data.lock pins it",
            paths[0]
        ),
        (count, [(folder, ..)]) => format!(
            "{verb} the {} files in {folder}/, which was deleted or moved, since data.lock pins them",
            units::grouped(count as u64)
        ),
        (count, _) => {
            let named: Vec<String> = items.iter().take(NAMED).map(shown).collect();
            let more: u64 = items.iter().skip(NAMED).map(|(_, files, _)| files).sum();
            let more = match more {
                0 => String::new(),
                more => format!(" and {} more", units::grouped(more)),
            };
            format!(
                "{verb} {} that were deleted or moved, since data.lock pins them: {}{more}",
                units::count(count as u64, "file", "files"),
                named.join(", ")
            )
        }
    }
}

/// every folder above a file in `files`, by its path in the project, so a folder outside the set
/// holds no file.
pub(crate) fn holding<'f>(files: impl Iterator<Item = &'f String>) -> HashSet<&'f str> {
    let mut folders = HashSet::new();
    for file in files {
        let mut path = file.as_str();
        while let Some((parent, _)) = path.rsplit_once('/') {
            if !folders.insert(parent) {
                break;
            }
            path = parent;
        }
    }
    folders
}

/// the topmost folder above `path`, no higher than `floor`, that holds no file, as `holding`
/// says: the folder a person deleted whole, or emptied, or none while the file's own folder still
/// holds one.
pub(crate) fn emptied<'p>(holding: &HashSet<&str>, path: &'p str, floor: &str) -> Option<&'p str> {
    let inside = path.strip_prefix(floor)?.strip_prefix('/')?;
    if !holding.contains(floor) {
        return Some(&path[..floor.len()]);
    }
    let mut end = floor.len();
    for part in inside.split('/').collect::<Vec<_>>().split_last()?.1 {
        end += 1 + part.len();
        if !holding.contains(&path[..end]) {
            return Some(&path[..end]);
        }
    }
    None
}

/// the paths of `by_dataset` gathered by the folder of their dataset, in the order the folders
/// first come, since datasets that share a folder share its line.
fn by_folder<'p>(
    manifest: &Manifest,
    by_dataset: impl IntoIterator<Item = (&'p String, &'p Vec<String>)>,
) -> Vec<(String, Vec<&'p String>)> {
    let mut folders: Vec<(String, Vec<&String>)> = Vec::new();
    for (dataset, paths) in by_dataset {
        let folder = plan::folder(manifest, dataset);
        let at = folders
            .iter()
            .position(|(seen, _)| *seen == folder)
            .unwrap_or_else(|| {
                folders.push((folder, Vec::new()));
                folders.len() - 1
            });
        folders[at].1.extend(paths);
    }
    for (_, paths) in &mut folders {
        paths.sort();
    }
    folders
}

/// the files in `folder` that were already at their places with the pinned bytes, which sync
/// took as placed: `linked` to the cache's copy now, or kept as they were. `-v` names each file.
fn tell_adopted(output: &Output, folder: &str, paths: &[&String], linked: bool) {
    let said = match (paths.len(), linked) {
        (0, _) => return,
        (1, true) => {
            "1 file already held the pinned bytes, so it is now linked to the cache".to_owned()
        }
        (1, false) => "1 file already held the pinned bytes, so it stays as it is".to_owned(),
        (count, true) => format!(
            "{count} files already held the pinned bytes, so they are now linked to the cache"
        ),
        (count, false) => {
            format!("{count} files already held the pinned bytes, so they stay as they are")
        }
    };
    output.step("Adopted", &format!("{folder} · {said}"));
    for path in paths {
        output.detail(path);
    }
}

/// a warning for the files a removal deletes, or `verb` says it would, that were already there
/// when sync adopted them, since someone other than fetchloom put them there.
fn warn_adopted_removed(output: &Output, paths: &[String], verb: &str) {
    match paths {
        [] => {}
        [path] => output.warn(&format!(
            "{verb} {path}, a file that was already there when fetchloom adopted it"
        )),
        [first, ..] => output.warn(&format!(
            "{verb} {} files that were already there when fetchloom adopted them, such as {first}",
            paths.len()
        )),
    }
}

/// why a sync copied files it would rather have linked. a data folder stays inside its project,
/// so when the cache is on another drive, it is the project that moves.
pub(super) fn copy_warning(why: Unlinked, data_dir: &str) -> Because {
    match why {
        Unlinked::OtherDrive => why.copied_because("the project", "files were"),
        Unlinked::Synced(_) | Unlinked::NoLinks => why.copied_because(data_dir, "files were"),
    }
}

/// the lines a dry run of `scope` prints for a placement it only planned: what each dataset
/// would gain or have replaced, what would be removed, and every file that would be left alone.
/// `-v` names each file.
pub(crate) fn tell_preview(output: &Output, manifest: &Manifest, scope: &Scope, preview: &Preview) {
    // sizes stay out: a file whose pin only a download can learn has no size yet
    let tell_files = |verb: &str, dataset: &str, files: &[String]| {
        if files.is_empty() {
            return;
        }
        output.step(
            verb,
            &format!(
                "{} · {}",
                plan::folder(manifest, dataset),
                units::count(files.len() as u64, "file", "files"),
            ),
        );
        for path in files {
            output.detail(path);
        }
    };
    for (dataset, planned) in &preview.by_dataset {
        tell_files("Would place", dataset, &planned.place);
    }
    for (dataset, planned) in &preview.by_dataset {
        tell_files("Would renew", dataset, &planned.replace);
    }
    for (dataset, planned) in &preview.by_dataset {
        tell_checks(output, manifest, dataset, &planned.check);
    }
    let adopting = preview
        .by_dataset
        .iter()
        .map(|(dataset, planned)| (dataset, &planned.adopt));
    for (folder, files) in by_folder(manifest, adopting) {
        tell_adoptions(output, &folder, &files);
    }
    for overwritten in &preview.placement.overwritten {
        output.warn(&format!(
            "{} is {}, and --force would replace it",
            overwritten.path,
            overwritten_what(overwritten.reason)
        ));
    }
    let placement = &preview.placement;
    tell_removed(
        output,
        manifest,
        ["Would retire", "Would remove"],
        placement,
    );
    for path in &preview.changes.remove {
        output.detail(path);
    }
    warn_adopted_removed(output, &placement.removed_adopted, "would remove");
    left_behind(output, manifest, placement.left_behind.as_ref());
    warn_kept(output, manifest, scope, placement);
    warn_retired(output, placement);
    warn_put_back(output, manifest, placement, "a sync would put back");
    warn_unreached(output, &placement.unreached);
    for path in &placement.restored {
        if placement.repaired.contains(path) {
            output.warn(&format!(
                "{path} would be put back as pinned: its bytes changed though its size and time did not"
            ));
        } else {
            output.warn(&match changed_copy(placement, path) {
                Some(copy) => format!(
                    "{path} would be put back as pinned: it is linked to the cache's copy, which was changed. the changed copy would be kept as {}",
                    copy.kept_as
                ),
                None => format!(
                    "{path} would be put back as pinned: it is linked to the cache's copy, which was changed"
                ),
            });
        }
    }
}

/// the lines for the files a placement removed, or would remove: under the first of `verbs`, one
/// for each dataset `place = false` keeps out of the data folder now, then under the second one
/// for the files no dataset needs.
fn tell_removed(output: &Output, manifest: &Manifest, verbs: [&str; 2], placement: &Placement) {
    for (dataset, files) in &placement.unplaced {
        output.step(
            verbs[0],
            &format!(
                "{} · {}, since {dataset} has place = false",
                plan::folder(manifest, dataset),
                units::count(*files, "file", "files")
            ),
        );
    }
    let unneeded = placement
        .removed
        .saturating_sub(placement.unplaced.values().sum());
    if unneeded > 0 {
        output.step(
            verbs[1],
            &units::count(unneeded, "file no dataset needs", "files no dataset needs"),
        );
    }
}

/// the files of `dataset` already where a file goes whose pin only its download can learn, so
/// the dry run cannot yet tell whether they hold the pinned bytes. `-v` names each file.
fn tell_checks(output: &Output, manifest: &Manifest, dataset: &str, files: &[String]) {
    let said = match files.len() {
        0 => return,
        1 => "1 file is already there; the download tells whether it holds the pinned bytes"
            .to_owned(),
        count => format!(
            "{count} files are already there; the downloads tell whether they hold the pinned bytes"
        ),
    };
    output.step(
        "Would check",
        &format!("{} · {said}", plan::folder(manifest, dataset)),
    );
    for path in files {
        output.detail(path);
    }
}

/// the files in `folder` fetchloom did not place that already hold the pinned bytes, which a
/// sync would take as placed. `-v` names each file.
fn tell_adoptions(output: &Output, folder: &str, files: &[&String]) {
    let said = match files.len() {
        0 => return,
        1 => "1 file already holds the pinned bytes".to_owned(),
        count => format!("{count} files already hold the pinned bytes"),
    };
    output.step("Would adopt", &format!("{folder} · {said}"));
    for path in files {
        output.detail(path);
    }
}

/// a warning for the files fetchloom placed before `data_dir` changed, which stay where they are:
/// a sync deletes nothing outside the data folder. what else their folder holds is counted and
/// named, so the help to delete the folder never costs a person files of their own.
pub(super) fn left_behind(output: &Output, manifest: &Manifest, left: Option<&LeftBehind>) {
    let Some(LeftBehind {
        folder,
        files,
        others,
    }) = left
    else {
        return;
    };
    let Some(first) = files.first() else {
        return;
    };
    let before = format!("before data_dir changed to {}", manifest.data_dir);
    if let (Some(folder), [one, ..]) = (folder, others.as_slice()) {
        let placed = match files.len() {
            1 => format!("{first}, a file fetchloom placed {before}"),
            count => format!("{count} files fetchloom placed {before}, such as {first}"),
        };
        let (theirs, help) = match others.len() {
            1 => (
                format!("1 file it did not place: {one}"),
                format!("move {one} out of {folder} before you delete {folder}"),
            ),
            count => (
                format!("{count} files it did not place: {}", some_of(others)),
                format!(
                    "move the {count} files fetchloom did not place out of {folder} before you delete {folder}"
                ),
            ),
        };
        output.warn_with(
            &format!(
                "{folder} still holds {placed}, and {theirs}. fetchloom leaves them all there"
            ),
            &help,
        );
        for path in files {
            output.detail(path);
        }
        return;
    }
    let (said, help) = match (folder, files.len()) {
        (Some(folder), 1) => (
            format!(
                "{folder} still holds {first}, a file fetchloom placed {before}. fetchloom leaves it there"
            ),
            format!("delete {folder} when you no longer need what it holds"),
        ),
        (Some(folder), count) => (
            format!(
                "{folder} still holds {count} files fetchloom placed {before}, such as {first}. fetchloom leaves them there"
            ),
            format!("delete {folder} when you no longer need what it holds"),
        ),
        (None, 1) => (
            format!(
                "{first}, a file fetchloom placed {before}, is outside the data folder now. fetchloom leaves it there"
            ),
            "delete it when you no longer need it".to_owned(),
        ),
        (None, count) => (
            format!(
                "{count} files fetchloom placed {before} are outside the data folder now, such as {first}. fetchloom leaves them there"
            ),
            "delete them when you no longer need them".to_owned(),
        ),
    };
    output.warn_with(&said, &help);
    for path in files {
        output.detail(path);
    }
}

/// the first three of `paths`, and how many more there are: `a, b, c and 2 more`.
fn some_of(paths: &[String]) -> String {
    match paths {
        [first @ .., last] if paths.len() <= 3 && !first.is_empty() => {
            format!("{} and {last}", first.join(", "))
        }
        [a, b, c, rest @ ..] if !rest.is_empty() => {
            format!("{a}, {b}, {c} and {} more", rest.len())
        }
        _ => paths.join(""),
    }
}

/// a warning for each folder no dataset uses now that still holds files fetchloom did not place,
/// such as work of yours or a changed copy sync kept, which stay with the folder. `-v` names each
/// file.
fn warn_retired(output: &Output, placement: &Placement) {
    for retired in &placement.retired {
        let folder = &retired.folder;
        let (said, help) = match retired.unplaced.as_slice() {
            [] => continue,
            [only] => (
                format!(
                    "{folder} still holds {only}, a file fetchloom did not place, so the folder stays"
                ),
                format!(
                    "`{}` lists it as extra; delete it when you no longer need it",
                    crate::cli::project::command("status")
                ),
            ),
            [first, ..] => (
                format!(
                    "{folder} still holds {} files fetchloom did not place, such as {first}, so the folder stays",
                    retired.unplaced.len()
                ),
                format!(
                    "`{}` lists them as extra; delete them when you no longer need them",
                    crate::cli::project::command("status")
                ),
            ),
        };
        output.warn_with(&said, &help);
        for path in &retired.unplaced {
            output.detail(path);
        }
    }
}

/// a warning for each file a sync of `scope` left alone, with the command that replaces it.
fn warn_kept(output: &Output, manifest: &Manifest, scope: &Scope, placement: &Placement) {
    // one warning for each entry in the way, however many files it keeps out
    let mut blocking: BTreeMap<&str, (&Blocker, u64, BTreeSet<&str>)> = BTreeMap::new();
    for kept in &placement.kept {
        if let Some(blocker) = &kept.blocker {
            let entry = blocking
                .entry(&blocker.path)
                .or_insert((blocker, 0, BTreeSet::new()));
            entry.1 += 1;
            entry.2.extend(kept.dataset.as_deref());
        }
    }
    for (blocker, files, datasets) in blocking.into_values() {
        let force = forcing(manifest, scope, &datasets);
        let (said, help) = in_the_way(blocker, files, &force);
        match &blocker.dataset {
            Some(dataset) => output.warn_with(&said, &left_alone(manifest, dataset)),
            None => output.warn_with(&said, &help),
        }
    }
    for kept in &placement.kept {
        let force = forcing(
            manifest,
            scope,
            &kept.dataset.iter().map(String::as_str).collect(),
        );
        let (message, help) = match kept.reason {
            Reason::Blocked => continue,
            Reason::Changed => match kept.dataset.as_deref() {
                Some(dataset) if !readers(manifest, dataset).is_empty() => {
                    not_read(manifest, &kept.path, dataset)
                }
                Some(dataset) if kept.earlier => (
                    format!(
                        "{} has changed, so it stays, but it is an edit of an earlier version of {dataset}, not what data.lock pins now",
                        kept.path
                    ),
                    format!("`{force}` replaces it"),
                ),
                _ => (
                    format!("{} has changed, so it stays", kept.path),
                    format!("`{force}` replaces it"),
                ),
            },
            Reason::Shared => (
                format!(
                    "{} was edited, and the file is shared with other projects, so they see the edit too. it stays",
                    kept.path
                ),
                format!("`{force}` puts the original back"),
            ),
            Reason::Foreign => (
                format!(
                    "{} is not a file fetchloom placed, and it differs from data.lock, so it stays",
                    kept.path
                ),
                format!("`{force}` replaces it"),
            ),
            Reason::Unneeded => (
                format!(
                    "{} is no longer needed, but it has changed, so it stays",
                    kept.path
                ),
                "delete it if you do not need it".to_owned(),
            ),
        };
        output.warn_with(&message, &help);
    }
}

/// the recipes that read `dataset`.
fn readers<'m>(manifest: &'m Manifest, dataset: &str) -> Vec<&'m str> {
    manifest
        .datasets
        .iter()
        .filter(|(_, entry)| {
            entry
                .recipe()
                .is_some_and(|recipe| recipe.from.iter().any(|from| from == dataset))
        })
        .map(|(name, _)| name.as_str())
        .collect()
}

/// the warning for an edit at `path` of `dataset`, which recipes read: a step reads the bytes
/// data.lock pins, so the edit reaches no result, and replacing it would undo the fix.
fn not_read(manifest: &Manifest, path: &str, dataset: &str) -> (String, String) {
    let recipes = readers(manifest, dataset);
    let (them, either, verb) = match recipes.split_last() {
        Some((last, [])) => ((*last).to_owned(), (*last).to_owned(), "reads"),
        Some((last, rest)) => (
            format!("{} and {last}", rest.join(", ")),
            format!("{} or {last}", rest.join(", ")),
            "read",
        ),
        None => (String::new(), String::new(), "reads"),
    };
    (
        format!(
            "{path} has changed, so it stays, but {them} {verb} the bytes data.lock pins for {dataset}, not this edit"
        ),
        format!(
            "to build from the fix, make it in a step of {either}, or copy the file out of {}/ and add it as a dataset of its own with `{}` for {either} to read",
            manifest.data_dir,
            crate::cli::project::command("add <copy> --as <name>")
        ),
    )
}

/// the command that replaces what a sync of `scope` kept of `datasets`: after a sync of named
/// datasets, or a build, with the names of those datasets alone, so it replaces nothing in the
/// others, or with every name it placed when it does not know whose the files are. it names the
/// project as the run's `-C` did.
fn forcing(manifest: &Manifest, scope: &Scope, datasets: &BTreeSet<&str>) -> String {
    let Scope::Only(names) = scope else {
        return crate::cli::project::command("sync --force");
    };
    let placed = |name: &&str| {
        manifest
            .datasets
            .get(*name)
            .is_some_and(|entry| entry.place)
    };
    let mut chosen: Vec<&str> = names
        .iter()
        .map(String::as_str)
        .filter(|name| datasets.contains(name))
        .collect();
    if chosen.is_empty() {
        chosen = names.iter().map(String::as_str).filter(placed).collect();
    }
    crate::cli::project::command(&format!("sync --force {}", chosen.join(" ")))
}

/// what clears an entry in the way that is a file of `dataset`, or a folder of its files, which
/// a sync of other datasets leaves alone.
fn left_alone(manifest: &Manifest, dataset: &str) -> String {
    if manifest.datasets.contains_key(dataset) {
        format!(
            "it belongs to {dataset}: run `{}` first, or `{}` for every dataset",
            crate::cli::project::command(&format!("sync {dataset}")),
            crate::cli::project::command("sync")
        )
    } else {
        format!(
            "it belongs to {dataset}, which data.toml no longer has: `{}` syncs every dataset and removes it",
            crate::cli::project::command("sync")
        )
    }
}

/// what `--force` replaced in a file sync would otherwise keep for `reason`.
fn overwritten_what(reason: Reason) -> &'static str {
    match reason {
        Reason::Shared => "an edit through a link other projects share",
        Reason::Foreign => "a file fetchloom did not place",
        Reason::Changed | Reason::Unneeded | Reason::Blocked => "your edit",
    }
}

/// what an entry in the way of `files` files is, and what replaces it: `force`, the command that
/// replaces what a sync kept.
fn in_the_way(blocker: &Blocker, files: u64, force: &str) -> (String, String) {
    let path = &blocker.path;
    let what = entry_name(blocker.found);
    let force = if blocker.found == Entry::Link {
        format!("`{force}` replaces the link itself, never what it points to")
    } else {
        format!("`{force}` replaces it")
    };
    if blocker.folder {
        let pinned = if files == 1 {
            "the file data.lock pins under it is not placed"
        } else {
            "the files data.lock pins under it are not placed"
        };
        return (
            format!("{path} is {what} where data.lock needs a folder, so {pinned}"),
            force,
        );
    }
    if blocker.clearable {
        (
            format!("{path} is {what} where data.lock places a file, so it stays"),
            force,
        )
    } else {
        (
            format!(
                "{path} is a folder with files in it where data.lock places a file, so it stays, even with --force"
            ),
            "move what it holds elsewhere, then sync again".to_owned(),
        )
    }
}

fn entry_name(found: Entry) -> &'static str {
    match found {
        Entry::File => "a file",
        Entry::Folder => "a folder",
        Entry::Link => "a link",
        Entry::Special => "a special file",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::{Level, Style};

    /// the folder a file was deleted with is the topmost that holds no file, never above the
    /// floor, and a file whose own folder still holds one was deleted alone.
    #[test]
    fn a_file_belongs_to_the_topmost_folder_left_without_files() {
        let files = [
            "data/set/b/kept.csv".to_owned(),
            "data/notes.txt".to_owned(),
        ];
        let holding = holding(files.iter());
        for (path, floor, folder) in [
            ("data/set/a/x/y.csv", "data/set", Some("data/set/a")),
            ("data/set/b/gone.csv", "data/set", None),
            ("data/set/top.csv", "data/set", None),
            ("data/other/a/y.csv", "data/other", Some("data/other")),
            ("data/other/a/y.csv", "data/elsewhere", None),
        ] {
            assert_eq!(emptied(&holding, path, floor), folder, "{path}");
        }
    }

    /// a sync that puts back many files names the first five and counts the rest, with their
    /// thousands apart, and points at drop with the first drop leaves out, quoted for a shell.
    /// for the files drop leaves out none of, the help says what does for each kind: the step of
    /// the recipe that makes one, or removing the dataset one is the only file of. when no file
    /// of the data folder was left, it names `data_dir` too.
    #[test]
    fn files_put_back_are_counted_and_the_first_five_named() {
        let output = Output::new(
            Style {
                color: false,
                unicode: false,
                interactive: false,
                watched: false,
                links: false,
            },
            Level::Quiet,
            false,
            false,
        );
        let manifest = Manifest::parse("[datasets.pets]\nref = \"x\"\n").unwrap();
        let put_back = |paths: &[&str], undroppable: Vec<(&str, Undroppable)>| Placement {
            put_back: paths.iter().map(|path| (*path).to_owned()).collect(),
            undroppable: undroppable
                .into_iter()
                .map(|(path, why)| (path.to_owned(), why))
                .collect(),
            ..Placement::default()
        };
        let paths: Vec<String> = (1..=7)
            .map(|n| format!("data/pets/my images/{n}.png"))
            .collect();
        let paths: Vec<&str> = paths.iter().map(String::as_str).collect();
        let made = || Undroppable::Made("thumbs".to_owned());
        warn_put_back(&output, &manifest, &put_back(&paths, vec![]), "put back");
        let first = put_back(&paths[..1], vec![]);
        warn_put_back(&output, &manifest, &first, "a sync would put back");
        warn_put_back(&output, &manifest, &put_back(&[], vec![]), "put back");
        let mixed = put_back(
            &["data/thumbs/a.png", "data/pets/b.png"],
            vec![("data/thumbs/a.png", made())],
        );
        warn_put_back(&output, &manifest, &mixed, "put back");
        let result = put_back(&["data/thumbs/a.png"], vec![("data/thumbs/a.png", made())]);
        warn_put_back(&output, &manifest, &result, "put back");
        let only = put_back(
            &["data/m/m.csv"],
            vec![("data/m/m.csv", Undroppable::Only("m".to_owned()))],
        );
        warn_put_back(&output, &manifest, &only, "put back");
        let both = put_back(
            &["data/m/m.csv", "data/thumbs/a.png"],
            vec![
                ("data/m/m.csv", Undroppable::Only("m".to_owned())),
                ("data/thumbs/a.png", made()),
            ],
        );
        warn_put_back(&output, &manifest, &both, "put back");
        let many: Vec<String> = (0..5000).map(|n| format!("data/pets/{n}.png")).collect();
        let mut gone = put_back(&many.iter().map(String::as_str).collect::<Vec<_>>(), vec![]);
        gone.placed = 5000;
        warn_put_back(&output, &manifest, &gone, "put back");
        let own = "keep files of your own in a folder outside data/";
        assert_eq!(
            output.warned(),
            [
                format!(
                    "put back 7 files that were deleted or moved, since data.lock pins them: data/pets/my images/1.png, data/pets/my images/2.png, data/pets/my images/3.png, data/pets/my images/4.png, data/pets/my images/5.png and 2 more; to leave a file out for good, run `fetchloom drop \"data/pets/my images/1.png\"`; {own}"
                ),
                format!(
                    "a sync would put back data/pets/my images/1.png, which was deleted or moved, since data.lock pins it; to leave a file out for good, run `fetchloom drop \"data/pets/my images/1.png\"`; {own}"
                ),
                format!(
                    "put back 2 files that were deleted or moved, since data.lock pins them: data/thumbs/a.png, data/pets/b.png; to leave a file out for good, run `fetchloom drop data/pets/b.png`; thumbs makes data/thumbs/a.png, so to leave it out for good, choose the files inside thumbs's step; {own}"
                ),
                format!(
                    "put back data/thumbs/a.png, which was deleted or moved, since data.lock pins it; thumbs makes data/thumbs/a.png, so to leave it out for good, choose the files inside thumbs's step; {own}"
                ),
                format!(
                    "put back data/m/m.csv, which was deleted or moved, since data.lock pins it; data/m/m.csv is the only file of m, so to leave it out for good, run `fetchloom remove m`; {own}"
                ),
                format!(
                    "put back 2 files that were deleted or moved, since data.lock pins them: data/m/m.csv, data/thumbs/a.png; data/m/m.csv is the only file of m, so to leave it out for good, run `fetchloom remove m`; thumbs makes data/thumbs/a.png, so to leave it out for good, choose the files inside thumbs's step; {own}"
                ),
                format!(
                    "put back 5,000 files that were deleted or moved, since data.lock pins them: data/pets/0.png, data/pets/1.png, data/pets/2.png, data/pets/3.png, data/pets/4.png and 4,995 more; to leave a file out for good, run `fetchloom drop data/pets/0.png`; {own}; to keep the data folder somewhere else than data/, set data_dir in data.toml"
                ),
            ]
        );
    }
}
