//! Deciding what a repair fetches, once the damage is known.

use std::ops::Range;

use crate::limits::Limits;
use crate::seam::source::ByteRange;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WholeReason {
    TooManySpans { spans: u64, allowed: u64 },
    PastTheShare { damaged: u64, allowed: u64 },
    NoRanges,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RepairPlan {
    Nothing,
    Spans(Vec<ByteRange>),
    Whole(WholeReason),
}

#[must_use]
pub fn plan_repair(
    damaged: &[Range<u64>],
    object_len: u64,
    supports_ranges: bool,
    limits: &Limits,
) -> RepairPlan {
    if damaged.is_empty() {
        return RepairPlan::Nothing;
    }
    if !supports_ranges {
        return RepairPlan::Whole(WholeReason::NoRanges);
    }

    let spans = damaged.len() as u64;
    if spans > limits.repair_spans {
        return RepairPlan::Whole(WholeReason::TooManySpans {
            spans,
            allowed: limits.repair_spans,
        });
    }

    let bytes: u64 = damaged.iter().map(|span| span.end - span.start).sum();
    let allowed = object_len / 100 * limits.repair_whole_percent;
    if bytes > allowed {
        return RepairPlan::Whole(WholeReason::PastTheShare {
            damaged: bytes,
            allowed,
        });
    }

    RepairPlan::Spans(
        damaged
            .iter()
            .map(|span| ByteRange {
                start: span.start,
                end: span.end,
            })
            .collect(),
    )
}

impl WholeReason {
    #[must_use]
    pub fn degradation(self, object_len: u64) -> (String, String, String) {
        let requested = "a repair fetching only the damaged ranges".to_owned();
        let used = format!("a fetch of all {object_len} bytes");
        let reason = match self {
            Self::TooManySpans { spans, allowed } => format!(
                "the damage came to {spans} separate ranges and a repair asks a source for at most {allowed}, past which the requests cost more than the bytes they save"
            ),
            Self::PastTheShare { damaged, allowed } => format!(
                "{damaged} bytes are damaged and a ranged repair is only cheaper up to {allowed}, which is the share of the object past which one request for all of it costs less"
            ),
            Self::NoRanges => {
                "the source does not serve part of an object, so the only request it answers is one for the whole of it".to_owned()
            }
        };
        (requested, used, reason)
    }
}
