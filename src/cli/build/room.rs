//! the room a build's steps may take on the cache's disk, said before any runs: a question when
//! that leaves less free than the `build_floor` setting, a refusal when less is free already.

use std::collections::BTreeSet;
use std::path::Path;

use super::{Building, Course, course, whole};
use crate::build::Staging;
use crate::error::{Error, Kind};
use crate::recipe::Step;
use crate::space::{self, Needs};
use crate::units;

impl Building<'_> {
    /// says what the steps a build, or with `check` a check, would run may take, asking first when
    /// that passes the free space above the floor, and refusing below the floor, `--yes` or not.
    pub(super) fn room(&self, check: bool) -> Result<(), Error> {
        let running = self.running(check)?;
        if running.is_empty() {
            return Ok(());
        }
        let cache = &self.work.cache;
        let mut fetched = BTreeSet::new();
        let needs: Vec<Needs> = running
            .iter()
            .map(|step| self.needs(step, &mut fetched))
            .collect();
        // one probe a run: copies of what a step reads take no room where the disk clones
        let clones = Staging::new(cache).is_ok_and(|staging| staging.clones());
        let peak = space::peak(&needs, clones);
        let unknown: Vec<&str> = running
            .iter()
            .zip(&needs)
            .filter(|(_, needs)| needs.result.is_none())
            .map(|(step, _)| step.name.as_str())
            .collect();
        let free = space::free(cache.root());
        let besides = besides(&unknown);
        let has = free.map_or_else(String::new, |free| {
            format!(", which has {} free", units::size(free))
        });
        self.context.output.step(
            "Needs",
            &format!(
                "up to {}{besides} on the disk holding the cache{has}",
                units::size(peak)
            ),
        );
        let needed = peak.saturating_add(self.floor);
        match fits(peak, self.floor, free) {
            Fits::Yes => return Ok(()),
            Fits::Maybe if self.context.yes => return Ok(()),
            Fits::Maybe => {}
            Fits::No => return Err(below(self.floor, free.unwrap_or(0), cache.root())),
        }
        crate::cli::ask_room(
            &self.context.output,
            "this build",
            needed,
            free.unwrap_or_default(),
            cache.root(),
        )
        .map_err(|error| {
            error.detail(format!(
                "its steps may take up to {}, and the build_floor setting keeps {} free",
                units::size(peak),
                units::size(self.floor)
            ))
        })
    }

    /// the steps a build, or with `check` a check, would run, as far as the cache tells: every one
    /// for a check, else those [`course`] finds unheld and unbuilt and all after, erring on more.
    fn running(&self, check: bool) -> Result<Vec<Step>, Error> {
        let cache = &self.work.cache;
        let mut running: Vec<Step> = Vec::new();
        for name in &self.recipes {
            let step = Step::read(&self.work.project.root, &self.manifest, &self.lock, name)?
                .ok_or_else(|| Error::new(Kind::ProjectInvalid, format!("{name} is no recipe")))?;
            let after = step
                .inputs
                .iter()
                .any(|read| running.iter().any(|ran| ran.name == read.name));
            let settled = !check
                && !after
                && step.key().is_some_and(|key| {
                    matches!(
                        course(&self.lock, name, key, |files| whole(cache, files), cache),
                        Course::Held(_) | Course::Built(_)
                    )
                });
            if !settled {
                running.push(step);
            }
        }
        Ok(running)
    }

    /// what `step` may take: the files of its fetched datasets the cache lacks, once a build as
    /// `fetched` records, what it reads in all, and its result as data.lock last pinned it.
    fn needs(&self, step: &Step, fetched: &mut BTreeSet<String>) -> Needs {
        let cache = &self.work.cache;
        let size = |files: &[crate::cache::Member]| {
            files
                .iter()
                .map(|file| file.size)
                .fold(0, u64::saturating_add)
        };
        let mut needs = Needs {
            result: self
                .lock
                .result(&step.name)
                .map(|pinned| size(&pinned.files)),
            ..Needs::default()
        };
        for read in &step.inputs {
            if read.made {
                let bytes = self
                    .lock
                    .result(&read.name)
                    .map_or(0, |pinned| size(&pinned.files));
                needs.inputs = needs.inputs.saturating_add(bytes);
                continue;
            }
            let Some(locked) = self.lock.get(&read.name) else {
                continue;
            };
            let counted = !fetched.insert(read.name.clone());
            for file in &locked.files {
                let lacks = |sha256, size| cache.find(sha256, Some(size)).is_none();
                let members = file
                    .unpacks_to
                    .and_then(|tree| cache.unpacked(&file.sha256, Some(&tree)));
                let (reads, download) = match (&members, file.unpacks_to) {
                    (Some(members), _) => {
                        let missing = size(
                            &members
                                .iter()
                                .filter(|member| lacks(&member.sha256, member.size))
                                .cloned()
                                .collect::<Vec<_>>(),
                        );
                        let archive = if missing > 0 && lacks(&file.sha256, file.size) {
                            file.size
                        } else {
                            0
                        };
                        (size(members), missing.saturating_add(archive))
                    }
                    // what an archive unpacks to is not known yet: as much as it holds again
                    (None, Some(_)) => {
                        let archive = if lacks(&file.sha256, file.size) {
                            file.size
                        } else {
                            0
                        };
                        (file.size, archive.saturating_add(file.size))
                    }
                    (None, None) => (
                        file.size,
                        if lacks(&file.sha256, file.size) {
                            file.size
                        } else {
                            0
                        },
                    ),
                };
                needs.inputs = needs.inputs.saturating_add(reads);
                if !counted {
                    needs.downloads = needs.downloads.saturating_add(download);
                }
            }
        }
        needs
    }
}

/// how the room steps may take, `peak`, fits on a disk with `free` bytes free, above `floor`, the
/// least a step may leave free.
#[derive(Debug, PartialEq, Eq)]
enum Fits {
    /// it fits, or the system does not say what is free.
    Yes,
    /// it may not: a person decides, as `--yes` does, since the room errs on more and the floor
    /// still stops a step that would pass it.
    Maybe,
    /// less than the floor is free already, so the floor would stop the first step at its first
    /// look, whatever anyone answers.
    No,
}

fn fits(peak: u64, floor: u64, free: Option<u64>) -> Fits {
    match free {
        Some(free) if free < floor => Fits::No,
        free if space::short(peak.saturating_add(floor), free).is_some() => Fits::Maybe,
        _ => Fits::Yes,
    }
}

/// the error for the cache's disk at `cache` with `free` bytes, less than `floor`: no answer gets
/// past it, only room made on that disk or a floor someone lowers on purpose.
fn below(floor: u64, free: u64, cache: &Path) -> Error {
    Error::new(
        Kind::LocalIo,
        format!(
            "the disk holding the cache has {} free, less than the {} build_floor keeps free, so no step can run",
            units::size(free),
            units::size(floor)
        ),
    )
    .detail(format!("the cache is at {}", cache.display()))
    .help("free space on that disk, or set FETCHLOOM_CACHE to a folder on a bigger one. to let steps leave less free, lower build_floor in the settings file or FETCHLOOM_BUILD_FLOOR")
}

/// what the `Needs` line adds for the steps `unknown` whose result's size is not known yet.
fn besides(unknown: &[&str]) -> String {
    match unknown {
        [] => String::new(),
        [name] => format!(", besides what {name} writes,"),
        names => format!(", besides what {} write,", units::listed(names, "and")),
    }
}

#[cfg(test)]
mod tests {
    use super::{Fits, besides, fits};

    /// a question a person may answer, since the room errs on more and the floor still stops a
    /// step; below the floor already, none, and a disk that does not say is never short.
    #[test]
    fn no_answer_lets_a_step_run_below_the_floor() {
        for (peak, free, fit) in [
            (10, Some(110), Fits::Yes),
            (10, Some(109), Fits::Maybe),
            (10, Some(100), Fits::Maybe),
            (10, Some(99), Fits::No),
            (0, Some(99), Fits::No),
            (10, None, Fits::Yes),
        ] {
            assert_eq!(fits(peak, 100, free), fit, "{peak} B of {free:?} free");
        }
    }

    #[test]
    fn what_the_steps_write_agrees_with_how_many_they_are() {
        assert_eq!(besides(&[]), "");
        assert_eq!(besides(&["clean"]), ", besides what clean writes,");
        assert_eq!(
            besides(&["thumbs", "index", "clean"]),
            ", besides what thumbs, index and clean write,"
        );
    }
}
