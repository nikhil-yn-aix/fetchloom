//! The content-addressed store behind the Store seam.

#[cfg(test)]
use fetchloom_faults as _;
#[cfg(test)]
use fetchloom_platform as _;
#[cfg(test)]
use tempfile as _;

pub mod bundle;
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
use fetchloom_engine::durability::DurabilityTier;
use fetchloom_engine::error::{Error, ErrorKind, Surface, filesystem_failure};
use fetchloom_engine::identity::BootId;
use fetchloom_engine::pool::Processor;
use fetchloom_engine::seam::platform::{OwnerToken, Platform};
use fetchloom_engine::verification::VerificationPolicy;
use fetchloom_engine::work::WorkCounter;

use crate::layout::{DIRECTORIES, Layout};

/// The mode a cache directory carries.
#[cfg(unix)]
const SHARED_DIRECTORY_MODE: u32 = 0o1777;

/// The mode a published object carries.
#[cfg(unix)]
const PUBLISHED_OBJECT_MODE: u32 = 0o444;

/// The name a probe writes to learn whether a volume locks.
const LOCK_PROBE: &str = "fetchloom-lock-probe";

/// A cache directory this process may read and write.
#[derive(Debug)]
pub struct Cache<P: Platform> {
    layout: Layout,
    platform: P,
    tier: DurabilityTier,
    policy: VerificationPolicy,
    token: OwnerToken,
    work: Arc<WorkCounter>,
    processor: Arc<Processor>,
    packed: std::sync::Mutex<
        Option<
            std::collections::BTreeMap<
                fetchloom_engine::digest::ContentDigest,
                (std::path::PathBuf, crate::pack::Entry),
            >,
        >,
    >,
}

impl<P: Platform> Cache<P> {
    /// Opens the cache at a root, creating it when it is absent.
    ///
    /// # Errors
    ///
    /// Fails when the directory cannot be created or written to, when the
    /// format on disk is not the one this build writes, when the root spans
    /// volumes, and when the volume cannot express advisory locking.
    pub fn open(
        root: impl AsRef<Path>,
        platform: P,
        tier: DurabilityTier,
        policy: VerificationPolicy,
        work: Arc<WorkCounter>,
        processor: Arc<Processor>,
    ) -> Result<Self, Error> {
        let layout = Layout::new(root.as_ref());
        create_directories(&layout, &work)?;
        check_format(&layout, &work)?;
        check_one_volume(&platform, &layout)?;
        check_locking(&platform, &layout)?;

        let token = platform.owner_token()?;
        let cache = Self {
            layout,
            platform,
            tier,
            policy,
            token,
            work,
            processor,
            packed: std::sync::Mutex::new(None),
        };
        cache.recover()?;
        Ok(cache)
    }

    /// Returns where the cache is.
    #[must_use]
    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    /// Returns the platform the cache calls.
    #[must_use]
    pub fn platform(&self) -> &P {
        &self.platform
    }

    /// Returns the durability tier every publication uses.
    #[must_use]
    pub fn tier(&self) -> DurabilityTier {
        self.tier
    }

    /// Returns the check applied to an object already present.
    #[must_use]
    pub fn policy(&self) -> VerificationPolicy {
        self.policy
    }

    /// Returns where the run counts the file bytes it moves.
    #[must_use]
    pub fn work(&self) -> &Arc<WorkCounter> {
        &self.work
    }

    /// Returns the pool the two digests of an object are taken on.
    #[must_use]
    pub fn processor(&self) -> &Arc<Processor> {
        &self.processor
    }

    /// Returns what this process records about itself.
    #[must_use]
    pub fn token(&self) -> &OwnerToken {
        &self.token
    }

    /// Removes every entry a previous boot of this machine left behind.
    ///
    /// # Errors
    ///
    /// Fails when an entry cannot be removed.
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

/// Removes the whole cache at a root, whatever wrote it.
///
/// # Errors
///
/// Fails when the directory cannot be removed. A root that is already absent
/// succeeds.
pub fn clear(root: &Path) -> Result<(), Error> {
    match std::fs::remove_dir_all(root) {
        Ok(()) => Ok(()),
        Err(reason) if reason.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(reason) => Err(filesystem_failure(Surface::Cache, root, &reason)),
    }
}

/// The name a source record for an entry is written under.
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

/// Removes the entries in a directory that this machine wrote in a previous
/// boot, along with their owner records.
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

/// Removes a file or a directory, whichever the path is.
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

/// Creates the cache root and every directory it holds.
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

/// Gives a cache directory the mode every user of the cache needs.
#[cfg(unix)]
fn share_directory(directory: &Path) -> Result<(), Error> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(
        directory,
        std::fs::Permissions::from_mode(SHARED_DIRECTORY_MODE),
    )
    .map_err(|reason| filesystem_failure(Surface::Cache, directory, &reason))
}

/// Leaves a cache directory with the entries it inherited.
#[cfg(windows)]
#[expect(
    clippy::unnecessary_wraps,
    reason = "the Unix form of this call fails, and one signature keeps the caller written once"
)]
fn share_directory(_directory: &Path) -> Result<(), Error> {
    Ok(())
}

/// Makes a published object read-only.
#[cfg(unix)]
pub(crate) fn seal_object(path: &Path) -> Result<(), Error> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(PUBLISHED_OBJECT_MODE))
        .map_err(|reason| filesystem_failure(Surface::Cache, path, &reason))
}

/// Leaves a published object with the entries it inherited.
#[cfg(windows)]
#[expect(
    clippy::unnecessary_wraps,
    reason = "the Unix form of this call fails, and one signature keeps the caller written once"
)]
pub(crate) fn seal_object(_path: &Path) -> Result<(), Error> {
    Ok(())
}

/// Writes the format fingerprint, or checks the one already there.
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
            std::fs::write(layout.format(), ours).map_err(|why| {
                Error::new(
                    ErrorKind::CacheCorrupt,
                    format!("{}: {why}", layout.format().display()),
                )
            })?;
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

/// Refuses a cache whose directories are not all on one volume.
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

/// Refuses a volume that cannot express advisory locking across users.
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
