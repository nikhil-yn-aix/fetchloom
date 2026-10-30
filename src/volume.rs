//! the volume under a folder fetchloom writes to: the cache, a project's data folder or the folder
//! `get` fills. FAT32 and exFAT cannot hardlink, and on windows they refuse to remove a read-only
//! file, which every object and, by default, every placed file is.

use std::fs::{self, File, Metadata, OpenOptions, Permissions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// how a volume links, renames and removes files, and whether it can hardlink at all, learned
/// the first time a removal needs to know.
#[derive(Debug)]
pub(crate) struct Volume {
    /// a folder of fetchloom's own on the volume, where the probe link is made: the cache's
    /// `tmp/`, a project's `.fetchloom`, or the folder `get` fills.
    scratch: PathBuf,
    hard_link: fn(&Path, &Path) -> io::Result<()>,
    rename: fn(&Path, &Path) -> io::Result<()>,
    remove_file: fn(&Path) -> io::Result<()>,
    links: OnceLock<bool>,
}

impl Volume {
    pub(crate) fn new(scratch: PathBuf) -> Volume {
        Volume {
            scratch,
            hard_link: |original, link| fs::hard_link(original, link),
            rename: |from, to| fs::rename(from, to),
            remove_file: |path| fs::remove_file(path),
            links: OnceLock::new(),
        }
    }

    pub(crate) fn hard_link(&self, original: &Path, link: &Path) -> io::Result<()> {
        (self.hard_link)(original, link)
    }

    pub(crate) fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        (self.rename)(from, to)
    }

    /// renames a verified replacement over an old file without leaving its name empty. windows
    /// may require clearing the old file's read-only flag; restoring it through the same handle
    /// also restores any placed hardlinks, without changing the new file at the name.
    pub(crate) fn replace(&self, from: &Path, to: &Path) -> io::Result<()> {
        let refused = match self.rename(from, to) {
            Err(err) if err.kind() == io::ErrorKind::PermissionDenied => err,
            renamed => return renamed,
        };
        let file = open_attributes(to)?;
        let metadata = file.metadata()?;
        let before = metadata.permissions();
        if !is_plain(&metadata) || !before.readonly() {
            return Err(refused);
        }
        file.set_permissions(writable(before.clone()))?;
        let renamed = self.rename(from, to);
        let restored = file.set_permissions(before);
        renamed.and(restored)
    }

    /// removes the file at `path`. FAT32 and exFAT refuse to remove a read-only file on windows.
    /// a read-only file on a volume that cannot hardlink, as they cannot, has no other name, so
    /// it is made writable for a second try. on a volume that can hardlink the refusal stands:
    /// read-only belongs to the file rather than one name, and clearing it could make a
    /// project's placed hardlink of an object writable.
    pub(crate) fn remove(&self, path: &Path) -> io::Result<()> {
        match (self.remove_file)(path) {
            Err(refused) if refused.kind() == io::ErrorKind::PermissionDenied => {
                self.remove_read_only(path).unwrap_or(Err(refused))
            }
            removed => removed,
        }
    }

    /// removes the file at `path`, which fetchloom recorded as a hardlink of a cached object. its
    /// read-only flag is the object's too, so it is never cleared, whatever the volume.
    pub(crate) fn remove_hardlink(&self, path: &Path) -> io::Result<()> {
        (self.remove_file)(path)
    }

    /// clears the read-only flag of the plain file at `path` and removes it, putting the flag
    /// back when that fails too. none when the file is not a read-only plain file or may have
    /// another name. the flag changes through a handle that never follows a link, so a link
    /// swapped in for the file cannot pass the change to a file outside the cache.
    fn remove_read_only(&self, path: &Path) -> Option<io::Result<()>> {
        let file = open_attributes(path).ok()?;
        let before = file.metadata().ok().filter(is_plain)?.permissions();
        if !before.readonly() || self.can_hardlink() != Some(false) {
            return None;
        }
        file.set_permissions(writable(before.clone())).ok()?;
        let removed = (self.remove_file)(path);
        if removed.is_err() {
            let _ = file.set_permissions(before);
        }
        Some(removed)
    }

    /// whether the volume can hardlink, learned once by linking a new hidden file in the scratch
    /// folder. none when the probe failed in a way that says nothing about links.
    pub(crate) fn can_hardlink(&self) -> Option<bool> {
        if let Some(links) = self.links.get() {
            return Some(*links);
        }
        let [probe, link] = ["probe", "link"].map(|ending| {
            self.scratch
                .join(format!(".fetchloom-{:016x}.{ending}", fastrand::u64(..)))
        });
        create_hidden(&probe).ok()?;
        let linked = (self.hard_link)(&probe, &link);
        let _ = fs::remove_file(&probe);
        let links = match linked {
            Ok(()) => {
                let _ = fs::remove_file(&link);
                true
            }
            Err(err) if refuses_links(&err) => false,
            Err(_) => return None,
        };
        Some(*self.links.get_or_init(|| links))
    }
}

/// whether `err`, from making a reflink or a hardlink, says the volume makes no such link at all.
/// FAT32 and exFAT refuse hardlinks with code 1: `ERROR_INVALID_FUNCTION` on windows, `EPERM` on
/// linux. `EPERM` can also refuse one file alone, such as another user's under linux's
/// `protected_hardlinks`, which only a probe on a file of fetchloom's own tells apart.
pub(crate) fn refuses_links(err: &io::Error) -> bool {
    err.kind() == io::ErrorKind::Unsupported || err.raw_os_error().is_some_and(refusal_code)
}

/// whether `err` says the two files sit on different volumes, as a link from a cache on another
/// drive does.
pub(crate) fn crosses_volumes(err: &io::Error) -> bool {
    #[cfg(windows)]
    {
        const NOT_SAME_DEVICE: u32 = 17;
        if err
            .raw_os_error()
            .is_some_and(|code| win32(code) == NOT_SAME_DEVICE)
        {
            return true;
        }
    }
    err.kind() == io::ErrorKind::CrossesDevices
}

/// `ERROR_INVALID_FUNCTION` and `ERROR_NOT_SUPPORTED`, the codes of a windows volume that cannot
/// link or clone at all.
#[cfg(windows)]
fn refusal_code(code: i32) -> bool {
    const INVALID_FUNCTION: u32 = 1;
    const NOT_SUPPORTED: u32 = 50;
    matches!(win32(code), INVALID_FUNCTION | NOT_SUPPORTED)
}

/// the windows error code `code` carries. reflink-copy reports windows errors as HRESULTs, which
/// keep the code in their low 16 bits.
#[cfg(windows)]
fn win32(code: i32) -> u32 {
    let code = code.cast_unsigned();
    if code >> 16 == 0x8007 {
        code & 0xffff
    } else {
        code
    }
}

/// `EPERM`. linux reports the other refusals with codes std already calls unsupported.
#[cfg(not(windows))]
fn refusal_code(code: i32) -> bool {
    code == 1
}

/// makes a new empty file that file browsers do not show: windows hides it by its attribute,
/// other systems by the dot its name starts with.
pub(crate) fn create_hidden(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_ATTRIBUTE_HIDDEN: u32 = 2;
        options.attributes(FILE_ATTRIBUTE_HIDDEN);
    }
    options.open(path)
}

/// opens `path` only to read and change its attributes, never following a link at its end.
fn open_attributes(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_READ_ATTRIBUTES: u32 = 0x80;
        const FILE_WRITE_ATTRIBUTES: u32 = 0x100;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options
            .access_mode(FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    #[cfg(unix)]
    {
        use rustix::fs::OFlags;
        use std::os::unix::fs::OpenOptionsExt;
        // a fifo would hold an open for reading until something wrote to it
        options
            .read(true)
            .custom_flags((OFlags::NOFOLLOW | OFlags::NONBLOCK).bits().cast_signed());
    }
    options.open(path)
}

/// whether `metadata` describes a plain file, not a link, a folder or a device.
pub(crate) fn is_plain(metadata: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return false;
        }
    }
    metadata.file_type().is_file()
}

/// `permissions` that let the file's owner write it.
fn writable(mut permissions: Permissions) -> Permissions {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        permissions.set_mode(permissions.mode() | 0o200);
    }
    #[cfg(not(unix))]
    #[expect(
        clippy::permissions_set_readonly_false,
        reason = "windows has one read-only flag, not write bits for other users"
    )]
    permissions.set_readonly(false);
    permissions
}

#[cfg(test)]
impl Volume {
    /// a volume that refuses to remove a read-only file, as FAT32 and exFAT do on windows, and
    /// that hardlinks only when `links` says so, which FAT cannot.
    pub(crate) fn refusing(scratch: PathBuf, links: bool) -> Volume {
        let hard_link: fn(&Path, &Path) -> io::Result<()> = if links {
            |original, link| fs::hard_link(original, link)
        } else {
            // what FAT32 and exFAT answer
            |_, _| Err(io::Error::from_raw_os_error(1))
        };
        Volume {
            scratch,
            hard_link,
            rename: |from, to| {
                if fs::symlink_metadata(to).is_ok_and(|metadata| metadata.permissions().readonly()) {
                    return Err(io::ErrorKind::PermissionDenied.into());
                }
                fs::rename(from, to)
            },
            remove_file: |path| {
                if fs::symlink_metadata(path)?.permissions().readonly() {
                    return Err(io::ErrorKind::PermissionDenied.into());
                }
                fs::remove_file(path)
            },
            links: OnceLock::new(),
        }
    }

    /// this volume, losing every rename to another writer: just before the rename, that writer
    /// publishes the same file read-only at the destination, and the rename is then refused, as
    /// windows refuses to rename over a read-only file.
    pub(crate) fn losing_renames(self) -> Volume {
        Volume {
            rename: |from, to| {
                fs::copy(from, to)?;
                crate::cache::tests::seal(to);
                Err(io::ErrorKind::PermissionDenied.into())
            },
            ..self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::tests::{hardlinks, hold, seal};

    /// a read-only file, as every object is.
    fn sealed(path: &Path) {
        fs::write(path, b"abc").unwrap();
        seal(path);
    }

    fn volume(dir: &Path, links: bool) -> Volume {
        let tmp = dir.join("tmp");
        fs::create_dir(&tmp).unwrap();
        Volume::refusing(tmp, links)
    }

    fn probes_left(dir: &Path) -> usize {
        fs::read_dir(dir.join("tmp")).unwrap().count()
    }

    #[test]
    fn a_volume_without_hardlinks_removes_a_read_only_file() {
        let dir = tempfile::tempdir().unwrap();
        let fat = volume(dir.path(), false);
        let object = dir.path().join("object");
        sealed(&object);
        fat.remove(&object).unwrap();
        assert!(!object.exists());
        assert_eq!(probes_left(dir.path()), 0);
    }

    #[test]
    fn a_volume_with_hardlinks_keeps_the_flag_a_placed_link_shares() {
        let dir = tempfile::tempdir().unwrap();
        // FAT32 and exFAT cannot hardlink, so no placed file can share an object there
        if !hardlinks(dir.path()) {
            return;
        }
        let volume = volume(dir.path(), true);
        let object = dir.path().join("object");
        sealed(&object);
        let placed = dir.path().join("placed.csv");
        fs::hard_link(&object, &placed).unwrap();
        let refused = volume.remove(&object).unwrap_err();
        assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied);
        assert!(object.exists());
        assert!(fs::metadata(&placed).unwrap().permissions().readonly());
        assert_eq!(probes_left(dir.path()), 0);
    }

    #[test]
    fn a_file_refused_again_stays_read_only() {
        let dir = tempfile::tempdir().unwrap();
        let fat = volume(dir.path(), false);
        let folder = dir.path().join("objects");
        fs::create_dir(&folder).unwrap();
        let object = folder.join("object");
        sealed(&object);
        let held = hold(&object);
        let removed = fat.remove(&object).is_ok();
        drop(held);
        // root on unix removes it anyway
        assert_eq!(removed, !object.exists());
        if !removed {
            assert!(fs::metadata(&object).unwrap().permissions().readonly());
        }
    }
}
