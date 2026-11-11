//! the room a build's steps may take on the disk holding the cache, said before any runs, with a
//! question when that disk has less free above the `build_floor` setting.

use std::collections::BTreeSet;

use super::{Building, Course, course, whole};
use crate::build::Staging;
use crate::error::{Error, Kind};
use crate::recipe::Step;
use crate::space::{self, Needs};
use crate::units;

impl Building<'_> {
    /// says how much the steps a build, or with `check` a check, would run may take on the disk
    /// holding the cache, and asks first when that disk has less free above the floor: `--yes`
    /// answers yes, and with no one to ask the build stops with `unconfirmed` before anything runs.
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
        let besides = match unknown.as_slice() {
            [] => String::new(),
            names => format!(", besides what {} writes,", super::listed(names)),
        };
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
        if self.context.yes || space::short(needed, free).is_none() {
            return Ok(());
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

    /// the steps a build, or with `check` a check, would run, as far as the cache tells before
    /// any does: every one for a check; else each that [`course`] does not find held or built
    /// before, and each after one that runs. a mirror may yet give a pinned result's files, so
    /// this errs on more.
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

    /// what `step` may take: the files of the fetched datasets it reads that the cache lacks,
    /// counted once in a build, as `fetched` records, what it reads in all, and the result
    /// data.lock pinned for it last.
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
