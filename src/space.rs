//! free space on the disk a run downloads to, so a download too big for it is told before it
//! starts, and the largest file a disk holds, so a file FAT32 cannot hold is refused first.

use std::path::Path;

use crate::error::{Error, Kind};

/// the most bytes one file holds on FAT32, which keeps a file's size in 32 bits: 4 GiB less one
/// byte. exFAT keeps it in 64.
pub(crate) const FAT_LARGEST: u64 = (1 << 32) - 1;

/// what a disk is formatted as, as far as fetchloom's work changes with it: FAT and exFAT make
/// no links and change one folder at a time, and FAT holds no file past [`FAT_LARGEST`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Filesystem {
    /// FAT32, or FAT12 and FAT16 on smaller disks.
    Fat,
    Exfat,
    Other,
}

impl Filesystem {
    /// the filesystem a name from macos or windows stands for: "msdos" and "FAT32" are FAT.
    #[cfg(any(not(target_os = "linux"), debug_assertions, test))]
    pub(crate) fn named(name: &str) -> Filesystem {
        let is = |names: &[&str]| names.iter().any(|known| name.eq_ignore_ascii_case(known));
        if is(&["exfat"]) {
            Filesystem::Exfat
        } else if is(&["vfat", "fat", "fat12", "fat16", "fat32", "msdos", "msdosfs"]) {
            Filesystem::Fat
        } else {
            Filesystem::Other
        }
    }

    /// the filesystem a linux `statfs` magic number stands for. vfat and msdos share one.
    #[cfg(any(target_os = "linux", test))]
    pub(crate) fn from_magic(magic: i128) -> Filesystem {
        match magic {
            0x4d44 => Filesystem::Fat,
            0x2011_bab0 => Filesystem::Exfat,
            _ => Filesystem::Other,
        }
    }

    /// the filesystem holding `path`, which must exist. one the system does not name is
    /// `Other`.
    pub(crate) fn of(path: &Path) -> Filesystem {
        Filesystem::chosen().unwrap_or_else(|| probed(path))
    }

    /// a disk of a chosen format, for CLI tests. release builds never read it.
    pub(crate) fn chosen() -> Option<Filesystem> {
        #[cfg(debug_assertions)]
        if let Ok(name) = std::env::var("FETCHLOOM_TEST_FILESYSTEM") {
            return Some(Filesystem::named(&name));
        }
        None
    }

    /// the filesystem `stat`, from `statfs` or `fstatfs`, describes.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) fn of_stat(stat: &rustix::fs::StatFs) -> Filesystem {
        #[cfg(target_os = "linux")]
        {
            Filesystem::from_magic(i128::from(stat.f_type))
        }
        #[cfg(target_os = "macos")]
        {
            let name: Vec<u8> = stat
                .f_fstypename
                .iter()
                .take_while(|&&byte| byte != 0)
                .map(|byte| byte.cast_unsigned())
                .collect();
            Filesystem::named(&String::from_utf8_lossy(&name))
        }
    }

    /// whether its folders change one at a time, as on FAT and exFAT, so changing several at
    /// once only queues the changes.
    pub(crate) fn serializes(self) -> bool {
        self != Filesystem::Other
    }

    /// the most bytes one file holds on this filesystem, when that is less than any disk holds.
    pub(crate) fn largest_file(self) -> Option<u64> {
        (self == Filesystem::Fat).then_some(FAT_LARGEST)
    }

    pub(crate) fn holds(self, size: u64) -> bool {
        self.largest_file().is_none_or(|largest| size <= largest)
    }
}

/// refuses, before any of `files` downloads, the first the disk holding the cache at `cache`
/// cannot hold, as `local.io` naming the file, its size and what to do.
pub(crate) fn cache_holds<'f>(
    cache: &Path,
    files: impl IntoIterator<Item = (&'f str, u64)>,
) -> Result<(), Error> {
    let mut large = files
        .into_iter()
        .filter(|(_, size)| *size > FAT_LARGEST)
        .peekable();
    if large.peek().is_none() {
        return Ok(());
    }
    let filesystem = Filesystem::of(cache);
    match large.find(|(_, size)| !filesystem.holds(*size)) {
        Some((what, size)) => Err(Error::new(
            Kind::LocalIo,
            too_large(format_args!("the cache at {}", cache.display()), what, size),
        )
        .help("set FETCHLOOM_CACHE to a folder on a disk formatted as exFAT, NTFS, APFS or ext4")),
        None => Ok(()),
    }
}

/// why `what`, a file of `size` bytes, cannot go to `place`, which is on a FAT disk.
pub(crate) fn too_large(place: impl std::fmt::Display, what: &str, size: u64) -> String {
    format!(
        "{place} is on a FAT32 disk, which holds no file of 4 GiB or more, and {what} is {}",
        crate::units::size(size)
    )
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn probed(path: &Path) -> Filesystem {
    rustix::fs::statfs(path).map_or(Filesystem::Other, |stat| Filesystem::of_stat(&stat))
}

#[cfg(windows)]
fn probed(path: &Path) -> Filesystem {
    let Ok(path) = std::fs::canonicalize(path) else {
        return Filesystem::Other;
    };
    let Ok(disks) =
        sysinfo::Disks::new_with_refreshed_list_specifics(sysinfo::DiskRefreshKind::nothing())
    else {
        return Filesystem::Other;
    };
    disks
        .iter()
        .filter_map(|disk| {
            let mount = std::fs::canonicalize(disk.mount_point()).ok()?;
            path.starts_with(&mount)
                .then_some((mount.components().count(), disk))
        })
        .max_by_key(|(length, _)| *length)
        .and_then(|(_, disk)| disk.file_system().to_str().map(Filesystem::named))
        .unwrap_or(Filesystem::Other)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn probed(_: &Path) -> Filesystem {
    Filesystem::Other
}

/// the bytes this user may still write on the disk holding `path`, which must exist. none when the
/// system does not say.
pub(crate) fn free(path: &Path) -> Option<u64> {
    // a disk of a chosen size, for CLI tests; release builds never read it
    #[cfg(debug_assertions)]
    if let Some(bytes) = std::env::var("FETCHLOOM_TEST_FREE")
        .ok()
        .and_then(|text| text.parse().ok())
    {
        return Some(bytes);
    }
    measured(path)
}

#[cfg(unix)]
fn measured(path: &Path) -> Option<u64> {
    let stat = rustix::fs::statvfs(path).ok()?;
    stat.f_bavail.checked_mul(stat.f_frsize)
}

#[cfg(windows)]
fn measured(path: &Path) -> Option<u64> {
    let path = std::fs::canonicalize(path).ok()?;
    let disks = sysinfo::Disks::new_with_refreshed_list_specifics(
        sysinfo::DiskRefreshKind::nothing().with_storage(),
    )
    .ok()?;
    disks
        .iter()
        .filter_map(|disk| {
            let mount = std::fs::canonicalize(disk.mount_point()).ok()?;
            path.starts_with(&mount)
                .then_some((mount.components().count(), disk.available_space()))
        })
        .max_by_key(|(length, _)| *length)
        .map(|(_, free)| free)
}

#[cfg(not(any(unix, windows)))]
fn measured(_: &Path) -> Option<u64> {
    None
}

/// the total bytes of the disk holding `path`, which must exist; none when the system does not say.
pub(crate) fn total(path: &Path) -> Option<u64> {
    sized(path)
}

#[cfg(unix)]
fn sized(path: &Path) -> Option<u64> {
    let stat = rustix::fs::statvfs(path).ok()?;
    stat.f_blocks.checked_mul(stat.f_frsize)
}

#[cfg(windows)]
fn sized(path: &Path) -> Option<u64> {
    let path = std::fs::canonicalize(path).ok()?;
    let disks = sysinfo::Disks::new_with_refreshed_list_specifics(
        sysinfo::DiskRefreshKind::nothing().with_storage(),
    )
    .ok()?;
    disks
        .iter()
        .filter_map(|disk| {
            let mount = std::fs::canonicalize(disk.mount_point()).ok()?;
            path.starts_with(&mount)
                .then_some((mount.components().count(), disk.total_space()))
        })
        .max_by_key(|(length, _)| *length)
        .map(|(_, total)| total)
}

#[cfg(not(any(unix, windows)))]
fn sized(_: &Path) -> Option<u64> {
    None
}

/// the least free space a step may leave on a disk of `total` bytes: `set`, as the `build_floor`
/// setting gives it, else the larger of 1 GiB and 1% of the disk.
pub(crate) fn floor(set: Option<u64>, total: Option<u64>) -> u64 {
    set.unwrap_or_else(|| (1 << 30).max(total.unwrap_or(0) / 100))
}

/// what one step of a build may take on the disk holding the cache, as far as fetchloom can tell
/// before it runs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Needs {
    /// the files it reads that the cache lacks, as their downloads bring them, with what an
    /// archive among them unpacks to, or its size again while that is not known.
    pub(crate) downloads: u64,
    /// the files it reads, which its private copies take where the disk does not clone files.
    pub(crate) inputs: u64,
    /// the result data.lock pinned for it last, none when it was never built.
    pub(crate) result: Option<u64>,
}

/// the most a build of `steps` may take on the cache's disk: every download and result stays, while
/// only the largest step's copies count, and only where the disk does not clone files.
pub(crate) fn peak(steps: &[Needs], clones: bool) -> u64 {
    let stays = steps
        .iter()
        .map(|step| step.downloads.saturating_add(step.result.unwrap_or(0)))
        .fold(0, u64::saturating_add);
    let copies = if clones {
        0
    } else {
        steps.iter().map(|step| step.inputs).max().unwrap_or(0)
    };
    stays.saturating_add(copies)
}

/// how far `needed` bytes pass what a disk has `free`, none when they fit or the disk does not say.
pub(crate) fn short(needed: u64, free: Option<u64>) -> Option<u64> {
    free.and_then(|free| needed.checked_sub(free))
        .filter(|short| *short > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FAT32 holds a file of 4 GiB less one byte and nothing larger; exFAT holds larger ones.
    #[test]
    fn fat32_holds_files_up_to_4_gib_less_one_byte() {
        for (filesystem, size, holds) in [
            (Filesystem::Fat, FAT_LARGEST - 1, true),
            (Filesystem::Fat, FAT_LARGEST, true),
            (Filesystem::Fat, FAT_LARGEST + 1, false),
            (Filesystem::Fat, u64::MAX, false),
            (Filesystem::Exfat, FAT_LARGEST + 1, true),
            (Filesystem::Other, u64::MAX, true),
        ] {
            assert_eq!(filesystem.holds(size), holds, "{filesystem:?} {size}");
        }
        assert_eq!(FAT_LARGEST, 4_294_967_295);
    }

    #[test]
    fn filesystems_are_told_apart_by_name_and_magic() {
        for name in ["vfat", "msdos", "FAT32", "fat", "FAT16", "msdosfs"] {
            assert_eq!(Filesystem::named(name), Filesystem::Fat, "{name}");
        }
        for name in ["exfat", "exFAT"] {
            assert_eq!(Filesystem::named(name), Filesystem::Exfat, "{name}");
        }
        for name in ["ntfs", "refs", "apfs", "ext4", "xfs", "not-exfat", ""] {
            assert_eq!(Filesystem::named(name), Filesystem::Other, "{name}");
        }
        assert_eq!(Filesystem::from_magic(0x4d44), Filesystem::Fat);
        assert_eq!(Filesystem::from_magic(0x2011_bab0), Filesystem::Exfat);
        for magic in [-1, 0, 0xef53, 0x5846_5342] {
            assert_eq!(Filesystem::from_magic(magic), Filesystem::Other);
        }
    }

    /// downloads and results add up, while copies count once, for the step that reads most.
    #[test]
    fn the_estimate_counts_copies_only_without_clones() {
        let steps = [
            Needs {
                downloads: 50,
                inputs: 100,
                result: Some(20),
            },
            Needs {
                downloads: 5,
                inputs: 300,
                result: None,
            },
        ];
        assert_eq!(peak(&steps, true), 75);
        assert_eq!(peak(&steps, false), 375);
        assert_eq!(peak(&[], false), 0);
    }

    /// a floor set is kept as it is; otherwise it is 1% of the disk, never under 1 GiB.
    #[test]
    fn the_floor_is_the_setting_or_a_share_of_the_disk() {
        assert_eq!(floor(Some(5), Some(1 << 40)), 5);
        assert_eq!(floor(None, Some(1 << 40)), (1 << 40) / 100);
        assert_eq!(floor(None, Some(10 << 30)), 1 << 30);
        assert_eq!(floor(None, None), 1 << 30);
    }

    #[test]
    fn only_bytes_past_what_is_free_fall_short() {
        assert_eq!(short(10, Some(4)), Some(6));
        assert_eq!(short(10, Some(10)), None);
        assert_eq!(short(10, Some(11)), None);
        assert_eq!(short(0, Some(0)), None);
        // a disk that does not say is never short
        assert_eq!(short(u64::MAX, None), None);
    }

    #[test]
    fn a_real_disk_tells_its_free_space() {
        let folder = tempfile::tempdir().unwrap();
        assert!(measured(folder.path()).is_some_and(|free| free > 0));
        assert_eq!(measured(&folder.path().join("missing")), None);
        let free = measured(folder.path()).unwrap_or_default();
        assert!(sized(folder.path()).is_some_and(|total| total >= free));
    }
}
