//! archives in a run: unpacked a few at a time, checked against data.lock, and dropped from the
//! cache once their files are in unless still needed, so a 50 GB zip costs 50 GB and not 100.

use std::cell::Cell;
use std::num::NonZero;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, PoisonError};
use std::time::{Duration, Instant};

use tokio::task::JoinSet;

use super::super::Context;
use super::super::cache::needed_for_drop;
use super::draft::{self, Draft, Unit, Why};
use super::place::Placing;
use super::{Loop, job};
use crate::archive::{self, System, Unpacked, parts};
use crate::cache::{Cache, parts_key};
use crate::digest::Sha256;
use crate::error::{Error, Kind};
use crate::output::{Bar, Output, Ticker, Warning};
use crate::plan;
use crate::project::lock::archives::{self, Archive};
use crate::project::lock::{Lock, Locked, LockedFile};
use crate::project::manifest::Manifest;
use crate::source::local;
use crate::threads::Threads;
use crate::units;

/// an archive the cache holds, to unpack on this machine.
pub(in crate::cli) struct Unpack {
    /// its record's key: its SHA-256, or for a split archive, [`parts_key`] of its parts.
    pub(in crate::cli) archive: Sha256,
    /// a split archive's parts, each SHA-256 and size, in order; empty for one file.
    pub(in crate::cli) parts: Vec<(Sha256, u64)>,
    pub(in crate::cli) size: u64,
    pub(in crate::cli) label: String,
    /// the tree data.lock pins for it, the only one recorded once known.
    pub(in crate::cli) tree: Option<Sha256>,
    /// a first part alone with no tree pinned yet, maybe the start of an archive whose other parts
    /// are elsewhere, so bytes that do not unpack stay packed with a warning, not an error.
    pub(in crate::cli) lone: bool,
}

impl Unpack {
    /// an archive in one file.
    pub(in crate::cli) fn whole(
        archive: Sha256,
        size: u64,
        label: String,
        tree: Option<Sha256>,
    ) -> Unpack {
        Unpack {
            archive,
            parts: Vec::new(),
            size,
            label,
            tree,
            lone: false,
        }
    }

    /// an archive split into `parts`, each SHA-256 and size, in order.
    pub(in crate::cli) fn split(
        parts: Vec<(Sha256, u64)>,
        label: String,
        tree: Option<Sha256>,
    ) -> Unpack {
        Unpack {
            archive: parts_key(parts.iter().copied()),
            size: parts.iter().map(|(_, size)| size).sum(),
            lone: parts.len() == 1 && tree.is_none(),
            parts,
            label,
            tree,
        }
    }

    /// the archive data.lock pins as `archive`, labelled `label`.
    pub(in crate::cli) fn pinned(archive: &Archive<'_>, label: String) -> Unpack {
        let tree = archive.file.unpacks_to;
        if archive.parts.is_empty() {
            return Unpack::whole(archive.file.sha256, archive.file.size, label, tree);
        }
        let parts = archive.parts.iter().map(|part| (part.sha256, part.size));
        Unpack::split(parts.collect(), label, tree)
    }

    /// the cache objects holding its bytes.
    fn objects(&self) -> Vec<Sha256> {
        if self.parts.is_empty() {
            vec![self.archive]
        } else {
            self.parts.iter().map(|(part, _)| *part).collect()
        }
    }
}

/// unpacks archives the cache holds and records what each holds, or none for bytes that are no
/// archive after all; a few run at once, each with a bar on a terminal.
pub(in crate::cli) async fn unpack_all(
    context: &Context,
    cache: &Cache,
    archives: Vec<Unpack>,
) -> Result<Vec<Option<Unpacked>>, Error> {
    let mut results: Vec<Option<Unpacked>> = archives
        .iter()
        .map(|unpack| recorded(cache, unpack))
        .collect();
    let archives: Vec<(usize, Unpack)> = archives
        .into_iter()
        .enumerate()
        .filter(|(index, _)| results[*index].is_none())
        .collect();
    if archives.is_empty() {
        return Ok(results);
    }
    // each archive keeps a core decompressing and a few threads writing files
    let at_once = std::thread::available_parallelism()
        .map_or(1, NonZero::get)
        .div_ceil(4)
        .min(4);
    let (at_once, writers) = context.threads.archives(at_once, archives.len());
    let failed = Arc::new(AtomicBool::new(false));
    let what = match archives.as_slice() {
        [(_, only)] => only.label.clone(),
        _ => units::count(archives.len() as u64, "archive", "archives"),
    };
    let sizes = archives.iter().map(|(_, unpack)| Some(unpack.size));
    let ticker = Arc::new(context.output.ticker("Unpacking", what, sizes));
    let mut tasks = JoinSet::new();
    let mut queue = archives.into_iter();
    loop {
        while tasks.len() < at_once
            && let Some((index, unpack)) = queue.next()
        {
            let cache = cache.clone();
            let stop = context.stop.clone();
            let failed = failed.clone();
            let told = Told::new(&context.output, &unpack, ticker.clone());
            tasks.spawn_blocking(move || {
                let started = Instant::now();
                let watch = |now: u64| {
                    told.heard(now);
                    stop.is_asked() || failed.load(Ordering::Relaxed)
                };
                let tree = unpack.tree.as_ref();
                let unpacked = if unpack.parts.is_empty() {
                    let size = unpack.size;
                    archive::unpack_cached(&cache, &unpack.archive, size, tree, &watch, writers)
                } else {
                    archive::unpack_cached_parts(&cache, &unpack.parts, tree, &watch, writers)
                };
                drop(told);
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
                for digest in &unpacked.added {
                    context.work.wrote(digest);
                }
                tell_unpacked(&context.output, &unpack.label, &unpacked, elapsed);
                results[index] = Some(unpacked);
            }
            Ok(None) if unpack.lone => tell_lone(&context.output, &unpack.label),
            // the run says how a person unpacks it where it places the file
            Ok(None) if archive::unsupported(cache, &unpack.archive, unpack.size).is_some() => {}
            Ok(None) => context.output.warn(
                Warning::ArchiveUnreadable,
                &format!(
                    "{} is no archive fetchloom can read, so it stays as it is",
                    unpack.label
                ),
            ),
            Err(err) if unpack.lone && err.kind == Kind::SourceInvalid => {
                tell_lone(&context.output, &unpack.label);
            }
            Err(err) => {
                // the others stop at their next read
                failed.store(true, Ordering::Relaxed);
                return Err(err.detail(format!("in {}", unpack.label)));
            }
        }
    }
    Ok(results)
}

/// what an archive shows while it unpacks: the bytes read, then, while its files are still written,
/// a finishing bar with no total, so it never stands full with no time left.
struct Told {
    bar: Bar,
    ticker: Arc<Ticker>,
    label: String,
    size: u64,
    read: Cell<u64>,
}

impl Told {
    fn new(output: &Output, unpack: &Unpack, ticker: Arc<Ticker>) -> Told {
        let bar = output.bar("Unpacking", unpack.label.clone());
        bar.start(Some(unpack.size), 0);
        Told {
            bar,
            ticker,
            label: unpack.label.clone(),
            size: unpack.size,
            read: Cell::new(0),
        }
    }

    /// hears that `now` bytes of the archive were read, `size` once the readers are done.
    fn heard(&self, now: u64) {
        let before = self.read.replace(now);
        let bytes = now.saturating_sub(before);
        self.ticker.advance(bytes);
        if now < self.size {
            self.bar.advance(bytes);
        } else if before < self.size {
            // each call redraws: the verb goes first and the count before the total, so no frame
            // shows a full bar or a count against a total of nothing
            self.bar.message("Finishing", self.label.clone());
            self.bar.start(None, 0);
            self.bar.length(0);
        }
    }
}

/// what `unpack` unpacked to when another run recorded it while this one waited, with every file
/// in the cache, so it needs neither its own bytes nor a second unpacking.
fn recorded(cache: &Cache, unpack: &Unpack) -> Option<Unpacked> {
    let (tree, members) = cache.unpacked_with_tree(&unpack.archive)?;
    if unpack.tree.is_some_and(|wanted| wanted != tree) {
        return None;
    }
    members
        .iter()
        .all(|member| cache.find(&member.sha256, Some(member.size)).is_some())
        .then(|| Unpacked {
            members,
            tree,
            added: Vec::new(),
            skipped: archive::Skipped::default(),
        })
}

/// drops each archive's own bytes now that its files are in, unless a project here or `current`
/// needs them; a project whose data.toml is gone needs none, and an unreadable one keeps them all.
pub(in crate::cli) fn drop_archives(
    cache: &Cache,
    archives: &[Sha256],
    current: Option<&Lock>,
    threads: Threads,
) {
    if archives.is_empty() {
        return;
    }
    let stop = crate::stop::Stop::default();
    let needed = match needed_for_drop(cache, current, archives, threads, &stop) {
        Ok(needed) => needed,
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
    /// hints, before anything downloads, that `name` holds tar shards its `draft` would unpack;
    /// `added` means `add`'s command line, where `--no-extract` keeps them packed too.
    pub(super) fn hint_shards(&self, name: &str, draft: &Draft, added: bool) {
        if !self.context.output.shows_hints() {
            return;
        }
        if let Some(shards) = draft.shards() {
            let keep = if added {
                "--no-extract, or extract = false in data.toml"
            } else {
                "extract = false in data.toml"
            };
            self.context
                .output
                .hint(&shards.hint(name, keep, Some(name)));
        }
    }

    /// learns what every new archive in `drafts` unpacks to, and says which archives stay packed
    /// and how a person can unpack them.
    pub(super) async fn unpack_drafts(
        &self,
        manifest: &Manifest,
        drafts: &mut [Draft],
    ) -> Result<(), Error> {
        let mut slots = Vec::new();
        let mut unpacks = Vec::new();
        for (index, draft) in drafts.iter_mut().enumerate() {
            for (dataset, path, why) in draft.left_packed() {
                self.tell_packed(manifest, dataset, path, &why);
            }
            for (file, unpack) in draft.unknown_archives(&self.cache) {
                slots.push((index, file));
                unpacks.push(unpack);
            }
        }
        let trees = self.unpack(unpacks).await?;
        for ((draft, file), tree) in slots.into_iter().zip(trees) {
            drafts[draft].unpacked(file, tree);
        }
        for draft in drafts.iter() {
            self.tell_unportable(manifest, &draft.foreseen(&self.cache));
        }
        Ok(())
    }

    /// warns once for `locked`, a dataset this run pins, naming its files windows or a fat disk
    /// cannot store. they stay as they are, since this machine stores them.
    fn tell_unportable(&self, manifest: &Manifest, locked: &Locked) {
        const SHOWN: usize = 3;
        let members = |archive: &Archive<'_>| archive.recorded(&self.cache);
        let paths = plan::unportable(manifest, locked, &members);
        if paths.is_empty() {
            return;
        }
        let listed = units::some_of(&paths, SHOWN);
        let name = &locked.name;
        let said = match paths.len() {
            1 => {
                format!("{name} has a file whose name windows and fat disks cannot store: {listed}")
            }
            count => format!(
                "{name} has {} files whose names windows and fat disks cannot store: {listed}",
                units::grouped(count as u64)
            ),
        };
        self.context.output.warn_with(
            Warning::PlaceUnportable,
            &said,
            "a teammate on windows, or with the data on a fat or exfat disk, cannot place them; leave them out with exclude in data.toml if one needs to",
        );
    }

    /// the archives `placing` puts in the data folder that stay packed, with dataset and why: kept
    /// packed by name, or named as archives but of a format fetchloom does not unpack.
    pub(super) fn packed_in<'l>(
        &self,
        manifest: &Manifest,
        lock: &'l Lock,
        placing: &Placing<'_>,
    ) -> Vec<(&'l str, &'l str, Why)> {
        let mut found = Vec::new();
        for locked in lock
            .datasets
            .iter()
            .filter(|locked| self.scope.includes(&locked.name))
        {
            let paths = locked.files.iter().map(|file| file.path.as_str());
            let archives = draft::archives(paths, locked.extract);
            for (at, why) in archives.packed {
                found.push((locked.name.as_str(), &locked.files[at], Some(why)));
            }
            for unit in archives.unpack {
                if let Unit::Whole(at) = unit
                    && locked.files[at].unpacks_to.is_none()
                {
                    found.push((locked.name.as_str(), &locked.files[at], None));
                }
            }
        }
        if found.is_empty() {
            return Vec::new();
        }
        let places: Vec<String> = found
            .iter()
            .map(|(dataset, file, _)| format!("{}/{}", plan::folder(manifest, dataset), file.path))
            .collect();
        let put = placing.puts(&places.iter().map(String::as_str).collect());
        found
            .into_iter()
            .zip(&places)
            .filter(|(_, place)| put.contains(place.as_str()))
            .filter_map(|((dataset, file, why), _)| {
                let why = why.or_else(|| {
                    archive::unsupported(&self.cache, &file.sha256, file.size).map(Why::Bytes)
                })?;
                Some((dataset, file.path.as_str(), why))
            })
            .collect()
    }

    /// says the archive at `path`, a file of `dataset` that came inside its archive `outer`, stays
    /// packed, with how a person unpacks it beside itself; a run tells each once.
    pub(super) fn tell_nested(&self, manifest: &Manifest, dataset: &str, path: &str, outer: &str) {
        let folder = format!("{}/", plan::folder(manifest, dataset));
        let inside = path.strip_prefix(&folder).unwrap_or(path);
        let first = self
            .told_packed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert((dataset.to_owned(), inside.to_owned()));
        if !first {
            return;
        }
        let here = std::env::current_dir().unwrap_or_else(|_| self.project.root.clone());
        let placed = format!("{}{path}", crate::cli::up_to(&self.project.root, &here));
        let beside = placed
            .rsplit_once('/')
            .map_or_else(String::new, |(folder, _)| format!("{folder}/"));
        self.context.output.warn_with(
            Warning::ArchivePacked,
            &format!(
                "{dataset}/{inside} stays packed: it came inside {outer}, and fetchloom unpacks no archive inside another"
            ),
            &archive::unpack_inner(System::RUNNING, &placed, &beside),
        );
    }

    /// says the archive at `path` in `dataset` stays packed and why, and how to unpack it beside
    /// itself in the data folder, which git ignores; a run tells each once.
    pub(super) fn tell_packed(&self, manifest: &Manifest, dataset: &str, path: &str, why: &Why) {
        let first = self
            .told_packed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert((dataset.to_owned(), path.to_owned()));
        if !first {
            return;
        }
        let here = std::env::current_dir().unwrap_or_else(|_| self.project.root.clone());
        let up = crate::cli::up_to(&self.project.root, &here);
        let placed = format!("{up}{}/{path}", plan::folder(manifest, dataset));
        let beside = placed
            .rsplit_once('/')
            .map_or_else(String::new, |(folder, _)| format!("{folder}/"));
        let stop = "extract = false";
        let (said, help) = match why {
            Why::Format(format) => (
                format!("{dataset}/{path} stays packed, since fetchloom does not unpack {format}"),
                archive::unpack_elsewhere(System::RUNNING, &placed, &beside, stop),
            ),
            Why::Bytes(format) => (
                format!(
                    "{dataset}/{path} stays packed: its bytes are a {format}, which fetchloom does not unpack"
                ),
                archive::unpack_elsewhere(System::RUNNING, &placed, &beside, stop),
            ),
            Why::Volumes {
                format: "rar",
                count,
            } => (
                format!(
                    "{dataset}/{path} stays packed with {} of its rar, since fetchloom does not unpack rar",
                    units::count(*count as u64 - 1, "other volume", "other volumes")
                ),
                archive::unpack_elsewhere(System::RUNNING, &placed, &beside, stop),
            ),
            Why::Volumes { count, .. } => (
                format!(
                    "{dataset}/{path} stays packed with {} it spans, since fetchloom does not unpack a zip spanned over several files",
                    units::count(*count as u64 - 1, "other file", "other files")
                ),
                archive::join_elsewhere(System::RUNNING, &placed, &beside, stop),
            ),
            Why::Missing(missing) => (
                format!(
                    "{dataset}/{missing} is missing, so the parts of {} stay as they are",
                    parts::whole(missing).unwrap_or(missing)
                ),
                "fetchloom joins a split archive only from every one of its parts: check that select and exclude keep them all, or ask the dataset's authors for the missing one".to_owned(),
            ),
        };
        self.context
            .output
            .warn_with(Warning::ArchivePacked, &said, &help);
    }

    /// learns from the cache what every archive of `pinned`, imports pinned whole, unpacks to, and
    /// says which stay packed; one the cache lacks was imported elsewhere, as no link fetches it.
    pub(super) async fn unpack_pinned(
        &self,
        manifest: &Manifest,
        pinned: &mut [Locked],
    ) -> Result<(), Error> {
        let mut slots = Vec::new();
        let mut unpacks = Vec::new();
        for (index, locked) in pinned.iter_mut().enumerate() {
            let found = draft::archives(
                locked.files.iter().map(|file| file.path.as_str()),
                locked.extract,
            );
            for (at, why) in &found.packed {
                self.tell_packed(manifest, &locked.name, &locked.files[*at].path, why);
            }
            for unit in found.unpack {
                let at = unit.first();
                if locked.files[at].unpacks_to.is_some() {
                    continue;
                }
                let key = unit.key(&locked.files, |file| (file.sha256, file.size));
                if let Some(tree) = self.cache.unpacked_tree(&key) {
                    locked.files[at].unpacks_to = Some(tree);
                    continue;
                }
                let lacks = |index: &usize| {
                    let file = &locked.files[*index];
                    self.cache.find(&file.sha256, Some(file.size)).is_none()
                };
                if unit.files().iter().any(lacks) {
                    return Err(local::imported_elsewhere(
                        &locked.name,
                        &plan::folder(manifest, &locked.name),
                        false,
                    ));
                }
                let label = format!("{}/{}", locked.name, locked.files[at].path);
                let file = &locked.files[at];
                slots.push((index, at));
                unpacks.push(match &unit {
                    Unit::Whole(_) => Unpack::whole(file.sha256, file.size, label, None),
                    Unit::Split(set) => {
                        let parts = set.iter().map(|index| {
                            let part = &locked.files[*index];
                            (part.sha256, part.size)
                        });
                        Unpack::split(parts.collect(), label, None)
                    }
                });
            }
        }
        let trees = self.unpack(unpacks).await?;
        for ((index, at), tree) in slots.into_iter().zip(trees) {
            pinned[index].files[at].unpacks_to = tree;
        }
        for locked in pinned.iter() {
            self.tell_unportable(manifest, locked);
        }
        Ok(())
    }

    /// unpacks archives the cache holds, remembers their bytes for [`Loop::drop_unpacked`], and
    /// says each one's tree hash, or none for a file whose bytes are no archive after all.
    async fn unpack(&self, archives: Vec<Unpack>) -> Result<Vec<Option<Sha256>>, Error> {
        // most syncs unpack nothing, and need no progress ticker for it
        if archives.is_empty() {
            return Ok(Vec::new());
        }
        let objects: Vec<Vec<Sha256>> = archives.iter().map(Unpack::objects).collect();
        let unpacked = unpack_all(self.context, &self.cache, archives).await?;
        let mut done = self.unpacked.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(objects
            .into_iter()
            .zip(unpacked)
            .map(|(objects, unpacked)| {
                unpacked.map(|unpacked| {
                    done.extend(objects);
                    unpacked.tree
                })
            })
            .collect())
    }

    /// unpacks the pinned archives whose files this machine lacks, downloading what the cache
    /// lacks, and checks each against the lock. one that differs is hashed first, so changed bytes
    /// download again; only an intact archive that unpacks to other files is an error.
    pub(super) async fn unpack_locked(
        &self,
        manifest: &Manifest,
        lock: &Lock,
        archives: &[(String, Archive<'_>)],
    ) -> Result<(), Error> {
        let intact = |dataset: &str, archive: &Archive<'_>| {
            let mut whole = true;
            for file in archive.files() {
                let checked = self.cache.verify(&file.sha256).map_err(|err| {
                    Error::io(
                        format_args!("could not check {dataset}/{}", file.path),
                        &err,
                    )
                })?;
                if !checked {
                    self.context.output.warn(
                        Warning::CacheChanged,
                        &format!(
                            "{dataset}/{} changed in the cache, so it is downloaded again",
                            file.path
                        ),
                    );
                }
                whole &= checked;
            }
            Ok::<bool, Error>(whole)
        };
        let checked = match self.unpack_checked(manifest, lock, archives).await {
            Err(error) if error.kind == Kind::SourceInvalid => {
                // a checksum can fail before a tree exists to compare with the lock
                let mut changed = false;
                for (dataset, archive) in archives {
                    changed |= !intact(dataset, archive)?;
                }
                if !changed {
                    return Err(error);
                }
                self.unpack_checked(manifest, lock, archives).await?
            }
            result => result?,
        };
        let mut damaged = Vec::new();
        for (dataset, archive) in checked {
            if intact(&dataset, &archive)? {
                return Err(other_files(&dataset, archive.file));
            }
            damaged.push((dataset, archive));
        }
        match self.unpack_checked(manifest, lock, &damaged).await?.first() {
            Some((dataset, archive)) => Err(other_files(dataset, archive.file)),
            None => Ok(()),
        }
    }

    /// downloads the archives' files the cache lacks and unpacks every archive, and returns
    /// those that unpacked to other files than the lock says.
    async fn unpack_checked<'f>(
        &self,
        manifest: &Manifest,
        lock: &Lock,
        archives: &[(String, Archive<'f>)],
    ) -> Result<Vec<(String, Archive<'f>)>, Error> {
        if archives.is_empty() {
            return Ok(Vec::new());
        }
        let lacking = || {
            archives.iter().flat_map(|(dataset, archive)| {
                let files = archive.files().into_iter();
                files
                    .filter(|file| self.cache.find(&file.sha256, Some(file.size)).is_none())
                    .map(move |file| (dataset, file))
            })
        };
        // before any link is read, so one that cannot be followed stops no mirror
        let folder = super::fetch::data_folder::DataFolder::default();
        super::fetch::mirrors::fill(self.context, &self.net, &self.cache, &folder, || {
            lacking()
                .map(|(dataset, file)| crate::mirror::Wanted {
                    digest: file.sha256,
                    size: file.size,
                    label: format!("{dataset}/{}", file.path),
                })
                .collect()
        })
        .await
        .map_err(|error| self.as_left(error))?;
        let lacked: Vec<(&str, &LockedFile)> = lacking()
            .map(|(dataset, file)| (dataset.as_str(), file))
            .collect();
        self.refuse_unfetchable(manifest, &lacked)?;
        let jobs = lacking()
            .map(|(dataset, file)| job(manifest, lock, dataset, file))
            .collect::<Result<Vec<_>, _>>()?;
        self.download(jobs).await?;
        let unpacks = archives
            .iter()
            .map(|(dataset, archive)| {
                Unpack::pinned(archive, format!("{dataset}/{}", archive.file.path))
            })
            .collect();
        let trees = self.unpack(unpacks).await?;
        Ok(archives
            .iter()
            .zip(trees)
            .filter(|((_, archive), tree)| *tree != archive.file.unpacks_to)
            .map(|(archive, _)| archive.clone())
            .collect())
    }

    /// drops archives whose members are now cached, each part of a split one, records reused after
    /// a failed run included, unless something here still needs their packed bytes.
    pub(in crate::cli) fn drop_unpacked(&self, lock: &Lock) {
        let mut unpacked =
            std::mem::take(&mut *self.unpacked.lock().unwrap_or_else(PoisonError::into_inner));
        for dataset in &lock.datasets {
            for pinned in archives::pinned(&dataset.files) {
                if let archives::Pinned::Archive(archive) = pinned {
                    unpacked.extend(archive.files().into_iter().map(|file| file.sha256));
                }
            }
        }
        unpacked.sort_unstable();
        unpacked.dedup();
        drop_archives(&self.cache, &unpacked, Some(lock), self.context.threads);
    }
}

/// an intact archive that unpacks to other files than data.lock says, as one pinned by another
/// version of fetchloom can.
fn other_files(dataset: &str, file: &LockedFile) -> Error {
    Error::new(
        Kind::IntegrityMismatch,
        format!(
            "{dataset}/{} unpacked to other files than data.lock says",
            file.path
        ),
    )
    .help(format!(
        "data.lock may come from another version of fetchloom. if what this one unpacks is intended, run `{}`",
        crate::cli::project::command(&format!("lock --upgrade {dataset}"))
    ))
}

/// says that the lone part `label` stays packed, as its bytes are no archive on their own.
fn tell_lone(output: &Output, label: &str) {
    output.warn_with(
        Warning::ArchivePacked,
        &format!("{label} stays packed, since it does not unpack on its own"),
        "it may be the first part of an archive split into several files, which fetchloom joins only inside one dataset, such as a folder or a record that lists every part",
    );
}

/// says what `label` unpacked to, and what it held that a system writes by itself, which was
/// left out as a folder import leaves it out.
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
    let skipped = &unpacked.skipped;
    if let Some(first) = &skipped.first {
        let said = match skipped.files {
            1 => format!("{first} in {label}, which the system makes"),
            files => format!(
                "{first} and {} more the system makes in {label}",
                units::grouped(files - 1)
            ),
        };
        output.step("Skipped", &said);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::tests::store;
    use crate::digest;
    use crate::net::Offline;
    use crate::output::{Level, Style};
    use crate::project::Project;
    use crate::project::lock::Locked;
    use crate::source::recorded::block_on;

    fn context() -> Context {
        let mut context = Context::new(Output::new(
            Style {
                color: false,
                unicode: false,
                interactive: false,
                watched: false,
                links: false,
                accessible: false,
            },
            Level::Quiet,
            false,
            false,
        ));
        context.offline = Offline::Flag;
        context.threads = crate::threads::Threads::read(&|_| Some("1".into())).unwrap();
        context
    }

    fn tar(bytes: &[u8]) -> Vec<u8> {
        let mut builder = ::tar::Builder::new(Vec::new());
        let mut header = ::tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, "member.txt", bytes)
            .unwrap();
        builder.into_inner().unwrap()
    }

    fn lock_for(file: LockedFile) -> Lock {
        Lock {
            datasets: vec![Locked {
                name: "set".to_owned(),
                reference: "https://example.org/set.tar".to_owned(),
                spec: String::new(),
                extract: true,
                resolved: "https://example.org/set.tar".to_owned(),
                title: None,
                license: None,
                doi: None,
                access: crate::source::Access::Unknown,
                timestamp: "2026-01-01T00:00:00Z".to_owned(),
                files: vec![file],
            }],
            ..Lock::default()
        }
    }

    /// once the readers are done the bar fills toward nothing, so it never stands full with no time
    /// left, and stays so until the files are in the cache.
    #[test]
    fn a_bar_read_to_its_end_is_finishing_until_the_files_are_in() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path().join("cache")).unwrap();
        let output = Output::new(
            Style {
                color: false,
                unicode: false,
                interactive: true,
                watched: false,
                links: false,
                accessible: false,
            },
            Level::Normal,
            false,
            false,
        );
        // larger than one read, so the bar hears the archive part way first
        let member = b"0123456789abcdef".repeat(40_000);
        let stored = store(&cache, &tar(&member));
        let unpack = Unpack::whole(stored.digest, stored.size, "set/set.tar".to_owned(), None);
        let ticker = Arc::new(output.ticker("Unpacking", unpack.label.clone(), [None]));
        let told = Told::new(&output, &unpack, ticker);
        let in_cache = || {
            cache
                .find(&digest::sha256(&member), Some(member.len() as u64))
                .is_some()
        };
        let heard = std::cell::RefCell::new(Vec::new());
        let watch = |now: u64| {
            told.heard(now);
            heard.borrow_mut().push((now, told.bar.total(), in_cache()));
            false
        };
        let unpacked =
            archive::unpack_cached(&cache, &stored.digest, stored.size, None, &watch, Some(2))
                .unwrap()
                .unwrap();
        let heard = heard.into_inner();
        assert!(
            heard
                .iter()
                .any(|&(now, total, _)| now < stored.size && total == Some(stored.size)),
            "{heard:?}"
        );
        assert_eq!(heard.last(), Some(&(stored.size, Some(0), false)));
        assert!(in_cache());
        assert_eq!(unpacked.members.len(), 1);
        assert_eq!(told.bar.total(), Some(0));
        assert_eq!(told.bar.told().as_deref(), Some("set/set.tar"));
    }

    #[test]
    fn mixed_batches_keep_input_order_and_store_only_real_archive_members() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path().join("cache")).unwrap();
        let context = context();
        let plain = store(&cache, b"not an archive");
        let first = store(&cache, &tar(b"first"));
        let second = store(&cache, &tar(b"second"));
        let archives = [
            ("plain.zip", &plain),
            ("first.tar", &first),
            ("second.tar", &second),
        ]
        .into_iter()
        .map(|(label, stored)| Unpack::whole(stored.digest, stored.size, label.to_owned(), None))
        .collect();
        let unpacked = block_on(unpack_all(&context, &cache, archives)).unwrap();
        assert_eq!(unpacked.len(), 3);
        assert!(unpacked[0].is_none());
        for (index, bytes) in [(1, b"first".as_slice()), (2, b"second")] {
            let unpacked = unpacked[index].as_ref().unwrap();
            assert_eq!(unpacked.members.len(), 1);
            assert_eq!(unpacked.members[0].path, "member.txt");
            assert_eq!(unpacked.members[0].sha256, digest::sha256(bytes));
            assert_eq!(
                std::fs::read(cache.object_path(&unpacked.members[0].sha256)).unwrap(),
                bytes
            );
        }
        assert_eq!(context.work.counts().objects_written, 2);
        assert!(cache.unpacked(&plain.digest, None).is_none());
        assert_eq!(
            cache.unpacked(&first.digest, None).unwrap()[0].sha256,
            digest::sha256(b"first")
        );
    }

    /// a first part alone may be only the start of an archive whose other parts are elsewhere;
    /// parts together that do not unpack are a damaged archive, as one file would be.
    #[test]
    fn a_lone_first_part_that_does_not_unpack_stays_packed() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path().join("cache")).unwrap();
        let whole = tar(&b"a member much longer than one tar block, ".repeat(64));
        let half = store(&cache, &whole[..whole.len() / 2]);
        let first = (half.digest, half.size);
        let lone = Unpack::split(vec![first], "set/set.tar.001".to_owned(), None);
        let unpacked = block_on(unpack_all(&context(), &cache, vec![lone])).unwrap();
        assert!(unpacked[0].is_none(), "stays packed");
        let wrong = store(&cache, b"not the rest of it");
        let parts = vec![first, (wrong.digest, wrong.size)];
        let two = Unpack::split(parts, "set/set.tar.001".to_owned(), None);
        let error = block_on(unpack_all(&context(), &cache, vec![two])).unwrap_err();
        assert_eq!(error.kind, Kind::SourceInvalid, "{}", error.message);
        let pinned = Unpack::split(vec![first], "set/set.tar.001".to_owned(), Some(half.digest));
        let error = block_on(unpack_all(&context(), &cache, vec![pinned])).unwrap_err();
        assert_eq!(error.kind, Kind::SourceInvalid, "a pinned tree must unpack");
    }

    #[test]
    fn an_archive_unpacked_meanwhile_is_not_unpacked_again() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path().join("cache")).unwrap();
        let stored = store(&cache, &tar(b"member"));
        let unpack = || Unpack::whole(stored.digest, stored.size, "set/set.tar".to_owned(), None);
        // another run unpacks the archive, then drops its bytes as a sync does once its files
        // are in, while this run waited for the download
        let theirs = block_on(unpack_all(&context(), &cache, vec![unpack()])).unwrap();
        let theirs = theirs[0].as_ref().unwrap();
        drop_archives(
            &cache,
            &[stored.digest],
            Some(&Lock::default()),
            Threads::default(),
        );
        assert!(cache.find(&stored.digest, None).is_none());
        let context = context();
        let ours = block_on(unpack_all(&context, &cache, vec![unpack()])).unwrap();
        let ours = ours[0].as_ref().unwrap();
        assert_eq!(ours.tree, theirs.tree);
        assert_eq!(ours.members.len(), 1);
        assert_eq!(ours.members[0].sha256, digest::sha256(b"member"));
        assert_eq!(ours.added, Vec::new());
        assert_eq!(context.work.counts().objects_written, 0);
    }

    #[test]
    fn a_missing_archive_reports_which_batch_item_failed() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path().join("cache")).unwrap();
        let context = context();
        let error = block_on(unpack_all(
            &context,
            &cache,
            vec![Unpack::whole(
                digest::sha256(b"missing"),
                7,
                "set/missing.tar".to_owned(),
                None,
            )],
        ))
        .unwrap_err();
        assert_eq!(error.kind, Kind::LocalIo);
        assert_eq!(error.details, ["in set/missing.tar"]);
        assert!(error.help.unwrap().contains("download"));
        assert_eq!(context.work.counts().objects_written, 0);
    }

    #[test]
    fn intact_archives_with_a_different_pinned_tree_require_an_explicit_upgrade() {
        let dir = tempfile::tempdir().unwrap();
        let project = Project {
            root: dir.path().to_path_buf(),
        };
        let cache = Cache::open(dir.path().join("cache")).unwrap();
        let stored = store(&cache, &tar(b"actual member"));
        let lock = lock_for(LockedFile {
            path: "set.tar".to_owned(),
            size: stored.size,
            sha256: stored.digest,
            md5: None,
            url: "https://example.org/set.tar".to_owned(),
            unpacks_to: Some(digest::sha256(b"a different tree")),
        });
        let context = context();
        let runner = Loop::new(&context, &project, Arc::new(context.net()), cache);
        let error = block_on(runner.unpack_locked(
            &Manifest::parse("").unwrap(),
            &lock,
            &archives::of(&lock, vec![("set".to_owned(), &lock.datasets[0].files[0])]),
        ))
        .unwrap_err();
        assert_eq!(error.kind, Kind::IntegrityMismatch);
        assert!(error.message.contains("set/set.tar"));
        assert!(error.help.unwrap().contains("lock --upgrade set"));
        assert!(runner.cache.verify(&stored.digest).unwrap());
        assert!(runner.cache.unpacked(&stored.digest, None).is_none());
        assert_eq!(context.work.counts().requests, 0);
    }

    #[test]
    fn dropping_archives_keeps_bytes_needed_by_the_current_lock() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path().join("cache")).unwrap();
        let kept = store(&cache, b"still needed as a file");
        let removed = store(&cache, b"already unpacked");
        let lock = lock_for(LockedFile {
            path: "set.tar".to_owned(),
            size: kept.size,
            sha256: kept.digest,
            md5: None,
            url: "https://example.org/set.tar".to_owned(),
            unpacks_to: None,
        });
        drop_archives(
            &cache,
            &[kept.digest, removed.digest],
            Some(&lock),
            Threads::default(),
        );
        assert_eq!(
            std::fs::read(cache.object_path(&kept.digest)).unwrap(),
            b"still needed as a file"
        );
        assert!(cache.find(&removed.digest, None).is_none());
        drop_archives(
            &cache,
            &[kept.digest],
            Some(&Lock::default()),
            Threads::default(),
        );
        assert!(cache.find(&kept.digest, None).is_none());
    }

    #[test]
    fn an_unreadable_registered_lock_keeps_all_unpacked_archives() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path().join("cache")).unwrap();
        let root = dir.path().join("project");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("data.toml"), b"").unwrap();
        std::fs::write(root.join("data.lock"), b"not a lock").unwrap();
        cache.register(&root).unwrap();
        let archive = store(&cache, b"keep me until the lock can be read");
        drop_archives(&cache, &[archive.digest], None, Threads::default());
        assert_eq!(
            std::fs::read(cache.object_path(&archive.digest)).unwrap(),
            b"keep me until the lock can be read"
        );
        // a data.toml that is there but cannot be read is no missing one
        std::fs::remove_file(root.join("data.lock")).unwrap();
        std::fs::remove_file(root.join("data.toml")).unwrap();
        std::fs::create_dir(root.join("data.toml")).unwrap();
        drop_archives(&cache, &[archive.digest], None, Threads::default());
        assert!(cache.find(&archive.digest, None).is_some());
    }

    /// gc forgets a registered project whose data.toml is gone and frees what it alone needed,
    /// so an unpack keeps nothing for it either. only gc forgets the project itself.
    #[test]
    fn a_project_whose_data_toml_is_gone_keeps_no_unpacked_archive() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path().join("cache")).unwrap();
        let root = dir.path().join("deleted");
        std::fs::create_dir(&root).unwrap();
        cache.register(&root).unwrap();
        let archive = store(&cache, b"nobody needs me packed");
        drop_archives(&cache, &[archive.digest], None, Threads::default());
        assert!(cache.find(&archive.digest, None).is_none());
        let registered = cache.registered_projects(&crate::stop::Stop::default());
        assert_eq!(registered.unwrap().len(), 1);
    }

    #[test]
    fn intact_damaged_archives_preserve_the_source_error() {
        let dir = tempfile::tempdir().unwrap();
        let project = Project {
            root: dir.path().to_path_buf(),
        };
        let cache = Cache::open(dir.path().join("cache")).unwrap();
        let stored = store(&cache, b"PK\x03\x04broken");
        let lock = lock_for(LockedFile {
            path: "set.zip".to_owned(),
            size: stored.size,
            sha256: stored.digest,
            md5: None,
            url: "https://example.org/set.zip".to_owned(),
            unpacks_to: Some(digest::sha256(b"a pinned tree")),
        });
        let context = context();
        let runner = Loop::new(&context, &project, Arc::new(context.net()), cache);
        let error = block_on(runner.unpack_locked(
            &Manifest::parse("").unwrap(),
            &lock,
            &archives::of(&lock, vec![("set".to_owned(), &lock.datasets[0].files[0])]),
        ))
        .unwrap_err();
        assert_eq!(error.kind, Kind::SourceInvalid);
        assert!(error.message.contains("cut short"), "{}", error.message);
        assert!(runner.cache.changed().is_empty());
        assert_eq!(context.work.counts().requests, 0);
    }
}
