//! What the store does differently on unix: mode bits, space reservation, file identity and
//! links.

use std::fs::{self, File, Metadata, Permissions};
use std::io;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

use rustix::io::Errno;

use crate::tier::Sharing;

/// Sets the mode of a directory the store created: 0755, or 02775 for a tier shared by a group.
pub(crate) fn set_dir_mode(path: &Path, sharing: Sharing) -> io::Result<()> {
    let mode = match sharing {
        Sharing::Private => 0o755,
        Sharing::Group => 0o2775,
    };
    fs::set_permissions(path, Permissions::from_mode(mode))
}

/// Makes the file at `path` writable by its owner (0644) or read only (0444).
pub(crate) fn set_writable(path: &Path, writable: bool) -> io::Result<()> {
    fs::set_permissions(
        path,
        Permissions::from_mode(if writable { 0o644 } else { 0o444 }),
    )
}

/// Makes the open file read only: mode 0444.
pub(crate) fn seal(file: &File) -> io::Result<()> {
    file.set_permissions(Permissions::from_mode(0o444))
}

/// Reserves `length` bytes for `file`, so a full disk fails before anything is written, and
/// sets the length instead on a filesystem without `fallocate`.
pub(crate) fn reserve(file: &File, length: u64) -> io::Result<()> {
    match rustix::fs::fallocate(file, rustix::fs::FallocateFlags::empty(), 0, length) {
        Ok(()) => Ok(()),
        Err(errno) if [Errno::OPNOTSUPP, Errno::NOTSUP, Errno::NOSYS].contains(&errno) => {
            file.set_len(length)
        }
        Err(errno) => Err(errno.into()),
    }
}

/// What tells one file from another: the device and the inode.
pub(crate) fn identity(metadata: &Metadata) -> u128 {
    (u128::from(metadata.dev()) << 64) | u128::from(metadata.ino())
}

/// Clones `object` to `to` where the filesystem shares extents.
pub(crate) fn reflink(object: &Path, to: &Path) -> io::Result<()> {
    reflink_copy::reflink(object, to)
}

/// Creates a symlink at `to` pointing to `object`.
pub(crate) fn symlink(object: &Path, to: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(object, to)
}

/// Renames `staging` over `target`.
pub(crate) fn replace(staging: &Path, target: &Path) -> io::Result<()> {
    fs::rename(staging, target)
}
