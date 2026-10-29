//! placing files in the data folder: what fetchloom may delete or replace, and what it keeps.

use std::fs;
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::support::{Part, Result, Sandbox, Server, delete, file, reply, text};

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

/// sets when `path` was last written, leaving its read-only flag as it was.
fn set_time(path: &Path, time: SystemTime) -> Result {
    let before = fs::metadata(path)?.permissions();
    let mut writable = before.clone();
    #[expect(
        clippy::permissions_set_readonly_false,
        reason = "only a writable file takes a new time"
    )]
    writable.set_readonly(false);
    fs::set_permissions(path, writable)?;
    fs::File::options()
        .write(true)
        .open(path)?
        .set_modified(time)?;
    fs::set_permissions(path, before)?;
    Ok(())
}

/// gives iris in `project`, and the state recording it, one whole second as their time, as a
/// clock as coarse as FAT's does when both are written within that second.
fn same_second(project: &Path) -> Result<SystemTime> {
    let placed = project.join("data").join("iris").join("iris.csv");
    let state_path = project.join(".fetchloom").join("state");
    let seconds = fs::metadata(&placed)?
        .modified()?
        .duration_since(UNIX_EPOCH)?
        .as_secs();
    let second = UNIX_EPOCH + Duration::from_secs(seconds);
    let mut state: serde_json::Value = serde_json::from_slice(&fs::read(&state_path)?)?;
    state["files"]["data/iris/iris.csv"]["look"]["modified"] =
        serde_json::json!(u64::try_from(Duration::from_secs(seconds).as_nanos())?);
    fs::write(&state_path, serde_json::to_vec(&state)?)?;
    set_time(&placed, second)?;
    set_time(&state_path, second)?;
    Ok(second)
}

/// a coarse clock gives a file edited right after it was placed the size and time recorded for
/// it. the state written in that same tick tells such a file apart, so it is hashed.
#[test]
fn an_edit_a_coarse_clock_hides_is_found_by_hashing() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(
        "/files/iris.csv",
        vec![file(V1, "\"v1\""), file(V1, "\"v1\"")],
    )?;
    let added = sandbox.run(&["add", &server.url])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let second = same_second(&sandbox.work())?;
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    let mut edited = V1.to_vec();
    edited[0] = b'S';
    edit(&placed, &edited)?;
    set_time(&placed, second)?;

    let status = text(&sandbox.run(&["status"])?.stderr)?;
    assert!(status.contains("Edited data/iris/iris.csv"), "{status}");
    let kept = "data/iris/iris.csv was changed by hand, so it stays";
    let path = text(&sandbox.run(&["path", "iris"])?.stderr)?;
    assert!(
        path.contains(kept),
        "path trusted the file without syncing: {path}"
    );
    // the sync path ran found the object behind a hardlink changed and marked it, which must
    // not turn this project's own edit into one made elsewhere
    let synced = text(&sandbox.run(&["sync"])?.stderr)?;
    assert!(synced.contains(kept), "{synced}");
    assert_eq!(fs::read(&placed)?, edited, "the edit stays without --force");
    let forced = sandbox.run(&["sync", "--force"])?;
    assert_eq!(forced.status.code(), Some(0), "{}", text(&forced.stderr)?);
    assert_eq!(fs::read(&placed)?, V1, "the locked bytes are back");
    // only an edit through a hardlink reaches the cached object, which is then downloaded again
    let links = sandbox.links()?;
    let hardlinked = links.hardlink && !links.reflink;
    assert_eq!(server.requests().len(), if hardlinked { 2 } else { 1 });
    Ok(())
}

/// a file as new as the state is hashed once and settled by writing the state again, after which
/// a sync hashes nothing and writes nothing.
#[test]
fn a_sync_settles_files_as_new_as_the_state_and_then_hashes_nothing() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    let added = sandbox.run(&["add", &server.url])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let second = same_second(&sandbox.work())?;
    // the file settles only once the state is written in a later tick than the file's time,
    // which a clock as coarse as FAT32's takes up to two seconds to reach
    sandbox.wait_for_a_later_time(second)?;
    let state_path = sandbox.work().join(".fetchloom").join("state");
    let traces = sandbox.folder("traces")?;
    let sync = |name: &str| -> Result<String> {
        let trace = traces.join(name);
        let output = sandbox
            .command(&["sync"])
            .env("FETCHLOOM_TRACE", &trace)
            .output()?;
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(0), "{stderr}");
        assert!(stderr.contains("nothing to do"), "{stderr}");
        Ok(fs::read_to_string(trace)?)
    };

    let settling = sync("settling.json")?;
    assert!(settling.contains("\"name\":\"hash_racy\""), "{settling}");
    let settled = fs::metadata(&state_path)?.modified()?;
    assert_ne!(settled, second, "the state is written again");
    let quiet = sync("quiet.json")?;
    assert!(!quiet.contains("\"name\":\"hash_racy\""), "{quiet}");
    assert_eq!(fs::metadata(&state_path)?.modified()?, settled);
    Ok(())
}

#[test]
fn removing_a_dataset_keeps_an_edit_a_coarse_clock_hides() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    let added = sandbox.run(&["add", &server.url])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let second = same_second(&sandbox.work())?;
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    let mut edited = V1.to_vec();
    edited[0] = b'S';
    edit(&placed, &edited)?;
    set_time(&placed, second)?;

    let removed = sandbox.run(&["remove", "iris"])?;

    let stderr = text(&removed.stderr)?;
    assert_eq!(removed.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("data/iris/iris.csv is no longer needed, but it was changed by hand"),
        "{stderr}"
    );
    assert_eq!(fs::read(&placed)?, edited);
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
    // a coarse clock, such as the tick linux stamps files with, can give the file placed last the
    // state's own time, and the first sync settles it by writing the state again in a later tick
    sandbox.wait_for_a_later_time(written(".fetchloom/state")?)?;
    let settled = sandbox.run(&["sync"])?;
    assert_eq!(settled.status.code(), Some(0), "{}", text(&settled.stderr)?);
    let before = (written("data.lock")?, written(".fetchloom/state")?);
    // a rewrite now would carry a later time
    sandbox.wait_for_a_later_time(before.0.max(before.1))?;
    let trace = sandbox.folder("traces")?.join("sync.json");
    let output = sandbox
        .command(&["sync"])
        .env("FETCHLOOM_TRACE", &trace)
        .output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    assert_eq!(
        (written("data.lock")?, written(".fetchloom/state")?),
        before
    );
    let events = fs::read_to_string(&trace)?;
    assert!(!events.contains("\"name\":\"hash_racy\""), "{events}");
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
/// file, with iris then edited through the first project's link. nothing, on a filesystem
/// without hardlinks.
fn shared_and_edited(sandbox: &Sandbox, server: &Server) -> Result<Option<std::path::PathBuf>> {
    if !sandbox.links()?.hardlink {
        return Ok(None);
    }
    let added = sandbox
        .command(&["add", &server.url])
        .env("OneDrive", sandbox.work())
        .output()?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let other = sandbox.folder("other")?;
    for name in ["data.toml", "data.lock"] {
        fs::copy(sandbox.work().join(name), other.join(name))?;
    }
    let synced = sandbox
        .command_in(&other, &["sync"])
        .env("OneDrive", &other)
        .output()?;
    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    for project in [sandbox.work(), other.clone()] {
        link_iris(sandbox, &project)?;
    }
    let mut edited = V1.to_vec();
    edited[0] = b'S';
    edit(
        &sandbox.work().join("data").join("iris").join("iris.csv"),
        &edited,
    )?;
    Ok(Some(other))
}

/// gives the fixture real hardlinks even when the filesystem chose reflinks or copies.
fn link_iris(sandbox: &Sandbox, project: &Path) -> Result {
    let state_path = project.join(".fetchloom").join("state");
    let mut state: serde_json::Value = serde_json::from_slice(&fs::read(&state_path)?)?;
    let path = "data/iris/iris.csv";
    let record = &mut state["files"][path];
    let digest = record["sha256"]
        .as_str()
        .ok_or("a placed file has a digest")?;
    let object = sandbox
        .cache()
        .join("objects")
        .join(&digest[..2])
        .join(&digest[2..]);
    let destination = project.join(path);
    delete(&destination)?;
    fs::hard_link(object, &destination)?;
    let metadata = fs::symlink_metadata(destination)?;
    let modified = u64::try_from(
        metadata
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos(),
    )?;
    record["method"] = serde_json::json!("hardlink");
    record["look"] = serde_json::json!({"size": metadata.len(), "modified": modified});
    record["writable"] = serde_json::json!(false);
    fs::write(state_path, serde_json::to_vec(&state)?)?;
    Ok(())
}

#[test]
fn sync_force_puts_back_a_link_another_project_edited() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(
        "/files/iris.csv",
        vec![file(V1, "\"v1\""), file(V1, "\"v1\"")],
    )?;
    let Some(other) = shared_and_edited(&sandbox, &server)? else {
        return Ok(());
    };
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
    let Some(other) = shared_and_edited(&sandbox, &server)? else {
        return Ok(());
    };
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
    let Some(other) = shared_and_edited(&sandbox, &server)? else {
        return Ok(());
    };
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

/// gives iris in `project` back the modification time its state recorded, as a tool that keeps
/// times would after an edit of the same size.
fn keep_time(project: &Path) -> Result {
    let state: serde_json::Value =
        serde_json::from_slice(&fs::read(project.join(".fetchloom").join("state"))?)?;
    let modified = state["files"]["data/iris/iris.csv"]["look"]["modified"]
        .as_u64()
        .ok_or("a placed file has a time")?;
    fs::File::options()
        .write(true)
        .open(project.join("data").join("iris").join("iris.csv"))?
        .set_modified(std::time::UNIX_EPOCH + Duration::from_nanos(modified))?;
    Ok(())
}

/// an edit through one project's link that keeps the size and time shows in no listing on any
/// platform, so the cache's mark is all the other project has. status, sync and path agree on
/// it: sync puts the locked bytes back, and then nothing differs.
#[test]
fn a_marked_link_whose_size_and_time_stayed_is_put_back() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(
        "/files/iris.csv",
        vec![file(V1, "\"v1\""), file(V1, "\"v1\"")],
    )?;
    let Some(other) = shared_and_edited(&sandbox, &server)? else {
        return Ok(());
    };
    keep_time(&sandbox.work())?;
    let verified = sandbox.run(&["cache", "verify"])?;
    assert_eq!(
        verified.status.code(),
        Some(30),
        "{}",
        text(&verified.stderr)?
    );
    let before = sandbox.command_in(&other, &["--json", "status"]).output()?;
    let before: serde_json::Value = serde_json::from_slice(&before.stdout)?;
    assert_eq!(
        before["result"]["edited_elsewhere"],
        serde_json::json!(["data/iris/iris.csv"])
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
    let after = sandbox.command_in(&other, &["status"]).output()?;
    assert_eq!(text(&after.stderr)?, "✓ everything matches data.lock\n");
    // a ready dataset answers without taking the project lock
    let lock = other.join(".fetchloom").join("lock");
    fs::remove_file(&lock)?;
    let path = sandbox.command_in(&other, &["path", "iris"]).output()?;
    assert_eq!(path.status.code(), Some(0), "{}", text(&path.stderr)?);
    assert!(!lock.exists(), "path synced again");
    Ok(())
}

#[test]
fn a_read_only_file_becomes_writable_when_the_dataset_asks_for_copies() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    // a folder a sync client owns gets no hardlinks, so iris is a read-only reflink or copy
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

/// asking for writable copies after the files were placed read-only leaves them waiting for a
/// sync, which status shows and path does before it answers.
#[test]
fn read_only_files_of_a_dataset_that_asks_for_copies_need_a_sync() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    let added = sandbox.run(&["add", &server.url])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    assert!(fs::metadata(&placed)?.permissions().readonly());
    fs::write(
        sandbox.work().join("data.toml"),
        format!("[datasets.iris]\nref = \"{}\"\ncopy = true\n", server.url),
    )?;

    let status = sandbox.run(&["status"])?;
    let stderr = text(&status.stderr)?;
    assert!(stderr.contains("Outdated data/iris/iris.csv"), "{stderr}");
    let document: serde_json::Value =
        serde_json::from_slice(&sandbox.run(&["--json", "status"])?.stdout)?;
    assert_eq!(
        document["result"]["outdated"],
        serde_json::json!(["data/iris/iris.csv"])
    );
    let path = sandbox.run(&["path", "iris"])?;
    assert_eq!(path.status.code(), Some(0), "{}", text(&path.stderr)?);
    assert!(
        !fs::metadata(&placed)?.permissions().readonly(),
        "path answered before making iris writable"
    );
    assert_eq!(fs::read(&placed)?, V1);
    let after = sandbox.run(&["status"])?;
    assert_eq!(text(&after.stderr)?, "✓ everything matches data.lock\n");
    Ok(())
}

#[test]
fn zero_byte_copies_are_reported_as_copies() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/empty.csv", vec![file(b"", "\"empty\"")])?;
    fs::write(
        sandbox.work().join("data.toml"),
        format!("[datasets.empty]\nref = \"{}\"\ncopy = true\n", server.url),
    )?;
    let output = sandbox
        .command(&["sync", "-v"])
        .env("OneDrive", sandbox.work())
        .output()?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    if sandbox.links()?.reflink {
        assert!(stderr.contains("linked, no extra disk"), "{stderr}");
    } else {
        assert!(stderr.contains("1 file copied, 0 B"), "{stderr}");
        assert!(stderr.contains("1 copy"), "{stderr}");
        assert!(stderr.contains("data is inside onedrive"), "{stderr}");
    }
    assert_eq!(
        fs::metadata(sandbox.work().join("data/empty/empty.csv"))?.len(),
        0
    );
    Ok(())
}
