//! the lines a placement prints: what each dataset cost, what was removed, and every file left
//! alone or put back, with the reason.

use std::collections::BTreeMap;

use super::put::Tally;
use super::{Placement, Reason};
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
            (_, 0) => "linked, no extra disk".to_owned(),
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
    for kept in &placement.kept {
        let message = match kept.reason {
            Reason::Changed => format!(
                "{} was changed by hand, so it stays. `fetchloom sync --force` replaces it",
                kept.path
            ),
            Reason::Foreign => format!(
                "{} was not placed by fetchloom and holds other bytes, so it stays. `fetchloom sync --force` replaces it",
                kept.path
            ),
            Reason::Unneeded => format!(
                "{} is no longer needed, but it was changed by hand, so it stays. delete it if you do not need it",
                kept.path
            ),
        };
        output.warn(&message);
    }
    for path in &placement.restored {
        output.warn(&format!(
            "{path} was changed through a link in another project, so the locked bytes were put back"
        ));
    }
}
