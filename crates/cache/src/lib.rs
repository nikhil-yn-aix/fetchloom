//! The content-addressed store behind the Store seam.

#[cfg(test)]
use fetchloom_faults as _;
#[cfg(test)]
use fetchloom_platform as _;
#[cfg(test)]
use tempfile as _;
#[cfg(all(test, windows))]
use windows_sys as _;

pub mod bundle;
pub mod compact;
pub mod compress;
pub mod diagnosis;
pub mod format;
pub mod ingest;
pub mod layout;
pub mod measurement;
pub mod pack;
pub mod prune;
pub mod rebuild;
pub mod receipts;
pub mod record;
pub mod repair;
pub mod resolution;
pub mod storage;
pub mod store;
pub mod verify;
pub mod witness;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fetchloom_engine::capability::Backing;
use fetchloom_engine::compression::CompressionChoice;
use fetchloom_engine::degrade::{Degradation, DegradeQueue};
use fetchloom_engine::durability::DurabilityTier;
use fetchloom_engine::error::{Error, ErrorKind, Surface, filesystem_failure};
use fetchloom_engine::identity::BootId;
use fetchloom_engine::pool::Processor;
use fetchloom_engine::seam::platform::{OwnerToken, Platform};
use fetchloom_engine::seam::policy::IoMode;
use fetchloom_engine::tuning::{CAN_RELEASE_PAGES, resolve_io_mode};
use fetchloom_engine::verification::VerificationPolicy;
use fetchloom_engine::work::WorkCounter;

use crate::layout::{DIRECTORIES, Layout};

#[cfg(unix)]
const SHARED_DIRECTORY_MODE: u32 = 0o1777;

#[cfg(unix)]
const PRIVATE_DIRECTORY_MODE: u32 = 0o700;

#[cfg(unix)]
const PUBLISHED_OBJECT_MODE: u32 = 0o444;

/// A pack holds objects and is appended to, so it carries what contracts.md
/// states an object carries, minus the immutability an object gets by never
/// being written again: readable by every user, writable only by its creator.
#[cfg(unix)]
const PACK_MODE: u32 = 0o644;

const LOCK_PROBE: &str = "fetchloom-lock-probe";

/// What a cache was opened with, as opposed to what it holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CacheSettings {
    pub tier: DurabilityTier,
    pub policy: VerificationPolicy,
    pub io: IoMode,
    pub compression: CompressionChoice,
}

#[derive(Debug)]
pub struct Cache<P: Platform> {
    layout: Layout,
    platform: P,
    tier: DurabilityTier,
    policy: VerificationPolicy,
    io_mode: IoMode,
    compression: CompressionChoice,
    volume_compresses: bool,
    degradations: DegradeQueue,
    token: OwnerToken,
    work: Arc<WorkCounter>,
    processor: Arc<Processor>,
    appending: std::sync::Mutex<()>,
    packed: std::sync::Mutex<Option<Arc<crate::pack::Index>>>,
    unflushed_pack: std::sync::atomic::AtomicBool,
    sharing: Sharing,
}

/// What a record naming the process behind every scratch file it wrote is
/// called, so a sweep can tell which boot those files belong to.
pub(crate) const SESSION_SUFFIX: &str = ".session";

const SESSION_KIND: &str = "session";

impl<P: Platform> Cache<P> {
    /// # Errors
    /// `cache.format_mismatch` when the directory was written by a build with
    /// another format, `cache.cross_volume` when the cache spans volumes,
    /// `cache.locking_unsupported` when the volume is reached over a network
    /// or cannot express an advisory lock, and `cache.corrupt` when the
    /// directories cannot be created or read.
    pub fn open(
        root: impl AsRef<Path>,
        platform: P,
        settings: CacheSettings,
        work: Arc<WorkCounter>,
        processor: Arc<Processor>,
    ) -> Result<Self, Error> {
        let CacheSettings {
            tier,
            policy,
            io: requested_io,
            compression,
        } = settings;
        let layout = Layout::new(root.as_ref());
        let layout_root = layout.root().to_path_buf();
        create_directories(&layout, &work)?;
        check_format(&layout, &work)?;
        check_one_volume(&platform, &layout)?;
        check_locking(&platform, &layout)?;

        platform.remember_probes_in(&layout.meta());
        let capabilities = platform.volume_capabilities(&layout.objects())?;
        let degradations = DegradeQueue::new();
        let io_mode = resolve_io_mode(
            requested_io,
            &capabilities,
            CAN_RELEASE_PAGES,
            &degradations,
        );
        let volume_compresses = capabilities.compresses;
        if volume_compresses && compression != CompressionChoice::None {
            degradations.record(
                format!("cached objects written {compression}"),
                "cached objects written raw",
                format!(
                    "{} is on a volume that compresses what is written to it, so compressing again would spend processor time to store the same bytes twice over",
                    layout.objects().display()
                ),
            );
        }

        let token = platform.owner_token()?;
        let cache = Self {
            layout,
            platform,
            tier,
            policy,
            io_mode,
            compression,
            volume_compresses,
            degradations,
            token,
            work,
            processor,
            appending: std::sync::Mutex::new(()),
            packed: std::sync::Mutex::new(None),
            unflushed_pack: std::sync::atomic::AtomicBool::new(false),
            sharing: sharing_of(layout_root.as_path()),
        };
        cache.recover()?;
        Ok(cache)
    }

    pub(crate) fn pack_awaits_flushing(&self) {
        self.unflushed_pack
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// Pushes this run's pack once for every entry appended since the last
    /// push. `strict` already pushed each entry and `fast` pushes nothing, so
    /// this is `normal`'s whole durability and belongs before anything durable
    /// names what the pack holds.
    ///
    /// # Errors
    /// `cache.corrupt` when the pack cannot be opened or the flush is refused,
    /// and `resource.disk` when the volume is full.
    pub fn flush_packs(&self) -> Result<(), Error> {
        if !self
            .unflushed_pack
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return Ok(());
        }
        let path = self.own_pack();
        let file = match std::fs::File::options().append(true).open(&path) {
            Ok(file) => file,
            Err(reason) if reason.kind() == std::io::ErrorKind::NotFound => {
                self.unflushed_pack
                    .store(false, std::sync::atomic::Ordering::SeqCst);
                return Ok(());
            }
            Err(reason) => return Err(filesystem_failure(Surface::Cache, &path, &reason)),
        };
        self.platform.flush(&file, self.tier)?;
        self.work.touched_file();
        self.unflushed_pack
            .store(false, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    #[must_use]
    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    #[must_use]
    pub fn platform(&self) -> &P {
        &self.platform
    }

    #[must_use]
    pub fn tier(&self) -> DurabilityTier {
        self.tier
    }

    #[must_use]
    pub fn policy(&self) -> VerificationPolicy {
        self.policy
    }

    #[must_use]
    pub fn take_degradations(&self) -> Vec<Degradation> {
        self.degradations.take()
    }

    /// What the platform this cache writes through recorded, kept apart from
    /// the cache's own so a run drains it once at the end rather than at
    /// whichever moment the volume happened to be asked about itself.
    pub fn take_platform_degradations(&self) -> Vec<Degradation> {
        self.platform().take_degradations()
    }

    /// What this run was asked to do, which is not always what it does: a
    /// volume that compresses on its own is stored raw whatever was asked.
    #[must_use]
    pub fn compression(&self) -> CompressionChoice {
        if self.volume_compresses {
            CompressionChoice::None
        } else {
            self.compression
        }
    }

    #[must_use]
    pub fn work(&self) -> &Arc<WorkCounter> {
        &self.work
    }

    #[must_use]
    pub fn processor(&self) -> &Arc<Processor> {
        &self.processor
    }

    #[must_use]
    pub fn token(&self) -> &OwnerToken {
        &self.token
    }

    fn recover(&self) -> Result<(), Error> {
        if already_recovered(&self.layout, &self.token.boot)? {
            return Ok(());
        }
        sweep_previous_boot(&self.layout.partial(), &self.token)?;
        sweep_previous_boot(&self.layout.staging(), &self.token)?;
        fetchloom_engine::atomic::replace(
            &self.layout.recovered(),
            self.token.boot.as_str().as_bytes(),
        )
        .map_err(|(_, reason)| {
            filesystem_failure(Surface::Cache, &self.layout.recovered(), &reason)
        })?;
        self.work.touched_file();
        Ok(())
    }
}

/// # Errors
/// `cache.corrupt` when the directory exists and cannot be removed. A cache
/// that is not there is not an error.
pub fn clear(root: &Path) -> Result<(), Error> {
    match std::fs::remove_dir_all(root) {
        Ok(()) => Ok(()),
        Err(reason) if reason.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(reason) => Err(filesystem_failure(Surface::Cache, root, &reason)),
    }
}

fn source_record_of(entry: &Path) -> PathBuf {
    let mut name = entry.as_os_str().to_owned();
    name.push(".source");
    PathBuf::from(name)
}

fn owner_record_of(entry: &Path) -> PathBuf {
    let mut name = entry.as_os_str().to_owned();
    name.push(".owner");
    PathBuf::from(name)
}

fn already_recovered(layout: &Layout, boot: &BootId) -> Result<bool, Error> {
    match std::fs::read(layout.recovered()) {
        Ok(bytes) => Ok(bytes == boot.as_str().as_bytes()),
        Err(reason) if reason.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(reason) => Err(filesystem_failure(
            Surface::Cache,
            &layout.recovered(),
            &reason,
        )),
    }
}

/// Removes what a previous boot on this machine left in `directory`.
///
/// Session records are read before anything is removed: a directory yields
/// entries in any order, and removing a record before the file it names leaves
/// that file behind. An entry with no owner record is reclaimed through the
/// session record instead.
fn sweep_previous_boot(directory: &Path, token: &OwnerToken) -> Result<(), Error> {
    let sessions = sessions_in(directory)?;
    let entries = std::fs::read_dir(directory)
        .map_err(|reason| filesystem_failure(Surface::Cache, directory, &reason))?;
    let mut swept = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path
            .extension()
            .is_some_and(|kind| kind == "owner" || kind == "source" || kind == SESSION_KIND)
        {
            continue;
        }
        let record = owner_record_of(&path);
        let wrote_it = match record::read_owner(&record)? {
            Some(found) => found,
            None => match session_for(&sessions, &path) {
                Some(found) => found.clone(),
                None => continue,
            },
        };
        if wrote_it.machine != token.machine || wrote_it.boot == token.boot {
            continue;
        }
        swept.push(path);
    }
    for path in swept {
        remove(&path)?;
        remove(&owner_record_of(&path))?;
        remove(&source_record_of(&path))?;
    }
    for (name, wrote_it) in sessions {
        if wrote_it.machine == token.machine && wrote_it.boot != token.boot {
            remove(&directory.join(format!("{name}{SESSION_SUFFIX}")))?;
        }
    }
    Ok(())
}

fn sessions_in(directory: &Path) -> Result<BTreeMap<String, OwnerToken>, Error> {
    let entries = std::fs::read_dir(directory)
        .map_err(|reason| filesystem_failure(Surface::Cache, directory, &reason))?;
    let mut found = BTreeMap::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|kind| kind != SESSION_KIND) {
            continue;
        }
        let Some(name) = path.file_stem().and_then(std::ffi::OsStr::to_str) else {
            continue;
        };
        if let Some(wrote_it) = record::read_owner(&path)? {
            found.insert(name.to_owned(), wrote_it);
        }
    }
    Ok(found)
}

fn session_for<'a>(
    sessions: &'a BTreeMap<String, OwnerToken>,
    entry: &Path,
) -> Option<&'a OwnerToken> {
    let name = entry.file_name().and_then(std::ffi::OsStr::to_str)?;
    let mut parts = name.splitn(3, '-');
    let (pid, start) = (parts.next()?, parts.next()?);
    parts.next()?;
    sessions.get(&format!("{pid}-{start}"))
}

/// The record a process wrote once, naming the boot behind every scratch file
/// whose name carries that process and start.
fn remove(path: &Path) -> Result<(), Error> {
    let outcome = if path.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
    match outcome {
        Ok(()) => Ok(()),
        Err(reason) if reason.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(reason) => Err(filesystem_failure(Surface::Cache, path, &reason)),
    }
}

fn create_directories(layout: &Layout, work: &WorkCounter) -> Result<(), Error> {
    let sharing = sharing_of(layout.root());
    let mut wanted = vec![layout.root().to_path_buf()];
    for name in DIRECTORIES {
        wanted.push(layout.root().join(name));
    }
    wanted.push(layout.marks());
    wanted.push(layout.records());
    wanted.push(layout.witnesses());
    wanted.push(layout.resolutions());
    wanted.push(layout.measurements());
    for directory in wanted {
        let absent = !directory.is_dir();
        std::fs::create_dir_all(&directory)
            .map_err(|reason| filesystem_failure(Surface::Cache, directory.as_path(), &reason))?;
        if absent {
            work.touched_file();
        }
        share_directory(&directory, sharing)?;
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Sharing {
    Shared,
    Private,
}

#[cfg(unix)]
pub(crate) fn sharing_of(root: &Path) -> Sharing {
    use std::os::unix::fs::PermissionsExt;

    fn writable_by_others(path: &Path) -> bool {
        std::fs::symlink_metadata(path)
            .map(|found| found.permissions().mode() & 0o022 != 0)
            .unwrap_or(false)
    }

    if root.is_dir() {
        if writable_by_others(root) {
            return Sharing::Shared;
        }
        return Sharing::Private;
    }
    match root.parent() {
        Some(parent) if writable_by_others(parent) => Sharing::Shared,
        _ => Sharing::Private,
    }
}

#[cfg(windows)]
pub(crate) fn sharing_of(_root: &Path) -> Sharing {
    Sharing::Private
}

#[cfg(unix)]
fn share_directory(directory: &Path, sharing: Sharing) -> Result<(), Error> {
    use std::os::unix::fs::PermissionsExt;

    let mode = match sharing {
        Sharing::Shared => SHARED_DIRECTORY_MODE,
        Sharing::Private => PRIVATE_DIRECTORY_MODE,
    };
    std::fs::set_permissions(directory, std::fs::Permissions::from_mode(mode))
        .map_err(|reason| filesystem_failure(Surface::Cache, directory, &reason))
}

#[cfg(windows)]
#[expect(
    clippy::unnecessary_wraps,
    reason = "the Unix form of this call fails, and one signature keeps the caller written once"
)]
fn share_directory(_directory: &Path, _sharing: Sharing) -> Result<(), Error> {
    Ok(())
}

#[cfg(unix)]
pub(crate) fn share_pack(path: &Path) -> Result<(), Error> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(PACK_MODE))
        .map_err(|reason| filesystem_failure(Surface::Cache, path, &reason))
}

#[cfg(windows)]
#[expect(
    clippy::unnecessary_wraps,
    reason = "the Unix form of this call fails, and one signature keeps the caller written once"
)]
pub(crate) fn share_pack(_path: &Path) -> Result<(), Error> {
    Ok(())
}

#[cfg(unix)]
pub(crate) fn seal_object(path: &Path) -> Result<(), Error> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(PUBLISHED_OBJECT_MODE))
        .map_err(|reason| filesystem_failure(Surface::Cache, path, &reason))
}

#[cfg(windows)]
#[expect(
    clippy::unnecessary_wraps,
    reason = "the Unix form of this call fails, and one signature keeps the caller written once"
)]
pub(crate) fn seal_object(_path: &Path) -> Result<(), Error> {
    Ok(())
}

fn check_format(layout: &Layout, work: &WorkCounter) -> Result<(), Error> {
    let ours = format::render(format::fingerprint());
    match std::fs::read_to_string(layout.format()) {
        Ok(found) if found == ours => Ok(()),
        Ok(_) => Err(Error::new(
            ErrorKind::CacheFormatMismatch,
            format!(
                "run cache clear, because {} was written in a format this build does not read and nothing is migrated",
                layout.root().display()
            ),
        )),
        Err(reason) if reason.kind() == std::io::ErrorKind::NotFound => {
            fetchloom_engine::atomic::replace(&layout.format(), ours.as_bytes()).map_err(
                |(site, why)| match site {
                    fetchloom_engine::atomic::Site::Scratch(beside) => {
                        filesystem_failure(Surface::Cache, &beside, &why)
                    }
                    fetchloom_engine::atomic::Site::Final => {
                        filesystem_failure(Surface::Cache, &layout.format(), &why)
                    }
                },
            )?;
            work.touched_file();
            Ok(())
        }
        Err(reason) => Err(filesystem_failure(
            Surface::Cache,
            &layout.format(),
            &reason,
        )),
    }
}

fn check_one_volume<P: Platform>(platform: &P, layout: &Layout) -> Result<(), Error> {
    let objects = platform.volume_id(&layout.objects())?;
    for other in [layout.partial(), layout.staging(), layout.locks()] {
        if platform.volume_id(&other)? != objects {
            return Err(Error::new(
                ErrorKind::CacheCrossVolume,
                format!(
                    "put the whole cache on one volume, because {} is on another volume than {} and a publish is a rename and never a copy",
                    other.display(),
                    layout.objects().display()
                ),
            ));
        }
    }
    Ok(())
}

fn check_locking<P: Platform>(platform: &P, layout: &Layout) -> Result<(), Error> {
    if platform.volume_backing(&layout.locks())? == Backing::Network {
        return Err(Error::new(
            ErrorKind::CacheLockingUnsupported,
            format!(
                "put the cache on a local volume, because {} is reached over a network and its locks cannot be trusted to hold across the machines that share it",
                layout.root().display()
            ),
        ));
    }
    let probe = layout
        .locks()
        .join(format!("{LOCK_PROBE}-{}", std::process::id()));
    let held = platform.try_lock(&probe)?;
    drop(held);
    let _ = std::fs::remove_file(&probe);
    Ok(())
}

impl<P: Platform> Cache<P> {
    pub(crate) fn is_shared(&self) -> bool {
        self.sharing == Sharing::Shared
    }
}
