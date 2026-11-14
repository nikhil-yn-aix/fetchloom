//! what this machine gives a run: its processors, the memory a run may use and how many files the
//! process may hold open, read once when a run starts, and the programs on its PATH.

use std::ffi::{OsStr, OsString};
#[cfg(any(target_os = "linux", test))]
use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{Error, Kind};

/// the open files a run asks for when its soft limit is lower, a file for each download, archive
/// writer and placer. uv, openjdk and graalvm stop here too: past it some code breaks or slows.
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
    /// whether `memory` is what a cgroup's limit leaves, less than the system has available.
    pub(crate) memory_in_cgroup: bool,
    /// files the process may hold open, after raising its soft limit as far as the system lets
    /// it. none where the system sets no such limit.
    pub(crate) open_files: Option<u64>,
}

impl Machine {
    /// reads this machine, raising the process's open-file limit on the way.
    pub(crate) fn read() -> Machine {
        // a container of a chosen size, for CLI tests; release builds never read it
        #[cfg(debug_assertions)]
        let memory = std::env::var("FETCHLOOM_TEST_ROOM")
            .ok()
            .and_then(|text| text.parse().ok())
            .map(|bytes| Room {
                bytes,
                in_cgroup: true,
            })
            .or_else(memory);
        #[cfg(not(debug_assertions))]
        let memory = memory();
        Machine {
            processors: std::thread::available_parallelism().map_or(1, std::num::NonZero::get),
            memory: memory.map(|room| room.bytes),
            memory_in_cgroup: memory.is_some_and(|room| room.in_cgroup),
            open_files: raise_open_files(),
        }
    }
}

impl Machine {
    /// the memory a container's limit leaves a run, none when no container's limit is what
    /// holds it.
    pub(crate) fn container_room(&self) -> Option<u64> {
        self.memory.filter(|_| self.memory_in_cgroup)
    }
}

/// the memory a process may use, and whether a cgroup's limit is what holds it there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Room {
    bytes: u64,
    in_cgroup: bool,
}

#[cfg(target_os = "linux")]
fn memory() -> Option<Room> {
    memory_under(Path::new("/"))
}

/// other systems tell their free memory only through calls this crate keeps out, as it holds
/// no unsafe code, so a run there uses `FETCHLOOM_MEMORY` or the smallest budget.
#[cfg(not(target_os = "linux"))]
fn memory() -> Option<Room> {
    None
}

/// the memory a process may use, from `/proc` and `/sys` below `root`: what the kernel reports
/// available, or less when the process's cgroups leave less room under their limits.
#[cfg(any(target_os = "linux", test))]
fn memory_under(root: &Path) -> Option<Room> {
    let free = available(&root.join("proc/meminfo"));
    let (bytes, in_cgroup) = match (free, cgroup_room(root)) {
        (Some(free), Some(limit)) => (free.min(limit), limit < free),
        (Some(free), None) => (free, false),
        (None, limit) => (limit?, true),
    };
    Some(Room { bytes, in_cgroup })
}

/// how many processors the system has online, whatever this process may use of them: none where
/// it does not say.
pub(crate) fn processors_online() -> Option<usize> {
    #[cfg(target_os = "linux")]
    {
        fs::read_to_string("/sys/devices/system/cpu/online")
            .ok()
            .and_then(|list| listed_processors(&list))
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// how many processors a list such as `0-3,8,10-11` names.
#[cfg(any(target_os = "linux", test))]
fn listed_processors(text: &str) -> Option<usize> {
    text.trim().split(',').try_fold(0, |count, range| {
        let (low, high) = range.split_once('-').unwrap_or((range, range));
        let (low, high): (usize, usize) = (low.parse().ok()?, high.parse().ok()?);
        Some(count + high.checked_sub(low)? + 1)
    })
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

/// the program `file` in the first absolute folder of `path`, a PATH value, that holds it as a file
/// this user may run. a relative folder would be wherever fetchloom runs, so it is passed over.
pub(crate) fn installed(path: &OsStr, file: &str) -> Option<PathBuf> {
    std::env::split_paths(path)
        .filter(|folder| folder.is_absolute())
        .map(|folder| folder.join(file))
        .find(|program| runnable(program))
}

/// whether `program` is a file this user may run: on unix, one with an execute bit set.
fn runnable(program: &Path) -> bool {
    let Ok(found) = program.metadata() else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        found.is_file() && found.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        found.is_file()
    }
}

/// reads the environment, so tests can give each run its own.
pub(crate) type Var<'a> = &'a dyn Fn(&str) -> Option<OsString>;

/// the environment of this process, the one `Var` most callers want.
pub(crate) fn env(name: &str) -> Option<OsString> {
    std::env::var_os(name)
}

/// the home folder `var` names: `USERPROFILE`, else `HOME`, on windows; `HOME` elsewhere.
pub(crate) fn home(var: Var<'_>) -> Option<PathBuf> {
    let set = |name: &str| {
        var(name)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    };
    if cfg!(windows) {
        set("USERPROFILE").or_else(|| set("HOME"))
    } else {
        set("HOME")
    }
}

/// fetchloom's folder for settings, where each system keeps a program's settings.
pub(crate) fn settings_folder(var: Var<'_>) -> Option<PathBuf> {
    let set = |name: &str| {
        var(name)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    };
    if cfg!(windows) {
        return set("APPDATA").map(|appdata| appdata.join("fetchloom"));
    }
    if cfg!(target_os = "macos") {
        return home(var).map(|home| home.join("Library/Application Support/fetchloom"));
    }
    // the XDG spec has a relative value ignored, which would move the file with the working folder
    set("XDG_CONFIG_HOME")
        .filter(|path| path.is_absolute())
        .or_else(|| home(var).map(|home| home.join(".config")))
        .map(|config| config.join("fetchloom"))
}

/// whether the on/off variable `name` is on, as [`switched`] reads it: a value it cannot read is
/// off.
pub(crate) fn is_on(var: Var<'_>, name: &str) -> bool {
    matches!(switched(name, var(name).as_deref()), Ok(Some(true)))
}

/// what an on/off setting `named` says as `value`: none when unset or blank, true for `true` or
/// `1`, false for `false` or `0`, in any case. anything else is a usage error without a help, for
/// the caller to refuse or to tell as a warning.
pub(crate) fn switched(
    named: &str,
    value: Option<&std::ffi::OsStr>,
) -> Result<Option<bool>, Error> {
    let Some(value) = value else {
        return Ok(None);
    };
    let text = value.to_string_lossy();
    match text.trim().to_ascii_lowercase().as_str() {
        "" => Ok(None),
        "true" | "1" => Ok(Some(true)),
        "false" | "0" => Ok(Some(false)),
        _ => Err(Error::new(
            Kind::Usage,
            format!("{named} is \"{text}\", which is not true or false"),
        )),
    }
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

    /// the memory `memory_under` finds below `root`, without where it came from.
    fn bytes(root: &Path) -> Option<u64> {
        memory_under(root).map(|room| room.bytes)
    }

    const MEMINFO: &str =
        "MemTotal:       16384000 kB\nMemFree:         1000000 kB\nMemAvailable:    8000000 kB\n";

    #[test]
    fn without_cgroups_memory_is_what_the_kernel_reports_available() {
        let root = root(&[("proc/meminfo", MEMINFO)]);
        assert_eq!(bytes(root.path()), Some(8_000_000 * 1024));
        assert!(!memory_under(root.path()).unwrap().in_cgroup);
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
        assert_eq!(bytes(root.path()), Some(2_147_483_648 - 536_870_912));
        assert!(memory_under(root.path()).unwrap().in_cgroup);
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
        assert_eq!(bytes(root.path()), Some(1_073_741_824 - 1_000_000_000));
        let root = self::root(&[
            ("proc/self/cgroup", "0::/one\n"),
            ("sys/fs/cgroup/one/memory.max", "4294967296\n"),
            ("sys/fs/cgroup/one/memory.high", "1073741824\n"),
            ("sys/fs/cgroup/one/memory.current", "73741824\n"),
        ]);
        assert_eq!(bytes(root.path()), Some(1_000_000_000));
        let root = self::root(&[
            ("proc/self/cgroup", "0::/one\n"),
            ("sys/fs/cgroup/one/memory.max", "max\n"),
            ("sys/fs/cgroup/one/memory.high", "1073741824\n"),
            ("sys/fs/cgroup/one/memory.current", "73741824\n"),
        ]);
        assert_eq!(bytes(root.path()), Some(1_000_000_000));
    }

    #[test]
    fn a_roomy_cgroup_leaves_the_available_memory_in_charge() {
        let root = root(&[
            ("proc/meminfo", MEMINFO),
            ("proc/self/cgroup", "0::/roomy\n"),
            ("sys/fs/cgroup/roomy/memory.max", "68719476736\n"),
            ("sys/fs/cgroup/roomy/memory.current", "1048576\n"),
        ]);
        let found = memory_under(root.path()).unwrap();
        assert_eq!((found.bytes, found.in_cgroup), (8_000_000 * 1024, false));
    }

    #[test]
    fn a_processor_list_counts_each_range_and_single() {
        for (list, count) in [
            ("0-23\n", Some(24)),
            ("0\n", Some(1)),
            ("0-3,8,10-11", Some(7)),
            ("", None),
            ("3-1", None),
            ("0-x", None),
        ] {
            assert_eq!(listed_processors(list), count, "{list:?}");
        }
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
        assert_eq!(bytes(root.path()), Some(500_000_000));
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
        assert_eq!(bytes(root.path()), Some(2_000_000_000));
    }

    #[test]
    fn unreadable_files_leave_what_can_be_read() {
        assert_eq!(bytes(root(&[]).path()), None);
        let root = root(&[
            ("proc/meminfo", "MemTotal: 100 kB\n"),
            ("proc/self/cgroup", "0::/\n"),
            ("sys/fs/cgroup/memory.max", "4096\n"),
            ("sys/fs/cgroup/memory.current", "1024\n"),
        ]);
        assert_eq!(bytes(root.path()), Some(3072));
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

    /// folders are tried in PATH's order, passing over a relative folder, a folder by that name
    /// and, on unix, a file with no execute bit.
    #[test]
    fn a_program_is_the_first_runnable_file_in_an_absolute_folder_of_path() {
        let root = tempfile::tempdir().unwrap();
        let [plain, folder, first, second] = ["plain", "folder", "first", "second"].map(|name| {
            let at = root.path().join(name);
            fs::create_dir(&at).unwrap();
            at
        });
        fs::create_dir(folder.join("tool")).unwrap();
        for at in [&plain, &first, &second] {
            fs::write(at.join("tool"), "#!/bin/sh\n").unwrap();
        }
        #[cfg(unix)]
        for at in [&first, &second] {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(at.join("tool"), fs::Permissions::from_mode(0o755)).unwrap();
        }
        let relative = Path::new("relative");
        let plain_runs = (!cfg!(unix)).then(|| plain.join("tool"));
        let cases: [(&[&Path], Option<PathBuf>); 6] = [
            (&[], None),
            (&[relative], None),
            (&[&folder], None),
            (&[&plain], plain_runs),
            (
                &[&folder, relative, &first, &second],
                Some(first.join("tool")),
            ),
            (&[&second, &first], Some(second.join("tool"))),
        ];
        for (folders, program) in cases {
            let path = std::env::join_paths(folders).unwrap();
            assert_eq!(installed(&path, "tool"), program, "{folders:?}");
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_relative_xdg_config_home_is_ignored() {
        let var = |name: &str| match name {
            "HOME" => Some(OsString::from("/home/ada")),
            "XDG_CONFIG_HOME" => Some(OsString::from("relative/config")),
            _ => None,
        };
        assert_eq!(
            settings_folder(&var),
            Some(PathBuf::from("/home/ada/.config/fetchloom"))
        );
        let absolute = |name: &str| match name {
            "HOME" => Some(OsString::from("/home/ada")),
            "XDG_CONFIG_HOME" => Some(OsString::from("/xdg")),
            _ => None,
        };
        assert_eq!(
            settings_folder(&absolute),
            Some(PathBuf::from("/xdg/fetchloom"))
        );
    }
}
