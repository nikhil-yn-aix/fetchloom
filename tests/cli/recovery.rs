//! a data.lock fetchloom cannot use: the advice each error gives, and that following it works.

use std::fs;
use std::process::Output;

use serde_json::Value;

use crate::support::{Result, Sandbox, Server, file, text};

const NEWER: &str = "update fetchloom to read this newer format";
const BROKEN: &str = "restore data.lock from git, or delete it and run `fetchloom lock`, which resolves every dataset again and may pin newer data";

/// the help of the error a `--json` run ended with.
fn help(output: &Output) -> Result<String> {
    let document: Value = serde_json::from_slice(&output.stdout)?;
    let help = document["errors"][0]["help"].as_str().ok_or("no help")?;
    Ok(help.to_owned())
}

#[test]
fn a_lock_from_a_newer_fetchloom_or_a_broken_one_gets_advice_that_works() -> Result {
    let sandbox = Sandbox::new()?;
    fs::write(sandbox.work().join("data.toml"), "[datasets]\n")?;
    let path = sandbox.work().join("data.lock");
    for (contents, advice) in [
        (&b"version = 2\n"[..], NEWER),
        (b"version = [", BROKEN),
        (b"version = 1\n\xff\xfe\n", BROKEN),
    ] {
        fs::write(&path, contents)?;
        for command in ["sync", "lock"] {
            let output = sandbox.run(&["--json", command])?;
            assert_eq!(output.status.code(), Some(50), "{command}");
            assert_eq!(help(&output)?, advice, "{command}");
        }
    }
    fs::remove_file(path)?;
    let pinned = sandbox.run(&["lock"])?;
    assert_eq!(pinned.status.code(), Some(0), "{}", text(&pinned.stderr)?);
    Ok(())
}

#[test]
fn gc_says_where_another_projects_lock_is_fixed() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/iris.csv", vec![file(b"iris", "\"v1\"")])?;
    assert_eq!(sandbox.run(&["add", &server.url])?.status.code(), Some(0));
    let elsewhere = sandbox.folder("elsewhere")?;
    for (contents, advice) in [
        ("version = 2\n", NEWER.to_owned()),
        ("version = [", format!("in that project, {BROKEN}")),
    ] {
        fs::write(sandbox.work().join("data.lock"), contents)?;
        let output = sandbox
            .command_in(&elsewhere, &["--json", "cache", "gc"])
            .output()?;
        assert_eq!(output.status.code(), Some(50));
        let document: Value = serde_json::from_slice(&output.stdout)?;
        let message = document["errors"][0]["message"]
            .as_str()
            .ok_or("no message")?;
        let project = sandbox.work().display().to_string();
        assert!(message.contains(&project), "{message}");
        assert_eq!(help(&output)?, advice);
    }
    Ok(())
}

#[test]
fn a_broken_link_in_the_lock_is_pinned_again_by_an_upgrade() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(
        "/iris.csv",
        vec![file(b"iris", "\"v1\""), file(b"iris", "\"v1\"")],
    )?;
    assert_eq!(sandbox.run(&["add", &server.url])?.status.code(), Some(0));
    let clone = sandbox.folder("clone")?;
    fs::copy(sandbox.work().join("data.toml"), clone.join("data.toml"))?;
    let lock = fs::read_to_string(sandbox.work().join("data.lock"))?;
    let broken = lock.replace(&format!("url = \"{}\"", server.url), "url = \"not a link\"");
    fs::write(clone.join("data.lock"), broken)?;
    let cache = sandbox.folder("cold-cache")?;
    let run = |args: &[&str]| {
        sandbox
            .command_in(&clone, args)
            .env("FETCHLOOM_CACHE", &cache)
            .output()
    };
    let failed = run(&["--json", "sync"])?;
    assert_eq!(failed.status.code(), Some(50));
    assert_eq!(
        help(&failed)?,
        "restore data.lock from git, or run `fetchloom lock --upgrade iris`, which resolves it again and may pin newer data"
    );
    let upgraded = run(&["lock", "--upgrade", "iris"])?;
    assert_eq!(
        upgraded.status.code(),
        Some(0),
        "{}",
        text(&upgraded.stderr)?
    );
    let synced = run(&["sync"])?;
    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    assert_eq!(
        fs::read(clone.join("data").join("iris").join("iris.csv"))?,
        b"iris"
    );
    Ok(())
}
