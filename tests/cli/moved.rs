//! data that moved: when a link data.lock recorded is dead, the source is asked again, and what
//! it points to now must still match the pinned hash.

use std::fs;
use std::path::PathBuf;

use crate::support::{Result, Sandbox, Server, file, reply, text};

const IRIS: &[u8] = b"sepal_length,species\n5.1,setosa\n";
const PATH: &str = "/files/iris.csv";

/// a fresh clone of a project whose data.lock records iris at a link that is now dead, with a
/// cold cache. the server's first response serves `add`, the second answers the dead link and
/// the third serves the link the source lists now.
fn moved(sandbox: &Sandbox, server: &Server) -> Result<PathBuf> {
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
        stderr.contains("iris/iris.csv is gone from its recorded link"),
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
        stderr.contains("is not the file that was locked"),
        "{stderr}"
    );
    assert!(!clone.join("data").join("iris").join("iris.csv").exists());
    Ok(())
}
