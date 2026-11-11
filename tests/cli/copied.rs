//! a project copied with its data folder to a machine whose cache never held its files: what the
//! data folder holds at the paths data.lock places is taken back into the cache, after a check of
//! its bytes, so push, build and sync need no request and no source.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::build::{build, building_in, runs};
use crate::export::sha256;
use crate::project::{IRIS, PATH, added};
use crate::support::{Result, Sandbox, Server, delete, edit, file, json, text};

/// a recipe that makes `clean` by copying iris, so the data folder holds iris's bytes twice.
const CLEAN: &str = "\n[datasets.clean]\nfrom = [\"iris\"]\nrun = [\"recipe-step\", \"copy\", \"{iris}\", \"{out}\"]\n";

/// iris with one byte changed: the size data.lock pins, other bytes.
const EDITED: &[u8] = b"sepal_length,species\n5.2,setosa\n";

/// a project in the work folder whose data folder is `inputs`, with iris added from `server`
/// and, with `recipe`, [`CLEAN`] built from it: one object at two paths.
fn project(sandbox: &Sandbox, server: &Server, recipe: bool) -> Result {
    let manifest = sandbox.work().join("data.toml");
    fs::write(&manifest, "[project]\ndata_dir = \"inputs\"\n")?;
    added(sandbox, server)?;
    if recipe {
        let written = fs::read_to_string(&manifest)?;
        fs::write(&manifest, format!("{written}{CLEAN}"))?;
        let built = build(sandbox, &["build"])?;
        assert_eq!(built.status.code(), Some(0), "{}", text(&built.stderr)?);
    }
    Ok(())
}

/// the work folder copied to `name` as `cp -a` copies it, data folder, data.lock.d and
/// `.fetchloom` included, with an empty cache of its own: the project on another machine.
fn copied(sandbox: &Sandbox, name: &str) -> Result<(PathBuf, PathBuf)> {
    let folder = sandbox.folder(name)?;
    copy_tree(&sandbox.work(), &folder)?;
    Ok((folder, sandbox.folder(&format!("{name} cache"))?))
}

/// copies every file below `from` into `to`, keeping each file's modification time and
/// read-only flag.
fn copy_tree(from: &Path, to: &Path) -> Result {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
            continue;
        }
        let found = fs::metadata(entry.path())?;
        fs::File::create(&target)?.write_all(&fs::read(entry.path())?)?;
        fs::File::options()
            .write(true)
            .open(&target)?
            .set_modified(found.modified()?)?;
        fs::set_permissions(&target, found.permissions())?;
    }
    Ok(())
}

/// the bytes and modification time of the file at `path`.
fn look(path: &Path) -> Result<(Vec<u8>, SystemTime)> {
    Ok((fs::read(path)?, fs::metadata(path)?.modified()?))
}

/// where a cache or a mirror keeps the object holding `bytes`, below `root`.
fn object(root: &Path, bytes: &[u8]) -> PathBuf {
    let hex = sha256(bytes);
    root.join("objects").join(&hex[..2]).join(&hex[2..])
}

/// the file at `path` is no other name of the object `cache` keeps, so an edit of one never
/// reaches the other.
fn unlinked(path: &Path, cache: &Path) -> Result {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let (file, kept) = (fs::metadata(path)?, fs::metadata(object(cache, IRIS))?);
        assert_eq!(file.nlink(), 1, "{}", path.display());
        assert_ne!(file.ino(), kept.ino(), "{}", path.display());
    }
    #[cfg(not(unix))]
    let _ = (path, cache);
    Ok(())
}

/// push copies what the cache lacks from the data folder into the cache, then into the mirror,
/// asking nothing of the source and leaving the data folder's files as they were. its dry run
/// says so and writes nothing.
#[test]
fn a_copied_project_pushes_from_its_data_folder_with_no_request() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    project(&sandbox, &server, true)?;
    let asked = server.requests().len();
    let (two, cache) = copied(&sandbox, "two")?;
    let placed = two.join("inputs").join("iris").join("iris.csv");
    let before = look(&placed)?;
    let mirror = sandbox.folder("lab")?.join("mirror");
    let push = |args: &[&str]| {
        sandbox
            .command_in(&two, args)
            .arg("--to")
            .arg(&mirror)
            .env("FETCHLOOM_CACHE", &cache)
            .output()
    };
    let dry = push(&["--yes", "push", "--dry-run"])?;
    let stderr = text(&dry.stderr)?;
    assert_eq!(dry.status.code(), Some(0), "{stderr}");
    let said = format!(
        "Would copy 1 file · {} B from inputs into the cache",
        IRIS.len()
    );
    assert!(stderr.contains(&said), "{stderr}");
    assert!(!mirror.exists() && !object(&cache, IRIS).exists());
    let output = push(&["--json", "--yes", "push"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
    let document = json(&output)?;
    assert_eq!(document["result"]["copied"], 1, "{document}");
    assert_eq!(document["work"]["data_folder_objects"], 1, "{document}");
    assert_eq!(
        document["work"]["data_folder_bytes"],
        IRIS.len(),
        "{document}"
    );
    assert_eq!(fs::read(object(&mirror, IRIS))?, IRIS);
    assert_eq!(server.requests().len(), asked, "the source is never asked");
    assert_eq!(look(&placed)?, before);
    unlinked(&placed, &cache)?;
    unlinked(&two.join("inputs").join("clean").join("iris.csv"), &cache)?;
    Ok(())
}

/// a check runs every step from the inputs data.lock pins, which the data folder holds.
#[test]
fn an_offline_check_of_a_copied_project_reads_its_data_folder() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    project(&sandbox, &server, true)?;
    let (two, cache) = copied(&sandbox, "two")?;
    let ran = runs(&two)?;
    let output = building_in(&sandbox, &two, &["--offline", "build", "--check"])?
        .env("FETCHLOOM_CACHE", &cache)
        .output()?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    let said = format!(
        "Copied 1 file · {} B from inputs into the cache",
        IRIS.len()
    );
    assert!(stderr.contains(&said), "{stderr}");
    assert_eq!(runs(&two)?, ran + 1);
    Ok(())
}

/// a recipe never built here runs offline on what the data folder holds of its input.
#[test]
fn an_offline_build_runs_a_recipe_whose_input_is_only_in_the_data_folder() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    project(&sandbox, &server, false)?;
    let (two, cache) = copied(&sandbox, "two")?;
    let manifest = two.join("data.toml");
    let written = fs::read_to_string(&manifest)?;
    fs::write(&manifest, format!("{written}{CLEAN}"))?;
    let output = building_in(&sandbox, &two, &["--offline", "build"])?
        .env("FETCHLOOM_CACHE", &cache)
        .output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    assert_eq!(runs(&two)?, 1);
    assert_eq!(
        fs::read(two.join("inputs").join("clean").join("iris.csv"))?,
        IRIS
    );
    Ok(())
}

/// a file of the pinned size holding other bytes is never taken or changed, so the push fails
/// as it would with no file there.
#[test]
fn an_edit_of_the_same_size_is_never_taken() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    project(&sandbox, &server, false)?;
    let (two, cache) = copied(&sandbox, "two")?;
    let placed = two.join("inputs").join("iris").join("iris.csv");
    delete(&placed)?;
    fs::write(&placed, EDITED)?;
    let before = look(&placed)?;
    let mirror = sandbox.folder("lab")?.join("mirror");
    let output = sandbox
        .command_in(&two, &["--json", "--yes", "push", "--to"])
        .arg(&mirror)
        .env("FETCHLOOM_CACHE", &cache)
        .output()?;
    assert_eq!(output.status.code(), Some(20), "{}", text(&output.stdout)?);
    let document = json(&output)?;
    let error = &document["errors"][0];
    assert_eq!(error["kind"], "offline.missing", "{document}");
    assert_eq!(
        error["message"],
        "this machine's cache lacks files the push needs, and push never downloads"
    );
    assert_eq!(document["work"]["data_folder_objects"], 0, "{document}");
    assert_eq!(look(&placed)?, before);
    assert!(!object(&cache, IRIS).exists());
    assert!(!object(&cache, EDITED).exists());
    Ok(())
}

/// `cp -a` keeps a hardlink between two names of one file, so an edit made in place through one
/// name is the other's too, and neither holds the pinned bytes, though both have their size: the
/// dry run promises no copy, and the push, as it, stops with what is missing and changes nothing.
#[test]
fn a_dry_run_promises_no_copy_of_an_edit_two_names_share() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    project(&sandbox, &server, true)?;
    let (two, cache) = copied(&sandbox, "two")?;
    let (raw, made) = (
        two.join("inputs").join("iris").join("iris.csv"),
        two.join("inputs").join("clean").join("iris.csv"),
    );
    // a drive without hardlinks, as fat, gets the edit at both names instead
    if sandbox.links()?.hardlink {
        delete(&made)?;
        fs::hard_link(&raw, &made)?;
        edit(&raw, EDITED)?;
    } else {
        edit(&raw, EDITED)?;
        edit(&made, EDITED)?;
    }
    let before = (look(&raw)?, look(&made)?);
    let mirror = sandbox.folder("lab")?.join("mirror");
    let push = |args: &[&str]| {
        sandbox
            .command_in(&two, args)
            .arg("--to")
            .arg(&mirror)
            .env("FETCHLOOM_CACHE", &cache)
            .output()
    };
    let missing =
        "error: this machine's cache lacks files the push needs, and push never downloads";
    let dry = push(&["--yes", "push", "--dry-run"])?;
    let stderr = text(&dry.stderr)?;
    assert_eq!(dry.status.code(), Some(20), "{stderr}");
    assert!(stderr.contains(missing), "{stderr}");
    assert!(!stderr.contains("Would copy"), "{stderr}");
    let pushed = push(&["--yes", "push"])?;
    let stderr = text(&pushed.stderr)?;
    assert_eq!(pushed.status.code(), Some(20), "{stderr}");
    assert!(stderr.contains(missing), "{stderr}");
    assert!(!stderr.contains("Copied"), "{stderr}");
    assert!(!mirror.exists());
    assert!(!object(&cache, IRIS).exists() && !object(&cache, EDITED).exists());
    assert_eq!((look(&raw)?, look(&made)?), before);
    Ok(())
}

/// a file the cache lacks whose bytes the data folder holds at another path is placed from
/// there, before any mirror is asked, so a result needs no build: status lists it as missing
/// rather than unbuilt, a dry run would place it, and sync places it.
#[test]
fn a_file_the_data_folder_holds_elsewhere_is_placed_from_there() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    project(&sandbox, &server, true)?;
    let (two, cache) = copied(&sandbox, "two")?;
    let made = two.join("inputs").join("clean").join("iris.csv");
    delete(&made)?;
    let mirror = sandbox.folder("mirror")?;
    fs::write(mirror.join("fetchloom-mirror"), "fetchloom mirror 1\n")?;
    let mirrored = object(&mirror, IRIS);
    fs::create_dir_all(mirrored.parent().ok_or("a fan folder")?)?;
    fs::write(&mirrored, IRIS)?;
    let run = |args: &[&str]| -> Result<serde_json::Value> {
        let output = sandbox
            .command_in(&two, args)
            .env("FETCHLOOM_CACHE", &cache)
            .env("FETCHLOOM_MIRRORS", &mirror)
            .output()?;
        assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
        json(&output)
    };
    let status = run(&["--json", "status"])?;
    assert_eq!(
        status["result"]["missing"],
        serde_json::json!(["inputs/clean/iris.csv"])
    );
    assert_eq!(status["result"]["unbuilt"], serde_json::json!([]));
    let dry = run(&["--json", "--offline", "sync", "--dry-run"])?;
    assert_eq!(dry["result"]["placement"]["placed"], 1, "{dry}");
    assert_eq!(dry["result"]["changes"]["download"], serde_json::json!([]));
    let synced = run(&["--json", "--offline", "sync"])?;
    assert_eq!(synced["result"]["placement"]["placed"], 1, "{synced}");
    assert_eq!(synced["work"]["data_folder_objects"], 1, "{synced}");
    assert_eq!(synced["work"]["mirror_objects"], 0, "{synced}");
    assert_eq!(fs::read(&made)?, IRIS);
    unlinked(&two.join("inputs").join("iris").join("iris.csv"), &cache)?;
    Ok(())
}
