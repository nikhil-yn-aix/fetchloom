//! what this machine gives a run: its processors, the memory a run may use and how many files the
//! process may hold open. read once, when a run starts.

#[cfg(any(target_os = "linux", test))]
use std::fs;
#[cfg(any(target_os = "linux", test))]
use std::path::Path;

/// the open files a run asks for when its soft limit is lower, as many downloads, archive
/// writers and placement workers can hold a file each. uv, openjdk and graalvm stop here too:
/// some code breaks past it, and code that walks every possible descriptor slows down.
#[cfg(unix)]
const OPEN_FILES: u64 = 1 << 20;
/// what macos grants one process when it refuses [`OPEN_FILES`].
#[cfg(unix)]
const OPEN_MAX: u64 = 10_240;

/// what this machine gives a run. its default stands for a machine that says nothing of itself.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Machine {
    /// processors the run may use, after a container's cpu quota and the affinity mask.
    pub(crate) processors: usize,
    /// bytes of memory a run may use: on linux what the system has available, or what is left
    /// under a container's memory limit when that is less. none elsewhere.
    pub(crate) memory: Option<u64>,
    /// files the process may hold open, after raising its soft limit as far as the system lets
    /// it. none where the system sets no such limit.
    pub(crate) open_files: Option<u64>,
}

impl Machine {
    /// reads this machine, raising the process's open-file limit on the way.
    pub(crate) fn read() -> Machine {
        Machine {
            processors: std::thread::available_parallelism().map_or(1, std::num::NonZero::get),
            memory: memory(),
            open_files: raise_open_files(),
        }
    }
}

#[cfg(target_os = "linux")]
fn memory() -> Option<u64> {
    memory_under(Path::new("/"))
}

/// other systems tell their free memory only through calls this crate keeps out, as it holds
/// no unsafe code, so a run there uses `FETCHLOOM_MEMORY` or the smallest budget.
#[cfg(not(target_os = "linux"))]
fn memory() -> Option<u64> {
    None
}

/// the memory a process may use, from `/proc` and `/sys` below `root`: what the kernel reports
/// available, or less when the process's cgroups leave less room under their limits.
#[cfg(any(target_os = "linux", test))]
fn memory_under(root: &Path) -> Option<u64> {
    let free = available(&root.join("proc/meminfo"));
    match (free, cgroup_room(root)) {
        (Some(free), Some(limit)) => Some(free.min(limit)),
        (free, limit) => free.or(limit),
    }
}

#[cfg(any(target_os = "linux", test))]
fn available(meminfo: &Path) -> Option<u64> {
    let text = fs::read_to_string(meminfo).ok()?;
    let line = text
        .lines()
        .find_map(|line| line.strip_prefix("MemAvailable:"))?;
    let kib: u64 = line.trim().strip_suffix("kB")?.trim().parse().ok()?;
    kib.checked_mul(1024)
}

/// the least room left under the memory limits of this process's cgroup and of each group above
/// it. a container may mount its own group at the root of `/sys/fs/cgroup` while
/// `/proc/self/cgroup` still names the full path, so the groups that do not exist are skipped.
#[cfg(any(target_os = "linux", test))]
fn cgroup_room(root: &Path) -> Option<u64> {
    let groups = fs::read_to_string(root.join("proc/self/cgroup")).ok()?;
    if let Some(path) = groups.lines().find_map(|line| line.strip_prefix("0::")) {
        return least(&root.join("sys/fs/cgroup"), path, |group| {
            // `memory.high` throttles a group well before `memory.max` kills it, and either may
            // be `max` while the other is set
            let bound = match (
                limit(&group.join("memory.max")),
                limit(&group.join("memory.high")),
            ) {
                (Some(max), Some(high)) => max.min(high),
                (bound, other) => bound.or(other)?,
            };
            let current = number(&group.join("memory.current")).unwrap_or(0);
            Some(bound.saturating_sub(current))
        });
    }
    let path = groups.lines().find_map(|line| {
        let mut fields = line.splitn(3, ':');
        let controllers = fields.nth(1)?;
        let path = fields.next()?;
        controllers
            .split(',')
            .any(|controller| controller == "memory")
            .then_some(path)
    })?;
    least(&root.join("sys/fs/cgroup/memory"), path, |group| {
        let max = limit(&group.join("memory.limit_in_bytes"))?;
        let current = number(&group.join("memory.usage_in_bytes"))?;
        Some(max.saturating_sub(current))
    })
}

/// the least `room` among `mount` and each folder below it on the way to `path`.
#[cfg(any(target_os = "linux", test))]
fn least(mount: &Path, path: &str, room: impl Fn(&Path) -> Option<u64>) -> Option<u64> {
    let mut group = mount.to_path_buf();
    let mut least = room(&group);
    for part in path.split('/').filter(|part| !part.is_empty()) {
        group.push(part);
        if !group.is_dir() {
            break;
        }
        least = match (least, room(&group)) {
            (Some(least), Some(here)) => Some(least.min(here)),
            (least, here) => least.or(here),
        };
    }
    least
}

/// a limit file's bytes; none for `max` or for v1's way of saying unlimited, a number near the
/// largest a 64-bit page count allows.
#[cfg(any(target_os = "linux", test))]
fn limit(path: &Path) -> Option<u64> {
    number(path).filter(|&bytes| bytes < 1 << 60)
}

#[cfg(any(target_os = "linux", test))]
fn number(path: &Path) -> Option<u64> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// raises the soft open-file limit toward the hard one, at most to [`OPEN_FILES`] or, where
/// the system refuses that, [`OPEN_MAX`], and says the limit the process now has.
#[cfg(unix)]
fn raise_open_files() -> Option<u64> {
    use rustix::process::{Resource, Rlimit, getrlimit, setrlimit};
    let limits = getrlimit(Resource::Nofile);
    let current = limits.current?;
    for most in [OPEN_FILES, OPEN_MAX] {
        let wanted = limits.maximum.map_or(most, |hard| hard.min(most));
        if wanted <= current {
            return Some(current);
        }
        let raised = Rlimit {
            current: Some(wanted),
            maximum: limits.maximum,
        };
        if setrlimit(Resource::Nofile, raised).is_ok() {
            return Some(wanted);
        }
    }
    Some(current)
}

#[cfg(not(unix))]
fn raise_open_files() -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// a fixture root holding `files`, each a path below the root and its text.
    fn root(files: &[(&str, &str)]) -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        for (path, text) in files {
            let path = root.path().join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        root
    }

    const MEMINFO: &str =
        "MemTotal:       16384000 kB\nMemFree:         1000000 kB\nMemAvailable:    8000000 kB\n";

    #[test]
    fn without_cgroups_memory_is_what_the_kernel_reports_available() {
        let root = root(&[("proc/meminfo", MEMINFO)]);
        assert_eq!(memory_under(root.path()), Some(8_000_000 * 1024));
    }

    #[test]
    fn a_cgroup_v2_limit_leaves_its_room_after_what_the_group_uses() {
        let root = root(&[
            ("proc/meminfo", MEMINFO),
            ("proc/self/cgroup", "0::/kubepods/pod1\n"),
            ("sys/fs/cgroup/memory.max", "max\n"),
            ("sys/fs/cgroup/memory.current", "9000000000\n"),
            ("sys/fs/cgroup/kubepods/memory.max", "max\n"),
            ("sys/fs/cgroup/kubepods/memory.current", "5000000000\n"),
            ("sys/fs/cgroup/kubepods/pod1/memory.max", "2147483648\n"),
            ("sys/fs/cgroup/kubepods/pod1/memory.current", "536870912\n"),
        ]);
        assert_eq!(memory_under(root.path()), Some(2_147_483_648 - 536_870_912));
    }

    #[test]
    fn a_tighter_parent_group_or_memory_high_wins() {
        let root = root(&[
            ("proc/meminfo", MEMINFO),
            ("proc/self/cgroup", "0::/outer/inner\n"),
            ("sys/fs/cgroup/outer/memory.max", "1073741824\n"),
            ("sys/fs/cgroup/outer/memory.current", "1000000000\n"),
            ("sys/fs/cgroup/outer/inner/memory.max", "4294967296\n"),
            ("sys/fs/cgroup/outer/inner/memory.high", "3221225472\n"),
            ("sys/fs/cgroup/outer/inner/memory.current", "100\n"),
        ]);
        assert_eq!(
            memory_under(root.path()),
            Some(1_073_741_824 - 1_000_000_000)
        );
        let root = self::root(&[
            ("proc/self/cgroup", "0::/one\n"),
            ("sys/fs/cgroup/one/memory.max", "4294967296\n"),
            ("sys/fs/cgroup/one/memory.high", "1073741824\n"),
            ("sys/fs/cgroup/one/memory.current", "73741824\n"),
        ]);
        assert_eq!(memory_under(root.path()), Some(1_000_000_000));
        let root = self::root(&[
            ("proc/self/cgroup", "0::/one\n"),
            ("sys/fs/cgroup/one/memory.max", "max\n"),
            ("sys/fs/cgroup/one/memory.high", "1073741824\n"),
            ("sys/fs/cgroup/one/memory.current", "73741824\n"),
        ]);
        assert_eq!(memory_under(root.path()), Some(1_000_000_000));
    }

    /// a container with its own cgroup namespace mounts its group at the root, while one without
    /// still names the host's path, which does not exist inside it.
    #[test]
    fn a_group_mounted_at_the_root_is_read_there() {
        let root = root(&[
            ("proc/meminfo", MEMINFO),
            ("proc/self/cgroup", "0::/docker/0123abcd\n"),
            ("sys/fs/cgroup/memory.max", "536870912\n"),
            ("sys/fs/cgroup/memory.current", "36870912\n"),
        ]);
        assert_eq!(memory_under(root.path()), Some(500_000_000));
    }

    #[test]
    fn cgroup_v1_reads_the_memory_controller_and_treats_its_huge_value_as_no_limit() {
        let root = root(&[
            ("proc/meminfo", MEMINFO),
            (
                "proc/self/cgroup",
                "12:cpu,cpuacct:/job\n11:memory:/job\n1:name=systemd:/job\n",
            ),
            (
                "sys/fs/cgroup/memory/memory.limit_in_bytes",
                "9223372036854771712\n",
            ),
            ("sys/fs/cgroup/memory/memory.usage_in_bytes", "123\n"),
            (
                "sys/fs/cgroup/memory/job/memory.limit_in_bytes",
                "3000000000\n",
            ),
            (
                "sys/fs/cgroup/memory/job/memory.usage_in_bytes",
                "1000000000\n",
            ),
        ]);
        assert_eq!(memory_under(root.path()), Some(2_000_000_000));
    }

    #[test]
    fn unreadable_files_leave_what_can_be_read() {
        assert_eq!(memory_under(root(&[]).path()), None);
        let root = root(&[
            ("proc/meminfo", "MemTotal: 100 kB\n"),
            ("proc/self/cgroup", "0::/\n"),
            ("sys/fs/cgroup/memory.max", "4096\n"),
            ("sys/fs/cgroup/memory.current", "1024\n"),
        ]);
        assert_eq!(memory_under(root.path()), Some(3072));
    }

    #[cfg(unix)]
    #[test]
    fn the_open_file_limit_is_raised_to_what_the_system_allows() {
        let limit = raise_open_files().unwrap();
        let limits = rustix::process::getrlimit(rustix::process::Resource::Nofile);
        assert_eq!(limits.current, Some(limit));
        let hard = limits.maximum.unwrap_or(u64::MAX);
        assert!(
            limit >= hard.min(OPEN_MAX),
            "{limit} against a hard limit of {hard}"
        );
    }
}
