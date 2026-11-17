//! no command runs a program because a project holds it. data.toml, data.lock and every file
//! beside them come with a clone, so a program in the working folder, or in a folder PATH names
//! relatively, never runs; git never runs from inside the project, even where PATH names that
//! folder by its full path; a setting in the repository's own git config never runs; and shell
//! words in names, links and paths stay words.
//!
//! the programs fetchloom does run are the ones a person configures: the `credential_process` an
//! AWS profile names (`buckets::a_credential_process_gives_the_keys_for_a_listed_bucket`), and the
//! AWS CLI (`buckets::a_listed_bucket_reads_with_the_aws_clis_default_chain`) and the az CLI
//! (`azure::a_listed_private_container_is_read_with_the_az_clis_login`) from an absolute folder on
//! PATH, which is the person's even inside a project, as an activated environment's is; git from
//! an absolute folder outside the project; the system's own `id`. gcloud never runs: fetchloom
//! reads the login it saves (`google::a_listed_private_bucket_is_read_with_application_default_credentials`).
//! the tests here and beside those hold each of them to that boundary.

use std::ffi::OsString;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use crate::support::{Result, Routes, Sandbox, file, text, zip_of};

/// what fetchloom, or a tool it might start, could look for by name.
const TOOLS: [&str; 31] = [
    "git",
    "sh",
    "bash",
    "id",
    "aws",
    "az",
    "gcloud",
    "python",
    "python3",
    "fetchloom",
    "fl",
    "curl",
    "wget",
    "tar",
    "unzip",
    "gzip",
    "bzip2",
    "xz",
    "zstd",
    "7z",
    "unrar",
    "ssh",
    "gpg",
    "less",
    "more",
    "open",
    "xdg-open",
    "getent",
    "whoami",
    "pwsh",
    "powershell",
];

/// a program named `tool` in `folder` that leaves `<ran>/<tool>` when it runs: a script on unix;
/// on windows a batch file of each kind that does the same, and an `.exe` that is no program, whose
/// start fails.
fn bait(folder: &Path, tool: &str, ran: &Path) -> Result {
    let marker = ran.join(tool);
    #[cfg(unix)]
    crate::buckets::program(&folder.join(tool), &format!(": > '{}'", marker.display()))?;
    #[cfg(windows)]
    {
        for extension in ["cmd", "bat"] {
            fs::write(
                folder.join(format!("{tool}.{extension}")),
                format!("@echo off\r\ntype nul > \"{}\"\r\n", marker.display()),
            )?;
        }
        fs::write(folder.join(format!("{tool}.exe")), b"not a program")?;
    }
    Ok(())
}

/// the tools that ran, by the marks their baits left.
fn ran(marks: &Path) -> Result<Vec<String>> {
    let mut names: Vec<String> = fs::read_dir(marks)?
        .map(|entry| Ok(entry?.file_name().to_string_lossy().into_owned()))
        .collect::<std::io::Result<_>>()?;
    names.sort();
    Ok(names)
}

/// the absolute folders of this machine's PATH, where git and the system's own programs are.
fn system_folders() -> Vec<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .filter(|folder| folder.is_absolute())
        .collect()
}

/// a cloned project at `project`, a bait of every tool in it, in its `bin` and in its
/// `.venv/bin`, and a PATH as a person's shell might hold in it: the working folder, an empty
/// entry and `bin` by relative names first, then `.venv/bin` by its full path, as an activated
/// environment adds it, then this machine's own folders.
struct Planted {
    marks: PathBuf,
    path: OsString,
}

impl Planted {
    fn new(sandbox: &Sandbox, project: &Path) -> Result<Planted> {
        let marks = sandbox.folder("ran")?;
        let venv = project.join(".venv").join("bin");
        for folder in [project.to_path_buf(), project.join("bin"), venv.clone()] {
            fs::create_dir_all(&folder)?;
            for tool in TOOLS {
                bait(&folder, tool, &marks)?;
            }
        }
        let mut folders = vec![
            PathBuf::from("."),
            PathBuf::new(),
            PathBuf::from("bin"),
            venv,
        ];
        folders.extend(system_folders());
        Ok(Planted {
            marks,
            path: std::env::join_paths(folders)?,
        })
    }

    /// fetchloom with `args` in `folder` under the planted PATH, with git's variables of the
    /// machine running the tests left out; fails the test when any bait ran.
    fn run_in(&self, sandbox: &Sandbox, folder: &Path, args: &[&str]) -> Result<Output> {
        let mut command = sandbox.command_in(folder, args);
        for name in crate::merge::git_variables() {
            command.env_remove(name);
        }
        let output = command.env("PATH", &self.path).output()?;
        assert_eq!(
            ran(&self.marks)?,
            Vec::<String>::new(),
            "{args:?} ran a program the project holds: {}",
            text(&output.stderr)?
        );
        Ok(output)
    }

    /// [`Planted::run_in`] in the work folder, which must succeed.
    fn ok(&self, sandbox: &Sandbox, args: &[&str]) -> Result<Output> {
        let output = self.run_in(sandbox, &sandbox.work(), args)?;
        assert_eq!(
            output.status.code(),
            Some(0),
            "{args:?}: {}",
            text(&output.stderr)?
        );
        Ok(output)
    }
}

/// what a repository's own config may tell git to run, each a bait; git reads `.git/config`
/// from the clone itself, as a project shared as a folder or an archive brings it.
fn hostile_git_config(work: &Path, marks: &Path) -> Result {
    let hooks = work.join(".git").join("hooks-bait");
    fs::create_dir_all(&hooks)?;
    let program = |name: &str| -> Result<String> {
        let path = hooks.join(name);
        #[cfg(unix)]
        crate::buckets::program(
            &path,
            &format!(": > '{}'", marks.join(format!("git-{name}")).display()),
        )?;
        #[cfg(windows)]
        fs::write(
            &path,
            format!(
                "#!/bin/sh\n: > '{}'\n",
                marks.join(format!("git-{name}")).display()
            ),
        )?;
        Ok(path.display().to_string().replace('\\', "/"))
    };
    for hook in [
        "pre-commit",
        "post-checkout",
        "post-merge",
        "post-index-change",
        "reference-transaction",
        "fsmonitor-watchman",
    ] {
        program(hook)?;
    }
    let hooks_path = hooks.display().to_string().replace('\\', "/");
    let settings = [
        ("core", "fsmonitor", program("fsmonitor")?),
        ("core", "hooksPath", hooks_path),
        ("core", "pager", program("pager")?),
        ("core", "editor", program("editor")?),
        ("core", "sshCommand", program("ssh")?),
        ("core", "askPass", program("askpass")?),
        ("diff", "external", program("external-diff")?),
        ("diff \"bait\"", "textconv", program("textconv")?),
        ("filter \"bait\"", "clean", program("clean")?),
        ("filter \"bait\"", "smudge", program("smudge")?),
        ("filter \"bait\"", "process", program("process")?),
        (
            "credential",
            "helper",
            format!("!{}", program("credential")?),
        ),
        ("gpg", "program", program("gpg")?),
        ("sequence", "editor", program("sequence-editor")?),
    ];
    let mut config = fs::read_to_string(work.join(".git").join("config"))?;
    for (section, key, value) in settings {
        writeln!(config, "[{section}]\n\t{key} = {value}")?;
    }
    fs::write(work.join(".git").join("config"), config)?;
    fs::write(work.join(".gitattributes"), "* filter=bait diff=bait\n")?;
    Ok(())
}

/// a link whose path and query hold what a shell would act on, and the name its file takes.
const SHELL_WORDS: &str = "/m$(reboot)/a;b&c'd!e.csv";
const SHELL_QUERY: &str = "?x=$(id)&y=`id`|id";

/// a project with a git history, as its author committed it before this machine saw it: iris,
/// and a link whose path and query hold shell words, placed in a folder whose name does too.
/// none without git.
fn clone_with_shell_words(sandbox: &Sandbox, routes: &Routes) -> Result<Option<String>> {
    if crate::merge::ok(sandbox, &["init", "-q"])?.is_none() {
        return Ok(None);
    }
    let work = sandbox.work();
    let added = sandbox.run(&["add", &format!("{}/iris.csv", routes.url), "--as", "iris"])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    crate::merge::ok(sandbox, &["add", "."])?;
    crate::merge::ok(sandbox, &["commit", "-qm", "iris"])?;
    let weird = format!("{}{SHELL_WORDS}{SHELL_QUERY}", routes.url);
    let added = sandbox.run(&["add", &weird, "--as", "weird"])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let manifest = fs::read_to_string(work.join("data.toml"))?;
    fs::write(
        work.join("data.toml"),
        manifest.replace(
            "[datasets.weird]\n",
            "[datasets.weird]\npath = \"raw/$(reboot);&'x\"\n",
        ),
    )?;
    crate::merge::ok(sandbox, &["add", "."])?;
    crate::merge::ok(sandbox, &["commit", "-qm", "weird"])?;
    Ok(Some(weird))
}

/// every command, run in a cloned project that plants a bait of every tool where a careless
/// lookup would find it, with a repository config that names baits for git to run, a link,
/// a destination, a project folder and a cache folder that hold shell words. none of them runs,
/// the sync that moves a dataset's .gitignore into a folder named with shell words included.
#[test]
fn no_command_runs_a_program_the_project_holds() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = Routes::start(vec![
        (
            "/iris.csv".to_owned(),
            file(b"sepal,species\n5.1,setosa\n", "\"1\""),
        ),
        (
            "/late.zip".to_owned(),
            file(&zip_of(&[("a.csv", b"a\n"), ("b.csv", b"b\n")])?, "\"2\""),
        ),
        (
            format!("{SHELL_WORDS}{SHELL_QUERY}"),
            file(b"words\n", "\"3\""),
        ),
        (
            "/reads.csv.gz".to_owned(),
            file(&crate::decompress::gz(b"r\n")?, "\"4\""),
        ),
        (
            "/notes.txt.bz2".to_owned(),
            file(&crate::decompress::bz2(b"n\n")?, "\"5\""),
        ),
    ])?;
    sandbox.allow(&routes.url);
    let Some(weird) = clone_with_shell_words(&sandbox, &routes)? else {
        return Ok(());
    };
    let work = sandbox.work();
    let planted = Planted::new(&sandbox, &work)?;
    hostile_git_config(&work, &planted.marks)?;
    planted.ok(&sandbox, &["sync"])?;
    assert_eq!(
        fs::read(work.join("data/raw/$(reboot);&'x/a;b&c'd!e.csv"))?,
        b"words\n"
    );
    for folder in ["data/iris", "data/raw/$(reboot);&'x"] {
        assert_eq!(fs::read(work.join(folder).join(".gitignore"))?, b"*\n");
    }
    assert!(!work.join("data/weird").exists());
    planted.ok(
        &sandbox,
        &["add", &format!("{}/late.zip", routes.url), "--as", "late"],
    )?;
    assert_eq!(fs::read(work.join("data/late/b.csv"))?, b"b\n");
    decompressing(&sandbox, &planted, &routes)?;
    let mirror = sandbox.folder("mirror")?.display().to_string();
    let got = sandbox.folder("got")?.display().to_string();
    let link = format!("{}/iris.csv", routes.url);
    for args in [
        &["lock"][..],
        &["lock", "--check"],
        &["status"],
        &["--json", "status"],
        &["sync"],
        &["sync", "--dry-run"],
        &["audit"],
        &["diff"],
        &["diff", "HEAD~1", "HEAD"],
        &["list"],
        &["list", "weird"],
        &["export"],
        &["export", "--format", "links"],
        &["export", "--format", "bibtex"],
        &["push", "--to", &mirror, "--dry-run"],
        &["push", "--to", &mirror, "--yes"],
        &["path", "iris"],
        &["exclude", "data/late/a.csv", "--dry-run"],
        &["drop", "late", "--yes"],
        &["sync", "late"],
        &["remove", "late", "--yes"],
        &["cache"],
        &["cache", "gc", "--now", "--yes"],
        &["cache", "verify"],
        &["settings"],
        &["settings", "set", "jobs", "2"],
        &["settings", "unset", "jobs"],
        &["get", &link, "--dir", &got],
        &["auth"],
        &["completions", "bash"],
        &["help"],
        &["help", "concepts"],
        &["--version"],
    ] {
        planted.ok(&sandbox, args)?;
    }
    // a folder of the project pinned where it is, as an output: captured, pinned again after a
    // rerun, previewed, checked, put back whole and untracked
    fs::create_dir_all(work.join("results"))?;
    fs::write(work.join("results/r.csv"), b"r\n")?;
    planted.ok(&sandbox, &["add", "results", "--as", "results"])?;
    fs::write(work.join("results/r.csv"), b"rerun\n")?;
    planted.ok(&sandbox, &["add", "results", "--as", "results", "--yes"])?;
    planted.ok(&sandbox, &["add", "results", "--as", "results", "-n"])?;
    planted.ok(&sandbox, &["status", "--verify", "--check"])?;
    fs::write(work.join("results/late.csv"), b"late\n")?;
    planted.ok(&sandbox, &["sync", "--force", "results"])?;
    assert!(!work.join("results/late.csv").exists());
    planted.ok(&sandbox, &["remove", "results", "--yes"])?;
    assert_eq!(fs::read(work.join("results/r.csv"))?, b"rerun\n");
    // a search reaches sites no test may ask, so it is run offline, where it fails
    planted.run_in(&sandbox, &work, &["--offline", "search", "iris"])?;
    in_folders_with_shell_words(&sandbox, &weird)
}

/// a single compressed file added with `--decompress`, a sync that decompresses what data.toml
/// asks of it, and a get of each into a folder and at `-o`, each decoded by fetchloom itself.
fn decompressing(sandbox: &Sandbox, planted: &Planted, routes: &Routes) -> Result {
    let work = sandbox.work();
    planted.ok(
        sandbox,
        &[
            "add",
            &format!("{}/reads.csv.gz", routes.url),
            "--decompress",
            "--as",
            "reads",
        ],
    )?;
    assert_eq!(fs::read(work.join("data/reads/reads.csv"))?, b"r\n");
    let manifest = fs::read_to_string(work.join("data.toml"))?;
    fs::write(
        work.join("data.toml"),
        format!(
            "{manifest}\n[datasets.notes]\nref = \"{}/notes.txt.bz2\"\ndecompress = true\n",
            routes.url
        ),
    )?;
    planted.ok(sandbox, &["sync", "notes"])?;
    assert_eq!(fs::read(work.join("data/notes/notes.txt"))?, b"n\n");
    let got = sandbox.folder("decoded")?;
    let reads = format!("{}/reads.csv.gz", routes.url);
    let into = got.display().to_string();
    planted.ok(sandbox, &["get", &reads, "--decompress", "--dir", &into])?;
    assert_eq!(fs::read(got.join("reads.csv"))?, b"r\n");
    let notes = format!("{}/notes.txt.bz2", routes.url);
    let at = got.join("notes.txt").display().to_string();
    planted.ok(sandbox, &["get", &notes, "--decompress", "-o", &at])?;
    assert_eq!(fs::read(got.join("notes.txt"))?, b"n\n");
    Ok(())
}

/// a project, and a cache, in folders whose names a shell would act on, each project folder with
/// its own baits.
fn in_folders_with_shell_words(sandbox: &Sandbox, link: &str) -> Result {
    let named = sandbox.folder("p $(reboot);&'x")?;
    let cache = sandbox.folder("c $(reboot);&'x")?;
    let planted = Planted::new(sandbox, &named)?;
    for args in [
        &["add", link, "--as", "weird"][..],
        &["sync"],
        &["status"],
        &["cache", "gc", "--now", "--yes"],
    ] {
        let output = sandbox
            .command_in(&named, args)
            .env("PATH", &planted.path)
            .env("FETCHLOOM_CACHE", &cache)
            .output()?;
        assert_eq!(
            output.status.code(),
            Some(0),
            "{args:?}: {}",
            text(&output.stderr)?
        );
        assert_eq!(ran(&planted.marks)?, Vec::<String>::new(), "{args:?}");
    }
    Ok(())
}

/// a merge that git stopped at conflicts in data.toml and data.lock, read through git's index
/// with the repository's config naming a bait for everything git could run.
#[test]
fn merging_reads_git_without_running_what_the_repository_configures() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = crate::merge::three_files()?;
    sandbox.allow(&routes.url);
    if crate::merge::two_additions(&sandbox, &routes, "merge")?.is_none() {
        return Ok(());
    }
    let work = sandbox.work();
    let planted = Planted::new(&sandbox, &work)?;
    hostile_git_config(&work, &planted.marks)?;
    for args in [
        &["status"][..],
        &["lock", "--check"],
        &["sync"],
        &["lock"],
        &["status"],
    ] {
        planted.run_in(&sandbox, &work, args)?;
    }
    let lock = fs::read_to_string(work.join("data.lock"))?;
    for name in ["alpha", "beta", "core"] {
        assert!(lock.contains(&format!("name = \"{name}\"")), "{lock}");
    }
    assert!(!lock.contains("<<<<<<<"), "{lock}");
    Ok(())
}

/// data.toml holds no command: a key that would name one is refused as any unknown key is, and a
/// dataset name a shell would act on is refused before anything is fetched.
#[test]
fn a_data_toml_naming_a_command_or_a_shell_word_is_refused() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    let planted = Planted::new(&sandbox, &work)?;
    let mark = planted.marks.join("data.toml");
    let command = format!(
        "[\"sh\", \"-c\", \": > '{}'\"]",
        mark.display().to_string().replace('\\', "/")
    );
    let mut tables = Vec::new();
    for key in ["run", "cmd", "script", "build", "command", "exec", "hook"] {
        tables.push(format!(
            "{key} = {command}\n[datasets.iris]\nref = \"zenodo:1\"\n"
        ));
        tables.push(format!(
            "[datasets.iris]\nref = \"zenodo:1\"\n{key} = {command}\n"
        ));
    }
    for name in [
        "$(reboot)",
        "a;b",
        "a|b",
        "a&b",
        "`id`",
        "a b",
        "../up",
        "a'b",
    ] {
        tables.push(format!("[datasets.{name:?}]\nref = \"zenodo:1\"\n"));
    }
    for table in tables {
        fs::write(work.join("data.toml"), &table)?;
        for args in [
            &["--offline", "sync"][..],
            &["--offline", "lock"],
            &["list"],
            &["status"],
        ] {
            let output = planted.run_in(&sandbox, &work, args)?;
            let stderr = text(&output.stderr)?;
            assert!(!output.status.success(), "{table}{args:?}: {stderr}");
            assert!(
                stderr.contains("data.toml has a key fetchloom does not know")
                    || stderr.contains("is not a valid dataset name"),
                "{table}{args:?}: {stderr}"
            );
        }
        assert!(!mark.exists(), "{table}");
    }
    fs::remove_file(work.join("data.toml"))?;
    let refused = planted.run_in(
        &sandbox,
        &work,
        &["--offline", "add", "zenodo:1", "--as", "$(reboot)"],
    )?;
    let stderr = text(&refused.stderr)?;
    assert_eq!(refused.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("not a valid dataset name"), "{stderr}");
    Ok(())
}

/// a lab's shared cache registers projects of other users. when one cannot be read, gc names its
/// owner with the system's `id`, never one in the working folder or in a folder PATH names
/// relatively or that the project holds, where a clone could have put a program of that name.
#[cfg(unix)]
#[test]
fn the_owner_of_an_unreadable_project_is_named_by_the_systems_id() -> Result {
    use crate::support::{sha256_hex, skipped};
    use std::os::unix::fs::MetadataExt;
    let sandbox = Sandbox::new()?;
    let me = fs::metadata(sandbox.work())?.uid();
    if me == 0 {
        return skipped("unprivileged", "root may look into every folder");
    }
    // a folder of another user's that this one may not look into, as root's home
    let Some(theirs) = ["/root", "/var/root"]
        .into_iter()
        .map(Path::new)
        .find(|folder| {
            fs::metadata(folder).is_ok_and(|found| found.uid() != me)
                && fs::metadata(folder.join("project"))
                    .is_err_and(|err| err.kind() == std::io::ErrorKind::PermissionDenied)
        })
    else {
        return skipped(
            "closed-folder",
            "no folder of another user's is closed to this one",
        );
    };
    sandbox.make_cache(&sandbox.cache())?;
    let root = theirs.join("project");
    let key = sha256_hex(root.as_os_str().as_encoded_bytes());
    fs::write(
        sandbox.cache().join("projects").join(format!("{key}.json")),
        format!("{{\"root\":\"{}\"}}", root.display()),
    )?;
    let planted = Planted::new(&sandbox, &sandbox.work())?;
    let output = planted.run_in(&sandbox, &sandbox.work(), &["cache", "gc"])?;
    let stderr = text(&output.stderr)?;
    assert_ne!(output.status.code(), Some(0), "{stderr}");
    let owner = fs::metadata(theirs)?.uid();
    let named = std::process::Command::new("/usr/bin/id")
        .args(["-nu", &owner.to_string()])
        .output()?;
    let name = String::from_utf8(named.stdout)?.trim().to_owned();
    assert!(
        stderr.contains(&format!("{} belongs to {name}:", root.display())),
        "{stderr}"
    );
    Ok(())
}
