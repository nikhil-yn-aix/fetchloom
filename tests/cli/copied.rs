//! a project copied with its data folder to a machine whose cache never held its files: what the
//! data folder holds at the paths data.lock places is taken back into the cache, after a check of
//! its bytes, so push, build and sync need no request and no source.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::build::{build, building_in, runs};
use crate::project::{IRIS, PATH, added};
use crate::support::{Result, Sandbox, Server, delete, edit, file, json, sha256_hex, text};

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
    let hex = sha256_hex(bytes);
    root.join("objects").join(&hex[..2]).join(&hex[2..])
}

/// the file at `path` is no other name of the object `cache` keeps, so an edit of one never
/// reaches the other.
fn unlinked(path: &Path, cache: &Path) -> Result {
    let kept = object(cache, IRIS);
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let (file, kept) = (fs::metadata(path)?, fs::metadata(&kept)?);
        assert_eq!(file.nlink(), 1, "{}", path.display());
        assert_ne!(file.ino(), kept.ino(), "{}", path.display());
    }
    #[cfg(windows)]
    {
        let information = |path: &Path| winapi_util::file::information(fs::File::open(path)?);
        let (file, kept) = (information(path)?, information(&kept)?);
        assert_eq!(file.number_of_links(), 1, "{}", path.display());
        assert_ne!(file.file_index(), kept.file_index(), "{}", path.display());
    }
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

/// a file of the pinned size holding other bytes is never taken or changed, so the push
/// downloads the pinned bytes, as it would with no file there.
#[test]
fn an_edit_of_the_same_size_is_never_taken() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), file(IRIS, "\"v1\"")])?;
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
    let document = json(&output)?;
    assert_eq!(output.status.code(), Some(0), "{document}");
    assert_eq!(document["work"]["data_folder_objects"], 0, "{document}");
    assert_eq!(document["work"]["requests"], 1, "{document}");
    assert_eq!(look(&placed)?, before);
    assert_eq!(fs::read(object(&mirror, IRIS))?, IRIS);
    assert!(!object(&cache, EDITED).exists());
    Ok(())
}

/// `cp -a` keeps a hardlink between two names of one file, so an edit made in place through one
/// name is the other's too, and neither holds the pinned bytes, though both have their size: the
/// dry run promises no copy and names the download instead, and the push downloads the pinned
/// bytes, changing neither name.
#[test]
fn a_dry_run_promises_no_copy_of_an_edit_two_names_share() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), file(IRIS, "\"v1\"")])?;
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
    let dry = push(&["--yes", "push", "--dry-run"])?;
    let stderr = text(&dry.stderr)?;
    assert_eq!(dry.status.code(), Some(0), "{stderr}");
    let fetch = format!("Would fetch iris/iris.csv \u{b7} {} B\n", IRIS.len());
    assert!(stderr.contains(&fetch), "{stderr}");
    assert!(!stderr.contains("Would copy"), "{stderr}");
    assert!(!mirror.exists());
    let pushed = push(&["--yes", "push"])?;
    let stderr = text(&pushed.stderr)?;
    assert_eq!(pushed.status.code(), Some(0), "{stderr}");
    assert!(stderr.contains(" Downloaded 1 file"), "{stderr}");
    assert_eq!(fs::read(object(&mirror, IRIS))?, IRIS);
    assert!(!object(&cache, EDITED).exists());
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

/// a project whose dataset is a zip, settled, then copied to `two` with an empty cache, which
/// knows neither the zip nor what it holds, and the server that serves the zip `downloads`
/// times, the add's included.
fn copied_zip(sandbox: &Sandbox, downloads: usize) -> Result<(Server, PathBuf, PathBuf)> {
    let zip = fs::read(crate::support::workspace().join("tests/fixtures/archives/python.zip"))?;
    let responses = (0..downloads).map(|_| file(&zip, "\"z1\"")).collect();
    let server = Server::start("/files/dataset.zip", responses)?;
    sandbox.allow(&server.url);
    let added = sandbox.run(&["add", &server.url])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let (two, cache) = copied(sandbox, "two")?;
    Ok((server, two, cache))
}

/// a copy whose cache has no list of what its zip holds cannot tell the zip's files from others:
/// status says they are not checked, never that the zip, which no sync places, is missing, nor
/// that the files it placed are extra, and writes nothing. a file of the person's own beside them
/// is still extra.
#[test]
fn status_of_a_copied_zip_says_its_files_are_not_checked() -> Result {
    let sandbox = Sandbox::new()?;
    let (_server, two, cache) = copied_zip(&sandbox, 1)?;
    // an edit of data.toml unsettles the copy, so status compares each file
    let manifest = two.join("data.toml");
    let written = fs::read_to_string(&manifest)?;
    fs::write(
        &manifest,
        format!("{written}\n# copied to another machine\n"),
    )?;
    fs::write(
        two.join("data").join("dataset").join("notes.txt"),
        b"mine\n",
    )?;
    let state = fs::read(two.join(".fetchloom").join("state"))?;
    let status = |args: &[&str]| {
        sandbox
            .command_in(&two, args)
            .env("FETCHLOOM_CACHE", &cache)
            .output()
    };
    let document = json(&status(&["--json", "status"])?)?;
    let result = &document["result"];
    assert_eq!(
        result["not_unpacked"],
        serde_json::json!(["data/dataset/dataset.zip"])
    );
    assert_eq!(
        (&result["extra"], &result["missing"]),
        (
            &serde_json::json!(["data/dataset/notes.txt"]),
            &serde_json::json!([])
        )
    );
    let output = status(&["status"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    assert_eq!(
        text(&output.stdout)?,
        "      Extra data/dataset/notes.txt\n  Unchecked data/dataset/dataset.zip · this machine has no list of the files it holds, so they are not checked\n"
    );
    assert_eq!(fs::read(two.join(".fetchloom").join("state"))?, state);
    Ok(())
}

/// push downloads the pinned zip the copy's cache lacks, as a sync would, says so, and pushes
/// what it holds with their list, so a fresh clone then syncs from the mirror alone. no file or
/// pin of the copy changes.
#[test]
fn a_copied_project_pushes_once_it_fetched_what_its_cache_lacks() -> Result {
    let sandbox = Sandbox::new()?;
    let (server, two, cache) = copied_zip(&sandbox, 2)?;
    let lock = fs::read(two.join("data.lock"))?;
    let readme = two.join("data").join("dataset").join("README.md");
    let before = look(&readme)?;
    let mirror = sandbox.folder("lab")?.join("mirror");
    let pushed = sandbox
        .command_in(&two, &["--yes", "push", "--to"])
        .arg(&mirror)
        .env("FETCHLOOM_CACHE", &cache)
        .output()?;
    let stderr = text(&pushed.stderr)?;
    assert_eq!(pushed.status.code(), Some(0), "{stderr}");
    assert!(stderr.contains(" Downloaded 1 file"), "{stderr}");
    assert_eq!(fs::read(two.join("data.lock"))?, lock);
    assert_eq!(look(&readme)?, before);
    let asked = server.requests().len();
    let fresh = sandbox.folder("fresh")?;
    for name in ["data.toml", "data.lock"] {
        fs::copy(two.join(name), fresh.join(name))?;
    }
    let synced = sandbox
        .command_in(&fresh, &["--json", "sync", "--locked"])
        .env("FETCHLOOM_CACHE", sandbox.folder("fresh cache")?)
        .env("FETCHLOOM_MIRRORS", &mirror)
        .env("FETCHLOOM_MIRRORS_ONLY", "1")
        .output()?;
    let document = json(&synced)?;
    assert_eq!(synced.status.code(), Some(0), "{document}");
    assert_eq!(document["work"]["requests"], 0, "{document}");
    assert_eq!(server.requests().len(), asked, "the source is never asked");
    assert_eq!(
        fs::read(fresh.join("data").join("dataset").join("README.md"))?,
        before.0
    );
    Ok(())
}

/// offline, push cannot fetch what its cache lacks, so it stops before writing anything, naming
/// the zip, with help that works on the next push: run it online. a sync would have nothing to
/// do here, so it is never the fix.
#[test]
fn an_offline_push_says_to_run_it_online() -> Result {
    let sandbox = Sandbox::new()?;
    let (server, two, cache) = copied_zip(&sandbox, 1)?;
    let mirror = sandbox.folder("lab")?.join("mirror");
    let output = sandbox
        .command_in(&two, &["--json", "--offline", "--yes", "push", "--to"])
        .arg(&mirror)
        .env("FETCHLOOM_CACHE", &cache)
        .output()?;
    let document = json(&output)?;
    assert_eq!(output.status.code(), Some(20), "{document}");
    let error = &document["errors"][0];
    assert_eq!(error["kind"], "offline.missing", "{document}");
    assert_eq!(error["details"], serde_json::json!(["dataset/dataset.zip"]));
    assert!(
        error["help"]
            .as_str()
            .is_some_and(|help| help.starts_with("run it again without --offline")),
        "{error}"
    );
    assert!(error["fix"].is_null(), "{error}");
    assert_eq!(server.requests().len(), 1, "only the add downloaded");
    assert!(!mirror.exists());
    Ok(())
}

/// a recipe reads iris, which `place = false` keeps out of the data folder, so a copy of the
/// project to another cache has iris nowhere on this machine, and no sync fetches it. `push
/// --inputs` downloads it from its source and pushes it with the result the data folder holds.
#[test]
fn push_inputs_fetches_an_input_no_sync_places() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let manifest = sandbox.work().join("data.toml");
    let written = fs::read_to_string(&manifest)?;
    fs::write(
        &manifest,
        format!(
            "{}\n[datasets.clean]\nfrom = [\"iris\"]\nrun = [\"recipe-step\", \"write\", \"clean.csv\", \"done\", \"{{iris}}\", \"{{out}}\"]\n",
            written.replacen("[datasets.iris]\n", "[datasets.iris]\nplace = false\n", 1)
        ),
    )?;
    let built = build(&sandbox, &["build"])?;
    assert_eq!(built.status.code(), Some(0), "{}", text(&built.stderr)?);
    assert!(!sandbox.work().join("data").join("iris").exists());
    let (two, cache) = copied(&sandbox, "two")?;
    let mirror = sandbox.folder("lab")?.join("mirror");
    let pushed = sandbox
        .command_in(&two, &["--json", "--yes", "push", "--inputs", "--to"])
        .arg(&mirror)
        .env("FETCHLOOM_CACHE", &cache)
        .output()?;
    let document = json(&pushed)?;
    assert_eq!(pushed.status.code(), Some(0), "{document}");
    assert_eq!(document["work"]["requests"], 1, "{document}");
    assert_eq!(fs::read(object(&mirror, IRIS))?, IRIS);
    assert_eq!(fs::read(object(&mirror, b"done"))?, b"done");
    Ok(())
}

/// the files below `folder`, in every folder inside it, or none when it is not there.
fn files_below(folder: &Path) -> Result<usize> {
    let Ok(entries) = fs::read_dir(folder) else {
        return Ok(0);
    };
    let mut count = 0;
    for entry in entries {
        let entry = entry?;
        count += if entry.file_type()?.is_dir() {
            files_below(&entry.path())?
        } else {
            1
        };
    }
    Ok(count)
}

/// push asks about data whose source did not say who may read it before it downloads what its
/// cache lacks, so a push nobody says yes to costs no request and leaves the cache with nothing
/// new.
#[test]
fn a_push_asks_before_it_downloads_anything() -> Result {
    let sandbox = Sandbox::new()?;
    let (server, two, cache) = copied_zip(&sandbox, 2)?;
    let mirror = sandbox.folder("lab")?.join("mirror");
    let output = sandbox
        .command_in(&two, &["--json", "push", "--to"])
        .arg(&mirror)
        .env("FETCHLOOM_CACHE", &cache)
        .output()?;
    let document = json(&output)?;
    assert_eq!(output.status.code(), Some(1), "{document}");
    assert_eq!(document["errors"][0]["kind"], "unconfirmed", "{document}");
    assert_eq!(server.requests().len(), 1, "only the add downloaded");
    for folder in ["objects", "unpacked", "partial"] {
        assert_eq!(files_below(&cache.join(folder))?, 0, "{folder}");
    }
    assert!(!mirror.exists());
    Ok(())
}
