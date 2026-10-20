//! What the store does differently on windows: the read only attribute in place of mode bits,
//! file identity from the creation time, and renames over read only files.

use std::fs::{self, File, Metadata};
use std::io;
use std::os::windows::fs::OpenOptionsExt;
use std::path::Path;
use std::time::SystemTime;

use crate::fingerprint::nanos;
use crate::tier::Sharing;

/// `FILE_READ_ATTRIBUTES`: read a file's attributes without reading its bytes.
const READ_ATTRIBUTES: u32 = 0x80;

/// `FILE_WRITE_ATTRIBUTES`: change a file's attributes and times without writing its bytes.
const WRITE_ATTRIBUTES: u32 = 0x100;

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

/// Sets the modification time of the file at `path` to now, through a handle that may write
/// attributes, which a read only file allows.
pub(crate) fn freshen(path: &Path) -> io::Result<()> {
    fs::OpenOptions::new()
        .access_mode(WRITE_ATTRIBUTES)
        .open(path)?
        .set_modified(SystemTime::now())
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
    match fs::rename(staging, target) {
        Err(error)
            if error.kind() == io::ErrorKind::PermissionDenied
                && fs::symlink_metadata(target).is_ok_and(|m| m.permissions().readonly()) =>
        {
            let replaced = fs::OpenOptions::new()
                .access_mode(READ_ATTRIBUTES | WRITE_ATTRIBUTES)
                .open(target)?;
            set_handle_readonly(&replaced, false)?;
            let renamed = fs::rename(staging, target);
            set_handle_readonly(&replaced, true)?;
            renamed
        }
        done => done,
    }
}

/// Renames `scratch` to `target` and says it renamed. Every object and record is read only
/// before it is placed, and windows refuses to rename over a read only file, so of writers
/// racing for one name exactly one places it, at the cost of one call instead of a link and a
/// removal.
pub(crate) fn publish(scratch: &Path, target: &Path) -> io::Result<bool> {
    fs::rename(scratch, target).map(|()| true)
}

fn set_handle_readonly(file: &File, readonly: bool) -> io::Result<()> {
    let mut permissions = file.metadata()?.permissions();
    permissions.set_readonly(readonly);
    file.set_permissions(permissions)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publishing_onto_a_read_only_object_fails_and_leaves_it() {
        let temp = tempfile::tempdir().unwrap();
        let (scratch, target) = (temp.path().join("scratch"), temp.path().join("target"));
        fs::write(&scratch, b"new").unwrap();
        fs::write(&target, b"old").unwrap();
        set_writable(&target, false).unwrap();
        let error = publish(&scratch, &target).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(fs::read(&target).unwrap(), b"old");
        set_writable(&target, true).unwrap();
        fs::remove_file(&target).unwrap();
        assert!(publish(&scratch, &target).unwrap());
        assert_eq!(fs::read(&target).unwrap(), b"new");
    }
}
