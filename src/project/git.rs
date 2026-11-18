//! git, run as a reader of a project's repository: each side of an unfinished merge of data.toml
//! and data.lock whole, which files git has, and the files a commit holds. it runs from an
//! absolute path outside the project with no shell, input or transport, within bounds, and any
//! failure means no answer.

use std::collections::{BTreeMap, HashSet};
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
/// the variables that tie git to one repository, as `git rev-parse --local-env-vars` lists them. a
/// hook or `git rebase --exec` hands them down, and they would point git away from `root`'s.
const REPOSITORY_VARIABLES: [&str; 15] = [
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_CONFIG",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
    "GIT_OBJECT_DIRECTORY",
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_IMPLICIT_WORK_TREE",
    "GIT_GRAFT_FILE",
    "GIT_INDEX_FILE",
    "GIT_NO_REPLACE_OBJECTS",
    "GIT_REPLACE_REF_BASE",
    "GIT_PREFIX",
    "GIT_SHALLOW_FILE",
    "GIT_COMMON_DIR",
];

/// git as [`find`] finds it on this run's PATH, outside the project at `project`.
pub(crate) fn on_path(project: &Path) -> Option<PathBuf> {
    find(std::env::var_os("PATH").as_deref(), project)
}

/// git, as the first absolute folder on `path` that holds it outside `project`: a cloned
/// repository could have put one inside, in a folder PATH names.
pub(crate) fn find(path: Option<&OsStr>, project: &Path) -> Option<PathBuf> {
    let name = if cfg!(windows) { "git.exe" } else { "git" };
    crate::machine::outside(path?, name, project)
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

/// `file` in `root`, which git could not merge, whole as git's index holds each side: stage 2
/// ours, 3 theirs and 1 their base. none when git lacks either side, as once `git add` resolves it.
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

/// what git stops at a conflict in, which decides whose side git calls ours and whose theirs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Operation {
    Merge,
    Rebase,
    CherryPick,
    Revert,
}

impl Operation {
    /// git's name for the commit this brings in, theirs. a revert brings in the parent of the
    /// commit it takes back.
    pub(crate) fn theirs(self) -> &'static str {
        match self {
            Operation::Merge => "MERGE_HEAD",
            Operation::Rebase => "REBASE_HEAD",
            Operation::CherryPick => "CHERRY_PICK_HEAD",
            Operation::Revert => "REVERT_HEAD^",
        }
    }
}

/// `file` in `root`, which git merged by itself, whole as each side of the unfinished merge,
/// rebase, cherry-pick or revert has it in its commit. none when none is under way or one lacks it.
pub(crate) fn committed(git: &Path, root: &Path, file: &str) -> Option<[String; 2]> {
    let read = |theirs: bool| text(in_commit(git, root, file, theirs)?);
    Some([read(false)?, read(true)?])
}

/// `file` in `root` as one side's commit of the unfinished merge, rebase, cherry-pick or revert
/// holds it: ours, `HEAD`, unless `theirs`. none when none is under way or that commit lacks it.
pub(crate) fn in_commit(git: &Path, root: &Path, file: &str, theirs: bool) -> Option<Vec<u8>> {
    let commit = if theirs {
        operation(git, root)?.theirs()
    } else {
        "HEAD"
    };
    let blob = format!("{commit}:./{file}");
    read_objects(git, root, &["cat-file", "blob", &blob])
}

/// the merge, rebase, cherry-pick or revert under way in the repository at `root`, as `git status`
/// tells it, none when git is in none of them. git leaves `REBASE_HEAD` behind once a rebase that
/// stopped is done, so only the rebase's own folder says one is under way.
pub(crate) fn operation(git: &Path, root: &Path) -> Option<Operation> {
    let head = |name: &str| run(git, root, &["rev-parse", "-q", "--verify", name]).is_ok();
    if head("MERGE_HEAD") {
        Some(Operation::Merge)
    } else if rebasing(git, root) {
        Some(Operation::Rebase)
    } else if head("CHERRY_PICK_HEAD") {
        Some(Operation::CherryPick)
    } else if head("REVERT_HEAD") {
        Some(Operation::Revert)
    } else {
        None
    }
}

/// whether a rebase is under way: its folder in the repository's git folder, `rebase-merge`, or
/// `rebase-apply` without the `applying` that `git am` adds.
fn rebasing(git: &Path, root: &Path) -> bool {
    let Ok(listed) = run(
        git,
        root,
        &[
            "rev-parse",
            "--git-path",
            "rebase-merge",
            "--git-path",
            "rebase-apply",
            "--git-path",
            "rebase-apply/applying",
        ],
    ) else {
        return false;
    };
    let listed = String::from_utf8_lossy(&listed);
    let paths: Vec<PathBuf> = listed
        .lines()
        .map(|line| super::native(root, line))
        .collect();
    match paths.as_slice() {
        [merging, applying, am] => merging.is_dir() || (applying.is_dir() && !am.exists()),
        _ => false,
    }
}

/// `commit`, as `HEAD` or [`Operation::theirs`] names it, by its short name and subject, as
/// `git log --oneline` shows it. it is read from the commit object, since `git log` runs whatever
/// signature checker the repository's settings name. none when git cannot tell.
pub(crate) fn commit(git: &Path, root: &Path, commit: &str) -> Option<String> {
    let short = text(
        run(
            git,
            root,
            &["rev-parse", "-q", "--verify", "--short", commit],
        )
        .ok()?,
    )?;
    let object = read_objects(git, root, &["cat-file", "commit", commit])?;
    let object = String::from_utf8_lossy(&object);
    // headers end at the first blank line, and the subject is the message's first paragraph
    let message = object.split_once("\n\n").map_or("", |(_, message)| message);
    let subject = message
        .split("\n\n")
        .find(|paragraph| !paragraph.trim().is_empty())
        .unwrap_or_default();
    let words: Vec<&str> = short
        .split_whitespace()
        .chain(subject.split_whitespace())
        .collect();
    (!words.is_empty()).then(|| words.join(" "))
}

fn text(bytes: Vec<u8>) -> Option<String> {
    String::from_utf8(bytes).ok()
}

/// why [`resolve`] found no commit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Unnamed {
    /// git does not read a repository at the folder: there is none, or git does not trust it.
    NoRepository,
    /// the repository has no commit by that name.
    NoCommit,
}

/// the full name of the commit `revision` names in the repository holding `root`. `revision` is
/// read as a revision alone, never as an option, whatever it starts with.
pub(crate) fn resolve(git: &Path, root: &Path, revision: &str) -> Result<String, Unnamed> {
    if run(git, root, &["rev-parse", "--git-dir"]).is_err() {
        return Err(Unnamed::NoRepository);
    }
    let asked = format!("{revision}^{{commit}}");
    let named = run(
        git,
        root,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            "--end-of-options",
            &asked,
        ],
    )
    .map_err(|_| Unnamed::NoCommit)?;
    text(named)
        .map(|named| named.trim().to_owned())
        .filter(|named| is_object(named))
        .ok_or(Unnamed::NoCommit)
}

/// whether the repository holding `root` is a shallow clone, which lacks the commits before the
/// ones it was cloned with. false where git cannot tell.
pub(crate) fn shallow(git: &Path, root: &Path) -> bool {
    run(git, root, &["rev-parse", "--is-shallow-repository"])
        .ok()
        .and_then(text)
        .is_some_and(|said| said.trim() == "true")
}

/// the plain files `commit` holds at `paths` and below them, each by its path from `root` with
/// `/` between folders, with the name of its blob. a link, a submodule or a folder is left out.
/// none when git cannot list them.
pub(crate) fn blobs(
    git: &Path,
    root: &Path,
    commit: &str,
    paths: &[&str],
) -> Option<BTreeMap<String, String>> {
    let specs: Vec<String> = paths.iter().map(|path| format!("./{path}")).collect();
    let mut arguments = vec![
        "--literal-pathspecs",
        "ls-tree",
        "-r",
        "-z",
        "--end-of-options",
        commit,
        "--",
    ];
    arguments.extend(specs.iter().map(String::as_str));
    let listed = run(git, root, &arguments).ok()?;
    let mut found = BTreeMap::new();
    // each entry is `<mode> <type> <object>\t<path>`
    for entry in listed
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        let (head, path) = std::str::from_utf8(entry).ok()?.split_once('\t')?;
        let mut fields = head.split(' ');
        let (Some(mode), Some(kind), Some(object)) = (fields.next(), fields.next(), fields.next())
        else {
            return None;
        };
        if kind == "blob" && ["100644", "100755"].contains(&mode) && is_object(object) {
            found.insert(path.to_owned(), object.to_owned());
        }
    }
    Some(found)
}

/// the bytes of the blob `object`, from this repository alone.
pub(crate) fn blob(git: &Path, root: &Path, object: &str) -> Option<Vec<u8>> {
    read_objects(git, root, &["cat-file", "blob", "--end-of-options", object])
}

/// whether `name` is an object's full name: 40 hex digits, or 64 in a SHA-256 repository.
fn is_object(name: &str) -> bool {
    [40, 64].contains(&name.len()) && name.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// the files git has, tracked or staged, below `folder` in the work tree at `root`, by their paths
/// from `root`, the folder taken as written, never as a pattern. it reads only git's index, with
/// the git [`on_path`] finds. none when there is none or it cannot tell, as outside a repository.
pub(crate) fn tracked(root: &Path, folder: &str) -> Option<HashSet<String>> {
    let listed = run(
        &on_path(root)?,
        root,
        &[
            "--literal-pathspecs",
            "ls-files",
            "-z",
            "--cached",
            "--",
            &format!("./{folder}"),
        ],
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

/// the files git users commit only to keep a folder, or its rules, in git.
const HOLDERS: [&str; 3] = [".gitkeep", ".keep", ".gitignore"];

/// whether git tracks files of the person's in `folder` of the project at `root`, besides
/// [`HOLDERS`]: a folder they keep in git, where a marker would hide their next file. no
/// repository, no git and no answer are no.
pub(crate) fn keeps(root: &Path, folder: &str) -> bool {
    super::Project::repository(root).is_some()
        && tracked(root, folder).is_some_and(|tracked| {
            tracked.iter().any(|path| {
                let name = path
                    .rsplit_once('/')
                    .map_or(path.as_str(), |(_, name)| name);
                !HOLDERS.contains(&name)
            })
        })
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
/// written, never as a pattern, from its index and ignore rules, as the git [`on_path`] finds
/// reads them. none when there is none or it cannot tell.
pub(crate) fn known(root: &Path, paths: &[&str]) -> Option<Known> {
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
    let listed = run(&on_path(root)?, root, &arguments).ok()?;
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

/// what git prints for `arguments`, which read objects: with `--no-lazy-fetch` where git knows the
/// option, else without it. either way git has no transport, so a missing object is never fetched.
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
    let mut command = Command::new(git);
    for name in REPOSITORY_VARIABLES {
        command.env_remove(name);
    }
    let mut child = command
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
        let found = find(std::env::var_os("PATH").as_deref(), elsewhere.path());
        if found.is_none() {
            crate::test_skips::skip("git", "git is not installed");
        }
        found
    }

    /// git with none of the machine's settings: no GIT_* variable, global or system config, and
    /// `home` as its home.
    fn plain(git: &Path, home: &Path) -> Command {
        let mut command = Command::new(git);
        for (name, _) in std::env::vars_os() {
            let upper = name.to_string_lossy().to_ascii_uppercase();
            if upper.starts_with("GIT_") || upper == "XDG_CONFIG_HOME" {
                command.env_remove(name);
            }
        }
        command.env("HOME", home).env("GIT_CONFIG_NOSYSTEM", "1");
        command
    }

    /// runs git in `folder` with a fixed name and none of the machine's settings.
    fn git_in(git: &Path, folder: &Path, arguments: &[&str]) {
        let status = plain(git, folder)
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

    /// whether git, as fetchloom runs it, works in `folder`. it refuses a repository on a disk
    /// that records no owner, as FAT does, unless the machine's settings trust it.
    fn trusted(git: &Path, folder: &Path) -> bool {
        run(git, folder, &["rev-parse", "--git-dir"]).is_ok()
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
            Some(planted.clone()),
            "the same git outside the project is found"
        );
        let installed = outside.path().join("git");
        std::fs::copy(&planted, &installed).unwrap();
        let path = std::env::join_paths([&bin, &outside.path().to_path_buf()]).unwrap();
        assert_eq!(
            find(Some(&path), project.path()),
            Some(installed),
            "the project's git hides none later on PATH"
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

    /// a file git merged by itself has no stages, so its sides come from the merged commits.
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

    /// git leaves `REBASE_HEAD` behind once a rebase that stopped at a conflict is done: only the
    /// rebase's own folder says one is under way, as `git status` tells it, and only then is the
    /// commit it names a side to read.
    #[test]
    fn a_rebase_is_under_way_only_while_its_folder_is_there() {
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
        assert_eq!(operation(&git, &root), None);
        git_in(&git, at, &["checkout", "-q", "theirs"]);
        git_in(&git, at, &["rebase", "-q", "main"]);
        assert_eq!(operation(&git, &root), Some(Operation::Rebase));
        assert!(committed(&git, &root, "notes").is_some());
        std::fs::write(root.join("data.toml"), one(7)).unwrap();
        git_in(&git, at, &["add", "."]);
        git_in(
            &git,
            at,
            &["-c", "core.editor=true", "rebase", "--continue"],
        );
        assert!(at.join(".git/REBASE_HEAD").exists(), "git keeps it");
        assert_eq!(operation(&git, &root), None);
        assert_eq!(committed(&git, &root, "notes"), None, "no side to read");
    }

    /// a folder is read as named, never as a pattern git matches: `run*` holds none of `run1`'s
    /// files, as an output's folder may be named so.
    #[test]
    fn tracked_reads_a_folder_as_named() {
        let Some(git) = git() else {
            return;
        };
        let repository = tempfile::tempdir().unwrap();
        let at = repository.path();
        std::fs::create_dir(at.join("run1")).unwrap();
        std::fs::write(at.join("run1/a.csv"), "1\n").unwrap();
        git_in(&git, at, &["init", "-q"]);
        git_in(&git, at, &["add", "-A"]);
        if !trusted(&git, at) {
            return;
        }
        let one = HashSet::from(["run1/a.csv".to_owned()]);
        assert_eq!(tracked(at, "run1"), Some(one));
        assert_eq!(tracked(at, "run*"), Some(HashSet::new()));
    }

    /// a hook or `git rebase --exec` hands its own repository down in `GIT_DIR` and its kin; the
    /// sides still come from the project's repository, and the other one is left alone.
    #[test]
    fn a_stray_git_dir_changes_nothing() {
        let Some(git) = git() else {
            return;
        };
        let elsewhere = tempfile::tempdir().unwrap();
        git_in(&git, elsewhere.path(), &["init", "-q"]);
        let dot = elsewhere.path().join(".git");
        let config = std::fs::read(dot.join("config")).unwrap();
        let child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "project::git::tests::the_sides_come_whole_from_the_index_and_the_base_is_empty_when_both_added_the_file",
                "project::git::tests::the_sides_of_a_file_git_merged_come_from_the_commits",
            ])
            .env("GIT_DIR", &dot)
            .env("GIT_WORK_TREE", elsewhere.path())
            .env("GIT_INDEX_FILE", dot.join("index"))
            .output()
            .unwrap();
        let printed = String::from_utf8_lossy(&child.stdout);
        assert!(
            child.status.success() && printed.contains("2 passed"),
            "{printed}"
        );
        assert_eq!(std::fs::read(dot.join("config")).unwrap(), config);
    }

    /// a repository that came as a folder, as in an archive, can carry settings that name a
    /// program to check signatures, which `git log` runs for a signed commit. a side's commit is
    /// read without it, as `git log --oneline` shows it.
    #[cfg(unix)]
    #[test]
    fn a_commit_is_read_without_running_a_program_the_repository_names() {
        use std::io::Write as _;
        use std::os::unix::fs::PermissionsExt;
        let Some(git) = git() else {
            return;
        };
        let folder = tempfile::tempdir().unwrap();
        let at = folder.path();
        git_in(&git, at, &["init", "-q"]);
        std::fs::write(at.join("notes"), "base\n").unwrap();
        git_in(&git, at, &["add", "."]);
        git_in(&git, at, &["commit", "-qm", "keep the\nnotes\n\nwhy"]);
        if !trusted(&git, at) {
            return;
        }
        let unsigned = run(&git, at, &["cat-file", "commit", "HEAD"]).unwrap();
        let signature = "gpgsig -----BEGIN PGP SIGNATURE-----\n \n -----END PGP SIGNATURE-----\n\n";
        let signed =
            String::from_utf8(unsigned)
                .unwrap()
                .replacen("\n\n", &format!("\n{signature}"), 1);
        let mut writing = plain(&git, at)
            .args(["hash-object", "-w", "-t", "commit", "--stdin"])
            .current_dir(at)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        writing
            .stdin
            .take()
            .unwrap()
            .write_all(signed.as_bytes())
            .unwrap();
        let written = writing.wait_with_output().unwrap();
        let id = String::from_utf8(written.stdout).unwrap();
        git_in(&git, at, &["update-ref", "HEAD", id.trim()]);
        let (checker, ran) = (at.join("checker"), at.join("ran"));
        std::fs::write(&checker, format!("#!/bin/sh\ntouch '{}'\n", ran.display())).unwrap();
        std::fs::set_permissions(&checker, std::fs::Permissions::from_mode(0o755)).unwrap();
        git_in(&git, at, &["config", "log.showSignature", "true"]);
        git_in(
            &git,
            at,
            &["config", "gpg.program", &checker.display().to_string()],
        );

        let short = run(&git, at, &["rev-parse", "--short", "HEAD"]).unwrap();
        let short = String::from_utf8(short).unwrap();
        assert_eq!(
            commit(&git, at, "HEAD"),
            Some(format!("{} keep the notes", short.trim()))
        );
        assert!(!ran.exists(), "reading the commit ran the checker");
        plain(&git, at)
            .args(["log", "-1"])
            .current_dir(at)
            .output()
            .unwrap();
        assert!(
            ran.exists(),
            "git log, as the repository sets it, runs the checker"
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
        let fetched = plain(&git, folder.path())
            .args(["-c", "safe.directory=*", "-C"])
            .arg(&clone)
            .args(["cat-file", "blob", "HEAD:./data.toml"])
            .output()
            .unwrap();
        assert!(
            fetched.status.success(),
            "the blob was there to fetch: {}",
            String::from_utf8_lossy(&fetched.stderr)
        );
    }
}
