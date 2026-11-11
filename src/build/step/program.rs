//! finding a step's program on windows as a shell there finds it. std looks in fetchloom.exe's
//! own folder and in the windows folders before PATH, and gives a bare name `.exe` alone, so
//! `npm` would miss `npm.cmd` and a program beside fetchloom.exe would stand in for the one on
//! PATH. the program found goes to std as a full path, and std still quotes a `.bat` or `.cmd`
//! program's arguments for cmd.exe and refuses one it cannot pass safely.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

/// the extensions of the programs windows starts: `.com` and `.exe` themselves, `.bat` and
/// `.cmd` through cmd.exe.
const STARTABLE: [&str; 4] = [".com", ".exe", ".bat", ".cmd"];

/// the program `name`, the step's first word, names: its full path when a shell in `folder`, the
/// step's folder, finds it, else `name` as written, which std then looks for as it does. the
/// step's own `env` may set PATH and PATHEXT.
#[cfg(windows)]
pub(super) fn find(
    name: &OsStr,
    folder: &Path,
    env: &std::collections::BTreeMap<String, String>,
) -> OsString {
    let variable = |key: &str| {
        env.iter()
            .find(|(set, _)| set.eq_ignore_ascii_case(key))
            .map(|(_, value)| OsString::from(value))
            .or_else(|| std::env::var_os(key))
    };
    let path: Vec<PathBuf> = variable("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default();
    let is_file = |path: &Path| std::fs::symlink_metadata(path).is_ok_and(|meta| !meta.is_dir());
    search(
        name,
        folder,
        &path,
        variable("PATHEXT").as_deref(),
        &is_file,
    )
    .map_or_else(|| name.to_owned(), PathBuf::into_os_string)
}

/// where a shell in `folder` finds the program `name`: a name with a folder in it, such as
/// `tools\prep`, from `folder`, and a bare name in each folder of `path` in turn, a relative one
/// taken from `folder`, but never in `folder` itself. in each folder it is the name as written
/// when that ends with an extension of `pathext` that windows starts, else the name with each of
/// those added, in `pathext`'s order. `is_file` says whether a path names a file.
fn search(
    name: &OsStr,
    folder: &Path,
    path: &[PathBuf],
    pathext: Option<&OsStr>,
    is_file: &dyn Fn(&Path) -> bool,
) -> Option<PathBuf> {
    let written = name.to_string_lossy().to_ascii_lowercase();
    if written.is_empty() {
        return None;
    }
    let extensions = extensions(pathext);
    let names: Vec<OsString> = if extensions
        .iter()
        .any(|extension| written.ends_with(extension.as_str()))
    {
        vec![name.to_owned()]
    } else {
        extensions
            .iter()
            .map(|extension| {
                let mut named = name.to_owned();
                named.push(extension);
                named
            })
            .collect()
    };
    let folders: Vec<PathBuf> = if written.contains(['/', '\\']) {
        vec![folder.to_path_buf()]
    } else {
        path.iter()
            .filter(|entry| !entry.as_os_str().is_empty())
            .map(|entry| folder.join(entry))
            .collect()
    };
    folders
        .iter()
        .flat_map(|at| names.iter().map(move |named| at.join(named)))
        .find(|candidate| is_file(candidate))
}

/// the extensions of `pathext` that windows starts, lowercase and in its order, or all of them
/// when it names none.
fn extensions(pathext: Option<&OsStr>) -> Vec<String> {
    let pathext = pathext.map(OsStr::to_string_lossy).unwrap_or_default();
    let named: Vec<String> = pathext
        .split(';')
        .map(|extension| extension.trim().to_ascii_lowercase())
        .filter(|extension| STARTABLE.contains(&extension.as_str()))
        .collect();
    if named.is_empty() {
        STARTABLE.map(str::to_owned).to_vec()
    } else {
        named
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    /// a bare name is found through PATH's folders in order, trying PATHEXT's extensions in each,
    /// so a `.cmd` shim in an earlier folder wins over an `.exe` in a later one. a name that ends
    /// with one of them is taken as written, and one with a folder in it comes from the step's
    /// folder. types windows cannot start, the step's folder and an empty PATH entry are passed
    /// over.
    #[test]
    fn windows_lookup_follows_path_order_and_pathext() {
        let folder = Path::new("/project");
        let (first, second) = (Path::new("/first"), Path::new("/second"));
        let files: BTreeSet<String> = [
            first.join("npm.cmd"),
            second.join("npm.exe"),
            first.join("tool.js"),
            second.join("tool.exe"),
            second.join("python3.11.exe"),
            folder.join("uv.exe"),
            second.join("uv.exe"),
            folder.join("bin").join("fmt.bat"),
            folder.join("tools").join("prep.bat"),
        ]
        .iter()
        .map(|file| lowercase(file))
        .collect();
        // windows' disks ignore case
        let is_file = |path: &Path| files.contains(&lowercase(path));
        let path = [first.into(), PathBuf::new(), second.into(), "bin".into()];
        let pathext = OsStr::new(".COM;.EXE;.BAT;.CMD;.VBS;.JS");
        let found = |name: &str| search(OsStr::new(name), folder, &path, Some(pathext), &is_file);
        assert_eq!(found("npm"), Some(first.join("npm.cmd")));
        assert_eq!(found("npm.exe"), Some(second.join("npm.exe")));
        assert_eq!(found("NPM.CMD"), Some(first.join("NPM.CMD")));
        assert_eq!(found("tool"), Some(second.join("tool.exe")));
        assert_eq!(found("python3.11"), Some(second.join("python3.11.exe")));
        assert_eq!(found("uv"), Some(second.join("uv.exe")));
        assert_eq!(found("fmt"), Some(folder.join("bin").join("fmt.bat")));
        assert_eq!(found("tools/prep"), Some(folder.join("tools/prep.bat")));
        assert_eq!(found("missing"), None);
        assert_eq!(
            search(OsStr::new("npm"), folder, &path, None, &is_file),
            Some(first.join("npm.cmd")),
            "with no PATHEXT, the extensions windows starts"
        );
    }

    /// `path` as a lowercase string, with `/` between folders on every system.
    fn lowercase(path: &Path) -> String {
        path.to_string_lossy().replace('\\', "/").to_lowercase()
    }
}
