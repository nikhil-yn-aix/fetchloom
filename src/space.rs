//! free space on the disk a run downloads to, so a download bigger than its disk is told before it
//! starts rather than once the disk fills.

use std::path::Path;

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
    );
    disks
        .list()
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

/// how far `needed` bytes pass what a disk has `free`, none when they fit or the disk does not say.
pub(crate) fn short(needed: u64, free: Option<u64>) -> Option<u64> {
    free.and_then(|free| needed.checked_sub(free))
        .filter(|short| *short > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_bytes_past_what_is_free_fall_short() {
        assert_eq!(short(10, Some(4)), Some(6));
        assert_eq!(short(10, Some(10)), None);
        assert_eq!(short(10, Some(11)), None);
        assert_eq!(short(0, Some(0)), None);
        // a disk that does not say is never short
        assert_eq!(short(u64::MAX, None), None);
    }

    #[cfg(unix)]
    #[test]
    fn a_real_disk_tells_its_free_space() {
        let folder = tempfile::tempdir().unwrap();
        assert!(measured(folder.path()).is_some_and(|free| free > 0));
        assert_eq!(measured(&folder.path().join("missing")), None);
    }
}
