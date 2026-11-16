//! pinned files another tool keeps on this machine: hugging face's cache, kagglehub's and the
//! folders `reuse` names give them to a fresh clone before any request, as a clone or a copy,
//! and nothing there changes.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::project::{IRIS, PATH, added, clone};
use crate::support::{Result, Routes, Sandbox, Server, file, json, reply, sha256_hex, text};

const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
/// a plain git file of the repository, whose blob hugging face names by its git hash.
const CARD: &[u8] = b"# rows\n";
const CARD_GIT: &str = "5d41402abc4b2a76b9719d911017c592ae0ab1c2";

/// an lfs file of the repository, whose SHA-256 the hub states and names its blob by.
fn rows() -> Vec<u8> {
    vec![b'r'; 3000]
}

/// the hub's API and its file server for `lab/rows` at [`COMMIT`].
#[cfg(debug_assertions)]
fn hub() -> Result<(Routes, Routes)> {
    let answer = |body: serde_json::Value| {
        reply(
            "200 OK",
            &[("Content-Type", "application/json")],
            body.to_string().as_bytes(),
        )
    };
    let rows = rows();
    let api = Routes::start(vec![
        (
            "/api/datasets/lab/rows/revision/main?expand=sha&expand=gated&expand=private&expand=tags&expand=cardData".to_owned(),
            answer(serde_json::json!({"id": "lab/rows", "sha": COMMIT, "tags": []})),
        ),
        (
            format!("/api/datasets/lab/rows/tree/{COMMIT}?recursive=true"),
            answer(serde_json::json!([
                {"type": "file", "oid": CARD_GIT, "size": CARD.len(), "path": "README.md"},
                {"type": "file", "oid": "2", "size": rows.len(), "path": "data/rows.parquet",
                 "lfs": {"oid": sha256_hex(&rows), "size": rows.len(), "pointerSize": 130}},
            ])),
        ),
    ])?;
    let resolve = format!("/huggingface.co/datasets/lab/rows/resolve/{COMMIT}");
    let files = Routes::start(vec![
        (format!("{resolve}/README.md"), file(CARD, "\"c\"")),
        (format!("{resolve}/data/rows.parquet"), file(&rows, "\"r\"")),
    ])?;
    Ok((api, files))
}

/// `hf:datasets/lab/rows` added in the work folder, then cloned into `name` with an empty cache
/// of its own: a teammate's fresh clone.
#[cfg(debug_assertions)]
fn pinned(sandbox: &Sandbox, api: &Routes, files: &Routes, name: &str) -> Result<PathBuf> {
    let output = sandbox
        .command(&["add", "hf:datasets/lab/rows"])
        .env("FETCHLOOM_TEST_HF_API", &api.url)
        .env("FETCHLOOM_TEST_HOSTS", &files.url)
        .output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    clone(sandbox, name)
}

/// hugging face's cache in the sandbox's home, laid out as `hf download` leaves it: each blob
/// named by its etag, the SHA-256 for an lfs file and the git hash for a plain one, and the
/// snapshot's entries links to them, or the files themselves where links cannot be made.
fn hugging_face_cache(sandbox: &Sandbox, rows: &[u8]) -> Result<PathBuf> {
    let root = in_home(sandbox, &[".cache", "huggingface", "hub"])?;
    let repository = root.join("datasets--lab--rows");
    let (blobs, snapshot) = (
        repository.join("blobs"),
        repository.join("snapshots").join(COMMIT),
    );
    fs::create_dir_all(&blobs)?;
    fs::create_dir_all(snapshot.join("data"))?;
    fs::create_dir_all(repository.join("refs"))?;
    fs::write(repository.join("refs").join("main"), COMMIT)?;
    let rows_blob = sha256_hex(&self::rows());
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        fs::write(blobs.join(&rows_blob), rows)?;
        fs::write(blobs.join(CARD_GIT), CARD)?;
        symlink(
            format!("../../../blobs/{rows_blob}"),
            snapshot.join("data").join("rows.parquet"),
        )?;
        symlink(
            format!("../../blobs/{CARD_GIT}"),
            snapshot.join("README.md"),
        )?;
    }
    #[cfg(windows)]
    {
        let _ = rows_blob;
        fs::write(snapshot.join("data").join("rows.parquet"), rows)?;
        fs::write(snapshot.join("README.md"), CARD)?;
    }
    Ok(root)
}

/// the folder `names` below the sandbox's home, made, spelled as fetchloom spells it from there.
fn in_home(sandbox: &Sandbox, names: &[&str]) -> Result<PathBuf> {
    let folder = names
        .iter()
        .fold(sandbox.folder("home")?, |path, name| path.join(name));
    fs::create_dir_all(&folder)?;
    Ok(folder)
}

/// a file as a look finds it: its path, bytes, permissions and modification time.
type Looked = (PathBuf, Vec<u8>, fs::Permissions, SystemTime);

/// every file below `root` as a look finds it, so one that changed nothing finds them again.
fn every_file(root: &Path) -> Result<Vec<Looked>> {
    let mut found = Vec::new();
    let mut waiting = vec![root.to_path_buf()];
    while let Some(folder) = waiting.pop() {
        for entry in fs::read_dir(&folder)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                waiting.push(entry.path());
            } else if kind.is_file() {
                let found_at = fs::symlink_metadata(entry.path())?;
                found.push((
                    entry.path(),
                    fs::read(entry.path())?,
                    found_at.permissions(),
                    found_at.modified()?,
                ));
            }
        }
    }
    found.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(found)
}

/// the file at `path` has no other name, so no link was made to it.
fn alone(path: &Path) -> Result {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        assert_eq!(fs::metadata(path)?.nlink(), 1, "{}", path.display());
    }
    #[cfg(windows)]
    {
        let found = winapi_util::file::information(fs::File::open(path)?)?;
        assert_eq!(found.number_of_links(), 1, "{}", path.display());
    }
    Ok(())
}

/// the owner's example: the same data twice beside hugging face's cache. a teammate's clone takes
/// both files from it with no request, as clones where the disk makes them, else as copies whose
/// line names the bytes and the `hf cache rm` that frees hugging face's copy. nothing in that cache
/// changes, deleting it afterwards works and leaves the project whole, and a second sync finds
/// nothing to do.
#[cfg(debug_assertions)]
#[test]
fn a_clone_takes_its_pins_from_the_hugging_face_cache_with_no_request() -> Result {
    let sandbox = Sandbox::new()?;
    let (api, files) = hub()?;
    let teammate = pinned(&sandbox, &api, &files, "teammate")?;
    let hub_root = hugging_face_cache(&sandbox, &rows())?;
    let before = every_file(&hub_root)?;
    let (asked, cache) = (files.requests().len(), sandbox.folder("teammate cache")?);
    let run = |args: &[&str]| {
        sandbox
            .command_in(&teammate, args)
            .env("FETCHLOOM_TEST_HF_API", &api.url)
            .env("FETCHLOOM_TEST_HOSTS", &files.url)
            .env("FETCHLOOM_CACHE", &cache)
            .output()
    };
    let output = run(&["sync"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert_eq!(files.requests().len(), asked, "{stderr}");
    let size = "2.9 KiB";
    let cloned = sandbox.links()?.reflink && cfg!(any(target_os = "linux", target_os = "macos"));
    if cloned {
        assert!(
            stderr.contains(&format!(
                "Cloned 2 files · {size} from the hugging face cache into the cache"
            )),
            "{stderr}"
        );
    } else {
        assert!(
            stderr.contains(&format!(
                "Copied 2 files · {size} from the hugging face cache into the cache"
            )),
            "{stderr}"
        );
        assert!(
            stderr.contains(&format!(
                "the hugging face cache at {} keeps its own copy of these {size}; `hf cache rm dataset/lab/rows` frees it there if nothing else needs it",
                hub_root.display()
            )),
            "{stderr}"
        );
    }
    let placed = teammate.join("data").join("rows");
    assert_eq!(fs::read(placed.join("data").join("rows.parquet"))?, rows());
    assert_eq!(fs::read(placed.join("README.md"))?, CARD);
    assert_eq!(every_file(&hub_root)?, before);
    for (path, ..) in &before {
        alone(path)?;
    }
    // what `hf cache rm` does: hugging face's own files go, and fetchloom's stay
    fs::remove_dir_all(&hub_root)?;
    let again = run(&["--json", "sync"])?;
    assert_eq!(again.status.code(), Some(0), "{}", text(&again.stdout)?);
    let document = json(&again)?;
    for counter in ["requests", "reused_objects", "objects_written"] {
        assert_eq!(document["work"][counter], 0, "{counter}: {document}");
    }
    let status = run(&["status"])?;
    assert_eq!(status.status.code(), Some(0), "{}", text(&status.stderr)?);
    assert_eq!(fs::read(placed.join("data").join("rows.parquet"))?, rows());
    Ok(())
}

/// a blob named by the pin that holds other bytes is passed over and the file downloaded, and
/// hugging face's file stays as it was.
#[cfg(debug_assertions)]
#[test]
fn a_copy_of_other_bytes_in_the_hugging_face_cache_is_left_and_downloaded() -> Result {
    let sandbox = Sandbox::new()?;
    let (api, files) = hub()?;
    let teammate = pinned(&sandbox, &api, &files, "teammate")?;
    let hub_root = hugging_face_cache(&sandbox, &vec![b'x'; 3000])?;
    let before = every_file(&hub_root)?;
    let asked = files.requests().len();
    let output = sandbox
        .command_in(&teammate, &["--json", "sync"])
        .env("FETCHLOOM_TEST_HF_API", &api.url)
        .env("FETCHLOOM_TEST_HOSTS", &files.url)
        .env("FETCHLOOM_CACHE", sandbox.folder("teammate cache")?)
        .output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
    let document = json(&output)?;
    // the card still comes from hugging face's cache; the rows, whose copy differs, are fetched
    assert_eq!(document["work"]["reused_objects"], 1, "{document}");
    let fetched: Vec<String> = files.targets()[asked..].to_vec();
    assert_eq!(
        fetched,
        [format!(
            "/huggingface.co/datasets/lab/rows/resolve/{COMMIT}/data/rows.parquet"
        )]
    );
    let placed = teammate.join("data").join("rows").join("data");
    assert_eq!(fs::read(placed.join("rows.parquet"))?, rows());
    assert_eq!(every_file(&hub_root)?, before);
    Ok(())
}

/// offline, hugging face's cache is on this machine, so it still gives what the cache lacks.
#[cfg(debug_assertions)]
#[test]
fn an_offline_sync_takes_pins_from_the_hugging_face_cache() -> Result {
    let sandbox = Sandbox::new()?;
    let (api, files) = hub()?;
    let teammate = pinned(&sandbox, &api, &files, "teammate")?;
    hugging_face_cache(&sandbox, &rows())?;
    let output = sandbox
        .command_in(&teammate, &["--offline", "sync"])
        .env("FETCHLOOM_CACHE", sandbox.folder("teammate cache")?)
        .output()?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(stderr.contains("from the hugging face cache"), "{stderr}");
    let placed = teammate.join("data").join("rows").join("data");
    assert_eq!(fs::read(placed.join("rows.parquet"))?, rows());
    Ok(())
}

/// a folder `reuse` names, such as one `hf download --local-dir` filled, holds the file under
/// another name: it comes into the cache once, and an empty `FETCHLOOM_REUSE` reads no folder,
/// not even hugging face's.
#[test]
fn a_reuse_folder_gives_a_file_under_another_name() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let teammate = clone(&sandbox, "teammate")?;
    let local = sandbox.folder("downloads")?.join("iris");
    fs::create_dir_all(&local)?;
    fs::write(local.join("Iris.csv"), IRIS)?;
    fs::write(local.join("notes.txt"), b"not iris\n")?;
    let cache = sandbox.folder("teammate cache")?;
    let run = |reuse: &str| {
        sandbox
            .command_in(&teammate, &["--json", "sync"])
            .env("FETCHLOOM_CACHE", &cache)
            .env("FETCHLOOM_REUSE", reuse)
            .output()
    };
    let output = run(&local.display().to_string())?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
    let document = json(&output)?;
    assert_eq!(document["work"]["reused_objects"], 1, "{document}");
    assert_eq!(document["work"]["requests"], 0, "{document}");
    assert_eq!(fs::read(local.join("Iris.csv"))?, IRIS);
    let again = json(&run(&local.display().to_string())?)?;
    assert_eq!(again["work"]["reused_objects"], 0, "{again}");
    assert_eq!(again["work"]["requests"], 0, "{again}");
    // with reuse turned off, a fresh cache downloads, though hugging face's cache has the file
    let hub = in_home(
        &sandbox,
        &[".cache", "huggingface", "hub", "datasets--a--b", "blobs"],
    )?;
    fs::write(hub.join(sha256_hex(IRIS)), IRIS)?;
    let off = sandbox.folder("cache off")?;
    let fresh = sandbox.folder("fresh")?;
    for name in ["data.toml", "data.lock"] {
        fs::copy(teammate.join(name), fresh.join(name))?;
    }
    let output = sandbox
        .command_in(&fresh, &["--json", "sync"])
        .env("FETCHLOOM_CACHE", &off)
        .env("FETCHLOOM_REUSE", "")
        .output()?;
    let document = json(&output)?;
    assert_eq!(document["work"]["reused_objects"], 0, "{document}");
    assert_eq!(document["work"]["requests"], 1, "{document}");
    Ok(())
}

/// hugging face's cache is read by default through its own layout alone, so a pinned file it holds
/// under a name no shortcut finds, as a plain git file's blob is named, is downloaded. listing the
/// cache in `reuse` has every file of the pinned size there looked at, and gives it with no request.
#[test]
fn only_a_listed_folder_is_searched_for_a_file_no_shortcut_finds() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let hub = in_home(&sandbox, &[".cache", "huggingface", "hub"])?;
    let blobs = hub.join("datasets--lab--iris").join("blobs");
    fs::create_dir_all(&blobs)?;
    fs::write(blobs.join(CARD_GIT), IRIS)?;
    let sync = |name: &str, reuse: Option<&Path>| -> Result<serde_json::Value> {
        let teammate = clone(&sandbox, name)?;
        let mut command = sandbox.command_in(&teammate, &["--json", "sync"]);
        command.env("FETCHLOOM_CACHE", sandbox.folder(&format!("{name} cache"))?);
        if let Some(reuse) = reuse {
            command.env("FETCHLOOM_REUSE", reuse);
        }
        let output = command.output()?;
        assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
        json(&output)
    };
    let unlisted = sync("unlisted", None)?;
    assert_eq!(unlisted["work"]["reused_objects"], 0, "{unlisted}");
    assert_eq!(unlisted["work"]["requests"], 1, "{unlisted}");
    let listed = sync("listed", Some(&hub))?;
    assert_eq!(listed["work"]["reused_objects"], 1, "{listed}");
    assert_eq!(listed["work"]["requests"], 0, "{listed}");
    Ok(())
}

/// kagglehub marks a version complete once it has every file; one it has not marked may still be
/// arriving, so its files are never taken, though they would hash right, even from kagglehub's
/// cache listed in `reuse`, where any file of the pinned size is looked at.
#[test]
fn a_kagglehub_version_without_its_marker_is_not_used() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let dataset = in_home(
        &sandbox,
        &[".cache", "kagglehub", "datasets", "uciml", "iris"],
    )?;
    let version = dataset.join("versions").join("2");
    fs::create_dir_all(&version)?;
    fs::write(version.join("Iris.csv"), IRIS)?;
    let kagglehub = in_home(&sandbox, &[".cache", "kagglehub"])?;
    let run = |name: &str| -> Result<serde_json::Value> {
        let teammate = clone(&sandbox, name)?;
        let output = sandbox
            .command_in(&teammate, &["--json", "sync"])
            .env("FETCHLOOM_CACHE", sandbox.folder(&format!("{name} cache"))?)
            .env("FETCHLOOM_REUSE", &kagglehub)
            .output()?;
        assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
        json(&output)
    };
    let unmarked = run("one")?;
    assert_eq!(unmarked["work"]["reused_objects"], 0, "{unmarked}");
    assert_eq!(unmarked["work"]["requests"], 1, "{unmarked}");
    fs::write(dataset.join("2.complete"), b"")?;
    let marked = run("two")?;
    assert_eq!(marked["work"]["reused_objects"], 1, "{marked}");
    assert_eq!(marked["work"]["requests"], 0, "{marked}");
    Ok(())
}
