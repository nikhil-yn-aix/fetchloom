//! `drop`: a pinned dataset's bytes freed on this machine once each of its files is found
//! elsewhere, left out of a plain sync, and brought back by a sync that names it.

use std::fs;

use serde_json::json;

use crate::build::{build, with};
use crate::mirrors::{IRIS, mirror_of};
use crate::recipes::{WRITE, object};
use crate::support::{Result, Routes, Sandbox, Server, edit, file, json, ok, reply, text};

/// a project in the sandbox's work folder whose dataset iris is iris.csv from the source it
/// returns, synced, so the data folder and the cache hold it.
fn iris(sandbox: &Sandbox) -> Result<Routes> {
    let source = Routes::start(vec![("/files/iris.csv".to_owned(), file(IRIS, "\"i\""))])?;
    sandbox.allow(&source.url);
    ok(&sandbox.run(&["add", &format!("{}/files/iris.csv", source.url)])?)?;
    Ok(source)
}

#[test]
fn a_dropped_dataset_is_left_out_until_a_sync_names_it() -> Result {
    let sandbox = Sandbox::new()?;
    let source = iris(&sandbox)?;
    let placed = sandbox.work().join("data/iris/iris.csv");
    let cached = object(&sandbox.cache(), IRIS);
    let output = sandbox.run(&["--json", "drop", "iris", "--dry-run"])?;
    ok(&output)?;
    let result = &json(&output)?["result"];
    assert_eq!(
        (&result["files"], &result["objects"]),
        (&json!(1), &json!(1))
    );
    assert!(
        placed.exists() && cached.exists(),
        "a dry run frees nothing"
    );
    let output = sandbox.run(&["--json", "drop", "iris"])?;
    ok(&output)?;
    let result = &json(&output)?["result"];
    assert_eq!(result["dropped"], json!(["iris"]));
    assert_eq!(
        (&result["files"], &result["objects"]),
        (&json!(1), &json!(1))
    );
    assert!(
        result["bytes"].as_u64() >= Some(IRIS.len() as u64),
        "{result}"
    );
    assert!(!placed.exists() && !cached.exists());
    // data.toml and data.lock keep it, and it is no difference
    let output = sandbox.run(&["--json", "status", "--check"])?;
    ok(&output)?;
    let status = &json(&output)?["result"];
    assert_eq!(status["dropped"], json!(["iris"]));
    assert_eq!(status["missing"], json!([]));
    let listed = json(&sandbox.run(&["--json", "list"])?)?;
    assert_eq!(listed["result"]["datasets"][0]["dropped"], true);
    let asked = source.requests().len();
    let output = sandbox.run(&["sync"])?;
    ok(&output)?;
    let told = text(&output.stderr)?;
    assert!(
        told.contains(
            "Dropped iris · left out on this machine; `fetchloom sync iris` brings it back"
        ),
        "{told}"
    );
    assert!(!placed.exists());
    assert_eq!(
        source.requests().len(),
        asked,
        "a plain sync asks nothing for it"
    );
    ok(&sandbox.run(&["sync", "iris"])?)?;
    assert_eq!(fs::read(&placed)?, IRIS);
    assert_eq!(source.requests().len(), asked + 1);
    let status = json(&sandbox.run(&["--json", "status"])?)?;
    assert_eq!(status["result"]["dropped"], json!([]));
    Ok(())
}

#[test]
fn a_dataset_no_mirror_or_source_has_stays_and_a_mirror_brings_one_back() -> Result {
    let sandbox = Sandbox::new()?;
    let source = iris(&sandbox)?;
    let root = sandbox.folder("mirror")?;
    mirror_of(&sandbox, &root)?;
    source.change(vec![(
        "/files/iris.csv".to_owned(),
        reply("404 Not Found", &[("Content-Length", "0")], b""),
    )]);
    let placed = sandbox.work().join("data/iris/iris.csv");
    let output = sandbox.run(&["--json", "drop", "iris"])?;
    assert_eq!(output.status.code(), Some(10), "{}", text(&output.stderr)?);
    let error = &json(&output)?["errors"][0];
    assert_eq!(error["kind"], "reference.unresolved");
    assert!(
        error["details"][0]
            .as_str()
            .is_some_and(|line| line.starts_with("iris/iris.csv · gone: ")),
        "{error}"
    );
    assert!(placed.exists() && object(&sandbox.cache(), IRIS).exists());
    // offline, only a folder mirror whose copy hashes to the pin counts
    let output = sandbox.run(&["--json", "--offline", "drop", "iris"])?;
    assert_eq!(output.status.code(), Some(20), "{}", text(&output.stderr)?);
    assert_eq!(json(&output)?["errors"][0]["kind"], "offline.missing");
    let mirrored = |args: &[&str]| sandbox.command(args).arg("--mirror").arg(&root).output();
    ok(&mirrored(&["--offline", "drop", "iris"])?)?;
    assert!(!placed.exists());
    let asked = source.requests().len();
    ok(&mirrored(&["sync", "iris"])?)?;
    assert_eq!(fs::read(&placed)?, IRIS);
    assert_eq!(source.requests().len(), asked, "the mirror held it");
    Ok(())
}

#[test]
fn a_dataset_not_downloaded_yet_has_nothing_to_drop() -> Result {
    let sandbox = Sandbox::new()?;
    let _source = iris(&sandbox)?;
    let lock = sandbox.work().join("data.lock");
    let pinned = fs::read_to_string(&lock)?;
    fs::write(
        &lock,
        pinned.replacen("files = [", "downloaded = false\nfiles = [", 1),
    )?;
    let output = sandbox.run(&["--json", "drop", "iris"])?;
    assert_eq!(output.status.code(), Some(2), "{}", text(&output.stderr)?);
    assert_eq!(
        json(&output)?["errors"][0]["message"],
        "iris is pinned by its source's SHA-256 but not downloaded yet, so nothing of it is on this machine to drop"
    );
    assert!(sandbox.work().join("data/iris/iris.csv").exists());
    assert!(object(&sandbox.cache(), IRIS).exists());
    Ok(())
}

#[test]
fn a_changed_file_stays_and_is_named() -> Result {
    let sandbox = Sandbox::new()?;
    let _source = iris(&sandbox)?;
    let placed = sandbox.work().join("data/iris/iris.csv");
    edit(&placed, b"my notes\n")?;
    let output = sandbox.run(&["--json", "drop", "iris"])?;
    ok(&output)?;
    let result = &json(&output)?["result"];
    assert_eq!(result["dropped"], json!(["iris"]));
    assert_eq!(
        result["placement"]["kept"][0]["path"], "data/iris/iris.csv",
        "{result}"
    );
    assert_eq!(fs::read(&placed)?, b"my notes\n");
    Ok(())
}

#[test]
fn a_result_drops_only_where_a_mirror_holds_it() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(
        crate::project::PATH,
        vec![file(crate::project::IRIS, "\"v1\"")],
    )?;
    with(&sandbox, &server, WRITE)?;
    let built = build(&sandbox, &["build"])?;
    assert_eq!(built.status.code(), Some(0), "{}", text(&built.stderr)?);
    let result = sandbox.work().join("data/clean/clean.csv");
    let output = sandbox.run(&["--json", "drop", "clean"])?;
    assert_eq!(output.status.code(), Some(10), "{}", text(&output.stderr)?);
    let error = &json(&output)?["errors"][0];
    assert_eq!(
        error["details"],
        json!(["clean/clean.csv · no mirror holds this file of a recipe's result"])
    );
    assert_eq!(
        error["help"],
        "`fetchloom push clean` keeps a copy of each on a mirror first, then drop it"
    );
    assert!(result.exists());
    let root = sandbox.folder("mirror")?;
    ok(&sandbox
        .command(&["push", "clean", "--yes", "--to"])
        .arg(&root)
        .output()?)?;
    ok(&sandbox
        .command(&["drop", "clean", "--mirror"])
        .arg(&root)
        .output()?)?;
    assert!(!result.exists());
    Ok(())
}

#[test]
fn gc_frees_what_only_a_dropped_dataset_pins() -> Result {
    let sandbox = Sandbox::new()?;
    let _source = iris(&sandbox)?;
    ok(&sandbox.run(&["drop", "iris"])?)?;
    // its bytes back in the cache, as a build that reads it brings them, are needed no more
    let cached = object(&sandbox.cache(), IRIS);
    fs::create_dir_all(cached.parent().ok_or("an object in no folder")?)?;
    fs::write(&cached, IRIS)?;
    let output = sandbox.run(&["--json", "cache", "gc", "--now"])?;
    ok(&output)?;
    assert_eq!(json(&output)?["result"]["removed_objects"], 1);
    assert!(!cached.exists());
    Ok(())
}
