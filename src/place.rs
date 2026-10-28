//! putting cached objects where a project expects them, by reflink, else hardlink, else copy.

mod synced;

use std::fs::{self, File};
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// how a file was placed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Method {
    /// a new file that shares the object's blocks until either one changes.
    Reflink,
    /// a second name for the object itself.
    Hardlink,
    /// a new file holding its own copy of the object's bytes.
    Copy,
}

/// puts objects into one folder tree, and remembers which methods its filesystem refuses.
pub(crate) struct Placer {
    synced: Option<&'static str>,
    can_reflink: bool,
    can_hardlink: bool,
    other_volume: bool,
}

impl Placer {
    /// prepares placement under `root`, and notes whether `root` sits inside a folder a sync client owns.
    pub(crate) fn new(root: &Path) -> Placer {
        let root = std::path::absolute(root).unwrap_or_else(|_| root.to_path_buf());
        let synced = synced::client(&root, &|name| std::env::var_os(name));
        Placer::starting_at(Method::Reflink, synced)
    }

    /// a placer that skips the methods above `first`, as if the filesystem had refused them.
    fn starting_at(first: Method, synced: Option<&'static str>) -> Placer {
        Placer {
            synced,
            can_reflink: first == Method::Reflink,
            can_hardlink: first != Method::Copy,
            other_volume: false,
        }
    }

    /// the sync client that owns `root`, if any: "onedrive", "dropbox", "icloud drive" or "google drive".
    pub(crate) fn synced(&self) -> Option<&'static str> {
        self.synced
    }

    /// whether links failed because the cache sits on another drive, which a cache on the same
    /// drive would fix.
    pub(crate) fn other_volume(&self) -> bool {
        self.other_volume
    }

    /// puts `object` at `destination`, which must not exist yet. creates missing parent folders.
    /// `writable` asks for a writable file, for tools that write into their inputs.
    pub(crate) fn place(
        &mut self,
        object: &Path,
        destination: &Path,
        writable: bool,
    ) -> io::Result<Method> {
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        if self.can_reflink {
            match reflink_copy::reflink(object, destination) {
                Ok(()) => {
                    seal(destination, writable)?;
                    return Ok(Method::Reflink);
                }
                Err(error) => self.learn(Method::Reflink, error)?,
            }
        }
        // sync clients break hardlinks or upload every link as a full file.
        if self.can_hardlink && !writable && self.synced.is_none() {
            match fs::hard_link(object, destination) {
                Ok(()) => return Ok(Method::Hardlink),
                Err(error) => self.learn(Method::Hardlink, error)?,
            }
        }
        copy(object, destination)?;
        seal(destination, writable)?;
        Ok(Method::Copy)
    }

    /// learns from `method` failing with `error`: a method the filesystem refuses is not tried
    /// again, and an error no other method can avoid, such as an existing destination, is returned.
    fn learn(&mut self, method: Method, error: io::Error) -> io::Result<()> {
        if error.kind() == io::ErrorKind::AlreadyExists {
            return Err(error);
        }
        if is_refusal(&error) {
            match method {
                Method::Reflink => self.can_reflink = false,
                Method::Hardlink => {
                    self.can_hardlink = false;
                    self.other_volume = error.kind() == io::ErrorKind::CrossesDevices;
                }
                Method::Copy => {}
            }
        }
        Ok(())
    }
}

/// whether `error` means the filesystem refuses the method that failed for every file, not only
/// for this one.
fn is_refusal(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::Unsupported | io::ErrorKind::CrossesDevices
    ) || error.raw_os_error().is_some_and(is_refusal_code)
}

/// whether a windows error code says the volume cannot link or clone at all.
#[cfg(windows)]
fn is_refusal_code(code: i32) -> bool {
    const INVALID_FUNCTION: u32 = 1;
    const NOT_SAME_DEVICE: u32 = 17;
    const NOT_SUPPORTED: u32 = 50;
    // reflink-copy reports windows errors as HRESULTs, which keep the code in their low 16 bits.
    let code = code.cast_unsigned();
    let code = if code >> 16 == 0x8007 {
        code & 0xffff
    } else {
        code
    };
    matches!(code, INVALID_FUNCTION | NOT_SAME_DEVICE | NOT_SUPPORTED)
}

/// whether a mac error code is `ENOTSUP`, which clonefile answers on a volume that cannot clone
/// and which std leaves uncategorized.
#[cfg(target_os = "macos")]
fn is_refusal_code(code: i32) -> bool {
    code == 45
}

/// linux reports every refusal with a code std already calls unsupported or cross-device.
#[cfg(not(any(windows, target_os = "macos")))]
fn is_refusal_code(_code: i32) -> bool {
    false
}

/// copies `object` to `destination`, failing when `destination` exists.
fn copy(object: &Path, destination: &Path) -> io::Result<()> {
    // std::fs::copy replaces an existing file, so the name is claimed first by an open that fails
    // when it exists.
    File::create_new(destination)?;
    undo_on_error(fs::copy(object, destination), destination).map(drop)
}

/// makes a new reflink or copy at `destination` writable or read-only, as asked. a hardlink is
/// never sealed, because its flag is the object's.
fn seal(destination: &Path, writable: bool) -> io::Result<()> {
    undo_on_error(set_writable(destination, writable), destination)
}

/// makes the file at `path` writable by its owner, or read-only for everyone.
#[cfg(unix)]
fn set_writable(path: &Path, writable: bool) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = fs::metadata(path)?.permissions();
    let mode = permissions.mode();
    permissions.set_mode(if writable {
        mode | 0o200
    } else {
        mode & !0o222
    });
    fs::set_permissions(path, permissions)
}

/// makes the file at `path` writable, or read-only.
#[cfg(not(unix))]
fn set_writable(path: &Path, writable: bool) -> io::Result<()> {
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_readonly(!writable);
    fs::set_permissions(path, permissions)
}

/// removes the file just made at `destination` when a later step failed, so placing it again
/// starts clean.
fn undo_on_error<T>(result: io::Result<T>, destination: &Path) -> io::Result<T> {
    if result.is_err() {
        let _ = fs::remove_file(destination);
    }
    result
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use tempfile::TempDir;

    use super::*;

    const CONTENTS: &str = "station,temperature\nlerwick,9.1\n";

    /// a temporary folder holding one cached object, read-only like every object in the cache
    /// unless `read_only` is false.
    fn cache(read_only: bool) -> (TempDir, PathBuf) {
        let folder = TempDir::new().unwrap();
        let object = folder.path().join("object");
        fs::write(&object, CONTENTS).unwrap();
        if read_only {
            let mut permissions = fs::metadata(&object).unwrap().permissions();
            permissions.set_readonly(true);
            fs::set_permissions(&object, permissions).unwrap();
        }
        (folder, object)
    }

    #[test]
    fn a_hardlink_is_chosen_on_one_volume() {
        let (folder, object) = cache(true);
        let destination = folder.path().join("placed.csv");
        let mut placer = Placer::starting_at(Method::Hardlink, None);

        let method = placer.place(&object, &destination, false).unwrap();

        assert_eq!(method, Method::Hardlink);
        assert_eq!(fs::read_to_string(&destination).unwrap(), CONTENTS);
    }

    #[test]
    fn starting_below_hardlinks_falls_back_to_a_copy() {
        let (folder, object) = cache(true);
        let destination = folder.path().join("placed.csv");
        let mut placer = Placer::starting_at(Method::Copy, None);

        let method = placer.place(&object, &destination, false).unwrap();

        assert_eq!(method, Method::Copy);
        assert_eq!(fs::read_to_string(&destination).unwrap(), CONTENTS);
    }

    #[test]
    fn missing_parent_folders_are_created() {
        let (folder, object) = cache(true);
        let destination = folder
            .path()
            .join("data")
            .join("weather")
            .join("placed.csv");
        let mut placer = Placer::starting_at(Method::Hardlink, None);

        placer.place(&object, &destination, false).unwrap();

        assert_eq!(fs::read_to_string(&destination).unwrap(), CONTENTS);
    }

    #[test]
    fn an_existing_destination_is_an_error_whatever_the_method() {
        for first in [Method::Reflink, Method::Hardlink, Method::Copy] {
            let (folder, object) = cache(true);
            let destination = folder.path().join("placed.csv");
            fs::write(&destination, "edited by hand").unwrap();
            let mut placer = Placer::starting_at(first, None);

            let error = placer.place(&object, &destination, false).unwrap_err();

            assert_eq!(error.kind(), io::ErrorKind::AlreadyExists, "{first:?}");
            assert_eq!(
                fs::read_to_string(&destination).unwrap(),
                "edited by hand",
                "{first:?}"
            );
        }
    }

    #[test]
    fn a_hardlink_keeps_the_objects_read_only_flag() {
        let (folder, object) = cache(true);
        let destination = folder.path().join("placed.csv");
        let mut placer = Placer::starting_at(Method::Hardlink, None);

        let method = placer.place(&object, &destination, false).unwrap();

        assert_eq!(method, Method::Hardlink);
        assert!(is_read_only(&destination));
        assert!(is_read_only(&object));
    }

    #[test]
    fn a_copy_is_made_read_only() {
        let (folder, object) = cache(false);
        let destination = folder.path().join("placed.csv");
        let mut placer = Placer::starting_at(Method::Copy, None);

        placer.place(&object, &destination, false).unwrap();

        assert!(is_read_only(&destination));
    }

    #[test]
    fn a_writable_placement_is_a_writable_copy() {
        let (folder, object) = cache(true);
        let destination = folder.path().join("placed.csv");
        let mut placer = Placer::starting_at(Method::Hardlink, None);

        let method = placer.place(&object, &destination, true).unwrap();

        assert_eq!(method, Method::Copy);
        assert!(!is_read_only(&destination));
        assert!(is_read_only(&object));
    }

    #[test]
    fn a_synced_folder_gets_a_copy_instead_of_a_hardlink() {
        let (folder, object) = cache(true);
        let destination = folder.path().join("placed.csv");
        let mut placer = Placer::starting_at(Method::Hardlink, Some("onedrive"));

        let method = placer.place(&object, &destination, false).unwrap();

        assert_eq!(method, Method::Copy);
    }

    #[test]
    fn the_whole_ladder_never_copies_within_one_volume() {
        let (folder, object) = cache(true);
        let mut placer = Placer::starting_at(Method::Reflink, None);

        for name in ["first.csv", "second.csv"] {
            let destination = folder.path().join(name);
            let method = placer.place(&object, &destination, false).unwrap();
            assert_ne!(method, Method::Copy, "{name}");
            assert_eq!(fs::read_to_string(&destination).unwrap(), CONTENTS);
        }
    }

    #[test]
    fn a_method_the_filesystem_refuses_is_not_tried_again() {
        let refusals = [
            io::Error::from(io::ErrorKind::Unsupported),
            io::Error::from(io::ErrorKind::CrossesDevices),
            // how NTFS refuses a reflink: "incorrect function", wrapped in an HRESULT.
            #[cfg(windows)]
            io::Error::from_raw_os_error(0x8007_0001_u32.cast_signed()),
            // ENOTSUP, how a mac volume without clones refuses one.
            #[cfg(target_os = "macos")]
            io::Error::from_raw_os_error(45),
        ];
        for refusal in refusals {
            let (folder, object) = cache(true);
            let mut placer = Placer::starting_at(Method::Hardlink, None);
            let described = refusal.to_string();

            placer.learn(Method::Hardlink, refusal).unwrap();
            let method = placer
                .place(&object, &folder.path().join("placed.csv"), false)
                .unwrap();

            assert_eq!(method, Method::Copy, "{described}");
        }
    }

    #[test]
    fn too_many_links_falls_back_for_that_file_alone() {
        let (folder, object) = cache(true);
        let mut placer = Placer::starting_at(Method::Hardlink, None);

        placer
            .learn(Method::Hardlink, io::ErrorKind::TooManyLinks.into())
            .unwrap();
        let method = placer
            .place(&object, &folder.path().join("placed.csv"), false)
            .unwrap();

        assert_eq!(method, Method::Hardlink);
    }

    fn is_read_only(path: &Path) -> bool {
        fs::metadata(path).unwrap().permissions().readonly()
    }
}
