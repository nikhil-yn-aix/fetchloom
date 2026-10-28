//! archives in a project run: unpacked a few at a time on threads of their own, checked against
//! data.lock, and dropped from the cache once their files are placed, unless a project on this
//! machine keeps them packed, so a 50 GB zip costs 50 GB and not 100.

use std::cell::Cell;
use std::collections::HashSet;
use std::num::NonZero;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, PoisonError};
use std::time::{Duration, Instant};

use tokio::task::JoinSet;

use super::draft::Draft;
use super::{Loop, job};
use crate::archive::{self, Unpacked};
use crate::cache::Cache;
use crate::digest::Sha256;
use crate::error::{Error, Kind};
use crate::net::Expect;
use crate::project::Project;
use crate::project::lock::{Lock, LockedFile};
use crate::units;

/// an archive the cache holds, to unpack on this machine.
pub(super) struct Unpack {
    pub(super) archive: Sha256,
    pub(super) size: u64,
    pub(super) label: String,
}

impl Loop<'_> {
    /// learns what every new archive in `drafts` unpacks to, and says which archives stay packed.
    pub(super) async fn unpack_drafts(&self, drafts: &mut [Draft]) -> Result<(), Error> {
        let mut slots = Vec::new();
        let mut unpacks = Vec::new();
        for (index, draft) in drafts.iter_mut().enumerate() {
            for (label, format) in draft.left_packed() {
                self.context.output.warn(&format!(
                    "{label} stays packed, since fetchloom does not unpack {format}"
                ));
            }
            for todo in draft.unknown_archives(&self.cache) {
                slots.push((index, todo.file));
                unpacks.push(Unpack {
                    archive: todo.archive,
                    size: todo.size,
                    label: todo.label,
                });
            }
        }
        let trees = self.unpack(unpacks).await?;
        for ((draft, file), tree) in slots.into_iter().zip(trees) {
            drafts[draft].unpacked(file, tree);
        }
        Ok(())
    }

    /// unpacks archives the cache holds and records what each unpacked to. says each one's tree
    /// hash, or none for a file whose bytes are no archive after all.
    pub(super) async fn unpack(&self, archives: Vec<Unpack>) -> Result<Vec<Option<Sha256>>, Error> {
        // each archive keeps a core decompressing and a few threads writing files
        let at_once = std::thread::available_parallelism()
            .map_or(1, NonZero::get)
            .div_ceil(4)
            .min(4);
        let failed = Arc::new(AtomicBool::new(false));
        let mut trees = vec![None; archives.len()];
        let mut tasks = JoinSet::new();
        let mut queue = archives.into_iter().enumerate();
        loop {
            while tasks.len() < at_once
                && let Some((index, unpack)) = queue.next()
            {
                let cache = self.cache.clone();
                let stop = self.context.stop.clone();
                let failed = failed.clone();
                let bar = self
                    .context
                    .output
                    .bar(format!("unpacking {}", unpack.label));
                bar.start(Some(unpack.size), 0);
                tasks.spawn_blocking(move || {
                    let started = Instant::now();
                    let read = Cell::new(0);
                    let watch = |now: u64| {
                        bar.advance(now.saturating_sub(read.replace(now)));
                        stop.is_asked() || failed.load(Ordering::Relaxed)
                    };
                    let unpacked =
                        archive::unpack_cached(&cache, &unpack.archive, unpack.size, &watch);
                    drop(bar);
                    (index, unpack, started.elapsed(), unpacked)
                });
            }
            let Some(joined) = tasks.join_next().await else {
                break;
            };
            let (index, unpack, elapsed, unpacked) = joined.map_err(|err| {
                Error::new(
                    Kind::LocalIo,
                    format!("unpacking stopped unexpectedly: {err}"),
                )
            })?;
            match unpacked {
                Ok(Some(unpacked)) => {
                    self.tell_unpacked(&unpack.label, &unpacked, elapsed);
                    self.unpacked
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(unpack.archive);
                    trees[index] = Some(unpacked.tree);
                }
                Ok(None) => self.context.output.warn(&format!(
                    "{} is no archive fetchloom can read, so it stays as it is",
                    unpack.label
                )),
                Err(err) => {
                    // the others stop at their next read
                    failed.store(true, Ordering::Relaxed);
                    return Err(err.detail(format!("in {}", unpack.label)));
                }
            }
        }
        Ok(trees)
    }

    /// unpacks archives the lock pins whose files this machine lacks, downloading those the
    /// cache does not hold, and checks that each unpacks to what the lock says.
    pub(super) async fn unpack_locked(
        &self,
        archives: &[(String, &LockedFile)],
    ) -> Result<(), Error> {
        if archives.is_empty() {
            return Ok(());
        }
        let jobs = archives
            .iter()
            .filter(|(_, file)| self.cache.find(&file.sha256, Some(file.size)).is_none())
            .map(|(dataset, file)| {
                job(
                    dataset,
                    &file.path,
                    &file.url,
                    Expect {
                        sha256: Some(file.sha256),
                        md5: file.md5,
                        size: Some(file.size),
                    },
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.download(jobs).await?;
        let unpacks = archives
            .iter()
            .map(|(dataset, file)| Unpack {
                archive: file.sha256,
                size: file.size,
                label: format!("{dataset}/{}", file.path),
            })
            .collect();
        let trees = self.unpack(unpacks).await?;
        for ((dataset, file), tree) in archives.iter().zip(trees) {
            if tree != file.unpacks_to {
                return Err(Error::new(
                    Kind::IntegrityMismatch,
                    format!(
                        "{dataset}/{} unpacked to other files than data.lock says",
                        file.path
                    ),
                )
                .help(format!(
                    "data.lock may come from another version of fetchloom. `fetchloom lock --upgrade {dataset}` pins what this one unpacks"
                )));
            }
        }
        Ok(())
    }

    /// drops from the cache each archive this run unpacked, now that its files are in the
    /// cache, unless a project on this machine keeps it packed.
    pub(in crate::cli) fn drop_unpacked(&self, lock: &Lock) {
        let unpacked =
            std::mem::take(&mut *self.unpacked.lock().unwrap_or_else(PoisonError::into_inner));
        if unpacked.is_empty() {
            return;
        }
        let packed = packed(&self.cache, lock);
        for archive in unpacked.iter().filter(|archive| !packed.contains(archive)) {
            // a later run downloads the archive again if it is ever needed, so failing costs disk
            if let Err(err) = self.cache.drop_object(archive) {
                tracing::debug!(%err, "could not drop an unpacked archive");
            }
        }
    }

    fn tell_unpacked(&self, label: &str, unpacked: &Unpacked, elapsed: Duration) {
        let bytes: u64 = unpacked.members.iter().map(|member| member.size).sum();
        self.context.output.step(
            "Unpacked",
            &format!(
                "{label} · {} · {} in {}",
                units::count(unpacked.members.len() as u64, "file", "files"),
                units::size(bytes),
                units::duration(elapsed)
            ),
        );
    }
}

/// every file a project on this machine places as it was downloaded, archives included.
fn packed(cache: &Cache, lock: &Lock) -> HashSet<Sha256> {
    let whole = |lock: &Lock| -> Vec<Sha256> {
        lock.datasets
            .iter()
            .flat_map(|dataset| &dataset.files)
            .filter(|file| file.unpacks_to.is_none())
            .map(|file| file.sha256)
            .collect()
    };
    let mut packed: HashSet<Sha256> = whole(lock).into_iter().collect();
    for root in cache.projects() {
        if let Ok(Some(other)) = (Project { root }).lock() {
            packed.extend(whole(&other));
        }
    }
    packed
}
