//! the lines a placement prints: what each dataset cost, what was removed, and every file left
//! alone or put back, with the reason.

use std::collections::BTreeMap;

use super::put::Tally;
use super::{Blocker, ChangedCopy, Entry, LeftBehind, Placement, Preview, Reason, Scope};
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
                "{} · {}",
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
        output.step(
            "Placed",
            &format!(
                "{folder} · {} {how}",
                units::count(tally.files, "file", "files")
            ),
        );
    }
    for (dataset, paths) in &placement.adopted {
        let (linked, kept): (Vec<&String>, Vec<&String>) = paths
            .iter()
            .partition(|path| placement.relinked.contains(*path));
        tell_adopted(output, manifest, dataset, &linked, true);
        tell_adopted(output, manifest, dataset, &kept, false);
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
    warn_put_back(output, manifest, &placement.put_back, "put back");
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
pub(crate) fn unreached_said(found: &plan::Unreached) -> (String, String) {
    let inside = match found.archives.as_slice() {
        [one] => one
            .rsplit_once('/')
            .map_or(one.as_str(), |(_, name)| name)
            .to_owned(),
        many => format!("{} archives", many.len()),
    };
    let first = found.paths.first().map_or("", String::as_str);
    let command = format!("fetchloom drop {}", crate::cli::shell_word(first));
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
/// back, as `verb` says, since data.lock still pins them: how to leave a sample out for good,
/// and where a person's own files stay out of the way.
fn warn_put_back(output: &Output, manifest: &Manifest, paths: &[String], verb: &str) {
    let Some(first) = paths.first() else {
        return;
    };
    let said = match paths.len() {
        1 => format!("{verb} {first}, which was deleted or moved, since data.lock pins it"),
        count => {
            let named = paths[..count.min(NAMED)].join(", ");
            let more = match count.saturating_sub(NAMED) {
                0 => String::new(),
                more => format!(" and {more} more"),
            };
            format!(
                "{verb} {count} files that were deleted or moved, since data.lock pins them: {named}{more}"
            )
        }
    };
    output.warn_with(
        &said,
        &format!(
            "to leave a file out for good, run `fetchloom drop {}`; keep files of your own in a folder outside {}/",
            crate::cli::shell_word(first),
            manifest.data_dir
        ),
    );
    for path in paths {
        output.detail(path);
    }
}

/// the files of `dataset` that were already at their places with the pinned bytes, which sync
/// took as placed: `linked` to the cache's copy now, or kept as they were. `-v` names each file.
fn tell_adopted(
    output: &Output,
    manifest: &Manifest,
    dataset: &str,
    paths: &[&String],
    linked: bool,
) {
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
    output.step(
        "Adopted",
        &format!("{} · {said}", plan::folder(manifest, dataset)),
    );
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
    for (dataset, planned) in &preview.by_dataset {
        tell_adoptions(output, manifest, dataset, &planned.adopt);
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
    warn_put_back(
        output,
        manifest,
        &placement.put_back,
        "a sync would put back",
    );
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

/// the files of `dataset` fetchloom did not place that already hold the pinned bytes, which a
/// sync would take as placed. `-v` names each file.
fn tell_adoptions(output: &Output, manifest: &Manifest, dataset: &str, files: &[String]) {
    let said = match files.len() {
        0 => return,
        1 => "1 file already holds the pinned bytes".to_owned(),
        count => format!("{count} files already hold the pinned bytes"),
    };
    output.step(
        "Would adopt",
        &format!("{} · {said}", plan::folder(manifest, dataset)),
    );
    for path in files {
        output.detail(path);
    }
}

/// a warning for the files fetchloom placed before `data_dir` changed, which stay where they are:
/// a sync deletes nothing outside the data folder.
pub(super) fn left_behind(output: &Output, manifest: &Manifest, left: Option<&LeftBehind>) {
    let Some(LeftBehind { folder, files }) = left else {
        return;
    };
    let Some(first) = files.first() else {
        return;
    };
    let before = format!("before data_dir changed to {}", manifest.data_dir);
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
                "`fetchloom status` lists it as extra; delete it when you no longer need it",
            ),
            [first, ..] => (
                format!(
                    "{folder} still holds {} files fetchloom did not place, such as {first}, so the folder stays",
                    retired.unplaced.len()
                ),
                "`fetchloom status` lists them as extra; delete them when you no longer need them",
            ),
        };
        output.warn_with(&said, help);
        for path in &retired.unplaced {
            output.detail(path);
        }
    }
}

/// a warning for each file a sync of `scope` left alone, with the command that replaces it.
fn warn_kept(output: &Output, manifest: &Manifest, scope: &Scope, placement: &Placement) {
    let force = forcing(scope);
    // one warning for each entry in the way, however many files it keeps out
    let mut blocking: BTreeMap<&str, (&Blocker, u64)> = BTreeMap::new();
    for kept in &placement.kept {
        if let Some(blocker) = &kept.blocker {
            blocking.entry(&blocker.path).or_insert((blocker, 0)).1 += 1;
        }
    }
    for (blocker, files) in blocking.into_values() {
        let (said, help) = in_the_way(blocker, files, &force);
        match &blocker.dataset {
            Some(dataset) => output.warn_with(&said, &left_alone(manifest, dataset)),
            None => output.warn_with(&said, &help),
        }
    }
    for kept in &placement.kept {
        let (message, help) = match kept.reason {
            Reason::Blocked => continue,
            Reason::Changed => (
                format!("{} has changed, so it stays", kept.path),
                format!("`{force}` replaces it"),
            ),
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

/// the command that replaces what a sync of `scope` kept: with the names of a sync of named
/// datasets, so it replaces nothing in the others.
fn forcing(scope: &Scope) -> String {
    match scope {
        Scope::All => "fetchloom sync --force".to_owned(),
        Scope::Only(names) => {
            let names: Vec<&str> = names.iter().map(String::as_str).collect();
            format!("fetchloom sync --force {}", names.join(" "))
        }
    }
}

/// what clears an entry in the way that is a file of `dataset`, or a folder of its files, which
/// a sync of other datasets leaves alone.
fn left_alone(manifest: &Manifest, dataset: &str) -> String {
    if manifest.datasets.contains_key(dataset) {
        format!(
            "it belongs to {dataset}: run `fetchloom sync {dataset}` first, or `fetchloom sync` for every dataset"
        )
    } else {
        format!(
            "it belongs to {dataset}, which data.toml no longer has: `fetchloom sync` syncs every dataset and removes it"
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

    /// a sync that puts back many files names the first five and counts the rest, and points
    /// at drop with the first, quoted for a shell.
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
        let paths: Vec<String> = (1..=7)
            .map(|n| format!("data/pets/my images/{n}.png"))
            .collect();
        warn_put_back(&output, &manifest, &paths, "put back");
        warn_put_back(&output, &manifest, &paths[..1], "a sync would put back");
        warn_put_back(&output, &manifest, &[], "put back");
        assert_eq!(
            output.warned(),
            [
                "put back 7 files that were deleted or moved, since data.lock pins them: data/pets/my images/1.png, data/pets/my images/2.png, data/pets/my images/3.png, data/pets/my images/4.png, data/pets/my images/5.png and 2 more; to leave a file out for good, run `fetchloom drop \"data/pets/my images/1.png\"`; keep files of your own in a folder outside data/",
                "a sync would put back data/pets/my images/1.png, which was deleted or moved, since data.lock pins it; to leave a file out for good, run `fetchloom drop \"data/pets/my images/1.png\"`; keep files of your own in a folder outside data/",
            ]
        );
    }
}
