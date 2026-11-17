//! a project copied with its data folder to a machine whose cache never held its files: the data
//! folder's bytes, once checked, go back into the cache, so push and sync need no request.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::project::{IRIS, PATH, added};
use crate::support::{Result, Sandbox, Server, delete, edit, file, json, sha256_hex, text};

/// iris with one byte changed: the size data.lock pins, other bytes.
const EDITED: &[u8] = b"sepal_length,species\n5.2,setosa\n";

/// a project in the work folder whose data folder is `inputs`, with iris added from `server`.
fn project(sandbox: &Sandbox, server: &Server) -> Result {
    let manifest = sandbox.work().join("data.toml");
    fs::write(&manifest, "[project]\ndata_dir = \"inputs\"\n")?;
    added(sandbox, server)?;
    Ok(())
}

/// [`project`] with twin added after iris from the server it returns, which holds iris's bytes
/// too: one object at two paths.
fn twice(sandbox: &Sandbox, server: &Server) -> Result<Server> {
    project(sandbox, server)?;
    let twin = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    sandbox.allow(&twin.url);
    let output = sandbox.run(&["add", &twin.url, "--as", "twin"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    Ok(twin)
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
pub(crate) fn copy_tree(from: &Path, to: &Path) -> Result {
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

/// the bytes go through the cache to the mirror, leaving the data folder's files as they were,
/// and the dry run says so and writes nothing.
#[test]
fn a_copied_project_pushes_from_its_data_folder_with_no_request() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    let twin = twice(&sandbox, &server)?;
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
    assert_eq!(twin.requests().len(), 1, "only the add asked twin's source");
    assert_eq!(look(&placed)?, before);
    unlinked(&placed, &cache)?;
    unlinked(&two.join("inputs").join("twin").join("iris.csv"), &cache)?;
    Ok(())
}

/// every file of a copy holds the pinned bytes, so status finds no difference and says in one
/// line that a sync records them, as the sync then does, after which status has nothing to say.
#[test]
fn a_copied_project_differs_in_nothing_until_a_sync_records_it() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    project(&sandbox, &server)?;
    let (two, cache) = copied(&sandbox, "two")?;
    let run = |args: &[&str]| {
        sandbox
            .command_in(&two, args)
            .env("FETCHLOOM_CACHE", &cache)
            .output()
    };
    let ok = "         Ok everything matches data.lock by size and time\n";
    let status = run(&["status"])?;
    assert_eq!(status.status.code(), Some(0), "{}", text(&status.stderr)?);
    assert_eq!(text(&status.stdout)?, "");
    assert_eq!(
        text(&status.stderr)?,
        format!(
            "   In place 1 file already holds the pinned bytes; `fetchloom sync` records it as placed\n{ok}"
        )
    );
    let document = json(&run(&["--json", "status"])?)?;
    assert_eq!(
        document["result"]["adoptable"],
        serde_json::json!(["inputs/iris/iris.csv"])
    );
    assert_eq!(document["result"]["untracked"], serde_json::json!([]));
    let synced = run(&["sync"])?;
    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    assert_eq!(text(&run(&["status"])?.stderr)?, ok);
    Ok(())
}

/// the push leaves the edit as it is and downloads the pinned bytes, as with no file there.
#[test]
fn an_edit_of_the_same_size_is_never_taken() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), file(IRIS, "\"v1\"")])?;
    project(&sandbox, &server)?;
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

/// `cp -a` keeps a hardlink, so an edit in place through one name reaches the other; the dry run
/// names the download instead, and the push changes neither name.
#[test]
fn a_dry_run_promises_no_copy_of_an_edit_two_names_share() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), file(IRIS, "\"v1\"")])?;
    let _twin = twice(&sandbox, &server)?;
    let (two, cache) = copied(&sandbox, "two")?;
    let (raw, made) = (
        two.join("inputs").join("iris").join("iris.csv"),
        two.join("inputs").join("twin").join("iris.csv"),
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

/// the data folder is asked before any mirror: status calls the file missing, and a dry run and
/// sync place it offline from the other path that holds its bytes.
#[test]
fn a_file_the_data_folder_holds_elsewhere_is_placed_from_there() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    let _twin = twice(&sandbox, &server)?;
    let (two, cache) = copied(&sandbox, "two")?;
    let made = two.join("inputs").join("twin").join("iris.csv");
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
        serde_json::json!(["inputs/twin/iris.csv"])
    );
    let dry = run(&["--json", "--offline", "sync", "--dry-run"])?;
    assert_eq!(dry["result"]["placement"]["placed"], 1, "{dry}");
    assert_eq!(dry["result"]["changes"]["download"], serde_json::json!([]));
    let synced = run(&["--json", "--offline", "sync"])?;
    assert_eq!(synced["result"]["placement"]["placed"], 1, "{synced}");
    assert_eq!(synced["work"]["data_folder_objects"], 1, "{synced}");
    assert_eq!(synced["work"]["mirror_objects"], 0, "{synced}");
    assert_eq!(fs::read(&made)?, IRIS);
    Ok(())
}

/// a settled project of one zip in the work folder, and the server that serves the zip
/// `downloads` times, the add's included.
fn zipped(sandbox: &Sandbox, downloads: usize) -> Result<Server> {
    archived(sandbox, "python.zip", "dataset.zip", downloads)
}

/// the bytes of the archive fixture `fixture`.
fn fixture(fixture: &str) -> Result<Vec<u8>> {
    let path = crate::support::workspace().join("tests/fixtures/archives");
    Ok(fs::read(path.join(fixture))?)
}

/// a settled project of the archive fixture `fixture`, served as `name`, in the work folder, and
/// the server that serves it `downloads` times, the add's included.
fn archived(sandbox: &Sandbox, fixture: &str, name: &str, downloads: usize) -> Result<Server> {
    let bytes = self::fixture(fixture)?;
    let responses = (0..downloads).map(|_| file(&bytes, "\"z1\"")).collect();
    let server = Server::start(&format!("/files/{name}"), responses)?;
    sandbox.allow(&server.url);
    let added = sandbox.run(&["add", &server.url])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    Ok(server)
}

/// [`zipped`] copied to `two` with an empty cache, which knows neither the zip nor what it holds.
fn copied_zip(sandbox: &Sandbox, downloads: usize) -> Result<(Server, PathBuf, PathBuf)> {
    let server = zipped(sandbox, downloads)?;
    let (two, cache) = copied(sandbox, "two")?;
    Ok((server, two, cache))
}

/// a copy carries the .gitignore each dataset folder keeps out of git with, and the record that
/// fetchloom made it: a removal of another dataset there, whose cache never held the zip, cannot
/// list the zip's files yet, and keeps that folder's marker all the same.
#[test]
fn a_copy_keeps_the_marker_of_a_folder_it_cannot_list() -> Result {
    let sandbox = Sandbox::new()?;
    let _server = zipped(&sandbox, 1)?;
    let iris = Server::start("/files/iris.csv", vec![file(b"a,b\n1,2\n", "\"i1\"")])?;
    sandbox.allow(&iris.url);
    let added = sandbox.run(&["add", &iris.url])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let (two, cache) = copied(&sandbox, "two")?;
    let marker = two.join("data").join("dataset").join(".gitignore");
    assert_eq!(fs::read(&marker)?, b"*\n");
    let removed = sandbox
        .command_in(&two, &["remove", "iris"])
        .env("FETCHLOOM_CACHE", &cache)
        .output()?;
    assert_eq!(removed.status.code(), Some(0), "{}", text(&removed.stderr)?);
    assert_eq!(fs::read(&marker)?, b"*\n", "{}", text(&removed.stderr)?);
    let status = sandbox
        .command_in(&two, &["--json", "status"])
        .env("FETCHLOOM_CACHE", &cache)
        .output()?;
    let document = json(&status)?;
    assert_eq!(
        document["result"]["yours"],
        serde_json::json!([]),
        "{document}"
    );
    Ok(())
}

/// with no list of the zip's files, status calls neither the zip missing nor its files extra, and
/// writes nothing; a file of the person's own beside them is theirs, kept. the list is missing for
/// a second person sharing the folder, whose cache never held the zip. a copy would not do: its
/// state names the folder it came from, so it is not used, and no file has a record.
#[test]
fn status_of_a_zip_the_cache_never_held_says_its_files_are_not_checked() -> Result {
    let sandbox = Sandbox::new()?;
    let _server = zipped(&sandbox, 1)?;
    let (two, cache) = (sandbox.work(), sandbox.folder("their cache")?);
    // an edit of data.toml unsettles the project, so status compares each file
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
        (&result["yours"], &result["missing"]),
        (
            &serde_json::json!(["data/dataset/notes.txt"]),
            &serde_json::json!([])
        )
    );
    let output = status(&["status"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    assert_eq!(
        text(&output.stdout)?,
        "  Unchecked data/dataset/dataset.zip · this machine has no list of the files it holds, so they are not checked\n"
    );
    assert_eq!(fs::read(two.join(".fetchloom").join("state"))?, state);
    Ok(())
}

/// in a copy no file has a record, so a file of the person's own beside the zip's keeps its
/// folder from proving the tree data.lock pins, and nothing tells the zip's files from others
/// there: status calls none of them the person's, as it says it does not check them. a file
/// outside every dataset's folder is none of status's business.
#[test]
fn status_of_a_copy_whose_cache_never_held_the_zip_calls_none_of_its_files_extra() -> Result {
    let sandbox = Sandbox::new()?;
    let (_server, two, cache) = copied_zip(&sandbox, 1)?;
    fs::write(two.join("data").join("notes.txt"), b"mine\n")?;
    fs::write(
        two.join("data").join("dataset").join("notes.txt"),
        b"mine\n",
    )?;
    let status = |args: &[&str]| {
        sandbox
            .command_in(&two, args)
            .env("FETCHLOOM_CACHE", &cache)
            .output()
    };
    let document = json(&status(&["--json", "status"])?)?;
    let result = &document["result"];
    assert_eq!(
        (
            &result["not_unpacked"],
            &result["extra"],
            &result["unneeded"]
        ),
        (
            &serde_json::json!(["data/dataset/dataset.zip"]),
            &serde_json::json!([]),
            &serde_json::json!([])
        ),
        "{document}"
    );
    assert_eq!(result["yours"], serde_json::json!([]), "{document}");
    let output = status(&["status"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    assert_eq!(
        text(&output.stdout)?,
        "  Unchecked data/dataset/dataset.zip · this machine has no list of the files it holds, so they are not checked\n"
    );
    Ok(())
}

/// a clone whose cache never held the zip downloads it first, so status counts it among what a
/// sync would download, as the dry run does.
#[test]
fn status_counts_a_zip_this_cache_lacks_among_the_downloads() -> Result {
    let sandbox = Sandbox::new()?;
    let _server = zipped(&sandbox, 1)?;
    let fresh = crate::project::clone(&sandbox, "fresh")?;
    let cache = sandbox.folder("fresh cache")?;
    let run = |args: &[&str]| {
        sandbox
            .command_in(&fresh, args)
            .env("FETCHLOOM_CACHE", &cache)
            .output()
    };
    let status = json(&run(&["--json", "status"])?)?;
    let dry = json(&run(&["--json", "sync", "--dry-run"])?)?;
    let (counted, planned) = (
        &status["result"]["to_download"],
        &dry["result"]["changes"]["to_download"],
    );
    assert_eq!(counted["files"], 1, "{status}");
    assert_eq!(
        (&counted["files"], &counted["bytes"]),
        (&planned["files"], &planned["bytes"]),
        "{status}\n{dry}"
    );
    Ok(())
}

/// the zip's files go to the mirror with their list, so a fresh clone syncs from it alone, and no
/// file or pin of the copy changes.
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

/// the push stops before writing anything, naming the zip; a sync would have nothing to do here,
/// so it is never the fix.
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

/// how many files are below `folder` at any depth, or 0 when it is not there.
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

/// the question about data of unknown access comes first, so a push nobody says yes to costs no
/// request and adds nothing to the cache.
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

/// the bytes of each file below `folder`, by its path there with `/` between folders.
fn contents(folder: &Path) -> Result<Vec<(String, Vec<u8>)>> {
    let mut found = Vec::new();
    let mut folders = vec![folder.to_path_buf()];
    while let Some(at) = folders.pop() {
        for entry in fs::read_dir(&at)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                folders.push(entry.path());
                continue;
            }
            let path = entry.path();
            let relative = path
                .strip_prefix(folder)?
                .to_string_lossy()
                .replace('\\', "/");
            found.push((relative, fs::read(&path)?));
        }
    }
    found.sort();
    Ok(found)
}

/// the files the zip unpacked to in the copy's dataset folder, as placing put them there.
const ZIP_FILES: [&str; 5] = [
    "data/dataset/README.md",
    "data/dataset/data/caf\u{e9}.csv",
    "data/dataset/data/empty.txt",
    "data/dataset/data/train.csv",
    "data/dataset/long/a-file-name-longer-than-one-hundred-bytes-so-old-tar-headers-cannot-hold-it-and-pax-or-gnu-records-must.txt",
];

/// a copy's zip whose files sit where the zip unpacked them is checked against the tree data.lock
/// pins and taken as placed: the sync sends no request and leaves every file as it was, the zip
/// stays out of the cache, and status then has nothing to say.
#[test]
fn a_copied_zip_folder_is_checked_in_place_and_never_downloaded() -> Result {
    let sandbox = Sandbox::new()?;
    let (server, two, cache) = copied_zip(&sandbox, 1)?;
    let run = |args: &[&str]| {
        sandbox
            .command_in(&two, args)
            .env("FETCHLOOM_CACHE", &cache)
            .output()
    };
    let folder = two.join("data").join("dataset");
    let before = contents(&folder)?;
    let synced = run(&["--json", "sync"])?;
    let document = json(&synced)?;
    assert_eq!(synced.status.code(), Some(0), "{document}");
    assert_eq!(document["work"]["bytes_down"], 0, "{document}");
    assert_eq!(server.requests().len(), 1, "the add's download alone");
    assert_eq!(contents(&folder)?, before);
    assert!(!object(&cache, &fixture("python.zip")?).exists());
    let status = run(&["status"])?;
    assert_eq!(
        text(&status.stderr)?,
        "         Ok everything matches data.lock by size and time\n"
    );
    Ok(())
}

/// offline, as on a compute node, `fetchloom path` answers a copy's zip folder that holds what
/// data.lock pins, and a sync takes it as placed, though the cache never held the zip.
#[test]
fn offline_a_copied_zip_folder_answers_path_and_syncs() -> Result {
    let sandbox = Sandbox::new()?;
    let (server, two, cache) = copied_zip(&sandbox, 1)?;
    let run = |args: &[&str]| {
        sandbox
            .command_in(&two, args)
            .env("FETCHLOOM_CACHE", &cache)
            .output()
    };
    let path = run(&["--offline", "path", "dataset/data/train.csv"])?;
    assert_eq!(path.status.code(), Some(0), "{}", text(&path.stderr)?);
    let folder = two.join("data").join("dataset");
    assert_eq!(
        text(&path.stdout)?,
        format!("{}\n", folder.join("data").join("train.csv").display())
    );
    let synced = run(&["--offline", "sync"])?;
    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    assert_eq!(server.requests().len(), 1, "the add's download alone");
    Ok(())
}

/// status checks a copy's zip folder in place: its files hold what data.lock pins, so it names
/// no zip unchecked and counts no download, and says in one line that a sync records them, as a
/// dry run does.
#[test]
fn status_of_a_copied_zip_folder_finds_its_files_in_place() -> Result {
    let sandbox = Sandbox::new()?;
    let (_server, two, cache) = copied_zip(&sandbox, 1)?;
    let run = |args: &[&str]| {
        sandbox
            .command_in(&two, args)
            .env("FETCHLOOM_CACHE", &cache)
            .output()
    };
    let output = run(&["status"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    assert_eq!(text(&output.stdout)?, "");
    assert_eq!(
        text(&output.stderr)?,
        "   In place 5 files already hold the pinned bytes; `fetchloom sync` records them as placed\n         Ok everything matches data.lock by size and time\n"
    );
    let document = json(&run(&["--json", "status"])?)?;
    let result = &document["result"];
    assert_eq!(result["not_unpacked"], serde_json::json!([]), "{document}");
    assert_eq!(
        result["adoptable"],
        serde_json::json!(ZIP_FILES),
        "{document}"
    );
    assert_eq!(result["to_download"]["files"], 0, "{document}");
    let dry = json(&run(&["--json", "sync", "--dry-run"])?)?;
    let changes = &dry["result"]["changes"];
    assert_eq!(changes["download"], serde_json::json!([]), "{dry}");
    assert_eq!(changes["unpack"], serde_json::json!([]), "{dry}");
    assert!(
        !cache.join("unpacked").exists(),
        "a dry run records nothing"
    );
    Ok(())
}

/// one file of the copy's zip folder that holds other bytes fails the check, since only the
/// whole tree data.lock pins proves the files and it cannot say which one differs: the zip
/// downloads whole, the other files are taken as placed, and the changed one stays, as a file
/// fetchloom did not place does, until `sync --force`. offline, the sync stops as before.
#[test]
fn a_changed_file_of_a_copied_zip_folder_brings_the_zip_again() -> Result {
    let sandbox = Sandbox::new()?;
    let (server, two, cache) = copied_zip(&sandbox, 2)?;
    let run = |args: &[&str]| {
        sandbox
            .command_in(&two, args)
            .env("FETCHLOOM_CACHE", &cache)
            .output()
    };
    let changed = two
        .join("data")
        .join("dataset")
        .join("data")
        .join("train.csv");
    edit(&changed, b"a,b\n9,9\n")?;
    let status = json(&run(&["--json", "status"])?)?;
    assert_eq!(
        status["result"]["not_unpacked"],
        serde_json::json!(["data/dataset/dataset.zip"]),
        "{status}"
    );
    let offline = run(&["--json", "--offline", "sync"])?;
    let document = json(&offline)?;
    assert_eq!(offline.status.code(), Some(20), "{document}");
    assert_eq!(
        document["errors"][0]["kind"], "offline.missing",
        "{document}"
    );
    let synced = run(&["sync"])?;
    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(0), "{stderr}");
    assert_eq!(server.requests().len(), 2, "the zip again: {stderr}");
    assert!(
        stderr.contains("warning: data/dataset/data/train.csv holds other bytes than data.lock pins, and fetchloom did not place it, so it stays"),
        "{stderr}"
    );
    assert_eq!(fs::read(&changed)?, b"a,b\n9,9\n");
    Ok(())
}

/// a copy's tar.gz folder is checked in place as a zip's is.
#[test]
fn a_copied_tar_folder_is_checked_in_place_too() -> Result {
    let sandbox = Sandbox::new()?;
    let server = archived(&sandbox, "python.tar.gz", "dataset.tar.gz", 1)?;
    let (two, cache) = copied(&sandbox, "two")?;
    let synced = sandbox
        .command_in(&two, &["--json", "--offline", "sync"])
        .env("FETCHLOOM_CACHE", &cache)
        .output()?;
    let document = json(&synced)?;
    assert_eq!(synced.status.code(), Some(0), "{document}");
    assert_eq!(server.requests().len(), 1, "the add's download alone");
    Ok(())
}

/// once the check took the files as placed, the zip stays out of the cache until a file of it is
/// needed again: a file deleted after the check brings the zip, which puts it back.
#[test]
fn a_file_gone_after_the_check_brings_the_zip() -> Result {
    let sandbox = Sandbox::new()?;
    let (server, two, cache) = copied_zip(&sandbox, 2)?;
    let run = |args: &[&str]| {
        sandbox
            .command_in(&two, args)
            .env("FETCHLOOM_CACHE", &cache)
            .output()
    };
    let synced = run(&["sync"])?;
    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    assert_eq!(server.requests().len(), 1, "the add's download alone");
    let readme = two.join("data").join("dataset").join("README.md");
    let bytes = fs::read(&readme)?;
    delete(&readme)?;
    let again = run(&["sync"])?;
    assert_eq!(again.status.code(), Some(0), "{}", text(&again.stderr)?);
    assert_eq!(
        server.requests().len(),
        2,
        "the zip, once a file of it is needed"
    );
    assert_eq!(fs::read(&readme)?, bytes);
    Ok(())
}

/// a push from a copy whose zip folder the check took sends the zip's files from the data folder,
/// with the list of them the check recorded, and asks the source nothing.
#[test]
fn a_checked_zip_folder_pushes_its_files_and_their_list() -> Result {
    let sandbox = Sandbox::new()?;
    let (server, two, cache) = copied_zip(&sandbox, 1)?;
    let run = |args: &[&str]| {
        sandbox
            .command_in(&two, args)
            .env("FETCHLOOM_CACHE", &cache)
            .output()
    };
    let synced = run(&["sync"])?;
    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    let mirror = sandbox.folder("lab")?.join("mirror");
    let pushed = sandbox
        .command_in(&two, &["--json", "--yes", "push", "--to"])
        .arg(&mirror)
        .env("FETCHLOOM_CACHE", &cache)
        .output()?;
    let document = json(&pushed)?;
    assert_eq!(pushed.status.code(), Some(0), "{document}");
    assert_eq!(document["work"]["requests"], 0, "{document}");
    assert_eq!(server.requests().len(), 1, "the add's download alone");
    let readme = fs::read(two.join("data").join("dataset").join("README.md"))?;
    assert_eq!(fs::read(object(&mirror, &readme))?, readme);
    assert_eq!(files_below(&mirror.join("trees"))?, 1, "the zip's list");
    Ok(())
}

/// a zip beside another file of its dataset keeps the top folder its files share, and its folder
/// there is checked in place as a lone zip's is: offline, nothing is asked of the source.
#[test]
fn a_copied_zip_beside_other_files_is_checked_in_place() -> Result {
    let sandbox = Sandbox::new()?;
    let html = "<!DOCTYPE HTML>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<title>Directory listing for /set/</title>\n</head>\n<body>\n<h1>Directory listing for /set/</h1>\n<hr>\n<ul>\n<li><a href=\"flowers.zip\">flowers.zip</a></li>\n<li><a href=\"notes.csv\">notes.csv</a></li>\n</ul>\n<hr>\n</body>\n</html>\n";
    let length = html.len().to_string();
    let listing = crate::support::reply(
        "200 OK",
        &[
            ("Content-Type", "text/html; charset=utf-8"),
            ("Content-Length", &length),
        ],
        html.as_bytes(),
    );
    let routes = crate::support::Routes::start(vec![
        ("/set/".to_owned(), listing),
        (
            "/set/flowers.zip".to_owned(),
            file(&fixture("python.zip")?, "\"f1\""),
        ),
        ("/set/notes.csv".to_owned(), file(b"a,b\n1,2\n", "\"n1\"")),
    ])?;
    sandbox.allow(&routes.url);
    let added = sandbox.run(&["add", &format!("{}/set/", routes.url)])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    assert!(sandbox.work().join("data/set/dataset/README.md").exists());
    let (two, cache) = copied(&sandbox, "two")?;
    let asked = routes.requests().len();
    let synced = sandbox
        .command_in(&two, &["--offline", "sync"])
        .env("FETCHLOOM_CACHE", &cache)
        .output()?;
    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    assert_eq!(routes.requests().len(), asked);
    Ok(())
}

/// a single compressed file placed decompressed is checked in place by the one file it
/// decompressed to.
#[test]
fn a_copied_decompressed_file_is_checked_in_place() -> Result {
    let sandbox = Sandbox::new()?;
    let packed = crate::decompress::gz(b"read,1\n")?;
    let server = Server::start("/files/reads.csv.gz", vec![file(&packed, "\"r1\"")])?;
    sandbox.allow(&server.url);
    let added = sandbox.run(&["add", &server.url, "--decompress"])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let (two, cache) = copied(&sandbox, "two")?;
    let synced = sandbox
        .command_in(&two, &["--offline", "sync"])
        .env("FETCHLOOM_CACHE", &cache)
        .output()?;
    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    assert_eq!(
        fs::read(two.join("data").join("reads").join("reads.csv"))?,
        b"read,1\n"
    );
    assert_eq!(server.requests().len(), 1, "the add's download alone");
    Ok(())
}

/// the list the check recorded stays while the project pins its tree, through a gc that frees
/// everything unneeded: a push after it still finds it and asks the source nothing.
#[test]
fn a_gc_keeps_the_list_a_check_in_place_recorded() -> Result {
    let sandbox = Sandbox::new()?;
    let (server, two, cache) = copied_zip(&sandbox, 1)?;
    let run = |args: &[&str]| {
        sandbox
            .command_in(&two, args)
            .env("FETCHLOOM_CACHE", &cache)
            .output()
    };
    for args in [&["sync"][..], &["cache", "gc", "--now", "--yes"]] {
        let output = run(args)?;
        assert_eq!(
            output.status.code(),
            Some(0),
            "{args:?}: {}",
            text(&output.stderr)?
        );
    }
    let mirror = sandbox.folder("lab")?.join("mirror");
    let pushed = sandbox
        .command_in(&two, &["--json", "--offline", "--yes", "push", "--to"])
        .arg(&mirror)
        .env("FETCHLOOM_CACHE", &cache)
        .output()?;
    let document = json(&pushed)?;
    assert_eq!(pushed.status.code(), Some(0), "{document}");
    assert_eq!(files_below(&mirror.join("trees"))?, 1, "the zip's list");
    assert_eq!(server.requests().len(), 1, "the add's download alone");
    Ok(())
}
