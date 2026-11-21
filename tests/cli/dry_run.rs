//! `--dry-run` on add, remove, sync, get and cache gc tells the plan and changes nothing on disk.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::project::added;
use crate::support::{Refusing, Result, Routes, Sandbox, Server, file, json, reply, text, warning};

const IRIS: &[u8] = b"sepal_length,species\n5.1,setosa\n";
const PATH: &str = "/files/iris.csv";

/// every file and folder under `root` with its bytes, or nothing when `root` does not exist.
fn tree(root: &Path) -> Result<BTreeMap<PathBuf, Option<Vec<u8>>>> {
    let mut found = BTreeMap::new();
    let mut folders = vec![root.to_path_buf()];
    while let Some(folder) = folders.pop() {
        let Ok(entries) = fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries {
            let path = entry?.path();
            if path.is_dir() {
                found.insert(path.clone(), None);
                folders.push(path);
            } else {
                found.insert(path.clone(), Some(fs::read(&path)?));
            }
        }
    }
    Ok(found)
}

/// a dry run of lock refuses a link at a private address as lock and sync's dry run do, before
/// saying it would pin it.
#[test]
fn lock_dry_run_refuses_a_private_address_as_lock_does() -> Result {
    let sandbox = Sandbox::new()?;
    fs::write(
        sandbox.work().join("data.toml"),
        "[datasets.x]\nref = \"http://10.0.0.7/iris.csv\"\n",
    )?;
    for args in [
        &["--json", "lock", "--dry-run"][..],
        &["--json", "sync", "--dry-run"],
        &["--json", "lock"],
    ] {
        let output = sandbox.run(args)?;
        assert_eq!(
            output.status.code(),
            Some(10),
            "{args:?}: {}",
            text(&output.stderr)?
        );
        let error = &json(&output)?["errors"][0];
        assert_eq!(error["kind"], "network.refused", "{args:?}");
        assert_eq!(
            error["message"],
            "fetchloom will not follow http://10.0.0.7/iris.csv: it points at the private address 10.0.0.7",
            "{args:?}"
        );
    }
    assert!(!sandbox.work().join("data.lock").exists());
    Ok(())
}

/// a folder listing as python's http.server writes it, of `entries` in `folder`.
fn listing(folder: &str, entries: &[&str]) -> Vec<crate::support::Part> {
    let mut items = String::new();
    for entry in entries {
        items.push_str("<li><a href=\"");
        items.push_str(entry);
        items.push_str("\">");
        items.push_str(entry);
        items.push_str("</a></li>\n");
    }
    let html = format!(
        "<!DOCTYPE HTML>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<title>Directory listing for {folder}</title>\n</head>\n<body>\n<h1>Directory listing for {folder}</h1>\n<hr>\n<ul>\n{items}</ul>\n<hr>\n</body>\n</html>\n"
    );
    reply(
        "200 OK",
        &[
            ("Content-Type", "text/html; charset=utf-8"),
            ("Content-Length", &html.len().to_string()),
        ],
        html.as_bytes(),
    )
}

/// each run's exit code and what it planned in [`a_dry_run_plans_each_download_once`]: its json's
/// changes, an offline run's refusal, or the lines naming each file to fetch and what it would
/// download.
fn plans_each_download_once() -> [(Option<i32>, String); 7] {
    let places = r#""place":["data/data/a.csv","data/data/b.csv","data/data/sub/c.csv""#;
    [
        (
            Some(0),
            format!(
                r#"{{"download":[{{"path":"data/a.csv","size":null}},{{"path":"data/b.csv","size":null}},{{"path":"data/sub/c.csv","size":null}}],{places}],"remove":[],"replace":[],"to_download":{{"bytes":0,"files":3,"sizes_unknown":3,"time":null}},"unpack":[]}}"#
            ),
        ),
        (
            Some(0),
            "Would fetch data/a.csv · size unknown\nWould fetch data/b.csv · size unknown\nWould fetch data/sub/c.csv · size unknown".to_owned(),
        ),
        (
            Some(0),
            format!(
                r#"{{"download":[{{"path":"data/a.csv","size":2}},{{"path":"data/b.csv","size":3}},{{"path":"data/sub/c.csv","size":32}}],{places}],"remove":[],"replace":[],"to_download":{{"bytes":37,"files":3,"sizes_unknown":0,"time":null}},"unpack":[]}}"#
            ),
        ),
        (
            Some(0),
            "Would fetch data/a.csv · 2 B\nWould fetch data/b.csv · 3 B\nWould fetch data/sub/c.csv · 32 B\nWould download 3 files · 37 B".to_owned(),
        ),
        (
            Some(20),
            "error: offline, and 3 files data.lock names are not in the cache\n  data/a.csv\n  data/b.csv\n  data/sub/c.csv\n  help: run it again without --offline to download them, or set FETCHLOOM_CACHE to a cache folder that already holds them, or name a folder mirror that holds them with `--mirror <folder>`\n".to_owned(),
        ),
        (
            Some(0),
            format!(
                r#"{{"download":[{{"path":"more/d.csv","size":null}},{{"path":"data/a.csv","size":2}},{{"path":"data/b.csv","size":3}},{{"path":"data/sub/c.csv","size":32}}],{places},"data/more/d.csv"],"remove":[],"replace":[],"to_download":{{"bytes":37,"files":4,"sizes_unknown":1,"time":null}},"unpack":[]}}"#
            ),
        ),
        (
            Some(0),
            "Would fetch more/d.csv · size unknown\nWould fetch data/a.csv · 2 B\nWould fetch data/b.csv · 3 B\nWould fetch data/sub/c.csv · 32 B\nWould download 4 files · 37 B and 1 of unknown size".to_owned(),
        ),
    ]
}

/// a dry run plans each file it would fetch once, as a download would fetch it: adding a folder
/// whose files relocking downloads, syncing a clone whose cache lacks every pinned file, offline
/// too, and both in one sync. each run's exit code, the downloads its json plans and the lines
/// -v tells of them are as a sync that built each job as a download does told them.
#[test]
fn a_dry_run_plans_each_download_once() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = Routes::start(vec![
        (
            "/data/".to_owned(),
            listing("/data/", &["a.csv", "b.csv", "sub/"]),
        ),
        ("/data/sub/".to_owned(), listing("/data/sub/", &["c.csv"])),
        ("/data/a.csv".to_owned(), file(b"a\n", "\"a\"")),
        ("/data/b.csv".to_owned(), file(b"bb\n", "\"b\"")),
        ("/data/sub/c.csv".to_owned(), file(IRIS, "\"c\"")),
        ("/more/".to_owned(), listing("/more/", &["d.csv"])),
        ("/more/d.csv".to_owned(), file(b"ddd\n", "\"d\"")),
    ])?;
    sandbox.allow(&routes.url);
    let (data, more) = (
        format!("{}/data/", routes.url),
        format!("{}/more/", routes.url),
    );
    let (clone, empty) = (sandbox.folder("clone")?, sandbox.folder("empty-cache")?);
    // a run's exit code and what it planned: its json's changes, what an offline run refused, or
    // the lines that name each file a run would fetch and what it would download
    let told = |args: &[&str]| -> Result<(Option<i32>, String)> {
        let mut command = sandbox.command(args);
        if fs::exists(sandbox.work().join("data.lock"))? {
            command.current_dir(&clone).env("FETCHLOOM_CACHE", &empty);
        }
        let output = command.output()?;
        let said = match args[0] {
            "--json" => json(&output)?["result"]["changes"].to_string(),
            "--offline" => text(&output.stderr)?,
            _ => {
                let lines = format!("{}{}", text(&output.stdout)?, text(&output.stderr)?);
                let planned = lines.lines().map(str::trim_start).filter(|line| {
                    line.starts_with("Would fetch ") || line.starts_with("Would download ")
                });
                planned.collect::<Vec<_>>().join("\n")
            }
        };
        Ok((output.status.code(), said))
    };
    let mut runs = vec![
        told(&["--json", "add", &data, "--dry-run"])?,
        told(&["-v", "add", &data, "--dry-run"])?,
    ];
    let added = sandbox.run(&["add", &data])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    for name in ["data.toml", "data.lock"] {
        fs::copy(sandbox.work().join(name), clone.join(name))?;
    }
    runs.push(told(&["--json", "sync", "--dry-run"])?);
    runs.push(told(&["-v", "sync", "--dry-run"])?);
    runs.push(told(&["--offline", "sync", "--dry-run"])?);
    // a dataset relocking pins beside those the cache lacks
    let manifest = clone.join("data.toml");
    let pinned = fs::read_to_string(&manifest)?;
    fs::write(
        &manifest,
        format!("{pinned}\n[datasets.more]\nref = \"{more}\"\n"),
    )?;
    runs.push(told(&["--json", "sync", "--dry-run"])?);
    runs.push(told(&["-v", "sync", "--dry-run"])?);
    let expected = plans_each_download_once();
    for (run, ((code, said), (want_code, want))) in runs.iter().zip(expected).enumerate() {
        assert_eq!((*code, said), (want_code, &want), "run {run}");
    }
    let downloads = |said: &str| -> Result<Option<usize>> {
        let changes: serde_json::Value = serde_json::from_str(said)?;
        Ok(changes["download"].as_array().map(Vec::len))
    };
    assert_eq!(downloads(&runs[0].1)?, Some(3), "{}", runs[0].1);
    assert_eq!(downloads(&runs[2].1)?, Some(3), "{}", runs[2].1);
    assert_ne!(runs[4].0, Some(0), "{}", runs[4].1);
    assert_eq!(downloads(&runs[5].1)?, Some(4), "{}", runs[5].1);
    Ok(())
}

#[test]
fn sync_dry_run_on_a_fresh_clone_tells_what_it_would_place() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let clone = sandbox.folder("clone")?;
    for name in ["data.toml", "data.lock"] {
        fs::copy(sandbox.work().join(name), clone.join(name))?;
    }
    let (project, cache) = (tree(&clone)?, tree(&sandbox.cache())?);
    let output = sandbox
        .command_in(&clone, &["sync", "--dry-run"])
        .output()?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert_eq!(
        stderr,
        "      Would place data/iris · 1 file\n         Ok dry run, nothing was changed\n"
    );
    let document = json(
        &sandbox
            .command_in(&clone, &["--json", "sync", "-n"])
            .output()?,
    )?;
    let result = &document["result"];
    assert_eq!(result["dry_run"], true);
    assert_eq!(result["lock_changed"], false);
    assert_eq!(result["placement"]["placed"], 1);
    assert_eq!(result["placement"]["unchanged"], 0);
    assert_eq!(
        result["changes"],
        serde_json::json!({
            "place": ["data/iris/iris.csv"],
            "replace": [],
            "remove": [],
            "download": [],
            "unpack": [],
            "to_download": {"files": 0, "bytes": 0, "sizes_unknown": 0, "time": null},
        })
    );
    assert_eq!(document["work"]["requests"], 0);
    assert_eq!(tree(&clone)?, project, "the clone is as it was");
    assert_eq!(tree(&sandbox.cache())?, cache, "the cache is as it was");
    assert_eq!(server.requests().len(), 1, "only add downloaded");
    Ok(())
}

/// a dry run warns of a `path` that names the data folder again, as the sync would, so the
/// mistake shows before anything lands in data/data.
#[test]
fn sync_dry_run_warns_of_a_path_that_names_the_data_folder_again() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let manifest = sandbox.work().join("data.toml");
    let text_now = fs::read_to_string(&manifest)?;
    fs::write(&manifest, format!("{text_now}path = \"data/iris-win\"\n"))?;
    let said = "path = \"data/iris-win\" starts with the data folder's name, so datasets.iris lands in data/data/iris-win";
    let output = sandbox.run(&["sync", "--dry-run"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(stderr.contains(&format!("warning: {said}\n")), "{stderr}");
    assert!(
        stderr.contains("Would place data/data/iris-win"),
        "{stderr}"
    );
    let document = json(&sandbox.run(&["--json", "sync", "--dry-run"])?)?;
    assert_eq!(
        warning(&document, "place.path_folder")["message"],
        said,
        "{document}"
    );
    assert!(!sandbox.work().join("data").join("data").exists());
    Ok(())
}

#[test]
fn sync_dry_run_lists_downloads_with_their_sizes_and_fetches_none() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    crate::support::delete(&placed)?;
    let cold = sandbox.cache().with_file_name("cold");
    let project = tree(&sandbox.work())?;
    let output = sandbox
        .command(&["sync", "--dry-run"])
        .env("FETCHLOOM_CACHE", &cold)
        .output()?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert_eq!(
        stderr,
        "      Would fetch iris/iris.csv · 32 B\n      Would place data/iris · 1 file\n      Would put back data/iris/iris.csv, which was deleted or moved, since data.lock pins it\n         Ok dry run, nothing was changed\n"
    );
    let document = json(
        &sandbox
            .command(&["--json", "sync", "--dry-run"])
            .env("FETCHLOOM_CACHE", &cold)
            .output()?,
    )?;
    assert_eq!(
        document["result"]["changes"]["download"],
        serde_json::json!([{ "path": "iris/iris.csv", "size": IRIS.len() }])
    );
    assert!(!cold.exists(), "a dry run creates no cache");
    assert_eq!(tree(&sandbox.work())?, project);
    assert_eq!(server.requests().len(), 1, "nothing was downloaded");

    let offline = sandbox
        .command(&["--offline", "sync", "--dry-run"])
        .env("FETCHLOOM_CACHE", &cold)
        .output()?;
    assert_eq!(
        offline.status.code(),
        Some(20),
        "offline, the run would stop"
    );
    Ok(())
}

#[test]
fn sync_dry_run_tells_the_lock_a_new_dataset_would_get() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    let other = Server::start("/files/cows.csv", vec![file(b"cow\nbess\n", "\"c1\"")])?;
    sandbox.allow(&other.url);
    added(&sandbox, &server)?;
    let manifest = sandbox.work().join("data.toml");
    let updated = format!(
        "{}\n[datasets.cows]\nref = \"{}\"\n",
        fs::read_to_string(&manifest)?,
        other.url
    );
    fs::write(&manifest, updated)?;
    let (project, cache) = (tree(&sandbox.work())?, tree(&sandbox.cache())?);
    let output = sandbox.run(&["sync", "-n"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert_eq!(
        stderr,
        format!(
            "      Would pin cows, new in data.lock: {} · 1 file · size unknown\n      Would fetch cows/cows.csv · size unknown\n      Would place data/cows · 1 file\n         Ok dry run, nothing was changed\n",
            other.url
        )
    );
    let document = json(&sandbox.run(&["--json", "sync", "-n"])?)?;
    let result = &document["result"];
    assert_eq!(result["lock_changed"], true);
    assert_eq!(result["placement"]["placed"], 1);
    assert_eq!(result["placement"]["unchanged"], 1);
    assert_eq!(
        result["changes"]["download"],
        serde_json::json!([{ "path": "cows/cows.csv", "size": null }])
    );
    assert_eq!(other.requests().len(), 0, "a plain link is not asked");
    assert_eq!(tree(&sandbox.work())?, project, "data.lock is as it was");
    assert_eq!(tree(&sandbox.cache())?, cache);
    Ok(())
}

#[test]
fn sync_dry_run_with_force_tells_what_it_would_replace() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    crate::support::delete(&placed)?;
    fs::write(&placed, b"mine\n")?;
    let (project, cache) = (tree(&sandbox.work())?, tree(&sandbox.cache())?);
    let kept = sandbox.run(&["sync", "-n"])?;
    let stderr = text(&kept.stderr)?;
    assert_eq!(kept.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.starts_with("warning: data/iris/iris.csv "),
        "{stderr}"
    );
    assert!(
        stderr.ends_with("         Ok dry run, nothing was changed\n"),
        "{stderr}"
    );
    let output = sandbox.run(&["sync", "--force", "--dry-run"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert_eq!(
        stderr,
        "      Would renew data/iris · 1 file\nwarning: data/iris/iris.csv is your edit, and --force would replace it\n         Ok dry run, nothing was changed\n"
    );
    let document = json(&sandbox.run(&["--json", "sync", "--force", "-n"])?)?;
    let result = &document["result"];
    assert_eq!(result["dry_run"], true);
    assert_eq!(result["lock_changed"], false);
    assert_eq!(result["placement"]["replaced"], 1);
    assert_eq!(
        result["placement"]["overwritten"],
        serde_json::json!([{"path": "data/iris/iris.csv", "reason": "changed"}])
    );
    assert_eq!(
        result["changes"]["replace"],
        serde_json::json!(["data/iris/iris.csv"])
    );
    assert_eq!(result["changes"]["download"], serde_json::json!([]));
    assert_eq!(tree(&sandbox.work())?, project, "the edit is still there");
    assert_eq!(tree(&sandbox.cache())?, cache);
    let forced = sandbox.run(&["sync", "--force"])?;
    let stderr = text(&forced.stderr)?;
    assert_eq!(forced.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.starts_with(
            "    Placing 1 file\n   Replaced data/iris/iris.csv · discarding your edit\n"
        ),
        "{stderr}"
    );
    Ok(())
}

#[test]
fn remove_dry_run_tells_what_it_would_remove() -> Result {
    let sandbox = Sandbox::new()?;
    let first = Server::start("/a.csv", vec![file(b"a\n", "\"a\"")])?;
    let second = Server::start("/b.csv", vec![file(b"b\n", "\"b\"")])?;
    added(&sandbox, &first)?;
    added(&sandbox, &second)?;
    let (project, cache) = (tree(&sandbox.work())?, tree(&sandbox.cache())?);
    let output = sandbox.run(&["remove", "a", "--dry-run"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    sandbox
        .filters()
        .bind(|| insta::assert_snapshot!("remove_dry_run", stderr));
    let verbose = text(&sandbox.run(&["-v", "remove", "a", "-n"])?.stderr)?;
    assert!(
        verbose.contains("\n            data/a/a.csv\n"),
        "-v names each file\n{verbose}"
    );
    let document = json(&sandbox.run(&["--json", "remove", "a", "-n"])?)?;
    let result = &document["result"];
    assert_eq!(result["dry_run"], true);
    assert_eq!(result["datasets"], serde_json::json!(["a"]));
    assert_eq!(result["lock_changed"], true);
    assert_eq!(result["placement"]["removed"], 1);
    assert_eq!(
        result["changes"]["remove"],
        serde_json::json!(["data/a/a.csv"])
    );
    assert_eq!(tree(&sandbox.work())?, project, "the project is as it was");
    assert_eq!(tree(&sandbox.cache())?, cache);
    Ok(())
}

#[test]
fn cache_gc_dry_run_tells_what_it_would_free() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    fs::remove_file(sandbox.work().join("data.toml"))?;
    let cache = tree(&sandbox.cache())?;
    let output = sandbox.run(&["cache", "gc", "--now", "--dry-run"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert_eq!(
        stderr,
        "      Would remove 1 file · 32 B from the cache\n         Ok dry run, nothing was changed\n"
    );
    let young = text(&sandbox.run(&["cache", "gc", "-n"])?.stderr)?;
    assert_eq!(
        young,
        "         Ok nothing to free yet; dry run, nothing was changed\n  hint: 32 B no project needs is kept for a day in case you want it back; `fetchloom cache gc --now` frees it now\n"
    );
    let document = json(&sandbox.run(&["--json", "cache", "gc", "--now", "-n"])?)?;
    let result = &document["result"];
    assert_eq!(result["dry_run"], true);
    assert_eq!(result["removed_objects"], 1);
    assert_eq!(result["removed_bytes"], IRIS.len());
    let removed: Vec<&str> = result["changes"]["remove"]
        .as_array()
        .ok_or("no paths")?
        .iter()
        .filter_map(serde_json::Value::as_str)
        .collect();
    assert!(
        removed.iter().any(|path| path.contains("objects")),
        "{removed:?}"
    );
    assert!(
        removed.iter().any(|path| path.contains("projects")),
        "the forgotten project's record: {removed:?}"
    );
    assert_eq!(tree(&sandbox.cache())?, cache, "the cache is as it was");
    let real = json(&sandbox.run(&["--json", "cache", "gc", "--now"])?)?;
    assert_eq!(
        real["result"]["removed_objects"], 1,
        "gc frees what it said it would"
    );
    assert!(real["result"].get("dry_run").is_none());
    Ok(())
}

/// the lines are a cached file no project needs and an unfinished download no fetchloom holds.
#[test]
fn cache_gc_dry_run_tells_its_lines_as_the_real_gc_does() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    fs::remove_file(sandbox.work().join("data.toml"))?;
    let key = "ab".repeat(32);
    let partial = sandbox.cache().join("partial").join(&key[..1]);
    fs::create_dir_all(&partial)?;
    for (extension, bytes) in [("part", &b"sepal"[..]), ("json", b"{}"), ("lock", b"")] {
        fs::write(partial.join(format!("{key}.{extension}")), bytes)?;
    }
    let told = |args: &[&str], verb: &str| -> Result<Vec<String>> {
        let output = sandbox.run(args)?;
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(0), "{stderr}");
        Ok(stderr
            .lines()
            .filter_map(|line| line.trim_start().strip_prefix(verb))
            .map(str::to_owned)
            .collect())
    };
    let dry = told(&["cache", "gc", "--now", "--dry-run"], "Would remove ")?;
    let real = told(&["cache", "gc", "--now"], "Removed ")?;
    assert_eq!(dry.len(), 2, "{dry:?}");
    assert_eq!(dry, real);
    Ok(())
}

/// a dry add asks a plain link for its first byte where the add downloads the file: it names the
/// dataset and its file as the server does, as the add then does, and fails where the add would.
#[test]
fn an_add_dry_run_asks_the_link_as_the_add_would() -> Result {
    use crate::support::reply;
    let sandbox = Sandbox::new()?;
    let named = || {
        reply(
            "200 OK",
            &[
                ("Content-Length", "32"),
                ("ETag", "\"r1\""),
                (
                    "Content-Disposition",
                    "attachment; filename=\"report-2024.csv\"",
                ),
            ],
            IRIS,
        )
    };
    let server = Server::start(
        "/disp/flowers.csv",
        vec![named(), named(), named(), named()],
    )?;
    let before = tree(&sandbox.work())?;
    let output = sandbox.run(&["add", &server.url, "--dry-run"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("Would fetch report-2024/report-2024.csv · 32 B\n"),
        "{stderr}"
    );
    let document = json(&sandbox.run(&["--json", "add", &server.url, "-n"])?)?;
    assert_eq!(
        document["result"]["datasets"],
        serde_json::json!(["report-2024"]),
        "{document}"
    );
    let renamed = text(
        &sandbox
            .run(&["add", &server.url, "--as", "rep", "-n"])?
            .stderr,
    )?;
    assert!(
        renamed.contains("Would fetch rep/report-2024.csv · 32 B\n"),
        "{renamed}"
    );
    let asked = server.requests();
    assert_eq!(asked.len(), 3);
    assert!(
        asked
            .iter()
            .all(|head| head.to_ascii_lowercase().contains("range: bytes=0-0")),
        "{asked:?}"
    );
    assert_eq!(tree(&sandbox.work())?, before);
    let added = sandbox.run(&["add", &server.url])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    assert!(
        sandbox
            .work()
            .join("data/report-2024/report-2024.csv")
            .is_file()
    );
    // nothing at the link, or no answer at all: the dry run fails as the add does
    let gone = || reply("404 Not Found", &[("Content-Length", "0")], b"");
    let missing = Server::start(PATH, vec![gone(), gone()])?;
    let refusing = Refusing::new()?;
    let closed = refusing.address;
    for link in [missing.url.clone(), format!("http://{closed}{PATH}")] {
        let run = |args: &[&str]| sandbox.command(args).env("FETCHLOOM_RETRIES", "0").output();
        let dry = run(&["--json", "add", &link, "--dry-run"])?;
        let real = run(&["--json", "add", &link])?;
        assert_ne!(real.status.code(), Some(0), "{link}");
        assert_eq!(
            dry.status.code(),
            real.status.code(),
            "{link}: {}",
            text(&dry.stdout)?
        );
        assert_eq!(
            json(&dry)?["errors"][0]["kind"],
            json(&real)?["errors"][0]["kind"],
            "{link}"
        );
    }
    Ok(())
}

/// a dry add asks the link for its first byte only, and writes nothing.
#[test]
fn add_dry_run_writes_nothing_and_downloads_nothing() -> Result {
    let sandbox = Sandbox::new()?;
    let iris = || file(IRIS, "\"v1\"");
    let server = Server::start(PATH, vec![iris(), iris(), iris()])?;
    fs::create_dir(sandbox.work().join(".git"))?;
    let project = tree(&sandbox.work())?;
    let output = sandbox.run(&["add", &server.url, "--dry-run"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    sandbox
        .filters()
        .bind(|| insta::assert_snapshot!("add_dry_run", stderr));
    let document = json(&sandbox.run(&["--json", "add", &server.url, "-n"])?)?;
    let result = &document["result"];
    assert_eq!(result["dry_run"], true);
    assert_eq!(result["datasets"], serde_json::json!(["iris"]));
    assert_eq!(result["lock_changed"], true);
    assert_eq!(result["placement"]["placed"], 1);
    assert_eq!(
        result["changes"]["place"],
        serde_json::json!(["data/iris/iris.csv"])
    );
    assert_eq!(
        result["changes"]["download"],
        serde_json::json!([{ "path": "iris/iris.csv", "size": 32 }])
    );
    assert_eq!(
        tree(&sandbox.work())?,
        project,
        "no data.toml, lock or data/"
    );
    assert!(!sandbox.cache().exists(), "no cache");
    let asked = server.requests();
    assert_eq!(asked.len(), 2);
    assert!(
        asked
            .iter()
            .all(|head| head.to_ascii_lowercase().contains("range: bytes=0-0")),
        "{asked:?}"
    );

    added(&sandbox, &server)?;
    let project = tree(&sandbox.work())?;
    let again = sandbox.run(&["add", &server.url, "-n"])?;
    assert_eq!(
        text(&again.stderr)?,
        "         Ok iris is already in data.toml; dry run, nothing was changed\n"
    );
    assert_eq!(tree(&sandbox.work())?, project);
    Ok(())
}

#[test]
fn a_dry_run_leaves_an_empty_cache_folder_empty() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    let other = Server::start("/files/cows.csv", vec![file(b"cow\nbess\n", "\"c1\"")])?;
    added(&sandbox, &server)?;
    let empty = sandbox.folder("empty")?;
    let project = tree(&sandbox.work())?;
    for args in [
        vec!["sync", "--dry-run"],
        vec!["add", other.url.as_str(), "--dry-run"],
        vec!["remove", "iris", "--dry-run"],
    ] {
        let output = sandbox
            .command(&args)
            .env("FETCHLOOM_CACHE", &empty)
            .output()?;
        assert_eq!(
            output.status.code(),
            Some(0),
            "{args:?}: {}",
            text(&output.stderr)?
        );
        assert_eq!(
            fs::read_dir(&empty)?.count(),
            0,
            "{args:?} leaves the cache folder empty"
        );
    }
    assert_eq!(tree(&sandbox.work())?, project);
    assert_eq!(
        other.requests().len(),
        1,
        "the dry add asks for the first byte"
    );
    Ok(())
}

#[test]
fn sync_dry_run_on_a_settled_project_needs_no_cache() -> Result {
    let sandbox = Sandbox::new()?;
    let zip = fs::read(crate::support::workspace().join("tests/fixtures/archives/python.zip"))?;
    let server = Server::start("/files/dataset.zip", vec![file(&zip, "\"z1\"")])?;
    added(&sandbox, &server)?;
    let settled = sandbox.run(&["sync"])?;
    assert_eq!(settled.status.code(), Some(0), "{}", text(&settled.stderr)?);
    // on a clock as coarse as hfs+ or fat, files placed within the state's own second stay
    // racy and are hashed again, so the state is written once more past that second
    let state = sandbox.work().join(".fetchloom").join("state");
    let written = fs::metadata(&state)?.modified()?;
    if written
        .duration_since(std::time::UNIX_EPOCH)?
        .subsec_nanos()
        == 0
    {
        sandbox.wait_for_a_later_time(written)?;
        fs::write(&state, fs::read(&state)?)?;
    }
    let cold = sandbox.cache().with_file_name("cold");
    let project = tree(&sandbox.work())?;
    for args in [
        vec!["sync", "--dry-run"],
        vec!["--offline", "sync", "--dry-run"],
    ] {
        let output = sandbox
            .command(&args)
            .env("FETCHLOOM_CACHE", &cold)
            .output()?;
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(0), "{args:?}: {stderr}");
        assert_eq!(
            stderr,
            "         Ok all pinned files are in place by size and time; dry run, nothing was changed\n",
            "{args:?}"
        );
    }
    assert!(!cold.exists(), "a dry run creates no cache");
    assert_eq!(tree(&sandbox.work())?, project);
    let real = sandbox
        .command(&["--offline", "sync"])
        .env("FETCHLOOM_CACHE", &cold)
        .output()?;
    assert_eq!(
        text(&real.stderr)?,
        "         Ok all pinned files are in place by size and time, nothing to do\n"
    );
    assert_eq!(server.requests().len(), 1);
    Ok(())
}

#[test]
fn cache_gc_dry_run_lists_the_marks_a_real_gc_removes() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    fs::remove_file(sandbox.work().join("data.toml"))?;
    let objects = sandbox.cache().join("objects");
    let object = tree(&objects)?
        .into_iter()
        .find_map(|(path, bytes)| bytes.map(|_| path))
        .ok_or("no object")?;
    let hex = format!(
        "{}{}",
        object
            .parent()
            .and_then(Path::file_name)
            .ok_or("no fan")?
            .to_string_lossy(),
        object.file_name().ok_or("no name")?.to_string_lossy()
    );
    let mark = sandbox.cache().join("changed").join(hex);
    fs::write(&mark, b"")?;
    let before = tree(&sandbox.cache())?;
    let document = json(&sandbox.run(&["--json", "cache", "gc", "--now", "--dry-run"])?)?;
    let listed: Vec<PathBuf> = document["result"]["changes"]["remove"]
        .as_array()
        .ok_or("no paths")?
        .iter()
        .filter_map(serde_json::Value::as_str)
        .map(PathBuf::from)
        .collect();
    assert!(listed.contains(&object), "{listed:?}");
    assert!(listed.contains(&mark), "{listed:?}");
    assert_eq!(tree(&sandbox.cache())?, before, "the cache is as it was");
    let real = sandbox.run(&["cache", "gc", "--now"])?;
    assert_eq!(real.status.code(), Some(0), "{}", text(&real.stderr)?);
    let after = tree(&sandbox.cache())?;
    let removed: Vec<&PathBuf> = before
        .keys()
        .filter(|path| !after.contains_key(*path))
        .collect();
    let mut expected: Vec<&PathBuf> = listed.iter().collect();
    expected.sort();
    assert_eq!(removed, expected, "the dry run lists what gc removes");
    Ok(())
}

#[test]
fn a_dry_run_audits_damaged_read_only_hardlinks() -> Result {
    use std::time::{Duration, UNIX_EPOCH};

    let sandbox = Sandbox::new()?;
    if !sandbox.links()?.hardlink {
        return crate::support::skipped("hardlinks", "this disk makes no hardlinks");
    }
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let lock: toml::Value = toml::from_str(&fs::read_to_string(sandbox.work().join("data.lock"))?)?;
    let digest = lock["dataset"][0]["files"][0]["sha256"]
        .as_str()
        .ok_or("missing digest")?;
    let object = sandbox
        .cache()
        .join("objects")
        .join(&digest[..2])
        .join(&digest[2..]);
    let placed = sandbox.work().join("data/iris/iris.csv");
    crate::support::delete(&placed)?;
    fs::hard_link(&object, &placed)?;
    crate::support::edit(&placed, IRIS)?;
    let recorded = UNIX_EPOCH + Duration::from_secs(1_600_000_000);
    fs::File::options()
        .write(true)
        .open(&placed)?
        .set_modified(recorded)?;
    let state_path = sandbox.work().join(".fetchloom/state");
    let mut state: serde_json::Value = serde_json::from_slice(&fs::read(&state_path)?)?;
    let record = &mut state["files"]["data/iris/iris.csv"];
    record["method"] = "hardlink".into();
    record["look"]["modified"] =
        u64::try_from(recorded.duration_since(UNIX_EPOCH)?.as_nanos())?.into();
    fs::write(&state_path, serde_json::to_vec(&state)?)?;
    let mut changed = IRIS.to_vec();
    changed[0] = b'S';
    crate::support::edit(&placed, &changed)?;
    fs::File::options()
        .write(true)
        .open(&placed)?
        .set_modified(recorded + Duration::from_secs(10))?;
    let mut permissions = fs::metadata(&placed)?.permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&placed, permissions)?;
    let before_project = tree(&sandbox.work())?;
    let before_cache = tree(&sandbox.cache())?;

    let dry = sandbox.run(&["--json", "--offline", "sync", "--force", "--dry-run"])?;
    assert_eq!(tree(&sandbox.work())?, before_project);
    assert_eq!(tree(&sandbox.cache())?, before_cache);
    assert!(fs::metadata(&object)?.permissions().readonly());
    let real = sandbox.run(&["--json", "--offline", "sync", "--force"])?;
    assert_eq!(real.status.code(), Some(20), "{}", text(&real.stdout)?);
    assert_eq!(dry.status.code(), Some(20), "{}", text(&dry.stdout)?);
    assert_eq!(
        json(&dry)?["errors"][0]["kind"],
        json(&real)?["errors"][0]["kind"]
    );
    assert_eq!(fs::read(&placed)?, changed);
    assert_eq!(server.requests().len(), 1);
    Ok(())
}

/// the link publishes no checksum, so only the download tells whether the file holds the pinned
/// bytes, and the dry run says it would check it.
#[test]
fn a_file_already_in_place_is_not_called_different_before_the_download() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), file(IRIS, "\"v1\"")])?;
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    fs::create_dir_all(placed.parent().ok_or("a placed file has a folder")?)?;
    fs::write(&placed, IRIS)?;
    let project = tree(&sandbox.work())?;
    let output = sandbox.run(&["add", &server.url, "--dry-run"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(!stderr.contains("differs"), "{stderr}");
    assert!(
        stderr.contains(
            "      Would check data/iris · 1 file is already there; the download tells whether it holds the pinned bytes"
        ),
        "{stderr}"
    );
    let document = json(&sandbox.run(&["--json", "add", &server.url, "-n"])?)?;
    assert_eq!(
        crate::support::warned(&document),
        Vec::<String>::new(),
        "{document}"
    );
    let changes = &document["result"]["changes"];
    assert_eq!(changes["place"], serde_json::json!([]), "{document}");
    assert_eq!(changes["replace"], serde_json::json!([]), "{document}");
    assert_eq!(
        changes["download"],
        serde_json::json!([{ "path": "iris/iris.csv", "size": 32 }]),
        "{document}"
    );
    assert_eq!(tree(&sandbox.work())?, project);
    assert_eq!(server.requests().len(), 2, "a first byte for each dry run");
    Ok(())
}

/// a dry get, like a dry add, asks each link's server for the file's first byte, where the get
/// downloads it, so it tells the size the server states and fails where the get would, as for a
/// link with nothing behind it; an archive's files stay unknown, it makes no cache folder, and it
/// fails offline as the get would.
#[test]
fn get_dry_run_tells_what_it_would_fetch_and_changes_nothing() -> Result {
    let sandbox = Sandbox::new()?;
    let csv = Server::start(PATH, vec![file(IRIS, "\"i\""), file(IRIS, "\"i\"")])?;
    let zipped = b"PK\x03\x04 a zip as its server sends it";
    let zip = Server::start("/files/flowers.zip", vec![file(zipped, "\"z\"")])?;
    let gone = Server::start(
        "/files/gone.csv",
        vec![reply("404 Not Found", &[("Content-Length", "0")], b"")],
    )?;
    let work = tree(&sandbox.work())?;
    let output = sandbox.run(&["get", &csv.url, "--dry-run"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert_eq!(
        stderr,
        format!(
            "      Would fetch iris.csv · {} B\n      Would put 1 file in iris\n         Ok dry run, nothing was changed\n",
            IRIS.len()
        )
    );
    let output = sandbox.run(&["get", &zip.url, "-n"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert_eq!(
        stderr,
        format!(
            "      Would fetch flowers.zip · {} B\n      Would open flowers.zip\n  hint: an archive's files are known once it is unpacked, so they are not counted\n      Would put its files in flowers\n         Ok dry run, nothing was changed\n",
            zipped.len()
        )
    );
    let document = json(&sandbox.run(&["--json", "get", &csv.url, "-n", "-o", "iris.csv"])?)?;
    let result = &document["result"];
    assert_eq!(result["dry_run"], true, "{document}");
    assert_eq!(
        result["file"],
        sandbox.work().join("iris.csv").display().to_string()
    );
    assert_eq!(
        result["changes"],
        serde_json::json!({
            "place": ["iris.csv"],
            "replace": [],
            "remove": [],
            "download": [{"path": "iris.csv", "size": IRIS.len()}],
            "unpack": [],
            "to_download": {"files": 1, "bytes": IRIS.len(), "sizes_unknown": 0, "time": null},
        })
    );
    assert_eq!(result["files"], serde_json::json!([]));
    let missing = sandbox.run(&["get", &gone.url, "-n"])?;
    let stderr = text(&missing.stderr)?;
    assert_eq!(missing.status.code(), Some(10), "{stderr}");
    assert!(stderr.contains("has nothing at"), "{stderr}");
    let offline = sandbox.run(&["--json", "--offline", "get", &csv.url, "-n"])?;
    assert_eq!(
        offline.status.code(),
        Some(20),
        "{}",
        text(&offline.stdout)?
    );
    assert_eq!(json(&offline)?["errors"][0]["kind"], "offline.missing");
    assert_eq!(tree(&sandbox.work())?, work, "nothing was written");
    assert!(!sandbox.cache().exists(), "no cache folder was made");
    assert_eq!(
        (
            csv.requests().len(),
            zip.requests().len(),
            gone.requests().len()
        ),
        (2, 1, 1),
        "a first byte for each dry run"
    );
    Ok(())
}

/// on a fresh machine no time shows; once a sync downloaded from a host, a dry run of other files
/// from it says about how long they take, and `--json` carries it.
#[test]
fn a_dry_run_times_its_downloads_once_a_host_is_known() -> Result {
    let sandbox = Sandbox::new()?;
    let (a, b) = (vec![b'a'; 1 << 20], vec![b'b'; 1 << 20]);
    let server = Routes::start(vec![
        ("/a.bin".to_owned(), file(&a, "\"a\"")),
        ("/b.bin".to_owned(), file(&b, "\"b\"")),
    ])?;
    sandbox.allow(&server.url);
    for name in ["a", "b"] {
        let link = format!("{}/{name}.bin", server.url);
        crate::support::ok(&sandbox.run(&["add", &link])?)?;
    }
    crate::support::delete(&sandbox.speeds())?;
    let clone = sandbox.folder("clone")?;
    for name in ["data.toml", "data.lock"] {
        fs::copy(sandbox.work().join(name), clone.join(name))?;
    }
    let cold = sandbox.cache().with_file_name("cold");
    let run = |args: &[&str]| {
        sandbox
            .command_in(&clone, args)
            .env("FETCHLOOM_CACHE", &cold)
            .output()
    };
    let unknown = text(&run(&["sync", "-n"])?.stderr)?;
    assert!(
        unknown.contains("Would download 2 files · 2.0 MiB\n"),
        "{unknown}"
    );
    let first = run(&["sync", "a"])?;
    let stderr = text(&first.stderr)?;
    assert_eq!(first.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("Downloading a/a.bin · 1.0 MiB\n"),
        "{stderr}"
    );
    let later = text(&run(&["sync", "-n"])?.stderr)?;
    let line = later
        .lines()
        .find(|line| line.contains("Would download"))
        .ok_or_else(|| format!("no total: {later}"))?;
    let said = line
        .strip_prefix("      Would download b/b.bin · 1.0 MiB · ")
        .ok_or_else(|| format!("not the new file's total: {later}"))?;
    assert!(
        said == "under 10 s" || said.starts_with("about "),
        "{later}"
    );
    let document = json(&run(&["--json", "sync", "-n"])?)?;
    let total = &document["result"]["changes"]["to_download"];
    assert_eq!(total["files"], 1, "{document}");
    assert_eq!(total["bytes"], b.len(), "{document}");
    assert_eq!(total["sizes_unknown"], 0, "{document}");
    assert_eq!(total["time"]["bound"], "about", "{document}");
    assert!(total["time"]["seconds"].is_u64(), "{document}");
    Ok(())
}

/// a download with no time to give says one shows once it starts only where a bar shows it: at a
/// terminal, never in a pipe or a log.
#[cfg(target_os = "linux")]
#[test]
fn only_a_terminal_hears_that_the_time_shows_once_it_starts() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Routes::start(vec![("/a.bin".to_owned(), file(&[b'a'; 1 << 20], "\"a\""))])?;
    sandbox.allow(&server.url);
    crate::support::ok(&sandbox.run(&["add", &format!("{}/a.bin", server.url)])?)?;
    let later = "the time shows once it starts";
    crate::support::delete(&sandbox.speeds())?;
    let (piped, cache) = crate::mirrors::cold(&sandbox, "piped")?;
    let mut sync = sandbox.command_in(&piped, &["sync"]);
    sync.env("FETCHLOOM_CACHE", &cache);
    let stderr = text(&sync.output()?.stderr)?;
    assert!(
        stderr.contains("Downloading a/a.bin · 1.0 MiB\n"),
        "{stderr}"
    );
    assert!(!stderr.contains(later), "{stderr}");
    crate::support::delete(&sandbox.speeds())?;
    let (shown, cache) = crate::mirrors::cold(&sandbox, "shown")?;
    let shown = shown.to_string_lossy();
    let mut sync = sandbox.command(&["-C", &shown, "sync"]);
    sync.env("FETCHLOOM_CACHE", &cache);
    let Some(screen) = sandbox.on_a_terminal_running(&sync)? else {
        return Ok(());
    };
    assert!(screen.contains(later), "{screen}");
    Ok(())
}

/// a clone's link to a private address `allowed_origins` does not list: status warns and counts no
/// download, and a dry run and the sync stop before saying they download anything.
#[test]
fn a_link_a_sync_refuses_is_never_announced_as_a_download() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    crate::support::ok(&sandbox.run(&["add", &server.url])?)?;
    let speeds = sandbox.speeds();
    fs::write(
        &speeds,
        r#"{"hosts":{"127.0.0.1":{"rate":1048576.0,"overhead":0.1,"heard":1}}}"#,
    )?;
    let (clone, cache) = crate::mirrors::cold(&sandbox, "clone")?;
    let run = |args: &[&str]| {
        sandbox
            .command_in(&clone, args)
            .env("FETCHLOOM_CACHE", &cache)
            .output()
    };
    let refusal = format!("will not follow {}", server.url);
    let announced = |told: &str| {
        ["Would download", "Would fetch", "Downloading"]
            .iter()
            .any(|line| told.contains(line))
    };
    let status = text(&run(&["status"])?.stderr)?;
    assert!(status.contains(&refusal), "{status}");
    assert!(!announced(&status), "{status}");
    for args in [&["sync", "--dry-run"][..], &["sync"]] {
        let output = run(args)?;
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(10), "{args:?}: {stderr}");
        assert!(stderr.contains(&refusal), "{args:?}: {stderr}");
        assert!(!announced(&stderr), "{args:?}: {stderr}");
    }
    assert_eq!(server.requests().len(), 1, "the clone asked nothing");
    Ok(())
}

/// a dry run of a file larger than the cache's disk has free says so, as the add or get itself
/// would before any download, rather than ending as if it would go through.
#[cfg(debug_assertions)]
#[test]
fn a_dry_run_tells_a_download_the_disk_has_no_room_for() -> Result {
    let sandbox = Sandbox::new()?;
    let huge = || {
        reply(
            "206 Partial Content",
            &[
                ("Content-Range", "bytes 0-0/1048576"),
                ("Content-Length", "1"),
                ("ETag", "\"v1\""),
            ],
            b"x",
        )
    };
    let server = Server::start("/huge.bin", vec![huge(), huge()])?;
    for command in ["add", "get"] {
        let output = sandbox
            .command(&[command, &server.url, "--dry-run"])
            .env("FETCHLOOM_TEST_FREE", "1000")
            .output()?;
        let stderr = text(&output.stderr)?;
        assert!(
            stderr.contains(&format!(
                "warning: this {command} needs at least 1.0 MiB on the disk holding the cache at "
            )),
            "{stderr}"
        );
        assert!(stderr.contains(", which has 1000 B free\n"), "{stderr}");
    }
    Ok(())
}

/// a dry add or get judges a web page where a file was asked as the download does, by the first
/// bytes it asks for: a page under a data file's name and one that names no file fail as the add
/// fails, while a csv its server calls a page, or a page `--web-page` asks for, is the file.
#[test]
fn a_dry_run_judges_a_web_page_as_the_download_does() -> Result {
    let sandbox = Sandbox::new()?;
    let page = b"<!doctype html><html><body>sign in</body></html>\n";
    let length = page.len().to_string();
    let html = [
        ("Content-Type", "text/html; charset=utf-8"),
        ("Content-Length", length.as_str()),
    ];
    let csv_length = IRIS.len().to_string();
    let labelled = [
        ("Content-Type", "text/html"),
        ("Content-Length", csv_length.as_str()),
    ];
    let source = Routes::start(vec![
        ("/page/x.csv".to_owned(), reply("200 OK", &html, page)),
        ("/page/listing".to_owned(), reply("200 OK", &html, page)),
        (
            "/files/labelled.csv".to_owned(),
            reply("200 OK", &labelled, IRIS),
        ),
    ])?;
    sandbox.allow(&source.url);
    let link = |path: &str| format!("{}{path}", source.url);
    let signed = "answered with a web page, such as a sign-in page, not a data file";
    let listing = "answered with a web page, such as a folder listing, not a file";
    for (command, path, said) in [
        ("add", "/page/x.csv", signed),
        ("add", "/page/listing", listing),
        ("get", "/page/x.csv", signed),
        ("get", "/page/listing", listing),
    ] {
        let output = sandbox.run(&[command, &link(path), "--dry-run"])?;
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(10), "{command} {path}: {stderr}");
        assert!(stderr.contains(said), "{command} {path}: {stderr}");
    }
    // the add itself fails the same way
    let added = sandbox.run(&["add", &link("/page/x.csv")])?;
    assert_eq!(added.status.code(), Some(10), "{}", text(&added.stderr)?);
    assert!(text(&added.stderr)?.contains(signed));
    for args in [
        vec!["add", "--dry-run", "--web-page"],
        vec!["get", "--dry-run", "--web-page"],
    ] {
        let mut args = args.clone();
        let page_link = link("/page/x.csv");
        args.push(&page_link);
        let output = sandbox.run(&args)?;
        assert_eq!(
            output.status.code(),
            Some(0),
            "{args:?}: {}",
            text(&output.stderr)?
        );
    }
    let output = sandbox.run(&["add", &link("/files/labelled.csv"), "--dry-run"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    Ok(())
}
