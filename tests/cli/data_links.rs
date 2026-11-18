//! a data folder a person linked onto another disk, as clusters keep code in a small home and
//! data on scratch: followed only into a folder this machine's `data_links` setting lists.

use std::fs;
use std::path::{Path, PathBuf};

use crate::support::{Result, Routes, Sandbox, delete, file, link_folder, ok, text};

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
/// folder holding a file of other bytes at a path data.lock pins, nor one inside another project's
/// folder, so neither a sync, the `sync --force` that replaces files, `path`, nor an add of another
/// dataset, after which a later run would trust the folder, writes anything there.
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
    let routes = Routes::start(vec![
        ("/iris.csv".to_owned(), file(IRIS, "\"1\"")),
        ("/wine.csv".to_owned(), file(b"wine\n", "\"2\"")),
    ])?;
    sandbox.allow(&routes.url);
    cloned(&sandbox, &format!("{}/iris.csv", routes.url))?;
    ok(&sandbox.run(&["settings", "set", "data_links", &root.display().to_string()])?)?;
    let before = listing(&thesis)?;
    let wine = format!("{}/wine.csv", routes.url);
    for args in [
        &["sync"][..],
        &["sync", "--force"],
        &["path", "iris"],
        &["add", &wine, "--as", "wine"],
    ] {
        let output = sandbox.run(args)?;
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(50), "{args:?}: {stderr}");
        assert!(
            stderr.contains(&format!(
                " is a link to {}, where iris/iris.csv holds other bytes than data.lock pins, which `sync --force` would replace, so fetchloom places nothing through it\n",
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

/// a teammate's clone holds the author's data link when it was committed before the line that
/// hides it. refused as not on this machine or not listed, a link git tracks is said to have come
/// with the clone, with the way to a plain data folder: deleting it, and committing that. a link
/// git does not track gets no such line.
#[cfg(unix)]
#[test]
fn a_refused_link_git_tracks_says_it_came_with_the_clone() -> Result {
    let sandbox = Sandbox::new()?;
    if crate::merge::ok(&sandbox, &["init", "-q"])?.is_none() {
        return Ok(());
    }
    let routes = Routes::start(vec![("/iris.csv".to_owned(), file(IRIS, "\"1\""))])?;
    sandbox.allow(&routes.url);
    cloned(&sandbox, &format!("{}/iris.csv", routes.url))?;
    let data = sandbox.work().join("data");
    let came = "the link came with the clone, as git tracks it: deleting it with `git rm -- data`, and committing that, gives this project a plain data folder";
    let commit = |message: &str| -> Result {
        crate::merge::ok(&sandbox, &["add", "-A"])?;
        crate::merge::ok(&sandbox, &["commit", "-qm", message])?;
        Ok(())
    };
    let refused = |args: &[&str], euid: Option<&str>, why: &str| -> Result<String> {
        let mut command = sandbox.command(args);
        if let Some(euid) = euid {
            command.env("FETCHLOOM_TEST_EUID", euid);
        }
        let output = command.output()?;
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(50), "{why}: {stderr}");
        assert!(stderr.contains(why), "{why}: {stderr}");
        Ok(stderr)
    };
    let elsewhere = sandbox.folder("elsewhere")?;
    link_folder(&elsewhere, &data)?;
    let unlisted = "which no folder in data_links holds, so fetchloom places nothing through it";
    let said = refused(&["sync"], None, unlisted)?;
    assert!(!said.contains(came), "not in git yet: {said}");
    commit("link data elsewhere")?;
    let said = refused(&["sync"], None, unlisted)?;
    assert!(said.contains(came), "{said}");
    unlink(&data)?;
    link_folder(&sandbox.folder("gone")?.join("data"), &data)?;
    commit("link data to a disk this machine lacks")?;
    let said = refused(
        &["sync"],
        None,
        "which does not exist: its disk may be unmounted",
    )?;
    assert!(said.contains(came), "{said}");
    crate::merge::ok(&sandbox, &["rm", "-q", "--", "data"])?;
    crate::merge::ok(&sandbox, &["commit", "-qm", "a plain data folder"])?;
    let synced = sandbox.run(&["sync"])?;
    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    assert!(fs::symlink_metadata(&data)?.is_dir());
    assert_eq!(fs::read(data.join("iris").join("iris.csv"))?, IRIS);
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
/// an edit told of at the same path is no match for a file there of the same size, as its time is
/// another.
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
    let routes = Routes::start(vec![
        ("/iris.csv".to_owned(), file(IRIS, "\"1\"")),
        ("/wine.csv".to_owned(), file(b"wine\n", "\"2\"")),
    ])?;
    sandbox.allow(&routes.url);
    ok(&sandbox.run(&["add", &format!("{}/iris.csv", routes.url), "--as", "iris"])?)?;
    let edited = target.join("iris").join("iris.csv");
    delete(&edited)?;
    fs::write(&edited, b"edited\n")?;
    let told = sandbox.run(&["--offline", "sync"])?;
    let said = text(&told.stderr)?;
    assert!(
        said.contains("warning: data/iris/iris.csv has changed, so it stays\n"),
        "{said}"
    );
    let other = scratch.join("someone");
    fs::create_dir_all(other.join("iris"))?;
    let theirs = other.join("iris").join("iris.csv");
    fs::write(&theirs, b"theirs\n")?;
    let later = fs::metadata(&edited)?.modified()? + std::time::Duration::from_secs(1);
    fs::File::options()
        .write(true)
        .open(&theirs)?
        .set_modified(later)?;
    unlink(&data)?;
    link_folder(&other, &data)?;
    let before = listing(&other)?;
    let wine = format!("{}/wine.csv", routes.url);
    for args in [
        &["--offline", "sync"][..],
        &["--offline", "sync", "--force"],
        &["add", &wine, "--as", "wine"],
    ] {
        let output = sandbox.run(args)?;
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(50), "{args:?}: {stderr}");
        assert!(
            stderr.contains("where iris/iris.csv holds other bytes than data.lock pins, which `sync --force` would replace"),
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

/// a data folder linked onto a disk apart from the cache, as clusters keep the code and the cache
/// in home and the data on scratch, gets copies. the warning names the cache and the data folder,
/// and its help moves the cache onto the data folder's disk, never the project, whose code
/// stays where it is backed up.
#[cfg(target_os = "linux")]
#[test]
fn a_data_folder_on_another_disk_than_the_cache_moves_the_cache() -> Result {
    use std::os::unix::fs::MetadataExt;
    let sandbox = Sandbox::new()?;
    let shared = Path::new("/dev/shm");
    let apart = fs::metadata(shared)
        .ok()
        .zip(fs::metadata(sandbox.work()).ok())
        .is_some_and(|(shared, work)| shared.dev() != work.dev());
    if !apart {
        return crate::support::skipped("two-disks", "/dev/shm is on the sandbox's own disk");
    }
    let scratch = tempfile::tempdir_in(shared)?;
    let target = scratch.path().join("project-data");
    fs::create_dir(&target)?;
    link_folder(&target, &sandbox.work().join("data"))?;
    ok(&sandbox.run(&[
        "settings",
        "set",
        "data_links",
        &scratch.path().display().to_string(),
    ])?)?;
    let routes = Routes::start(vec![("/iris.csv".to_owned(), file(IRIS, "\"1\""))])?;
    sandbox.allow(&routes.url);
    let added = sandbox.run(&["add", &format!("{}/iris.csv", routes.url)])?;
    let stderr = text(&added.stderr)?;
    assert_eq!(added.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("warning: the cache is on another drive than data, so files were copied\n  help: to link files instead, set FETCHLOOM_CACHE to a folder on the drive of data. see https://nikhil-yn-aix.github.io/fetchloom/user/concepts/cache-and-placement/#links-need-one-drive\n"),
        "{stderr}"
    );
    assert!(!stderr.contains("move the project"), "{stderr}");
    assert_eq!(fs::read(target.join("iris").join("iris.csv"))?, IRIS);
    Ok(())
}

/// a data folder link git already tracks, as `git add -A` takes it, stays committed whatever
/// .gitignore says, so the hint also names the command that takes it out of git and leaves it in
/// place, with the link's name quoted for the shell.
#[test]
fn a_committed_data_link_is_told_how_to_leave_git() -> Result {
    let sandbox = Sandbox::new()?;
    if crate::merge::ok(&sandbox, &["init", "-q"])?.is_none() {
        return Ok(());
    }
    let scratch = sandbox.folder("scratch")?;
    let target = scratch.join("project-data");
    fs::create_dir(&target)?;
    fs::write(
        sandbox.work().join("data.toml"),
        "[project]\ndata_dir = \"my data\"\n",
    )?;
    if link_folder(&target, &sandbox.work().join("my data")).is_err() {
        return crate::support::skipped("symlinks", "no symbolic link can be made here");
    }
    crate::merge::ok(&sandbox, &["add", "-A"])?;
    crate::merge::ok(&sandbox, &["commit", "-qm", "linked"])?;
    ok(&sandbox.run(&[
        "settings",
        "set",
        "data_links",
        &scratch.display().to_string(),
    ])?)?;
    let routes = Routes::start(vec![("/iris.csv".to_owned(), file(IRIS, "\"1\""))])?;
    sandbox.allow(&routes.url);
    let link = format!("{}/iris.csv", routes.url);
    let added = sandbox.run(&["add", &link, "--as", "iris"])?;
    let stderr = text(&added.stderr)?;
    assert_eq!(added.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains(
            "if git tracks it already, `git rm --cached -- \"my data\"` takes it out of git and leaves it where it is"
        ),
        "{stderr}"
    );
    Ok(())
}

/// a person who moves a data folder holding their own processed/ and interim/ beside the datasets
/// onto scratch, and links it back, keeps working: a first follow takes what this project does
/// not place there as the person's, as the data folder keeps a person's files everywhere.
#[test]
fn a_moved_data_folder_with_the_persons_own_folders_is_followed() -> Result {
    let sandbox = Sandbox::new()?;
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
    for (folder, name) in [("processed", "clean.csv"), ("interim", "step1.csv")] {
        fs::create_dir_all(data.join(folder))?;
        fs::write(data.join(folder).join(name), b"mine\n")?;
    }
    let moved = scratch.join("project-data");
    fs::rename(&data, &moved)?;
    link_folder(&moved, &data)?;
    ok(&sandbox.run(&[
        "settings",
        "set",
        "data_links",
        &scratch.display().to_string(),
    ])?)?;
    for args in [&["--offline", "status"][..], &["--offline", "sync"]] {
        let output = sandbox.run(args)?;
        assert_eq!(
            output.status.code(),
            Some(0),
            "{args:?}: {}",
            text(&output.stderr)?
        );
    }
    assert_eq!(
        fs::read(moved.join("processed").join("clean.csv"))?,
        b"mine\n"
    );
    assert_eq!(
        fs::read(moved.join("interim").join("step1.csv"))?,
        b"mine\n"
    );
    assert_eq!(fs::read(moved.join("iris").join("iris.csv"))?, IRIS);
    assert_eq!(recorded(&sandbox.work(), "data")?, Some(resolved(&moved)?));
    Ok(())
}

/// a pipeline may write into a freshly linked data folder before fetchloom ever runs there: its
/// files are the person's, so the first follow takes the folder and leaves them be.
#[test]
fn a_pipelines_files_written_before_the_first_follow_stay() -> Result {
    let sandbox = Sandbox::new()?;
    let scratch = sandbox.folder("scratch")?;
    let target = scratch.join("project-data");
    fs::create_dir_all(target.join("results"))?;
    let data = sandbox.work().join("data");
    if link_folder(&target, &data).is_err() {
        return crate::support::skipped("symlinks", "no symbolic link can be made here");
    }
    fs::write(data.join("results").join("out.csv"), b"ran\n")?;
    ok(&sandbox.run(&[
        "settings",
        "set",
        "data_links",
        &scratch.display().to_string(),
    ])?)?;
    let routes = Routes::start(vec![("/iris.csv".to_owned(), file(IRIS, "\"1\""))])?;
    sandbox.allow(&routes.url);
    let added = sandbox.run(&["add", &format!("{}/iris.csv", routes.url), "--as", "iris"])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    assert_eq!(fs::read(target.join("results").join("out.csv"))?, b"ran\n");
    assert_eq!(fs::read(target.join("iris").join("iris.csv"))?, IRIS);
    Ok(())
}

/// a file downloaded by hand to the path data.lock pins, before the link's first follow, is taken
/// as placed when it holds the pinned bytes, as a copied project's files are; one holding other
/// bytes, or a file where data.lock pins a folder, is what `sync --force` would replace, so the
/// first follow refuses the folder.
#[test]
fn a_file_downloaded_by_hand_is_adopted_only_with_the_pinned_bytes() -> Result {
    let sandbox = Sandbox::new()?;
    let scratch = sandbox.folder("scratch")?;
    let data = sandbox.work().join("data");
    let probe = sandbox.work().join("probe");
    if link_folder(&scratch, &probe).is_err() {
        return crate::support::skipped("symlinks", "no symbolic link can be made here");
    }
    unlink(&probe)?;
    let routes = Routes::start(vec![("/iris.csv".to_owned(), file(IRIS, "\"1\""))])?;
    sandbox.allow(&routes.url);
    cloned(&sandbox, &format!("{}/iris.csv", routes.url))?;
    ok(&sandbox.run(&[
        "settings",
        "set",
        "data_links",
        &scratch.display().to_string(),
    ])?)?;
    // each folder the link leads to in turn, the file put there, and why its first follow refuses
    let cases: [(&str, &str, &[u8], Option<&str>); 3] = [
        ("pinned", "iris/iris.csv", IRIS, None),
        (
            "other",
            "iris/iris.csv",
            b"mine\n",
            Some(
                "iris/iris.csv holds other bytes than data.lock pins, which `sync --force` would replace",
            ),
        ),
        (
            "in-the-way",
            "iris",
            b"mine\n",
            Some("iris stands where data.lock pins a folder, which `sync --force` would remove"),
        ),
    ];
    for (folder, file, bytes, refused) in cases {
        let target = scratch.join(folder);
        let put = target.join(file);
        fs::create_dir_all(put.parent().unwrap_or(&target))?;
        fs::write(&put, bytes)?;
        if data.exists() {
            unlink(&data)?;
        }
        link_folder(&target, &data)?;
        let output = sandbox.run(&["--offline", "sync"])?;
        let stderr = text(&output.stderr)?;
        if let Some(why) = refused {
            assert_eq!(output.status.code(), Some(50), "{folder}: {stderr}");
            assert!(
                stderr.contains(&format!(
                    " is a link to {}, where {why}, so fetchloom places nothing through it\n",
                    real(&target)?
                )),
                "{folder}: {stderr}"
            );
        } else {
            assert_eq!(output.status.code(), Some(0), "{stderr}");
            let status = sandbox.run(&["--offline", "status"])?;
            let said = text(&status.stderr)?;
            assert!(said.contains("Ok everything matches data.lock"), "{said}");
            assert_eq!(recorded(&sandbox.work(), "data")?, Some(resolved(&target)?));
        }
        assert_eq!(fs::read(&put)?, bytes, "{folder}");
    }
    // a link where data.lock pins a folder holds nothing of the person's: no first follow refuses
    // it, and `sync --force` takes the link away, never what it leads to, and places the file
    let linked = scratch.join("linked");
    let elsewhere = scratch.join("elsewhere");
    fs::create_dir_all(&linked)?;
    fs::create_dir_all(&elsewhere)?;
    fs::write(elsewhere.join("notes.txt"), b"mine\n")?;
    link_folder(&elsewhere, &linked.join("iris"))?;
    unlink(&data)?;
    link_folder(&linked, &data)?;
    let kept = sandbox.run(&["--offline", "sync"])?;
    let stderr = text(&kept.stderr)?;
    assert!(!stderr.contains("places nothing through it"), "{stderr}");
    assert_eq!(kept.status.code(), Some(0), "{stderr}");
    ok(&sandbox.run(&["--offline", "sync", "--force"])?)?;
    assert_eq!(fs::read(linked.join("iris").join("iris.csv"))?, IRIS);
    assert_eq!(fs::read(elsewhere.join("notes.txt"))?, b"mine\n");
    Ok(())
}

/// a person who fixed a header in place, which fetchloom told them it keeps, and later moves the
/// data folder onto scratch keeping its times, as mv or rsync -a do, keeps working: the edit the
/// state told of, by its path, size and time, is theirs, which only their own `sync --force` could
/// touch, so the link's first follow takes the folder with it.
#[test]
fn a_told_edit_moved_with_its_data_folder_is_followed() -> Result {
    let sandbox = Sandbox::new()?;
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
    let placed = data.join("iris").join("iris.csv");
    delete(&placed)?;
    fs::write(&placed, b"sl,sw,pl,pw,species\n")?;
    let told = sandbox.run(&["--offline", "sync"])?;
    let said = text(&told.stderr)?;
    assert!(
        said.contains("warning: data/iris/iris.csv has changed, so it stays\n"),
        "{said}"
    );
    let moved = scratch.join("project-data");
    fs::rename(&data, &moved)?;
    link_folder(&moved, &data)?;
    ok(&sandbox.run(&[
        "settings",
        "set",
        "data_links",
        &scratch.display().to_string(),
    ])?)?;
    let synced = sandbox.run(&["--offline", "sync"])?;
    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(0), "{stderr}");
    assert!(!stderr.contains("places nothing through it"), "{stderr}");
    assert_eq!(
        fs::read(moved.join("iris").join("iris.csv"))?,
        b"sl,sw,pl,pw,species\n"
    );
    assert_eq!(recorded(&sandbox.work(), "data")?, Some(resolved(&moved)?));
    Ok(())
}

/// w1: an output its pipeline wrote again, changing one file and adding another, which sync told
/// of and left whole, moves with its data folder onto scratch keeping its times: both files are
/// the person's, as a told edit is, so the link's first follow takes the folder, and sync, an add
/// and path work through it. pinned again, the files are the pin's, and their records go.
#[test]
fn a_told_output_change_moved_with_its_data_folder_is_followed() -> Result {
    let sandbox = Sandbox::new()?;
    let scratch = sandbox.folder("scratch")?;
    let probe = sandbox.work().join("probe");
    if link_folder(&scratch, &probe).is_err() {
        return crate::support::skipped("symlinks", "no symbolic link can be made here");
    }
    unlink(&probe)?;
    let data = sandbox.work().join("data");
    let processed = data.join("processed");
    fs::create_dir_all(&processed)?;
    fs::write(processed.join("features.csv"), b"x,y\n")?;
    ok(&sandbox.run(&["add", "data/processed", "--as", "processed"])?)?;
    fs::write(processed.join("features.csv"), b"x,y,z\n")?;
    fs::write(processed.join("fig.svg"), b"<svg/>\n")?;
    let told = sandbox.run(&["sync"])?;
    let said = text(&told.stderr)?;
    assert!(
        said.contains(
            "warning: data/processed changed since processed was pinned: 1 file changed, 1 added"
        ),
        "{said}"
    );
    let moved = scratch.join("project-data");
    fs::rename(&data, &moved)?;
    link_folder(&moved, &data)?;
    ok(&sandbox.run(&[
        "settings",
        "set",
        "data_links",
        &scratch.display().to_string(),
    ])?)?;
    let routes = Routes::start(vec![("/iris.csv".to_owned(), file(IRIS, "\"1\""))])?;
    sandbox.allow(&routes.url);
    let link = format!("{}/iris.csv", routes.url);
    for args in [
        &["sync"][..],
        &["add", &link, "--as", "iris"],
        &["path", "processed"],
    ] {
        let output = sandbox.run(args)?;
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(0), "{args:?}: {stderr}");
    }
    assert_eq!(
        fs::read(moved.join("processed").join("features.csv"))?,
        b"x,y,z\n"
    );
    assert_eq!(
        fs::read(moved.join("processed").join("fig.svg"))?,
        b"<svg/>\n"
    );
    let edits = || -> Result<usize> {
        let state = fs::read(sandbox.work().join(".fetchloom").join("state"))?;
        let state: serde_json::Value = serde_json::from_slice(&state)?;
        Ok(state["told"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|told| {
                told.as_str()
                    .is_some_and(|told| told.starts_with("edit:data/processed/"))
            })
            .count())
    };
    assert_eq!(edits()?, 2);
    ok(&sandbox.run(&["add", "data/processed", "--as", "processed", "--yes"])?)?;
    assert_eq!(edits()?, 0);
    Ok(())
}

/// once a link's first follow is recorded, the folder is this project's data folder as any other:
/// a person's edit of a pinned file and a pipeline's new file in an output's folder are told as
/// anywhere, and no sync refuses the link for them.
#[test]
fn a_recorded_link_takes_edits_as_any_data_folder() -> Result {
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
    fs::create_dir(data.join("processed"))?;
    fs::write(data.join("processed").join("clean.csv"), b"clean\n")?;
    ok(&sandbox.run(&["add", "data/processed", "--as", "processed"])?)?;
    assert_eq!(recorded(&sandbox.work(), "data")?, Some(resolved(&target)?));
    let placed = data.join("iris").join("iris.csv");
    delete(&placed)?;
    fs::write(&placed, b"sl,sw,pl,pw,species\n")?;
    fs::write(data.join("processed").join("new.csv"), b"new\n")?;
    let synced = sandbox.run(&["--offline", "sync"])?;
    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("warning: data/iris/iris.csv has changed, so it stays\n"),
        "{stderr}"
    );
    assert!(
        stderr.contains("warning: data/processed changed since processed was pinned: "),
        "{stderr}"
    );
    assert_eq!(fs::read(&placed)?, b"sl,sw,pl,pw,species\n");
    assert_eq!(fs::read(data.join("processed").join("new.csv"))?, b"new\n");
    Ok(())
}

/// cookiecutter's data science layout keeps a .gitkeep in each of data's folders, and a person
/// keeps notes there too: moved onto scratch with nothing pinned in those folders, they are the
/// person's, so the link's first follow takes the folder as it is.
#[test]
fn a_cookiecutter_data_folder_on_scratch_is_followed() -> Result {
    let sandbox = Sandbox::new()?;
    let scratch = sandbox.folder("scratch")?;
    let target = scratch.join("project-data");
    let layout = ["external", "interim", "processed", "raw"];
    for folder in layout {
        fs::create_dir_all(target.join(folder))?;
        fs::write(target.join(folder).join(".gitkeep"), b"")?;
    }
    fs::write(
        target.join("raw").join("README.md"),
        b"where the raw files came from\n",
    )?;
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
    let added = sandbox.run(&["add", &format!("{}/iris.csv", routes.url), "--as", "iris"])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    assert_eq!(fs::read(target.join("iris").join("iris.csv"))?, IRIS);
    for folder in layout {
        assert!(target.join(folder).join(".gitkeep").is_file(), "{folder}");
    }
    assert_eq!(
        fs::read(target.join("raw").join("README.md"))?,
        b"where the raw files came from\n"
    );
    assert_eq!(recorded(&sandbox.work(), "data")?, Some(resolved(&target)?));
    Ok(())
}

/// a file in the folder of an output this project declares that `sync --force` would harm, one no
/// pin of the output places, which it deletes as the output's snapshot lacks it, or a rerun's at a
/// path the pin holds, which it replaces, refuses a link's first follow of the folder holding it,
/// with help naming the pin of the folder again; git's placeholder there is no file of the output.
/// pinning that folder again claims the file, and the link is followed then.
#[test]
fn a_file_force_would_harm_in_an_outputs_folder_refuses_the_first_follow() -> Result {
    for (file, bytes, harm) in [
        (
            "stray.csv",
            &b"mine\n"[..],
            "processed/stray.csv is in an output's folder though no pin places it, which `sync --force` would delete",
        ),
        (
            "clean.csv",
            b"rerun\n",
            "processed/clean.csv holds other bytes than data.lock pins, which `sync --force` would replace",
        ),
    ] {
        let sandbox = Sandbox::new()?;
        let scratch = sandbox.folder("scratch")?;
        let target = scratch.join("project-data");
        let processed = target.join("processed");
        fs::create_dir_all(&processed)?;
        if link_folder(&target, &sandbox.work().join("data")).is_err() {
            return crate::support::skipped("symlinks", "no symbolic link can be made here");
        }
        let origin = sandbox.folder("origin")?;
        fs::create_dir_all(origin.join("data").join("processed"))?;
        fs::write(
            origin.join("data").join("processed").join("clean.csv"),
            b"clean\n",
        )?;
        let pinned = sandbox
            .command_in(&origin, &["add", "data/processed", "--as", "processed"])
            .output()?;
        assert_eq!(pinned.status.code(), Some(0), "{}", text(&pinned.stderr)?);
        for name in ["data.toml", "data.lock"] {
            fs::copy(origin.join(name), sandbox.work().join(name))?;
        }
        fs::write(processed.join("clean.csv"), b"clean\n")?;
        fs::write(processed.join(".gitkeep"), b"")?;
        fs::write(processed.join(file), bytes)?;
        ok(&sandbox.run(&[
            "settings",
            "set",
            "data_links",
            &scratch.display().to_string(),
        ])?)?;
        let before = listing(&target)?;
        for args in [
            &["--offline", "sync"][..],
            &["--offline", "sync", "--force", "processed"],
        ] {
            let output = sandbox.run(args)?;
            let stderr = text(&output.stderr)?;
            assert_eq!(output.status.code(), Some(50), "{args:?}: {stderr}");
            assert!(
                stderr.contains(&format!(
                    " is a link to {}, where {harm}, so fetchloom places nothing through it\n",
                    real(&target)?
                )),
                "{args:?}: {stderr}"
            );
            assert!(
                stderr.contains("help: point the link at a folder of this project's own, or keep that file by moving it out of the output's folder or by pinning the folder again with `fetchloom add ./data/processed --as processed`\n"),
                "{args:?}: {stderr}"
            );
            assert_eq!(listing(&target)?, before, "{args:?}");
        }
        ok(&sandbox.run(&["add", "./data/processed", "--as", "processed", "--yes"])?)?;
        let synced = sandbox.run(&["--offline", "sync"])?;
        assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
        assert_eq!(fs::read(processed.join(file))?, bytes);
        assert_eq!(recorded(&sandbox.work(), "data")?, Some(resolved(&target)?));
    }
    Ok(())
}

/// a .gitignore in fetchloom's own marker form that this project's state does not record marks a
/// dataset folder of another fetchloom project, so a first follow refuses the folder holding it,
/// whatever else lies beside it: a fresh link's, and a settled project's whose data folder moved
/// there, which a sync with nothing to place checks as well.
#[test]
fn a_folder_another_projects_marker_holds_is_refused() -> Result {
    let sandbox = Sandbox::new()?;
    let scratch = sandbox.folder("scratch")?;
    let target = scratch.join("shared");
    fs::create_dir_all(target.join("results"))?;
    fs::write(target.join("results").join(".gitignore"), b"*\n")?;
    fs::write(target.join("results").join("table.csv"), b"theirs\n")?;
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
    let before = listing(&target)?;
    let output = sandbox.run(&["add", &format!("{}/iris.csv", routes.url), "--as", "iris"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(50), "{stderr}");
    assert!(
        stderr.contains(&format!(
            " is a link to {}, where results/.gitignore marks a dataset folder of another fetchloom project, so fetchloom places nothing through it\n",
            real(&target)?
        )),
        "{stderr}"
    );
    assert_eq!(listing(&target)?, before);
    // an output folder's marker names its files after a first line of fetchloom's own
    fs::write(
        target.join("results").join(".gitignore"),
        b"# fetchloom keeps the dataset files placed here out of git, and rewrites this list as they change\n/table.csv\n",
    )?;
    unlink(&data)?;
    ok(&sandbox.run(&["add", &format!("{}/iris.csv", routes.url), "--as", "iris"])?)?;
    let moved = scratch.join("moved");
    fs::rename(&data, &moved)?;
    fs::rename(target.join("results"), moved.join("results"))?;
    link_folder(&moved, &data)?;
    let before = listing(&moved)?;
    let synced = sandbox.run(&["--offline", "sync"])?;
    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(50), "{stderr}");
    assert!(
        stderr.contains(&format!(
            " is a link to {}, where results/.gitignore marks a dataset folder of another fetchloom project, so fetchloom places nothing through it\n",
            real(&moved)?
        )),
        "{stderr}"
    );
    assert_eq!(listing(&moved)?, before);
    Ok(())
}

/// an output's capture claims the folder it pins, so a capture that is a link's first follow takes
/// the folder with a marker of fetchloom's form in it, as a renamed output folder carries its own.
#[test]
fn a_capture_claims_a_marker_in_the_folder_it_pins() -> Result {
    let sandbox = Sandbox::new()?;
    let scratch = sandbox.folder("scratch")?;
    let target = scratch.join("project-data");
    fs::create_dir_all(target.join("results"))?;
    fs::write(target.join("results").join(".gitignore"), b"*\n")?;
    fs::write(target.join("results").join("table.csv"), b"a,b\n1,2\n")?;
    if link_folder(&target, &sandbox.work().join("data")).is_err() {
        return crate::support::skipped("symlinks", "no symbolic link can be made here");
    }
    ok(&sandbox.run(&[
        "settings",
        "set",
        "data_links",
        &scratch.display().to_string(),
    ])?)?;
    let pinned = sandbox.run(&["add", "data/results", "--as", "results"])?;
    assert_eq!(pinned.status.code(), Some(0), "{}", text(&pinned.stderr)?);
    assert_eq!(
        fs::read(target.join("results").join("table.csv"))?,
        b"a,b\n1,2\n"
    );
    assert_eq!(recorded(&sandbox.work(), "data")?, Some(resolved(&target)?));
    Ok(())
}

/// git sees a data folder link itself, so a commit takes it and each clone then refuses it, and a
/// line ending in /, as cookiecutter's /data/, hides folders alone. the line that hides the link,
/// /data, is said once whatever the terminal, as a notebook runs fetchloom with none, and in --json
/// as a warning of its kind; with that line there, nothing.
#[test]
fn a_followed_link_git_sees_is_told_once() -> Result {
    for (rules, json) in [("/data/\n", false), ("/data/\n", true), ("/data\n", false)] {
        let sandbox = Sandbox::new()?;
        if crate::merge::ok(&sandbox, &["init", "-q"])?.is_none() {
            return Ok(());
        }
        fs::write(sandbox.work().join(".gitignore"), rules)?;
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
        let routes = Routes::start(vec![
            ("/iris.csv".to_owned(), file(IRIS, "\"1\"")),
            ("/wine.csv".to_owned(), file(b"wine\n", "\"2\"")),
        ])?;
        sandbox.allow(&routes.url);
        // whether the add of `name` said the /data line, and the root line hiding data/
        let told = |name: &str| -> Result<(bool, bool)> {
            let link = format!("{}/{name}.csv", routes.url);
            let mut args = vec!["add", &link, "--as", name];
            if json {
                args.insert(0, "--json");
            }
            let output = sandbox.run(&args)?;
            let stderr = text(&output.stderr)?;
            assert_eq!(output.status.code(), Some(0), "{name}: {stderr}");
            if json {
                let kinds = crate::support::kinds(&crate::support::json(&output)?)
                    .into_iter()
                    .map(str::to_owned)
                    .collect::<Vec<_>>();
                return Ok((
                    kinds.iter().any(|kind| kind == "git.data_link_unignored"),
                    kinds.iter().any(|kind| kind == "git.data_folder_ignored"),
                ));
            }
            Ok((
                stderr.contains("add the line /data to the project's .gitignore"),
                stderr.contains("keeps all of data/"),
            ))
        };
        let said = rules == "/data/\n";
        // git never looks behind a link, so no line of .gitignore hides the files there
        assert_eq!(told("iris")?, (said, false), "{rules}, json: {json}");
        assert_eq!(
            told("wine")?,
            (false, false),
            "{rules}, json: {json}, said once"
        );
    }
    Ok(())
}
