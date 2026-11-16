//! pinned files the cache lacks that hugging face's or kagglehub's cache, or a folder the `reuse`
//! setting names, holds: taken into the cache before any mirror or source is asked, as a clone
//! where the disk makes one, else as a copy, once their bytes hash to the pin. those folders are
//! only read. a file another tool keeps is never linked, so deleting it there leaves the cache
//! whole, and a link would seal that tool's file read-only. the caches read by default give what
//! their own layout names alone; any file of a pinned size is looked at only in folders a person
//! lists, since that walk costs every run that downloads something.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, PoisonError};

use crate::cache::{Cache, Copied, Sealed};
use crate::cli::Context;
use crate::digest::Sha256;
use crate::error::Error;
use crate::mirror::Wanted;
use crate::reuse::{self, Folder, Tool};
use crate::stop::Stop;
use crate::units;
use crate::work::Work;

/// copies made at once, as from the data folder.
const COPIES: usize = 8;

/// the folders a run reads copies from, and the pins it found in none of them.
#[derive(Default)]
pub(in crate::cli) struct Reused {
    folders: Vec<Folder>,
    /// whether a person listed the folders, so every file of a pinned size in them is looked at.
    listed: bool,
    /// each folder's hugging face repository folders, read once a run: none for a folder that is
    /// no hugging face cache.
    repositories: OnceLock<Vec<Vec<PathBuf>>>,
    /// pins this run looked for in every folder without finding them, which a later look skips.
    looked: Mutex<HashSet<Sha256>>,
}

/// a copy taken into the cache: from which folder and file, sealed and waiting to be published.
struct Taken {
    folder: usize,
    path: PathBuf,
    sealed: Sealed,
}

/// a copy now in the cache: its pin, where it came from, its size and whether it is a clone.
struct Got {
    digest: Sha256,
    folder: usize,
    path: PathBuf,
    size: u64,
    cloned: bool,
}

impl Reused {
    pub(in crate::cli) fn new(folders: Vec<Folder>, listed: bool) -> Reused {
        Reused {
            folders,
            listed,
            ..Reused::default()
        }
    }

    /// whether there is no folder to read.
    pub(in crate::cli) fn is_empty(&self) -> bool {
        self.folders.is_empty()
    }

    fn repositories(&self) -> &[Vec<PathBuf>] {
        self.repositories.get_or_init(|| {
            self.folders
                .iter()
                .map(|folder| match folder.tool {
                    Some(Tool::Kagglehub) => Vec::new(),
                    _ => reuse::repositories(&folder.path),
                })
                .collect()
        })
    }

    /// the tool that keeps folder `index`: any folder holding hugging face's repository folders
    /// is a hugging face cache, wherever it is.
    fn tool(&self, index: usize) -> Option<Tool> {
        self.folders[index]
            .tool
            .or_else(|| (!self.repositories()[index].is_empty()).then_some(Tool::HuggingFace))
    }

    /// the files that may hold `want` by where a tool keeps it, in order: hugging face's blob
    /// named by its SHA-256 in any repository, its snapshot entry for a hugging face pin, then
    /// kagglehub's file of a kaggle pin's version.
    fn shortcuts(&self, want: &Wanted) -> Vec<(usize, PathBuf)> {
        let known = want.link.as_deref().and_then(reuse::known);
        let mut found = Vec::new();
        for (index, folder) in self.folders.iter().enumerate() {
            let repositories = &self.repositories()[index];
            if !repositories.is_empty() {
                let paths = reuse::in_hugging_face(
                    &folder.path,
                    repositories,
                    &want.digest,
                    want.size,
                    known.as_ref(),
                );
                found.extend(paths.into_iter().map(|path| (index, path)));
            }
            if folder.tool == Some(Tool::Kagglehub)
                && let Some(known) = &known
                && let Some(path) = reuse::in_kagglehub(&folder.path, known, want.size)
            {
                found.push((index, path));
            }
        }
        found
    }

    fn looked(&self) -> std::sync::MutexGuard<'_, HashSet<Sha256>> {
        self.looked.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// takes into the cache each of `wanted` a folder holds, and returns the rest: first from
    /// where a tool keeps it, then, in folders a person listed, from any file of its size a scan
    /// finds, each once its bytes hash to the pin. fails at a stop or when the cache cannot be
    /// written.
    pub(in crate::cli) fn take(
        &self,
        context: &Context,
        cache: &Cache,
        wanted: Vec<Wanted>,
    ) -> Result<Vec<Wanted>, Error> {
        if self.folders.is_empty() || wanted.is_empty() {
            return Ok(wanted);
        }
        let ask: Vec<&Wanted> = {
            let looked = self.looked();
            wanted
                .iter()
                .filter(|want| !looked.contains(&want.digest))
                .collect()
        };
        if ask.is_empty() {
            return Ok(wanted);
        }
        let taken = self.take_all(context, cache, &ask).map_err(|err| {
            if err.kind() == io::ErrorKind::Interrupted {
                Stop::stopped()
            } else {
                Error::io("could not copy into the cache", &err)
            }
        })?;
        let got: HashSet<Sha256> = taken.iter().map(|got| got.digest).collect();
        self.looked().extend(
            ask.iter()
                .filter(|want| !got.contains(&want.digest))
                .map(|want| want.digest),
        );
        self.tell(context, &taken);
        Ok(wanted
            .into_iter()
            .filter(|want| !got.contains(&want.digest))
            .collect())
    }

    /// the pins of `wanted` the folders hold, found as [`Reused::take`] finds them but taking in
    /// nothing: each candidate's bytes are hashed and none is copied, as a dry run needs. fails
    /// at a stop.
    pub(in crate::cli) fn holding(
        &self,
        context: &Context,
        cache: &Cache,
        wanted: &[Wanted],
    ) -> Result<HashSet<Sha256>, Error> {
        let (threads, stop) = (context.threads.at_most(COPIES), &context.stop);
        let hashed = |path: &Path, size: u64| crate::cache::sha256_of(path, size, stop);
        let held = || -> io::Result<HashSet<Sha256>> {
            if self.folders.is_empty() || wanted.is_empty() {
                return Ok(HashSet::new());
            }
            let shortcut = crate::place::run_all(wanted, threads, |want| {
                for (_, path) in self.shortcuts(want) {
                    if hashed(&path, want.size)? == Some(want.digest) {
                        return Ok(true);
                    }
                }
                Ok::<_, io::Error>(false)
            });
            let mut held = HashSet::new();
            let mut rest = HashSet::new();
            for (want, found) in wanted.iter().zip(shortcut) {
                if found? {
                    held.insert(want.digest);
                } else {
                    rest.insert(want.digest);
                }
            }
            if self.listed && !rest.is_empty() {
                let sizes: HashSet<u64> = wanted
                    .iter()
                    .filter(|want| rest.contains(&want.digest))
                    .map(|want| want.size)
                    .collect();
                let found = reuse::scan(&self.folders, &sizes, cache.root(), stop)?;
                let candidates: Vec<(u64, &Path)> = found
                    .iter()
                    .flat_map(|(size, paths)| paths.iter().map(|(_, path)| (*size, path.as_path())))
                    .collect();
                let digests =
                    crate::place::run_all(&candidates, threads, |(size, path)| hashed(path, *size));
                for digest in digests {
                    held.extend(digest?.filter(|digest| rest.contains(digest)));
                }
            }
            Ok(held)
        };
        held().map_err(|err| {
            if err.kind() == io::ErrorKind::Interrupted {
                Stop::stopped()
            } else {
                Error::io("could not read a reuse folder", &err)
            }
        })
    }

    /// takes in what the shortcuts find of `ask`, then what a scan of listed folders finds of the
    /// rest.
    fn take_all(&self, context: &Context, cache: &Cache, ask: &[&Wanted]) -> io::Result<Vec<Got>> {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        let batch = if ask.len() > crate::volume::MOST_FLUSHED_ALONE {
            cache.batch()?
        } else {
            None
        };
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        let flush = batch.is_none();
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        let flush = true;
        let (threads, stop) = (context.threads.at_most(COPIES), &context.stop);
        let shortcut = crate::place::run_all(ask, threads, |want| {
            for (folder, path) in self.shortcuts(want) {
                if let Some(sealed) = sealed(cache, &path, want, flush, stop)? {
                    return Ok(Some(Taken {
                        folder,
                        path,
                        sealed,
                    }));
                }
            }
            Ok::<_, io::Error>(None)
        });
        let mut taken = Vec::new();
        let mut rest = Vec::new();
        for (want, found) in ask.iter().zip(shortcut) {
            match found? {
                Some(found) => taken.push(found),
                None => rest.push(*want),
            }
        }
        if self.listed && !rest.is_empty() {
            taken.extend(self.scanned(context, cache, &rest, flush)?);
        }
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        if let Some(batch) = batch {
            batch.flush()?;
        }
        taken
            .into_iter()
            .map(|found| {
                let cloned = found.sealed.cloned;
                let stored = found.sealed.publish(cache)?;
                if stored.new {
                    context.work.wrote(&stored.digest);
                }
                Ok(Got {
                    digest: stored.digest,
                    folder: found.folder,
                    path: found.path,
                    size: stored.size,
                    cloned,
                })
            })
            .collect()
    }

    /// takes in each of `rest` from a file of its size a scan of every folder finds. a size wanted
    /// once has each such file tried in turn; one many pins share has each file hashed once.
    fn scanned(
        &self,
        context: &Context,
        cache: &Cache,
        rest: &[&Wanted],
        flush: bool,
    ) -> io::Result<Vec<Taken>> {
        let stop = &context.stop;
        let sizes: HashSet<u64> = rest.iter().map(|want| want.size).collect();
        let found = reuse::scan(&self.folders, &sizes, cache.root(), stop)?;
        let mut by_size: HashMap<u64, Vec<&Wanted>> = HashMap::new();
        for want in rest {
            by_size.entry(want.size).or_default().push(want);
        }
        let threads = context.threads.at_most(COPIES);
        let mut tries: Vec<(&Wanted, Vec<(usize, PathBuf)>)> = Vec::new();
        for (size, wants) in by_size {
            let Some(candidates) = found.get(&size) else {
                continue;
            };
            if let [want] = wants.as_slice() {
                tries.push((want, candidates.clone()));
                continue;
            }
            let digests = crate::place::run_all(candidates, threads, |(_, path)| {
                crate::cache::sha256_of(path, size, stop)
            });
            let mut first: HashMap<Sha256, (usize, PathBuf)> = HashMap::new();
            for (candidate, digest) in candidates.iter().zip(digests) {
                if let Some(digest) = digest? {
                    first.entry(digest).or_insert_with(|| candidate.clone());
                }
            }
            for want in wants {
                if let Some(candidate) = first.remove(&want.digest) {
                    tries.push((want, vec![candidate]));
                }
            }
        }
        let taken = crate::place::run_all(&tries, threads, |(want, candidates)| {
            for (folder, path) in candidates {
                if let Some(sealed) = sealed(cache, path, want, flush, stop)? {
                    return Ok(Some(Taken {
                        folder: *folder,
                        path: path.clone(),
                        sealed,
                    }));
                }
            }
            Ok::<_, io::Error>(None)
        });
        taken.into_iter().filter_map(Result::transpose).collect()
    }

    /// says what came from each folder, and for a copy where the other copy stays, so a person
    /// may free it there.
    fn tell(&self, context: &Context, taken: &[Got]) {
        let (mut files, mut bytes) = (0, 0);
        // by folder and by clone or copy: files, bytes and where they were
        let mut told: HashMap<(usize, bool), (u64, u64, Vec<&Path>)> = HashMap::new();
        for got in taken {
            files += 1;
            bytes += got.size;
            let entry = told.entry((got.folder, got.cloned)).or_default();
            entry.0 += 1;
            entry.1 += got.size;
            entry.2.push(&got.path);
        }
        Work::add(&context.work.reused_objects, files);
        Work::add(&context.work.reused_bytes, bytes);
        let mut told: Vec<_> = told.into_iter().collect();
        told.sort_by_key(|(key, _)| *key);
        for ((index, cloned), (files, bytes, paths)) in told {
            let folder = &self.folders[index];
            let tool = self.tool(index);
            let from = tool.map_or_else(
                || folder.path.display().to_string(),
                |tool| tool.cache().to_owned(),
            );
            let verb = if cloned { "Cloned" } else { "Copied" };
            context.output.step(
                verb,
                &format!(
                    "{} \u{b7} {} from {from} into the cache",
                    units::count(files, "file", "files"),
                    units::size(bytes)
                ),
            );
            if !cloned {
                context
                    .output
                    .hint(&kept_there(&folder.path, tool, bytes, &paths));
            }
        }
    }
}

/// where the other copy of `bytes` copied from the folder at `root` stays, and how to free it there
/// once nothing else needs it: `hf cache rm` with each repository for hugging face's cache.
fn kept_there(root: &Path, tool: Option<Tool>, bytes: u64, paths: &[&Path]) -> String {
    let size = units::size(bytes);
    match tool {
        Some(Tool::HuggingFace) => {
            let repositories: BTreeSet<String> = paths
                .iter()
                .filter_map(|path| {
                    let first = path.strip_prefix(root).ok()?.iter().next()?;
                    reuse::removable(first)
                })
                .collect();
            let command = if repositories.is_empty() {
                "`hf cache rm`".to_owned()
            } else {
                format!(
                    "`hf cache rm {}`",
                    repositories.into_iter().collect::<Vec<_>>().join(" ")
                )
            };
            format!(
                "the hugging face cache at {} keeps its own copy of these {size}; {command} frees it there if nothing else needs it",
                root.display()
            )
        }
        Some(Tool::Kagglehub) => format!(
            "kagglehub's cache at {} keeps its own copy of these {size}; delete it there if nothing else needs it",
            root.display()
        ),
        None => {
            let first = paths.first().map_or(root, |path| path);
            format!(
                "{} keeps its own copy of these {size}, such as {}; delete it there if you no longer need it",
                root.display(),
                first.display()
            )
        }
    }
}

/// the object `want` pins, cloned or copied from the file at `path` when its bytes are the pinned
/// ones. a file of other bytes, unreadable or not plain is passed over.
fn sealed(
    cache: &Cache,
    path: &Path,
    want: &Wanted,
    flush: bool,
    stop: &Stop,
) -> io::Result<Option<Sealed>> {
    match cache.copy_in(path, &want.digest, want.size, flush, stop)? {
        Copied::Sealed(object) => Ok(Some(object)),
        Copied::OtherSize | Copied::OtherBytes | Copied::NotPlain => {
            tracing::debug!(path = %path.display(), "holds other bytes than data.lock pins");
            Ok(None)
        }
        Copied::Unread(err) => {
            tracing::debug!(path = %path.display(), %err, "could not be read");
            Ok(None)
        }
    }
}
