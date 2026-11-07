//! the experiment switches, each on: the same answers, files and bytes as with it off.

use std::fs;
use std::process::Output;

use crate::support::{Result, Sandbox, Server, delete, edit, file, text};

const IRIS: &[u8] = b"sepal_length,species\n5.1,setosa\n";
const COWS: &[u8] = b"name\nbess\n";

/// the command with `FETCHLOOM_EXP_<switch>` on, tracing to `trace` when given.
fn with(sandbox: &Sandbox, switch: &str, args: &[&str], trace: Option<&str>) -> Result<Output> {
    let mut command = sandbox.command(args);
    command.env(format!("FETCHLOOM_EXP_{switch}"), "1");
    if let Some(name) = trace {
        command.env("FETCHLOOM_TRACE", sandbox.folder("traces")?.join(name));
    }
    Ok(command.output()?)
}

fn trace(sandbox: &Sandbox, name: &str) -> Result<String> {
    Ok(fs::read_to_string(sandbox.folder("traces")?.join(name))?)
}

fn has(events: &str, span: &str) -> bool {
    events.contains(&format!("\"name\":\"{span}\""))
}

/// a project with iris as a writable copy, synced and then settled by a later sync.
fn settled_copy(sandbox: &Sandbox, server: &Server) -> Result {
    fs::write(
        sandbox.work().join("data.toml"),
        format!("[datasets.iris]\nref = \"{}\"\ncopy = true\n", server.url),
    )?;
    let first = sandbox.run(&["sync"])?;
    assert_eq!(first.status.code(), Some(0), "{}", text(&first.stderr)?);
    let state = sandbox.work().join(".fetchloom").join("state");
    sandbox.wait_for_a_later_time(fs::metadata(&state)?.modified()?)?;
    let settling = sandbox.run(&["sync"])?;
    assert_eq!(
        settling.status.code(),
        Some(0),
        "{}",
        text(&settling.stderr)?
    );
    Ok(())
}

#[test]
fn settled_first_answers_a_settled_project_without_reading_data_lock() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/files/iris.csv", vec![file(IRIS, "\"v1\"")])?;
    settled_copy(&sandbox, &server)?;
    let written = |name: &str| fs::metadata(sandbox.work().join(name))?.modified();
    let before = (written("data.lock")?, written(".fetchloom/state")?);
    sandbox.wait_for_a_later_time(before.0.max(before.1))?;

    let output = with(&sandbox, "SETTLED_FIRST", &["sync"], Some("settled.json"))?;

    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("all pinned files are in place, nothing to do"),
        "{stderr}"
    );
    let events = trace(&sandbox, "settled.json")?;
    assert!(has(&events, "scan"), "{events}");
    assert!(!has(&events, "read_lock"), "{events}");
    assert!(!has(&events, "plan"), "{events}");
    assert_eq!(
        (written("data.lock")?, written(".fetchloom/state")?),
        before
    );
    let json = with(&sandbox, "SETTLED_FIRST", &["--json", "sync"], None)?;
    let document: serde_json::Value = serde_json::from_slice(&json.stdout)?;
    assert_eq!(
        document["result"]["placement"]["unchanged"], 1,
        "{document}"
    );
    assert_eq!(document["result"]["lock_changed"], false, "{document}");
    assert_eq!(server.requests().len(), 1);
    Ok(())
}

#[test]
fn settled_first_still_finds_every_change() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/files/iris.csv", vec![file(IRIS, "\"v1\"")])?;
    settled_copy(&sandbox, &server)?;
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    let sync = |name: &str, args: &[&str]| -> Result<(String, String)> {
        let output = with(&sandbox, "SETTLED_FIRST", args, Some(name))?;
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(0), "{stderr}");
        Ok((stderr, trace(&sandbox, name)?))
    };

    delete(&placed)?;
    let (said, events) = sync("removed.json", &["sync"])?;
    assert!(said.contains("Synced"), "{said}");
    assert!(
        has(&events, "read_lock") && has(&events, "place"),
        "{events}"
    );
    assert_eq!(fs::read(&placed)?, IRIS);

    edit(&placed, b"sepal_length,species\n5.1,setosa\nedited\n")?;
    let (said, _) = sync("edited.json", &["sync"])?;
    assert!(
        said.contains("data/iris/iris.csv has changed, so it stays"),
        "{said}"
    );
    let (_, _) = sync("forced.json", &["sync", "--force"])?;
    assert_eq!(fs::read(&placed)?, IRIS);

    delete(&placed)?;
    fs::write(&placed, b"another file entirely\n")?;
    let (said, _) = sync("replaced.json", &["sync"])?;
    assert!(
        said.contains("data/iris/iris.csv has changed, so it stays"),
        "{said}"
    );
    let (_, _) = sync("restored.json", &["sync", "--force"])?;
    assert_eq!(fs::read(&placed)?, IRIS);

    let lock = sandbox.work().join("data.lock");
    let mut text_of_lock = fs::read_to_string(&lock)?;
    text_of_lock.push('\n');
    fs::write(&lock, text_of_lock)?;
    let (_, events) = sync("relocked.json", &["sync"])?;
    assert!(
        has(&events, "read_lock") && has(&events, "relock"),
        "{events}"
    );
    assert_eq!(fs::read(&placed)?, IRIS);
    assert_eq!(server.requests().len(), 1, "the cache still holds iris");
    Ok(())
}

/// the same command with the switch off and on: the same exit code, output and errors.
fn same_with(sandbox: &Sandbox, switch: &str, args: &[&str]) -> Result {
    let off = sandbox.run(args)?;
    let on = with(sandbox, switch, args, None)?;
    assert_eq!(on.status.code(), off.status.code(), "{args:?}");
    assert_eq!(text(&on.stdout)?, text(&off.stdout)?, "{args:?}");
    assert_eq!(text(&on.stderr)?, text(&off.stderr)?, "{args:?}");
    Ok(())
}

#[test]
fn path_scoped_answers_as_path_does() -> Result {
    let sandbox = Sandbox::new()?;
    let iris = Server::start("/files/iris.csv", vec![file(IRIS, "\"v1\"")])?;
    let cows = Server::start("/files/cows.csv", vec![file(COWS, "\"c1\"")])?;
    for server in [&iris, &cows] {
        let added = sandbox.run(&["add", &server.url])?;
        assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    }
    for args in [
        &["path", "iris"][..],
        &["path", "iris/iris.csv"],
        &["path", "cows/cows.csv"],
        &["path", "iris/iris.csv/"],
        &["path", "iris/iirs.csv"],
        &["path", "irs"],
    ] {
        same_with(&sandbox, "PATH_SCOPED", args)?;
    }

    // a file another folder lost is not the asked dataset's business
    let other = sandbox.work().join("data").join("cows").join("cows.csv");
    delete(&other)?;
    let output = with(
        &sandbox,
        "PATH_SCOPED",
        &["path", "iris"],
        Some("scoped.json"),
    )?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    assert!(!has(&trace(&sandbox, "scoped.json")?, "relock"));
    assert!(!other.exists());

    // the asked dataset's own lost file is placed again, as without the switch
    let mine = sandbox.work().join("data").join("iris").join("iris.csv");
    delete(&mine)?;
    let output = with(&sandbox, "PATH_SCOPED", &["path", "iris/iris.csv"], None)?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    assert_eq!(text(&output.stdout)?, format!("{}\n", mine.display()));
    assert_eq!(fs::read(&mine)?, IRIS);
    Ok(())
}

#[test]
fn path_scoped_syncs_a_fresh_clone_first() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/files/iris.csv", vec![file(IRIS, "\"v1\"")])?;
    let added = sandbox.run(&["add", &server.url])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let clone = sandbox.folder("clone")?;
    for name in ["data.toml", "data.lock"] {
        fs::copy(sandbox.work().join(name), clone.join(name))?;
    }
    let output = sandbox
        .command_in(&clone, &["path", "iris"])
        .env("FETCHLOOM_EXP_PATH_SCOPED", "1")
        .output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    assert_eq!(
        text(&output.stdout)?,
        format!("{}\n", clone.join("data").join("iris").display())
    );
    assert_eq!(
        fs::read(clone.join("data").join("iris").join("iris.csv"))?,
        IRIS
    );
    Ok(())
}
