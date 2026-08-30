//! Executing the commands this build implements.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fetchloom_cache::Cache;
use fetchloom_cache::ingest::Ingested;
use fetchloom_engine::canonical;

use fetchloom_engine::degrade::DegradeQueue;
use fetchloom_engine::digest::{ContentDigest, TreeDigest};
use fetchloom_engine::durability::DurabilityTier;
use fetchloom_engine::error::{Error, ErrorKind, Layer};
use fetchloom_engine::event::{Event, EventPayload, Sequence};
use fetchloom_engine::hashing;
use fetchloom_engine::limits::Limits;
use fetchloom_engine::manifest::ArchiveFormat;
use fetchloom_engine::pool::Processor;
use fetchloom_engine::reconcile::{ReconcileOutcome, Reconciled, reconcile};
use fetchloom_engine::redact::SafeUrl;
use fetchloom_engine::seam::observer::Observer;
use fetchloom_engine::seam::platform::Platform;
use fetchloom_engine::selection::Selection;
use fetchloom_engine::threads::ThreadBudget;
use fetchloom_engine::transfer::{SleepingPause, Transfer};
use fetchloom_engine::tree::{EntryPath, Mode, TreeEntry};
use fetchloom_engine::work::{Work, WorkCounter};
use fetchloom_platform::NativePlatform;
use fetchloom_sources::HttpSource;

use crate::materialize;

/// What a completed run reports.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct RunResult {
    /// What the run did.
    pub status: &'static str,
    /// The dataset the run materialized.
    pub dataset: String,
    /// The tree the run produced.
    pub tree: TreeDigest,
    /// Where the tree was materialized.
    pub destination: PathBuf,
    /// How many entries the tree holds.
    pub entries: u64,
    /// How many bytes those entries hold.
    pub bytes: u64,
    /// What the run read, wrote, and asked for.
    pub work: Work,
}

/// Turns a reference into the local path it names.
///
/// Takes a reference. Returns the path a `file:` location or a plain local path
/// names. Fails when the reference names something this build cannot resolve,
/// which is every scheme that would need the network.
///
/// # Errors
///
/// Returns a resolution failure naming what could not be resolved.
pub fn local_path(reference: &str) -> Result<PathBuf, Error> {
    if let Some(rest) = reference.strip_prefix("file://") {
        let trimmed = rest.strip_prefix('/').unwrap_or(rest);
        let looks_like_windows_path = trimmed.as_bytes().get(1).is_some_and(|byte| *byte == b':');
        let path = if looks_like_windows_path {
            PathBuf::from(trimmed)
        } else {
            PathBuf::from(format!("/{trimmed}"))
        };
        return Ok(path);
    }
    if reference.contains("://") {
        return Err(Error::new(
            ErrorKind::ReferenceUnresolved,
            format!(
                "this build resolves only a local path or a file: location, not {}",
                SafeUrl::new(reference)
            ),
        ));
    }
    let path = PathBuf::from(reference);
    if path.exists() {
        Ok(path)
    } else {
        Err(Error::new(
            ErrorKind::ReferenceUnresolved,
            format!(
                "check that {} names a path that exists",
                SafeUrl::new(reference)
            ),
        ))
    }
}

/// Returns the default destination for a source path.
///
/// Takes the resolved source. Returns the current directory joined with the
/// source's own last component.
#[must_use]
pub fn default_destination(source: &Path) -> PathBuf {
    let name = source
        .file_name()
        .map_or_else(|| PathBuf::from("dataset"), PathBuf::from);
    PathBuf::from(".").join(name)
}

fn failure(kind: ErrorKind, path: &Path, reason: &std::io::Error) -> Error {
    Error::new(kind, format!("{}: {reason}", path.display()))
}

/// Everything a materialization runs against, so one call does not take a list
/// of loose arguments.
#[derive(Clone, Copy)]
pub struct Materialization<'a> {
    /// The pool the digests are computed on.
    pub processor: &'a Processor,
    /// The platform the filesystem work goes through.
    pub platform: &'a NativePlatform,
    /// How far a write is pushed before publication.
    pub durability: DurabilityTier,
    /// The cache to read and write, when the run has one.
    pub cache: Option<&'a Cache<NativePlatform>>,
    /// Where the run counts the work it did.
    pub work: &'a Arc<WorkCounter>,
    /// Whether a recognized archive is extracted rather than kept as a file.
    pub extract: bool,
}

/// Materializes a local source tree into a destination.
///
/// Takes what the materialization runs against, the source, the destination,
/// the selection that decides which members land and under what path,
/// whether a modified or foreign entry may be overwritten or removed, and
/// whether the destination's own contents are accepted as correct instead.
/// When the destination does not exist, every selected entry is staged and
/// published as one atomic rename. When it does, each entry is reconciled
/// against the tree this run resolved: an entry that already matches is left
/// untouched, a missing entry is restored alone, and a modified or foreign
/// entry stops the run unless `force` or `adopt` says otherwise. Returns what
/// the run produced.
///
/// # Errors
///
/// Fails when the source cannot be read, when the selection matches nothing
/// or a layout leaves a member with no path or a collision, when a
/// destination entry is modified or foreign and neither `force` nor `adopt`
/// was given, and when staging cannot be published.
#[expect(
    clippy::too_many_arguments,
    reason = "the selection, force, and adopt flags each name a contract behavior of their own"
)]
pub fn materialize_local(
    with: &Materialization<'_>,
    source: &Path,
    destination: &Path,
    selection: &Selection,
    force: bool,
    adopt: bool,
    observer: &dyn Observer,
    sequence: &Sequence,
) -> Result<RunResult, Error> {
    let emit = |payload: EventPayload| observer.emit(&Event::new(sequence, payload));

    emit(EventPayload::ResolveStart);

    let dataset = source.file_name().map_or_else(
        || "dataset".to_owned(),
        |name| name.to_string_lossy().into_owned(),
    );

    if let Some(ingested) = archive_to_unpack(with, source, &dataset)? {
        emit(EventPayload::ResolveEnd { duration_ms: 0 });
        emit(EventPayload::PlanReady);
        if destination.exists() {
            return Err(Error::new(
                ErrorKind::DestinationForeign,
                format!(
                    "remove {} or choose another destination with --output",
                    destination.display()
                ),
            ));
        }
        let (tree, entries) = publish_one_object(
            with,
            ingested.digest,
            ingested.size,
            &dataset,
            destination,
            selection,
        )?;
        emit(EventPayload::PublishCommit);
        return Ok(RunResult {
            status: "materialized",
            dataset,
            tree,
            destination: destination.to_path_buf(),
            entries,
            bytes: ingested.size,
            work: with.work.taken(),
        });
    }

    let walked = materialize::walk(source)?;
    let walked = apply_selection(walked, selection)?;
    emit(EventPayload::ResolveEnd { duration_ms: 0 });
    emit(EventPayload::PlanReady);

    if destination.exists() {
        return reconcile_existing(with, destination, &walked, force, adopt, &dataset, &emit);
    }

    materialize_fresh(with, &walked, destination, &dataset, &emit)
}

fn materialize_fresh(
    with: &Materialization<'_>,
    walked: &materialize::Walked,
    destination: &Path,
    dataset: &str,
    emit: &dyn Fn(EventPayload),
) -> Result<RunResult, Error> {
    let Materialization {
        platform,
        durability,
        ..
    } = *with;

    let staging = staging_beside(destination);
    if staging.exists() {
        std::fs::remove_dir_all(&staging)
            .map_err(|reason| failure(ErrorKind::DestinationForeign, &staging, &reason))?;
    }
    std::fs::create_dir_all(&staging)
        .map_err(|reason| failure(ErrorKind::DestinationForeign, &staging, &reason))?;

    let outcome = fill_staging(with, &staging, walked, emit);
    let mut entries = match outcome {
        Ok(entries) => entries,
        Err(error) => {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(error);
        }
    };
    entries.extend(walked.entries.iter().cloned());

    let tree = canonical::tree_digest(&entries);

    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|reason| failure(ErrorKind::DestinationForeign, parent, &reason))?;
    }
    if let Err(error) = platform.publish_directory(&staging, destination, durability) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(error);
    }
    for entry in platform.take_degradations() {
        emit(EventPayload::Degrade {
            requested: entry.requested,
            used: entry.used,
            reason: entry.reason,
        });
    }
    emit(EventPayload::PublishCommit);

    Ok(RunResult {
        status: "materialized",
        dataset: dataset.to_owned(),
        tree,
        destination: destination.to_path_buf(),
        entries: entries.len() as u64,
        bytes: walked.bytes,
        work: with.work.taken(),
    })
}

fn entry_size(entry: &TreeEntry) -> u64 {
    match entry {
        TreeEntry::File { size, .. } | TreeEntry::Symlink { size, .. } => *size,
        TreeEntry::Directory { .. } => 0,
    }
}

fn reconcile_existing(
    with: &Materialization<'_>,
    destination: &Path,
    walked: &materialize::Walked,
    force: bool,
    adopt: bool,
    dataset: &str,
    emit: &dyn Fn(EventPayload),
) -> Result<RunResult, Error> {
    let Materialization { processor, .. } = *with;

    let mut resolved: Vec<TreeEntry> = walked.entries.clone();
    resolved.extend(hash_files(
        &walked.root,
        &walked.files,
        processor,
        with.work,
    )?);

    let destination_tree = destination_entries(destination, processor, with.work)?;
    let outcomes = reconcile(&resolved, &destination_tree);
    for found in &outcomes {
        emit(EventPayload::ReconcileOutcomeReached {
            path: found.path.as_str().to_owned(),
            outcome: found.outcome,
        });
    }

    if outcomes
        .iter()
        .all(|found| found.outcome == ReconcileOutcome::Unchanged)
    {
        return Ok(RunResult {
            status: "unchanged",
            dataset: dataset.to_owned(),
            tree: canonical::tree_digest(&resolved),
            destination: destination.to_path_buf(),
            entries: resolved.len() as u64,
            bytes: resolved.iter().map(entry_size).sum(),
            work: with.work.taken(),
        });
    }

    if adopt {
        return Ok(RunResult {
            status: "adopted",
            dataset: dataset.to_owned(),
            tree: canonical::tree_digest(&destination_tree),
            destination: destination.to_path_buf(),
            entries: destination_tree.len() as u64,
            bytes: destination_tree.iter().map(entry_size).sum(),
            work: with.work.taken(),
        });
    }

    let modified: Vec<&str> = outcomes
        .iter()
        .filter(|found| found.outcome == ReconcileOutcome::Modified)
        .map(|found| found.path.as_str())
        .collect();
    let foreign: Vec<&str> = outcomes
        .iter()
        .filter(|found| found.outcome == ReconcileOutcome::Foreign)
        .map(|found| found.path.as_str())
        .collect();

    if !force && (!modified.is_empty() || !foreign.is_empty()) {
        let kind = if modified.is_empty() {
            ErrorKind::DestinationForeign
        } else {
            ErrorKind::DestinationModified
        };
        let mut named: Vec<String> = modified
            .iter()
            .map(|path| format!("{path} (modified)"))
            .collect();
        named.extend(foreign.iter().map(|path| format!("{path} (foreign)")));
        return Err(Error::new(
            kind,
            format!(
                "run again with --force to overwrite the modified entries and remove the foreign ones, or --adopt to accept the destination as it stands: {}",
                named.join(", ")
            ),
        ));
    }

    if force {
        return materialize_fresh(with, walked, destination, dataset, emit);
    }

    restore_missing(with, destination, &resolved, walked, &outcomes, emit)?;
    Ok(RunResult {
        status: "restored",
        dataset: dataset.to_owned(),
        tree: canonical::tree_digest(&resolved),
        destination: destination.to_path_buf(),
        entries: resolved.len() as u64,
        bytes: resolved.iter().map(entry_size).sum(),
        work: with.work.taken(),
    })
}

fn temp_beside(to: &Path) -> PathBuf {
    let name = to.file_name().map_or_else(
        || "entry".to_owned(),
        |name| name.to_string_lossy().into_owned(),
    );
    to.with_file_name(format!(".{name}.fetchloom-restore"))
}

fn restore_missing(
    with: &Materialization<'_>,
    destination: &Path,
    resolved: &[TreeEntry],
    walked: &materialize::Walked,
    outcomes: &[Reconciled],
    emit: &dyn Fn(EventPayload),
) -> Result<(), Error> {
    let missing: HashSet<&str> = outcomes
        .iter()
        .filter(|found| found.outcome == ReconcileOutcome::Restored)
        .map(|found| found.path.as_str())
        .collect();

    let mut directories: Vec<&EntryPath> = resolved
        .iter()
        .filter_map(|entry| match entry {
            TreeEntry::Directory { path } if missing.contains(path.as_str()) => Some(path),
            _ => None,
        })
        .collect();
    directories.sort_by_key(|path| path.as_str().matches('/').count());
    for path in directories {
        let target = destination.join(path.as_str());
        std::fs::create_dir_all(&target)
            .map_err(|reason| failure(ErrorKind::DestinationForeign, &target, &reason))?;
    }

    let gave_up = std::cell::Cell::new(false);
    for file in &walked.files {
        if !missing.contains(file.entry.as_str()) {
            continue;
        }
        let to = destination.join(file.entry.as_str());
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|reason| failure(ErrorKind::DestinationForeign, parent, &reason))?;
        }
        let from = walked.root.join(&file.relative);
        let temp = temp_beside(&to);
        if let Err(error) = place_file(with, &gave_up, &from, &temp, emit) {
            let _ = std::fs::remove_file(&temp);
            return Err(error);
        }
        std::fs::rename(&temp, &to)
            .map_err(|reason| failure(ErrorKind::DestinationForeign, &to, &reason))?;
    }

    for entry in with.platform.take_degradations() {
        emit(EventPayload::Degrade {
            requested: entry.requested,
            used: entry.used,
            reason: entry.reason,
        });
    }
    emit(EventPayload::PublishCommit);
    Ok(())
}

fn fill_staging(
    with: &Materialization<'_>,
    staging: &Path,
    walked: &materialize::Walked,
    emit: &dyn Fn(EventPayload),
) -> Result<Vec<TreeEntry>, Error> {
    let source = walked.root.as_path();
    emit(EventPayload::TransferStart {
        source: fetchloom_engine::redact::SafeUrl::new(&source.to_string_lossy()),
        expected_bytes: Some(walked.bytes),
    });

    for entry in &walked.entries {
        if let TreeEntry::Directory { path } = entry {
            let target = staging.join(path.as_str());
            std::fs::create_dir_all(&target)
                .map_err(|reason| failure(ErrorKind::DestinationForeign, &target, &reason))?;
        }
    }

    let mut entries = Vec::with_capacity(walked.files.len());
    let mut copied = 0u64;
    let gave_up = std::cell::Cell::new(false);
    for file in &walked.files {
        let from = source.join(&file.relative);
        let to = staging.join(file.entry.as_str());
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|reason| failure(ErrorKind::DestinationForeign, parent, &reason))?;
        }
        let (size, content) = place_file(with, &gave_up, &from, &to, emit)?;
        copied += size;
        emit(EventPayload::TransferProgress { bytes: copied });
        entries.push(TreeEntry::File {
            path: file.entry.clone(),
            mode: file.mode,
            size,
            content,
        });
    }

    for link in &walked.links {
        let at = staging.join(link.entry.as_str());
        with.platform
            .create_symlink(&link.target, &at)
            .map_err(|reason| {
                Error::new(
                    ErrorKind::DestinationUnrepresentable,
                    format!(
                        "materialize {} somewhere this platform creates symbolic links, because {}",
                        link.entry.as_str(),
                        reason.next_action()
                    ),
                )
            })?;
    }

    emit(EventPayload::TransferEnd {
        bytes: copied,
        duration_ms: 0,
    });
    Ok(entries)
}

/// Places one file at its target, through the cache when one is open.
///
/// Takes whether an earlier file in this run already gave up on the cache,
/// so every later file goes straight to a byte copy without repeating the
/// same failure. Returns the length and content digest that were written.
///
/// # Errors
///
/// Fails when the source cannot be read or the target cannot be written.
fn place_file(
    with: &Materialization<'_>,
    gave_up: &std::cell::Cell<bool>,
    from: &Path,
    to: &Path,
    emit: &dyn Fn(EventPayload),
) -> Result<(u64, ContentDigest), Error> {
    let Materialization {
        processor,
        platform,
        cache,
        ..
    } = *with;
    let usable = cache.filter(|_| !gave_up.get());
    match usable {
        None => materialize::copy_file(processor, with.work, from, to),
        Some(held) => match through_cache(held, platform, from, to, emit) {
            Ok(measured) => Ok(measured),
            Err(refused)
                if refused.layer() == Layer::Cache || refused.layer() == Layer::Resource =>
            {
                gave_up.set(true);
                emit(EventPayload::Degrade {
                    requested: "keeping this object in the cache".to_owned(),
                    used: "no cache for the rest of this run, so nothing more is retained"
                        .to_owned(),
                    reason: refused.next_action().to_owned(),
                });
                let _ = std::fs::remove_file(to);
                materialize::copy_file(processor, with.work, from, to)
            }
            Err(refused) => Err(refused),
        },
    }
}

/// Puts one file through the cache and materializes it from there.
///
/// Reads the source once, hashing it as it is written into the cache, then
/// places the object at its destination by sharing blocks where the volume can.
/// A source the cache already holds is a hit, and nothing is written into the
/// cache for it.
fn through_cache(
    cache: &Cache<NativePlatform>,
    platform: &NativePlatform,
    from: &Path,
    to: &Path,
    emit: &dyn Fn(EventPayload),
) -> Result<(u64, ContentDigest), Error> {
    let ingested = cache.ingest(from)?;
    if ingested.waited_for.is_some() {
        emit(EventPayload::CacheWait {
            digest: ingested.digest,
        });
    }
    if ingested.was_present {
        emit(EventPayload::CacheHit {
            digest: ingested.digest,
        });
    } else {
        emit(EventPayload::CacheMiss {
            digest: ingested.digest,
        });
    }

    let lease = cache.read_lease(ingested.digest)?;
    let object = cache.layout().object(ingested.digest);
    platform.clone_or_copy(&object, to)?;
    drop(lease);
    Ok((ingested.size, ingested.digest))
}

fn staging_beside(destination: &Path) -> PathBuf {
    let name = destination.file_name().map_or_else(
        || "dataset".to_owned(),
        |name| name.to_string_lossy().into_owned(),
    );
    destination.with_file_name(format!(".{name}.fetchloom-staging"))
}

/// Recomputes the tree digest of a materialized directory.
///
/// Takes the directory to read. Returns its entries and their tree digest. This
/// build recomputes and reports; comparing against a receipt arrives with
/// receipts.
///
/// # Errors
///
/// Fails when the directory cannot be read and when an entry cannot be
/// represented on this platform.
pub fn verify_tree(path: &Path) -> Result<(TreeDigest, u64), Error> {
    if !path.exists() {
        return Err(Error::new(
            ErrorKind::ReferenceUnresolved,
            format!("check that {} names a path that exists", path.display()),
        ));
    }
    let processor = Processor::new(ThreadBudget::resolve(
        std::thread::available_parallelism().unwrap_or(std::num::NonZeroUsize::MIN),
        None,
    ))
    .map_err(|reason| {
        Error::new(
            ErrorKind::ResourceLimit,
            format!("the processor pool could not be built: {reason}"),
        )
    })?;
    let work = WorkCounter::new();
    let entries = destination_entries(path, &processor, &work)?;
    Ok((canonical::tree_digest(&entries), entries.len() as u64))
}

/// Walks a directory and hashes every file it holds, without writing
/// anything anywhere.
///
/// Takes the root the files are relative to, the files a walk of it found,
/// the processor pool the hashing runs on, and where the bytes read are
/// counted. Returns one `TreeEntry::File` per file, carrying the digest of
/// its bytes.
///
/// # Errors
///
/// Fails when a file cannot be opened or read.
fn hash_files(
    root: &Path,
    files: &[materialize::SourceFile],
    processor: &Processor,
    work: &WorkCounter,
) -> Result<Vec<TreeEntry>, Error> {
    let mut entries = Vec::with_capacity(files.len());
    for file in files {
        let full = root.join(&file.relative);
        let handle = std::fs::File::open(&full)
            .map_err(|reason| failure(ErrorKind::ReferenceUnresolved, &full, &reason))?;
        let size = handle
            .metadata()
            .map_err(|reason| failure(ErrorKind::ReferenceUnresolved, &full, &reason))?
            .len();
        let counted = CountedRead {
            inner: handle,
            work,
        };
        let digests = hashing::hash_stream(processor, counted)
            .map_err(|reason| failure(ErrorKind::IntegrityMismatch, &full, &reason))?;
        entries.push(TreeEntry::File {
            path: file.entry.clone(),
            mode: file.mode,
            size,
            content: digests.content,
        });
    }
    Ok(entries)
}

/// A reader that counts every byte it yields as read work, and writes
/// nothing anywhere.
struct CountedRead<'a, R> {
    inner: R,
    work: &'a WorkCounter,
}

impl<R: Read> Read for CountedRead<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let read = self.inner.read(buffer)?;
        if read > 0 {
            self.work.read_bytes(read as u64);
        }
        Ok(read)
    }
}

/// Walks a destination directory and returns the entries it currently holds.
///
/// Takes the destination, the processor pool, and where bytes read are
/// counted. Reuses the same walk a source is read through, so there is one
/// walk rather than two. Returns every directory, symlink, and hashed file
/// found.
///
/// # Errors
///
/// Fails when the destination cannot be read or a file cannot be hashed.
fn destination_entries(
    destination: &Path,
    processor: &Processor,
    work: &WorkCounter,
) -> Result<Vec<TreeEntry>, Error> {
    let walked = materialize::walk(destination)?;
    let mut entries = walked.entries.clone();
    entries.extend(hash_files(&walked.root, &walked.files, processor, work)?);
    Ok(entries)
}

enum SelectionCandidate {
    Directory,
    Symlink {
        size: u64,
        content: ContentDigest,
        target: Vec<u8>,
    },
    File(materialize::SourceFile),
}

/// Applies a selection to what a walk of a source found.
///
/// Takes the walk and the selection. Returns a walk holding only the
/// selected members, renamed under the selection's layout, with every
/// ancestor directory a selected member needs.
///
/// # Errors
///
/// Fails with `reference.unresolved` when the selection matches nothing, and
/// with `destination.unrepresentable` when the layout leaves a member with
/// no path, and with `archive.collision` when two members land on the same
/// path.
fn apply_selection(
    walked: materialize::Walked,
    selection: &Selection,
) -> Result<materialize::Walked, Error> {
    let mut members: Vec<String> = Vec::new();
    let mut candidates: Vec<SelectionCandidate> = Vec::new();
    for entry in &walked.entries {
        match entry {
            TreeEntry::Directory { path } => {
                members.push(path.as_str().to_owned());
                candidates.push(SelectionCandidate::Directory);
            }
            TreeEntry::Symlink {
                path,
                size,
                content,
            } => {
                members.push(path.as_str().to_owned());
                let target = walked
                    .links
                    .iter()
                    .find(|link| link.entry == *path)
                    .map(|link| link.target.clone())
                    .unwrap_or_default();
                candidates.push(SelectionCandidate::Symlink {
                    size: *size,
                    content: *content,
                    target,
                });
            }
            TreeEntry::File { .. } => {}
        }
    }
    for file in &walked.files {
        members.push(file.entry.as_str().to_owned());
        candidates.push(SelectionCandidate::File(file.clone()));
    }

    let refs: Vec<&str> = members.iter().map(String::as_str).collect();
    let applied = selection.apply(&refs)?;

    let mut new_entries: Vec<TreeEntry> = Vec::new();
    let mut new_files: Vec<materialize::SourceFile> = Vec::new();
    let mut new_links: Vec<materialize::SourceLink> = Vec::new();
    let mut claimed: HashMap<String, String> = HashMap::new();

    for member in &applied.members {
        let original = &members[member.index];
        if let Some(earlier) = claimed.insert(member.path.as_str().to_owned(), original.clone()) {
            return Err(Error::new(
                ErrorKind::ArchiveCollision,
                format!(
                    "rename {earlier} or {original} so --layout does not flatten both onto {}",
                    member.path
                ),
            ));
        }
        match &candidates[member.index] {
            SelectionCandidate::Directory => new_entries.push(TreeEntry::Directory {
                path: member.path.clone(),
            }),
            SelectionCandidate::Symlink {
                size,
                content,
                target,
            } => {
                new_entries.push(TreeEntry::Symlink {
                    path: member.path.clone(),
                    size: *size,
                    content: *content,
                });
                new_links.push(materialize::SourceLink {
                    entry: member.path.clone(),
                    target: target.clone(),
                });
            }
            SelectionCandidate::File(file) => {
                let mut rewritten = file.clone();
                rewritten.entry = member.path.clone();
                new_files.push(rewritten);
            }
        }
    }
    for directory in &applied.directories {
        new_entries.push(TreeEntry::Directory {
            path: directory.clone(),
        });
    }

    let mut bytes = 0u64;
    for file in &new_files {
        let full = walked.root.join(&file.relative);
        let metadata = std::fs::symlink_metadata(&full)
            .map_err(|reason| failure(ErrorKind::ReferenceUnresolved, &full, &reason))?;
        bytes += metadata.len();
    }

    Ok(materialize::Walked {
        entries: new_entries,
        files: new_files,
        links: new_links,
        bytes,
        root: walked.root,
    })
}

/// Refuses a reference that would need the network while the network is
/// forbidden.
///
/// Takes the reference and whether the run is offline. Returns nothing when the
/// reference names something reachable without the network.
///
/// # Errors
///
/// Fails with a policy failure when the reference names a network location and
/// the run forbids network activity.
pub fn allowed_offline(reference: &str, offline: bool) -> Result<(), Error> {
    if !offline {
        return Ok(());
    }
    let network = reference.contains("://") && !reference.starts_with("file://");
    if network {
        return Err(Error::new(
            ErrorKind::PolicyOffline,
            format!(
                "run the command again without --offline to reach {}",
                SafeUrl::new(reference)
            ),
        ));
    }
    Ok(())
}

/// Materializes one object named by an HTTP or HTTPS reference.
///
/// Takes what the materialization runs against, the location, and the
/// destination. Streams the object into the cache, hashing it as it arrives,
/// and publishes a destination holding that one entry.
///
/// # Errors
///
/// Fails when the source is unreachable or refuses the request, when the cache
/// cannot be written, and when the destination already exists.
pub fn materialize_remote(
    with: &Materialization<'_>,
    location: &str,
    destination: &Path,
    selection: &Selection,
    observer: &dyn Observer,
    sequence: &Sequence,
) -> Result<RunResult, Error> {
    let emit = |payload: EventPayload| observer.emit(&Event::new(sequence, payload));
    let name = object_name(location);

    if destination.exists() {
        return Err(Error::new(
            ErrorKind::DestinationForeign,
            format!(
                "remove {} or choose another destination with --output",
                destination.display()
            ),
        ));
    }

    emit(EventPayload::ResolveStart);
    let source = HttpSource::new(Limits::default(), Arc::clone(with.work));
    let pause = SleepingPause;
    let limits = Limits::default();
    emit(EventPayload::ResolveEnd { duration_ms: 0 });
    emit(EventPayload::PlanReady);

    let Some(cache) = with.cache else {
        return Err(Error::new(
            ErrorKind::CacheCorrupt,
            "run without --no-cache, because this build streams a remote object through the cache",
        ));
    };

    let degradations = DegradeQueue::new();
    let transfer = Transfer {
        store: cache,
        source: &source,
        pause: &pause,
        limits: &limits,
        degradations: &degradations,
        observer,
        sequence,
    };
    let transferred = transfer.run(None, &[location.to_owned()])?;
    for entry in source.take_degradations() {
        emit(EventPayload::Degrade {
            requested: entry.requested,
            used: entry.used,
            reason: entry.reason,
        });
    }
    for entry in degradations.take() {
        emit(EventPayload::Degrade {
            requested: entry.requested,
            used: entry.used,
            reason: entry.reason,
        });
    }

    let size = transferred.bytes_kept + transferred.bytes_transferred;
    let (tree, entries) = publish_one_object(
        with,
        transferred.digest,
        size,
        &name,
        destination,
        selection,
    )?;
    emit(EventPayload::PublishCommit);

    Ok(RunResult {
        status: "materialized",
        dataset: name,
        tree,
        destination: destination.to_path_buf(),
        entries,
        bytes: size,
        work: with.work.taken(),
    })
}

fn place_object(
    with: &Materialization<'_>,
    digest: ContentDigest,
    into: &Path,
) -> Result<(), Error> {
    let Some(cache) = with.cache else {
        return Err(Error::new(
            ErrorKind::CacheCorrupt,
            "run without --no-cache, because this build streams a remote object through the cache",
        ));
    };
    let object = cache.layout().object(digest);
    with.platform.clone_or_copy(&object, into).map(|_| ())
}

fn object_name(location: &str) -> String {
    let after_scheme = location
        .split_once("://")
        .map_or(location, |(_, rest)| rest);
    let path = after_scheme
        .split(['?', '#'])
        .next()
        .unwrap_or(after_scheme);
    let last = path.rsplit('/').find(|part| !part.is_empty());
    last.map_or_else(|| "object".to_owned(), str::to_owned)
}

/// Reports whether a reference names a location this build fetches over the
/// network.
#[must_use]
pub fn is_remote(reference: &str) -> bool {
    reference.starts_with("http://") || reference.starts_with("https://")
}

/// Returns the name the object at a remote location is materialized under.
#[must_use]
pub fn remote_name(location: &str) -> String {
    object_name(location)
}

fn publish_one_object(
    with: &Materialization<'_>,
    digest: ContentDigest,
    size: u64,
    name: &str,
    destination: &Path,
    selection: &Selection,
) -> Result<(TreeDigest, u64), Error> {
    let staging = staging_beside(destination);
    if staging.exists() {
        std::fs::remove_dir_all(&staging)
            .map_err(|reason| failure(ErrorKind::DestinationForeign, &staging, &reason))?;
    }
    std::fs::create_dir_all(&staging)
        .map_err(|reason| failure(ErrorKind::DestinationForeign, &staging, &reason))?;

    let built = match packed_format(with, digest, name)? {
        Some(format) => extract_into(with, digest, format, &staging, selection),
        None => place_object(with, digest, &staging.join(name)).and_then(|()| {
            Ok(vec![TreeEntry::File {
                path: EntryPath::new(name).map_err(|reason| {
                    Error::new(ErrorKind::ReferenceUnresolved, reason.to_string())
                })?,
                size,
                mode: Mode::ReadWrite,
                content: digest,
            }])
        }),
    };
    let entries = match built {
        Ok(entries) => entries,
        Err(error) => {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(error);
        }
    };

    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|reason| failure(ErrorKind::DestinationForeign, parent, &reason))?;
    }
    if let Err(error) = with
        .platform
        .publish_directory(&staging, destination, with.durability)
    {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(error);
    }
    Ok((canonical::tree_digest(&entries), entries.len() as u64))
}

/// Returns the archive format an object holds, when it holds one this build
/// extracts.
///
/// Takes what the materialization runs against, the digest of the object, and
/// the name the location gave it. Returns nothing when the name states no
/// archive extension, when the run asked for no extraction, and when there is
/// no cache to read the object from.
///
/// # Errors
///
/// Fails with `archive.unsupported` when the name and the object's own leading
/// bytes disagree about what the object is.
fn packed_format(
    with: &Materialization<'_>,
    digest: ContentDigest,
    name: &str,
) -> Result<Option<ArchiveFormat>, Error> {
    if !with.extract {
        return Ok(None);
    }
    let Some(cache) = with.cache else {
        return Ok(None);
    };
    if fetchloom_archive::format_from_extension(name).is_none() {
        return Ok(None);
    }
    let object = cache.layout().object(digest);
    let mut file = std::fs::File::open(&object)
        .map_err(|reason| failure(ErrorKind::ArchiveUnsupported, &object, &reason))?;
    let mut header = [0_u8; 16];
    let filled = read_up_to(&mut file, &mut header)?;
    fetchloom_archive::recognize(None, name, &header[..filled])
}

fn read_up_to(file: &mut std::fs::File, into: &mut [u8]) -> Result<usize, Error> {
    let mut filled = 0;
    while filled < into.len() {
        let taken = file.read(&mut into[filled..]).map_err(|reason| {
            Error::new(
                ErrorKind::ArchiveUnsupported,
                format!("the archive's leading bytes could not be read: {reason}"),
            )
        })?;
        if taken == 0 {
            break;
        }
        filled += taken;
    }
    Ok(filled)
}

/// Extracts a cached archive into a staging directory.
///
/// Takes what the materialization runs against, the digest of the archive, the
/// format it holds, the staging directory, and the selection deciding which
/// members land. Returns the entries that were written.
///
/// # Errors
///
/// Fails with the archive or destination failure the reader or the extraction
/// named, and publishes nothing.
fn extract_into(
    with: &Materialization<'_>,
    digest: ContentDigest,
    format: ArchiveFormat,
    staging: &Path,
    selection: &Selection,
) -> Result<Vec<TreeEntry>, Error> {
    let Some(cache) = with.cache else {
        return Err(Error::new(
            ErrorKind::CacheCorrupt,
            "run without --no-cache, because this build extracts an archive out of the cache",
        ));
    };
    let object = cache.layout().object(digest);
    let file = std::fs::File::open(&object)
        .map_err(|reason| failure(ErrorKind::ArchiveUnsupported, &object, &reason))?;
    let mut reader = fetchloom_archive::ArchiveReader::new(
        file,
        format,
        object.to_string_lossy().into_owned(),
        Limits::default(),
    )?;
    fetchloom_archive::extract(
        &mut reader,
        selection,
        staging,
        Limits::default(),
        with.platform,
    )
}

/// Puts a local archive into the cache so it can be extracted from there.
///
/// Takes what the materialization runs against, the source, and the name the
/// source is known by. Returns nothing when the source is not one file, when
/// its name states no archive extension, when the run asked for no
/// extraction, and when there is no cache to hold it.
///
/// # Errors
///
/// Fails when the source cannot be read into the cache.
fn archive_to_unpack(
    with: &Materialization<'_>,
    source: &Path,
    name: &str,
) -> Result<Option<Ingested>, Error> {
    if !with.extract {
        return Ok(None);
    }
    let Some(cache) = with.cache else {
        return Ok(None);
    };
    if fetchloom_archive::format_from_extension(name).is_none() || !source.is_file() {
        return Ok(None);
    }
    Ok(Some(cache.ingest(source)?))
}
