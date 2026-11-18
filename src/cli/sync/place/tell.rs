//! the lines a placement prints: what each dataset cost, what was removed, and every file left
//! alone or put back, with the reason.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use super::put::Tally;
use super::{Blocker, ChangedCopy, Entry, LeftBehind, Placement, Planned, Preview, Reason, Scope};
use crate::cli::sync::disk::is_inside;
use crate::output::{Output, Warning};
use crate::place::Method;
use crate::plan;
use crate::project::manifest::Manifest;
use crate::source::local;
use crate::units;

pub(super) fn tell(
    output: &Output,
    manifest: &Manifest,
    tallies: &BTreeMap<&str, Tally>,
    placement: &Placement,
    told: Option<&BTreeSet<String>>,
    scope: &Scope,
) {
    for (path, found) in &placement.cleared {
        output.step(
            "Removed",
            &format!("{path} · {} that was in the way", entry_name(*found)),
        );
    }
    tell_cleared(output, "Removed", &placement.cleared_outputs);
    warn_tracked(output, placement);
    warn_changed_outputs(output, placement);
    // what is discarded was the person's, so the line asks for attention, not a finished step
    for overwritten in &placement.overwritten {
        output.step_in(
            "Replaced",
            &format!(
                "{} · discarding {}",
                overwritten.path,
                overwritten_what(overwritten.reason)
            ),
            crate::output::Tone::Warn,
        );
    }
    // datasets that share a folder share its line, where the first of them comes
    let mut folders: Vec<(String, Tally)> = Vec::new();
    let mut at_folder: HashMap<String, usize> = HashMap::new();
    let adding = told.is_some_and(|names| !names.is_empty());
    let told = told.filter(|_| !output.shows_details());
    let mut datasets: u64 = 0;
    for (dataset, tally) in tallies
        .iter()
        .filter(|(dataset, _)| !told.is_some_and(|told| told.contains(**dataset)))
    {
        datasets += 1;
        let folder = plan::folder(manifest, dataset);
        #[cfg(test)]
        tests::looking();
        let at = *at_folder.entry(folder).or_insert_with_key(|folder| {
            folders.push((folder.clone(), Tally::default()));
            folders.len() - 1
        });
        let sum = &mut folders[at].1;
        sum.files += tally.files;
        sum.clones += tally.clones;
        sum.links += tally.links;
        sum.copied += tally.copied;
    }
    tell_placed(output, placement, &folders, datasets);
    tell_refilled(output, manifest, placement);
    // an add tells it among its own lines, where it told the rule it added before
    if !adding {
        super::tell_marked(output, placement);
    }
    for (folder, paths) in by_folder(manifest, &placement.adopted) {
        let (linked, kept): (Vec<&String>, Vec<&String>) = paths
            .into_iter()
            .partition(|path| placement.relinked.contains_key(*path));
        tell_adopted(
            output,
            &folder,
            &linked,
            Some(&linked_how(placement, &linked)),
        );
        tell_adopted(output, &folder, &kept, None);
    }
    tell_linked(output, manifest, placement);
    tell_removed(output, manifest, ["Retired", "Removed"], placement);
    warn_adopted_removed(output, &placement.removed_adopted, "removed");
    if let Some((why, _)) = placement.copied {
        let because = why.copied_because(&manifest.data_dir, "files were");
        match &because.help {
            Some(help) => output.warn_with(Warning::PlaceCopied, &because.said, help),
            None => output.warn(Warning::PlaceCopied, &because.said),
        }
    }
    warn_kept(output, manifest, scope, placement);
    warn_retired(output, placement);
    warn_put_back(output, placement, "put back");
    if placement.r_package {
        output.warn_with(
            Warning::ProjectRPackage,
            "this folder is an R package, whose data/ holds the datasets R loads, and R CMD check refuses other files there",
            &format!(
                "set data_dir = \"data-raw\" under [project] in data.toml, then run `{}` to place the files there",
                crate::cli::project::command("sync")
            ),
        );
    }
    warn_unreached(output, &placement.unreached);
    warn_restored(output, placement);
    super::mark::warn_unmarked(output, manifest, placement);
}

/// a warning for each file put back though no edit of this project's showed, saying where its
/// changed bytes were kept.
fn warn_restored(output: &Output, placement: &Placement) {
    for path in &placement.restored {
        if placement.repaired.contains(path) {
            let said = format!(
                "{path} was put back as pinned: its bytes had changed though its size and time had not"
            );
            match changed_copy(placement, path) {
                Some(copy) => output.warn_with(
                    Warning::PlaceRestored,
                    &format!("{said}. the changed copy is kept as {}", copy.kept_as),
                    &format!(
                        "if no one meant to change the file, delete {}",
                        copy.kept_as
                    ),
                ),
                None => output.warn(Warning::PlaceRestored, &said),
            }
        } else {
            let edits = "to edit a dataset's files, set copy = true for it in data.toml";
            let (said, help) = match changed_copy(placement, path) {
                // the change may be an edit through the link or damage, which no one can tell
                // apart from here, so the copy stays until a person deletes it
                Some(copy) => (
                    format!(
                        "{path} was put back as pinned: it is linked to the cache's copy, which was changed. the changed copy is kept as {}",
                        copy.kept_as
                    ),
                    format!(
                        "if no one meant to change the file, delete {}; {edits}",
                        copy.kept_as
                    ),
                ),
                None => (
                    format!(
                        "{path} was put back as pinned: it is linked to the cache's copy, which was changed"
                    ),
                    edits.to_owned(),
                ),
            };
            output.warn_with(Warning::PlaceRestored, &said, &help);
        }
    }
}

/// where the changed copy of `path` was kept, if sync kept it.
fn changed_copy<'p>(placement: &'p Placement, path: &str) -> Option<&'p ChangedCopy> {
    placement
        .changed_copies
        .iter()
        .find(|copy| copy.path == path)
}

/// a warning for each exclude naming files inside an archive, which only a members table reaches.
fn warn_unreached(output: &Output, unreached: &[plan::Unreached]) {
    for found in unreached {
        let (said, help) = unreached_said(found);
        output.warn_detailed(
            Warning::PlaceExcludeUnreached,
            &said,
            Some(&help),
            &found.paths,
        );
        for path in &found.paths {
            output.detail(path);
        }
    }
}

/// what an exclude naming files inside an archive leaves undone, and the command that does it.
fn unreached_said(found: &plan::Unreached) -> (String, String) {
    let inside = match found.archives.as_slice() {
        [one] => one
            .rsplit_once('/')
            .map_or(one.as_str(), |(_, name)| name)
            .to_owned(),
        many => format!("{} archives", units::grouped(many.len() as u64)),
    };
    let first = found.paths.first().map_or("", String::as_str);
    let command =
        crate::cli::project::command(&format!("exclude {}", crate::cli::shell_word(first)));
    let (what, help) = match found.paths.len() {
        1 => ("a file".to_owned(), format!("run `{command}`")),
        count => (
            format!("{} files", units::grouped(count as u64)),
            format!(
                "run `{}` with their paths, as `{command}`",
                crate::cli::project::command("exclude")
            ),
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

/// a warning for the files someone deleted or moved that a sync puts back, as `verb` says, with up
/// to [`NAMED`] ways to leave them out for good. a folder deleted whole is named once, and a
/// dataset's own folder deleted whole is a fresh start, which needs no warning.
fn warn_put_back(output: &Output, placement: &Placement, verb: &str) {
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
    let mut help = Vec::new();
    let unpacked: Vec<&(String, String)> = paths
        .iter()
        .filter_map(|path| placement.unpacked.get(*path))
        .collect();
    // the file the person unpacked by hand stays theirs, and decompress takes it as placed
    if let Some((beside, _)) = unpacked.first() {
        let datasets: BTreeSet<&str> = unpacked
            .iter()
            .map(|(_, dataset)| dataset.as_str())
            .collect();
        let datasets: Vec<&str> = datasets.into_iter().collect();
        help.push(format!(
            "to keep {beside}, which you unpacked, in its place, set decompress = true for {} in data.toml",
            units::listed(&datasets, "and")
        ));
    }
    if unpacked.len() < paths.len() {
        help.push(
            "there is nothing to do: each sync puts back the files data.lock pins".to_owned(),
        );
        if let Some((name, files, path)) = items.iter().find(|(_, _, path)| {
            !placement.unexcludable.contains_key(*path) && !placement.unpacked.contains_key(*path)
        }) {
            let (what, name) = if *files > 1 {
                ("folder", *name)
            } else {
                ("file", path.as_str())
            };
            help.push(format!(
                "to leave a {what} out for good, run `{}`",
                crate::cli::project::command(&format!("exclude {}", crate::cli::shell_word(name)))
            ));
        }
    }
    output.warn_detailed(Warning::PlacePutBack, &said, Some(&help.join("; ")), &paths);
    for path in paths {
        output.detail(path);
    }
}

/// what a warning about `paths` names: the one file, the one folder deleted whole, or the first
/// [`NAMED`] of `items`, a folder with how many files it held, and how many the rest hold.
fn put_back_said(
    verb: &str,
    paths: &[&String],
    items: &[(&str, u64, &String)],
    folders: &BTreeMap<&str, usize>,
) -> String {
    let shown = |(name, files, path): &(&str, u64, &String)| match *files {
        1 => (*path).clone(),
        files if folders.contains_key(name) => format!("the {} in {name}/", units::grouped(files)),
        _ => (*name).to_owned(),
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

/// the topmost folder above `path`, no higher than `floor`, that `holding` says holds no file: the
/// folder a person deleted or emptied, or none while the file's own folder holds one.
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
    let mut at_folder: HashMap<String, usize> = HashMap::new();
    for (dataset, paths) in by_dataset {
        let folder = plan::folder(manifest, dataset);
        let at = *at_folder.entry(folder).or_insert_with_key(|folder| {
            folders.push((folder.clone(), Vec::new()));
            folders.len() - 1
        });
        folders[at].1.extend(paths);
    }
    for (_, paths) in &mut folders {
        paths.sort();
    }
    folders
}

/// the files sync adopted in `folder`, linked to the cache as `linked` says, as in `linked to the
/// cache`, or staying as they are. `-v` names each file.
fn tell_adopted(output: &Output, folder: &str, paths: &[&String], linked: Option<&str>) {
    let said = match (paths.len(), linked) {
        (0, _) => return,
        (1, Some(how)) => {
            format!("1 file already held the pinned bytes, so it is now {how}")
        }
        (1, None) => "1 file already held the pinned bytes, so it stays as it is".to_owned(),
        (count, Some(how)) => format!(
            "{} files already held the pinned bytes, so they are now {how}",
            units::grouped(count as u64)
        ),
        (count, None) => {
            format!(
                "{} files already held the pinned bytes, so they stay as they are",
                units::grouped(count as u64)
            )
        }
    };
    output.step("Adopted", &format!("{folder} · {said}"));
    for path in paths {
        output.detail(path);
    }
}

/// how the files `paths` of [`Placement::relinked`] were linked: `linked to the cache`, `cloned
/// from the cache`, or both.
fn linked_how(placement: &Placement, paths: &[&String]) -> String {
    let clones = paths
        .iter()
        .filter(|path| {
            placement
                .relinked
                .get(**path)
                .is_some_and(|linked| linked.method == Method::Reflink)
        })
        .count() as u64;
    crate::place::placed_how(clones, paths.len() as u64 - clones, 0, 0)
}

/// a line for each folder whose copies of cached files, which fetchloom placed or took as placed
/// before this run, sync linked to the cache, with the bytes that frees. `-v` names each file.
fn tell_linked(output: &Output, manifest: &Manifest, placement: &Placement) {
    let adopted: HashSet<&String> = placement.adopted.values().flatten().collect();
    let mut by_dataset: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (path, linked) in &placement.relinked {
        if !adopted.contains(path) {
            by_dataset
                .entry(linked.dataset.clone())
                .or_default()
                .push(path.clone());
        }
    }
    for (folder, paths) in by_folder(manifest, &by_dataset) {
        let freed = paths
            .iter()
            .filter_map(|path| placement.relinked.get(*path))
            .fold(0_u64, |sum, linked| sum.saturating_add(linked.size));
        let how = linked_how(placement, &paths);
        let said = match paths.len() {
            1 => format!("1 copy of a cached file is now {how}"),
            count => format!(
                "{} copies of cached files are now {how}",
                units::grouped(count as u64)
            ),
        };
        output.step(
            "Linked",
            &format!("{folder} · {said}, freeing {}", units::size(freed)),
        );
        for path in paths {
            output.detail(path);
        }
    }
}

/// a warning for removed files someone else put there before sync adopted them, as `verb` says.
fn warn_adopted_removed(output: &Output, paths: &[String], verb: &str) {
    match paths {
        [] => {}
        [path] => output.warn(Warning::PlaceAdoptedRemoved, &format!(
            "{verb} {path}, a file that was already there when fetchloom adopted it"
        )),
        [first, ..] => output.warn(Warning::PlaceAdoptedRemoved, &format!(
            "{verb} {} files that were already there when fetchloom adopted them, such as {first}",
            paths.len()
        )),
    }
}

/// the lines a dry run of `scope` prints for a placement it only planned. `-v` names each file.
pub(crate) fn tell_preview(output: &Output, manifest: &Manifest, scope: &Scope, preview: &Preview) {
    // sizes stay out: a file whose pin only a download can learn has no size yet
    let tell_files = |verb: &str, files_of: fn(&Planned) -> &Vec<String>| {
        let datasets: Vec<(&String, &Vec<String>)> = preview
            .by_dataset
            .iter()
            .map(|(dataset, planned)| (dataset, files_of(planned)))
            .filter(|(_, files)| !files.is_empty())
            .collect();
        if !crate::cli::fetch::one_by_one(output, datasets.len()) {
            let files = datasets.iter().map(|(_, files)| files.len() as u64).sum();
            output.step(
                verb,
                &format!(
                    "{} · {} · `-v` lists them",
                    units::count(datasets.len() as u64, "dataset", "datasets"),
                    units::count(files, "file", "files")
                ),
            );
            return;
        }
        for (dataset, files) in datasets {
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
        }
    };
    tell_files("Would place", |planned| &planned.place);
    tell_files("Would renew", |planned| &planned.replace);
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
    let placement = &preview.placement;
    tell_refilled(output, manifest, placement);
    super::mark::tell_made(output, &placement.marked, placement.in_git, "Would ignore");
    for overwritten in &placement.overwritten {
        output.warn(
            Warning::PlaceOverwritten,
            &format!(
                "{} is {}, and --force would replace it",
                overwritten.path,
                overwritten_what(overwritten.reason)
            ),
        );
    }
    tell_would_remove(output, manifest, preview);
    warn_adopted_removed(output, &placement.removed_adopted, "would remove");
    left_behind(output, manifest, placement.left_behind.as_ref());
    warn_changed_outputs(output, placement);
    warn_kept(output, manifest, scope, placement);
    warn_retired(output, placement);
    warn_put_back(output, placement, "a sync would put back");
    warn_unreached(output, &placement.unreached);
    for path in &placement.restored {
        if placement.repaired.contains(path) {
            let said = format!(
                "{path} would be put back as pinned: its bytes changed though its size and time did not"
            );
            output.warn(
                Warning::PlaceRestored,
                &match changed_copy(placement, path) {
                    Some(copy) => {
                        format!("{said}. the changed copy would be kept as {}", copy.kept_as)
                    }
                    None => said,
                },
            );
        } else {
            output.warn(Warning::PlaceRestored, &match changed_copy(placement, path) {
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
    super::mark::warn_unmarked(output, manifest, placement);
}

/// a line, as `verb` says, for each file of `cleared`, by path, that an output's folder held beyond
/// what its pin holds, with that output's name.
fn tell_cleared(output: &Output, verb: &str, cleared: &[(String, String)]) {
    for (path, name) in cleared {
        output.step_in(
            verb,
            &format!("{path} · a file {name}'s pin does not hold"),
            crate::output::Tone::Warn,
        );
    }
}

/// a warning for each output whose folder holds files git tracks that `sync --force` left there,
/// as it never deletes or overwrites one, with the ways on; one this machine placed none of may be
/// a folder of code data.toml names as an output's.
fn warn_tracked(output: &Output, placement: &Placement) {
    for held in &placement.tracked {
        let help = if held.placed_here {
            local::git_ways(&held.name, &held.shown, false)
        } else {
            format!(
                "check that `at` for {} in data.toml names the folder its pipeline writes",
                held.name
            )
        };
        output.warn_with(
            Warning::PlaceKept,
            &local::kept_by_git(&held.name, &held.tracked),
            &help,
        );
    }
}

/// what a dry run would remove: each file an output's folder holds beyond its pin, by name, then
/// the files no dataset needs, which `-v` lists.
fn tell_would_remove(output: &Output, manifest: &Manifest, preview: &Preview) {
    let placement = &preview.placement;
    tell_cleared(output, "Would remove", &placement.cleared_outputs);
    warn_tracked(output, placement);
    tell_removed(
        output,
        manifest,
        ["Would retire", "Would remove"],
        placement,
    );
    let cleared: HashSet<&str> = placement
        .cleared_outputs
        .iter()
        .map(|(path, _)| path.as_str())
        .collect();
    for path in &preview.changes.remove {
        if !cleared.contains(path.as_str()) {
            output.detail(path);
        }
    }
}

/// a line for each folder `folders` placed, or past a few dozen one line for the `datasets` in
/// them, which `-v` lists.
fn tell_placed(output: &Output, placement: &Placement, folders: &[(String, Tally)], datasets: u64) {
    if crate::cli::fetch::one_by_one(output, folders.len()) {
        for (folder, tally) in folders {
            let fresh = if placement.refilled.contains(folder) {
                " · the folder was missing"
            } else {
                ""
            };
            output.step("Placed", &format!("{folder} · {}{fresh}", placed(tally)));
        }
    } else {
        let mut sum = Tally::default();
        for (_, tally) in folders {
            sum.files += tally.files;
            sum.clones += tally.clones;
            sum.links += tally.links;
            sum.copied += tally.copied;
        }
        let missing = folders
            .iter()
            .filter(|(folder, _)| placement.refilled.contains(folder))
            .count();
        let fresh = match missing {
            0 => String::new(),
            1 => " · 1 folder was missing".to_owned(),
            _ => format!(" · {} folders were missing", units::grouped(missing as u64)),
        };
        output.step(
            "Placed",
            &format!(
                "{} · {}{fresh} · `-v` lists them",
                units::count(datasets, "dataset", "datasets"),
                placed(&sum)
            ),
        );
    }
}

/// how to keep away what `placement` filled again once whole folders went: the data folder, or
/// the datasets of each folder no run told of yet, which a person deleted to free space or renamed.
fn tell_refilled(output: &Output, manifest: &Manifest, placement: &Placement) {
    if placement.moved {
        output.hint(&format!(
            "to keep the data folder somewhere else than {}/, set data_dir in data.toml",
            manifest.data_dir
        ));
        return;
    }
    // an output's folder is named by another key, so outputs are told apart
    let (outputs, others): (Vec<&str>, Vec<&str>) = placement
        .filled
        .iter()
        .filter(|name| placement.to_drop.contains(&plan::folder(manifest, name)))
        .map(String::as_str)
        .partition(|name| {
            manifest
                .datasets
                .get(*name)
                .is_some_and(crate::project::manifest::Entry::is_output)
        });
    for (datasets, outputs) in [(others, false), (outputs, true)] {
        let drop = crate::cli::project::command(&format!("drop {}", datasets.join(" ")));
        let moved = |name: &str| crate::source::local::set_moved(name, outputs);
        output.hint(&match datasets.as_slice() {
            [] => continue,
            [one] => format!(
                "if you deleted {} to free space, `{drop}` keeps it off this machine, or if you moved it, {}",
                plan::folder(manifest, one),
                moved(one)
            ),
            many => format!(
                "if you deleted their folders to free space, `{drop}` keeps {} off this machine, or if you moved one, {}",
                units::listed(many, "and"),
                moved("its dataset")
            ),
        });
    }
}

/// how many files `tally` placed, and how: `3 files linked to the cache`, `1 file cloned from the
/// cache` or `2 files copied, 1.2 KiB`, as [`crate::place::placed_how`] says it.
fn placed(tally: &Tally) -> String {
    let copies = tally.files - tally.clones - tally.links;
    let how = crate::place::placed_how(tally.clones, tally.links, copies, tally.copied);
    format!("{} {how}", units::count(tally.files, "file", "files"))
}

/// the lines for files a placement removed or would: `verbs[0]` per dataset a drop cut short,
/// `verbs[1]` for unneeded files, naming the data folder so it never reads as a cache cleanup.
fn tell_removed(output: &Output, manifest: &Manifest, verbs: [&str; 2], placement: &Placement) {
    for (dataset, files) in &placement.cut_short {
        output.step(
            verbs[0],
            &format!(
                "{} · {} a drop cut short left",
                plan::folder(manifest, dataset),
                units::count(*files, "file", "files")
            ),
        );
    }
    if !placement.cut_short.is_empty() {
        let names: Vec<&str> = placement.cut_short.keys().map(String::as_str).collect();
        output.hint(&format!(
            "`{}` frees what the cache still holds of {}",
            crate::cli::project::command(&format!("drop {}", names.join(" "))),
            if names.len() == 1 { "it" } else { "them" }
        ));
    }
    // what an output's folder held beyond its pin is told file by file, as its own
    let unneeded = placement
        .removed
        .saturating_sub(placement.cut_short.values().sum())
        .saturating_sub(placement.cleared_outputs.len() as u64);
    if unneeded > 0 {
        output.step(
            verbs[1],
            &format!(
                "{} from {}/ that no dataset needs",
                units::count(unneeded, "file", "files"),
                manifest.data_dir
            ),
        );
    }
}

/// the files of `dataset` already where a file goes whose pin only a download learns.
fn tell_checks(output: &Output, manifest: &Manifest, dataset: &str, files: &[String]) {
    let said = match files.len() {
        0 => return,
        1 => "1 file is already there; the download tells whether it holds the pinned bytes"
            .to_owned(),
        count => format!(
            "{} files are already there; the downloads tell whether they hold the pinned bytes",
            units::grouped(count as u64)
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

/// the files in `folder` a sync would adopt. `-v` names each file.
fn tell_adoptions(output: &Output, folder: &str, files: &[&String]) {
    let said = match files.len() {
        0 => return,
        1 => "1 file already holds the pinned bytes".to_owned(),
        count => format!(
            "{} files already hold the pinned bytes",
            units::grouped(count as u64)
        ),
    };
    output.step("Would adopt", &format!("{folder} · {said}"));
    for path in files {
        output.detail(path);
    }
}

/// a warning for the files placed before `data_dir` changed, which stay, since sync deletes nothing
/// outside the data folder. it names the folder's other files, so deleting it costs no one theirs.
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
            count => format!(
                "{} files fetchloom placed {before}, such as {first}",
                units::grouped(count as u64)
            ),
        };
        let (theirs, help) = match others.len() {
            1 => (
                format!("1 file it did not place: {one}"),
                format!("move {one} out of {folder} before you delete {folder}"),
            ),
            count => (
                format!(
                    "{} files it did not place: {}",
                    units::grouped(count as u64),
                    units::some_of(others, 3)
                ),
                format!(
                    "move the {} files fetchloom did not place out of {folder} before you delete {folder}",
                    units::grouped(count as u64)
                ),
            ),
        };
        output.warn_detailed(
            Warning::PlaceLeftBehind,
            &format!(
                "{folder} still holds {placed}, and {theirs}. fetchloom leaves them all there"
            ),
            Some(&help),
            files,
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
                "{folder} still holds {} files fetchloom placed {before}, such as {first}. fetchloom leaves them there",
                units::grouped(count as u64)
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
                "{} files fetchloom placed {before} are outside the data folder now, such as {first}. fetchloom leaves them there",
                units::grouped(count as u64)
            ),
            "delete them when you no longer need them".to_owned(),
        ),
    };
    output.warn_detailed(Warning::PlaceLeftBehind, &said, Some(&help), files);
    for path in files {
        output.detail(path);
    }
}

/// a line for the files a `sync --force` cut off at once had moved out of the way, given their
/// names back, or that a sync would give back. `-v` names each one.
pub(crate) fn restored_aside(output: &Output, restored: &[String], dry: bool) {
    let verb = if dry { "Would move back" } else { "Moved back" };
    let said = match restored {
        [] => return,
        [path] => {
            format!("{path} · a `sync --force` cut off before its end had moved it out of the way")
        }
        [first, ..] => format!(
            "{} files, such as {first} · a `sync --force` cut off before its end had moved them out of the way",
            units::grouped(restored.len() as u64)
        ),
    };
    output.step(verb, &said);
    if restored.len() > 1 {
        for path in restored {
            output.detail(path);
        }
    }
}

/// a warning for each person's file `sync --force` moved out of the way that could not go back,
/// by the hidden name it is kept under, told by every sync while it stays there.
pub(super) fn set_aside(output: &Output, manifest: &Manifest, aside: &BTreeMap<String, String>) {
    for (hidden, path) in aside {
        output.warn_detailed(
            Warning::PlaceSetAside,
            &format!(
                "{path} is kept as {hidden}: `sync --force` moved it out of the way and could not put it back"
            ),
            Some(&format!(
                "to get it back, move {hidden} out of {}/ or give it another name; sync never deletes it",
                manifest.data_dir
            )),
            [hidden],
        );
    }
}

/// a warning for each folder no dataset uses now that still holds files fetchloom did not place,
/// such as a person's work or a changed copy sync kept, which are the person's alone from then
/// on. `-v` names each file.
fn warn_retired(output: &Output, placement: &Placement) {
    for retired in &placement.retired {
        let folder = &retired.folder;
        let (said, help) = match (&retired.output, retired.unplaced.as_slice()) {
            (_, []) => continue,
            (Some((name, at)), [only]) => (
                format!(
                    "{folder} held {name} until its folder moved to {at}, and still holds {only}, a file its pin does not hold, so the folder stays"
                ),
                format!(
                    "it is yours, and fetchloom no longer looks at it; delete {folder} once you no longer need it"
                ),
            ),
            (Some((name, at)), [first, ..]) => (
                format!(
                    "{folder} held {name} until its folder moved to {at}, and still holds {} files its pin does not hold, such as {first}, so the folder stays",
                    retired.unplaced.len()
                ),
                format!(
                    "they are yours, and fetchloom no longer looks at them; delete {folder} once you no longer need them"
                ),
            ),
            (None, [only]) => (
                format!(
                    "{folder} still holds {only}, a file fetchloom did not place, so the folder stays"
                ),
                "it is yours, and fetchloom no longer looks at it".to_owned(),
            ),
            (None, [first, ..]) => (
                format!(
                    "{folder} still holds {} files fetchloom did not place, such as {first}, so the folder stays",
                    retired.unplaced.len()
                ),
                "they are yours, and fetchloom no longer looks at them".to_owned(),
            ),
        };
        output.warn_detailed(
            Warning::PlaceLeftBehind,
            &said,
            Some(&help),
            &retired.unplaced,
        );
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
        let help = match &blocker.dataset {
            Some(dataset) => left_alone(manifest, dataset),
            None => help,
        };
        output.warn_with(Warning::PlaceBlocked, &said, &help);
    }
    // a copy an output's move left is named with its folder
    let moved = |path: &str| {
        placement
            .retired
            .iter()
            .any(|retired| retired.output.is_some() && is_inside(path, &retired.folder))
    };
    for kept in &placement.kept {
        let force = forcing(
            manifest,
            scope,
            &kept.dataset.iter().map(String::as_str).collect(),
        );
        let (message, help) = match kept.reason {
            // told with its entry in the way above, or with its output's other files git tracks
            Reason::Blocked | Reason::Tracked => continue,
            Reason::Unneeded if moved(&kept.path) => continue,
            Reason::Changed | Reason::Foreign if kept.told => continue,
            Reason::Changed => match kept.dataset.as_deref() {
                Some(dataset) if kept.earlier => (
                    format!(
                        "{} has changed, so it stays, but it is an edit of an earlier version of {dataset}, not what data.lock pins now",
                        kept.path
                    ),
                    keeping_edits(dataset, &force),
                ),
                Some(dataset) => (
                    format!("{} has changed, so it stays", kept.path),
                    keeping_edits(dataset, &force),
                ),
                None => (
                    format!("{} has changed, so it stays", kept.path),
                    format!("`{force}` discards your edit"),
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
                    "{} holds other bytes than data.lock pins, and fetchloom did not place it, so it stays",
                    kept.path
                ),
                match kept.dataset.as_deref() {
                    Some(dataset) if kept.copied_edit => keeping_edits(dataset, &force),
                    _ => format!("`{force}` replaces it"),
                },
            ),
            Reason::Unneeded => (
                format!(
                    "{} is no longer needed, but it has changed, so it stays",
                    kept.path
                ),
                "delete it if you do not need it".to_owned(),
            ),
        };
        output.warn_with(Warning::PlaceKept, &message, &help);
    }
}

/// a warning for each output `placement` leaves whole, as its folder changed since it was pinned,
/// or is gone, with the ways on, unless an earlier run told of the same change.
fn warn_changed_outputs(output: &Output, placement: &Placement) {
    for changed in placement
        .changed_outputs
        .iter()
        .filter(|changed| !changed.told)
    {
        let said = changed.gone_said(&changed.at).unwrap_or_else(|| {
            format!(
                "{} changed since {} was pinned: {}, so sync leaves it as it is",
                changed.at,
                changed.name,
                changed.summary()
            )
        });
        output.warn_with(Warning::OutputChanged, &said, &changed.ways_on());
    }
}

/// the ways to keep an edit of a file of `dataset`, then `force`, the command that discards it.
fn keeping_edits(dataset: &str, force: &str) -> String {
    format!(
        "keep your version as a file or dataset of your own, or set copy = true for {dataset} in data.toml to edit its files in place; `{force}` discards your edit"
    )
}

/// the command replacing what a sync of `scope` kept of `datasets`: after a named sync, only the
/// kept datasets, so it replaces nothing else, or every placed one when it cannot tell.
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

/// what clears an entry in the way holding files of `dataset`, which a sync of others leaves alone.
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
        Reason::Tracked => "a file git tracks",
    }
}

/// the warning and help for an entry in the way of `files` files, `force` the command replacing it.
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

    /// a clone shares blocks with the cached copy only until either is written, so a person who
    /// reads "linked" would expect an edit to reach the cache, which it never does.
    #[test]
    fn a_placed_line_calls_clones_cloned_and_hardlinks_linked() {
        for (clones, links, copied, said) in [
            (2, 0, 0, "2 files cloned from the cache"),
            (0, 2, 0, "2 files linked to the cache"),
            (0, 0, 2, "2 files copied, 6 B"),
            (1, 0, 1, "2 files cloned and copied, 3 B"),
            (1, 1, 0, "2 files cloned and linked from the cache"),
        ] {
            let tally = Tally {
                files: clones + links + copied,
                clones,
                links,
                copied: copied * 3,
                ..Tally::default()
            };
            assert_eq!(placed(&tally), said);
        }
    }
    use crate::output::{Level, Style};

    #[test]
    fn files_inside_many_archives_are_counted_with_their_thousands_apart() {
        let found = plan::Unreached {
            dataset: "set".to_owned(),
            pattern: "*.csv".to_owned(),
            archives: (0..1_200)
                .map(|index| format!("data/set/{index}.zip"))
                .collect(),
            paths: vec!["data/set/0/a.csv".to_owned(), "data/set/1/a.csv".to_owned()],
        };
        assert_eq!(
            unreached_said(&found).0,
            "exclude \"*.csv\" in datasets.set names 2 files inside 1,200 archives, which exclude does not reach"
        );
    }

    #[test]
    fn a_deleted_folder_is_named_once_and_a_lone_file_by_its_path() {
        let (only, first, second) = (
            "data/set/c/only.csv".to_owned(),
            "data/set/a/1.csv".to_owned(),
            "data/set/a/2.csv".to_owned(),
        );
        let paths = [&first, &second, &only];
        let items = [("data/set/a", 2, &first), ("data/set/c", 1, &only)];
        let folders = BTreeMap::from([("data/set/a", 0), ("data/set/c", 1)]);
        assert_eq!(
            put_back_said("put back", &paths, &items, &folders),
            "put back 3 files that were deleted or moved, since data.lock pins them: the 2 in data/set/a/, data/set/c/only.csv"
        );
    }

    /// no folder above the floor counts, and a file whose folder still holds one was deleted alone.
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

    thread_local! {
        static LOOKS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    pub(super) fn looking() {
        LOOKS.set(LOOKS.get() + 1);
    }

    /// looking through every folder told before took placing 40,000 links from 0.6 s to 7.6 s.
    /// datasets that share a path share the line where the first of them comes.
    #[test]
    fn each_dataset_finds_its_folder_with_one_look() {
        use std::fmt::Write as _;

        let mut text =
            "[datasets.a-shared]\nref = \"x\"\npath = \"shared\"\n\n[datasets.b-alone]\nref = \"x\"\n\n[datasets.c-shared]\nref = \"x\"\npath = \"shared\"\n".to_owned();
        for number in 0..2000 {
            let _ = write!(text, "\n[datasets.d{number:04}]\nref = \"x\"\n");
        }
        let manifest = Manifest::parse(&text).unwrap();
        let tallies: BTreeMap<&str, Tally> = manifest
            .datasets
            .keys()
            .map(|name| {
                let tally = Tally {
                    files: 1,
                    links: 1,
                    ..Tally::default()
                };
                (name.as_str(), tally)
            })
            .collect();
        let output = Output::new(
            Style {
                color: false,
                unicode: false,
                interactive: false,
                watched: false,
                links: false,
                accessible: false,
            },
            Level::Verbose,
            false,
            false,
        );
        let looked = LOOKS.get();
        tell(
            &output,
            &manifest,
            &tallies,
            &Placement::default(),
            None,
            &Scope::All,
        );
        let looks = LOOKS.get() - looked;
        assert!(
            looks <= 2 * tallies.len(),
            "{looks} looks for {} datasets",
            tallies.len()
        );
        let said = output.said();
        assert_eq!(said.len(), 2002, "{:?}", &said[..3]);
        assert_eq!(
            said[..3],
            [
                "     Placed data/shared - 2 files linked to the cache",
                "     Placed data/b-alone - 1 file linked to the cache",
                "     Placed data/d0000 - 1 file linked to the cache",
            ]
        );
        assert_eq!(
            said[2001],
            "     Placed data/d1999 - 1 file linked to the cache"
        );
    }

    /// a few dozen folders get a line each; past that one line counts them, and `-v` names each.
    #[test]
    fn many_folders_placed_are_counted_on_one_line() {
        use std::fmt::Write as _;

        for (datasets, level, lines) in [
            (40, Level::Normal, 40),
            (41, Level::Normal, 1),
            (41, Level::Verbose, 41),
        ] {
            let mut text = String::new();
            for number in 0..datasets {
                let _ = write!(text, "\n[datasets.d{number:02}]\nref = \"x\"\n");
            }
            let manifest = Manifest::parse(&text).unwrap();
            let tallies: BTreeMap<&str, Tally> = manifest
                .datasets
                .keys()
                .map(|name| {
                    let tally = Tally {
                        files: 2,
                        links: 2,
                        ..Tally::default()
                    };
                    (name.as_str(), tally)
                })
                .collect();
            let output = Output::new(
                Style {
                    color: false,
                    unicode: false,
                    interactive: false,
                    watched: false,
                    links: false,
                    accessible: false,
                },
                level,
                false,
                false,
            );
            let placement = Placement {
                refilled: ["data/d03".to_owned()].into(),
                ..Placement::default()
            };
            tell(&output, &manifest, &tallies, &placement, None, &Scope::All);
            let said = output.said();
            assert_eq!(said.len(), lines, "{datasets}: {said:?}");
            if lines == 1 {
                assert_eq!(
                    said[0],
                    "     Placed 41 datasets - 82 files linked to the cache - 1 folder was missing - `-v` lists them"
                );
            } else {
                assert_eq!(
                    said[3],
                    "     Placed data/d03 - 2 files linked to the cache - the folder was missing"
                );
            }
        }
    }

    /// dataset folders gone at once, deleted or moved, are named in one hint, and an output's
    /// apart, as data.toml names its folder by `at`.
    #[test]
    fn folders_gone_at_once_are_named_in_one_hint() {
        let text = "[datasets.iris]\nref = \"x\"\n\n[datasets.pets]\nref = \"y\"\n\n[datasets.processed]\nref = \"local\"\nat = \"data/processed\"\n";
        let manifest = Manifest::parse(text).unwrap();
        let style = Style {
            color: false,
            unicode: false,
            interactive: false,
            watched: false,
            links: false,
            accessible: false,
        };
        for (filled, hints) in [
            (
                &["iris", "pets"][..],
                &[
                    "  hint: if you deleted their folders to free space, `fetchloom drop iris pets` keeps iris and pets off this machine, or if you moved one, set path = \"<new folder>\" for its dataset in data.toml",
                ][..],
            ),
            (
                &["iris", "processed"],
                &[
                    "  hint: if you deleted data/iris to free space, `fetchloom drop iris` keeps it off this machine, or if you moved it, set path = \"<new folder>\" for iris in data.toml",
                    "  hint: if you deleted data/processed to free space, `fetchloom drop processed` keeps it off this machine, or if you moved it, set at = \"<new folder>\" for processed in data.toml",
                ],
            ),
        ] {
            let output = Output::new(style, Level::Normal, false, false);
            let placement = Placement {
                filled: filled.iter().map(|name| (*name).to_owned()).collect(),
                to_drop: filled
                    .iter()
                    .map(|name| plan::folder(&manifest, name))
                    .collect(),
                ..Placement::default()
            };
            tell_refilled(&output, &manifest, &placement);
            assert_eq!(output.said(), hints, "{filled:?}");
        }
    }

    /// a dry run names the folder of each of a few dozen datasets it would place; past that one
    /// line counts them, and `-v` names each with its files.
    #[test]
    fn many_datasets_a_dry_run_would_place_are_counted_on_one_line() {
        for (datasets, level, lines) in [
            (40, Level::Normal, 40),
            (41, Level::Normal, 1),
            (41, Level::Verbose, 82),
        ] {
            let manifest = Manifest::parse("").unwrap();
            let preview = Preview {
                by_dataset: (0..datasets)
                    .map(|number| {
                        let planned = Planned {
                            place: vec![format!("data/d{number:02}/a.csv")],
                            ..Planned::default()
                        };
                        (format!("d{number:02}"), planned)
                    })
                    .collect(),
                ..Preview::default()
            };
            let output = Output::new(
                Style {
                    color: false,
                    unicode: false,
                    interactive: false,
                    watched: false,
                    links: false,
                    accessible: false,
                },
                level,
                false,
                false,
            );
            tell_preview(&output, &manifest, &Scope::All, &preview);
            let said = output.said();
            assert_eq!(said.len(), lines, "{datasets}: {said:?}");
            if lines == 1 {
                assert_eq!(
                    said[0],
                    "      Would place 41 datasets - 41 files - `-v` lists them"
                );
            } else {
                assert_eq!(said[0], "      Would place data/d00 - 1 file");
            }
        }
    }

    /// the help says there is nothing to do, then gives exclude with the first file it can leave
    /// out, quoted for a shell. the only file of a dataset gets no more, and a whole data folder
    /// put back no `data_dir`, which a sync of a moved folder tells of apart.
    #[test]
    fn files_put_back_are_counted_and_the_first_five_named() {
        let output = Output::new(
            Style {
                color: false,
                unicode: false,
                interactive: false,
                watched: false,
                links: false,
                accessible: false,
            },
            Level::Quiet,
            false,
            false,
        );
        let put_back = |paths: &[&str], unexcludable: Vec<(&str, &str)>| Placement {
            put_back: paths.iter().map(|path| (*path).to_owned()).collect(),
            unexcludable: unexcludable
                .into_iter()
                .map(|(path, dataset)| (path.to_owned(), dataset.to_owned()))
                .collect(),
            ..Placement::default()
        };
        let paths: Vec<String> = (1..=7)
            .map(|n| format!("data/pets/my images/{n}.png"))
            .collect();
        let paths: Vec<&str> = paths.iter().map(String::as_str).collect();
        warn_put_back(&output, &put_back(&paths, vec![]), "put back");
        let first = put_back(&paths[..1], vec![]);
        warn_put_back(&output, &first, "a sync would put back");
        warn_put_back(&output, &put_back(&[], vec![]), "put back");
        let only = put_back(&["data/m/m.csv"], vec![("data/m/m.csv", "m")]);
        warn_put_back(&output, &only, "put back");
        let both = put_back(
            &["data/m/m.csv", "data/pets/b.png"],
            vec![("data/m/m.csv", "m")],
        );
        warn_put_back(&output, &both, "put back");
        let many: Vec<String> = (0..5000).map(|n| format!("data/pets/{n}.png")).collect();
        let mut gone = put_back(&many.iter().map(String::as_str).collect::<Vec<_>>(), vec![]);
        gone.placed = 5000;
        warn_put_back(&output, &gone, "put back");
        let nothing = "there is nothing to do: each sync puts back the files data.lock pins";
        assert_eq!(
            output.warned(),
            [
                format!(
                    "put back 7 files that were deleted or moved, since data.lock pins them: data/pets/my images/1.png, data/pets/my images/2.png, data/pets/my images/3.png, data/pets/my images/4.png, data/pets/my images/5.png and 2 more; {nothing}; to leave a file out for good, run `fetchloom exclude \"data/pets/my images/1.png\"`"
                ),
                format!(
                    "a sync would put back data/pets/my images/1.png, which was deleted or moved, since data.lock pins it; {nothing}; to leave a file out for good, run `fetchloom exclude \"data/pets/my images/1.png\"`"
                ),
                format!(
                    "put back data/m/m.csv, which was deleted or moved, since data.lock pins it; {nothing}"
                ),
                format!(
                    "put back 2 files that were deleted or moved, since data.lock pins them: data/m/m.csv, data/pets/b.png; {nothing}; to leave a file out for good, run `fetchloom exclude data/pets/b.png`"
                ),
                format!(
                    "put back 5,000 files that were deleted or moved, since data.lock pins them: data/pets/0.png, data/pets/1.png, data/pets/2.png, data/pets/3.png, data/pets/4.png and 4,995 more; {nothing}; to leave a file out for good, run `fetchloom exclude data/pets/0.png`"
                ),
            ]
        );
    }
}
