//! what status, `lock --check` and sync tell of [`crate::lineage`]'s judgments: a warning once
//! for each dataset that may be stale, and for each input none can check, with its fields for
//! `--json`.

use crate::lineage::{self, Judged, Said};
use crate::output::{Output, Warning};

/// tells, once for each dataset of `judged`, the inputs whose files changed. the exit code stays
/// as it was.
pub(super) fn tell_stale(output: &Output, judged: &[Judged]) {
    tell(output, Warning::DerivedStale, lineage::stale(judged));
}

/// tells each input of `judged` whose state cannot be checked, with what would let it be.
pub(super) fn tell_unchecked(output: &Output, judged: &[Judged]) {
    tell(
        output,
        Warning::DerivedUnchecked,
        lineage::unchecked(judged),
    );
}

fn tell(output: &Output, kind: Warning, said: Vec<Said>) {
    for said in said {
        output.warn_facts(kind, &said.message, &said.help, &said.details, said.facts);
    }
}
