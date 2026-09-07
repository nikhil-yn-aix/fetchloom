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
const PUBLISHED_OBJECT_MODE: u32 = 0o444;

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
}

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
        create_directories(&layout, &work)?;
        check_format(&layout, &work)?;
        check_one_volume(&platform, &layout)?;
        check_locking(&platform, &layout)?;

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
        };
        cache.recover()?;
        Ok(cache)
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
        std::fs::write(self.layout.recovered(), self.token.boot.as_str().as_bytes()).map_err(
            |reason| filesystem_failure(Surface::Cache, &self.layout.recovered(), &reason),
        )?;
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

fn sweep_previous_boot(directory: &Path, token: &OwnerToken) -> Result<(), Error> {
    let entries = std::fs::read_dir(directory)
        .map_err(|reason| filesystem_failure(Surface::Cache, directory, &reason))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path
            .extension()
            .is_some_and(|kind| kind == "owner" || kind == "source")
        {
            continue;
        }
        let record = owner_record_of(&path);
        let Some(wrote_it) = record::read_owner(&record)? else {
            continue;
        };
        if wrote_it.machine != token.machine || wrote_it.boot == token.boot {
            continue;
        }
        remove(&path)?;
        remove(&record)?;
        remove(&source_record_of(&path))?;
    }
    Ok(())
}

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
        share_directory(&directory)?;
    }
    Ok(())
}

#[cfg(unix)]
fn share_directory(directory: &Path) -> Result<(), Error> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(
        directory,
        std::fs::Permissions::from_mode(SHARED_DIRECTORY_MODE),
    )
    .map_err(|reason| filesystem_failure(Surface::Cache, directory, &reason))
}

#[cfg(windows)]
#[expect(
    clippy::unnecessary_wraps,
    reason = "the Unix form of this call fails, and one signature keeps the caller written once"
)]
fn share_directory(_directory: &Path) -> Result<(), Error> {
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
            std::fs::write(layout.format(), ours)
                .map_err(|why| filesystem_failure(Surface::Cache, &layout.format(), &why))?;
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
