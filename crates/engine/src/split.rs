//! When one object is fetched as several ranges at once, and what a run says
//! when it is not.

use std::num::NonZeroU32;

use crate::limits::Limits;
use crate::seam::source::{ByteRange, SourceIdentity, SourceMetadata};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    NotLarge,
    NotImmutable,
    NoRanges,
    NoMeasuredGain,
}

impl Refused {
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

/// # Errors
/// `Refused`, saying which of the four conditions a split needs was not met:
/// the object is not large, its identity is not immutable, the source serves
/// no ranges, or no measurement said a split would gain anything.
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
