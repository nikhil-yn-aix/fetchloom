//! `--dry-run` on add, remove, sync and cache gc: the plan is told and nothing on disk changes.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::support::{Result, Sandbox, Server, file, json, text};

const IRIS: &[u8] = b"sepal_length,species\n5.1,setosa\n";
const PATH: &str = "/files/iris.csv";

/// every file and folder under `root` with its bytes, or nothing when `root` does not exist.
fn tree(root: &Path) -> Result<BTreeMap<PathBuf, Option<Vec<u8>>>> {
    let mut found = BTreeMap::new();
    let mut folders = vec![root.to_path_buf()];
    while let Some(folder) = folders.pop() {
        let Ok(entries) = fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries {
            let path = entry?.path();
            if path.is_dir() {
                found.insert(path.clone(), None);
                folders.push(path);
            } else {
                found.insert(path.clone(), Some(fs::read(&path)?));
            }
        }
    }
    Ok(found)
}

/// a project with iris added from `server`, whose first response serves it.
fn added(sandbox: &Sandbox, server: &Server) -> Result {
    let output = sandbox.run(&["add", &server.url])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    Ok(())
}

#[test]
fn sync_dry_run_on_a_fresh_clone_tells_what_it_would_place() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let clone = sandbox.folder("clone")?;
    for name in ["data.toml", "data.lock"] {
        fs::copy(sandbox.work().join(name), clone.join(name))?;
    }
    let (project, cache) = (tree(&clone)?, tree(&sandbox.cache())?);
    let output = sandbox
        .command_in(&clone, &["sync", "--dry-run"])
        .output()?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert_eq!(
        stderr,
        "Would place data/iris · 1 file\n         Ok dry run, nothing was changed\n"
    );
    let document = json(
        &sandbox
            .command_in(&clone, &["--json", "sync", "-n"])
            .output()?,
    )?;
    let result = &document["result"];
    assert_eq!(result["dry_run"], true);
    assert_eq!(result["lock_changed"], false);
    assert_eq!(result["placement"]["placed"], 1);
    assert_eq!(result["placement"]["unchanged"], 0);
    assert_eq!(
        result["changes"],
        serde_json::json!({
            "place": ["data/iris/iris.csv"],
            "replace": [],
            "remove": [],
            "download": [],
            "unpack": [],
        })
    );
    assert_eq!(document["work"]["requests"], 0);
    assert_eq!(tree(&clone)?, project, "the clone is as it was");
    assert_eq!(tree(&sandbox.cache())?, cache, "the cache is as it was");
    assert_eq!(server.requests().len(), 1, "only add downloaded");
    Ok(())
}

#[test]
fn sync_dry_run_lists_downloads_with_their_sizes_and_fetches_none() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    crate::support::delete(&placed)?;
    let cold = sandbox.cache().with_file_name("cold");
    let project = tree(&sandbox.work())?;
    let output = sandbox
        .command(&["sync", "--dry-run"])
        .env("FETCHLOOM_CACHE", &cold)
        .output()?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert_eq!(
        stderr,
        "Would fetch iris/iris.csv · 32 B\nWould place data/iris · 1 file\n         Ok dry run, nothing was changed\n"
    );
    let document = json(
        &sandbox
            .command(&["--json", "sync", "--dry-run"])
            .env("FETCHLOOM_CACHE", &cold)
            .output()?,
    )?;
    assert_eq!(
        document["result"]["changes"]["download"],
        serde_json::json!([{ "path": "iris/iris.csv", "size": IRIS.len() }])
    );
    assert!(!cold.exists(), "a dry run creates no cache");
    assert_eq!(tree(&sandbox.work())?, project);
    assert_eq!(server.requests().len(), 1, "nothing was downloaded");

    let offline = sandbox
        .command(&["--offline", "sync", "--dry-run"])
        .env("FETCHLOOM_CACHE", &cold)
        .output()?;
    assert_eq!(
        offline.status.code(),
        Some(20),
        "offline, the run would stop"
    );
    Ok(())
}

#[test]
fn sync_dry_run_tells_the_lock_a_new_dataset_would_get() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    let other = Server::start("/files/cows.csv", vec![file(b"cow\nbess\n", "\"c1\"")])?;
    added(&sandbox, &server)?;
    let manifest = sandbox.work().join("data.toml");
    let updated = format!(
        "{}\n[datasets.cows]\nref = \"{}\"\n",
        fs::read_to_string(&manifest)?,
        other.url
    );
    fs::write(&manifest, updated)?;
    let (project, cache) = (tree(&sandbox.work())?, tree(&sandbox.cache())?);
    let output = sandbox.run(&["sync", "-n"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert_eq!(
        stderr,
        " Would lock cows · 1 file in data.lock\nWould fetch cows/cows.csv · size unknown\nWould place data/cows · 1 file\n         Ok dry run, nothing was changed\n"
    );
    let document = json(&sandbox.run(&["--json", "sync", "-n"])?)?;
    let result = &document["result"];
    assert_eq!(result["lock_changed"], true);
    assert_eq!(result["placement"]["placed"], 1);
    assert_eq!(result["placement"]["unchanged"], 1);
    assert_eq!(
        result["changes"]["download"],
        serde_json::json!([{ "path": "cows/cows.csv", "size": null }])
    );
    assert_eq!(other.requests().len(), 0, "a plain link is not asked");
    assert_eq!(tree(&sandbox.work())?, project, "data.lock is as it was");
    assert_eq!(tree(&sandbox.cache())?, cache);
    Ok(())
}

#[test]
fn sync_dry_run_with_force_tells_what_it_would_replace() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    crate::support::delete(&placed)?;
    fs::write(&placed, b"mine\n")?;
    let (project, cache) = (tree(&sandbox.work())?, tree(&sandbox.cache())?);
    let kept = sandbox.run(&["sync", "-n"])?;
    let stderr = text(&kept.stderr)?;
    assert_eq!(kept.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.starts_with("warning: data/iris/iris.csv "),
        "{stderr}"
    );
    assert!(
        stderr.ends_with("         Ok dry run, nothing was changed\n"),
        "{stderr}"
    );
    let output = sandbox.run(&["sync", "--force", "--dry-run"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert_eq!(
        stderr,
        "Would renew data/iris · 1 file\n         Ok dry run, nothing was changed\n"
    );
    let document = json(&sandbox.run(&["--json", "sync", "--force", "-n"])?)?;
    let result = &document["result"];
    assert_eq!(result["dry_run"], true);
    assert_eq!(result["lock_changed"], false);
    assert_eq!(result["placement"]["replaced"], 1);
    assert_eq!(
        result["changes"]["replace"],
        serde_json::json!(["data/iris/iris.csv"])
    );
    assert_eq!(result["changes"]["download"], serde_json::json!([]));
    assert_eq!(tree(&sandbox.work())?, project, "the edit is still there");
    assert_eq!(tree(&sandbox.cache())?, cache);
    Ok(())
}

#[test]
fn remove_dry_run_tells_what_it_would_remove() -> Result {
    let sandbox = Sandbox::new()?;
    let first = Server::start("/a.csv", vec![file(b"a\n", "\"a\"")])?;
    let second = Server::start("/b.csv", vec![file(b"b\n", "\"b\"")])?;
    added(&sandbox, &first)?;
    added(&sandbox, &second)?;
    let (project, cache) = (tree(&sandbox.work())?, tree(&sandbox.cache())?);
    let output = sandbox.run(&["remove", "a", "--dry-run"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    sandbox
        .filters()
        .bind(|| insta::assert_snapshot!("remove_dry_run", stderr));
    let verbose = text(&sandbox.run(&["-v", "remove", "a", "-n"])?.stderr)?;
    assert!(
        verbose.contains("\n            data/a/a.csv\n"),
        "-v names each file\n{verbose}"
    );
    let document = json(&sandbox.run(&["--json", "remove", "a", "-n"])?)?;
    let result = &document["result"];
    assert_eq!(result["dry_run"], true);
    assert_eq!(result["datasets"], serde_json::json!(["a"]));
    assert_eq!(result["lock_changed"], true);
    assert_eq!(result["placement"]["removed"], 1);
    assert_eq!(
        result["changes"]["remove"],
        serde_json::json!(["data/a/a.csv"])
    );
    assert_eq!(tree(&sandbox.work())?, project, "the project is as it was");
    assert_eq!(tree(&sandbox.cache())?, cache);
    Ok(())
}

#[test]
fn cache_gc_dry_run_tells_what_it_would_free() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    fs::remove_file(sandbox.work().join("data.toml"))?;
    let cache = tree(&sandbox.cache())?;
    let output = sandbox.run(&["cache", "gc", "--now", "--dry-run"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert_eq!(
        stderr,
        " Would free 1 file · 32 B from the cache\n         Ok dry run, nothing was changed\n"
    );
    let young = text(&sandbox.run(&["cache", "gc", "-n"])?.stderr)?;
    assert_eq!(
        young,
        "         Ok nothing to free yet; dry run, nothing was changed\n  hint: 32 B no project needs is kept for a day in case you want it back; `fetchloom cache gc --now` frees it now\n"
    );
    let document = json(&sandbox.run(&["--json", "cache", "gc", "--now", "-n"])?)?;
    let result = &document["result"];
    assert_eq!(result["dry_run"], true);
    assert_eq!(result["objects"], 1);
    assert_eq!(result["bytes"], IRIS.len());
    let removed: Vec<&str> = result["changes"]["remove"]
        .as_array()
        .ok_or("no paths")?
        .iter()
        .filter_map(serde_json::Value::as_str)
        .collect();
    assert!(
        removed.iter().any(|path| path.contains("objects")),
        "{removed:?}"
    );
    assert!(
        removed.iter().any(|path| path.contains("projects")),
        "the forgotten project's record: {removed:?}"
    );
    assert_eq!(tree(&sandbox.cache())?, cache, "the cache is as it was");
    let real = json(&sandbox.run(&["--json", "cache", "gc", "--now"])?)?;
    assert_eq!(
        real["result"]["objects"], 1,
        "gc frees what it said it would"
    );
    assert!(real["result"].get("dry_run").is_none());
    Ok(())
}

#[test]
fn add_dry_run_writes_nothing_and_downloads_nothing() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    fs::create_dir(sandbox.work().join(".git"))?;
    let project = tree(&sandbox.work())?;
    let output = sandbox.run(&["add", &server.url, "--dry-run"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    sandbox
        .filters()
        .bind(|| insta::assert_snapshot!("add_dry_run", stderr));
    let document = json(&sandbox.run(&["--json", "add", &server.url, "-n"])?)?;
    let result = &document["result"];
    assert_eq!(result["dry_run"], true);
    assert_eq!(result["datasets"], serde_json::json!(["iris"]));
    assert_eq!(result["lock_changed"], true);
    assert_eq!(result["placement"]["placed"], 1);
    assert_eq!(
        result["changes"]["place"],
        serde_json::json!(["data/iris/iris.csv"])
    );
    assert_eq!(
        result["changes"]["download"],
        serde_json::json!([{ "path": "iris/iris.csv", "size": null }])
    );
    assert_eq!(
        tree(&sandbox.work())?,
        project,
        "no data.toml, lock or data/"
    );
    assert!(!sandbox.cache().exists(), "no cache");
    assert_eq!(server.requests().len(), 0);

    added(&sandbox, &server)?;
    let project = tree(&sandbox.work())?;
    let again = sandbox.run(&["add", &server.url, "-n"])?;
    assert_eq!(
        text(&again.stderr)?,
        "         Ok iris is already in data.toml; dry run, nothing was changed\n"
    );
    assert_eq!(tree(&sandbox.work())?, project);
    Ok(())
}

#[test]
fn a_dry_run_leaves_an_empty_cache_folder_empty() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    let other = Server::start("/files/cows.csv", vec![file(b"cow\nbess\n", "\"c1\"")])?;
    added(&sandbox, &server)?;
    let empty = sandbox.folder("empty")?;
    let project = tree(&sandbox.work())?;
    for args in [
        vec!["sync", "--dry-run"],
        vec!["add", other.url.as_str(), "--dry-run"],
        vec!["remove", "iris", "--dry-run"],
        vec!["cache", "gc", "--dry-run"],
        vec!["cache", "gc", "--now", "--dry-run"],
    ] {
        let output = sandbox
            .command(&args)
            .env("FETCHLOOM_CACHE", &empty)
            .output()?;
        assert_eq!(
            output.status.code(),
            Some(0),
            "{args:?}: {}",
            text(&output.stderr)?
        );
        assert_eq!(
            fs::read_dir(&empty)?.count(),
            0,
            "{args:?} leaves the cache folder empty"
        );
    }
    assert_eq!(tree(&sandbox.work())?, project);
    assert_eq!(other.requests().len(), 0);
    Ok(())
}

#[test]
fn sync_dry_run_on_a_settled_project_needs_no_cache() -> Result {
    let sandbox = Sandbox::new()?;
    let zip =
        fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/archives/python.zip"))?;
    let server = Server::start("/files/dataset.zip", vec![file(&zip, "\"z1\"")])?;
    added(&sandbox, &server)?;
    let settled = sandbox.run(&["sync"])?;
    assert_eq!(settled.status.code(), Some(0), "{}", text(&settled.stderr)?);
    // on a clock as coarse as hfs+ or fat, files placed within the state's own second stay
    // racy and are hashed again, so the state is written once more past that second
    let state = sandbox.work().join(".fetchloom").join("state");
    let written = fs::metadata(&state)?.modified()?;
    if written
        .duration_since(std::time::UNIX_EPOCH)?
        .subsec_nanos()
        == 0
    {
        sandbox.wait_for_a_later_time(written)?;
        fs::write(&state, fs::read(&state)?)?;
    }
    let cold = sandbox.cache().with_file_name("cold");
    let project = tree(&sandbox.work())?;
    for args in [
        vec!["sync", "--dry-run"],
        vec!["--offline", "sync", "--dry-run"],
    ] {
        let output = sandbox
            .command(&args)
            .env("FETCHLOOM_CACHE", &cold)
            .output()?;
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(0), "{args:?}: {stderr}");
        assert_eq!(
            stderr, "         Ok all pinned files are in place; dry run, nothing was changed\n",
            "{args:?}"
        );
    }
    assert!(!cold.exists(), "a dry run creates no cache");
    assert_eq!(tree(&sandbox.work())?, project);
    let real = sandbox
        .command(&["--offline", "sync"])
        .env("FETCHLOOM_CACHE", &cold)
        .output()?;
    assert_eq!(
        text(&real.stderr)?,
        "         Ok all pinned files are in place, nothing to do\n"
    );
    assert_eq!(server.requests().len(), 1);
    Ok(())
}

#[test]
fn cache_gc_dry_run_lists_the_marks_a_real_gc_removes() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    fs::remove_file(sandbox.work().join("data.toml"))?;
    let objects = sandbox.cache().join("objects");
    let object = tree(&objects)?
        .into_iter()
        .find_map(|(path, bytes)| bytes.map(|_| path))
        .ok_or("no object")?;
    let hex = format!(
        "{}{}",
        object
            .parent()
            .and_then(Path::file_name)
            .ok_or("no fan")?
            .to_string_lossy(),
        object.file_name().ok_or("no name")?.to_string_lossy()
    );
    let mark = sandbox.cache().join("changed").join(hex);
    fs::write(&mark, b"")?;
    let before = tree(&sandbox.cache())?;
    let document = json(&sandbox.run(&["--json", "cache", "gc", "--now", "--dry-run"])?)?;
    let listed: Vec<PathBuf> = document["result"]["changes"]["remove"]
        .as_array()
        .ok_or("no paths")?
        .iter()
        .filter_map(serde_json::Value::as_str)
        .map(PathBuf::from)
        .collect();
    assert!(listed.contains(&object), "{listed:?}");
    assert!(listed.contains(&mark), "{listed:?}");
    assert_eq!(tree(&sandbox.cache())?, before, "the cache is as it was");
    let real = sandbox.run(&["cache", "gc", "--now"])?;
    assert_eq!(real.status.code(), Some(0), "{}", text(&real.stderr)?);
    let after = tree(&sandbox.cache())?;
    let removed: Vec<&PathBuf> = before
        .keys()
        .filter(|path| !after.contains_key(*path))
        .collect();
    let mut expected: Vec<&PathBuf> = listed.iter().collect();
    expected.sort();
    assert_eq!(removed, expected, "the dry run lists what gc removes");
    Ok(())
}
