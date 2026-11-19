//! what this machine gives a run: its processors, the memory a run may use and how many files the
//! process may hold open, read once when a run starts; the programs on its PATH; and its
//! environment, read through [`Var`], with the home and settings folders it names.

use std::ffi::{OsStr, OsString};
#[cfg(any(target_os = "linux", test))]
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::error::{Error, Kind};

/// the open files a run asks for when its soft limit is lower, a file for each download, archive
/// writer and placer. uv, openjdk and graalvm stop here too: past it some code breaks or slows.
#[cfg(unix)]
const OPEN_FILES: u64 = 1 << 20;
/// what macos grants one process when it refuses [`OPEN_FILES`].
#[cfg(unix)]
const OPEN_MAX: u64 = 10_240;

/// the least memory a run budgets for its buffers, and what it budgets when the machine does not
/// say how much it has.
pub(crate) const LEAST_MEMORY: u64 = 256 << 20;

/// what this machine gives a run. its default stands for a machine that says nothing of itself.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Machine {
    /// processors the run may use, after a container's cpu quota and the affinity mask.
    pub(crate) processors: usize,
    /// bytes of memory a run may use: what the system has available, or on linux what is left
    /// under a container's memory limit when that is less. none where the system does not say.
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
        // FETCHLOOM_TEST_ROOM is a container of a chosen size
        let memory = pretended("FETCHLOOM_TEST_ROOM")
            .map(|bytes| Room {
                bytes,
                in_cgroup: true,
            })
            .or_else(memory);
        let open_files = raise_open_files();
        Machine {
            processors: pretended_processors().unwrap_or_else(|| {
                std::thread::available_parallelism().map_or(1, std::num::NonZero::get)
            }),
            memory: memory.map(|room| room.bytes),
            memory_in_cgroup: memory.is_some_and(|room| room.in_cgroup),
            open_files: pretended("FETCHLOOM_TEST_OPEN_FILES").or(open_files),
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

/// windows and macos tell their free memory only through calls this crate keeps out, as it holds
/// no unsafe code, so sysinfo makes them.
#[cfg(any(windows, target_os = "macos"))]
fn memory() -> Option<Room> {
    let mut system = sysinfo::System::new().ok()?;
    system.refresh_memory_specifics(sysinfo::MemoryRefreshKind::nothing().with_ram());
    counted(
        system.total_memory(),
        system.used_memory(),
        system.available_memory(),
    )
}

/// other systems tell their free memory only through calls this crate keeps out, so a run there
/// uses `FETCHLOOM_MEMORY` or the smallest budget.
#[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
fn memory() -> Option<Room> {
    None
}

/// the memory a process may use by sysinfo's count: the total less what it counts used. on
/// windows that is the available physical memory; on macos the free, purgeable and file cache
/// pages, as linux counts available, where sysinfo's own `available` also counts the pages
/// programs are using. none when the count failed, which leaves `available` at zero.
#[cfg(any(windows, target_os = "macos", test))]
fn counted(total: u64, used: u64, available: u64) -> Option<Room> {
    let bytes = total
        .checked_sub(used)
        .filter(|&bytes| bytes > 0 && available > 0)?;
    Some(Room {
        bytes,
        in_cgroup: false,
    })
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

/// the number a debug build takes from the variable `name` in place of this machine's, so CLI
/// tests and docs examples print the same anywhere. release builds never read it.
pub(crate) fn pretended(name: &str) -> Option<u64> {
    #[cfg(debug_assertions)]
    {
        std::env::var(name).ok()?.parse().ok()
    }
    #[cfg(not(debug_assertions))]
    {
        let _ = name;
        None
    }
}

/// the processors `FETCHLOOM_TEST_PROCESSORS` gives a debug build, all of them online.
fn pretended_processors() -> Option<usize> {
    pretended("FETCHLOOM_TEST_PROCESSORS")
        .and_then(|count| usize::try_from(count).ok())
        .filter(|&count| count > 0)
}

/// how many processors the system has online, whatever this process may use of them: none where
/// it does not say.
pub(crate) fn processors_online() -> Option<usize> {
    if let Some(count) = pretended_processors() {
        return Some(count);
    }
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
    installed_where(path, file, |_| true)
}

/// [`installed`], passing over a program inside `project`, as a cloned repository could put one in
/// a folder PATH names. none when `project` cannot be read.
fn outside(path: &OsStr, file: &str, project: &Path) -> Option<PathBuf> {
    let project = project.canonicalize().ok()?;
    installed_where(path, file, |program| {
        program
            .canonicalize()
            .is_ok_and(|real| !real.starts_with(&project))
    })
}

/// the folder whose programs this run never runs: the project it works in, else the repository
/// around it, unless that is the home folder or holds it.
static UNTRUSTED: OnceLock<PathBuf> = OnceLock::new();

/// never runs a program inside `folder`, the project the run works in. set once, at start.
pub(crate) fn distrust(folder: PathBuf) {
    let _ = UNTRUSTED.set(folder);
}

#[cfg(test)]
thread_local! {
    /// a test's own folder in place of [`UNTRUSTED`], on its thread alone, as [`distrusting`]
    /// scopes it, since a folder the whole process kept would outlive the test that named it.
    static SCOPED: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

/// never runs a program inside `folder` on this test's thread until what it returns goes, and
/// leaves the process's own folder as it was.
#[cfg(test)]
pub(crate) fn distrusting(folder: PathBuf) -> impl Drop {
    struct Scope;
    impl Drop for Scope {
        fn drop(&mut self) {
            SCOPED.with_borrow_mut(|scoped| *scoped = None);
        }
    }
    SCOPED.with_borrow_mut(|scoped| *scoped = Some(folder));
    Scope
}

/// the folder [`distrust`] names, or a test's own while `distrusting` scopes one.
fn untrusted() -> Option<PathBuf> {
    #[cfg(test)]
    if let Some(folder) = SCOPED.with_borrow(Clone::clone) {
        return Some(folder);
    }
    UNTRUSTED.get().cloned()
}

/// the program `file` on the PATH `var` gives, past one inside the folder [`distrust`] names,
/// when there is one; else the one inside it passed over, if any, for help to name. every program
/// fetchloom runs is found here, by that one folder for the whole run.
pub(crate) fn program(var: Var<'_>, file: &str) -> Result<PathBuf, Option<PathBuf>> {
    let path = var("PATH").ok_or(None)?;
    let found = match untrusted() {
        Some(project) => outside(&path, file, &project),
        None => installed(&path, file),
    };
    found.ok_or_else(|| installed(&path, file))
}

/// [`installed`], passing over each program `kept` refuses, such as one a project holds, for the
/// next folder's.
pub(crate) fn installed_where(
    path: &OsStr,
    file: &str,
    kept: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    std::env::split_paths(path)
        .filter(|folder| folder.is_absolute())
        .map(|folder| folder.join(file))
        .find(|program| runnable(program) && kept(program))
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

/// a variable's text, when it is set and not empty.
pub(crate) fn set(var: Var<'_>, name: &str) -> Option<String> {
    var(name)
        .and_then(|value| value.into_string().ok())
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
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

/// fetchloom's folder for what it remembers on this machine: `$XDG_STATE_HOME/fetchloom`, else
/// `~/.local/state/fetchloom` on linux and other unix systems, the settings folder on macos and
/// `%LOCALAPPDATA%\fetchloom\state` on windows, inside the default cache folder there; none
/// without a home. it lives apart from a cache a person names, which a lab may share between
/// machines of different speeds.
pub(crate) fn state_folder(var: Var<'_>) -> Option<PathBuf> {
    let set = |name: &str| {
        var(name)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    };
    if cfg!(windows) {
        return set("LOCALAPPDATA").map(|local| local.join("fetchloom").join(STATE));
    }
    if cfg!(target_os = "macos") {
        return set("HOME").map(|home| home.join("Library/Application Support/fetchloom"));
    }
    // the XDG spec has a relative value ignored, which would move the file with the working folder
    set("XDG_STATE_HOME")
        .filter(|path| path.is_absolute())
        .or_else(|| set("HOME").map(|home| home.join(".local").join("state")))
        .map(|state| state.join("fetchloom"))
}

/// the state folder's own name on windows, where the default cache folder holds it.
pub(crate) const STATE: &str = "state";

/// the file in the state folder where fetchloom remembers how fast each host is.
pub(crate) const SPEEDS: &str = "speeds.json";

/// whether `name` is a file fetchloom keeps in its state folder: [`SPEEDS`], or the new copy of it
/// a run stopped while writing leaves, `.speeds.json.<random>.tmp`.
pub(crate) fn kept_in_state(name: &OsStr) -> bool {
    name.to_str().is_some_and(|name| {
        name == SPEEDS
            || name
                .strip_prefix(&format!(".{SPEEDS}."))
                .and_then(|rest| rest.strip_suffix(".tmp"))
                .is_some_and(|random| {
                    !random.is_empty() && random.bytes().all(|byte| byte.is_ascii_alphanumeric())
                })
    })
}

/// whether `path` is inside this user's profile, `USERPROFILE`, where windows keeps a person's
/// files to themselves: a folder elsewhere takes its permissions from the folders above it, which
/// may let other users in. both are resolved first, so a link counts where it leads and a file
/// not made yet where its folder is. a debug build takes the profile from
/// `FETCHLOOM_TEST_PROFILE` when it is set, since a CLI test's `USERPROFILE` is a sandbox folder
/// inside the real profile.
pub(crate) fn in_profile(var: Var<'_>, path: &Path) -> bool {
    let named = |name: &str| var(name).filter(|value| !value.is_empty());
    #[cfg(debug_assertions)]
    let profile = named("FETCHLOOM_TEST_PROFILE").or_else(|| named("USERPROFILE"));
    #[cfg(not(debug_assertions))]
    let profile = named("USERPROFILE");
    profile.is_some_and(|profile| within(&resolved(path), &resolved(Path::new(&profile))))
}

/// `path` made absolute, its longest start that exists followed through links and junctions.
fn resolved(path: &Path) -> PathBuf {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    absolute
        .ancestors()
        .find_map(|start| {
            let real = std::fs::canonicalize(start).ok()?;
            Some(real.join(absolute.strip_prefix(start).ok()?))
        })
        .unwrap_or(absolute)
}

/// `path` as a person writes it: without the `\\?\` windows starts a resolved path with, and a
/// share's `\\?\UNC\` written `\\`.
pub(crate) fn plain(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(share) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{share}"));
    }
    text.strip_prefix(r"\\?\")
        .map_or_else(|| path.to_path_buf(), PathBuf::from)
}

/// whether `path` is `profile` or inside it, as windows names files: part by part, so
/// C:\Users\bob-shared is not inside C:\Users\bob, in any case, and with or without the `\\?\` a
/// resolved path starts with.
pub(crate) fn within(path: &Path, profile: &Path) -> bool {
    let folded = |path: &Path| PathBuf::from(plain(path).to_string_lossy().to_lowercase());
    folded(path).starts_with(folded(profile))
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

    #[test]
    fn the_state_folder_follows_each_system() {
        let var = |name: &str| match name {
            "HOME" => Some("/home/ada".into()),
            "XDG_STATE_HOME" => Some("relative".into()),
            "LOCALAPPDATA" => Some("C:\\Users\\ada\\AppData\\Local".into()),
            _ => None,
        };
        let expected = if cfg!(windows) {
            PathBuf::from("C:\\Users\\ada\\AppData\\Local")
                .join("fetchloom")
                .join("state")
        } else if cfg!(target_os = "macos") {
            PathBuf::from("/home/ada/Library/Application Support/fetchloom")
        } else {
            PathBuf::from("/home/ada/.local/state/fetchloom")
        };
        assert_eq!(state_folder(&var), Some(expected));
        assert_eq!(state_folder(&|_| None), None);
    }

    /// windows keeps a file private inside the user's profile, told part by part and in any case,
    /// so a folder whose name only starts with the profile's is outside it.
    #[test]
    fn a_path_is_in_the_profile_only_at_or_below_it() {
        let profile = Path::new("C:/Users/Ada");
        for (path, inside) in [
            ("C:/Users/Ada", true),
            (
                "C:/Users/Ada/AppData/Roaming/fetchloom/credentials.json",
                true,
            ),
            ("c:/users/ada/appdata/local/fetchloom", true),
            ("C:/Users/Ada-shared/cache", false),
            ("C:/Users/Public/cache", false),
            ("C:/Users", false),
            ("D:/Users/Ada/cache", false),
            ("C:/fetchloom-cache", false),
        ] {
            assert_eq!(within(Path::new(path), profile), inside, "{path}");
        }
    }

    /// `fs::canonicalize` on windows starts a path with `\\?\`, or `\\?\UNC\` for a share, which
    /// names the same folder as the path without it.
    #[cfg(windows)]
    #[test]
    fn a_resolved_windows_path_is_in_the_profile_its_plain_form_is() {
        let profile = Path::new(r"C:\Users\Ada");
        assert!(within(
            Path::new(r"\\?\C:\Users\Ada\AppData\Local\fetchloom"),
            profile
        ));
        assert!(within(
            Path::new(r"C:\Users\Ada\cache"),
            Path::new(r"\\?\C:\Users\Ada")
        ));
        assert!(!within(Path::new(r"\\?\D:\Users\Ada\cache"), profile));
        assert!(within(
            Path::new(r"\\?\UNC\server\homes\ada\cache"),
            Path::new(r"\\server\homes\Ada")
        ));
    }

    /// a link in the profile to a folder outside it holds what it leads to, so it is outside;
    /// a file not made yet is where its folder is.
    #[cfg(unix)]
    #[test]
    fn a_link_out_of_the_profile_is_outside_it() {
        let root = tempfile::tempdir().unwrap();
        let (profile, elsewhere) = (root.path().join("profile"), root.path().join("elsewhere"));
        fs::create_dir_all(profile.join("AppData")).unwrap();
        fs::create_dir(&elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, profile.join("linked")).unwrap();
        let var = |name: &str| (name == "USERPROFILE").then(|| profile.clone().into_os_string());
        assert!(in_profile(
            &var,
            &profile.join("AppData/fetchloom/credentials.json")
        ));
        assert!(!in_profile(&var, &profile.join("linked/cache")));
        assert!(!in_profile(&var, &elsewhere.join("cache")));
        assert!(!in_profile(&|_: &str| None, &profile.join("AppData")));
    }

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

    const GIB: u64 = 1 << 30;

    /// windows' used is its total less the available physical memory, while macos' available
    /// also counts the pages programs are using, so both leave a run what is not used.
    #[test]
    fn a_system_count_leaves_a_run_what_is_not_used() {
        let windows = counted(16 * GIB, 10 * GIB, 6 * GIB);
        assert_eq!(
            windows,
            Some(Room {
                bytes: 6 * GIB,
                in_cgroup: false
            })
        );
        let macos = counted(16 * GIB, 10 * GIB, 14 * GIB);
        assert_eq!(macos.map(|room| room.bytes), Some(6 * GIB));
    }

    /// a failed count leaves windows' numbers at zero and macos' at its total alone, and either
    /// says nothing, so a run takes the smallest budget rather than none or all of memory.
    #[test]
    fn a_failed_system_count_says_nothing() {
        assert_eq!(counted(0, 0, 0), None);
        assert_eq!(counted(16 * GIB, 0, 0), None);
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

    /// a program inside the project is passed over, though PATH names its folder in full; the same
    /// program outside the project is found, and one later on PATH is not hidden by it.
    #[cfg(unix)]
    #[test]
    fn a_program_the_project_holds_is_passed_over() {
        use std::os::unix::fs::PermissionsExt;
        let project = tempfile::tempdir().unwrap();
        let bin = project.path().join("bin");
        fs::create_dir(&bin).unwrap();
        let planted = bin.join("tool");
        fs::write(&planted, "#!/bin/sh\necho planted\n").unwrap();
        fs::set_permissions(&planted, fs::Permissions::from_mode(0o755)).unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        assert_eq!(outside(bin.as_os_str(), "tool", project.path()), None);
        assert_eq!(
            outside(bin.as_os_str(), "tool", elsewhere.path()),
            Some(planted.clone()),
            "the same program outside the project is found"
        );
        let installed = elsewhere.path().join("tool");
        fs::copy(&planted, &installed).unwrap();
        let path = std::env::join_paths([&bin, &elsewhere.path().to_path_buf()]).unwrap();
        assert_eq!(
            outside(&path, "tool", project.path()),
            Some(installed),
            "the project's program hides none later on PATH"
        );
        assert_eq!(program(&|_| None, "tool"), Err(None));
    }

    /// a test's distrusted folder holds on its thread while what `distrusting` returns lives, and
    /// leaves nothing behind for the process: a later test finds the program there again.
    #[test]
    fn a_tests_distrusted_folder_ends_with_it() {
        let project = tempfile::tempdir().unwrap();
        let bin = project.path().join("bin");
        fs::create_dir(&bin).unwrap();
        let planted = bin.join("tool");
        fs::write(&planted, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&planted, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let var = |name: &str| (name == "PATH").then(|| bin.clone().into_os_string());
        {
            let _distrusted = distrusting(project.path().to_path_buf());
            assert_eq!(program(&var, "tool"), Err(Some(planted.clone())));
        }
        assert_eq!(program(&var, "tool"), Ok(planted));
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
