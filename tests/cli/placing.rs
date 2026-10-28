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
