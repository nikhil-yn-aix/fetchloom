//! What a run needs from each volume before it moves a byte.

use std::collections::BTreeMap;

use crate::error::{Error, ErrorKind};
use crate::plan::PlanDisk;

/// Every volume a run needs room on, with what it needs there.
///
/// Requirements that land on one volume are summed, because the volume answers
/// once for all of them. The partial and the object it becomes are the
/// exception: publication is a rename rather than a copy, so on one volume they
/// are the same bytes twice and the larger of the two is what has to fit.
///
/// A requirement no one stated a size for is not a requirement, because a
/// length nobody stated is not a length.
#[must_use]
pub fn needed_per_volume(disk: &PlanDisk) -> BTreeMap<String, u64> {
    let mut needed: BTreeMap<String, u64> = BTreeMap::new();
    let renamed_into_place = disk.partial.volume == disk.cache.volume;
    let published = if renamed_into_place {
        disk.partial.bytes.max(disk.cache.bytes)
    } else {
        disk.partial.bytes
    };
    let counted = [
        (&disk.partial.volume, published),
        (
            &disk.cache.volume,
            if renamed_into_place {
                None
            } else {
                disk.cache.bytes
            },
        ),
        (&disk.staging.volume, disk.staging.bytes),
        (&disk.destination.volume, disk.destination.bytes),
    ];
    for (volume, bytes) in counted {
        if let Some(bytes) = bytes {
            *needed.entry(volume.clone()).or_default() += bytes;
        }
    }
    needed
}

/// Refuses a run whose volumes cannot hold what it needs, before it begins.
///
/// `available` answers with the free bytes of a volume, or `None` where this
/// build cannot ask, which is not a refusal.
///
/// # Errors
/// `resource.disk` naming the volume, what the run needs there, and what it
/// holds.
pub fn room_for(disk: &PlanDisk, available: &dyn Fn(&str) -> Option<u64>) -> Result<(), Error> {
    for (volume, needed) in needed_per_volume(disk) {
        let Some(free) = available(&volume) else {
            continue;
        };
        if free < needed {
            return Err(Error::new(
                ErrorKind::ResourceDisk,
                format!(
                    "free {} bytes on {volume} or send this somewhere else, because the run needs {needed} bytes there and {free} are left",
                    needed - free
                ),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::expect_used,
        reason = "test assertions, where the value that was not there is the message"
    )]

    use super::{needed_per_volume, room_for};
    use crate::error::ErrorKind;
    use crate::plan::{PlanDisk, VolumeRequirement};

    fn on(volume: &str, bytes: Option<u64>) -> VolumeRequirement {
        VolumeRequirement {
            volume: volume.to_owned(),
            bytes,
        }
    }

    fn disk(partial: u64, cache: u64, staging: Option<u64>, destination: Option<u64>) -> PlanDisk {
        PlanDisk {
            partial: on("one", Some(partial)),
            cache: on("one", Some(cache)),
            staging: on("one", staging),
            destination: on("two", destination),
        }
    }

    #[test]
    fn requirements_on_one_volume_are_summed_because_the_volume_answers_once() {
        let needed = needed_per_volume(&disk(1000, 2000, Some(500), Some(4000)));
        assert_eq!(
            needed.get("one"),
            Some(&2500),
            "the partial and the object it is renamed into were counted twice"
        );
        assert_eq!(needed.get("two"), Some(&4000));
    }

    #[test]
    fn a_partial_on_another_volume_needs_room_of_its_own() {
        let mut apart = disk(1000, 2000, None, None);
        apart.partial = on("three", Some(1000));
        let needed = needed_per_volume(&apart);
        assert_eq!(needed.get("three"), Some(&1000));
        assert_eq!(
            needed.get("one"),
            Some(&2000),
            "a partial that has to be copied across volumes was counted on the destination volume anyway"
        );
    }

    #[test]
    fn a_requirement_no_one_sized_is_not_counted() {
        let needed = needed_per_volume(&disk(1000, 2000, None, None));
        assert_eq!(needed.get("one"), Some(&2000));
        assert_eq!(
            needed.get("two"),
            None,
            "an unsized requirement was invented"
        );
    }

    #[test]
    fn a_volume_that_cannot_hold_what_the_run_needs_is_refused_before_it_begins() {
        let refused = room_for(&disk(1000, 2000, None, Some(10)), &|volume| {
            Some(if volume == "one" { 1999 } else { 1 << 30 })
        })
        .expect_err("a run was allowed onto a volume 1 byte short of what it needs");
        assert_eq!(refused.kind(), ErrorKind::ResourceDisk);
        assert!(
            refused.next_action().contains("one"),
            "the refusal does not name the volume: {}",
            refused.next_action()
        );
    }

    #[test]
    fn a_volume_with_exactly_enough_is_not_refused() {
        assert!(
            room_for(&disk(1000, 2000, None, None), &|_| Some(2000)).is_ok(),
            "a volume holding exactly what the run needs was refused"
        );
    }

    #[test]
    fn a_volume_this_build_cannot_ask_about_is_not_a_refusal() {
        assert!(room_for(&disk(1 << 40, 1 << 40, None, None), &|_| None).is_ok());
    }
}
