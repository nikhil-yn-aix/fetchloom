//! git, run as a reader of a project's repository: data.toml and data.lock whole as each side of
//! a merge git left unfinished has them, with their merge base, so `fetchloom lock` takes no line
//! git merged from the other side for a side's, and which of the files in `data.lock.d` and of
//! a recipe's code git has, so a file a commit would leave out is named. apart from the steps
//! `fetchloom build` runs, git is the one program fetchloom runs: found by an absolute path
//! outside the project, with no shell, no input, no transport to fetch with, and bounded output
//! and time. reading objects also takes `--no-lazy-fetch` where git knows it, so a partial clone
//! never reaches the network; listing the index reads no object. any failure, a folder outside a
//! repository among them, means no answer.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

/// the most a read may hold. data.lock lists at most 50,000 files a dataset in itself.
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

/// a file as each side of an unfinished merge has it, and as their merge base had it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Stages {
    /// the side checked out when the merge began: in a rebase, the branch rebased onto.
    pub(crate) ours: String,
    /// the side the merge brings in: in a rebase, the commit replayed.
    pub(crate) theirs: String,
    /// empty when both sides added the file, none when git cannot read it.
    pub(crate) base: Option<String>,
}

/// `file`, a file in `root` that git could not merge, whole as git's index holds each side:
/// stage 2 ours, stage 3 theirs and stage 1 their base, which git lacks when both sides added the
/// file. none when git does not hold both sides, as once `git add` marks the file resolved.
pub(crate) fn stages(git: &Path, root: &Path, file: &str) -> Option<Stages> {
    let listed = run(
        git,
        root,
        &["ls-files", "--unmerged", "-z", "--", &format!("./{file}")],
    )
    .ok()?;
    let mut objects: [Option<String>; 3] = Default::default();
    for entry in std::str::from_utf8(&listed).ok()?.split('\0') {
        // each entry is `<mode> <object> <stage>\t<path>`
        let Some((head, _)) = entry.split_once('\t') else {
            continue;
        };
        let mut fields = head.split(' ').skip(1);
        let (Some(object), Some(stage)) = (fields.next(), fields.next()) else {
            return None;
        };
        let at = stage
            .parse::<usize>()
            .ok()
            .filter(|at| (1..=3).contains(at))?;
        objects[at - 1] = Some(object.to_owned());
    }
    let read = |object: &str| text(read_objects(git, root, &["cat-file", "blob", object])?);
    let [base, ours, theirs] = objects;
    Some(Stages {
        ours: read(&ours?)?,
        theirs: read(&theirs?)?,
        base: base.map_or_else(|| Some(String::new()), |object| read(&object)),
    })
}

/// `file`, a file in `root` that git merged by itself, whole as each side of the merge, rebase,
/// cherry-pick or revert git left unfinished there has it in its commit: ours from `HEAD`, theirs
/// from the commit the operation brings in, as git's index would give them for a file it could
/// not merge. none when no such operation is under way, or a side lacks the file.
pub(crate) fn committed(git: &Path, root: &Path, file: &str) -> Option<[String; 2]> {
    let read = |commit: &str| {
        let blob = format!("{commit}:./{file}");
        text(read_objects(git, root, &["cat-file", "blob", &blob])?)
    };
    // a revert brings in the parent of the commit it takes back
    let theirs = [
        "MERGE_HEAD",
        "REBASE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD^",
    ]
    .into_iter()
    .find_map(read)?;
    Some([read("HEAD")?, theirs])
}

fn text(bytes: Vec<u8>) -> Option<String> {
    String::from_utf8(bytes).ok()
}

/// the files git has, tracked or staged, below `folder` in the work tree at `root`, by their
/// paths from `root` with `/` between folders. it reads only git's index. none when git cannot
/// tell, as outside a repository.
pub(crate) fn tracked(git: &Path, root: &Path, folder: &str) -> Option<HashSet<String>> {
    let listed = run(
        git,
        root,
        &["ls-files", "-z", "--cached", "--", &format!("./{folder}")],
    )
    .ok()?;
    Some(
        listed
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
            .filter_map(|path| std::str::from_utf8(path).ok().map(str::to_owned))
            .collect(),
    )
}

/// what git knows of the files below some paths of a work tree, by their paths from its root
/// with `/` between folders. a file in neither set is one an ignore rule leaves out.
#[derive(Debug, Default)]
pub(crate) struct Known {
    /// the files git has, tracked or staged, and the folder of a submodule.
    pub(crate) tracked: HashSet<String>,
    /// the files git does not have that no ignore rule leaves out, and the folder of a repository
    /// inside the work tree, ending with `/`.
    pub(crate) untracked: HashSet<String>,
}

/// what git knows of the files below `paths` in the work tree at `root`, each path taken as
/// written, never as a pattern. it reads git's index and ignore rules, no object. none when git
/// cannot tell, as outside a repository.
pub(crate) fn known(git: &Path, root: &Path, paths: &[&str]) -> Option<Known> {
    let specs: Vec<String> = paths.iter().map(|path| format!("./{path}")).collect();
    let mut arguments = vec![
        "--literal-pathspecs",
        "ls-files",
        "-z",
        "-t",
        "--cached",
        "--others",
        "--exclude-standard",
        "--",
    ];
    arguments.extend(specs.iter().map(String::as_str));
    let listed = run(git, root, &arguments).ok()?;
    let mut known = Known::default();
    // each entry is `<tag> <path>`, where `?` tags a file git does not have
    for entry in listed.split(|byte| *byte == 0) {
        let Some((tag, path)) = std::str::from_utf8(entry)
            .ok()
            .and_then(|entry| entry.split_once(' '))
        else {
            continue;
        };
        if tag == "?" {
            known.untracked.insert(path.to_owned());
        } else {
            known.tracked.insert(path.to_owned());
        }
    }
    Some(known)
}

/// what git prints for `arguments`, which read objects: with `--no-lazy-fetch` where git knows
/// the option, else again without it. either way git has no transport, so a missing object fails
/// the read instead of being fetched.
fn read_objects(git: &Path, root: &Path, arguments: &[&str]) -> Option<Vec<u8>> {
    let mut unfetched = vec!["--no-lazy-fetch"];
    unfetched.extend_from_slice(arguments);
    match run(git, root, &unfetched) {
        Ok(bytes) => Some(bytes),
        // git refuses an option it does not know with 129, as git 2.43 refuses this one
        Err(Some(129)) => run(git, root, arguments).ok(),
        Err(_) => None,
    }
}

/// what git prints for `arguments` in `root`, when it succeeds within the bounds, else its exit
/// code, none when it did not finish by itself.
fn run(git: &Path, root: &Path, arguments: &[&str]) -> Result<Vec<u8>, Option<i32>> {
    let mut child = Command::new(git)
        .args(["-c", "core.fsmonitor=false", "-C"])
        .arg(root)
        .args(arguments)
        // started outside the project, so windows never loads a library the project holds
        .current_dir(git.parent().ok_or(None)?)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        // no protocol is allowed, so a git that would fetch a missing object cannot
        .env("GIT_ALLOW_PROTOCOL", "")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| None)?;
    let mut stdout = child.stdout.take().ok_or(None)?;
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
    let finished = child.wait().map_err(|_| None)?;
    match bytes {
        Some(bytes) if finished.success() => Ok(bytes),
        Some(_) => Err(finished.code()),
        None => Err(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// git as the machine running the tests has it, of any version.
    fn git() -> Option<PathBuf> {
        let elsewhere = tempfile::tempdir().ok()?;
        find(std::env::var_os("PATH").as_deref(), elsewhere.path())
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
        // a merge or rebase that conflicts fails, as it should here
        assert!(
            status.success() || ["merge", "rebase"].contains(&arguments[0]),
            "{arguments:?}"
        );
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
    fn no_git_gives_no_sides() {
        let folder = tempfile::tempdir().unwrap();
        assert_eq!(find(None, folder.path()), None);
        assert_eq!(find(Some(OsStr::new("")), folder.path()), None);
        assert_eq!(
            find(Some(OsStr::new("relative/bin")), folder.path()),
            None,
            "a folder named relative to wherever fetchloom runs is never searched"
        );
        let absent = folder.path().join("no-git-here");
        assert_eq!(stages(&absent, folder.path(), "data.toml"), None);
        assert_eq!(committed(&absent, folder.path(), "data.toml"), None);
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

    /// a repository in `at` whose branch theirs and main each wrote `study/data.toml` their own
    /// way, as `ours`, `theirs` and `base` say, none for a base without it. theirs alone also
    /// changes `study/notes`, which git then merges by itself.
    fn diverged(git: &Path, at: &Path, base: Option<&str>, ours: &str, theirs: &str) {
        let root = at.join("study");
        std::fs::create_dir_all(&root).unwrap();
        let write = |name: &str, text: &str| std::fs::write(root.join(name), text).unwrap();
        git_in(git, at, &["init", "-q"]);
        write("notes", "base\n");
        if let Some(base) = base {
            write("data.toml", base);
        }
        git_in(git, at, &["add", "."]);
        git_in(git, at, &["commit", "-qm", "start"]);
        git_in(git, at, &["checkout", "-qb", "theirs"]);
        write("data.toml", theirs);
        write("notes", "theirs\n");
        git_in(git, at, &["add", "."]);
        git_in(git, at, &["commit", "-qm", "theirs"]);
        git_in(git, at, &["checkout", "-q", "main"]);
        write("data.toml", ours);
        git_in(git, at, &["add", "."]);
        git_in(git, at, &["commit", "-qm", "ours"]);
    }

    /// git 2.43 knows no `--no-lazy-fetch` and still gives both sides whole and the base.
    #[test]
    fn the_sides_come_whole_from_the_index_and_the_base_is_empty_when_both_added_the_file() {
        let Some(git) = git() else {
            return;
        };
        let repository = tempfile::tempdir().unwrap();
        let at = repository.path();
        let root = at.join("study");
        let (alpha, beta) = (
            "[datasets.alpha]\nref = \"zenodo:3\"\n",
            "[datasets.beta]\nref = \"zenodo:2\"\n",
        );
        diverged(&git, at, None, alpha, beta);
        git_in(&git, at, &["merge", "-q", "theirs"]);
        if !trusted(&git, at) {
            // fetchloom never overrides git's ownership check, so git gives it nothing
            assert_eq!(stages(&git, &root, "data.toml"), None);
            return;
        }
        let both_added = Stages {
            ours: alpha.to_owned(),
            theirs: beta.to_owned(),
            base: Some(String::new()),
        };
        assert_eq!(stages(&git, &root, "data.toml"), Some(both_added));
        git_in(&git, at, &["merge", "--abort"]);

        let changed = tempfile::tempdir().unwrap();
        let at = changed.path();
        let root = at.join("study");
        let one = |reference: u8| format!("[datasets.beta]\nref = \"zenodo:{reference}\"\n");
        diverged(&git, at, Some(&one(4)), &one(6), &one(5));
        git_in(&git, at, &["merge", "-q", "theirs"]);
        let merged = Stages {
            ours: one(6),
            theirs: one(5),
            base: Some(one(4)),
        };
        assert_eq!(stages(&git, &root, "data.toml"), Some(merged.clone()));
        git_in(&git, at, &["add", "study/data.toml"]);
        assert_eq!(
            stages(&git, &root, "data.toml"),
            None,
            "a file marked resolved has no sides left"
        );
        git_in(&git, at, &["merge", "--abort"]);
        // a rebase replays theirs onto ours, and git's index orients the sides the same way
        git_in(&git, at, &["checkout", "-q", "theirs"]);
        git_in(&git, at, &["rebase", "-q", "main"]);
        assert_eq!(stages(&git, &root, "data.toml"), Some(merged));
    }

    /// a file git merged by itself has no stages; its sides come from the commits the merge or
    /// rebase put together.
    #[test]
    fn the_sides_of_a_file_git_merged_come_from_the_commits() {
        let Some(git) = git() else {
            return;
        };
        let repository = tempfile::tempdir().unwrap();
        let at = repository.path();
        let root = at.join("study");
        let one = |reference: u8| format!("[datasets.beta]\nref = \"zenodo:{reference}\"\n");
        diverged(&git, at, Some(&one(4)), &one(6), &one(5));
        if !trusted(&git, at) {
            return;
        }
        assert_eq!(committed(&git, &root, "notes"), None, "no merge yet");
        git_in(&git, at, &["merge", "-q", "theirs"]);
        assert_eq!(
            committed(&git, &root, "notes"),
            Some(["base\n".to_owned(), "theirs\n".to_owned()])
        );
        git_in(&git, at, &["merge", "--abort"]);
        git_in(&git, at, &["checkout", "-q", "theirs"]);
        git_in(&git, at, &["rebase", "-q", "main"]);
        assert_eq!(
            committed(&git, &root, "notes"),
            Some(["base\n".to_owned(), "theirs\n".to_owned()]),
            "the commit a rebase replays is theirs"
        );
    }

    /// a partial clone without a blob: reading it fails and fetches nothing, whatever git does
    /// with `--no-lazy-fetch`, while git without fetchloom's limits fetches it from the origin.
    #[test]
    fn a_missing_object_is_never_fetched() {
        let Some(git) = git() else {
            return;
        };
        let folder = tempfile::tempdir().unwrap();
        let origin = folder.path().join("origin");
        std::fs::create_dir(&origin).unwrap();
        git_in(&git, &origin, &["init", "-q"]);
        std::fs::write(
            origin.join("data.toml"),
            "[datasets.a]\nref = \"zenodo:1\"\n",
        )
        .unwrap();
        git_in(&git, &origin, &["add", "."]);
        git_in(&git, &origin, &["commit", "-qm", "start"]);
        git_in(&git, &origin, &["config", "uploadpack.allowFilter", "true"]);
        let path = origin.display().to_string().replace('\\', "/");
        let url = format!("file:///{}", path.trim_start_matches('/'));
        let clone = folder.path().join("clone");
        git_in(
            &git,
            folder.path(),
            &[
                "clone",
                "-q",
                "--filter=blob:none",
                "--no-checkout",
                &url,
                "clone",
            ],
        );
        if !trusted(&git, &clone) {
            return;
        }
        assert_eq!(
            read_objects(&git, &clone, &["cat-file", "blob", "HEAD:./data.toml"]),
            None
        );
        let mut plain = Command::new(&git);
        for (name, _) in std::env::vars_os() {
            if name.to_string_lossy().starts_with("GIT_") {
                plain.env_remove(name);
            }
        }
        let fetched = plain
            .args(["-c", "safe.directory=*", "-C"])
            .arg(&clone)
            .args(["cat-file", "blob", "HEAD:./data.toml"])
            .env("HOME", folder.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(
            fetched.status.success(),
            "the blob was there to fetch: {}",
            String::from_utf8_lossy(&fetched.stderr)
        );
    }
}
