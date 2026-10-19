//! Modes the store sets on what it creates. The process umask is never trusted.

use std::fs::{self, File};
use std::io;
use std::path::Path;

use crate::tier::Sharing;

/// Creates the directory `path` and sets the mode `sharing` asks for. An existing directory is
/// left as it is, since another user of a shared tier may own it.
pub(crate) fn create_dir(path: &Path, sharing: Sharing) -> io::Result<()> {
    match fs::create_dir(path) {
        Ok(()) => set_dir_mode(path, sharing),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(unix)]
fn set_dir_mode(path: &Path, sharing: Sharing) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = match sharing {
        Sharing::Private => 0o755,
        Sharing::Group => 0o2775,
    };
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

#[cfg(windows)]
#[expect(
    clippy::unnecessary_wraps,
    reason = "windows directories carry no mode bits, and callers share the unix signature"
)]
fn set_dir_mode(_path: &Path, _sharing: Sharing) -> io::Result<()> {
    Ok(())
}

/// Makes the open file read only: mode 0444 on unix, the read only attribute on windows.
pub(crate) fn seal(file: &File) -> io::Result<()> {
    #[cfg(unix)]
    let permissions = {
        use std::os::unix::fs::PermissionsExt;
        fs::Permissions::from_mode(0o444)
    };
    #[cfg(windows)]
    let permissions = {
        let mut permissions = file.metadata()?.permissions();
        permissions.set_readonly(true);
        permissions
    };
    file.set_permissions(permissions)
}
