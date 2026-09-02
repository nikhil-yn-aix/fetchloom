//! When one object is fetched as several ranges at once, and what a run says
//! when it is not.

use std::num::NonZeroU32;

use crate::limits::Limits;
use crate::seam::source::{ByteRange, SourceIdentity, SourceMetadata};

/// Why a run fetched an object whole where it would otherwise have split it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// The object is not long enough for the extra requests to pay for
    /// themselves.
    NotLarge,
    /// The source states nothing that fixes which bytes it is serving, so two
    /// spans may not come from one object.
    NotImmutable,
    /// The source cannot serve part of an object.
    NoRanges,
    /// Nothing this run measured says the host serves more with more streams.
    NoMeasuredGain,
}

impl Refused {
    /// Names the condition that failed, which is what the `degrade` reports.
    #[must_use]
    pub fn because(self) -> &'static str {
        match self {
            Self::NotLarge => "the object is not large enough to split",
            Self::NotImmutable => {
                "the source states no identity that cannot change under the same name"
            }
            Self::NoRanges => "the source does not serve ranges",
            Self::NoMeasuredGain => {
                "this run has not measured the host as serving more with more streams"
            }
        }
    }
}

/// Returns how many spans one object is fetched as, or the condition that
/// failed.
///
/// The four conditions are checked in the order they are cheapest to answer,
/// and the width is what the host's own controller permits: the adaptive
/// controller raised that count only because the host answered more requests
/// cleanly, which is the measurement that a second stream to this host is
/// worth opening.
///
/// # Errors
///
/// Returns the first condition that failed.
pub fn parts_for(
    metadata: &SourceMetadata,
    limits: &Limits,
    permitted: u32,
) -> Result<NonZeroU32, Refused> {
    if metadata.size.unwrap_or(0) <= limits.split_threshold {
        return Err(Refused::NotLarge);
    }
    if !immutable(&metadata.identity) {
        return Err(Refused::NotImmutable);
    }
    if !metadata.supports_ranges {
        return Err(Refused::NoRanges);
    }
    NonZeroU32::new(permitted)
        .filter(|width| width.get() > 1)
        .ok_or(Refused::NoMeasuredGain)
}

fn immutable(identity: &SourceIdentity) -> bool {
    matches!(
        identity,
        SourceIdentity::ContentAddress(_) | SourceIdentity::ImmutableVersion(_)
    )
}

/// Returns the spans a split asks for, which cover the missing bytes once, in
/// order, with no gap and no overlap.
#[must_use]
pub fn spans(start: u64, end: u64, parts: NonZeroU32) -> Vec<ByteRange> {
    let length = end.saturating_sub(start);
    if length == 0 {
        return Vec::new();
    }
    let wanted = u64::from(parts.get()).min(length);
    let each = length.div_ceil(wanted);
    let mut covered = Vec::new();
    let mut at = start;
    while at < end {
        let next = at.saturating_add(each).min(end);
        covered.push(ByteRange {
            start: at,
            end: next,
        });
        at = next;
    }
    covered
}
