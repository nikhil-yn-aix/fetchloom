//! two fetchloom processes in one project: one does the work while the other waits for it, then
//! uses what it did.

use std::fs;

use crate::support::{Part, Result, Running, Sandbox, Server, file, reply, text, wait_until};

const IRIS: &[u8] = b"sepal_length,species\n5.1,setosa\n";
const PATH: &str = "/files/iris.csv";

#[test]
fn path_waits_for_a_running_sync_and_answers() -> Result {
    let sandbox = Sandbox::new()?;
    let mut slow = reply(
        "200 OK",
        &[
            ("Content-Length", &IRIS.len().to_string()),
            ("ETag", "\"v1\""),
        ],
        b"",
    );
    let (release, held) = std::sync::mpsc::channel();
    slow.push(Part::Wait(held));
    slow.push(Part::Bytes(IRIS.to_vec()));
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), slow])?;
    let added = sandbox.run(&["add", &server.url])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let clone = sandbox.folder("clone")?;
    for name in ["data.toml", "data.lock"] {
        fs::copy(sandbox.work().join(name), clone.join(name))?;
    }
    let cold = sandbox.folder("cold")?;
    let syncing = Running::start(
        sandbox
            .command_in(&clone, &["sync"])
            .env("FETCHLOOM_CACHE", &cold),
    )?;
    wait_until("the sync download request", || {
        Ok(server.requests().len() >= 2)
    })?;
    let path = Running::start(
        sandbox
            .command_in(&clone, &["path", "iris"])
            .env("FETCHLOOM_CACHE", &cold),
    )?;
    let waiting = path.wait_for("Waiting");
    drop(release);
    waiting?;
    let path = path.finish()?;
    let synced = syncing.finish()?;
    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    let story = text(&path.stderr)?;
    assert_eq!(path.status.code(), Some(0), "{story}");
    assert_eq!(
        text(&path.stdout)?.trim(),
        clone.join("data").join("iris").display().to_string()
    );
    assert!(
        story.contains("Waiting") && story.lines().all(|line| line.contains("Waiting")),
        "only the wait is told: {story}"
    );
    Ok(())
}
