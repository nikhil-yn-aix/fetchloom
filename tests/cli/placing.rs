//! placing files in the data folder: what fetchloom may delete or replace, and what it keeps.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::support::{Part, Result, Sandbox, Server, delete, edit, file, json, reply, text};

const V1: &[u8] = b"sepal_length,species\n5.1,setosa\n";
const V2: &[u8] = b"sepal_length,species\n9.9,new\n";

fn names_alias(sandbox: &Sandbox, original: &str, other: &str) -> Result<bool> {
    let probe = sandbox.folder(".names")?;
    fs::write(probe.join(original), b"names")?;
    let aliases = probe.join(other).try_exists()?;
    fs::remove_dir_all(probe)?;
    Ok(aliases)
}

fn copy_iris(sandbox: &Sandbox, server: &Server) -> Result {
    sandbox.allow(&server.url);
    fs::write(
        sandbox.work().join("data.toml"),
        format!("[datasets.iris]\nref = \"{}\"\ncopy = true\n", server.url),
    )?;
    let output = sandbox.run(&["sync"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    Ok(())
}

fn clean_alias(sandbox: &Sandbox, server: &Server) -> Result {
    let status = sandbox.run(&["status"])?;
    let stderr = text(&status.stderr)?;
    assert_eq!(status.status.code(), Some(0), "{stderr}");
    assert!(stderr.contains("everything matches data.lock"), "{stderr}");
    for args in [
        &["--json", "sync", "--offline"][..],
        &["--json", "sync", "--offline", "--force"][..],
    ] {
        let output = sandbox.run(args)?;
        assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
        let document: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        let placement = &document["result"]["placement"];
        assert_eq!(placement["unchanged"], 1, "{document}");
        for count in ["placed", "replaced", "removed"] {
            assert_eq!(placement[count], 0, "{document}");
        }
        assert_eq!(placement["kept"], serde_json::json!([]), "{document}");
    }
    assert_eq!(server.requests().len(), 1);
    Ok(())
}

#[test]
fn case_alias() -> Result {
    let sandbox = Sandbox::new()?;
    if !names_alias(&sandbox, "iris.csv", "Iris.csv")? {
        return Ok(());
    }
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    copy_iris(&sandbox, &server)?;
    let folder = sandbox.work().join("data").join("iris");
    fs::rename(folder.join("iris.csv"), folder.join("Iris.csv"))?;

    clean_alias(&sandbox, &server)?;
    assert_eq!(fs::read(folder.join("Iris.csv"))?, V1);
    let state_path = sandbox.work().join(".fetchloom").join("state");
    let state: serde_json::Value = serde_json::from_slice(&fs::read(state_path)?)?;
    assert!(state["files"]["data/iris/iris.csv"].is_object());
    Ok(())
}

#[test]
fn force_alias() -> Result {
    let sandbox = Sandbox::new()?;
    if !names_alias(&sandbox, "iris.csv", "Iris.csv")? {
        return Ok(());
    }
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    copy_iris(&sandbox, &server)?;
    let folder = sandbox.work().join("data").join("iris");
    let renamed = folder.join("Iris.csv");
    fs::rename(folder.join("iris.csv"), &renamed)?;
    fs::write(&renamed, V2)?;

    let output = sandbox.run(&["sync", "--offline", "--force"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    assert_eq!(fs::read(&renamed)?, V1);
    clean_alias(&sandbox, &server)
}

#[test]
fn unicode_alias() -> Result {
    let sandbox = Sandbox::new()?;
    let original = "caf\u{e9}.csv";
    let other = "cafe\u{301}.csv";
    if !names_alias(&sandbox, original, other)? {
        return Ok(());
    }
    let server = Server::start("/files/caf%C3%A9.csv", vec![file(V1, "\"v1\"")])?;
    copy_iris(&sandbox, &server)?;
    let folder = sandbox.work().join("data").join("iris");
    fs::rename(folder.join(original), folder.join(other))?;

    clean_alias(&sandbox, &server)?;
    let lock = sandbox.work().join("data.lock");
    fs::write(&lock, fs::read_to_string(&lock)?.replace(original, other))?;
    clean_alias(&sandbox, &server)
}

#[test]
fn data_dir_alias() -> Result {
    let sandbox = Sandbox::new()?;
    if !names_alias(&sandbox, "data", "Data")? {
        return Ok(());
    }
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    copy_iris(&sandbox, &server)?;
    fs::rename(sandbox.work().join("data"), sandbox.work().join("Data"))?;
    clean_alias(&sandbox, &server)?;
    let manifest = sandbox.work().join("data.toml");
    fs::write(
        &manifest,
        format!(
            "[project]\ndata_dir = \"Data\"\n\n{}",
            fs::read_to_string(&manifest)?
        ),
    )?;

    clean_alias(&sandbox, &server)?;
    let state_path = sandbox.work().join(".fetchloom").join("state");
    let state: serde_json::Value = serde_json::from_slice(&fs::read(state_path)?)?;
    assert!(state["files"]["Data/iris/iris.csv"].is_object());
    assert!(state["files"]["data/iris/iris.csv"].is_null());
    Ok(())
}

#[test]
fn locked_alias() -> Result {
    let sandbox = Sandbox::new()?;
    if !names_alias(&sandbox, "iris.csv", "Iris.csv")? {
        return Ok(());
    }
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    copy_iris(&sandbox, &server)?;
    let lock = sandbox.work().join("data.lock");
    fs::write(
        &lock,
        fs::read_to_string(&lock)?.replace("path = \"iris.csv\"", "path = \"Iris.csv\""),
    )?;

    clean_alias(&sandbox, &server)?;
    let state_path = sandbox.work().join(".fetchloom").join("state");
    let state: serde_json::Value = serde_json::from_slice(&fs::read(state_path)?)?;
    assert!(state["files"]["data/iris/Iris.csv"].is_object());
    assert!(state["files"]["data/iris/iris.csv"].is_null());
    Ok(())
}

#[test]
fn alias_removal() -> Result {
    let sandbox = Sandbox::new()?;
    if !names_alias(&sandbox, "iris.csv", "Iris.csv")? {
        return Ok(());
    }
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    copy_iris(&sandbox, &server)?;
    let folder = sandbox.work().join("data").join("iris");
    fs::rename(folder.join("iris.csv"), folder.join("Iris.csv"))?;
    let manifest = sandbox.work().join("data.toml");
    fs::write(
        &manifest,
        format!(
            "[project]\ndata_dir = \"Data\"\n\n{}",
            fs::read_to_string(&manifest)?
        ),
    )?;

    let output = sandbox.run(&["remove", "iris"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    assert!(!folder.join("Iris.csv").try_exists()?);
    assert_eq!(server.requests().len(), 1);
    Ok(())
}

#[test]
fn distinct_names() -> Result {
    for (original, other, url) in [
        ("iris.csv", "Iris.csv", "/files/iris.csv"),
        ("caf\u{e9}.csv", "cafe\u{301}.csv", "/files/caf%C3%A9.csv"),
    ] {
        let sandbox = Sandbox::new()?;
        if names_alias(&sandbox, original, other)? {
            continue;
        }
        let server = Server::start(url, vec![file(V1, "\"v1\"")])?;
        copy_iris(&sandbox, &server)?;
        let folder = sandbox.work().join("data").join("iris");
        fs::rename(folder.join(original), folder.join(other))?;

        let status = sandbox.run(&["--json", "status"])?;
        assert_eq!(status.status.code(), Some(0), "{}", text(&status.stderr)?);
        let document: serde_json::Value = serde_json::from_slice(&status.stdout)?;
        assert_eq!(
            document["result"]["missing"],
            serde_json::json!([format!("data/iris/{original}")])
        );
        assert_eq!(
            document["result"]["extra"],
            serde_json::json!([format!("data/iris/{other}")])
        );
        let output = sandbox.run(&["sync", "--offline", "--force"])?;
        assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
        assert_eq!(fs::read(folder.join(original))?, V1);
        assert_eq!(fs::read(folder.join(other))?, V1);
        let status: serde_json::Value =
            serde_json::from_slice(&sandbox.run(&["--json", "status"])?.stdout)?;
        assert_eq!(status["result"]["missing"], serde_json::json!([]));
        assert_eq!(
            status["result"]["extra"],
            serde_json::json!([format!("data/iris/{other}")])
        );
        assert_eq!(server.requests().len(), 1);
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn distinct_inodes_do_not_alias() -> Result {
    let sandbox = Sandbox::new()?;
    if names_alias(&sandbox, "data", "Data")? {
        return Ok(());
    }
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    copy_iris(&sandbox, &server)?;
    let original = sandbox.work().join("data").join("iris").join("iris.csv");
    let folder = sandbox.work().join("Data").join("iris");
    fs::create_dir_all(&folder)?;
    let other = folder.join("iris.csv");
    let mut edited = V1.to_vec();
    edited[0] = b'S';
    fs::write(&other, edited)?;
    let modified = fs::metadata(&original)?.modified()?;
    set_time(&other, modified)?;
    assert_eq!(fs::metadata(&other)?.modified()?, modified);
    set_time(
        &sandbox.work().join(".fetchloom").join("state"),
        modified + Duration::from_secs(10),
    )?;
    let manifest = sandbox.work().join("data.toml");
    fs::write(
        &manifest,
        format!(
            "[project]\ndata_dir = \"Data\"\n\n{}",
            fs::read_to_string(&manifest)?
        ),
    )?;

    let status = sandbox.run(&["--json", "status"])?;
    assert_eq!(status.status.code(), Some(0), "{}", text(&status.stderr)?);
    let document: serde_json::Value = serde_json::from_slice(&status.stdout)?;
    assert_eq!(
        document["result"]["untracked"],
        serde_json::json!(["Data/iris/iris.csv"])
    );
    assert_eq!(fs::read(original)?, V1);
    Ok(())
}

#[test]
fn ambiguous_aliases() -> Result {
    let sandbox = Sandbox::new()?;
    if names_alias(&sandbox, "data", "Data")? || !sandbox.links()?.hardlink {
        return Ok(());
    }
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    copy_iris(&sandbox, &server)?;
    let original = sandbox.work().join("data").join("iris").join("iris.csv");
    let folder = sandbox.work().join("Data").join("iris");
    fs::create_dir_all(&folder)?;
    fs::hard_link(&original, folder.join("iris.csv"))?;
    fs::write(folder.join("Iris.csv"), V2)?;
    let manifest = sandbox.work().join("data.toml");
    fs::write(
        &manifest,
        format!(
            "[project]\ndata_dir = \"Data\"\n\n{}",
            fs::read_to_string(&manifest)?
        ),
    )?;

    let status = sandbox.run(&["--json", "status"])?;
    assert_eq!(status.status.code(), Some(0), "{}", text(&status.stderr)?);
    let document: serde_json::Value = serde_json::from_slice(&status.stdout)?;
    // no record joins the link, which holds the pinned bytes, so sync adopts it
    assert_eq!(
        document["result"]["adoptable"],
        serde_json::json!(["Data/iris/iris.csv"])
    );
    assert_eq!(
        document["result"]["extra"],
        serde_json::json!(["Data/iris/Iris.csv"])
    );
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
    sandbox.allow(&server.url);
    let added = sandbox.run(&["add", &server.url])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let second = same_second(&sandbox.work())?;
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    let mut edited = V1.to_vec();
    edited[0] = b'S';
    edit(&placed, &edited)?;
    set_time(&placed, second)?;

    let status = text(&sandbox.run(&["status"])?.stdout)?;
    let links = sandbox.links()?;
    if links.hardlinks_read_only() {
        // a hardlink is the cache's own copy, so the edit changed what every project reads, and
        // the sync path runs downloads the file again to put it back
        assert!(status.contains("Damaged data/iris/iris.csv"), "{status}");
        let path = text(&sandbox.run(&["path", "iris"])?.stderr)?;
        assert!(
            path.contains("data/iris/iris.csv was put back as pinned"),
            "path trusted the file without syncing: {path}"
        );
        assert_eq!(fs::read(&placed)?, V1, "the locked bytes are back");
        assert_eq!(server.requests().len(), 2);
        return Ok(());
    }
    assert!(status.contains("Edited data/iris/iris.csv"), "{status}");
    let path = text(&sandbox.run(&["path", "iris"])?.stderr)?;
    assert!(
        path.contains("data/iris/iris.csv has changed"),
        "path trusted the file without syncing: {path}"
    );
    let synced = text(&sandbox.run(&["sync"])?.stderr)?;
    assert!(
        synced.contains("data/iris/iris.csv has changed"),
        "{synced}"
    );
    assert_eq!(fs::read(&placed)?, edited, "the edit stays without --force");
    let forced = sandbox.run(&["sync", "--force"])?;
    assert_eq!(forced.status.code(), Some(0), "{}", text(&forced.stderr)?);
    assert_eq!(fs::read(&placed)?, V1, "the locked bytes are back");
    assert_eq!(
        server.requests().len(),
        1,
        "a project's own copy shares nothing"
    );
    Ok(())
}

/// a copy shares nothing with the cache, so no mark recalls an edit hidden by a coarse clock.
/// a sync that keeps the edit and writes the state must still leave it for --force to undo.
#[test]
fn a_kept_edit_a_coarse_clock_hides_still_yields_to_force() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    sandbox.allow(&server.url);
    fs::write(
        sandbox.work().join("data.toml"),
        format!("[datasets.iris]\nref = \"{}\"\ncopy = true\n", server.url),
    )?;
    let first = sandbox.run(&["sync"])?;
    assert_eq!(first.status.code(), Some(0), "{}", text(&first.stderr)?);
    let second = same_second(&sandbox.work())?;
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    let mut edited = V1.to_vec();
    edited[0] = b'S';
    edit(&placed, &edited)?;
    set_time(&placed, second)?;

    let kept = text(&sandbox.run(&["sync"])?.stderr)?;
    assert!(
        kept.contains("data/iris/iris.csv has changed, so it stays"),
        "{kept}"
    );
    let status = text(&sandbox.run(&["status"])?.stdout)?;
    assert!(status.contains("Edited data/iris/iris.csv"), "{status}");
    let forced = sandbox.run(&["sync", "--force"])?;
    let told = text(&forced.stderr)?;
    assert_eq!(forced.status.code(), Some(0), "{told}");
    // the one line after which the edit is gone says so
    assert!(
        told.starts_with("   Replaced data/iris/iris.csv · discarding your edit\n"),
        "{told}"
    );
    assert_eq!(fs::read(&placed)?, V1, "the locked bytes are back");
    assert_eq!(server.requests().len(), 1, "the cache still holds iris");
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
        stderr.contains("data/iris/iris.csv is no longer needed, but it has changed"),
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
    let (release, held) = std::sync::mpsc::channel();
    let (requested, arrived) = std::sync::mpsc::channel();
    slow.push(Part::Action(Box::new(move || {
        let _ = requested.send(());
    })));
    slow.push(Part::Wait(held));
    slow.push(Part::Bytes(V2.to_vec()));
    let iris = Server::start(
        "/files/iris.csv",
        vec![file(V1, "\"v1\""), file(V2, "\"v2\""), slow],
    )?;
    sandbox.allow(&iris.url);
    let cows = Server::start("/files/cows.csv", vec![file(b"cow\nbess\n", "\"c1\"")])?;
    sandbox.allow(&cows.url);
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
    arrived.recv()?;
    let outdated = sandbox.work().join("data").join("iris").join("iris.csv");
    let unneeded = sandbox.work().join("data").join("cows").join("cows.csv");
    edit(&outdated, b"my notes on iris\n")?;
    edit(&unneeded, b"my notes on cows\n")?;
    drop(release);
    let output = sync.wait_with_output()?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert_eq!(fs::read(&outdated)?, b"my notes on iris\n", "{stderr}");
    assert_eq!(fs::read(&unneeded)?, b"my notes on cows\n", "{stderr}");
    assert!(
        stderr.contains("data/iris/iris.csv has changed, so it stays"),
        "{stderr}"
    );
    assert!(
        stderr.contains("data/cows/cows.csv is no longer needed, but it has changed"),
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
    assert!(!events.contains("\"name\":\"aliases\""), "{events}");
    Ok(())
}

/// after a sync that left every file in place, a sync with data.toml and data.lock unchanged
/// plans nothing while every file looks as placed. an edit still sends it the long way.
#[test]
fn a_settled_project_syncs_without_planning_and_still_finds_edits() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(
        "/files/iris.csv",
        vec![file(V1, "\"v1\""), file(V1, "\"v1\"")],
    )?;
    sandbox.allow(&server.url);
    let added = sandbox.run(&["add", &server.url])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    // the state the first sync writes must be newer than the file, even on a coarse clock
    let state = sandbox.work().join(".fetchloom").join("state");
    sandbox.wait_for_a_later_time(fs::metadata(&state)?.modified()?)?;
    let traces = sandbox.folder("traces")?;
    let sync = |name: &str| -> Result<(String, String)> {
        let trace = traces.join(name);
        let output = sandbox
            .command(&["sync"])
            .env("FETCHLOOM_TRACE", &trace)
            .output()?;
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(0), "{stderr}");
        Ok((stderr, fs::read_to_string(trace)?))
    };
    let plans = |events: &str| events.contains("\"name\":\"plan\"");

    let (_, first) = sync("first.json")?;
    // add records data.toml and data.lock as it leaves them, so this sync plans only when a
    // coarse clock left the placed file as new as the state
    assert!(
        !plans(&first) || first.contains("\"name\":\"hash_racy\""),
        "{first}"
    );
    let (said, second) = sync("second.json")?;
    assert!(said.contains("nothing to do"), "{said}");
    assert!(!plans(&second), "{second}");
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    edit(&placed, b"sepal_length,species\n5.1,setosa\nedited\n")?;
    let (said, third) = sync("third.json")?;
    // a hardlink is the cache's own copy, which the edit changed, so it is put back
    let links = sandbox.links()?;
    let told = if links.hardlinks_read_only() {
        "data/iris/iris.csv was put back as pinned"
    } else {
        "data/iris/iris.csv has changed"
    };
    assert!(said.contains(told), "{said}");
    assert!(plans(&third), "{third}");
    Ok(())
}

/// a sync that left a file out records data.toml and data.lock as unsettled, so the next one
/// plans again and still tells about the file, though nothing it recorded changed.
#[test]
fn a_sync_that_left_a_file_out_is_never_taken_as_settled() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    let added = sandbox.run(&["add", &server.url])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let clone = sandbox.folder("clone")?;
    for name in ["data.toml", "data.lock"] {
        fs::copy(sandbox.work().join(name), clone.join(name))?;
    }
    let foreign = clone.join("data").join("iris");
    fs::create_dir_all(&foreign)?;
    fs::write(foreign.join("iris.csv"), V2)?;
    let kept = "data/iris/iris.csv is not a file fetchloom placed, and it differs from data.lock, so it stays";

    for _ in 0..2 {
        let synced = sandbox.command_in(&clone, &["sync"]).output()?;
        let stderr = text(&synced.stderr)?;
        assert_eq!(synced.status.code(), Some(0), "{stderr}");
        assert!(stderr.contains(kept), "{stderr}");
    }
    assert_eq!(fs::read(foreign.join("iris.csv"))?, V2);
    Ok(())
}

/// `add` records data.toml and data.lock as it leaves them, so the next sync can tell nothing
/// changed without planning. a `path` that has to sync syncs its dataset alone, which never
/// settles a project, so the sync after it plans once more, finds nothing to do and settles it.
#[test]
fn add_settles_the_project_and_a_syncing_path_does_not() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    let state = sandbox.work().join(".fetchloom").join("state");
    let settled = || -> Result<bool> {
        let recorded: serde_json::Value = serde_json::from_slice(&fs::read(&state)?)?;
        Ok(recorded["synced"]["complete"] == serde_json::json!(true))
    };
    let sync = |trace: &str| -> Result<(String, String)> {
        let trace = sandbox.folder("traces")?.join(trace);
        let output = sandbox
            .command(&["sync"])
            .env("FETCHLOOM_TRACE", &trace)
            .output()?;
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(0), "{stderr}");
        Ok((stderr, fs::read_to_string(&trace)?))
    };
    let added = sandbox.run(&["add", &server.url])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    assert!(settled()?, "{}", fs::read_to_string(&state)?);
    sandbox.wait_for_a_later_time(fs::metadata(&state)?.modified()?)?;
    let (stderr, events) = sync("after-add.json")?;
    // a coarse clock can leave the placed file as new as the state, which a sync hashes and
    // records again the long way
    if !events.contains("\"name\":\"hash_racy\"") {
        assert!(stderr.contains("nothing to do"), "{stderr}");
        assert!(!events.contains("\"name\":\"plan\""), "{events}");
    }

    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    delete(&placed)?;
    let path = sandbox.run(&["path", "iris"])?;
    assert_eq!(path.status.code(), Some(0), "{}", text(&path.stderr)?);
    assert!(placed.is_file());
    assert!(!settled()?, "{}", fs::read_to_string(&state)?);
    let (stderr, events) = sync("after-path.json")?;

    assert!(stderr.contains("nothing to do"), "{stderr}");
    assert!(events.contains("\"name\":\"plan\""), "{events}");
    assert!(settled()?, "{}", fs::read_to_string(&state)?);
    Ok(())
}

#[test]
fn a_trace_shows_each_phase_of_a_sync() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    let added = sandbox.run(&["add", &server.url])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    // a missing file sends the sync through every phase
    delete(&sandbox.work().join("data").join("iris").join("iris.csv"))?;
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
    let Some(other) = shared(sandbox, server)? else {
        return Ok(None);
    };
    let mut edited = V1.to_vec();
    edited[0] = b'S';
    edit(
        &sandbox.work().join("data").join("iris").join("iris.csv"),
        &edited,
    )?;
    Ok(Some(other))
}

/// a project in `work` and a second one in `other`, both holding iris as a link to one cached
/// file. nothing, on a filesystem without hardlinks.
fn shared(sandbox: &Sandbox, server: &Server) -> Result<Option<std::path::PathBuf>> {
    if !sandbox.links()?.hardlink {
        return Ok(None);
    }
    sandbox.allow(&server.url);
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
    // hfs+ keeps whole seconds and fat two, so a state written in the link's own tick leaves it
    // racy, and hashing would take the edit below for each project's own. states written later
    // vouch for the links as on a finer clock
    let modified =
        fs::metadata(sandbox.work().join("data").join("iris").join("iris.csv"))?.modified()?;
    if modified.duration_since(UNIX_EPOCH)?.subsec_nanos() == 0 {
        sandbox.wait_for_a_later_time(modified)?;
        for project in [sandbox.work(), other.clone()] {
            let state = project.join(".fetchloom").join("state");
            fs::write(&state, fs::read(&state)?)?;
        }
    }
    Ok(Some(other))
}

/// the case a person met: two projects hold one cached file through links, and someone makes it
/// writable and appends to it through the first project's link, which changes the cache's own
/// copy. the other project's status calls that damage to the cached copy, not its own edit, and
/// its plain sync downloads the file again and puts it back. the first project's plain sync then
/// puts its link back too, since it is still the very copy the cache found changed. each keeps a
/// changed copy of its own beside the pinned file, so the edit is not lost, and editing one
/// project's copy changes no other's.
#[test]
fn an_edit_through_a_shared_link_is_put_back_in_every_project() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(
        "/files/iris.csv",
        vec![file(V1, "\"v1\""), file(V1, "\"v1\"")],
    )?;
    let Some(other) = shared(&sandbox, &server)? else {
        return Ok(());
    };
    let mine = sandbox.work().join("data").join("iris").join("iris.csv");
    let theirs = other.join("data").join("iris").join("iris.csv");
    let mut grown = V1.to_vec();
    grown.extend_from_slice(b"local tweak\n");
    edit(&mine, &grown)?;

    let status = sandbox.command_in(&other, &["--json", "status"]).output()?;
    let report: serde_json::Value = serde_json::from_slice(&status.stdout)?;
    assert_eq!(
        report["result"]["damaged"],
        serde_json::json!(["data/iris/iris.csv"]),
        "{report}"
    );
    assert_eq!(
        report["result"]["edited"],
        serde_json::json!([]),
        "{report}"
    );
    let synced = sandbox.command_in(&other, &["sync"]).output()?;
    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(0), "{stderr}");
    assert_eq!(fs::read(&theirs)?, V1, "{stderr}");
    assert!(!stderr.contains("left unchanged"), "{stderr}");
    assert_eq!(
        server.requests().len(),
        2,
        "the changed file is fetched again"
    );

    let synced = sandbox.run(&["sync"])?;
    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(0), "{stderr}");
    assert_eq!(fs::read(&mine)?, V1, "{stderr}");
    assert_eq!(server.requests().len(), 2, "{stderr}");
    for project in [sandbox.work(), other.clone()] {
        assert_eq!(
            fs::read(project.join("data").join("iris").join("iris.csv.changed"))?,
            grown
        );
        only_the_kept_copy_differs(&sandbox, &project)?;
    }
    // each project keeps a changed copy of its own, so a later edit of one changes no other
    let kept = sandbox
        .work()
        .join("data")
        .join("iris")
        .join("iris.csv.changed");
    let theirs = other.join("data").join("iris").join("iris.csv.changed");
    assert!(!same_file::is_same_file(&kept, &theirs)?);
    edit(&kept, b"mine alone\n")?;
    assert_eq!(fs::read(&theirs)?, grown);
    Ok(())
}

/// that status in `project` finds nothing to put back, and only the changed copy sync kept beside
/// iris as an extra file.
fn only_the_kept_copy_differs(sandbox: &Sandbox, project: &Path) -> Result {
    let status = sandbox
        .command_in(project, &["--json", "status"])
        .output()?;
    let report: serde_json::Value = serde_json::from_slice(&status.stdout)?;
    let result = &report["result"];
    assert_eq!(
        result["extra"],
        serde_json::json!(["data/iris/iris.csv.changed"]),
        "{report}"
    );
    for found in [
        "missing",
        "edited",
        "untracked",
        "outdated",
        "blocked",
        "damaged",
    ] {
        assert_eq!(result[found], serde_json::json!([]), "{found}: {report}");
    }
    Ok(())
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

/// an edit through one project's hardlink shows in the other project's status, though on
/// windows the other folder's listing still holds the old size and time.
#[test]
fn an_edit_through_one_link_shows_in_the_other_projects_status() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(
        "/files/iris.csv",
        vec![file(V1, "\"v1\""), file(V1, "\"v1\"")],
    )?;
    let Some(other) = shared_and_edited(&sandbox, &server)? else {
        return Ok(());
    };
    let status = sandbox.command_in(&other, &["--json", "status"]).output()?;
    let document: serde_json::Value = serde_json::from_slice(&status.stdout)?;
    assert!(
        document["result"]
            .to_string()
            .contains("data/iris/iris.csv"),
        "{document}"
    );
    Ok(())
}

/// an edit through one project's hardlink that changes the size leaves the cached object at the
/// wrong size, so a new project downloads the file. the fresh bytes take the object's place.
#[test]
fn a_new_project_gets_the_locked_bytes_after_a_link_elsewhere_changed_size() -> Result {
    let sandbox = Sandbox::new()?;
    if !sandbox.links()?.hardlink {
        return Ok(());
    }
    let server = Server::start(
        "/files/iris.csv",
        vec![file(V1, "\"v1\""), file(V1, "\"v1\"")],
    )?;
    sandbox.allow(&server.url);
    let added = sandbox.run(&["add", &server.url])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    link_iris(&sandbox, &sandbox.work())?;
    let mut grown = V1.to_vec();
    grown.extend_from_slice(b"6.3,edited\n");
    edit(
        &sandbox.work().join("data").join("iris").join("iris.csv"),
        &grown,
    )?;
    let other = sandbox.folder("other")?;
    for name in ["data.toml", "data.lock"] {
        fs::copy(sandbox.work().join(name), other.join(name))?;
    }

    let synced = sandbox.command_in(&other, &["sync"]).output()?;

    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    assert_eq!(
        text(&fs::read(other.join("data").join("iris").join("iris.csv"))?)?,
        text(V1)?
    );
    assert_eq!(server.requests().len(), 2);
    Ok(())
}

/// an edit in place through one project's hardlink that keeps the size, saved as vim's `:w!`
/// saves a read-only file and then sealed again, leaves the cached object looking whole. its time
/// moved, so a new project's sync checks it, downloads the file again and places the locked bytes.
#[test]
fn a_new_project_gets_the_locked_bytes_after_a_link_elsewhere_changed_them_in_place() -> Result {
    let sandbox = Sandbox::new()?;
    if !sandbox.links()?.hardlink {
        return Ok(());
    }
    let server = Server::start(
        "/files/iris.csv",
        vec![file(V1, "\"v1\""), file(V1, "\"v1\"")],
    )?;
    sandbox.allow(&server.url);
    let added = sandbox.run(&["add", &server.url])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    link_iris(&sandbox, &sandbox.work())?;
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    let before = fs::metadata(&placed)?;
    if before.created().is_err() {
        return Ok(());
    }
    let mut changed = V1.to_vec();
    changed[0] = b'X';
    edit(&placed, &changed)?;
    fs::File::options()
        .write(true)
        .open(&placed)?
        .set_modified(before.modified()? + std::time::Duration::from_secs(3600))?;
    let mut permissions = fs::metadata(&placed)?.permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&placed, permissions)?;
    let other = sandbox.folder("other")?;
    for name in ["data.toml", "data.lock"] {
        fs::copy(sandbox.work().join(name), other.join(name))?;
    }

    let synced = sandbox.command_in(&other, &["sync"]).output()?;

    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    assert_eq!(
        text(&fs::read(other.join("data").join("iris").join("iris.csv"))?)?,
        text(V1)?
    );
    assert_eq!(server.requests().len(), 2);
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

/// a link to the cache's copy that an edit through another project's link changed is damaged,
/// in the terminal and in JSON alike, since both mean the pinned bytes are gone and sync puts
/// them back.
#[test]
fn status_files_a_changed_shared_copy_as_damaged_in_both_forms() -> Result {
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
    let stdout = text(&status.stdout)?;
    assert!(
        stdout
            .contains("Damaged data/iris/iris.csv · linked to the cache's copy, which was changed"),
        "{stdout}"
    );
    let report = json(&sandbox.command_in(&other, &["--json", "status"]).output()?)?;
    let result = &report["result"];
    assert_eq!(
        result["damaged"],
        serde_json::json!(["data/iris/iris.csv"]),
        "{report}"
    );
    assert!(result.get("edited_elsewhere").is_none(), "{report}");
    Ok(())
}

/// on windows a folder's listing keeps the old size and time of a hardlink edited through another
/// name, so fetchloom opens each hardlink to see it as it is. the other project then finds the
/// edit at once, as on linux, and its plain sync puts the pinned file back.
#[cfg(windows)]
#[test]
fn another_projects_edit_through_a_link_is_seen_at_once_on_windows() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(
        "/files/iris.csv",
        vec![file(V1, "\"v1\""), file(V1, "\"v1\"")],
    )?;
    let Some(other) = shared_and_edited(&sandbox, &server)? else {
        return Ok(());
    };
    let synced = sandbox.command_in(&other, &["sync"]).output()?;
    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("data/iris/iris.csv was put back as pinned"),
        "{stderr}"
    );
    assert_eq!(
        fs::read(other.join("data").join("iris").join("iris.csv"))?,
        V1
    );
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

/// a link to the cache's copy that someone made writable, but did not edit, leaves the copy every
/// project shares open to an edit. status shows it, and a plain sync makes it read-only again,
/// checking its bytes, with no download.
#[test]
fn a_link_made_writable_is_sealed_again() -> Result {
    let sandbox = Sandbox::new()?;
    if !sandbox.links()?.hardlink {
        return Ok(());
    }
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    let added = sandbox.run(&["add", &server.url])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    link_iris(&sandbox, &sandbox.work())?;
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    let mut permissions = fs::metadata(&placed)?.permissions();
    #[expect(
        clippy::permissions_set_readonly_false,
        reason = "the test opens a placed link to edits as a person can"
    )]
    permissions.set_readonly(false);
    fs::set_permissions(&placed, permissions)?;

    let status = sandbox.run(&["--json", "status"])?;
    let report: serde_json::Value = serde_json::from_slice(&status.stdout)?;
    assert_eq!(
        report["result"]["outdated"],
        serde_json::json!(["data/iris/iris.csv"]),
        "{report}"
    );
    let synced = sandbox.run(&["sync"])?;

    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    assert!(fs::metadata(&placed)?.permissions().readonly());
    assert_eq!(fs::read(&placed)?, V1);
    assert_eq!(server.requests().len(), 1, "the cache still holds iris");
    Ok(())
}

/// an edit through a shared link that keeps the size and time leaves no mark, so only hashing
/// finds it, and a plain sync could not: `status --verify` points to `sync --verify` alone.
#[test]
fn status_points_to_sync_verify_for_changes_only_hashing_finds() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(
        "/files/iris.csv",
        vec![file(V1, "\"v1\""), file(V1, "\"v1\"")],
    )?;
    let Some(other) = shared_and_edited(&sandbox, &server)? else {
        return Ok(());
    };
    keep_time(&sandbox.work())?;

    let status = sandbox
        .command_in(&other, &["status", "--verify"])
        .output()?;

    let stdout = text(&status.stdout)?;
    assert!(
        stdout.contains("Damaged data/iris/iris.csv · linked to the cache's copy"),
        "{stdout}"
    );
    let stderr = text(&status.stderr)?;
    assert!(stderr.contains("`fetchloom sync --verify`"), "{stderr}");
    assert!(!stderr.contains("`fetchloom sync` places"), "{stderr}");
    Ok(())
}

/// an edit through one project's link that keeps the size and time shows in no listing on any
/// platform, so the cache's mark is all the other project has. status, sync and path agree on
/// it: sync puts the locked bytes back and keeps the changed copy beside them, which is then all
/// that differs.
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
        before["result"]["damaged"],
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
        stderr.contains("data/iris/iris.csv was put back as pinned"),
        "{stderr}"
    );
    let mut edited = V1.to_vec();
    edited[0] = b'S';
    assert_eq!(
        fs::read(other.join("data").join("iris").join("iris.csv.changed"))?,
        edited
    );
    only_the_kept_copy_differs(&sandbox, &other)?;
    // a ready dataset answers without taking the project lock
    let lock = other.join(".fetchloom").join("lock");
    fs::remove_file(&lock)?;
    let path = sandbox.command_in(&other, &["path", "iris"]).output()?;
    assert_eq!(path.status.code(), Some(0), "{}", text(&path.stderr)?);
    assert!(!lock.exists(), "path synced again");
    Ok(())
}

/// a coarse clock can give a project's state the same time as the link it records, and then an
/// edit that keeps the size and time is racy: hashing finds it. the link is still the cache's
/// copy, so the edit changed what every project sharing it reads, and a plain sync puts the
/// pinned file back.
#[test]
fn a_racy_edit_through_a_shared_link_is_put_back() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(
        "/files/iris.csv",
        vec![file(V1, "\"v1\""), file(V1, "\"v1\"")],
    )?;
    if shared_and_edited(&sandbox, &server)?.is_none() {
        return Ok(());
    }
    keep_time(&sandbox.work())?;
    let state = sandbox.work().join(".fetchloom").join("state");
    let recorded: serde_json::Value = serde_json::from_slice(&fs::read(&state)?)?;
    let modified = recorded["files"]["data/iris/iris.csv"]["look"]["modified"]
        .as_u64()
        .ok_or("a placed file has a time")?;
    set_time(&state, UNIX_EPOCH + Duration::from_nanos(modified))?;
    let verified = sandbox.run(&["cache", "verify"])?;
    assert_eq!(
        verified.status.code(),
        Some(30),
        "{}",
        text(&verified.stderr)?
    );
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");

    let synced = sandbox.run(&["sync"])?;

    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(0), "{stderr}");
    assert_eq!(fs::read(&placed)?, V1, "{stderr}");
    assert!(
        stderr.contains("data/iris/iris.csv was put back as pinned"),
        "{stderr}"
    );
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

#[test]
fn a_recovered_hardlink_becomes_writable_when_copies_are_requested() -> Result {
    let sandbox = Sandbox::new()?;
    if !sandbox.links()?.hardlink {
        return Ok(());
    }
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    let added = sandbox
        .command(&["add", &server.url])
        .env("OneDrive", sandbox.work())
        .output()?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    link_iris(&sandbox, &sandbox.work())?;
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    assert!(fs::metadata(&placed)?.permissions().readonly());
    fs::write(
        sandbox.work().join("data.toml"),
        format!("[datasets.iris]\nref = \"{}\"\ncopy = true\n", server.url),
    )?;
    let before = fs::metadata(&placed)?.modified()?;
    set_time(&placed, before - Duration::from_secs(10))?;
    assert_ne!(fs::metadata(&placed)?.modified()?, before);

    let synced = sandbox.run(&["sync"])?;

    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(0), "{stderr}");
    assert!(
        !fs::metadata(&placed)?.permissions().readonly(),
        "recovery kept a read-only hardlink despite copy = true: {stderr}"
    );
    assert_eq!(fs::read(&placed)?, V1);
    assert_eq!(server.requests().len(), 1, "the cache still holds iris");
    let status = sandbox.run(&["status"])?;
    let stderr = text(&status.stderr)?;
    assert_eq!(status.status.code(), Some(0), "{stderr}");
    assert!(stderr.contains("everything matches data.lock"), "{stderr}");
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
    let stdout = text(&status.stdout)?;
    assert!(stdout.contains("Outdated data/iris/iris.csv"), "{stdout}");
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
    assert_eq!(
        text(&after.stderr)?,
        "         Ok everything matches data.lock by size and time\n"
    );
    Ok(())
}

#[test]
fn sync_reports_zero_byte_writable_copies_in_a_synced_folder() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/empty.csv", vec![file(b"", "\"empty\"")])?;
    sandbox.allow(&server.url);
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
        assert!(stderr.contains("linked to the cache"), "{stderr}");
    } else {
        assert!(stderr.contains("1 file copied, 0 B"), "{stderr}");
        assert!(stderr.contains("1 copy"), "{stderr}");
        // copy = true asks for copies, so the sync client is not what made them
        assert!(!stderr.contains("data is inside onedrive"), "{stderr}");
    }
    assert_eq!(
        fs::metadata(sandbox.work().join("data/empty/empty.csv"))?.len(),
        0
    );
    Ok(())
}

/// a run cut off between placing a link and writing the state leaves a file the next run adopts.
/// it is the cached object under another name, so the state records it as a hardlink, and an
/// edit through it is audited like any placed link.
#[test]
fn an_adopted_link_is_recorded_as_a_hardlink() -> Result {
    let sandbox = Sandbox::new()?;
    if !sandbox.links()?.hardlink {
        return Ok(());
    }
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    let added = sandbox.run(&["add", &server.url])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    link_iris(&sandbox, &sandbox.work())?;
    let state_path = sandbox.work().join(".fetchloom").join("state");
    fs::remove_file(&state_path)?;

    let synced = sandbox.run(&["sync"])?;

    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    let state: serde_json::Value = serde_json::from_slice(&fs::read(&state_path)?)?;
    assert_eq!(
        state["files"]["data/iris/iris.csv"]["method"],
        serde_json::json!("hardlink")
    );
    Ok(())
}

/// a project where iris was saved by hand at its place before `add`, which then downloaded it
/// to learn its hash, since the link publishes no checksum. the stderr of that add.
fn adopted_iris(sandbox: &Sandbox, server: &Server) -> Result<String> {
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    fs::create_dir_all(placed.parent().ok_or("a placed file has a folder")?)?;
    fs::write(&placed, V1)?;
    let added = sandbox.run(&["add", &server.url])?;
    let stderr = text(&added.stderr)?;
    assert_eq!(added.status.code(), Some(0), "{stderr}");
    assert_eq!(fs::read(&placed)?, V1, "{stderr}");
    Ok(stderr)
}

/// a file already at its place with the pinned bytes is taken as placed. where the disk makes
/// hardlinks it becomes a link to the downloaded copy, so one copy stays on disk, and the run
/// says so. elsewhere it stays as it is.
#[test]
fn a_file_already_in_place_is_linked_to_the_download_it_matches() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    let stderr = adopted_iris(&sandbox, &server)?;
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    let linked = same_file::is_same_file(&placed, iris_object(&sandbox, &sandbox.work())?)?;
    if sandbox.links()?.hardlink {
        assert!(linked, "{stderr}");
        assert!(fs::metadata(&placed)?.permissions().readonly());
        assert!(
            stderr.contains(
                "Adopted data/iris · 1 file already held the pinned bytes, so it is now linked to the cache"
            ),
            "{stderr}"
        );
    } else {
        assert!(!linked, "{stderr}");
        assert!(
            stderr.contains(
                "Adopted data/iris · 1 file already held the pinned bytes, so it stays as it is"
            ),
            "{stderr}"
        );
    }
    let status = sandbox.run(&["status"])?;
    assert_eq!(
        text(&status.stderr)?,
        "         Ok everything matches data.lock by size and time\n"
    );
    assert_eq!(server.requests().len(), 1);
    Ok(())
}

/// three datasets moved into one folder, `raw`, that already holds a copy of each one's file, as
/// files fetched by hand before the move. a plain sync adopts them, so status says so rather than
/// offering `--force`, and sync tells the folder once rather than once for each dataset.
#[test]
fn files_already_in_a_shared_folder_are_adopted_and_told_once() -> Result {
    let sandbox = Sandbox::new()?;
    let tables: [(&str, &[u8]); 3] = [("a", b"a,1\n"), ("b", b"b,2\n"), ("c", b"c,3\n")];
    let routes = crate::support::Routes::start(
        tables
            .iter()
            .map(|(name, bytes)| (format!("/{name}.csv"), file(bytes, "\"1\"")))
            .collect(),
    )?;
    let manifest = sandbox.work().join("data.toml");
    let raw = sandbox.work().join("data").join("raw");
    fs::create_dir_all(&raw)?;
    for (name, _) in tables {
        let added = sandbox.run(&["add", &format!("{}/{name}.csv", routes.url)])?;
        assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    }
    for (name, bytes) in tables {
        let table = format!("[datasets.{name}]\n");
        let written = fs::read_to_string(&manifest)?;
        fs::write(
            &manifest,
            written.replace(&table, &format!("{table}path = \"raw\"\n")),
        )?;
        fs::write(raw.join(format!("{name}.csv")), bytes)?;
    }
    let status = sandbox.run(&["status"])?;
    let told = text(&status.stdout)?;
    for name in ["a", "b", "c"] {
        let line = format!(
            "  Untracked data/raw/{name}.csv · it holds the pinned bytes, so `fetchloom sync` adopts it\n"
        );
        assert!(told.contains(&line), "{line:?} in\n{told}");
    }
    let story = text(&status.stderr)?;
    assert!(!format!("{told}{story}").contains("--force"), "{story}");
    let status = json(&sandbox.run(&["--json", "status"])?)?;
    assert_eq!(status["result"]["untracked"], serde_json::json!([]));
    assert_eq!(
        status["result"]["adoptable"],
        serde_json::json!(["data/raw/a.csv", "data/raw/b.csv", "data/raw/c.csv"])
    );
    let synced = sandbox.run(&["sync"])?;
    let told = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(0), "{told}");
    assert_eq!(told.matches("Adopted data/raw · ").count(), 1, "{told}");
    assert!(
        told.contains("Adopted data/raw · 3 files already held the pinned bytes, so they "),
        "{told}"
    );
    Ok(())
}

/// after a pull that took a dataset out of data.toml and data.lock, its folder holds the file
/// fetchloom placed, which the next sync removes, and notes of the person's own, which it keeps.
/// status tells them apart, and sync does as status said.
#[test]
fn status_tells_the_files_sync_removes_from_your_own() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = crate::support::Routes::start(vec![
        ("/tips.csv".to_owned(), file(b"tip,1\n", "\"1\"")),
        ("/penguins.csv".to_owned(), file(b"bill,39\n", "\"1\"")),
    ])?;
    let work = sandbox.work();
    let add = |name: &str| -> Result {
        let added = sandbox.run(&["add", &format!("{}/{name}.csv", routes.url)])?;
        assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
        Ok(())
    };
    add("tips")?;
    let pulled: Vec<(PathBuf, Vec<u8>)> = ["data.toml", "data.lock"]
        .iter()
        .map(|name| Ok((work.join(name), fs::read(work.join(name))?)))
        .collect::<Result<_>>()?;
    add("penguins")?;
    let folder = work.join("data").join("penguins");
    fs::write(folder.join("notes.txt"), b"mine\n")?;
    for (path, bytes) in &pulled {
        fs::write(path, bytes)?;
    }
    let status = sandbox.run(&["status"])?;
    let told = text(&status.stdout)?;
    for line in [
        "      Extra data/penguins/notes.txt\n",
        "      Extra data/penguins/penguins.csv · fetchloom placed it and no dataset needs it now, so `fetchloom sync` removes it\n",
    ] {
        assert!(told.contains(line), "{line:?} in\n{told}");
    }
    let hint =
        "  hint: `fetchloom sync` removes the files fetchloom placed that no dataset needs now\n";
    assert!(text(&status.stderr)?.contains(hint), "{hint:?}");
    let status = json(&sandbox.run(&["--json", "status"])?)?;
    assert_eq!(
        (&status["result"]["extra"], &status["result"]["unneeded"]),
        (
            &serde_json::json!(["data/penguins/notes.txt"]),
            &serde_json::json!(["data/penguins/penguins.csv"])
        )
    );
    let synced = sandbox.run(&["sync"])?;
    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    assert!(!folder.join("penguins.csv").exists());
    assert_eq!(fs::read(folder.join("notes.txt"))?, b"mine\n");
    Ok(())
}

/// removing a dataset deletes a file sync took as placed when it was already there, as it
/// deletes the files sync placed, and says so, in a dry run too, since the person put it there.
#[test]
fn remove_says_it_deletes_a_file_that_was_there_before_it_was_adopted() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    adopted_iris(&sandbox, &server)?;
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");

    let dry = json(&sandbox.run(&["--json", "remove", "iris", "--dry-run"])?)?;
    assert_eq!(
        dry["warnings"],
        serde_json::json!([
            "would remove data/iris/iris.csv, a file that was already there when fetchloom adopted it"
        ]),
        "{dry}"
    );
    assert_eq!(fs::read(&placed)?, V1);
    let removed = sandbox.run(&["remove", "iris"])?;

    let stderr = text(&removed.stderr)?;
    assert_eq!(removed.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains(
            "warning: removed data/iris/iris.csv, a file that was already there when fetchloom adopted it"
        ),
        "{stderr}"
    );
    assert!(!placed.exists(), "{stderr}");
    Ok(())
}

/// the cached object behind iris in `project`, by the digest its state records.
fn iris_object(sandbox: &Sandbox, project: &Path) -> Result<std::path::PathBuf> {
    let state: serde_json::Value =
        serde_json::from_slice(&fs::read(project.join(".fetchloom").join("state"))?)?;
    let digest = state["files"]["data/iris/iris.csv"]["sha256"]
        .as_str()
        .ok_or("a placed file has a digest")?;
    Ok(sandbox
        .cache()
        .join("objects")
        .join(&digest[..2])
        .join(&digest[2..]))
}

/// marks `object` as changed, as `cache verify` does before a new copy replaces its bytes.
fn mark(sandbox: &Sandbox, object: &Path) -> Result {
    let fan = object
        .parent()
        .and_then(Path::file_name)
        .ok_or("an object sits in a fan folder")?;
    let rest = object.file_name().ok_or("an object has a name")?;
    let folder = sandbox.cache().join("changed");
    fs::create_dir_all(&folder)?;
    fs::write(
        folder.join(format!(
            "{}{}",
            fan.to_string_lossy(),
            rest.to_string_lossy()
        )),
        b"",
    )?;
    Ok(())
}

/// keeps `path` from being removed while the result lives, as a program holding it open does:
/// windows refuses to delete a file opened without sharing deletion.
#[cfg(windows)]
fn hold(path: &Path) -> Result<Option<fs::File>> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_SHARE_READ: u32 = 1;
    Ok(Some(
        fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .open(path)?,
    ))
}

/// a folder nobody may write to while it lives.
#[cfg(unix)]
struct Unwritable(std::path::PathBuf);

#[cfg(unix)]
impl Drop for Unwritable {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o755));
    }
}

/// keeps `path` from being removed while the result lives: unix refuses to remove a file from a
/// folder nobody may write to. none for root, who may remove it anyway.
#[cfg(unix)]
fn hold(path: &Path) -> Result<Option<Unwritable>> {
    use std::os::unix::fs::PermissionsExt;
    let folder = path
        .parent()
        .ok_or("a file sits in a folder")?
        .to_path_buf();
    fs::set_permissions(&folder, fs::Permissions::from_mode(0o555))?;
    let held = Unwritable(folder.clone());
    let probe = folder.join("probe");
    if fs::write(&probe, b"").is_ok() {
        let _ = fs::remove_file(probe);
        return Ok(None);
    }
    Ok(Some(held))
}

/// a mark on an object still in the cache, as `cache verify` leaves one awaiting repair, whose
/// bytes changed while its size and time stayed. sync hashes the object before it places anything
/// from it, so the project sharing the link gets the locked bytes from a new download instead of
/// the changed object linked again.
#[test]
fn a_marked_object_still_in_the_cache_is_never_placed_again() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(
        "/files/iris.csv",
        vec![file(V1, "\"v1\""), file(V1, "\"v1\"")],
    )?;
    let Some(other) = shared_and_edited(&sandbox, &server)? else {
        return Ok(());
    };
    keep_time(&sandbox.work())?;
    let object = iris_object(&sandbox, &other)?;
    mark(&sandbox, &object)?;

    let synced = sandbox.command_in(&other, &["sync"]).output()?;

    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(0), "{stderr}");
    assert_eq!(
        fs::read(other.join("data").join("iris").join("iris.csv"))?,
        V1,
        "{stderr}"
    );
    assert_eq!(
        server.requests().len(),
        2,
        "the changed object is fetched again"
    );
    assert_eq!(
        fs::read(&object)?,
        V1,
        "the cache holds the locked bytes again"
    );
    Ok(())
}

/// a project that copies its files gets the locked bytes too, never a copy of the marked object.
#[test]
fn a_copy_is_never_made_from_a_marked_object() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(
        "/files/iris.csv",
        vec![file(V1, "\"v1\""), file(V1, "\"v1\"")],
    )?;
    let Some(other) = shared_and_edited(&sandbox, &server)? else {
        return Ok(());
    };
    keep_time(&sandbox.work())?;
    mark(&sandbox, &iris_object(&sandbox, &other)?)?;
    let copier = sandbox.folder("copier")?;
    fs::write(
        copier.join("data.toml"),
        format!("[datasets.iris]\nref = \"{}\"\ncopy = true\n", server.url),
    )?;
    fs::copy(sandbox.work().join("data.lock"), copier.join("data.lock"))?;

    let synced = sandbox.command_in(&copier, &["sync"]).output()?;

    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(0), "{stderr}");
    assert_eq!(
        fs::read(copier.join("data").join("iris").join("iris.csv"))?,
        V1,
        "{stderr}"
    );
    assert_eq!(
        server.requests().len(),
        2,
        "the changed object is fetched again"
    );
    Ok(())
}

/// a marked object another program holds open cannot be replaced. sync names the refused object
/// and leaves its old bytes in place.
#[test]
fn a_marked_object_that_cannot_be_replaced_stops_sync() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(
        "/files/iris.csv",
        vec![file(V1, "\"v1\""), file(V1, "\"v1\"")],
    )?;
    let Some(other) = shared_and_edited(&sandbox, &server)? else {
        return Ok(());
    };
    keep_time(&sandbox.work())?;
    let object = iris_object(&sandbox, &other)?;
    mark(&sandbox, &object)?;
    let Some(held) = hold(&object)? else {
        return Ok(());
    };

    let synced = sandbox.command_in(&other, &["sync"]).output()?;

    drop(held);
    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(50), "{stderr}");
    let name = object
        .file_name()
        .ok_or("an object has a name")?
        .to_string_lossy()
        .into_owned();
    assert!(stderr.contains(&name), "{stderr}");
    assert!(!stderr.contains("put back"), "{stderr}");
    Ok(())
}

#[cfg(windows)]
#[test]
fn an_unreadable_marked_link_stops_sync_without_replacing_it() -> Result {
    use std::os::windows::fs::OpenOptionsExt;

    let sandbox = Sandbox::new()?;
    if !sandbox.links()?.hardlink {
        return Ok(());
    }
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    let added = sandbox
        .command(&["add", &server.url])
        .env("OneDrive", sandbox.work())
        .output()?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    link_iris(&sandbox, &sandbox.work())?;
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    let state_path = sandbox.work().join(".fetchloom").join("state");
    let state = fs::read(&state_path)?;
    sandbox.wait_for_a_later_time(fs::metadata(&state_path)?.modified()?)?;
    mark(&sandbox, &iris_object(&sandbox, &sandbox.work())?)?;
    let held = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&placed)?;

    let synced = sandbox.run(&["sync"])?;
    let status = sandbox.run(&["status"])?;

    drop(held);
    assert_eq!(fs::read(&placed)?, V1, "the unreadable file must stay");
    assert_eq!(fs::read(&state_path)?, state, "the state must stay");
    for output in [synced, status] {
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(50), "{stderr}");
        assert!(
            stderr.contains("could not read data/iris/iris.csv to check it"),
            "missing 'could not read data/iris/iris.csv to check it': {stderr}"
        );
    }
    Ok(())
}

/// a link may name an older copy of an object than the one that changed, such as one placed
/// before gc freed the object and a download brought it back. sync hashes the project's own file
/// once, keeps it without downloading, and nothing differs afterwards.
#[test]
fn a_link_that_still_holds_the_locked_bytes_is_kept_after_a_mark() -> Result {
    let sandbox = Sandbox::new()?;
    if !sandbox.links()?.hardlink {
        return Ok(());
    }
    let server = Server::start(
        "/files/iris.csv",
        vec![file(V1, "\"v1\""), file(V1, "\"v1\"")],
    )?;
    sandbox.allow(&server.url);
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
    link_iris(&sandbox, &other)?;
    // gc frees an object no registered project needs, and the link keeps its bytes
    let object = iris_object(&sandbox, &other)?;
    delete(&object)?;
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    delete(&placed)?;
    let again = sandbox
        .command(&["sync"])
        .env("OneDrive", sandbox.work())
        .output()?;
    assert_eq!(again.status.code(), Some(0), "{}", text(&again.stderr)?);
    link_iris(&sandbox, &sandbox.work())?;
    let mut edited = V1.to_vec();
    edited[0] = b'S';
    edit(&placed, &edited)?;
    keep_time(&sandbox.work())?;
    mark(&sandbox, &object)?;

    let synced = sandbox
        .command_in(&other, &["--offline", "sync"])
        .output()?;

    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(0), "{stderr}");
    assert_eq!(
        fs::read(other.join("data").join("iris").join("iris.csv"))?,
        V1,
        "{stderr}"
    );
    let status = sandbox.command_in(&other, &["status"]).output()?;
    assert_eq!(
        text(&status.stderr)?,
        "         Ok everything matches data.lock by size and time\n"
    );
    Ok(())
}

/// get hashes a marked object before it takes it too, so it places the locked bytes from a new
/// download instead of the changed object.
#[test]
fn get_never_takes_a_marked_object() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(
        "/files/iris.csv",
        vec![file(V1, "\"v1\""), file(V1, "\"v1\"")],
    )?;
    let Some(other) = shared_and_edited(&sandbox, &server)? else {
        return Ok(());
    };
    keep_time(&sandbox.work())?;
    mark(&sandbox, &iris_object(&sandbox, &other)?)?;
    let folder = sandbox.folder("got")?;

    let got = sandbox.run(&["get", &server.url, "--dir", &folder.display().to_string()])?;

    let stderr = text(&got.stderr)?;
    assert_eq!(got.status.code(), Some(0), "{stderr}");
    assert_eq!(fs::read(folder.join("iris.csv"))?, V1, "{stderr}");
    Ok(())
}

#[test]
fn a_new_project_reseals_an_object_made_writable_through_a_link() -> Result {
    let sandbox = Sandbox::new()?;
    if !sandbox.links()?.hardlink {
        return Ok(());
    }
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    let added = sandbox.run(&["add", &server.url])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    link_iris(&sandbox, &sandbox.work())?;
    let first = sandbox.work().join("data").join("iris").join("iris.csv");
    let mut permissions = fs::metadata(&first)?.permissions();
    #[expect(
        clippy::permissions_set_readonly_false,
        reason = "the test models a user clearing a hardlink's flag"
    )]
    permissions.set_readonly(false);
    fs::set_permissions(&first, permissions)?;
    let other = sandbox.folder("second-project")?;
    for name in ["data.toml", "data.lock"] {
        fs::copy(sandbox.work().join(name), other.join(name))?;
    }

    let synced = sandbox
        .command_in(&other, &["sync", "--offline"])
        .output()?;

    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    let placed = other.join("data").join("iris").join("iris.csv");
    assert!(
        fs::metadata(&placed)?.permissions().readonly(),
        "the second project's input must be read-only"
    );
    assert_eq!(fs::read(&placed)?, V1);
    assert!(
        fs::metadata(iris_object(&sandbox, &other)?)?
            .permissions()
            .readonly()
    );
    assert_eq!(server.requests().len(), 1);
    Ok(())
}

#[test]
fn a_later_get_conflict_preserves_earlier_files() -> Result {
    use std::io::Write as _;
    let sandbox = Sandbox::new()?;
    let original = Server::start("/a.csv", vec![crate::support::file(b"old-a", "\"old\"")])?;
    let fetched = sandbox.run(&["get", &original.url, "--dir", "out"])?;
    assert!(fetched.status.success(), "{}", text(&fetched.stderr)?);
    fs::write(sandbox.work().join("out/b.csv"), b"foreign")?;
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, bytes) in [("a.csv", b"new-a"), ("b.csv", b"new-b")] {
        zip.start_file(name, zip::write::SimpleFileOptions::default())?;
        zip.write_all(bytes)?;
    }
    let bytes = zip.finish()?.into_inner();
    let archive = Server::start("/files.zip", vec![crate::support::file(&bytes, "\"new\"")])?;
    let output = sandbox.run(&["get", &archive.url, "--dir", "out", "--json"])?;
    assert_eq!(output.status.code(), Some(50), "{}", text(&output.stderr)?);
    let document = crate::support::json(&output)?;
    assert_eq!(document["errors"][0]["kind"], "local.not_empty");
    assert_eq!(fs::read(sandbox.work().join("out/b.csv"))?, b"foreign");
    assert_eq!(
        fs::read(sandbox.work().join("out/a.csv")).ok().as_deref(),
        Some(b"old-a".as_slice()),
        "{document}"
    );
    Ok(())
}

/// a link someone put where data.lock places a file is in the way: status calls it blocked
/// rather than missing, a plain sync leaves it with a warning, and `--force` replaces the link
/// itself, never what it points to.
#[cfg(unix)]
#[test]
fn force_replaces_a_link_at_a_pinned_path_without_following_it() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    let added = sandbox.run(&["add", &server.url])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    let precious = sandbox.folder("important")?.join("thesis.txt");
    fs::write(&precious, b"precious")?;
    delete(&placed)?;
    std::os::unix::fs::symlink(&precious, &placed)?;

    let status = sandbox.run(&["--json", "status"])?;
    let report: serde_json::Value = serde_json::from_slice(&status.stdout)?;
    assert_eq!(
        report["result"]["blocked"],
        serde_json::json!(["data/iris/iris.csv"]),
        "{report}"
    );
    assert_eq!(
        report["result"]["missing"],
        serde_json::json!([]),
        "{report}"
    );
    let kept = sandbox.run(&["sync"])?;
    let stderr = text(&kept.stderr)?;
    assert_eq!(kept.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("data/iris/iris.csv is a link where data.lock places a file"),
        "{stderr}"
    );
    assert!(fs::symlink_metadata(&placed)?.file_type().is_symlink());
    let forced = sandbox.run(&["sync", "--force"])?;
    let stderr = text(&forced.stderr)?;
    assert_eq!(forced.status.code(), Some(0), "{stderr}");
    assert!(
        fs::symlink_metadata(&placed)?.file_type().is_file(),
        "{stderr}"
    );
    assert_eq!(fs::read(&placed)?, V1);
    assert_eq!(fs::read(&precious)?, b"precious");
    let status = sandbox.run(&["status"])?;
    assert_eq!(
        text(&status.stderr)?,
        "         Ok everything matches data.lock by size and time\n"
    );
    Ok(())
}

/// a file where a dataset's folder belongs keeps every file of the dataset out. status names
/// that one file as blocking rather than each file as missing, a plain sync leaves it with a
/// warning, and `--force` replaces it with the folder and its files.
#[test]
fn force_replaces_a_file_where_a_dataset_folder_belongs() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    let added = sandbox.run(&["add", &server.url])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let folder = sandbox.work().join("data").join("iris");
    delete(&folder.join("iris.csv"))?;
    fs::remove_dir(&folder)?;
    fs::write(&folder, b"notes\n")?;

    let status = sandbox.run(&["--json", "status"])?;
    let report: serde_json::Value = serde_json::from_slice(&status.stdout)?;
    assert_eq!(
        report["result"]["blocked"],
        serde_json::json!(["data/iris"]),
        "{report}"
    );
    assert_eq!(
        report["result"]["missing"],
        serde_json::json!([]),
        "{report}"
    );
    assert_eq!(report["result"]["extra"], serde_json::json!([]), "{report}");
    let kept = sandbox.run(&["sync"])?;
    let stderr = text(&kept.stderr)?;
    assert_eq!(kept.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("data/iris is a file where data.lock needs a folder"),
        "{stderr}"
    );
    assert_eq!(fs::read(&folder)?, b"notes\n");
    let forced = sandbox.run(&["sync", "--force"])?;
    let stderr = text(&forced.stderr)?;
    assert_eq!(forced.status.code(), Some(0), "{stderr}");
    assert_eq!(fs::read(folder.join("iris.csv"))?, V1, "{stderr}");
    // the file moved out of the way goes once the files in its stead are in place
    let after = sandbox.run(&["status"])?;
    assert_eq!(
        text(&after.stderr)?,
        "         Ok everything matches data.lock by size and time\n"
    );
    Ok(())
}

/// a project that placed iris.csv, its data.lock, and that lock as it is and with iris.csv made a
/// folder holding the same file.
fn reshaped(sandbox: &Sandbox, server: &Server) -> Result<(std::path::PathBuf, String, String)> {
    sandbox.allow(&server.url);
    let added = sandbox.run(&["add", &server.url])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let lock = sandbox.work().join("data.lock");
    let as_file = fs::read_to_string(&lock)?;
    let as_folder = as_file.replace("path = \"iris.csv\"", "path = \"iris.csv/iris.csv\"");
    assert_ne!(as_folder, as_file);
    Ok((lock, as_file, as_folder))
}

/// a pinned file that becomes a folder of pinned files, and back, is placed by the sync that
/// removes what was placed in its way, rather than reported blocked until a second sync.
#[test]
fn a_file_that_becomes_a_folder_and_back_is_placed_in_one_sync() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    let (lock, as_file, as_folder) = reshaped(&sandbox, &server)?;
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    for (pins, file) in [(&as_folder, placed.join("iris.csv")), (&as_file, placed)] {
        fs::write(&lock, pins)?;
        let output = sandbox.run(&["--json", "sync"])?;
        let document: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(output.status.code(), Some(0), "{document}");
        let placement = &document["result"]["placement"];
        assert_eq!(placement["kept"], serde_json::json!([]), "{document}");
        assert_eq!(
            (&placement["placed"], &placement["removed"]),
            (&serde_json::json!(1), &serde_json::json!(1)),
            "{document}"
        );
        assert_eq!(fs::read(&file)?, V1, "{document}");
        let status = sandbox.run(&["status"])?;
        assert_eq!(
            text(&status.stderr)?,
            "         Ok everything matches data.lock by size and time\n"
        );
    }
    assert_eq!(server.requests().len(), 1);
    Ok(())
}

/// status agrees with sync: a file fetchloom placed that sync removes before it places anything,
/// or a folder of only such files, is unneeded rather than in the way, and the file behind it is
/// missing rather than blocked.
#[test]
fn status_lists_what_sync_removes_as_unneeded_rather_than_blocking() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    let (lock, as_file, as_folder) = reshaped(&sandbox, &server)?;
    for (pins, missing, unneeded) in [
        (
            &as_folder,
            "data/iris/iris.csv/iris.csv",
            "data/iris/iris.csv",
        ),
        (
            &as_file,
            "data/iris/iris.csv",
            "data/iris/iris.csv/iris.csv",
        ),
    ] {
        fs::write(&lock, pins)?;
        let status = sandbox.run(&["--json", "status"])?;
        let report: serde_json::Value = serde_json::from_slice(&status.stdout)?;
        assert_eq!(
            report["result"]["blocked"],
            serde_json::json!([]),
            "{report}"
        );
        assert_eq!(
            report["result"]["missing"],
            serde_json::json!([missing]),
            "{report}"
        );
        assert_eq!(
            report["result"]["unneeded"],
            serde_json::json!([unneeded]),
            "{report}"
        );
        assert_eq!(report["result"]["extra"], serde_json::json!([]), "{report}");
        // --force places it either way, so the next round starts from it in place
        let forced = sandbox.run(&["sync", "--force"])?;
        assert_eq!(forced.status.code(), Some(0), "{}", text(&forced.stderr)?);
    }
    Ok(())
}

/// an add whose placing fails is rolled back, and the files it placed for the dataset go with
/// data.toml and data.lock, so a failed add leaves nothing of the dataset in the data folder.
#[cfg(unix)]
#[test]
fn a_rolled_back_add_takes_the_files_it_placed_with_it() -> Result {
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt;
    if rustix::process::geteuid().is_root() {
        return Ok(());
    }
    let sandbox = Sandbox::new()?;
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, bytes) in [("a.csv", b"first"), ("sub/b.csv", b"other")] {
        zip.start_file(name, zip::write::SimpleFileOptions::default())?;
        zip.write_all(bytes)?;
    }
    let bytes = zip.finish()?.into_inner();
    let server = Server::start("/files.zip", vec![file(&bytes, "\"v1\"")])?;
    // a folder the dataset needs that refuses new files, so its second file cannot be placed
    let refusing = sandbox.work().join("data").join("rolled").join("sub");
    fs::create_dir_all(&refusing)?;
    fs::set_permissions(&refusing, fs::Permissions::from_mode(0o555))?;

    let added = sandbox.run(&["add", &server.url, "--as", "rolled"]);
    fs::set_permissions(&refusing, fs::Permissions::from_mode(0o755))?;
    let added = added?;

    let stderr = text(&added.stderr)?;
    assert_eq!(added.status.code(), Some(50), "{stderr}");
    assert!(
        stderr.contains("restored data.toml and data.lock"),
        "{stderr}"
    );
    assert!(!sandbox.work().join("data.toml").exists(), "{stderr}");
    assert!(
        !sandbox.work().join("data/rolled/a.csv").exists(),
        "{stderr}"
    );
    let state = sandbox.work().join(".fetchloom").join("state");
    if state.exists() {
        let state: serde_json::Value = serde_json::from_slice(&fs::read(&state)?)?;
        assert_eq!(state["files"], serde_json::json!({}), "{state}");
    }
    Ok(())
}

/// the work counters say how many files a run placed from bytes the cache already held, so a
/// sync that downloads nothing does not read as one that found nothing in the cache. `-v` says
/// it in words.
#[test]
fn the_counters_tell_files_placed_from_the_cache() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    let added = sandbox.run(&["--json", "add", &server.url])?;
    let report: serde_json::Value = serde_json::from_slice(&added.stdout)?;
    assert_eq!(report["work"]["placed_from_cache"], 0, "{report}");
    assert!(report["work"].get("cache_hits").is_none(), "{report}");
    for (name, args) in [
        ("clone", &["--json", "sync"][..]),
        ("other", &["-v", "sync"][..]),
    ] {
        let clone = sandbox.folder(name)?;
        for file in ["data.toml", "data.lock"] {
            fs::copy(sandbox.work().join(file), clone.join(file))?;
        }
        let synced = sandbox.command_in(&clone, args).output()?;
        assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
        if name == "clone" {
            let report: serde_json::Value = serde_json::from_slice(&synced.stdout)?;
            assert_eq!(report["work"]["placed_from_cache"], 1, "{report}");
            assert_eq!(report["work"]["requests"], 0, "{report}");
        } else {
            let stderr = text(&synced.stderr)?;
            assert!(stderr.contains("1 placed from the cache"), "{stderr}");
        }
    }
    Ok(())
}

/// status compares sizes and times, and hashes only files those cannot vouch for, so its last line
/// says what it compared rather than claiming the bytes match: an edit that kept a copy's size
/// and time passes unseen.
#[test]
fn status_says_it_compared_sizes_and_times() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    sandbox.allow(&server.url);
    fs::write(
        sandbox.work().join("data.toml"),
        format!("[datasets.iris]\nref = \"{}\"\ncopy = true\n", server.url),
    )?;
    let synced = sandbox.run(&["sync"])?;
    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    let state = sandbox.work().join(".fetchloom").join("state");
    sandbox.wait_for_a_later_time(fs::metadata(&state)?.modified()?)?;
    fs::write(&state, fs::read(&state)?)?;
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    let mut edited = V1.to_vec();
    edited[0] = b'S';
    let modified = fs::metadata(&placed)?.modified()?;
    edit(&placed, &edited)?;
    set_time(&placed, modified)?;

    let status = sandbox.run(&["status"])?;

    assert_eq!(
        text(&status.stderr)?,
        "         Ok everything matches data.lock by size and time\n"
    );
    Ok(())
}

/// a copy of iris placed in its own project, with its time no longer as new as the state, then
/// changed in place keeping its size and time, as a failing disk or a tool that restores times
/// leaves it.
fn quietly_changed_copy(sandbox: &Sandbox, server: &Server) -> Result<std::path::PathBuf> {
    sandbox.allow(&server.url);
    fs::write(
        sandbox.work().join("data.toml"),
        format!("[datasets.iris]\nref = \"{}\"\ncopy = true\n", server.url),
    )?;
    let synced = sandbox.run(&["sync"])?;
    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    let state = sandbox.work().join(".fetchloom").join("state");
    sandbox.wait_for_a_later_time(fs::metadata(&state)?.modified()?)?;
    fs::write(&state, fs::read(&state)?)?;
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    let mut edited = V1.to_vec();
    edited[0] = b'S';
    let modified = fs::metadata(&placed)?.modified()?;
    edit(&placed, &edited)?;
    set_time(&placed, modified)?;
    Ok(placed)
}

/// a change that keeps a copy's size and time shows in no listing. `status --verify` hashes every
/// placed file and lists it as damaged, a plain sync still finds nothing to do, and
/// `sync --verify` puts the pinned file back without --force.
#[test]
fn verify_finds_and_puts_back_a_change_that_kept_its_size_and_time() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    let placed = quietly_changed_copy(&sandbox, &server)?;

    let checked = sandbox.run(&["--json", "status", "--verify"])?;
    let report: serde_json::Value = serde_json::from_slice(&checked.stdout)?;
    assert_eq!(
        report["result"]["damaged"],
        serde_json::json!(["data/iris/iris.csv"]),
        "{report}"
    );
    let verified = sandbox.run(&["status", "--verify"])?;
    let told = text(&verified.stdout)?;
    assert!(
        told.contains(
            "Damaged data/iris/iris.csv · its bytes changed though its size and time did not"
        ),
        "{told}"
    );
    let hints = text(&verified.stderr)?;
    assert!(hints.contains("`fetchloom sync --verify`"), "{hints}");
    let plain = text(&sandbox.run(&["sync"])?.stderr)?;
    assert_eq!(
        plain,
        "         Ok all pinned files are in place by size and time, nothing to do\n"
    );
    let repaired = sandbox.run(&["sync", "--verify"])?;
    let stderr = text(&repaired.stderr)?;
    assert_eq!(repaired.status.code(), Some(0), "{stderr}");
    assert_eq!(fs::read(&placed)?, V1, "{stderr}");
    assert!(
        stderr.contains(
            "data/iris/iris.csv was put back as pinned: its bytes had changed though its size and time had not"
        ),
        "{stderr}"
    );
    let after = sandbox.run(&["status", "--verify"])?;
    assert_eq!(
        text(&after.stderr)?,
        "         Ok everything matches data.lock, checked byte for byte\n"
    );
    let again = sandbox.run(&["sync", "--verify"])?;
    assert_eq!(
        text(&again.stderr)?,
        "         Ok all pinned files are in place, checked byte for byte; nothing to do\n"
    );
    assert_eq!(server.requests().len(), 1, "the cache still held iris");
    Ok(())
}

/// `sync --verify` keeps the rule for an edit that shows: a copy whose size or time changed is the
/// project's own, and it stays with the usual warning until --force.
#[test]
fn verify_keeps_an_edit_that_shows() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    let placed = quietly_changed_copy(&sandbox, &server)?;
    let mut grown = V1.to_vec();
    grown.extend_from_slice(b"6.3,edited\n");
    edit(&placed, &grown)?;

    let kept = sandbox.run(&["sync", "--verify"])?;

    let stderr = text(&kept.stderr)?;
    assert_eq!(kept.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("data/iris/iris.csv has changed, so it stays"),
        "{stderr}"
    );
    assert_eq!(fs::read(&placed)?, grown);
    Ok(())
}

/// a change through a shared link that keeps the size and time, with the file made read-only
/// again, changes the cache's own copy unseen, so `sync --verify` marks it, downloads the file
/// again and puts the link back.
#[test]
fn verify_puts_back_a_link_whose_cached_copy_quietly_changed() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(
        "/files/iris.csv",
        vec![file(V1, "\"v1\""), file(V1, "\"v1\"")],
    )?;
    let Some(_) = shared_and_edited(&sandbox, &server)? else {
        return Ok(());
    };
    keep_time(&sandbox.work())?;
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    let mut sealed = fs::metadata(&placed)?.permissions();
    sealed.set_readonly(true);
    fs::set_permissions(&placed, sealed)?;
    let checked = sandbox.run(&["--json", "status", "--verify"])?;
    let report: serde_json::Value = serde_json::from_slice(&checked.stdout)?;
    assert_eq!(
        report["result"]["damaged"],
        serde_json::json!(["data/iris/iris.csv"]),
        "{report}"
    );

    let repaired = sandbox.run(&["sync", "--verify"])?;

    let stderr = text(&repaired.stderr)?;
    assert_eq!(repaired.status.code(), Some(0), "{stderr}");
    assert_eq!(fs::read(&placed)?, V1, "{stderr}");
    assert_eq!(
        server.requests().len(),
        2,
        "the changed copy is fetched again"
    );
    Ok(())
}

/// a sync never deletes outside the data folder, so a data folder `data_dir` moved away keeps the
/// files fetchloom placed in it, and the sync that moves it, and its dry run, say where they are.
/// files of the person's own there are counted and named, so deleting the folder loses none
/// unseen.
#[test]
fn a_changed_data_dir_leaves_the_old_folder_and_says_so() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    sandbox.allow(&server.url);
    let added = sandbox.run(&["add", &server.url])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let work = sandbox.work();
    for notes in [
        "data/iris/my-notes.txt",
        "data/readme.txt",
        "data/scratch/plot.py",
        "data/scratch/todo.md",
    ] {
        fs::create_dir_all(work.join(notes).parent().ok_or("no folder")?)?;
        fs::write(work.join(notes), b"mine\n")?;
    }
    let manifest = work.join("data.toml");
    fs::write(
        &manifest,
        format!(
            "[project]\ndata_dir = \"inputs/raw\"\n\n{}",
            fs::read_to_string(&manifest)?
        ),
    )?;

    let said = "data still holds data/iris/iris.csv, a file fetchloom placed before data_dir changed to inputs/raw, and 4 files it did not place: data/iris/my-notes.txt, data/readme.txt, data/scratch/plot.py and 1 more. fetchloom leaves them all there; move the 4 files fetchloom did not place out of data before you delete data";
    for args in [&["--json", "sync", "--dry-run"][..], &["--json", "sync"]] {
        let output = sandbox.run(args)?;
        assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
        let warnings = json(&output)?["warnings"].to_string();
        assert!(warnings.contains(said), "{warnings}");
    }
    assert_eq!(
        fs::read(work.join("data").join("iris").join("iris.csv"))?,
        V1
    );
    assert_eq!(fs::read(work.join("data/iris/my-notes.txt"))?, b"mine\n");
    assert_eq!(fs::read(work.join("inputs/raw/iris/iris.csv"))?, V1);
    Ok(())
}

/// sets `place = false` on the dataset `name` in the work folder's data.toml.
fn unplace(sandbox: &Sandbox, name: &str) -> Result {
    let manifest = sandbox.work().join("data.toml");
    let table = format!("[datasets.{name}]\n");
    let text = fs::read_to_string(&manifest)?;
    assert!(text.contains(&table), "{text}");
    fs::write(
        &manifest,
        text.replace(&table, &format!("{table}place = false\n")),
    )?;
    Ok(())
}

/// a dataset set to `place = false` retires what sync placed of it: unchanged files go, an
/// edited one stays with the warning a removed dataset's edit gets, and the sync says so once.
#[test]
fn place_false_retires_unchanged_files_and_keeps_edits() -> Result {
    let sandbox = Sandbox::new()?;
    let flowers = sandbox.folder("flowers")?;
    fs::write(flowers.join("petals.csv"), b"length\n1.4\n")?;
    fs::write(flowers.join("sepals.csv"), b"length\n5.1\n")?;
    let added = sandbox.run(&["add", "../flowers"])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let folder = sandbox.work().join("data").join("flowers");
    let edited = folder.join("sepals.csv");
    delete(&edited)?;
    fs::write(&edited, b"length\n5.1\n4.9\n")?;
    unplace(&sandbox, "flowers")?;

    let preview = sandbox.run(&["sync", "--dry-run"])?;
    assert_eq!(preview.status.code(), Some(0), "{}", text(&preview.stderr)?);
    let stderr = text(&preview.stderr)?;
    assert!(
        stderr.contains("Would retire data/flowers · 1 file, since flowers has place = false\n"),
        "{stderr}"
    );
    let output = sandbox.run(&["sync"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    let stderr = text(&output.stderr)?;
    assert!(
        stderr.contains("    Retired data/flowers · 1 file, since flowers has place = false\n"),
        "{stderr}"
    );
    assert!(
        stderr.contains(
            "warning: data/flowers/sepals.csv is no longer needed, but it has changed, so it stays\n"
        ),
        "{stderr}"
    );
    assert!(!stderr.contains("no dataset needs"), "{stderr}");
    assert!(!folder.join("petals.csv").exists());
    assert_eq!(fs::read(&edited)?, b"length\n5.1\n4.9\n");

    let again = sandbox.run(&["sync"])?;
    assert_eq!(again.status.code(), Some(0), "{}", text(&again.stderr)?);
    let stderr = text(&again.stderr)?;
    assert!(!stderr.contains("Retired"), "{stderr}");
    assert!(!stderr.contains("warning"), "{stderr}");
    Ok(())
}

/// a dataset with `place = false` has no folder, so `path` refuses it, and its files, with
/// the line of data.toml that would place it.
#[test]
fn path_of_an_unplaced_dataset_is_refused() -> Result {
    let sandbox = Sandbox::new()?;
    let flowers = sandbox.folder("flowers")?;
    fs::write(flowers.join("petals.csv"), b"length\n1.4\n")?;
    let added = sandbox.run(&["add", "../flowers"])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    unplace(&sandbox, "flowers")?;
    for target in ["flowers", "flowers/petals.csv"] {
        let output = sandbox.run(&["--json", "path", target])?;
        assert_eq!(output.status.code(), Some(2), "{}", text(&output.stdout)?);
        let error = &json(&output)?["errors"][0];
        assert_eq!(error["kind"], "usage");
        assert_eq!(
            error["message"],
            "flowers has place = false, so it has no folder in data/"
        );
        assert_eq!(
            error["help"],
            "set place = true on datasets.flowers in data.toml, then run `fetchloom sync flowers`"
        );
    }
    Ok(())
}

/// a pinned dataset with `place = false` needs no file in the data folder, so a sync in a
/// fresh clone, whose cache lacks its files, downloads nothing.
#[test]
fn sync_fetches_nothing_for_a_pinned_unplaced_dataset() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    sandbox.allow(&server.url);
    let added = sandbox.run(&["add", &server.url])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    unplace(&sandbox, "iris")?;
    let clone = crate::project::clone(&sandbox, "clone")?;
    let elsewhere = sandbox.folder("another cache")?;
    for args in [&["sync"][..], &["sync", "--locked"], &["sync", "iris"]] {
        let output = sandbox
            .command_in(&clone, args)
            .env("FETCHLOOM_CACHE", &elsewhere)
            .output()?;
        assert_eq!(
            output.status.code(),
            Some(0),
            "{args:?}: {}",
            text(&output.stderr)?
        );
    }
    assert_eq!(server.requests().len(), 1, "only the add downloaded");
    assert!(!clone.join("data").join("iris").exists());
    Ok(())
}

/// status tells a dataset with `place = false` apart from a difference. until a sync retires what
/// sync placed of it before, its line says the sync removes those files, apart from notes of the
/// person's own, which sync keeps, and the hint says what the sync does.
#[test]
fn status_says_an_unplaced_dataset_is_not_placed() -> Result {
    let sandbox = Sandbox::new()?;
    let flowers = sandbox.folder("flowers")?;
    fs::write(flowers.join("petals.csv"), b"length\n1.4\n")?;
    let added = sandbox.run(&["add", "../flowers"])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let notes = sandbox
        .work()
        .join("data")
        .join("flowers")
        .join("notes.txt");
    fs::write(&notes, b"mine\n")?;
    unplace(&sandbox, "flowers")?;
    let output = sandbox.run(&["status"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    let told = text(&output.stdout)?;
    for line in [
        " Not placed flowers · place = false in data.toml, so `fetchloom sync` removes its file from data/flowers\n",
        "      Extra data/flowers/notes.txt\n",
    ] {
        assert!(told.contains(line), "{line:?} in\n{told}");
    }
    let story = text(&output.stderr)?;
    let hint =
        "  hint: `fetchloom sync` removes the files fetchloom placed that no dataset needs now\n";
    assert!(story.contains(hint), "{hint:?} in\n{story}");
    assert!(!format!("{told}{story}").contains("petals.csv"), "{told}");
    let document = json(&sandbox.run(&["--json", "status"])?)?;
    assert_eq!(
        document["result"]["unplaced"],
        serde_json::json!(["flowers"])
    );
    let synced = sandbox.run(&["sync"])?;
    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    assert_eq!(fs::read(&notes)?, b"mine\n");
    fs::remove_file(&notes)?;
    let told = text(&sandbox.run(&["status"])?.stderr)?;
    assert!(
        told.contains(" Not placed flowers · place = false in data.toml\n"),
        "{told}"
    );
    assert!(
        told.contains("Ok everything matches data.lock by size and time\n"),
        "{told}"
    );
    Ok(())
}
