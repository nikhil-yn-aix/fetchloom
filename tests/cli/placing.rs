//! placing files in the data folder: what fetchloom may delete or replace, and what it keeps.

use std::fs;
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use crate::support::{Part, Result, Sandbox, Server, file, reply, text};

const V1: &[u8] = b"sepal_length,species\n5.1,setosa\n";
const V2: &[u8] = b"sepal_length,species\n9.9,new\n";

/// writes over a placed file as a user would, clearing its read-only flag first.
fn edit(path: &Path, bytes: &[u8]) -> Result {
    let mut permissions = fs::metadata(path)?.permissions();
    #[expect(
        clippy::permissions_set_readonly_false,
        reason = "the test edits a placed file as a user would"
    )]
    permissions.set_readonly(false);
    fs::set_permissions(path, permissions)?;
    fs::write(path, bytes)?;
    Ok(())
}

/// waits until `server` has seen `count` requests.
fn requested(server: &Server, count: usize) -> Result {
    let started = Instant::now();
    while server.requests().len() < count {
        if started.elapsed() > Duration::from_secs(60) {
            return Err("the request never came".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

#[test]
fn files_edited_while_a_download_runs_are_kept() -> Result {
    let sandbox = Sandbox::new()?;
    let mut slow = reply(
        "200 OK",
        &[
            ("Content-Length", &V2.len().to_string()),
            ("ETag", "\"v2\""),
        ],
        b"",
    );
    slow.push(Part::Pause(Duration::from_secs(2)));
    slow.push(Part::Bytes(V2.to_vec()));
    let iris = Server::start(
        "/files/iris.csv",
        vec![file(V1, "\"v1\""), file(V2, "\"v2\""), slow],
    )?;
    let cows = Server::start("/files/cows.csv", vec![file(b"cow\nbess\n", "\"c1\"")])?;
    for server in [&iris, &cows] {
        let added = sandbox.run(&["add", &server.url])?;
        assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    }
    let upgraded = sandbox.run(&["lock", "--upgrade", "iris"])?;
    assert_eq!(
        upgraded.status.code(),
        Some(0),
        "{}",
        text(&upgraded.stderr)?
    );
    fs::write(
        sandbox.work().join("data.toml"),
        format!("[datasets.iris]\nref = \"{}\"\n", iris.url),
    )?;
    // a cache without the new bytes makes the sync download them after it has looked at data/
    let sync = sandbox
        .command(&["sync"])
        .env("FETCHLOOM_CACHE", sandbox.folder("empty-cache")?)
        .stderr(Stdio::piped())
        .spawn()?;
    requested(&iris, 3)?;
    let outdated = sandbox.work().join("data").join("iris").join("iris.csv");
    let unneeded = sandbox.work().join("data").join("cows").join("cows.csv");
    edit(&outdated, b"my notes on iris\n")?;
    edit(&unneeded, b"my notes on cows\n")?;
    let output = sync.wait_with_output()?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert_eq!(fs::read(&outdated)?, b"my notes on iris\n", "{stderr}");
    assert_eq!(fs::read(&unneeded)?, b"my notes on cows\n", "{stderr}");
    assert!(
        stderr.contains("data/iris/iris.csv was changed by hand, so it stays"),
        "{stderr}"
    );
    assert!(
        stderr.contains("data/cows/cows.csv is no longer needed, but it was changed by hand"),
        "{stderr}"
    );
    Ok(())
}

#[test]
fn a_sync_with_nothing_to_do_writes_nothing() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    let added = sandbox.run(&["add", &server.url])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let written = |name: &str| fs::metadata(sandbox.work().join(name))?.modified();
    let before = (written("data.lock")?, written(".fetchloom/state")?);
    // a rewrite now would carry a later time
    std::thread::sleep(Duration::from_millis(50));
    let output = sandbox.run(&["sync"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    assert_eq!(
        (written("data.lock")?, written(".fetchloom/state")?),
        before
    );
    Ok(())
}

#[test]
fn a_trace_shows_each_phase_of_a_sync() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    let added = sandbox.run(&["add", &server.url])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let trace = sandbox.folder("traces")?.join("sync.json");
    let output = sandbox
        .command(&["sync"])
        .env("FETCHLOOM_TRACE", &trace)
        .output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    let events = fs::read_to_string(&trace)?;
    for phase in [
        "command",
        "read_manifest",
        "read_lock",
        "read_state",
        "scan",
        "audit",
        "relock",
        "plan",
        "decide",
        "find_objects",
        "place",
    ] {
        assert!(
            events.contains(&format!("\"name\":\"{phase}\"")),
            "{phase}: {events}"
        );
    }
    Ok(())
}

/// a project in `work` and a second one in `other`, both holding iris as a link to one cached
/// file, with iris then edited through the first project's link.
fn shared_and_edited(sandbox: &Sandbox, server: &Server) -> Result<std::path::PathBuf> {
    let added = sandbox.run(&["add", &server.url])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let other = sandbox.folder("other")?;
    for name in ["data.toml", "data.lock"] {
        fs::copy(sandbox.work().join(name), other.join(name))?;
    }
    let synced = sandbox.command_in(&other, &["sync"]).output()?;
    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    let mut edited = V1.to_vec();
    edited[0] = b'S';
    edit(
        &sandbox.work().join("data").join("iris").join("iris.csv"),
        &edited,
    )?;
    Ok(other)
}

#[test]
fn sync_force_puts_back_a_link_another_project_edited() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(
        "/files/iris.csv",
        vec![file(V1, "\"v1\""), file(V1, "\"v1\"")],
    )?;
    let other = shared_and_edited(&sandbox, &server)?;
    let forced = sandbox.command_in(&other, &["sync", "--force"]).output()?;
    assert_eq!(forced.status.code(), Some(0), "{}", text(&forced.stderr)?);
    assert_eq!(
        fs::read(other.join("data").join("iris").join("iris.csv"))?,
        V1
    );
    assert_eq!(
        server.requests().len(),
        2,
        "the changed object is fetched again"
    );
    Ok(())
}

#[test]
fn status_shows_a_link_another_project_edited_once_the_cache_knows() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    let other = shared_and_edited(&sandbox, &server)?;
    let verified = sandbox.run(&["cache", "verify"])?;
    assert_eq!(
        verified.status.code(),
        Some(30),
        "{}",
        text(&verified.stderr)?
    );
    let status = sandbox.command_in(&other, &["status"]).output()?;
    let stderr = text(&status.stderr)?;
    assert!(stderr.contains("Edited data/iris/iris.csv"), "{stderr}");
    Ok(())
}

/// on windows a folder's listing keeps the old size and time of a hardlink edited through another
/// name, so nothing in the other project's folder shows the edit. once the cache has found the
/// object changed, that project's next sync puts its copy back without --force.
#[cfg(windows)]
#[test]
fn the_first_sync_after_verify_puts_back_a_link_another_project_edited() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(
        "/files/iris.csv",
        vec![file(V1, "\"v1\""), file(V1, "\"v1\"")],
    )?;
    let other = shared_and_edited(&sandbox, &server)?;
    let verified = sandbox.run(&["cache", "verify"])?;
    assert_eq!(
        verified.status.code(),
        Some(30),
        "{}",
        text(&verified.stderr)?
    );
    let synced = sandbox.command_in(&other, &["sync"]).output()?;
    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(0), "{stderr}");
    assert_eq!(
        fs::read(other.join("data").join("iris").join("iris.csv"))?,
        V1,
        "{stderr}"
    );
    assert!(
        stderr.contains("data/iris/iris.csv was changed through a link in another project"),
        "{stderr}"
    );
    let edited = fs::read(sandbox.work().join("data").join("iris").join("iris.csv"))?;
    assert_eq!(edited[0], b'S', "the project that made the edit keeps it");
    Ok(())
}

#[test]
fn a_read_only_copy_becomes_writable_when_the_dataset_asks_for_copies() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    // a folder a sync client owns gets read-only copies instead of links
    let added = sandbox
        .command(&["add", &server.url])
        .env("OneDrive", sandbox.work())
        .output()?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    assert!(fs::metadata(&placed)?.permissions().readonly());
    fs::write(
        sandbox.work().join("data.toml"),
        format!("[datasets.iris]\nref = \"{}\"\ncopy = true\n", server.url),
    )?;
    let synced = sandbox
        .command(&["sync"])
        .env("OneDrive", sandbox.work())
        .output()?;
    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    assert!(
        !fs::metadata(&placed)?.permissions().readonly(),
        "{}",
        text(&synced.stderr)?
    );
    assert_eq!(fs::read(&placed)?, V1);
    assert_eq!(server.requests().len(), 1, "the cache still holds iris");
    Ok(())
}
