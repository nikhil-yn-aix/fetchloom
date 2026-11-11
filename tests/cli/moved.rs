//! data that moved: when a link data.lock recorded is dead, the source is asked again, and what
//! it points to now must still match the pinned hash. a project folder that moved keeps its
//! files as they were.

use std::fs;
use std::path::{Path, PathBuf};

use crate::support::{Result, Sandbox, Server, file, json, reply, text};

const IRIS: &[u8] = b"sepal_length,species\n5.1,setosa\n";
const PATH: &str = "/files/iris.csv";

/// a fresh clone of a project whose data.lock records iris at a link that is now dead, with a
/// cold cache. the server's first response serves `add`, the second answers the dead link and
/// the third serves the link the source lists now.
fn moved(sandbox: &Sandbox, server: &Server) -> Result<PathBuf> {
    sandbox.allow(&server.url);
    let added = sandbox.run(&["add", &server.url])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let lock = sandbox.work().join("data.lock");
    let dead = server.url.replace("/files/", "/gone/");
    let pinned = fs::read_to_string(&lock)?.replace(
        &format!("url = \"{}\"", server.url),
        &format!("url = \"{dead}\""),
    );
    let clone = sandbox.folder("clone")?;
    fs::write(clone.join("data.lock"), pinned)?;
    fs::copy(sandbox.work().join("data.toml"), clone.join("data.toml"))?;
    Ok(clone)
}

fn gone() -> Vec<crate::support::Part> {
    reply("404 Not Found", &[("Content-Length", "0")], b"")
}

#[test]
fn a_dead_link_is_looked_up_again_through_the_source() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(
        PATH,
        vec![file(IRIS, "\"v1\""), gone(), file(IRIS, "\"v1\"")],
    )?;
    let clone = moved(&sandbox, &server)?;
    let synced = sandbox
        .command_in(&clone, &["sync"])
        .env("FETCHLOOM_CACHE", sandbox.folder("cold")?)
        .output()?;
    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(0), "{stderr}");
    assert_eq!(
        fs::read(clone.join("data").join("iris").join("iris.csv"))?,
        IRIS
    );
    assert!(
        stderr.contains("iris/iris.csv is gone from its link in data.lock"),
        "{stderr}"
    );
    let requests = server.requests();
    assert!(
        requests[1].starts_with("get /gone/iris.csv "),
        "{}",
        requests[1]
    );
    assert!(
        requests[2].starts_with("get /files/iris.csv "),
        "{}",
        requests[2]
    );
    Ok(())
}

#[test]
fn bytes_found_elsewhere_must_match_the_lock() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(
        PATH,
        vec![
            file(IRIS, "\"v1\""),
            gone(),
            file(b"other bytes\n", "\"v2\""),
        ],
    )?;
    let clone = moved(&sandbox, &server)?;
    let synced = sandbox
        .command_in(&clone, &["sync"])
        .env("FETCHLOOM_CACHE", sandbox.folder("cold")?)
        .output()?;
    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(30), "{stderr}");
    assert!(
        stderr.contains("is not the file data.lock records"),
        "{stderr}"
    );
    assert!(!clone.join("data").join("iris").join("iris.csv").exists());
    Ok(())
}

#[test]
fn every_file_gone_for_good_is_named_with_where_it_was() -> Result {
    let sandbox = Sandbox::new()?;
    let iris = Server::start(PATH, vec![file(IRIS, "\"v1\""), gone()])?;
    sandbox.allow(&iris.url);
    let cows = Server::start(
        "/files/cows.csv",
        vec![file(b"cow,sound\nbess,moo\n", "\"c1\""), gone()],
    )?;
    sandbox.allow(&cows.url);
    for server in [&iris, &cows] {
        let added = sandbox.run(&["add", &server.url])?;
        assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    }
    let clone = sandbox.folder("clone")?;
    for name in ["data.toml", "data.lock"] {
        fs::copy(sandbox.work().join(name), clone.join(name))?;
    }
    let synced = sandbox
        .command_in(&clone, &["sync"])
        .env("FETCHLOOM_CACHE", sandbox.folder("cold")?)
        .output()?;
    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(10), "{stderr}");
    assert!(
        stderr.contains("2 files could not be downloaded"),
        "{stderr}"
    );
    for (label, server) in [("iris/iris.csv", &iris), ("cows/cows.csv", &cows)] {
        assert!(
            stderr.contains(&format!("{label}: 127.0.0.1 has nothing at {}", server.url)),
            "{stderr}"
        );
    }
    assert!(
        !stderr.contains("still lists"),
        "a link is its own source: {stderr}"
    );
    Ok(())
}

/// a project moved to another folder, then `cache gc --now` before any run there: gc no longer
/// knew the project, so it freed the zip and the record of what it holds. every file still looks
/// as the last complete sync left it, so status finds nothing, as sync does, with no request.
#[test]
fn a_moved_project_whose_cache_was_cleaned_still_matches() -> Result {
    let sandbox = Sandbox::new()?;
    let zip =
        fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/archives/python.zip"))?;
    let server = Server::start("/files/dataset.zip", vec![file(&zip, "\"z1\"")])?;
    let added = sandbox.run(&["add", &server.url])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    // a file as new as the state is hashed before it is trusted, as on a clock as coarse as fat's
    // or a reflink placed within the state's own tick. dating the state back to the oldest file
    // makes every file that new, so the settled sync below must hash them and still plan nothing
    let state = sandbox.work().join(".fetchloom").join("state");
    let oldest = modified_times(&sandbox.work().join("data"))?
        .into_iter()
        .min()
        .ok_or("add placed files")?;
    fs::File::options()
        .write(true)
        .open(&state)?
        .set_modified(oldest)?;
    let moved = sandbox.folder("elsewhere")?.join("project");
    fs::rename(sandbox.work(), &moved)?;
    let cleaned = sandbox
        .command_in(&moved, &["cache", "gc", "--now"])
        .output()?;
    let stderr = text(&cleaned.stderr)?;
    assert_eq!(cleaned.status.code(), Some(0), "{stderr}");
    assert!(stderr.contains("Removed"), "gc freed the zip: {stderr}");

    let status = json(&sandbox.command_in(&moved, &["--json", "status"]).output()?)?;
    let result = status["result"].as_object().ok_or("status has a result")?;
    for (name, found) in result.iter().filter(|(name, _)| *name != "project") {
        assert_eq!(found, &serde_json::json!([]), "{name}: {status}");
    }
    let told = sandbox.command_in(&moved, &["status"]).output()?;
    assert_eq!(
        text(&told.stderr)?,
        "         Ok everything matches data.lock by size and time\n"
    );
    let synced = json(&sandbox.command_in(&moved, &["--json", "sync"]).output()?)?;
    assert_eq!(synced["status"], "ok", "{synced}");
    assert_eq!(synced["work"]["requests"], 0, "{synced}");
    assert_eq!(server.requests().len(), 1, "only add downloaded");
    Ok(())
}

/// the modification time of every file below `folder`.
fn modified_times(folder: &Path) -> std::io::Result<Vec<std::time::SystemTime>> {
    let mut times = Vec::new();
    for entry in fs::read_dir(folder)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            times.extend(modified_times(&entry.path())?);
        } else if kind.is_file() {
            times.push(entry.metadata()?.modified()?);
        }
    }
    Ok(times)
}
