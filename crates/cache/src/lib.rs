//! The content-addressed store behind the Store seam.
//!
//! An entry in `objects/` has been verified and published by a rename, and
//! there is no other way for a file to appear there. One writer holds a digest
//! at a time and a reader holds the same lock shared, so a reader that is
//! running keeps its object from being pruned and a reader that was killed
//! releases it with no record to sweep.

#[cfg(test)]
use fetchloom_platform as _;
#[cfg(test)]
use tempfile as _;

pub mod format;
pub mod layout;
pub mod prune;
pub mod record;
pub mod store;
pub mod verify;

use std::path::{Path, PathBuf};

use fetchloom_engine::capability::Backing;
use fetchloom_engine::durability::DurabilityTier;
use fetchloom_engine::error::{Error, ErrorKind};
use fetchloom_engine::identity::BootId;
use fetchloom_engine::seam::platform::{OwnerToken, Platform};
use fetchloom_engine::verification::VerificationPolicy;

use crate::layout::{DIRECTORIES, Layout};

/// The mode a cache directory carries, so every user of the directory can add
/// entries and only an entry's owner can remove it.
#[cfg(unix)]
const SHARED_DIRECTORY_MODE: u32 = 0o1777;

/// The mode a published object carries, because an object never changes.
#[cfg(unix)]
const PUBLISHED_OBJECT_MODE: u32 = 0o444;

/// The name a probe writes to learn whether a volume locks.
///
/// The name carries this process's identity, because a probe that every process
/// shared would be a file each of them removes under the others.
const LOCK_PROBE: &str = "fetchloom-lock-probe";

/// A cache directory this process may read and write.
#[derive(Debug)]
pub struct Cache<P: Platform> {
    layout: Layout,
    platform: P,
    tier: DurabilityTier,
    policy: VerificationPolicy,
    token: OwnerToken,
}

impl<P: Platform> Cache<P> {
    /// Opens the cache at a root, creating it when it is absent.
    ///
    /// Checks the format fingerprint, refuses a root whose directories are on
    /// different volumes, and refuses a volume that cannot express advisory
    /// locking across users. Recovers orphaned entries from a previous boot the
    /// first time it is opened in this boot.
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
    ) -> Result<Self, Error> {
        let layout = Layout::new(root.as_ref());
        create_directories(&layout)?;
        check_format(&layout)?;
        check_one_volume(&platform, &layout)?;
        check_locking(&platform, &layout)?;

        let token = platform.owner_token()?;
        let cache = Self {
            layout,
            platform,
            tier,
            policy,
            token,
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

    /// Returns what this process records about itself.
    #[must_use]
    pub fn token(&self) -> &OwnerToken {
        &self.token
    }

    /// Removes every entry a previous boot of this machine left behind.
    ///
    /// Runs once per boot per cache. An entry recorded by another machine is
    /// left alone, and an entry from this boot is left alone whether or not its
    /// writer is still running.
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
        std::fs::write(self.layout.recovered(), self.token.boot.as_str().as_bytes())
            .map_err(|reason| failure(ErrorKind::CacheCorrupt, &self.layout.recovered(), &reason))
    }

    /// Removes the whole cache.
    ///
    /// # Errors
    ///
    /// Fails when the directory cannot be removed.
    pub fn clear(&self) -> Result<(), Error> {
        std::fs::remove_dir_all(self.layout.root())
            .map_err(|reason| failure(ErrorKind::CacheCorrupt, self.layout.root(), &reason))
    }
}

/// The name an owner record for an entry is written under.
fn owner_record_of(entry: &Path) -> PathBuf {
    let mut name = entry.as_os_str().to_owned();
    name.push(".owner");
    PathBuf::from(name)
}

/// Reports whether this boot has already recovered this cache.
fn already_recovered(layout: &Layout, boot: &BootId) -> Result<bool, Error> {
    match std::fs::read(layout.recovered()) {
        Ok(bytes) => Ok(bytes == boot.as_str().as_bytes()),
        Err(reason) if reason.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(reason) => Err(failure(
            ErrorKind::CacheCorrupt,
            &layout.recovered(),
            &reason,
        )),
    }
}

/// Removes the entries in a directory that this machine wrote in a previous
/// boot, along with their owner records.
fn sweep_previous_boot(directory: &Path, token: &OwnerToken) -> Result<(), Error> {
    let entries = std::fs::read_dir(directory)
        .map_err(|reason| failure(ErrorKind::CacheCorrupt, directory, &reason))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|kind| kind == "owner") {
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
    }
    Ok(())
}

/// Turns a filesystem failure into the error kind it deserves.
///
/// A volume with no room left is a resource failure and not a corruption, and
/// the caller decides what to do about the two differently.
pub(crate) fn failure(kind: ErrorKind, path: &Path, reason: &std::io::Error) -> Error {
    let kind = if reason.kind() == std::io::ErrorKind::StorageFull {
        ErrorKind::ResourceDisk
    } else {
        kind
    };
    Error::new(kind, format!("{}: {reason}", path.display()))
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
        Err(reason) => Err(failure(ErrorKind::CacheCorrupt, path, &reason)),
    }
}

/// Creates the cache root and every directory it holds.
fn create_directories(layout: &Layout) -> Result<(), Error> {
    let mut wanted = vec![layout.root().to_path_buf()];
    for name in DIRECTORIES {
        wanted.push(layout.root().join(name));
    }
    wanted.push(layout.marks());
    for directory in wanted {
        std::fs::create_dir_all(&directory)
            .map_err(|reason| failure(ErrorKind::CacheCorrupt, directory.as_path(), &reason))?;
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
    .map_err(|reason| failure(ErrorKind::CacheCorrupt, directory, &reason))
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

/// Makes a published object read-only, because an object never changes.
#[cfg(unix)]
pub(crate) fn seal_object(path: &Path) -> Result<(), Error> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(PUBLISHED_OBJECT_MODE))
        .map_err(|reason| failure(ErrorKind::CacheCorrupt, path, &reason))
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
fn check_format(layout: &Layout) -> Result<(), Error> {
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
            })
        }
        Err(reason) => Err(failure(ErrorKind::CacheCorrupt, &layout.format(), &reason)),
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
