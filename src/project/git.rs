//! git, run as a reader of a project's repository: the merge base of a file git could not merge,
//! read from git's index, so `fetchloom lock` can tell which side changed a dataset when the
//! conflict lines do not show the base, and which of the files in `data.lock.d` git has, so a
//! list a commit would leave out is named. apart from the steps `fetchloom build` runs, git is the
//! one program fetchloom runs: found by an absolute path outside the project, with no shell, no
//! input, and bounded output and time. reading objects takes `--no-lazy-fetch`, so a partial
//! clone never reaches the network; listing the index reads no object. any failure, an older git
//! or a folder outside a repository among them, means no answer.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

/// the most a base may hold. data.lock lists at most 50,000 files a dataset in itself.
const LARGEST: u64 = 256 << 20;
/// how long git may take to answer.
const PATIENCE: Duration = Duration::from_secs(10);

/// git, as the first absolute folder on `path` that holds it, unless that git is inside
/// `project`, which a cloned repository could have put there.
pub(crate) fn find(path: Option<&OsStr>, project: &Path) -> Option<PathBuf> {
    let name = if cfg!(windows) { "git.exe" } else { "git" };
    let project = project.canonicalize().ok()?;
    std::env::split_paths(path?)
        .filter(|folder| folder.is_absolute())
        .map(|folder| folder.join(name))
        .find(|program| runnable(program))
        .filter(|program| {
            program
                .canonicalize()
                .is_ok_and(|real| !real.starts_with(&project))
        })
}

/// whether `program` is a file this user may run.
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

/// `file`, a file in `root`, as it was at the merge base of the merge git left unfinished there:
/// the index's stage 1, or nothing at all when both sides added the file. none when git cannot
/// tell, as git older than 2.45, which refuses `--no-lazy-fetch`, cannot.
pub(crate) fn base(git: &Path, root: &Path, file: &str) -> Option<String> {
    let blob = format!(":1:./{file}");
    if let Some(bytes) = run(git, root, &["--no-lazy-fetch", "cat-file", "blob", &blob]) {
        return String::from_utf8(bytes).ok();
    }
    let staged = run(
        git,
        root,
        &[
            "--no-lazy-fetch",
            "ls-files",
            "--unmerged",
            "-z",
            "--",
            &format!("./{file}"),
        ],
    )?;
    let mut found: Vec<&[u8]> = staged
        .split(|byte| *byte == 0)
        .filter_map(|entry| {
            let (head, _) = entry.split_at(entry.iter().position(|byte| *byte == b'\t')?);
            head.rsplit(|byte| *byte == b' ').next()
        })
        .collect();
    found.sort_unstable();
    (found == [b"2", b"3"]).then(String::new)
}

/// the files git has, tracked or staged, below `folder` in the work tree at `root`, by their
/// paths from `root` with `/` between folders. it reads only git's index. none when git cannot
/// tell, as outside a repository.
pub(crate) fn tracked(git: &Path, root: &Path, folder: &str) -> Option<HashSet<String>> {
    let listed = run(
        git,
        root,
        &["ls-files", "-z", "--cached", "--", &format!("./{folder}")],
    )?;
    Some(
        listed
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
            .filter_map(|path| std::str::from_utf8(path).ok().map(str::to_owned))
            .collect(),
    )
}

/// what git prints for `arguments` in `root`, when it succeeds within the bounds.
fn run(git: &Path, root: &Path, arguments: &[&str]) -> Option<Vec<u8>> {
    let mut child = Command::new(git)
        .args(["-c", "core.fsmonitor=false", "-C"])
        .arg(root)
        .args(arguments)
        // started outside the project, so windows never loads a library the project holds
        .current_dir(git.parent()?)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let (sender, answer) = mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let read = (&mut stdout).take(LARGEST + 1).read_to_end(&mut bytes);
        let _ = sender.send(read.map(|_| bytes));
    });
    let bytes = match answer.recv_timeout(PATIENCE) {
        Ok(Ok(bytes)) if bytes.len() as u64 <= LARGEST => Some(bytes),
        _ => None,
    };
    if bytes.is_none() {
        let _ = child.kill();
    }
    let finished = child.wait().ok()?;
    finished.success().then_some(bytes).flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// git as the machine running the tests has it, when it reads bases: 2.45 or later.
    fn git() -> Option<PathBuf> {
        let elsewhere = tempfile::tempdir().ok()?;
        let git = find(std::env::var_os("PATH").as_deref(), elsewhere.path())?;
        Command::new(&git)
            .args(["--no-lazy-fetch", "version"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .ok()?
            .success()
            .then_some(git)
    }

    /// runs git in `folder` with a fixed name and no settings of the machine's.
    fn git_in(git: &Path, folder: &Path, arguments: &[&str]) {
        let mut command = Command::new(git);
        for (name, _) in std::env::vars_os() {
            if name.to_string_lossy().starts_with("GIT_") {
                command.env_remove(name);
            }
        }
        let status = command
            .args(["-c", "user.name=t", "-c", "user.email=t@example.org"])
            .args([
                "-c",
                "commit.gpgsign=false",
                "-c",
                "init.defaultBranch=main",
                "-c",
                "safe.directory=*",
            ])
            .args(arguments)
            .current_dir(folder)
            .env("HOME", folder)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        // a merge that conflicts fails, as it should here
        assert!(status.success() || arguments[0] == "merge", "{arguments:?}");
    }

    /// whether git, with the settings of the machine running the tests, works in `folder`. it
    /// refuses a repository on a disk that records no owner, as FAT does, unless told to trust it.
    fn trusted(git: &Path, folder: &Path) -> bool {
        Command::new(git)
            .arg("-C")
            .arg(folder)
            .args(["rev-parse", "--git-dir"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    #[test]
    fn no_git_gives_no_base() {
        let folder = tempfile::tempdir().unwrap();
        assert_eq!(find(None, folder.path()), None);
        assert_eq!(find(Some(OsStr::new("")), folder.path()), None);
        assert_eq!(
            find(Some(OsStr::new("relative/bin")), folder.path()),
            None,
            "a folder named relative to wherever fetchloom runs is never searched"
        );
        assert_eq!(
            base(
                &folder.path().join("no-git-here"),
                folder.path(),
                "data.toml"
            ),
            None
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_git_inside_the_project_is_never_run() {
        use std::os::unix::fs::PermissionsExt;
        let project = tempfile::tempdir().unwrap();
        let bin = project.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let planted = bin.join("git");
        std::fs::write(&planted, "#!/bin/sh\necho planted\n").unwrap();
        std::fs::set_permissions(&planted, std::fs::Permissions::from_mode(0o755)).unwrap();
        let outside = tempfile::tempdir().unwrap();
        assert_eq!(find(Some(bin.as_os_str()), project.path()), None);
        assert_eq!(
            find(Some(bin.as_os_str()), outside.path()),
            Some(planted),
            "the same git outside the project is found"
        );
    }

    #[test]
    fn the_base_comes_from_the_index_or_is_empty_when_both_sides_added_the_file() {
        let Some(git) = git() else {
            return;
        };
        let repository = tempfile::tempdir().unwrap();
        let root = repository.path().join("study");
        // git takes the folder away with its last file on a branch without it
        let write = |text: &str| {
            std::fs::create_dir_all(&root).unwrap();
            std::fs::write(root.join("data.toml"), text).unwrap();
        };
        let at = repository.path();
        git_in(&git, at, &["init", "-q"]);
        std::fs::write(at.join("readme"), "x").unwrap();
        git_in(&git, at, &["add", "."]);
        git_in(&git, at, &["commit", "-qm", "start"]);
        git_in(&git, at, &["checkout", "-qb", "theirs"]);
        write("[datasets.beta]\nref = \"zenodo:2\"\n");
        git_in(&git, at, &["add", "."]);
        git_in(&git, at, &["commit", "-qm", "theirs"]);
        git_in(&git, at, &["checkout", "-q", "main"]);
        write("[datasets.alpha]\nref = \"zenodo:3\"\n");
        git_in(&git, at, &["add", "."]);
        git_in(&git, at, &["commit", "-qm", "ours"]);
        git_in(&git, at, &["merge", "-q", "theirs"]);
        if !trusted(&git, at) {
            // fetchloom never overrides git's ownership check, so git gives it nothing
            assert_eq!(base(&git, &root, "data.toml"), None);
            return;
        }
        assert_eq!(
            base(&git, &root, "data.toml").as_deref(),
            Some(""),
            "both sides added it"
        );
        git_in(&git, at, &["merge", "--abort"]);
        git_in(&git, at, &["checkout", "-q", "theirs"]);
        write("[datasets.beta]\nref = \"zenodo:4\"\n");
        git_in(&git, at, &["commit", "-qam", "change"]);
        git_in(&git, at, &["checkout", "-q", "main"]);
        git_in(&git, at, &["merge", "-q", "theirs"]);
        git_in(&git, at, &["checkout", "--theirs", "--", "study/data.toml"]);
        git_in(&git, at, &["commit", "-qam", "merged"]);
        git_in(&git, at, &["checkout", "-q", "-b", "next"]);
        write("[datasets.beta]\nref = \"zenodo:5\"\n");
        git_in(&git, at, &["commit", "-qam", "five"]);
        git_in(&git, at, &["checkout", "-q", "main"]);
        write("[datasets.beta]\nref = \"zenodo:6\"\n");
        git_in(&git, at, &["commit", "-qam", "six"]);
        git_in(&git, at, &["merge", "-q", "next"]);
        assert_eq!(
            base(&git, &root, "data.toml").as_deref(),
            Some("[datasets.beta]\nref = \"zenodo:4\"\n")
        );
        git_in(&git, at, &["add", "study/data.toml"]);
        assert_eq!(
            base(&git, &root, "data.toml"),
            None,
            "a file marked resolved has no base left"
        );
    }
}
