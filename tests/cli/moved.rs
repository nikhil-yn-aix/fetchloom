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

#[test]
fn every_file_gone_for_good_is_named_with_where_it_was() -> Result {
    let sandbox = Sandbox::new()?;
    let iris = Server::start(PATH, vec![file(IRIS, "\"v1\""), gone()])?;
    let cows = Server::start(
        "/files/cows.csv",
        vec![file(b"cow,sound\nbess,moo\n", "\"c1\""), gone()],
    )?;
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
