//! the lines a placement prints: what each dataset cost, what was removed, and every file left
//! alone or put back, with the reason.

use std::collections::BTreeMap;

use super::put::Tally;
use super::{Blocker, ChangedCopy, Entry, Placement, Preview, Reason};
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
    for (dataset, tally) in tallies
        .iter()
        .filter(|(dataset, _)| told != Some(**dataset))
    {
        let how = match (tally.links, tally.files - tally.links) {
            (_, 0) => "linked to the cache".to_owned(),
            (0, _) => format!("copied, {}", units::size(tally.copied)),
            (_, _) => format!("partly copied, {}", units::size(tally.copied)),
        };
        output.step(
            "Placed",
            &format!(
                "{} · {} {how}",
                plan::folder(manifest, dataset),
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
    if placement.removed > 0 {
        output.step(
            "Removed",
            &units::count(
                placement.removed,
                "file no dataset needs",
                "files no dataset needs",
            ),
        );
    }
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
    warn_kept(output, placement);
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

/// the lines a dry run prints for a placement it only planned: what each dataset would gain or
/// have replaced, what would be removed, and every file that would be left alone. `-v` names
/// each file.
pub(crate) fn tell_preview(output: &Output, manifest: &Manifest, preview: &Preview) {
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
    if placement.removed > 0 {
        output.step(
            "Would drop",
            &units::count(
                placement.removed,
                "file no dataset needs",
                "files no dataset needs",
            ),
        );
        for path in &preview.changes.remove {
            output.detail(path);
        }
    }
    warn_adopted_removed(output, &placement.removed_adopted, "would remove");
    warn_kept(output, placement);
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

fn warn_kept(output: &Output, placement: &Placement) {
    // one warning for each entry in the way, however many files it keeps out
    let mut blocking: BTreeMap<&str, (&Blocker, u64)> = BTreeMap::new();
    for kept in &placement.kept {
        if let Some(blocker) = &kept.blocker {
            blocking.entry(&blocker.path).or_insert((blocker, 0)).1 += 1;
        }
    }
    for (blocker, files) in blocking.into_values() {
        let (said, help) = in_the_way(blocker, files);
        output.warn_with(&said, help);
    }
    for kept in &placement.kept {
        let (message, help) = match kept.reason {
            Reason::Blocked => continue,
            Reason::Changed => (
                format!("{} has changed, so it stays", kept.path),
                "`fetchloom sync --force` replaces it",
            ),
            Reason::Shared => (
                format!(
                    "{} was edited, and the file is shared with other projects, so they see the edit too. it stays",
                    kept.path
                ),
                "`fetchloom sync --force` puts the original back",
            ),
            Reason::Foreign => (
                format!(
                    "{} is not a file fetchloom placed, and it differs from data.lock, so it stays",
                    kept.path
                ),
                "`fetchloom sync --force` replaces it",
            ),
            Reason::Unneeded => (
                format!(
                    "{} is no longer needed, but it has changed, so it stays",
                    kept.path
                ),
                "delete it if you do not need it",
            ),
        };
        output.warn_with(&message, help);
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

/// what an entry in the way of `files` files is, and what replaces it.
fn in_the_way(blocker: &Blocker, files: u64) -> (String, &'static str) {
    let path = &blocker.path;
    let what = entry_name(blocker.found);
    let force = if blocker.found == Entry::Link {
        "`fetchloom sync --force` replaces the link itself, never what it points to"
    } else {
        "`fetchloom sync --force` replaces it"
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
            "move what it holds elsewhere, then sync again",
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
