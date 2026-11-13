//! a data.lock fetchloom cannot use: the advice each error gives, and that following it works.

use std::fmt::Write as _;
use std::fs;
use std::process::Output;

use serde_json::Value;

use crate::support::{Result, Sandbox, Server, file, sha256_hex, text};

/// the advice for a lock from a newer fetchloom, after `fetchloom --json <command>`.
fn newer(command: &str) -> String {
    format!("install a newer fetchloom, then run `fetchloom --json {command}` again")
}
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
        (&b"version = 2\n"[..], None),
        (b"version = [", Some(BROKEN)),
        (b"version = 1\n\xff\xfe\n", Some(BROKEN)),
    ] {
        fs::write(&path, contents)?;
        for command in ["sync", "lock"] {
            let output = sandbox.run(&["--json", command])?;
            assert_eq!(output.status.code(), Some(50), "{command}");
            let advice = advice.map_or_else(|| newer(command), str::to_owned);
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
        ("version = 2\n", newer("cache gc")),
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
    sandbox.allow(&server.url);
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

/// the cut leaves the new, locked bytes recorded as the old file; a file of the locked size is
/// hashed, so status and sync take it as placed, not as an edit only `--force` would clear.
#[test]
fn a_replacement_cut_off_before_the_state_was_written_is_taken_as_placed() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(
        "/iris.csv",
        vec![file(b"iris\n", "\"v1\""), file(b"iris, again\n", "\"v2\"")],
    )?;
    sandbox.allow(&server.url);
    let added = sandbox.run(&["add", &server.url])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let state = sandbox.work().join(".fetchloom").join("state");
    let before = fs::read(&state)?;
    let upgraded = sandbox.run(&["lock", "--upgrade"])?;
    assert_eq!(
        upgraded.status.code(),
        Some(0),
        "{}",
        text(&upgraded.stderr)?
    );
    let replaced = sandbox.run(&["sync"])?;
    assert_eq!(
        replaced.status.code(),
        Some(0),
        "{}",
        text(&replaced.stderr)?
    );
    // what a kill after placing and before the state write leaves behind
    fs::write(&state, before)?;

    let status = sandbox.run(&["status"])?;
    assert_eq!(
        text(&status.stderr)?,
        "         Ok everything matches data.lock by size and time\n"
    );
    let synced = sandbox.run(&["--json", "sync"])?;
    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    let report: Value = serde_json::from_slice(&synced.stdout)?;
    assert_eq!(report["result"]["placement"]["kept"], serde_json::json!([]));
    assert_eq!(
        fs::read(sandbox.work().join("data").join("iris").join("iris.csv"))?,
        b"iris, again\n"
    );
    let after = sandbox.run(&["status"])?;
    assert_eq!(
        text(&after.stderr)?,
        "         Ok everything matches data.lock by size and time\n"
    );
    Ok(())
}

#[test]
fn batch_errors_list_every_failure_in_json_but_only_ten_for_people() -> Result {
    let sandbox = Sandbox::new()?;
    let original = Server::start("/iris.csv", vec![file(b"iris", "\"v1\"")])?;
    sandbox.allow(&original.url);
    assert!(sandbox.run(&["add", &original.url])?.status.success());
    let locked = fs::read_to_string(sandbox.work().join("data.lock"))?;
    let (_, dataset) = locked.split_once("[[dataset]]").ok_or("missing dataset")?;
    let parsed: toml::Value = toml::from_str(&locked)?;
    let digest = parsed["dataset"][0]["files"][0]["sha256"]
        .as_str()
        .ok_or("missing file hash")?;
    let clone = sandbox.folder("batch")?;
    let cache = sandbox.folder("batch-cache")?;
    let mut manifest = String::new();
    let mut lock = "version = 1\n".to_owned();
    let mut servers = Vec::new();
    for index in 0..12 {
        let response = || {
            if index % 2 == 0 {
                crate::support::reply("403 Forbidden", &[], b"denied")
            } else {
                file(b"wrong", "\"bad\"")
            }
        };
        let server = Server::start("/iris.csv", vec![response(), response()])?;
        sandbox.allow(&server.url);
        let name = format!("d{index:02}");
        write!(manifest, "[datasets.{name}]\nref = \"{}\"\n", original.url)?;
        let entry = dataset
            .replace("name = \"iris\"", &format!("name = \"{name}\""))
            .replace(
                &format!("sha256 = \"{digest}\""),
                &format!("sha256 = \"{index:064x}\""),
            )
            .replace(
                &format!("url = \"{}\"", original.url),
                &format!("url = \"{}\"", server.url),
            );
        write!(lock, "\n[[dataset]]{entry}")?;
        servers.push(server);
    }
    fs::write(clone.join("data.toml"), manifest)?;
    fs::write(clone.join("data.lock"), lock)?;
    let run = |args: &[&str]| {
        sandbox
            .command_in(&clone, args)
            .env("FETCHLOOM_CACHE", &cache)
            .output()
    };
    let output = run(&["--json", "sync"])?;
    assert_eq!(output.status.code(), Some(40));
    let report = crate::support::json(&output)?;
    assert_eq!(report["errors"][0]["kind"], "auth.required");
    let failures = report["errors"][0]["failures"]
        .as_array()
        .ok_or("missing failed files")?;
    assert_eq!(failures.len(), 12);
    for (index, failure) in failures.iter().enumerate() {
        assert_eq!(failure["file"], format!("d{index:02}/iris.csv"));
        let kind = if index % 2 == 0 {
            "auth.required"
        } else {
            "integrity.mismatch"
        };
        assert_eq!(failure["kind"], kind);
        assert_ne!(failure["message"].as_str().ok_or("missing message")?, "");
    }
    let output = run(&["sync"])?;
    let stderr = text(&output.stderr)?;
    assert!(stderr.contains("d09/iris.csv"), "{stderr}");
    assert!(!stderr.contains("d10/iris.csv"), "{stderr}");
    assert!(!stderr.contains("d11/iris.csv"), "{stderr}");
    assert!(stderr.contains("and 2 more"), "{stderr}");
    assert!(servers.iter().all(|server| server.requests().len() == 2));
    Ok(())
}

/// data.lock's table and one list part for `name`, a recipe's result holding `files` made from
/// iris, as fetchloom writes them: the table's text, the part's path in the project and its text.
fn result(name: &str, files: &[(&str, &[u8])]) -> (String, String, String) {
    let mut tree = String::new();
    for (path, bytes) in files {
        let _ = writeln!(tree, "{path}\t{}\t{}", bytes.len(), sha256_hex(bytes));
    }
    let part_text = format!("path\tsize\tsha256\n{tree}");
    let listed = sha256_hex(part_text.as_bytes());
    let part = format!("data.lock.d/{name}.{}.tsv.zst", &listed[..12]);
    let bytes: usize = files.iter().map(|(_, bytes)| bytes.len()).sum();
    let table = format!(
        "\n[[dataset]]\nname = \"{name}\"\nfrom = [\n  {{ name = \"iris\", content = \"sha256:{}\" }},\n]\nrun = [\"python3\", \"clean.py\", \"{{iris}}\", \"{{out}}\"]\ncode = []\nkey = \"sha256:{}\"\nlocked = \"2026-11-10T00:00:00Z\"\ntree = \"sha256:{}\"\nfiles_in = [\"{part}\"]\nfile_count = {}\nfile_bytes = {bytes}\nfiles_sha256 = \"sha256:{listed}\"\n",
        sha256_hex(b"iris"),
        sha256_hex(b"key"),
        sha256_hex(tree.as_bytes()),
        files.len(),
    );
    (table, part, part_text)
}

/// only `fetchloom build` makes a result, so another command writing data.lock keeps its pin
/// while data.toml has the recipe, and a damaged list's refusal gives advice that works.
#[test]
fn a_pinned_recipe_result_stays_and_a_damaged_list_is_refused() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(
        "/iris.csv",
        vec![file(b"iris", "\"v1\""), file(b"iris, again", "\"v2\"")],
    )?;
    sandbox.allow(&server.url);
    assert_eq!(sandbox.run(&["add", &server.url])?.status.code(), Some(0));
    let manifest = sandbox.work().join("data.toml");
    let recipe = "\n[datasets.clean]\nfrom = [\"iris\"]\nrun = [\"python3\", \"clean.py\", \"{iris}\", \"{out}\"]\n";
    fs::write(
        &manifest,
        format!("{}{recipe}", fs::read_to_string(&manifest)?),
    )?;
    let (table, part, part_text) = result("clean", &[("iris.csv", b"abc")]);
    let lock = sandbox.work().join("data.lock");
    fs::create_dir(sandbox.work().join("data.lock.d"))?;
    fs::write(
        sandbox.work().join(&part),
        zstd::encode_all(part_text.as_bytes(), 9)?,
    )?;
    fs::write(&lock, format!("{}{table}", fs::read_to_string(&lock)?))?;

    let upgraded = sandbox.run(&["lock", "--upgrade"])?;
    assert_eq!(
        upgraded.status.code(),
        Some(0),
        "{}",
        text(&upgraded.stderr)?
    );
    let rewritten = fs::read_to_string(&lock)?;
    assert!(
        rewritten.contains("size = 11"),
        "the upgrade wrote data.lock: {rewritten}"
    );
    assert!(
        rewritten.starts_with(&format!("version = 1\n{table}")),
        "{rewritten}"
    );
    assert!(sandbox.work().join(&part).exists());
    let status = sandbox.run(&["--json", "status"])?;
    assert_eq!(status.status.code(), Some(0), "{}", text(&status.stderr)?);

    let (_, _, other) = result("clean", &[("iris.csv", b"abd")]);
    fs::write(
        sandbox.work().join(&part),
        zstd::encode_all(other.as_bytes(), 9)?,
    )?;
    let damaged = sandbox.run(&["--json", "status"])?;
    assert_eq!(damaged.status.code(), Some(50));
    assert_eq!(
        help(&damaged)?,
        "restore data.lock and data.lock.d from git, or delete data.lock and run `fetchloom lock`, which resolves every dataset again and may pin newer data"
    );
    Ok(())
}

/// no link fetches an import, so the help offers only ways without a network and no rerun, and
/// copying the files from the folder they came from, as it says, lets the sync work offline.
#[test]
fn an_import_missing_offline_is_never_offered_a_download() -> Result {
    let sandbox = Sandbox::new()?;
    let flowers = sandbox.folder("flowers")?;
    fs::write(flowers.join("sepals.csv"), b"length,width\n5.1,3.5\n")?;
    assert!(sandbox.run(&["add", "../flowers"])?.status.success());
    let clone = crate::project::clone(&sandbox, "clone")?;
    let cache = sandbox.folder("clone-cache")?;
    let sync = || {
        sandbox
            .command_in(&clone, &["--json", "sync", "--locked"])
            .env("FETCHLOOM_CACHE", &cache)
            .env("FETCHLOOM_OFFLINE", "1")
            .output()
    };
    let output = sync()?;
    assert_eq!(output.status.code(), Some(20), "{}", text(&output.stdout)?);
    let document = crate::support::json(&output)?;
    let error = &document["errors"][0];
    assert_eq!(error["kind"], "offline.missing");
    assert_eq!(
        error["message"],
        "flowers was imported from files, and this machine's cache does not hold them"
    );
    assert_eq!(error["details"], serde_json::json!(["flowers/sepals.csv"]));
    assert_eq!(
        error["help"],
        "no link fetches an import, so going online does not help: copy its files from the folder they came from to data/flowers by hand, then run `fetchloom sync`, or sync with `--mirror <folder>` naming a folder mirror that holds them"
    );
    assert_eq!(error["fix"], Value::Null);
    let placed = clone.join("data").join("flowers");
    fs::create_dir_all(&placed)?;
    fs::copy(flowers.join("sepals.csv"), placed.join("sepals.csv"))?;
    let output = sync()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
    Ok(())
}

/// offline asks no web mirror, so the error never claims it lacks the files, and a folder mirror
/// a push filled then serves the same run offline.
#[test]
fn offline_says_a_web_mirror_was_not_asked_and_names_a_folder_mirror() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/iris.csv", vec![file(b"iris", "\"v1\"")])?;
    sandbox.allow(&server.url);
    assert!(sandbox.run(&["add", &server.url])?.status.success());
    let clone = crate::project::clone(&sandbox, "clone")?;
    let cache = sandbox.folder("clone-cache")?;
    let sync = |mirror: &str| {
        sandbox
            .command_in(
                &clone,
                &[
                    "--json",
                    "--offline",
                    "sync",
                    "--locked",
                    "--mirror",
                    mirror,
                ],
            )
            .env("FETCHLOOM_CACHE", &cache)
            .output()
    };
    // offline, nothing listening there is ever reached
    let output = sync("http://127.0.0.1:9/")?;
    assert_eq!(output.status.code(), Some(20), "{}", text(&output.stdout)?);
    let document = crate::support::json(&output)?;
    assert_eq!(document["errors"][0]["kind"], "offline.missing");
    assert_eq!(
        document["errors"][0]["help"],
        "offline asks no web mirror, so http://127.0.0.1:9 was not asked: run it again without --offline to download them, or set FETCHLOOM_CACHE to a cache folder that already holds them, or name a folder mirror that holds them with `--mirror <folder>`"
    );
    let mirror = sandbox.folder("mirror")?;
    let mirror = mirror.to_string_lossy();
    let pushed = sandbox.run(&["--yes", "push", "--to", &mirror])?;
    assert_eq!(pushed.status.code(), Some(0), "{}", text(&pushed.stderr)?);
    let output = sync(&mirror)?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
    assert_eq!(
        fs::read(clone.join("data").join("iris").join("iris.csv"))?,
        b"iris"
    );
    Ok(())
}

/// an edit in place through a hardlink changed the cache's copy too; offline the edit stays, and
/// online the file is put back with the edit kept beside it.
#[test]
fn a_refetch_of_a_changed_cache_copy_that_fails_says_why() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(
        "/iris.csv",
        vec![file(b"iris", "\"v1\""), file(b"iris", "\"v1\"")],
    )?;
    sandbox.allow(&server.url);
    let added = crate::support::json(&sandbox.run(&["--json", "add", &server.url])?)?;
    if added["work"]["hardlinks"] != 1 {
        return crate::support::skipped(
            "placed-hardlinks",
            "this disk places no read-only file as a hardlink",
        );
    }
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    crate::support::edit(&placed, b"changed in place\n")?;
    let output = sandbox.run(&["--json", "--offline", "sync"])?;
    assert_eq!(output.status.code(), Some(20), "{}", text(&output.stdout)?);
    let error = &crate::support::json(&output)?["errors"][0];
    assert_eq!(
        error["details"],
        serde_json::json!([
            "iris/iris.csv",
            "data/iris/iris.csv is linked to the cache's copy, which was changed, so its pinned bytes must be fetched again"
        ])
    );
    assert_eq!(fs::read(&placed)?, b"changed in place\n");
    let synced = sandbox.run(&["sync"])?;
    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    assert_eq!(fs::read(&placed)?, b"iris");
    assert_eq!(
        fs::read(placed.with_file_name("iris.csv.changed"))?,
        b"changed in place\n"
    );
    Ok(())
}
