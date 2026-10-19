//! What the store does differently on windows: the read only attribute in place of mode bits,
//! file identity from the creation time, and renames over read only files.

use std::fs::{self, File, Metadata};
use std::io;
use std::os::windows::fs::OpenOptionsExt;
use std::path::Path;

use crate::fingerprint::nanos;
use crate::tier::Sharing;

/// Directories carry no mode bits on windows.
#[expect(
    clippy::unnecessary_wraps,
    reason = "windows directories carry no mode bits, and callers share the unix signature"
)]
pub(crate) fn set_dir_mode(_path: &Path, _sharing: Sharing) -> io::Result<()> {
    Ok(())
}

/// Clears or sets the read only attribute of the file at `path`.
pub(crate) fn set_writable(path: &Path, writable: bool) -> io::Result<()> {
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_readonly(!writable);
    fs::set_permissions(path, permissions)
}

/// Sets the read only attribute through the open handle.
pub(crate) fn seal(file: &File) -> io::Result<()> {
    set_handle_readonly(file, true)
}

/// Reserves `length` bytes for `file`, so a full disk fails before anything is written.
pub(crate) fn reserve(file: &File, length: u64) -> io::Result<()> {
    file.set_len(length)
}

/// What tells one file from another: its creation time, which a listing reports for free.
pub(crate) fn identity(metadata: &Metadata) -> u128 {
    nanos(metadata.created().ok())
}

/// Reflinks are not tried on windows yet: the crate that would make them has no tested windows
/// path.
pub(crate) fn reflink(_object: &Path, _to: &Path) -> io::Result<()> {
    Err(io::ErrorKind::Unsupported.into())
}

/// Creates a file symlink at `to` pointing to `object`.
pub(crate) fn symlink(object: &Path, to: &Path) -> io::Result<()> {
    std::os::windows::fs::symlink_file(object, to)
}

/// Renames `staging` over `target`. Windows refuses to rename over a read only file, and a
/// hardlinked destination shares its read only attribute with the object, so the attribute is
/// cleared through a handle, the rename runs, and the same handle sets it again on the file that
/// was replaced.
pub(crate) fn replace(staging: &Path, target: &Path) -> io::Result<()> {
    const READ_AND_WRITE_ATTRIBUTES: u32 = 0x80 | 0x100;
    match fs::rename(staging, target) {
        Err(error)
            if error.kind() == io::ErrorKind::PermissionDenied
                && fs::symlink_metadata(target).is_ok_and(|m| m.permissions().readonly()) =>
        {
            let replaced = fs::OpenOptions::new()
                .access_mode(READ_AND_WRITE_ATTRIBUTES)
                .open(target)?;
            set_handle_readonly(&replaced, false)?;
            let renamed = fs::rename(staging, target);
            set_handle_readonly(&replaced, true)?;
            renamed
        }
        done => done,
    }
}

fn set_handle_readonly(file: &File, readonly: bool) -> io::Result<()> {
    let mut permissions = file.metadata()?.permissions();
    permissions.set_readonly(readonly);
    file.set_permissions(permissions)
}
