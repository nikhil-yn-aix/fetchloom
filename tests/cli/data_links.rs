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
/// listed folder. one to the home folder or its autostart folder, one whose path climbs out of
/// the listed folder with `..`, one through a link inside the listed folder that leads out, and
/// one to a folder that is gone are refused before anything is written there; the refusal names
/// the setting with the command that lists the folder, and a folder a link leads to is never made.
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
    fs::remove_file(&manifest)?;
    // a dataset's folder that is a link stays where it leads
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
