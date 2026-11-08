//! the lines a placement prints: what each dataset cost, what was removed, and every file left
//! alone or put back, with the reason.

use std::collections::BTreeMap;

use super::put::Tally;
use super::{Blocker, Entry, Placement, Preview, Reason};
use crate::output::Output;
use crate::place::Placer;
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
    for (dataset, tally) in tallies
        .iter()
        .filter(|(dataset, _)| told != Some(**dataset))
    {
        let how = match (tally.links, tally.files - tally.links) {
            (_, 0) => "linked from the cache".to_owned(),
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
    if tallies.values().any(|tally| tally.unlinked > 0)
        && let Some(why) = placer.unlinked()
    {
        output.warn(&why.copied_because(&manifest.data_dir, "files were"));
    }
    warn_kept(output, placement);
    for path in &placement.restored {
        output.warn(&format!(
            "{path} was put back as pinned: it is linked to the cache's copy, which was changed. to edit a dataset's files, set copy = true for it in data.toml"
        ));
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
    warn_kept(output, placement);
    for path in &placement.restored {
        output.warn(&format!(
            "{path} would be put back as pinned: it is linked to the cache's copy, which was changed"
        ));
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
        output.warn(&in_the_way(blocker, files));
    }
    for kept in &placement.kept {
        let message = match kept.reason {
            Reason::Blocked => continue,
            Reason::Changed => format!(
                "{} has changed, so it stays. `fetchloom sync --force` replaces it",
                kept.path
            ),
            Reason::Shared => format!(
                "{} was edited, and the file is shared with other projects, so they see the edit too. it stays; `fetchloom sync --force` puts the original back",
                kept.path
            ),
            Reason::Foreign => format!(
                "{} is not recorded by fetchloom and has different contents, so it stays. `fetchloom sync --force` replaces it",
                kept.path
            ),
            Reason::Unneeded => format!(
                "{} is no longer needed, but it has changed, so it stays. delete it if you do not need it",
                kept.path
            ),
        };
        output.warn(&message);
    }
}

/// what an entry in the way of `files` files is, and what replaces it.
fn in_the_way(blocker: &Blocker, files: u64) -> String {
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
        return format!("{path} is {what} where data.lock needs a folder, so {pinned}. {force}");
    }
    if blocker.clearable {
        format!("{path} is {what} where data.lock places a file, so it stays. {force}")
    } else {
        format!(
            "{path} is a folder with files in it where data.lock places a file, so it stays, even with --force. move what it holds elsewhere, then sync again"
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
