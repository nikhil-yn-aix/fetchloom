//! Which source a run takes, scored on what a probe learned about each.

use std::cmp::Ordering;

use crate::resume::ResumeRung;
use crate::seam::source::{Cost, SourceIdentity, SourceMetadata};

#[derive(Clone, Debug)]
pub struct Probed {
    pub index: usize,
    pub location: String,
    pub metadata: Option<SourceMetadata>,
    pub throughput: Option<u64>,
    pub time_to_first_byte_ms: Option<u64>,
    pub refused: bool,
    pub headroom: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Separator {
    Reachable,
    Ranges,
    ImmutableIdentity,
    Throughput,
    TimeToFirstByte,
    EgressCost,
    PolitenessHeadroom,
    ManifestOrder,
    TheOnlyOne,
}

impl Separator {
    #[must_use]
    pub fn because(self) -> &'static str {
        match self {
            Self::Reachable => "it answered and the alternatives did not",
            Self::Ranges => "it serves part of an object and the alternatives do not",
            Self::ImmutableIdentity => {
                "it states an identity that cannot change under the same name"
            }
            Self::Throughput => "this run has measured its host as the faster one",
            Self::TimeToFirstByte => "this run has measured its host as answering sooner",
            Self::EgressCost => {
                "the alternatives charge the requester for the bytes and it does not"
            }
            Self::PolitenessHeadroom => {
                "its host has more transfers left inside the politeness ceiling"
            }
            Self::ManifestOrder => "nothing measured separated the candidates, so the manifest did",
            Self::TheOnlyOne => "the manifest named one source",
        }
    }
}

pub fn score(candidates: &mut [Probed]) -> Separator {
    candidates.sort_by(|left, right| order(left, right).then(left.index.cmp(&right.index)));
    match candidates {
        [] | [_] => Separator::TheOnlyOne,
        [first, second, ..] => separator(first, second),
    }
}

fn order(left: &Probed, right: &Probed) -> Ordering {
    better(reachable(left), reachable(right))
        .then_with(|| better(ranges(left), ranges(right)))
        .then_with(|| better(immutable(left), immutable(right)))
        .then_with(|| measured(right.throughput, left.throughput))
        .then_with(|| measured(left.time_to_first_byte_ms, right.time_to_first_byte_ms))
        .then_with(|| cost(left, right))
        .then_with(|| right.headroom.cmp(&left.headroom))
}

fn separator(left: &Probed, right: &Probed) -> Separator {
    if reachable(left) != reachable(right) {
        return Separator::Reachable;
    }
    if ranges(left) != ranges(right) {
        return Separator::Ranges;
    }
    if immutable(left) != immutable(right) {
        return Separator::ImmutableIdentity;
    }
    if measured(right.throughput, left.throughput) != Ordering::Equal {
        return Separator::Throughput;
    }
    if measured(left.time_to_first_byte_ms, right.time_to_first_byte_ms) != Ordering::Equal {
        return Separator::TimeToFirstByte;
    }
    if cost(left, right) != Ordering::Equal {
        return Separator::EgressCost;
    }
    if left.headroom != right.headroom {
        return Separator::PolitenessHeadroom;
    }
    Separator::ManifestOrder
}

fn reachable(candidate: &Probed) -> bool {
    candidate.metadata.is_some()
}

fn ranges(candidate: &Probed) -> bool {
    candidate
        .metadata
        .as_ref()
        .is_some_and(|found| found.supports_ranges)
}

fn immutable(candidate: &Probed) -> bool {
    candidate.metadata.as_ref().is_some_and(|found| {
        matches!(
            rung_of(&found.identity),
            ResumeRung::ImmutableIdentity | ResumeRung::Outboard
        )
    })
}

fn rung_of(identity: &SourceIdentity) -> ResumeRung {
    match identity {
        SourceIdentity::ContentAddress(_) | SourceIdentity::ImmutableVersion(_) => {
            ResumeRung::ImmutableIdentity
        }
        SourceIdentity::StrongValidator(_) => ResumeRung::StrongValidator,
        SourceIdentity::WeakValidator(_) => ResumeRung::WeakValidator,
        SourceIdentity::None => ResumeRung::NoValidator,
    }
}

fn better(left: bool, right: bool) -> Ordering {
    right.cmp(&left)
}

fn measured(left: Option<u64>, right: Option<u64>) -> Ordering {
    match (left, right) {
        (Some(left), Some(right)) => left.cmp(&right),
        _ => Ordering::Equal,
    }
}

fn cost(left: &Probed, right: &Probed) -> Ordering {
    let (Some(left), Some(right)) = (stated(left), stated(right)) else {
        return Ordering::Equal;
    };
    left.cmp(&right)
}

fn stated(candidate: &Probed) -> Option<bool> {
    let found = candidate.metadata.as_ref()?;
    if found.cost.is_unknown() {
        return None;
    }
    Some(charges(found.cost))
}

fn charges(cost: Cost) -> bool {
    cost.egress_charged == Some(true) || cost.requester_pays == Some(true)
}
