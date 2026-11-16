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
    /// says what the steps a build, or with `check` a check, would run may take, on the cache's
    /// disk and, when they run in TMPDIR on another disk, on that one, asking first when that
    /// passes the free space above a disk's floor, and refusing below it, `--yes` or not.
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
        // one probe a run: where a step's folder is, and whether copies of what it reads take no
        // room on the cache's disk, being elsewhere or where that disk clones
        let probe = Staging::new(cache, &self.scratch).ok();
        let clones = probe.as_ref().is_some_and(|staging| staging.clones(cache));
        let elsewhere = probe
            .as_ref()
            .and_then(Staging::elsewhere)
            .map(Path::to_path_buf);
        drop(probe);
        let unknown: Vec<&str> = running
            .iter()
            .zip(&needs)
            .filter(|(_, needs)| needs.result.is_none())
            .map(|(step, _)| step.name.as_str())
            .collect();
        let mut disks = vec![Disk {
            holding: Holding::Cache,
            folder: cache.root().to_path_buf(),
            peak: space::peak(&needs, clones),
            floor: self.floor,
        }];
        if let Some(scratch) = elsewhere {
            disks.push(Disk {
                holding: Holding::Scratch,
                peak: space::step_peak(&needs),
                floor: space::floor(
                    self.context.settings.build_floor.value,
                    space::total(&scratch),
                ),
                folder: scratch,
            });
        }
        let mut short = None;
        for disk in &disks {
            let free = space::free(&disk.folder);
            let has = free.map_or_else(String::new, |free| {
                format!(", which has {} free", units::size(free))
            });
            self.context.output.step(
                "Needs",
                &format!(
                    "{} on the disk holding {}{has}",
                    taken(disk.peak, &unknown),
                    disk.holding.named()
                ),
            );
            match fits(disk.peak, disk.floor, free) {
                Fits::Yes => {}
                Fits::Maybe => {
                    short.get_or_insert((disk, free.unwrap_or_default()));
                }
                Fits::No => return Err(below(disk, free.unwrap_or(0))),
            }
        }
        let Some((disk, free)) = short.filter(|_| !self.context.yes) else {
            return Ok(());
        };
        let needed = disk.peak.saturating_add(disk.floor);
        let asked = match disk.holding {
            Holding::Cache => crate::cli::ask_room(
                &self.context.output,
                "this build",
                needed,
                free,
                &disk.folder,
            ),
            Holding::Scratch => crate::cli::ask_room_on(
                &self.context.output,
                "this build",
                (needed, free),
                &format!("TMPDIR's folder {}", units::path(&disk.folder)),
                "free space on that disk, or set TMPDIR to a folder on a bigger one, then run the same command again. add --yes to go on anyway, as on a disk that compresses what it stores",
            ),
        };
        asked.map_err(|error| {
            error.detail(format!(
                "its steps may take up to {}, and the build_floor setting keeps {} free",
                units::size(disk.peak),
                units::size(disk.floor)
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

/// a disk a build's steps take room on, and what they may take there.
struct Disk {
    holding: Holding,
    /// a folder on it: the cache's, or the one TMPDIR names.
    folder: std::path::PathBuf,
    /// the most the steps may take there.
    peak: u64,
    /// the least a step may leave free there.
    floor: u64,
}

/// what the disk holds that a build's steps take room on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Holding {
    /// the cache, where every download and result stays.
    Cache,
    /// the folder TMPDIR names, where the steps run when others may write the cache's tmp/.
    Scratch,
}

impl Holding {
    /// what the disk holds, as `the disk holding <this>` says it.
    fn named(self) -> &'static str {
        match self {
            Holding::Cache => "the cache",
            Holding::Scratch => "TMPDIR",
        }
    }
}

/// how a person lets steps leave less free than the `build_floor` setting asks.
pub(super) const LOWER_FLOOR: &str = "lower the floor with `fetchloom settings set build_floor <size>`, or for one run with FETCHLOOM_BUILD_FLOOR";

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

/// the error for `disk`, with `free` bytes, less than its floor: no answer gets past it, only room
/// made on that disk or a floor someone lowers on purpose.
fn below(disk: &Disk, free: u64) -> Error {
    let (at, elsewhere) = match disk.holding {
        Holding::Cache => ("the cache is at", "set FETCHLOOM_CACHE"),
        Holding::Scratch => ("TMPDIR names", "set TMPDIR"),
    };
    Error::new(
        Kind::LocalIo,
        format!(
            "the disk holding {} has {} free, less than the {} build_floor keeps free, so no step can run",
            disk.holding.named(),
            units::size(free),
            units::size(disk.floor)
        ),
    )
    .detail(format!("{at} {}", disk.folder.display()))
    .help(format!("free space on that disk, or {elsewhere} to a folder on a bigger one. to let steps leave less free, {LOWER_FLOOR}"))
}

/// what the `Needs` line says the steps take: up to `peak` bytes, besides what the steps `unknown`
/// write, whose result's size is not known yet, or only that when nothing else takes room.
fn taken(peak: u64, unknown: &[&str]) -> String {
    let writes = match unknown {
        [] => return format!("up to {}", units::size(peak)),
        [name] => format!("what {name} writes"),
        names => format!("what {} write", units::listed(names, "and")),
    };
    if peak == 0 {
        writes
    } else {
        format!("up to {}, besides {writes},", units::size(peak))
    }
}

#[cfg(test)]
mod tests {
    use super::{Disk, Fits, Holding, below, fits, taken};

    /// short of room above the floor, a person is asked, since the room errs on more and the floor
    /// still stops a step; below it nobody is asked, and a disk that does not say is never short.
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

    /// a disk below its floor names what it holds, where that is and what moves it: TMPDIR, for
    /// the disk a shared cache's steps run on.
    #[test]
    fn a_disk_below_its_floor_is_named_with_what_moves_it() {
        let disk = |holding| Disk {
            holding,
            folder: std::path::PathBuf::from("/scratch/me/tmp"),
            peak: 0,
            floor: 100,
        };
        let refused = below(&disk(Holding::Scratch), 99);
        assert_eq!(
            refused.message,
            "the disk holding TMPDIR has 99 B free, less than the 100 B build_floor keeps free, so no step can run"
        );
        assert_eq!(refused.details, ["TMPDIR names /scratch/me/tmp"]);
        assert!(
            refused
                .help
                .as_deref()
                .is_some_and(|help| help.starts_with("free space on that disk, or set TMPDIR to")),
            "{refused:?}"
        );
        let cache = below(&disk(Holding::Cache), 99);
        assert!(cache.message.starts_with("the disk holding the cache has"));
    }

    /// a step whose result's size is not known yet is named, alone when nothing else takes room,
    /// as for a recipe that reads no dataset and was never built.
    #[test]
    fn the_room_names_the_steps_whose_results_are_not_known_yet() {
        assert_eq!(taken(5, &[]), "up to 5 B");
        assert_eq!(
            taken(5, &["clean"]),
            "up to 5 B, besides what clean writes,"
        );
        assert_eq!(
            taken(5, &["thumbs", "index", "clean"]),
            "up to 5 B, besides what thumbs, index and clean write,"
        );
        assert_eq!(taken(0, &["sim"]), "what sim writes");
    }
}
