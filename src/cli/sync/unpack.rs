//! archives in a run: unpacked a few at a time on threads of their own, checked against
//! data.lock, and dropped from the cache once their files are in, unless something on this
//! machine still needs their own bytes, so a 50 GB zip costs 50 GB and not 100.

use std::cell::Cell;
use std::num::NonZero;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, PoisonError};
use std::time::{Duration, Instant};

use tokio::task::JoinSet;

use super::super::Context;
use super::super::cache::needed;
use super::draft::Draft;
use super::{Loop, job};
use crate::archive::{self, Unpacked};
use crate::cache::Cache;
use crate::digest::Sha256;
use crate::error::{Error, Kind};
use crate::net::Expect;
use crate::output::Output;
use crate::project::lock::{Lock, LockedFile};
use crate::units;

/// an archive the cache holds, to unpack on this machine.
pub(in crate::cli) struct Unpack {
    pub(in crate::cli) archive: Sha256,
    pub(in crate::cli) size: u64,
    pub(in crate::cli) label: String,
}

/// unpacks archives the cache holds and records what each unpacked to: what it holds, or none
/// for a file whose bytes are no archive after all. a few run at once, each with a bar on a
/// terminal, while off one plain lines tell the whole batch.
pub(in crate::cli) async fn unpack_all(
    context: &Context,
    cache: &Cache,
    archives: Vec<Unpack>,
) -> Result<Vec<Option<Unpacked>>, Error> {
    // each archive keeps a core decompressing and a few threads writing files
    let at_once = std::thread::available_parallelism()
        .map_or(1, NonZero::get)
        .div_ceil(4)
        .min(4);
    let failed = Arc::new(AtomicBool::new(false));
    let what = match archives.as_slice() {
        [only] => only.label.clone(),
        _ => units::count(archives.len() as u64, "archive", "archives"),
    };
    let sizes = archives.iter().map(|unpack| Some(unpack.size));
    let ticker = Arc::new(context.output.ticker("Unpacking", what, sizes));
    let mut results: Vec<Option<Unpacked>> = std::iter::repeat_with(|| None)
        .take(archives.len())
        .collect();
    let mut tasks = JoinSet::new();
    let mut queue = archives.into_iter().enumerate();
    loop {
        while tasks.len() < at_once
            && let Some((index, unpack)) = queue.next()
        {
            let cache = cache.clone();
            let stop = context.stop.clone();
            let failed = failed.clone();
            let ticker = ticker.clone();
            let bar = context.output.bar(format!("unpacking {}", unpack.label));
            bar.start(Some(unpack.size), 0);
            tasks.spawn_blocking(move || {
                let started = Instant::now();
                let read = Cell::new(0);
                let watch = |now: u64| {
                    let bytes = now.saturating_sub(read.replace(now));
                    bar.advance(bytes);
                    ticker.advance(bytes);
                    stop.is_asked() || failed.load(Ordering::Relaxed)
                };
                let unpacked = archive::unpack_cached(&cache, &unpack.archive, unpack.size, &watch);
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
                tell_unpacked(&context.output, &unpack.label, &unpacked, elapsed);
                results[index] = Some(unpacked);
            }
            Ok(None) => context.output.warn(&format!(
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
    Ok(results)
}

/// drops each archive's own bytes from the cache, now that its files are in, unless a project on
/// this machine, or `current`, still needs them. when a lock cannot be read, nothing is dropped.
pub(in crate::cli) fn drop_archives(cache: &Cache, archives: &[Sha256], current: Option<&Lock>) {
    if archives.is_empty() {
        return;
    }
    let needed = match needed(cache, current) {
        Ok((needed, _)) => needed,
        Err(err) => {
            tracing::debug!(err = %err.message, "kept unpacked archives, since a lock could not be read");
            return;
        }
    };
    for archive in archives
        .iter()
        .filter(|archive| !needed.objects.contains(archive))
    {
        // a later run downloads the archive again if it is ever needed, so failing costs disk
        if let Err(err) = cache.drop_object(archive) {
            tracing::debug!(%err, "could not drop an unpacked archive");
        }
    }
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

    /// unpacks archives the cache holds, remembers them for [`Loop::drop_unpacked`], and says each
    /// one's tree hash, or none for a file whose bytes are no archive after all.
    async fn unpack(&self, archives: Vec<Unpack>) -> Result<Vec<Option<Sha256>>, Error> {
        let digests: Vec<Sha256> = archives.iter().map(|unpack| unpack.archive).collect();
        let unpacked = unpack_all(self.context, &self.cache, archives).await?;
        let mut done = self.unpacked.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(digests
            .into_iter()
            .zip(unpacked)
            .map(|(digest, unpacked)| {
                unpacked.map(|unpacked| {
                    done.push(digest);
                    unpacked.tree
                })
            })
            .collect())
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

    /// drops from the cache each archive this run unpacked, now that its files are in, unless
    /// something on this machine still needs its own bytes.
    pub(in crate::cli) fn drop_unpacked(&self, lock: &Lock) {
        let unpacked =
            std::mem::take(&mut *self.unpacked.lock().unwrap_or_else(PoisonError::into_inner));
        drop_archives(&self.cache, &unpacked, Some(lock));
    }
}

fn tell_unpacked(output: &Output, label: &str, unpacked: &Unpacked, elapsed: Duration) {
    let bytes: u64 = unpacked.members.iter().map(|member| member.size).sum();
    output.step(
        "Unpacked",
        &format!(
            "{label} · {} · {} in {}",
            units::count(unpacked.members.len() as u64, "file", "files"),
            units::size(bytes),
            units::duration(elapsed)
        ),
    );
}
