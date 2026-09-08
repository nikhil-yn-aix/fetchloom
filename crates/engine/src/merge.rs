//! The three way compare between the record a run wrote, the destination as it
//! stands, and what the reference resolves to now.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::tree::{EntryPath, TreeEntry};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Resolution {
    Unchanged,
    TakeUpstream,
    KeepYours,
    StaysDeleted,
    Conflict,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Merged {
    pub path: EntryPath,
    pub resolution: Resolution,
}

#[must_use]
pub fn merge(base: &[TreeEntry], yours: &[TreeEntry], upstream: &[TreeEntry]) -> Vec<Merged> {
    let mut sides: BTreeMap<&EntryPath, Sides<'_>> = BTreeMap::new();
    for entry in base {
        sides.entry(entry.path()).or_default().base = Some(entry);
    }
    for entry in yours {
        sides.entry(entry.path()).or_default().yours = Some(entry);
    }
    for entry in upstream {
        sides.entry(entry.path()).or_default().upstream = Some(entry);
    }

    let mut decided: Vec<Merged> = sides
        .into_iter()
        .map(|(path, sides)| Merged {
            path: path.clone(),
            resolution: sides.resolution(),
        })
        .collect();
    decided.sort_by(|left, right| {
        crate::canonical::compare(left.path.as_bytes(), right.path.as_bytes())
    });
    decided
}

#[derive(Clone, Copy, Default)]
struct Sides<'a> {
    base: Option<&'a TreeEntry>,
    yours: Option<&'a TreeEntry>,
    upstream: Option<&'a TreeEntry>,
}

impl Sides<'_> {
    fn resolution(self) -> Resolution {
        let Self {
            base,
            yours,
            upstream,
        } = self;
        if yours == upstream {
            return match yours {
                Some(_) => Resolution::Unchanged,
                None => Resolution::StaysDeleted,
            };
        }
        match (base == yours, base == upstream) {
            (true, true) => Resolution::Unchanged,
            (true, false) => Resolution::TakeUpstream,
            (false, true) => match yours {
                Some(_) => Resolution::KeepYours,
                None => Resolution::StaysDeleted,
            },
            (false, false) => Resolution::Conflict,
        }
    }
}
