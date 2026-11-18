//! what fetchloom owns in a project: its datasets' folders, and nothing beside them. a file
//! outside every dataset's folder is the person's alone, never counted, listed or touched, and one
//! a person puts inside a dataset's folder is theirs too, named once and kept.

use std::fs;

use crate::project::{IRIS, PATH};
use crate::support::{
    Result, Routes, Sandbox, add_ok, file, json, ok, text, without_copy_warning, zip_of,
};

/// a server answering iris at [`PATH`], as often as asked.
pub(crate) fn iris_server() -> Result<Routes> {
    Routes::start(vec![(PATH.to_owned(), file(IRIS, "\"v1\""))])
}

/// iris added to the project in `sandbox` from `server`, its file at data/iris/iris.csv.
pub(crate) fn with_iris(sandbox: &Sandbox, server: &Routes) -> Result {
    add_ok(sandbox, &format!("{}{PATH}", server.url))
}

/// writes each `(path, text)` below the project in `sandbox`, making its folders.
fn write_all(sandbox: &Sandbox, files: &[(&str, &str)]) -> Result {
    for (path, text) in files {
        let file = sandbox.work().join(path);
        fs::create_dir_all(file.parent().ok_or("a path outside any folder")?)?;
        fs::write(&file, text)?;
    }
    Ok(())
}

/// status judges only what lies in a dataset's folder: files a pipeline writes beside the
/// datasets, as cookiecutter's data/processed and data/interim, are never counted or listed, and
/// files a person puts in a dataset's folder are theirs, kept, never counted: a plain status says
/// nothing of them, `-v` and json name them. sync, with --force too, leaves every one as it was,
/// and lock --check has nothing to say of them.
#[test]
fn only_files_in_dataset_folders_are_judged() -> Result {
    let sandbox = Sandbox::new()?;
    let server = iris_server()?;
    with_iris(&sandbox, &server)?;
    let theirs = [
        ("data/processed/clean.csv", "a\n"),
        ("data/interim/step1.txt", "b\n"),
        ("data/iris/iris_clean.csv", "c\n"),
        ("data/iris/notes/readme.md", "d\n"),
    ];
    write_all(&sandbox, &theirs)?;

    let status = sandbox.run(&["status"])?;
    assert_eq!(status.status.code(), Some(0));
    assert_eq!(text(&status.stdout)?, "", "no row, since nothing differs");
    let told = without_copy_warning(&text(&status.stderr)?);
    assert_eq!(
        told,
        "         Ok everything matches data.lock by size and time\n"
    );
    let bare = sandbox.run(&[])?;
    assert_eq!(
        without_copy_warning(&text(&bare.stderr)?),
        "         Ok everything matches data.lock by size and time\n",
        "the one-line answer counts none of them"
    );
    let detailed = without_copy_warning(&text(&sandbox.run(&["status", "-v"])?.stderr)?);
    assert!(
        detailed.contains(
            "       Kept 2 files of yours in dataset folders\n            data/iris/iris_clean.csv\n            data/iris/notes/readme.md\n"
        ),
        "{detailed}"
    );
    assert!(!detailed.contains("processed"), "{detailed}");
    let document = json(&sandbox.run(&["--json", "status"])?)?;
    let result = &document["result"];
    assert_eq!(result["extra"], serde_json::json!([]), "{document}");
    assert_eq!(
        result["yours"],
        serde_json::json!(["data/iris/iris_clean.csv", "data/iris/notes/readme.md"]),
        "{document}"
    );
    let whole = document.to_string();
    assert!(
        !whole.contains("processed") && !whole.contains("interim"),
        "{whole}"
    );

    write_all(&sandbox, &[("data/iris/one.csv", "e\n")])?;
    fs::remove_file(sandbox.work().join("data/iris/notes/readme.md"))?;
    fs::remove_file(sandbox.work().join("data/iris/iris_clean.csv"))?;
    let one = without_copy_warning(&text(&sandbox.run(&["status"])?.stderr)?);
    assert!(!one.contains("one.csv"), "{one}");
    let one = without_copy_warning(&text(&sandbox.run(&["status", "-v"])?.stderr)?);
    assert!(
        one.contains("       Kept data/iris/one.csv · yours\n"),
        "{one}"
    );

    for args in [&["sync"][..], &["sync", "--force"], &["lock", "--check"]] {
        let output = sandbox.run(args)?;
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(0), "{args:?}: {stderr}");
        assert!(!stderr.contains("processed"), "{args:?}: {stderr}");
        assert!(!stderr.contains("one.csv"), "{args:?}: {stderr}");
    }
    for (path, text) in [
        ("data/processed/clean.csv", "a\n"),
        ("data/interim/step1.txt", "b\n"),
        ("data/iris/one.csv", "e\n"),
    ] {
        assert_eq!(
            fs::read_to_string(sandbox.work().join(path))?,
            text,
            "{path}"
        );
    }
    Ok(())
}

/// the files git does not have and does not ignore below the project in `sandbox`, sorted, as
/// `git status` lists them one by one; none without git.
fn untracked(sandbox: &Sandbox) -> Result<Option<Vec<String>>> {
    let Some(status) = git(sandbox, &["status", "--porcelain", "--untracked-files=all"])? else {
        return Ok(None);
    };
    assert!(status.status.success(), "{}", text(&status.stderr)?);
    let mut paths: Vec<String> = text(&status.stdout)?
        .lines()
        .filter_map(|line| line.strip_prefix("?? "))
        .map(str::to_owned)
        .collect();
    paths.sort();
    Ok(Some(paths))
}

/// git with `args` in the project of `sandbox`, none without git.
fn git(sandbox: &Sandbox, args: &[&str]) -> Result<Option<std::process::Output>> {
    crate::merge::git(sandbox, &sandbox.work(), args)
}

/// the rule git says ignores `path`, as `git check-ignore -v` prints it.
fn ignored_by(sandbox: &Sandbox, path: &str) -> Result<String> {
    let checked = git(sandbox, &["check-ignore", "-v", path])?.ok_or("git went away")?;
    text(&checked.stdout)
}

/// the line an add or a sync prints for the one dataset folder it keeps out of git.
fn ignored_line(folder: &str) -> String {
    format!(
        "    Ignored {folder} in git with {folder}/.gitignore, which also hides files you put there\n"
    )
}

/// a1 in a clean root: the add keeps data/iris out of git with a .gitignore of its own there and
/// never writes the root's, so git sees what a pipeline writes beside the datasets, and status
/// counts none of it.
#[test]
fn a_clean_root_leaves_git_all_but_the_dataset_folders() -> Result {
    let sandbox = Sandbox::new()?;
    if crate::merge::ok(&sandbox, &["init", "-q"])?.is_none() {
        return Ok(());
    }
    let server = iris_server()?;
    let link = format!("{}{PATH}", server.url);
    sandbox.allow(&link);
    let added = sandbox.run(&["add", &link])?;
    let stderr = text(&added.stderr)?;
    assert_eq!(added.status.code(), Some(0), "{stderr}");
    assert!(stderr.contains(&ignored_line("data/iris")), "{stderr}");
    assert!(!sandbox.work().join(".gitignore").exists());
    assert_eq!(
        crate::support::marker(&sandbox.work().join("data/iris/.gitignore"))?,
        b"*\n"
    );
    write_all(
        &sandbox,
        &[
            ("data/processed/clean.csv", "a\n"),
            ("data/interim/step1.txt", "b\n"),
        ],
    )?;
    let status = sandbox.run(&["status"])?;
    assert_eq!(text(&status.stdout)?, "");
    assert_eq!(
        without_copy_warning(&text(&status.stderr)?),
        "         Ok everything matches data.lock by size and time\n"
    );
    assert_eq!(
        untracked(&sandbox)?.ok_or("git went away")?,
        [
            "data.lock",
            "data.toml",
            "data/interim/step1.txt",
            "data/processed/clean.csv"
        ]
    );
    assert_eq!(
        ignored_by(&sandbox, "data/iris/iris.csv")?,
        "data/iris/.gitignore:1:*\tdata/iris/iris.csv\n"
    );
    Ok(())
}

/// a1 in a cookiecutter root, whose .gitignore hides all of data/: an add at a terminal says once
/// which line hides the person's own files too, as theirs to keep or take out, and leaves the file
/// as it was. a run with no terminal, as in CI, never says it. status never counts the template's
/// .gitkeep files, and once the person takes the line out git sees them, never the dataset's files.
#[test]
fn a_root_rule_hiding_the_data_folder_is_explained_once() -> Result {
    let sandbox = Sandbox::new()?;
    if crate::merge::ok(&sandbox, &["init", "-q"])?.is_none() {
        return Ok(());
    }
    let rules = "# exclude data from source control by default\n/data/\n";
    fs::write(sandbox.work().join(".gitignore"), rules)?;
    write_all(
        &sandbox,
        &[
            ("data/external/.gitkeep", ""),
            ("data/interim/.gitkeep", ""),
            ("data/processed/.gitkeep", ""),
            ("data/raw/.gitkeep", ""),
        ],
    )?;
    let routes = ["iris", "wine", "beer", "milk"].into_iter().map(|name| {
        (
            format!("/files/{name}.csv"),
            file(format!("{name}\n1\n").as_bytes(), "\"v1\""),
        )
    });
    let server = Routes::start(routes.collect())?;
    sandbox.allow(&server.url);
    let link = |name: &str| format!("{}/files/{name}.csv", server.url);
    let added = sandbox.run(&["add", &link("iris")])?;
    let stderr = text(&added.stderr)?;
    assert_eq!(added.status.code(), Some(0), "{stderr}");
    assert!(stderr.contains(&ignored_line("data/iris")), "{stderr}");
    assert!(!stderr.contains("keeps all of data/"), "{stderr}");
    let scripted = sandbox.run(&["--json", "add", &link("wine")])?;
    assert_eq!(
        scripted.status.code(),
        Some(0),
        "{}",
        text(&scripted.stderr)?
    );
    let kinds = json(&scripted)?;
    let kinds = crate::support::kinds(&kinds);
    assert!(!kinds.contains(&"git.data_folder_ignored"), "{kinds:?}");
    #[cfg(target_os = "linux")]
    {
        let explained = "warning: line 2 of .gitignore, /data/, keeps all of data/ out of git, your own files there too\n  help: each dataset's folder keeps itself out of git, so the line now only hides your own files in data/: keep it if that is what you want, or take it out for git to see them. fetchloom never edits .gitignore\n";
        if let Some(screen) = sandbox.on_a_terminal(&["add", &link("beer")])? {
            assert!(screen.contains(explained), "{screen}");
            let again = sandbox
                .on_a_terminal(&["add", &link("milk")])?
                .unwrap_or_default();
            assert!(again.contains("Added milk"), "{again}");
            assert!(!again.contains("keeps all of data/"), "said once: {again}");
        }
    }
    assert_eq!(
        fs::read_to_string(sandbox.work().join(".gitignore"))?,
        rules
    );
    let status = sandbox.run(&["status"])?;
    assert_eq!(text(&status.stdout)?, "");
    assert!(!text(&status.stderr)?.contains("gitkeep"));
    fs::write(
        sandbox.work().join(".gitignore"),
        "# exclude data from source control by default\n",
    )?;
    assert_eq!(
        untracked(&sandbox)?.ok_or("git went away")?,
        [
            ".gitignore",
            "data.lock",
            "data.toml",
            "data/external/.gitkeep",
            "data/interim/.gitkeep",
            "data/processed/.gitkeep",
            "data/raw/.gitkeep"
        ]
    );
    Ok(())
}

/// a1 in a project an older fetchloom set up: `/data/` in .gitignore, the files placed, no
/// dataset folder kept out of git by a file of its own. the next sync makes each one, and with no
/// terminal, as a script runs it, says nothing of the line; the sync after says nothing at all.
#[test]
fn an_upgraded_project_gets_its_folders_marked_and_the_line_explained() -> Result {
    let sandbox = Sandbox::new()?;
    if crate::merge::ok(&sandbox, &["init", "-q"])?.is_none() {
        return Ok(());
    }
    let server = iris_server()?;
    // a file of the person's own where the marker goes keeps the add from making one
    write_all(&sandbox, &[("data/iris/.gitignore", "mine\n")])?;
    with_iris(&sandbox, &server)?;
    fs::remove_file(sandbox.work().join("data/iris/.gitignore"))?;
    fs::write(sandbox.work().join(".gitignore"), "/data/\n")?;

    let synced = sandbox.run(&["sync"])?;
    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(0), "{stderr}");
    assert!(stderr.contains(&ignored_line("data/iris")), "{stderr}");
    assert!(!stderr.contains("keeps all of data/"), "{stderr}");
    assert_eq!(
        crate::support::marker(&sandbox.work().join("data/iris/.gitignore"))?,
        b"*\n"
    );
    assert_eq!(
        fs::read_to_string(sandbox.work().join(".gitignore"))?,
        "/data/\n"
    );
    let quiet = sandbox.run(&["sync"])?;
    assert_eq!(
        without_copy_warning(&text(&quiet.stderr)?),
        "         Ok all pinned files are in place by size and time, nothing to do\n"
    );
    Ok(())
}

/// b1: git tracks a file of the person's beside the datasets. the add warns of nothing and keeps
/// only data/iris out of git, and the line the old warning asked for changes nothing either.
#[test]
fn files_git_tracks_beside_the_datasets_raise_no_warning() -> Result {
    let sandbox = Sandbox::new()?;
    write_all(&sandbox, &[("data/processed/s.csv", "1\n")])?;
    if crate::merge::ok(&sandbox, &["init", "-q"])?.is_none() {
        return Ok(());
    }
    crate::merge::ok(&sandbox, &["add", "-A"])?;
    crate::merge::ok(&sandbox, &["commit", "-qm", "init"])?;
    let server = iris_server()?;
    let link = format!("{}{PATH}", server.url);
    sandbox.allow(&link);
    let added = json(&sandbox.run(&["--json", "add", &link])?)?;
    assert_eq!(added["status"], "ok", "{added}");
    assert_eq!(crate::support::kinds(&added), Vec::<&str>::new(), "{added}");
    assert_eq!(
        untracked(&sandbox)?.ok_or("git went away")?,
        ["data.lock", "data.toml"]
    );
    fs::write(sandbox.work().join(".gitignore"), "/data/iris/\n")?;
    for args in [&["--json", "sync"][..], &["--json", "sync", "--force"]] {
        let synced = json(&sandbox.run(args)?)?;
        assert_eq!(synced["status"], "ok", "{synced}");
        assert_eq!(
            crate::support::kinds(&synced),
            Vec::<&str>::new(),
            "{synced}"
        );
    }
    assert_eq!(
        fs::read_to_string(sandbox.work().join(".gitignore"))?,
        "/data/iris/\n"
    );
    Ok(())
}

/// b6: the person ignores data/ but takes data/processed back, git's way to commit one folder
/// there. sync leaves those rules as written, so the table stays in `git status`.
#[test]
fn a_persons_rules_for_the_data_folder_stay_as_written() -> Result {
    let sandbox = Sandbox::new()?;
    if crate::merge::ok(&sandbox, &["init", "-q"])?.is_none() {
        return Ok(());
    }
    let server = iris_server()?;
    with_iris(&sandbox, &server)?;
    let rules = "/data/*\n!/data/processed/\n";
    fs::write(sandbox.work().join(".gitignore"), rules)?;
    write_all(&sandbox, &[("data/processed/table.csv", "1\n")])?;
    let before = untracked(&sandbox)?.ok_or("git went away")?;
    assert!(
        before.contains(&"data/processed/table.csv".to_owned()),
        "{before:?}"
    );
    for args in [&["sync"][..], &["sync", "--force"]] {
        let synced = sandbox.run(args)?;
        let stderr = text(&synced.stderr)?;
        assert_eq!(synced.status.code(), Some(0), "{stderr}");
        assert!(!stderr.contains(".gitignore"), "{stderr}");
    }
    assert_eq!(
        fs::read_to_string(sandbox.work().join(".gitignore"))?,
        rules
    );
    assert_eq!(untracked(&sandbox)?.ok_or("git went away")?, before);
    Ok(())
}

/// a dataset folder's marker goes with the dataset: `remove` takes it and the folder, and a new
/// `path` moves it with the files. one a person changed is theirs, kept with their files, and
/// git sees those once no dataset holds the folder.
#[test]
fn a_marker_goes_with_its_dataset() -> Result {
    let sandbox = Sandbox::new()?;
    let in_git = crate::merge::ok(&sandbox, &["init", "-q"])?.is_some();
    let server = iris_server()?;
    with_iris(&sandbox, &server)?;
    let data = sandbox.work().join("data");
    assert_eq!(
        crate::support::marker(&data.join("iris/.gitignore"))?,
        b"*\n"
    );

    let toml = fs::read_to_string(sandbox.work().join("data.toml"))?;
    fs::write(
        sandbox.work().join("data.toml"),
        toml.replace(
            "[datasets.iris]\n",
            "[datasets.iris]\npath = \"raw/iris\"\n",
        ),
    )?;
    let moved = sandbox.run(&["sync"])?;
    assert_eq!(moved.status.code(), Some(0), "{}", text(&moved.stderr)?);
    assert!(!data.join("iris").exists(), "the old folder went whole");
    assert_eq!(
        crate::support::marker(&data.join("raw/iris/.gitignore"))?,
        b"*\n"
    );

    let removed = sandbox.run(&["remove", "iris"])?;
    assert_eq!(removed.status.code(), Some(0), "{}", text(&removed.stderr)?);
    assert!(
        !data.join("raw").exists(),
        "{:?}",
        fs::read_dir(&data)?.count()
    );

    with_iris(&sandbox, &server)?;
    fs::write(data.join("iris/.gitignore"), "*\n!notes.txt\n")?;
    write_all(&sandbox, &[("data/iris/notes.txt", "mine\n")])?;
    let removed = sandbox.run(&["remove", "iris"])?;
    assert_eq!(removed.status.code(), Some(0), "{}", text(&removed.stderr)?);
    assert_eq!(
        fs::read_to_string(data.join("iris/.gitignore"))?,
        "*\n!notes.txt\n"
    );
    assert!(!data.join("iris/iris.csv").exists());
    if in_git {
        assert_eq!(
            untracked(&sandbox)?.ok_or("git went away")?,
            ["data.lock", "data.toml", "data/iris/notes.txt"]
        );
    }
    Ok(())
}

/// a marker someone deleted comes back with the next sync, quietly, since the add said its line
/// once and neither the dry run nor the sync says it again. a .gitignore of the person's own where
/// a marker would go is never written over, and status names it as theirs.
#[test]
fn a_marker_comes_back_but_never_over_a_persons_file() -> Result {
    let sandbox = Sandbox::new()?;
    if crate::merge::ok(&sandbox, &["init", "-q"])?.is_none() {
        return Ok(());
    }
    let server = Routes::start(vec![
        (PATH.to_owned(), file(IRIS, "\"v1\"")),
        ("/files/wine.csv".to_owned(), file(b"a,b\n1,2\n", "\"w1\"")),
    ])?;
    sandbox.allow(&server.url);
    let added = sandbox.run(&["add", &format!("{}{PATH}", server.url)])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let marker = sandbox.work().join("data/iris/.gitignore");
    fs::remove_file(&marker)?;
    let dry = sandbox.run(&["sync", "--dry-run"])?;
    assert_eq!(
        without_copy_warning(&text(&dry.stderr)?),
        "         Ok all pinned files are in place by size and time; dry run, nothing was changed\n"
    );
    assert!(!marker.exists());
    let synced = sandbox.run(&["sync"])?;
    let stderr = text(&synced.stderr)?;
    assert!(!stderr.contains("Ignored"), "{stderr}");
    assert_eq!(crate::support::marker(&marker)?, b"*\n");

    let theirs = sandbox.work().join("data/wine/.gitignore");
    write_all(&sandbox, &[("data/wine/.gitignore", "")])?;
    let added = sandbox.run(&["add", &format!("{}/files/wine.csv", server.url)])?;
    let stderr = text(&added.stderr)?;
    assert_eq!(added.status.code(), Some(0), "{stderr}");
    assert!(!stderr.contains("Ignored"), "{stderr}");
    for args in [&["sync"][..], &["sync", "--force"]] {
        let synced = sandbox.run(args)?;
        assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
        assert!(!text(&synced.stderr)?.contains("Ignored"));
    }
    assert_eq!(fs::read(&theirs)?, b"");
    let status = json(&sandbox.run(&["--json", "status"])?)?;
    assert_eq!(
        status["result"]["yours"],
        serde_json::json!(["data/wine/.gitignore"]),
        "{status}"
    );
    Ok(())
}

/// a .gitignore of the person's own that holds just what fetchloom writes, `*`, is still theirs:
/// fetchloom never takes a file for its own by what it holds, so status names it as theirs and
/// removing the dataset leaves it, with the folder.
#[test]
fn a_persons_gitignore_like_fetchlooms_stays_theirs() -> Result {
    let sandbox = Sandbox::new()?;
    let server = iris_server()?;
    write_all(&sandbox, &[("data/iris/.gitignore", "*\n")])?;
    with_iris(&sandbox, &server)?;
    let status = json(&sandbox.run(&["--json", "status"])?)?;
    assert_eq!(
        status["result"]["yours"],
        serde_json::json!(["data/iris/.gitignore"]),
        "{status}"
    );
    let removed = sandbox.run(&["remove", "iris"])?;
    assert_eq!(removed.status.code(), Some(0), "{}", text(&removed.stderr)?);
    assert_eq!(
        fs::read(sandbox.work().join("data/iris/.gitignore"))?,
        b"*\n"
    );
    Ok(())
}

/// a dataset whose files hold a .gitignore at the top of its folder places it, where no marker
/// goes, and the run says which line keeps the folder out of git; with that line in the
/// project's .gitignore, it says nothing.
#[test]
fn a_dataset_with_its_own_gitignore_gets_the_line_to_add() -> Result {
    let sandbox = Sandbox::new()?;
    if crate::merge::ok(&sandbox, &["init", "-q"])?.is_none() {
        return Ok(());
    }
    let bytes = zip_of(&[(".gitignore", &b"*.tmp\n"[..]), ("a.csv", b"1\n")])?;
    let server = Routes::start(vec![("/files/pack.zip".to_owned(), file(&bytes, "\"p1\""))])?;
    let link = format!("{}/files/pack.zip", server.url);
    sandbox.allow(&link);
    let added = json(&sandbox.run(&["--json", "add", &link])?)?;
    assert_eq!(added["status"], "ok", "{added}");
    assert_eq!(
        crate::support::warned(&added),
        [
            "data/pack holds a .gitignore of the dataset's own, so fetchloom cannot keep data/pack out of git; to keep it out, add the line /data/pack/ to .gitignore"
        ]
    );
    assert_eq!(crate::support::kinds(&added), ["git.dataset_unignored"]);
    let pack = sandbox.work().join("data/pack");
    assert_eq!(fs::read(pack.join(".gitignore"))?, b"*.tmp\n");
    fs::write(sandbox.work().join(".gitignore"), "/data/pack/\n")?;
    crate::support::delete_folder(&pack)?;
    let synced = json(&sandbox.run(&["--json", "sync"])?)?;
    assert_eq!(synced["status"], "ok", "{synced}");
    assert_eq!(
        crate::support::kinds(&synced),
        Vec::<&str>::new(),
        "{synced}"
    );
    assert_eq!(fs::read(pack.join(".gitignore"))?, b"*.tmp\n");
    Ok(())
}

/// a link where the marker goes is the person's: never followed, read or replaced, and nothing
/// is made where it points.
#[cfg(unix)]
#[test]
fn a_link_where_the_marker_goes_is_never_followed() -> Result {
    let sandbox = Sandbox::new()?;
    let server = iris_server()?;
    fs::create_dir_all(sandbox.work().join("data/iris"))?;
    let aimed = sandbox.work().join("aimed");
    std::os::unix::fs::symlink(&aimed, sandbox.work().join("data/iris/.gitignore"))?;
    with_iris(&sandbox, &server)?;
    let link = fs::symlink_metadata(sandbox.work().join("data/iris/.gitignore"))?;
    assert!(link.file_type().is_symlink());
    assert!(!aimed.exists());
    assert_eq!(fs::read(sandbox.work().join("data/iris/iris.csv"))?, IRIS);
    Ok(())
}

/// the .gitignore fetchloom makes is readable by everyone who can read a file the person makes
/// there, so another account sharing the project folder reads it and git keeps the dataset out
/// for them too.
#[cfg(unix)]
#[test]
fn the_marker_is_readable_as_a_persons_own_files_are() -> Result {
    use std::os::unix::fs::PermissionsExt;
    let sandbox = Sandbox::new()?;
    let server = iris_server()?;
    with_iris(&sandbox, &server)?;
    let theirs = sandbox.work().join("data/iris/notes.txt");
    fs::write(&theirs, "mine\n")?;
    let mode = |path: &std::path::Path| -> Result<u32> {
        Ok(fs::metadata(path)?.permissions().mode() & 0o777)
    };
    let marker = sandbox.work().join("data/iris/.gitignore");
    assert_eq!(
        format!("{:o}", mode(&marker)?),
        format!("{:o}", mode(&theirs)?)
    );
    Ok(())
}

/// a dataset placed in the data folder itself shares it with the person's own files, so its
/// .gitignore names the dataset's own entries, anchored, rather than everything: git still sees
/// what the person writes there. the list follows the datasets' entries as they change, and a
/// folder that holds another dataset's folder is listed the same way, while a folder only a
/// dataset fills keeps `*`.
#[test]
fn a_marker_in_a_folder_shared_with_the_person_names_the_datasets_entries() -> Result {
    let sandbox = Sandbox::new()?;
    let in_git = crate::merge::ok(&sandbox, &["init", "-q"])?.is_some();
    let zip = zip_of(&[
        ("images/a.png", &b"png-a"[..]),
        ("images/b.png", b"png-b"),
        ("labels.csv", b"name,label\na,1\n"),
    ])?;
    let server = Routes::start(vec![
        (PATH.to_owned(), file(IRIS, "\"v1\"")),
        ("/files/pack.zip".to_owned(), file(&zip, "\"p1\"")),
        ("/files/inner.csv".to_owned(), file(b"x\n1\n", "\"n1\"")),
    ])?;
    sandbox.allow(&server.url);
    fs::write(
        sandbox.work().join("data.toml"),
        format!(
            "[datasets.iris]\nref = \"{0}{PATH}\"\npath = \".\"\n\n[datasets.pack]\nref = \"{0}/files/pack.zip\"\npath = \".\"\n\n[datasets.inner]\nref = \"{0}/files/inner.csv\"\npath = \"nested/inner\"\n\n[datasets.nested]\nref = \"{0}/files/inner.csv\"\npath = \"nested\"\n",
            server.url
        ),
    )?;
    write_all(&sandbox, &[("data/my_labels.csv", "mine\n")])?;
    let synced = sandbox.run(&["sync"])?;
    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(0), "{stderr}");
    let header = "# fetchloom keeps the dataset files placed here out of git, and rewrites this list as they change\n";
    let data = sandbox.work().join("data");
    assert_eq!(
        String::from_utf8(crate::support::marker(&data.join(".gitignore"))?)?,
        format!("{header}/.gitignore\n/images/\n/iris.csv\n/labels.csv\n")
    );
    assert_eq!(
        String::from_utf8(crate::support::marker(&data.join("nested/.gitignore"))?)?,
        format!("{header}/.gitignore\n/inner.csv\n")
    );
    assert_eq!(
        crate::support::marker(&data.join("nested/inner/.gitignore"))?,
        b"*\n"
    );
    if in_git {
        write_all(&sandbox, &[("data/scores.csv", "0.9\n")])?;
        assert_eq!(
            untracked(&sandbox)?.ok_or("git went away")?,
            [
                "data.lock",
                "data.toml",
                "data/my_labels.csv",
                "data/scores.csv"
            ]
        );
        assert_eq!(
            ignored_by(&sandbox, "data/images/a.png")?,
            "data/.gitignore:3:/images/\tdata/images/a.png\n"
        );
    }
    let status = sandbox.run(&["status"])?;
    assert_eq!(text(&status.stdout)?, "");

    let removed = sandbox.run(&["remove", "pack"])?;
    assert_eq!(removed.status.code(), Some(0), "{}", text(&removed.stderr)?);
    assert_eq!(
        String::from_utf8(crate::support::marker(&data.join(".gitignore"))?)?,
        format!("{header}/.gitignore\n/iris.csv\n")
    );
    let removed = sandbox.run(&["remove", "inner"])?;
    let stderr = text(&removed.stderr)?;
    assert_eq!(removed.status.code(), Some(0), "{stderr}");
    assert_eq!(
        crate::support::marker(&data.join("nested/.gitignore"))?,
        b"*\n"
    );
    if in_git {
        assert!(stderr.contains(&ignored_line("data/nested")), "{stderr}");
        // the remove said it, so the marker made again says nothing more
        fs::remove_file(data.join("nested/.gitignore"))?;
        let again = sandbox.run(&["sync"])?;
        let stderr = text(&again.stderr)?;
        assert_eq!(again.status.code(), Some(0), "{stderr}");
        assert!(!stderr.contains("Ignored"), "{stderr}");
    }
    Ok(())
}

/// a listed .gitignore is fetchloom's while it holds what fetchloom last wrote: once the person
/// adds a rule of their own, it is theirs, never rewritten or deleted.
#[test]
fn a_listed_marker_the_person_changed_is_theirs() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Routes::start(vec![
        (PATH.to_owned(), file(IRIS, "\"v1\"")),
        ("/files/wine.csv".to_owned(), file(b"a,b\n1,2\n", "\"w1\"")),
    ])?;
    sandbox.allow(&server.url);
    fs::write(
        sandbox.work().join("data.toml"),
        format!(
            "[datasets.iris]\nref = \"{0}{PATH}\"\npath = \".\"\n\n[datasets.wine]\nref = \"{0}/files/wine.csv\"\npath = \".\"\n",
            server.url
        ),
    )?;
    let synced = sandbox.run(&["sync"])?;
    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    let marker = sandbox.work().join("data/.gitignore");
    let mut theirs = fs::read_to_string(&marker)?;
    let (listed, _) = crate::support::unnamed(theirs.as_bytes());
    assert!(listed.ends_with(b"/iris.csv\n/wine.csv\n"), "{theirs}");
    theirs.push_str("/scratch/\n");
    fs::write(&marker, &theirs)?;
    for args in [&["remove", "wine"][..], &["remove", "iris"]] {
        let removed = sandbox.run(args)?;
        assert_eq!(removed.status.code(), Some(0), "{}", text(&removed.stderr)?);
        assert_eq!(fs::read_to_string(&marker)?, theirs, "{args:?}");
    }
    Ok(())
}

/// a listed .gitignore keeps naming the files of a dataset whose list this machine no longer
/// knows, as with another cache, since those files stay where they are.
#[test]
fn a_listed_marker_keeps_the_files_of_a_dataset_not_known_here() -> Result {
    let sandbox = Sandbox::new()?;
    let zip = zip_of(&[("images/a.png", &b"png-a"[..]), ("labels.csv", b"a,1\n")])?;
    let server = Routes::start(vec![
        (PATH.to_owned(), file(IRIS, "\"v1\"")),
        ("/files/wine.csv".to_owned(), file(b"a,b\n1,2\n", "\"w1\"")),
        ("/files/pack.zip".to_owned(), file(&zip, "\"p1\"")),
    ])?;
    sandbox.allow(&server.url);
    fs::write(
        sandbox.work().join("data.toml"),
        format!(
            "[datasets.iris]\nref = \"{0}{PATH}\"\npath = \".\"\n\n[datasets.wine]\nref = \"{0}/files/wine.csv\"\npath = \".\"\n\n[datasets.pack]\nref = \"{0}/files/pack.zip\"\npath = \".\"\n",
            server.url
        ),
    )?;
    let synced = sandbox.run(&["sync"])?;
    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    let cold = sandbox.folder("cold")?;
    let removed = sandbox
        .command(&["remove", "wine"])
        .env("FETCHLOOM_CACHE", &cold)
        .output()?;
    assert_eq!(removed.status.code(), Some(0), "{}", text(&removed.stderr)?);
    let marker = crate::support::marker(&sandbox.work().join("data/.gitignore"))?;
    assert!(
        marker.ends_with(b"/.gitignore\n/images/\n/iris.csv\n/labels.csv\n"),
        "{}",
        String::from_utf8_lossy(&marker)
    );
    Ok(())
}

/// what `sync` printed on stderr, which must succeed, without the copy note fat disks add.
pub(crate) fn synced(sandbox: &Sandbox) -> Result<String> {
    let output = sandbox.run(&["sync"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    Ok(without_copy_warning(&stderr))
}

/// a drop keeps the .gitignore of a dataset's folder that still holds a person's file, so git goes
/// on ignoring what it ignored, through the syncs after; it goes once the folder holds nothing.
#[test]
fn a_drop_keeps_the_marker_while_the_folder_holds_a_persons_file() -> Result {
    let sandbox = Sandbox::new()?;
    if crate::merge::ok(&sandbox, &["init", "-q"])?.is_none() {
        return Ok(());
    }
    let server = iris_server()?;
    with_iris(&sandbox, &server)?;
    let folder = sandbox.work().join("data/iris");
    fs::write(folder.join("notes.txt"), "mine\n")?;
    let dropped = sandbox.run(&["drop", "iris", "--yes"])?;
    let stderr = text(&dropped.stderr)?;
    assert_eq!(dropped.status.code(), Some(0), "{stderr}");
    let marker = crate::support::marker(&folder.join(".gitignore")).ok();
    assert_eq!(marker.as_deref(), Some(&b"*\n"[..]), "{stderr}");
    let seen = untracked(&sandbox)?.ok_or("git went away")?;
    assert_eq!(seen, ["data.lock", "data.toml"]);
    synced(&sandbox)?;
    assert_eq!(crate::support::marker(&folder.join(".gitignore"))?, b"*\n");
    fs::remove_file(folder.join("notes.txt"))?;
    synced(&sandbox)?;
    assert!(!folder.exists());
    Ok(())
}

/// a zip of `count` small csv files at its top, named f0000.csv and on.
fn many_files(count: usize) -> Result<Vec<u8>> {
    let names: Vec<String> = (0..count).map(|at| format!("f{at:04}.csv")).collect();
    let members: Vec<(&str, &[u8])> = names
        .iter()
        .map(|name| (name.as_str(), &b"1\n"[..]))
        .collect();
    zip_of(&members)
}

/// the warning for a dataset `many` placing `count` entries in the data folder it shares.
fn unnamed_warning(count: &str) -> String {
    format!(
        "warning: git sees the {count} entries many placed in data/, more than a .gitignore names one by one, and `*` would hide your own files there too\n  help: to keep them out of git, give many a folder of its own, as path = \"many\" in data.toml, where fetchloom's .gitignore needs no list; or add the line /data/ to the project's .gitignore, which hides your own files in data/ too\n"
    )
}

/// a thousand entries in a folder the person shares are named one by one. past that, git would
/// match too many lines, and `*` would hide the person's files there again, so the folder gets no
/// .gitignore of fetchloom's: the one there goes, and a sync says once which line would keep the
/// dataset out of git, or that a folder of its own needs none.
#[test]
fn a_shared_folder_too_full_to_name_gets_no_wildcard() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Routes::start(vec![
        (
            "/files/many.zip".to_owned(),
            file(&many_files(1000)?, "\"m1\""),
        ),
        (
            "/files/more.zip".to_owned(),
            file(&many_files(1001)?, "\"m2\""),
        ),
    ])?;
    sandbox.allow(&server.url);
    let manifest = sandbox.work().join("data.toml");
    let pin = |zip: &str| -> Result {
        fs::write(
            &manifest,
            format!(
                "[datasets.many]\nref = \"{}/files/{zip}\"\npath = \".\"\n",
                server.url
            ),
        )?;
        Ok(())
    };
    let marker = sandbox.work().join("data/.gitignore");
    pin("many.zip")?;
    let named = synced(&sandbox)?;
    assert!(!named.contains("git sees"), "{named}");
    let lines = fs::read_to_string(&marker)?;
    assert_eq!(
        lines.lines().count(),
        1003,
        "the header, itself, each entry and the project"
    );
    let (listed, _) = crate::support::unnamed(lines.as_bytes());
    assert!(listed.ends_with(b"/f0999.csv\n"), "{lines}");

    pin("more.zip")?;
    let unnamed = synced(&sandbox)?;
    assert!(unnamed.contains(&unnamed_warning("1,001")), "{unnamed}");
    assert!(!marker.exists(), "no wildcard, and no stale list");
    // a file put back makes the next sync place again, and still it says nothing more of git
    crate::support::delete(&sandbox.work().join("data/f0000.csv"))?;
    let again = synced(&sandbox)?;
    assert!(again.contains("put back data/f0000.csv"), "{again}");
    assert!(!again.contains("git sees"), "said once: {again}");
    assert!(!marker.exists());

    let fresh = Sandbox::new()?;
    fresh.allow(&server.url);
    fs::write(
        fresh.work().join("data.toml"),
        format!(
            "[datasets.many]\nref = \"{}/files/more.zip\"\npath = \".\"\n",
            server.url
        ),
    )?;
    crate::support::ok(&fresh.run(&["lock"])?)?;
    let dry = text(&fresh.run(&["sync", "--dry-run"])?.stderr)?;
    assert!(dry.contains(&unnamed_warning("1,001")), "{dry}");
    let first = synced(&fresh)?;
    assert!(first.contains(&unnamed_warning("1,001")), "{first}");
    assert!(!fresh.work().join("data/.gitignore").exists());
    Ok(())
}

/// n3: results/, an output's folder the person keeps in git with its tables committed, gets no
/// .gitignore at its first pin, which would hide the next table from git: the add says once that
/// git keeps it and fetchloom pins it too, and a sync or a pin again says nothing more of it. a
/// folder git holds only a .gitkeep, a .keep or a .gitignore in, at any depth, is not kept in git
/// and gets its .gitignore, and so does results/ pinned first again once git no longer tracks it.
#[test]
fn an_output_the_person_keeps_in_git_gets_no_marker() -> Result {
    let sandbox = Sandbox::new()?;
    write_all(
        &sandbox,
        &[
            ("results/table1.csv", "a,b\n1,2\n"),
            ("processed/.gitkeep", ""),
            ("figures/.keep", ""),
            ("models/sub/.gitignore", "*.tmp\n"),
        ],
    )?;
    if crate::merge::ok(&sandbox, &["init", "-q"])?.is_none() {
        return Ok(());
    }
    crate::merge::ok(&sandbox, &["add", "-A"])?;
    crate::merge::ok(&sandbox, &["commit", "-qm", "tables"])?;
    write_all(
        &sandbox,
        &[
            ("processed/clean.csv", "x\n"),
            ("figures/fig1.svg", "<svg/>\n"),
            ("models/sub/weights.bin", "w\n"),
        ],
    )?;
    let work = sandbox.work();
    let added = sandbox.run(&["add", "results", "--as", "results"])?;
    let stderr = text(&added.stderr)?;
    assert_eq!(added.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains(
            "       Kept results in git, which tracks files there; fetchloom pins it for sharing too\n"
        ),
        "{stderr}"
    );
    assert!(!stderr.contains("Ignored"), "{stderr}");
    assert!(!work.join("results/.gitignore").exists(), "{stderr}");
    assert_eq!(
        synced(&sandbox)?,
        "         Ok all pinned files are in place by size and time, nothing to do\n"
    );
    let again = sandbox.run(&["add", "results", "--as", "results"])?;
    let stderr = text(&again.stderr)?;
    assert_eq!(again.status.code(), Some(0), "{stderr}");
    assert!(!stderr.contains("in git"), "{stderr}");
    for folder in ["processed", "figures", "models"] {
        let added = sandbox.run(&["add", folder, "--as", folder])?;
        let stderr = text(&added.stderr)?;
        assert!(stderr.contains(&ignored_line(folder)), "{stderr}");
    }
    write_all(&sandbox, &[("results/table2.csv", "c,d\n3,4\n")])?;
    assert_eq!(
        untracked(&sandbox)?.ok_or("git went away")?,
        ["data.lock", "data.toml", "results/table2.csv"]
    );
    assert!(!work.join("results/.gitignore").exists());

    // once git lets go of the folder, its next first pin gives it a .gitignore
    ok(&sandbox.run(&["remove", "results", "--yes"])?)?;
    crate::merge::ok(&sandbox, &["rm", "-r", "-q", "--cached", "results"])?;
    let added = sandbox.run(&["add", "results", "--as", "results"])?;
    let stderr = text(&added.stderr)?;
    assert!(stderr.contains(&ignored_line("results")), "{stderr}");
    assert_eq!(
        crate::support::marker(&work.join("results/.gitignore"))?,
        b"*\n"
    );
    Ok(())
}

/// a teammate's clone of an output's folder git tracks gets no .gitignore either: the sync that
/// would make it asks git first, as the author's pin did, and says once that git keeps the
/// folder, so the teammate's next table reaches git. an output whose files wait for the
/// teammate's own pipeline run is asked the same before its folder is made ready.
#[test]
fn a_clone_asks_git_before_it_marks_an_outputs_folder() -> Result {
    let sandbox = Sandbox::new()?;
    if crate::merge::ok(&sandbox, &["init", "-q"])?.is_none() {
        return Ok(());
    }
    write_all(
        &sandbox,
        &[
            ("results/table1.csv", "a,b\n1,2\n"),
            ("features/README.md", "made by make features\n"),
        ],
    )?;
    crate::merge::ok(&sandbox, &["add", "-A"])?;
    crate::merge::ok(&sandbox, &["commit", "-qm", "tables"])?;
    write_all(&sandbox, &[("features/train.csv", "x,y\n0,1\n")])?;
    for output in ["results", "features"] {
        ok(&sandbox.run(&["add", output, "--as", output])?)?;
    }
    crate::merge::ok(&sandbox, &["add", "data.toml", "data.lock"])?;
    crate::merge::ok(&sandbox, &["commit", "-qm", "pinned"])?;
    let teammate = sandbox.folder("teammate")?;
    let into = teammate.display().to_string();
    let cloned = crate::merge::git(&sandbox, &sandbox.work(), &["clone", "-q", ".", &into])?
        .ok_or("git went away")?;
    assert!(cloned.status.success(), "{}", text(&cloned.stderr)?);
    let cache = sandbox.folder("teammate-cache")?;
    let sync = || {
        sandbox
            .command_in(&teammate, &["sync"])
            .env("FETCHLOOM_CACHE", &cache)
            .output()
    };
    let placed = sync()?;
    let stderr = text(&placed.stderr)?;
    assert_eq!(placed.status.code(), Some(0), "{stderr}");
    for output in ["results", "features"] {
        assert!(
            stderr.contains(&format!(
                "       Kept {output} in git, which tracks files there; fetchloom pins it for sharing too\n"
            )),
            "{stderr}"
        );
        assert!(
            !teammate.join(output).join(".gitignore").exists(),
            "{stderr}"
        );
    }
    assert!(!stderr.contains("Ignored"), "{stderr}");
    let again = text(&sync()?.stderr)?;
    assert!(!again.contains("in git"), "{again}");
    fs::write(teammate.join("results/table2.csv"), "c,d\n3,4\n")?;
    let status = crate::merge::git(&sandbox, &teammate, &["status", "--porcelain", "-uall"])?
        .ok_or("git went away")?;
    let seen = text(&status.stdout)?;
    assert!(seen.contains("?? results/table2.csv\n"), "{seen}");
    Ok(())
}

/// m4: a teammate's clone holds git's copy of an output's file the author pinned again without
/// committing it: status names it as git's copy, not as a file nothing tracks.
#[test]
fn status_names_gits_copy_of_an_outputs_file() -> Result {
    let sandbox = Sandbox::new()?;
    if crate::merge::ok(&sandbox, &["init", "-q"])?.is_none() {
        return Ok(());
    }
    write_all(&sandbox, &[("results/table1.csv", "a,b\n1,2\n")])?;
    crate::merge::ok(&sandbox, &["add", "-A"])?;
    crate::merge::ok(&sandbox, &["commit", "-qm", "tables"])?;
    ok(&sandbox.run(&["add", "results", "--as", "results"])?)?;
    write_all(&sandbox, &[("results/table1.csv", "a,b\n1,2\n3,4\n")])?;
    ok(&sandbox.run(&["add", "results", "--as", "results", "--yes"])?)?;
    crate::merge::ok(&sandbox, &["add", "data.toml", "data.lock"])?;
    crate::merge::ok(&sandbox, &["commit", "-qm", "pinned"])?;
    let teammate = sandbox.folder("teammate")?;
    let into = teammate.display().to_string();
    let cloned = crate::merge::git(&sandbox, &sandbox.work(), &["clone", "-q", ".", &into])?
        .ok_or("git went away")?;
    assert!(cloned.status.success(), "{}", text(&cloned.stderr)?);
    let status = sandbox.command_in(&teammate, &["status"]).output()?;
    assert_eq!(
        text(&status.stdout)?,
        "  Committed results/table1.csv · git's copy holds other bytes than data.lock pins\n",
        "{}",
        text(&status.stderr)?
    );
    // once git lets go of it, it is a file nothing tracks
    let args = ["rm", "-q", "--cached", "results/table1.csv"];
    let untracked = crate::merge::git(&sandbox, &teammate, &args)?.ok_or("git went away")?;
    assert!(untracked.status.success(), "{}", text(&untracked.stderr)?);
    let status = sandbox.command_in(&teammate, &["status"]).output()?;
    assert_eq!(
        text(&status.stdout)?,
        "  Untracked results/table1.csv · holds other bytes than data.lock pins\n"
    );
    Ok(())
}

/// n8: a pipeline that deletes its output's folder before it writes it again, as `rm -rf` before
/// a rerun does, gets the folder's .gitignore back from the next sync, as it was, with no line:
/// the add said it once for the folder, and neither the sync nor its dry run says it again.
#[test]
fn the_marker_line_is_said_once_per_folder() -> Result {
    let sandbox = Sandbox::new()?;
    if crate::merge::ok(&sandbox, &["init", "-q"])?.is_none() {
        return Ok(());
    }
    write_all(&sandbox, &[("results/table.csv", "a,b\n1,2\n")])?;
    let added = sandbox.run(&["add", "results", "--as", "results"])?;
    let stderr = text(&added.stderr)?;
    assert_eq!(added.status.code(), Some(0), "{stderr}");
    assert!(stderr.contains(&ignored_line("results")), "{stderr}");
    let folder = sandbox.work().join("results");
    for rerun in 1..=2 {
        crate::support::delete_folder(&folder)?;
        write_all(&sandbox, &[("results/table.csv", "a,b\n1,2\n")])?;
        let dry = text(&sandbox.run(&["sync", "--dry-run"])?.stderr)?;
        assert!(!dry.contains("in git"), "{rerun}: {dry}");
        let stderr = synced(&sandbox)?;
        assert!(!stderr.contains("in git"), "{rerun}: {stderr}");
        assert_eq!(
            crate::support::marker(&folder.join(".gitignore"))?,
            b"*\n",
            "{rerun}"
        );
    }
    Ok(())
}

/// a folder whose marker changes kind is told again each time, since the person's files there go
/// from hidden to seen by git or back: data/iris holds `*` until a dataset is placed inside it,
/// when its marker names iris's entries, and `*` again once that dataset goes.
#[test]
fn a_marker_that_changes_kind_is_told_again() -> Result {
    let sandbox = Sandbox::new()?;
    if crate::merge::ok(&sandbox, &["init", "-q"])?.is_none() {
        return Ok(());
    }
    let server = Routes::start(vec![
        (PATH.to_owned(), file(IRIS, "\"v1\"")),
        ("/files/masks.csv".to_owned(), file(b"m\n1\n", "\"m1\"")),
    ])?;
    sandbox.allow(&server.url);
    let link = format!("{}{PATH}", server.url);
    let added = sandbox.run(&["add", &link])?;
    let stderr = text(&added.stderr)?;
    assert!(stderr.contains(&ignored_line("data/iris")), "{stderr}");
    let manifest = sandbox.work().join("data.toml");
    let alone = fs::read_to_string(&manifest)?;
    fs::write(
        &manifest,
        format!(
            "{alone}\n[datasets.masks]\nref = \"{}/files/masks.csv\"\npath = \"iris/masks\"\n",
            server.url
        ),
    )?;
    let stderr = synced(&sandbox)?;
    assert!(
        stderr.contains(
            "    Ignored the dataset files in data/iris in git with data/iris/.gitignore, which names them, so files you put there stay in git\n"
        ),
        "{stderr}"
    );
    fs::write(&manifest, alone)?;
    let stderr = synced(&sandbox)?;
    assert!(stderr.contains(&ignored_line("data/iris")), "{stderr}");
    Ok(())
}

/// the line an add prints for an output's folder it leaves to git, which tracks files there.
fn kept_line(folder: &str) -> String {
    format!(
        "       Kept {folder} in git, which tracks files there; fetchloom pins it for sharing too\n"
    )
}

/// m3: each pin reads again whether git keeps an output's folder. figures/, where git held only a
/// .gitkeep at the first pin, gets its .gitignore then; once the person takes it out and commits a
/// figure there, the next pin leaves the folder to git and says so once, so a new figure shows in
/// git. when git lets go of the figure, a pin gives the folder its .gitignore back, said again,
/// and the syncs after keep it: a sync asks git only before it makes one.
#[test]
fn every_pin_reads_whether_git_keeps_an_outputs_folder() -> Result {
    let sandbox = Sandbox::new()?;
    write_all(
        &sandbox,
        &[("figures/.gitkeep", ""), ("figures/fig1.png", "png 1\n")],
    )?;
    if crate::merge::ok(&sandbox, &["init", "-q"])?.is_none() {
        return Ok(());
    }
    crate::merge::ok(&sandbox, &["add", "figures/.gitkeep"])?;
    crate::merge::ok(&sandbox, &["commit", "-qm", "folders"])?;
    let marker = sandbox.work().join("figures/.gitignore");
    // each pin says one of the two lines, as git keeps the folder or not
    let pin = |kept: bool| -> Result {
        let pinned = sandbox.run(&["add", "figures", "--as", "figures", "--yes"])?;
        let stderr = text(&pinned.stderr)?;
        assert_eq!(pinned.status.code(), Some(0), "{stderr}");
        assert_eq!(stderr.contains(&kept_line("figures")), kept, "{stderr}");
        assert_eq!(stderr.contains(&ignored_line("figures")), !kept, "{stderr}");
        Ok(())
    };
    pin(false)?;
    assert_eq!(crate::support::marker(&marker)?, b"*\n");
    fs::remove_file(&marker)?;
    crate::merge::ok(&sandbox, &["add", "figures/fig1.png"])?;
    crate::merge::ok(&sandbox, &["commit", "-qm", "a figure"])?;
    write_all(&sandbox, &[("figures/fig2.png", "png 2\n")])?;
    pin(true)?;
    assert!(!marker.exists());
    assert_eq!(
        untracked(&sandbox)?.ok_or("git went away")?,
        ["data.lock", "data.toml", "figures/fig2.png"]
    );
    let again = sandbox.run(&["add", "figures", "--as", "figures"])?;
    let stderr = text(&again.stderr)?;
    assert_eq!(again.status.code(), Some(0), "{stderr}");
    assert!(!stderr.contains("in git"), "said once: {stderr}");
    crate::merge::ok(&sandbox, &["rm", "-q", "--cached", "figures/fig1.png"])?;
    pin(false)?;
    assert_eq!(crate::support::marker(&marker)?, b"*\n");
    // a marker that goes comes back while git tracks none of the person's files there
    fs::remove_file(&marker)?;
    synced(&sandbox)?;
    let back = crate::support::marker(&marker).ok();
    assert_eq!(back.as_deref(), Some(&b"*\n"[..]));
    // one that stands is the pin's to change, though a sync reads the folder's files again
    crate::merge::ok(&sandbox, &["add", "-f", "figures/fig2.png"])?;
    crate::merge::ok(&sandbox, &["commit", "-qm", "a forced figure"])?;
    write_all(&sandbox, &[("figures/fig2.png", "png 2\n")])?;
    synced(&sandbox)?;
    assert_eq!(crate::support::marker(&marker)?, b"*\n");
    Ok(())
}

/// a remove that turns a folder's marker back to the kind the person was last told of says
/// nothing again: data/nested was told of in git as hidden, became a list while the project was
/// out of git, as between `rm -rf .git` and a new `git init`, where no line is said, and turns
/// back once inner goes.
#[test]
fn a_remove_says_no_marker_line_the_person_heard() -> Result {
    let sandbox = Sandbox::new()?;
    if crate::merge::ok(&sandbox, &["init", "-q"])?.is_none() {
        return Ok(());
    }
    let server = Routes::start(vec![
        ("/files/nested.csv".to_owned(), file(b"n\n1\n", "\"n1\"")),
        ("/files/inner.csv".to_owned(), file(b"i\n1\n", "\"i1\"")),
    ])?;
    sandbox.allow(&server.url);
    let manifest = sandbox.work().join("data.toml");
    let nested = format!(
        "[datasets.nested]\nref = \"{}/files/nested.csv\"\npath = \"nested\"\n",
        server.url
    );
    fs::write(&manifest, &nested)?;
    let stderr = synced(&sandbox)?;
    assert!(stderr.contains(&ignored_line("data/nested")), "{stderr}");
    crate::support::delete_folder(&sandbox.work().join(".git"))?;
    fs::write(
        &manifest,
        format!(
            "{nested}\n[datasets.inner]\nref = \"{}/files/inner.csv\"\npath = \"nested/inner\"\n",
            server.url
        ),
    )?;
    synced(&sandbox)?;
    crate::merge::ok(&sandbox, &["init", "-q"])?;
    let removed = sandbox.run(&["remove", "inner"])?;
    let stderr = text(&removed.stderr)?;
    assert_eq!(removed.status.code(), Some(0), "{stderr}");
    assert!(!stderr.contains("Ignored"), "{stderr}");
    let marker = sandbox.work().join("data/nested/.gitignore");
    assert_eq!(crate::support::marker(&marker)?, b"*\n");
    Ok(())
}

/// iris added, then taken out of the project as `leave` does, and added again: the second add
/// tells data/iris's marker line again, since the folder had no dataset meanwhile.
fn told_again_after(leave: impl Fn(&Sandbox) -> Result) -> Result {
    let sandbox = Sandbox::new()?;
    if crate::merge::ok(&sandbox, &["init", "-q"])?.is_none() {
        return Ok(());
    }
    let server = iris_server()?;
    let link = format!("{}{PATH}", server.url);
    sandbox.allow(&link);
    let added = sandbox.run(&["add", &link])?;
    let stderr = text(&added.stderr)?;
    assert!(stderr.contains(&ignored_line("data/iris")), "{stderr}");
    leave(&sandbox)?;
    let again = sandbox.run(&["add", &link])?;
    let stderr = text(&again.stderr)?;
    assert_eq!(again.status.code(), Some(0), "{stderr}");
    assert!(stderr.contains(&ignored_line("data/iris")), "{stderr}");
    Ok(())
}

/// a folder no dataset has any more forgets its marker line by the remove that leaves it so.
#[test]
fn a_folder_its_dataset_left_hears_its_marker_line_again() -> Result {
    told_again_after(|sandbox| ok(&sandbox.run(&["remove", "iris"])?))
}

/// a sync after the remove, which finds nothing to place, leaves the folder forgotten too.
#[test]
fn a_sync_between_keeps_the_left_folder_forgotten() -> Result {
    told_again_after(|sandbox| {
        ok(&sandbox.run(&["remove", "iris"])?)?;
        ok(&sandbox.run(&["sync"])?)
    })
}

/// a dataset taken out of data.toml by hand leaves its folder forgotten by the sync that removes
/// its files.
#[test]
fn a_sync_forgets_the_folder_of_a_dataset_data_toml_dropped() -> Result {
    told_again_after(|sandbox| {
        fs::write(sandbox.work().join("data.toml"), "")?;
        ok(&sandbox.run(&["sync"])?)
    })
}
