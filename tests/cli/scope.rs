//! `sync` with dataset names, against servers on this machine: the named datasets are resolved,
//! downloaded, placed and cleaned up, and every other dataset stays as data.lock and the data
//! folder have it.

use std::fs;
use std::path::PathBuf;
use std::process::Output;

use crate::project::clone;
use crate::support::{Result, Sandbox, Server, delete, file, json, text};

const SMALL: &[u8] = b"s";
const LARGE: &[u8] = b"l";

/// a project with two datasets, small and large, each added from a server of its own that
/// answers `downloads` requests in all, the add's included, so a test counts each one's
/// requests. both are placed and in the cache.
fn small_and_large(sandbox: &Sandbox, downloads: usize) -> Result<(Server, Server)> {
    let mut servers = Vec::new();
    for (name, bytes) in [("small", SMALL), ("large", LARGE)] {
        let responses = (0..downloads).map(|_| file(bytes, "\"v1\"")).collect();
        let server = Server::start(&format!("/files/{name}.csv"), responses)?;
        sandbox.allow(&server.url);
        let added = sandbox.run(&["add", &server.url])?;
        assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
        servers.push(server);
    }
    let large = servers.pop().ok_or("no server for large")?;
    let small = servers.pop().ok_or("no server for small")?;
    Ok((small, large))
}

/// a clone of the project with a cache of its own, so every file it places is downloaded.
struct Fresh {
    folder: PathBuf,
    cache: PathBuf,
}

impl Fresh {
    fn new(sandbox: &Sandbox) -> Result<Fresh> {
        Ok(Fresh {
            folder: clone(sandbox, "fresh")?,
            cache: sandbox.folder("fresh-cache")?,
        })
    }

    fn run(&self, sandbox: &Sandbox, args: &[&str]) -> Result<Output> {
        Ok(sandbox
            .command_in(&self.folder, args)
            .env("FETCHLOOM_CACHE", &self.cache)
            .output()?)
    }

    fn placed(&self, name: &str) -> PathBuf {
        self.folder
            .join("data")
            .join(name)
            .join(format!("{name}.csv"))
    }
}

#[test]
fn sync_named_fetches_and_places_only_that_dataset() -> Result {
    let sandbox = Sandbox::new()?;
    let (small, large) = small_and_large(&sandbox, 2)?;
    let fresh = Fresh::new(&sandbox)?;

    let output = fresh.run(&sandbox, &["--json", "sync", "small"])?;

    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
    let result = &json(&output)?["result"];
    assert_eq!(result["selected"], serde_json::json!(["small"]));
    assert_eq!(result["datasets"], serde_json::json!([]), "no pin changed");
    assert_eq!(result["placement"]["placed"], 1, "{result}");
    assert_eq!(
        (small.requests().len(), large.requests().len()),
        (2, 1),
        "one download for small after the adds, none for large"
    );
    assert_eq!(fs::read(fresh.placed("small"))?, SMALL);
    assert!(!fresh.folder.join("data").join("large").exists());
    Ok(())
}

/// a dry run of a named sync shows what that sync would do: small's download and placement,
/// and nothing of large.
#[test]
fn a_named_dry_run_shows_only_that_dataset() -> Result {
    let sandbox = Sandbox::new()?;
    let (_small, _large) = small_and_large(&sandbox, 1)?;
    let fresh = Fresh::new(&sandbox)?;

    let output = fresh.run(&sandbox, &["--json", "sync", "small", "--dry-run"])?;

    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
    let result = &json(&output)?["result"];
    assert_eq!(result["selected"], serde_json::json!(["small"]));
    assert_eq!(
        result["changes"]["download"],
        serde_json::json!([{ "path": "small/small.csv", "size": 1 }])
    );
    assert_eq!(
        result["changes"]["place"],
        serde_json::json!(["data/small/small.csv"])
    );
    Ok(())
}

/// a sync of named datasets never records the project as settled, so the plain sync after it
/// still looks at every dataset and places the one it left out.
#[test]
fn a_named_sync_never_settles_the_project() -> Result {
    let sandbox = Sandbox::new()?;
    let (_small, large) = small_and_large(&sandbox, 2)?;
    let fresh = Fresh::new(&sandbox)?;

    let named = fresh.run(&sandbox, &["sync", "small"])?;
    let all = fresh.run(&sandbox, &["--json", "sync"])?;

    let stderr = text(&named.stderr)?;
    assert_eq!(named.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("Synced small into data · 1 other dataset left as it was"),
        "{stderr}"
    );
    let result = &json(&all)?["result"];
    assert_eq!(
        result.get("selected"),
        Some(&serde_json::Value::Null),
        "{result}"
    );
    assert_eq!(result["placement"]["placed"], 1, "{result}");
    assert_eq!(fs::read(fresh.placed("large"))?, LARGE);
    assert_eq!(large.requests().len(), 2);
    Ok(())
}

/// a dataset gone from data.toml keeps its files and its entry in data.lock through a sync of
/// another dataset; the plain sync after it removes both.
#[test]
fn sync_named_never_removes_other_datasets_files() -> Result {
    let sandbox = Sandbox::new()?;
    let (small, _large) = small_and_large(&sandbox, 1)?;
    let work = sandbox.work();
    fs::write(
        work.join("data.toml"),
        format!("[datasets.small]\nref = \"{}\"\n", small.url),
    )?;
    let lock = fs::read(work.join("data.lock"))?;
    let large = work.join("data").join("large").join("large.csv");

    let named = sandbox.run(&["sync", "small"])?;

    assert_eq!(named.status.code(), Some(0), "{}", text(&named.stderr)?);
    assert_eq!(fs::read(&large)?, LARGE);
    assert_eq!(
        fs::read(work.join("data.lock"))?,
        lock,
        "large keeps its entry"
    );
    let all = sandbox.run(&["sync"])?;
    assert_eq!(all.status.code(), Some(0), "{}", text(&all.stderr)?);
    assert!(!large.exists(), "the plain sync removes large's files");
    Ok(())
}

/// `--force` with a name replaces only that dataset's edits; another dataset's edit stays byte
/// for byte.
#[test]
fn force_named_replaces_only_that_datasets_edits() -> Result {
    let sandbox = Sandbox::new()?;
    small_and_large(&sandbox, 1)?;
    let work = sandbox.work();
    let placed = |name: &str| work.join("data").join(name).join(format!("{name}.csv"));
    for name in ["small", "large"] {
        // editors save by writing a new file in place of the old one
        delete(&placed(name))?;
        fs::write(placed(name), format!("my {name} notes\n"))?;
    }

    let forced = sandbox.run(&["sync", "--force", "small"])?;

    assert_eq!(forced.status.code(), Some(0), "{}", text(&forced.stderr)?);
    assert_eq!(fs::read(placed("small"))?, SMALL);
    assert_eq!(fs::read_to_string(placed("large"))?, "my large notes\n");
    Ok(())
}

/// a name data.toml does not have is a usage error naming the closest one, before any request.
#[test]
fn an_unknown_name_is_refused_before_any_request() -> Result {
    let sandbox = Sandbox::new()?;
    small_and_large(&sandbox, 1)?;
    let fresh = Fresh::new(&sandbox)?;

    let output = fresh.run(&sandbox, &["--json", "sync", "smal"])?;

    assert_eq!(output.status.code(), Some(2), "{}", text(&output.stdout)?);
    let document = json(&output)?;
    let error = &document["errors"][0];
    assert_eq!(error["kind"], "usage");
    assert_eq!(error["message"], "data.toml has no dataset called smal");
    assert_eq!(error["help"], "did you mean `fetchloom sync small`?");
    assert_eq!(document["work"]["requests"], 0);
    assert!(!fresh.folder.join("data").exists());
    Ok(())
}
