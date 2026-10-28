//! two fetchloom processes in one project: one does the work while the other waits for it, then
//! uses what it did.

use std::fs;
use std::process::Stdio;
use std::time::Duration;

use crate::support::{Part, Result, Sandbox, Server, file, reply, text};

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
    slow.push(Part::Pause(Duration::from_millis(2000)));
    slow.push(Part::Bytes(IRIS.to_vec()));
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), slow])?;
    let added = sandbox.run(&["add", &server.url])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let clone = sandbox.folder("clone")?;
    for name in ["data.toml", "data.lock"] {
        fs::copy(sandbox.work().join(name), clone.join(name))?;
    }
    let cold = sandbox.folder("cold")?;
    let syncing = sandbox
        .command_in(&clone, &["sync"])
        .env("FETCHLOOM_CACHE", &cold)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    std::thread::sleep(Duration::from_millis(500));
    let path = sandbox
        .command_in(&clone, &["path", "iris"])
        .env("FETCHLOOM_CACHE", &cold)
        .output()?;
    let synced = syncing.wait_with_output()?;
    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    let story = text(&path.stderr)?;
    assert_eq!(path.status.code(), Some(0), "{story}");
    assert_eq!(
        text(&path.stdout)?.trim(),
        clone.join("data").join("iris").display().to_string()
    );
    assert!(
        story.lines().all(|line| line.contains("Waiting")),
        "only the wait is told: {story}"
    );
    Ok(())
}
