//! a data folder a person linked onto another disk, as clusters keep code in a small home and
//! data on scratch: followed only into a folder this machine's `data_links` setting lists.

use std::fs;
use std::path::{Path, PathBuf};

use crate::support::{Result, Routes, Sandbox, file, link_folder, ok, text};

const IRIS: &[u8] = b"sepal_length,species\n5.1,setosa\n";

/// `path` as fetchloom names a folder it followed a link to: resolved, and on windows without the
/// `\\?\` a resolved path starts with.
fn real(path: &Path) -> Result<String> {
    let found = fs::canonicalize(path)?.display().to_string();
    Ok(match found.strip_prefix(r"\\?\") {
        Some(rest) if cfg!(windows) => rest.to_owned(),
        _ => found,
    })
}

/// whether `text` holds each of `parts`, in order.
fn in_order(text: &str, parts: &[&str]) -> bool {
    let mut rest = text;
    parts.iter().all(|part| match rest.find(part) {
        Some(at) => {
            rest = &rest[at + part.len()..];
            true
        }
        None => false,
    })
}

/// takes away the link at `link`, never what it leads to: a junction goes as a folder does.
fn unlink(link: &Path) -> Result {
    fs::remove_file(link).or_else(|_| fs::remove_dir(link))?;
    Ok(())
}

/// every link below `root`, by its path, without entering one.
fn links(root: &Path) -> Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    for entry in fs::read_dir(root)? {
        let path = entry?.path();
        let kind = fs::symlink_metadata(&path)?.file_type();
        if kind.is_symlink() {
            found.push(path);
        } else if kind.is_dir() {
            found.extend(links(&path)?);
        }
    }
    Ok(found)
}

/// a link a person made from the project's data folder into a folder the `data_links` setting lists
/// is followed: the files land on the other disk, and status, remove and drop work there. the link
/// stays the person's, fetchloom makes none of its own, and the project's .gitignore is left as it
/// was.
#[test]
fn a_data_folder_linked_into_a_listed_folder_is_followed() -> Result {
    let sandbox = Sandbox::new()?;
    let scratch = sandbox.folder("scratch")?;
    let target = scratch.join("project-data");
    fs::create_dir(&target)?;
    let data = sandbox.work().join("data");
    if link_folder(&target, &data).is_err() {
        return crate::support::skipped("symlinks", "no symbolic link can be made here");
    }
    ok(&sandbox.run(&[
        "settings",
        "set",
        "data_links",
        &scratch.display().to_string(),
    ])?)?;
    let routes = Routes::start(vec![("/iris.csv".to_owned(), file(IRIS, "\"1\""))])?;
    sandbox.allow(&routes.url);
    let link = format!("{}/iris.csv", routes.url);
    let said = |args: &[&str]| -> Result<String> {
        let output = sandbox.run(args)?;
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(0), "{args:?}: {stderr}");
        Ok(stderr)
    };
    said(&["add", &link, "--as", "iris"])?;
    assert_eq!(fs::read(target.join("iris").join("iris.csv"))?, IRIS);
    let status = said(&["--offline", "status"])?;
    assert!(
        status.contains("Ok everything matches data.lock"),
        "{status}"
    );
    // an empty variable clears the list the settings file keeps, for one run
    let cleared = sandbox
        .command(&["--offline", "status"])
        .env("FETCHLOOM_DATA_LINKS", "")
        .output()?;
    assert_eq!(
        cleared.status.code(),
        Some(50),
        "{}",
        text(&cleared.stderr)?
    );
    said(&["--yes", "drop", "iris"])?;
    assert!(!target.join("iris").join("iris.csv").exists());
    said(&["sync", "iris"])?;
    assert_eq!(fs::read(target.join("iris").join("iris.csv"))?, IRIS);
    said(&["--yes", "remove", "iris"])?;
    assert!(!target.join("iris").exists());
    assert!(target.is_dir());
    assert!(fs::symlink_metadata(&data)?.file_type().is_symlink());
    assert_eq!(links(&sandbox.work())?, std::slice::from_ref(&data));
    assert_eq!(links(&scratch)?, Vec::<PathBuf>::new());
    assert!(!sandbox.work().join(".gitignore").exists());
    Ok(())
}

/// a link a cloned project brings can aim anywhere, so a data folder link is followed only into a
/// listed folder. one to the home folder or its autostart folder, one into a folder whose name only
/// begins as the listed one's does, one whose path climbs out of the listed folder with `..`, one
/// through a link inside the listed folder that leads out, and one to a folder that is gone are
/// refused before anything is written there; the refusal names the setting with the command that
/// lists the folder, and a folder a link leads to is never made.
#[test]
fn a_data_folder_link_anywhere_else_is_refused() -> Result {
    let sandbox = Sandbox::new()?;
    let scratch = sandbox.folder("scratch")?;
    let home = sandbox.folder("home")?;
    let autostart = home.join(".config").join("autostart");
    fs::create_dir_all(&autostart)?;
    let outside = sandbox.folder("outside")?;
    let inner = scratch.join("inner");
    if link_folder(&outside, &inner).is_err() {
        return crate::support::skipped("symlinks", "no symbolic link can be made here");
    }
    ok(&sandbox.run(&[
        "settings",
        "set",
        "data_links",
        &scratch.display().to_string(),
    ])?)?;
    let routes = Routes::start(vec![("/iris.csv".to_owned(), file(IRIS, "\"1\""))])?;
    sandbox.allow(&routes.url);
    let link = format!("{}/iris.csv", routes.url);
    let data = sandbox.work().join("data");
    let listed = scratch.display().to_string();
    for target in [
        home.clone(),
        autostart.clone(),
        sandbox.folder("scratch2")?,
        scratch.join("..").join("outside"),
        inner.clone(),
    ] {
        link_folder(&target, &data)?;
        let before = fs::read_dir(&target)?.count();
        let output = sandbox.run(&["add", &link, "--as", "iris"])?;
        let stderr = text(&output.stderr)?;
        assert_eq!(
            output.status.code(),
            Some(50),
            "{}: {stderr}",
            target.display()
        );
        let leads = real(&target)?;
        assert!(
            stderr.contains(&format!(
                " is a link to {leads}, which no folder in data_links holds, so fetchloom places nothing through it\n"
            )) && in_order(
                &stderr,
                &[
                    "check where it leads, since a cloned project's link can aim anywhere; if you made it to keep data on another disk, list that folder with `fetchloom settings set data_links ",
                    &listed,
                    &leads,
                    "`\n"
                ]
            ),
            "{}: {stderr}",
            target.display()
        );
        assert_eq!(
            fs::read_dir(&target)?.count(),
            before,
            "{}",
            target.display()
        );
        assert!(!sandbox.work().join("data.toml").exists(), "{stderr}");
        unlink(&data)?;
    }
    // a folder a link leads to that is gone may be on a disk not mounted now
    let gone = scratch.join("gone");
    link_folder(&gone, &data)?;
    let output = sandbox.run(&["add", &link, "--as", "iris"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(50), "{stderr}");
    assert!(
        stderr.contains(" which does not exist: its disk may be unmounted\n"),
        "{stderr}"
    );
    assert!(!gone.exists(), "{stderr}");
    unlink(&data)?;
    // the variable, when it lists the folders, is what the help names
    link_folder(&home, &data)?;
    let output = sandbox
        .command(&["add", &link, "--as", "iris"])
        .env("FETCHLOOM_DATA_LINKS", &scratch)
        .output()?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(50), "{stderr}");
    assert!(
        stderr.contains(&format!(
            "list that folder in FETCHLOOM_DATA_LINKS too, as `{listed},{}`",
            real(&home)?
        )),
        "{stderr}"
    );
    unlink(&data)?;
    Ok(())
}

/// a listed folder counts as resolved, so a list naming scratch through a link to it, as a
/// cluster's /scratch leads to /lustre/scratch, still holds a data folder linked into it.
#[test]
fn a_folder_listed_through_a_link_holds_what_lies_in_it() -> Result {
    let sandbox = Sandbox::new()?;
    let scratch = sandbox.folder("scratch")?;
    let target = scratch.join("project-data");
    fs::create_dir(&target)?;
    let via = scratch.with_file_name("via");
    if link_folder(&scratch, &via).is_err() {
        return crate::support::skipped("symlinks", "no symbolic link can be made here");
    }
    link_folder(&target, &sandbox.work().join("data"))?;
    let routes = Routes::start(vec![("/iris.csv".to_owned(), file(IRIS, "\"1\""))])?;
    sandbox.allow(&routes.url);
    let link = format!("{}/iris.csv", routes.url);
    let output = sandbox
        .command(&["add", &link, "--as", "iris"])
        .env("FETCHLOOM_DATA_LINKS", &via)
        .output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    assert_eq!(fs::read(target.join("iris").join("iris.csv"))?, IRIS);
    Ok(())
}

/// fetchloom follows the one link to the data folder: a second on the way to it, or a link in it
/// where a dataset's folder belongs, stays refused, and the cache may not sit in the folder the
/// link leads to.
#[test]
fn only_the_one_link_to_the_data_folder_is_followed() -> Result {
    let sandbox = Sandbox::new()?;
    let scratch = sandbox.folder("scratch")?;
    let target = scratch.join("d");
    let other = scratch.join("r");
    fs::create_dir_all(&target)?;
    fs::create_dir_all(&other)?;
    let data = sandbox.work().join("data");
    if link_folder(&target, &data).is_err() {
        return crate::support::skipped("symlinks", "no symbolic link can be made here");
    }
    ok(&sandbox.run(&[
        "settings",
        "set",
        "data_links",
        &scratch.display().to_string(),
    ])?)?;
    let routes = Routes::start(vec![("/iris.csv".to_owned(), file(IRIS, "\"1\""))])?;
    sandbox.allow(&routes.url);
    let link = format!("{}/iris.csv", routes.url);
    let manifest = sandbox.work().join("data.toml");
    fs::write(&manifest, "[project]\ndata_dir = \"data/raw\"\n")?;
    link_folder(&other, &target.join("raw"))?;
    let output = sandbox.run(&["add", &link, "--as", "iris"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(50), "{stderr}");
    assert!(
        stderr.contains(
            "is a second link on the way to the data folder, and fetchloom follows only the one"
        ),
        "{stderr}"
    );
    assert_eq!(fs::read_dir(&other)?.count(), 0);
    unlink(&target.join("raw"))?;
    // once the data folder is this project's, a dataset's folder that is a link stays where it
    // leads
    fs::write(&manifest, "")?;
    ok(&sandbox.run(&["sync"])?)?;
    fs::remove_file(&manifest)?;
    let elsewhere = sandbox.folder("elsewhere")?;
    link_folder(&elsewhere, &target.join("iris"))?;
    let output = sandbox.run(&["add", &link, "--as", "iris"])?;
    let stderr = text(&output.stderr)?;
    assert!(
        stderr.contains("is a link where data.lock needs a folder"),
        "{stderr}"
    );
    assert_eq!(fs::read_dir(&elsewhere)?.count(), 0);
    unlink(&target.join("iris"))?;
    // the cache inside the folder the link leads to would take a dataset's files for its own
    let output = sandbox
        .command(&["--offline", "status"])
        .env("FETCHLOOM_CACHE", target.join("cache"))
        .output()?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(50), "{stderr}");
    assert!(
        stderr.contains("is inside the data folder data"),
        "{stderr}"
    );
    Ok(())
}

/// data.toml and data.lock pinning the file at `link` as iris, written into the work folder as a
/// clone of a project brings them: no state, and nothing placed.
fn cloned(sandbox: &Sandbox, link: &str) -> Result {
    let origin = sandbox.folder("origin")?;
    let added = sandbox
        .command_in(&origin, &["add", link, "--as", "iris"])
        .output()?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    for name in ["data.toml", "data.lock"] {
        fs::copy(origin.join(name), sandbox.work().join(name))?;
    }
    Ok(())
}

/// the folder the state of the project at `root` records its data folder link `link` leading to.
fn recorded(root: &Path, link: &str) -> Result<Option<String>> {
    let state = fs::read(root.join(".fetchloom").join("state"))?;
    let state: serde_json::Value = serde_json::from_slice(&state)?;
    Ok(state["followed"][link].as_str().map(str::to_owned))
}

/// `path` resolved, as a state records a folder a link leads to.
fn resolved(path: &Path) -> Result<String> {
    Ok(fs::canonicalize(path)?.to_string_lossy().into_owned())
}

/// every file and folder below `root`, by its path there.
fn listing(root: &Path) -> Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    let mut folders = vec![root.to_path_buf()];
    while let Some(folder) = folders.pop() {
        for entry in fs::read_dir(folder)? {
            let path = entry?.path();
            if fs::symlink_metadata(&path)?.is_dir() {
                folders.push(path.clone());
            }
            found.push(path.strip_prefix(root)?.to_path_buf());
        }
    }
    found.sort();
    Ok(found)
}

/// a cloned project's committed `data` link may lead into another project's data folder under a
/// listed root, as /scratch/$USER holds each of a person's projects. its first follow takes no
/// folder holding files this project did not place, nor one inside another project's folder, so
/// neither a sync nor the `sync --force` that replaces files writes anything there.
#[test]
fn a_cloned_link_into_another_projects_data_folder_is_refused() -> Result {
    let sandbox = Sandbox::new()?;
    if crate::merge::ok(&sandbox, &["init", "-q"])?.is_none() {
        return Ok(());
    }
    let root = sandbox.folder("scratch-root")?;
    let thesis = root.join("thesis").join("data");
    fs::create_dir_all(thesis.join("iris"))?;
    fs::write(thesis.join("iris").join("iris.csv"), b"my own table\n")?;
    let data = sandbox.work().join("data");
    if link_folder(&thesis, &data).is_err() {
        return crate::support::skipped("symlinks", "no symbolic link can be made here");
    }
    let routes = Routes::start(vec![("/iris.csv".to_owned(), file(IRIS, "\"1\""))])?;
    sandbox.allow(&routes.url);
    cloned(&sandbox, &format!("{}/iris.csv", routes.url))?;
    ok(&sandbox.run(&["settings", "set", "data_links", &root.display().to_string()])?)?;
    let before = listing(&thesis)?;
    for args in [&["sync"][..], &["sync", "--force"]] {
        let output = sandbox.run(args)?;
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(50), "{args:?}: {stderr}");
        assert!(
            stderr.contains(&format!(
                " is a link to {}, which holds files this project did not place, such as iris/iris.csv, so fetchloom places nothing through it\n",
                real(&thesis)?
            )),
            "{args:?}: {stderr}"
        );
        assert_eq!(listing(&thesis)?, before, "{args:?}");
        assert_eq!(
            fs::read(thesis.join("iris").join("iris.csv"))?,
            b"my own table\n"
        );
    }
    // a folder inside another project's folder is that project's, though it is empty yet and lies
    // deeper there than the project's own data folder
    let notes = root.join("notes");
    let owner = root.join("thesis");
    for (project, inside) in [
        (notes.clone(), notes.join("data")),
        (owner.clone(), owner.join("results").join("data")),
    ] {
        fs::create_dir_all(&inside)?;
        fs::write(project.join("data.toml"), b"")?;
        unlink(&data)?;
        link_folder(&inside, &data)?;
        let output = sandbox.run(&["sync"])?;
        let stderr = text(&output.stderr)?;
        assert_eq!(
            output.status.code(),
            Some(50),
            "{}: {stderr}",
            inside.display()
        );
        assert!(
            stderr.contains(&format!(
                " is a link to {}, which lies in the project at {}, so fetchloom places nothing through it\n",
                real(&inside)?,
                real(&project)?
            )),
            "{stderr}"
        );
        assert_eq!(listing(&inside)?, Vec::<PathBuf>::new());
    }
    Ok(())
}

/// a person who moves their own data folder to scratch and links it back keeps working: the folder
/// holds what this project placed, the markers it made and what a system writes by itself, so the
/// first follow takes it and the state records it.
#[test]
fn a_data_folder_moved_to_scratch_with_its_state_is_followed() -> Result {
    let sandbox = Sandbox::new()?;
    if crate::merge::ok(&sandbox, &["init", "-q"])?.is_none() {
        return Ok(());
    }
    let scratch = sandbox.folder("scratch")?;
    let probe = sandbox.work().join("probe");
    if link_folder(&scratch, &probe).is_err() {
        return crate::support::skipped("symlinks", "no symbolic link can be made here");
    }
    unlink(&probe)?;
    let routes = Routes::start(vec![("/iris.csv".to_owned(), file(IRIS, "\"1\""))])?;
    sandbox.allow(&routes.url);
    ok(&sandbox.run(&["add", &format!("{}/iris.csv", routes.url), "--as", "iris"])?)?;
    let data = sandbox.work().join("data");
    let moved = scratch.join("project-data");
    fs::rename(&data, &moved)?;
    fs::write(moved.join("iris").join(".DS_Store"), b"finder\n")?;
    link_folder(&moved, &data)?;
    ok(&sandbox.run(&[
        "settings",
        "set",
        "data_links",
        &scratch.display().to_string(),
    ])?)?;
    for args in [&["--offline", "sync"][..], &["--offline", "status"]] {
        let output = sandbox.run(args)?;
        assert_eq!(
            output.status.code(),
            Some(0),
            "{args:?}: {}",
            text(&output.stderr)?
        );
    }
    assert_eq!(fs::read(moved.join("iris").join("iris.csv"))?, IRIS);
    assert_eq!(recorded(&sandbox.work(), "data")?, Some(resolved(&moved)?));
    Ok(())
}

/// an empty folder a link leads to is this project's from its first follow on: the state records
/// it, so a file the person puts there later stops no run.
#[test]
fn an_empty_folder_a_link_leads_to_is_followed_and_recorded() -> Result {
    let sandbox = Sandbox::new()?;
    let scratch = sandbox.folder("scratch")?;
    let target = scratch.join("project-data");
    fs::create_dir(&target)?;
    if link_folder(&target, &sandbox.work().join("data")).is_err() {
        return crate::support::skipped("symlinks", "no symbolic link can be made here");
    }
    ok(&sandbox.run(&[
        "settings",
        "set",
        "data_links",
        &scratch.display().to_string(),
    ])?)?;
    let routes = Routes::start(vec![("/iris.csv".to_owned(), file(IRIS, "\"1\""))])?;
    sandbox.allow(&routes.url);
    ok(&sandbox.run(&["add", &format!("{}/iris.csv", routes.url), "--as", "iris"])?)?;
    assert_eq!(fs::read(target.join("iris").join("iris.csv"))?, IRIS);
    assert_eq!(recorded(&sandbox.work(), "data")?, Some(resolved(&target)?));
    fs::write(target.join("notes.txt"), b"mine\n")?;
    let synced = sandbox.run(&["--offline", "sync"])?;
    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    Ok(())
}

/// a recorded link pointed at another folder is followed only as at first: one holding files this
/// project did not place is refused and left as it was, and the folder recorded is still followed.
#[test]
fn a_recorded_link_pointed_at_a_populated_folder_is_refused() -> Result {
    let sandbox = Sandbox::new()?;
    let scratch = sandbox.folder("scratch")?;
    let target = scratch.join("project-data");
    fs::create_dir(&target)?;
    let data = sandbox.work().join("data");
    if link_folder(&target, &data).is_err() {
        return crate::support::skipped("symlinks", "no symbolic link can be made here");
    }
    ok(&sandbox.run(&[
        "settings",
        "set",
        "data_links",
        &scratch.display().to_string(),
    ])?)?;
    let routes = Routes::start(vec![("/iris.csv".to_owned(), file(IRIS, "\"1\""))])?;
    sandbox.allow(&routes.url);
    ok(&sandbox.run(&["add", &format!("{}/iris.csv", routes.url), "--as", "iris"])?)?;
    let other = scratch.join("someone");
    fs::create_dir_all(other.join("iris"))?;
    fs::write(other.join("iris").join("iris.csv"), b"theirs\n")?;
    unlink(&data)?;
    link_folder(&other, &data)?;
    let before = listing(&other)?;
    for args in [
        &["--offline", "sync"][..],
        &["--offline", "sync", "--force"],
    ] {
        let output = sandbox.run(args)?;
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(50), "{args:?}: {stderr}");
        assert!(
            stderr.contains("which holds files this project did not place, such as iris/iris.csv"),
            "{args:?}: {stderr}"
        );
        assert_eq!(listing(&other)?, before, "{args:?}");
    }
    unlink(&data)?;
    link_folder(&target, &data)?;
    let back = sandbox.run(&["--offline", "sync"])?;
    assert_eq!(back.status.code(), Some(0), "{}", text(&back.stderr)?);
    Ok(())
}

/// git sees a data folder link itself, so a commit takes it and each clone then refuses it, and a
/// line ending in /, as cookiecutter's /data/, hides folders alone. a person at a terminal is told
/// once of the line that hides the link, /data; with that line there, nothing.
#[cfg(target_os = "linux")]
#[test]
fn a_followed_link_git_sees_is_told_once() -> Result {
    let told_line = "add the line /data to the project's .gitignore";
    for (rules, told) in [("/data/\n", true), ("/data\n", false)] {
        let sandbox = Sandbox::new()?;
        if crate::merge::ok(&sandbox, &["init", "-q"])?.is_none() {
            return Ok(());
        }
        fs::write(sandbox.work().join(".gitignore"), rules)?;
        let scratch = sandbox.folder("scratch")?;
        let target = scratch.join("project-data");
        fs::create_dir(&target)?;
        link_folder(&target, &sandbox.work().join("data"))?;
        ok(&sandbox.run(&[
            "settings",
            "set",
            "data_links",
            &scratch.display().to_string(),
        ])?)?;
        let routes = Routes::start(vec![
            ("/iris.csv".to_owned(), file(IRIS, "\"1\"")),
            ("/wine.csv".to_owned(), file(b"wine\n", "\"2\"")),
        ])?;
        sandbox.allow(&routes.url);
        let link = |name: &str| format!("{}/{name}.csv", routes.url);
        let Some(screen) = sandbox.on_a_terminal(&["add", &link("iris"), "--as", "iris"])? else {
            return Ok(());
        };
        assert!(screen.contains("Added iris"), "{rules}: {screen}");
        assert_eq!(screen.contains(told_line), told, "{rules}: {screen}");
        // git never looks behind a link, so no line of .gitignore hides the files there
        assert!(!screen.contains("keeps all of data/"), "{rules}: {screen}");
        let again = sandbox
            .on_a_terminal(&["add", &link("wine"), "--as", "wine"])?
            .unwrap_or_default();
        assert!(again.contains("Added wine"), "{rules}: {again}");
        assert!(!again.contains(told_line), "{rules}, said once: {again}");
    }
    Ok(())
}
