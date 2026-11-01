//! the lines a placement prints: what each dataset cost, what was removed, and every file left
//! alone or put back, with the reason.

use std::collections::BTreeMap;

use super::put::Tally;
use super::{Placement, Preview, Reason};
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
) {
    for (dataset, tally) in tallies {
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
    if tallies.values().any(|tally| tally.files > tally.links)
        && let Some(why) = placer.unlinked()
    {
        output.warn(&why.copied_because(&manifest.data_dir, "files were"));
    }
    warn_kept(output, placement);
    for path in &placement.restored {
        output.warn(&format!(
            "{path} is a linked file whose contents changed, so the pinned file was restored"
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
        tell_files("Would replace", dataset, &planned.replace);
    }
    let placement = &preview.placement;
    if placement.removed > 0 {
        output.step(
            "Would remove",
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
            "{path} is a linked file whose contents changed, so the pinned file would be restored"
        ));
    }
}

fn warn_kept(output: &Output, placement: &Placement) {
    for kept in &placement.kept {
        let message = match kept.reason {
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
