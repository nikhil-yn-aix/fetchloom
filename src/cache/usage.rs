//! where a cache's space goes: what registered projects need and how much of it they link, and
//! what gc frees now or once it is a day old. `fetchloom cache` reports it, and the weekly
//! reminder tells what gc frees from the same numbers, so the two cannot disagree.

use std::time::SystemTime;

use serde::Serialize;

use super::upkeep::{GRACE, Needed, Object, Scan};

/// where a cache's space goes, in payload bytes as gc counts them. they estimate what the disk
/// holds: the rest of a file's last block and a folder's own space are not counted.
#[derive(Debug, Default, PartialEq, Eq, Serialize)]
pub(crate) struct Usage {
    pub(crate) objects: u64,
    pub(crate) bytes: u64,
    /// what the registered projects need.
    pub(crate) needed: u64,
    /// of `needed`, what has another name outside the cache too, as a project's hardlink gives
    /// it, so deleting it from the cache would free nothing.
    pub(crate) linked: u64,
    /// what no registered project needs.
    pub(crate) unneeded: u64,
    /// of `unneeded`, what `cache gc` frees now: a day old, with no other name.
    pub(crate) freeable: u64,
    /// of `unneeded`, what gc frees once it is a day old.
    #[serde(skip)]
    pub(crate) young: u64,
    /// how many objects no registered project needs, which may all be empty files.
    #[serde(skip)]
    pub(crate) unneeded_objects: u64,
    /// what only builds read, which `cache gc --inputs` may free once recipes tell their inputs
    /// apart. none until then.
    pub(crate) inputs_only: Option<u64>,
}

impl Usage {
    /// what `scan`'s objects hold, against what `needed` keeps, as gc would count them at `now`.
    /// a windows listing does not count an object's names, so there each object is opened once.
    pub(crate) fn measure(scan: &Scan, needed: &Needed, now: SystemTime) -> Usage {
        let mut usage = Usage::default();
        for object in &scan.objects {
            usage.objects += 1;
            usage.bytes += object.size;
            let linked = has_other_names(object);
            if needed.objects.contains(&object.digest) {
                usage.needed += object.size;
                if linked {
                    usage.linked += object.size;
                }
                continue;
            }
            usage.unneeded += object.size;
            usage.unneeded_objects += 1;
            if linked {
                continue;
            }
            // as gc ages it: a time ahead of the clock, as FAT's rounding gives, is young
            if now
                .duration_since(object.changed)
                .is_ok_and(|age| age >= GRACE)
            {
                usage.freeable += object.size;
            } else {
                usage.young += object.size;
            }
        }
        usage
    }
}

/// whether `object` has a name outside the cache too, so removing it frees nothing.
fn has_other_names(object: &Object) -> bool {
    object.names.map_or_else(
        || crate::place::has_other_names(&object.path),
        |names| names > 1,
    )
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use super::*;
    use crate::digest;

    /// an object of `size` bytes, last changed at `changed`, with `names` names.
    fn object(name: &str, size: u64, changed: SystemTime, names: u64) -> Object {
        Object {
            digest: digest::sha256(name.as_bytes()),
            size,
            path: PathBuf::from(name),
            changed,
            names: Some(names),
        }
    }

    #[test]
    fn an_object_with_other_names_frees_nothing() {
        let now = SystemTime::now();
        let old = now - Duration::from_hours(48);
        let young = now - Duration::from_hours(1);
        let scan = Scan {
            objects: vec![
                object("unneeded", 1, old, 1),
                object("unneeded and linked by get", 10, old, 2),
                object("unneeded and young", 100, young, 1),
                object("needed and linked", 1_000, old, 2),
                object("needed", 10_000, young, 1),
            ],
            unreadable: Vec::new(),
        };
        let mut needed = Needed::default();
        for name in ["needed and linked", "needed"] {
            needed.objects.insert(digest::sha256(name.as_bytes()));
        }
        assert_eq!(
            Usage::measure(&scan, &needed, now),
            Usage {
                objects: 5,
                bytes: 11_111,
                needed: 11_000,
                linked: 1_000,
                unneeded: 111,
                freeable: 1,
                young: 100,
                unneeded_objects: 3,
                inputs_only: None,
            }
        );
    }
}
