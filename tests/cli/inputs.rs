//! a project's inputs, against servers on this machine: the dataset names and members tables
//! data.toml and data.lock may give, the names of the files inside datasets, and
//! `cache gc --inputs`, which frees the cache's copy of raw data that only builds read where a
//! folder mirror holds a copy that hashes to its name in the same run.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::build::{build, building_in, with};
use crate::project::{IRIS, PATH};
use crate::recipes::{WRITE, object, unplace};
use crate::support::{Result, Sandbox, Server, edit, file, json, text};

/// a project in the sandbox's work folder with iris from `server`, set to `place = false`, and
/// the recipe [`WRITE`] built from it, then pushed with what it is made from into a folder
/// mirror, whose folder this gives.
fn built_and_pushed(sandbox: &Sandbox, server: &Server) -> Result<PathBuf> {
    with(sandbox, server, WRITE)?;
    unplace(&sandbox.work(), "iris")?;
    let built = build(sandbox, &["build"])?;
    assert_eq!(built.status.code(), Some(0), "{}", text(&built.stderr)?);
    let mirror = sandbox.folder("lab")?.join("mirror");
    let pushed = sandbox
        .command(&["--yes", "push", "--inputs", "clean", "--to"])
        .arg(&mirror)
        .output()?;
    assert_eq!(pushed.status.code(), Some(0), "{}", text(&pushed.stderr)?);
    Ok(mirror)
}

/// with a copy on a folder mirror that hashes to its name, the raw input goes from the cache and
/// the result stays; the run names the mirror it relied on and what freeing costs.
#[test]
fn gc_inputs_frees_raw_input_with_a_verified_mirror_and_names_the_costs() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    let mirror = built_and_pushed(&sandbox, &server)?;
    let output = sandbox
        .command(&["--yes", "cache", "gc", "--inputs", "--mirror"])
        .arg(&mirror)
        .output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    let stderr = text(&output.stderr)?;
    sandbox.filters().bind(|| insta::assert_snapshot!(stderr));
    assert!(!object(&sandbox.cache(), IRIS).exists(), "the raw input");
    assert!(object(&sandbox.cache(), b"done").is_file(), "the result");
    Ok(())
}

/// a dry run reads the copies and says what it would free, as a run would, and frees and asks
/// nothing. a copy that does not hash to its name keeps the file, with the mirror it is on.
#[test]
fn gc_inputs_dry_run_frees_and_asks_nothing() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    let mirror = built_and_pushed(&sandbox, &server)?;
    let dry = || {
        sandbox
            .command(&["--json", "cache", "gc", "--inputs", "--dry-run"])
            .env("FETCHLOOM_MIRRORS", &mirror)
            .output()
    };
    let output = dry()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
    let document = json(&output)?;
    let result = &document["result"];
    assert_eq!(result["dry_run"], true);
    assert_eq!(
        result["inputs"],
        json!({
            "freed_objects": 1,
            "freed_bytes": IRIS.len(),
            "datasets": [{
                "project": sandbox.work(),
                "name": "iris",
                "objects": 1,
                "bytes": IRIS.len(),
                "route": {"mirrors": [mirror]},
                "costs": {"recipes": ["clean"]},
            }],
            "kept": [],
        })
    );
    let removed = result["changes"]["remove"]
        .as_array()
        .ok_or("no changes.remove")?;
    let freed = object(&sandbox.cache(), IRIS);
    assert!(
        removed.contains(&Value::String(freed.display().to_string())),
        "{removed:?}"
    );
    assert!(freed.is_file(), "a dry run frees nothing");
    let told = text(
        &sandbox
            .command(&["cache", "gc", "--inputs", "-n", "--mirror"])
            .arg(&mirror)
            .output()?
            .stderr,
    )?;
    assert!(
        told.contains(&format!(
            " Would free iris · 1 file, {} B of raw input that only builds read\n",
            IRIS.len()
        )),
        "{told}"
    );
    assert!(told.contains("Ok dry run, nothing was changed\n"), "{told}");
    let plain = json(&sandbox.run(&["--json", "cache", "gc", "--dry-run"])?)?;
    assert_eq!(
        plain["result"].get("inputs"),
        None,
        "plain gc keeps its shape"
    );

    let mut damaged = IRIS.to_vec();
    damaged[0] ^= 1;
    // a mirror lays out its objects as the cache does
    edit(&object(&mirror, IRIS), &damaged)?;
    let result = &json(&dry()?)?["result"];
    assert_eq!(
        result["inputs"],
        json!({
            "freed_objects": 0,
            "freed_bytes": 0,
            "datasets": [],
            "kept": [{
                "project": sandbox.work(),
                "name": "iris",
                "objects": 1,
                "bytes": IRIS.len(),
                "reason": "damaged",
                "mirror": mirror,
            }],
        })
    );
    assert!(freed.is_file());
    Ok(())
}

/// freeing raw input takes a yes: a script without `--yes` gets `unconfirmed` with what a yes
/// would free and no command to run, since only a person may agree, and nothing changes.
#[test]
fn gc_inputs_needs_yes_without_a_terminal() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    let mirror = built_and_pushed(&sandbox, &server)?;
    let output = sandbox
        .command(&["--json", "cache", "gc", "--inputs", "--mirror"])
        .arg(&mirror)
        .output()?;
    assert_eq!(output.status.code(), Some(1), "{}", text(&output.stdout)?);
    let document = json(&output)?;
    let error = &document["errors"][0];
    assert_eq!(error["kind"], "unconfirmed");
    assert_eq!(error["fix"], Value::Null);
    assert_eq!(document["result"]["inputs"]["freed_objects"], 1);
    assert!(object(&sandbox.cache(), IRIS).is_file(), "nothing changed");
    let piped = sandbox
        .command(&["cache", "gc", "--inputs", "--mirror"])
        .arg(&mirror)
        .output()?;
    assert_eq!(piped.status.code(), Some(1));
    let told = text(&piped.stderr)?;
    assert!(told.contains("needs a yes"), "{told}");
    assert!(object(&sandbox.cache(), IRIS).is_file());
    Ok(())
}

/// at a terminal, gc shows what it would free and asks; no keeps everything as it was.
#[cfg(target_os = "linux")]
#[test]
fn no_at_the_question_frees_nothing() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    let mirror = built_and_pushed(&sandbox, &server)?;
    let mut command = sandbox.command(&["cache", "gc", "--inputs", "--mirror"]);
    command.arg(&mirror);
    let Some((code, screen)) =
        crate::support::typed(&sandbox.work(), &command, "", "from the cache?", "n\r")?
    else {
        return Ok(());
    };
    assert_eq!(code, Some(1), "{screen}");
    assert!(screen.contains("Would free iris"), "{screen}");
    assert!(screen.contains("Cancelled"), "{screen}");
    assert!(object(&sandbox.cache(), IRIS).is_file());
    Ok(())
}

/// what freeing costs, end to end: the project still syncs and builds as it is, while a check
/// that runs clean again takes iris back from the mirror, offline, and without the mirror has
/// nowhere to turn.
#[test]
fn a_check_after_gc_inputs_takes_the_raw_input_back_from_the_mirror() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    let mirror = built_and_pushed(&sandbox, &server)?;
    let gc = sandbox
        .command(&["--json", "--yes", "cache", "gc", "--inputs"])
        .env("FETCHLOOM_MIRRORS", &mirror)
        .output()?;
    assert_eq!(gc.status.code(), Some(0), "{}", text(&gc.stdout)?);
    assert_eq!(json(&gc)?["result"]["inputs"]["freed_bytes"], IRIS.len());
    for args in [
        &["--offline", "sync", "--locked"][..],
        &["--offline", "status"],
    ] {
        let output = sandbox.run(args)?;
        assert_eq!(
            output.status.code(),
            Some(0),
            "{args:?}: {}",
            text(&output.stderr)?
        );
    }
    let built = build(&sandbox, &["--offline", "build"])?;
    let told = text(&built.stderr)?;
    assert!(told.contains(" Up to date clean\n"), "{told}");
    assert!(
        !object(&sandbox.cache(), IRIS).exists(),
        "nothing took it back"
    );
    let check = |mirror: Option<&Path>| -> Result<std::process::Output> {
        let mut command = building_in(
            &sandbox,
            &sandbox.work(),
            &["--json", "--offline", "build", "--check"],
        )?;
        if let Some(mirror) = mirror {
            command.env("FETCHLOOM_MIRRORS", mirror);
        }
        Ok(command.output()?)
    };
    let stuck = check(None)?;
    assert_eq!(stuck.status.code(), Some(20), "{}", text(&stuck.stdout)?);
    assert_eq!(json(&stuck)?["errors"][0]["kind"], "offline.missing");
    let checked = check(Some(&mirror))?;
    assert_eq!(checked.status.code(), Some(0), "{}", text(&checked.stdout)?);
    let document = json(&checked)?;
    assert_eq!(document["result"]["steps"][0]["check"]["matching"], 1);
    assert_eq!(
        document["work"]["mirror_objects"], 1,
        "iris, from the mirror"
    );
    assert!(object(&sandbox.cache(), IRIS).is_file());
    assert_eq!(server.requests().len(), 1, "only the add asked the source");
    Ok(())
}

/// a dataset's name is its folder on every machine that syncs the project, so a name windows
/// keeps for a device is refused wherever data.toml or a cloned data.lock gives it, before any
/// request or write, while the name add makes from a file called that works.
#[test]
fn windows_device_names_are_refused_before_any_request_or_write() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/con.csv", vec![file(b"a,b\n1,2\n", "\"v1\"")])?;
    sandbox.allow(&server.url);
    let manifest = sandbox.work().join("data.toml");
    for device in ["con", "aux", "nul", "com1", "lpt1"] {
        fs::write(
            &manifest,
            format!("[datasets.{device}]\nref = \"{}\"\n", server.url),
        )?;
        let synced = sandbox.run(&["sync"])?;
        let stderr = text(&synced.stderr)?;
        assert_eq!(synced.status.code(), Some(50), "{stderr}");
        assert!(
            stderr.starts_with(&format!(
                "error: \"{device}\" is not a valid dataset name\n"
            )),
            "{stderr}"
        );
        assert!(
            stderr.contains(&format!(
                "help: rename it to \"{device}-data\" in data.toml"
            )),
            "{stderr}"
        );
    }
    assert_eq!(server.requests().len(), 0);
    assert!(!sandbox.work().join("data.lock").exists());
    assert!(!sandbox.work().join("data").exists());

    fs::remove_file(&manifest)?;
    let added = sandbox.run(&["add", &server.url])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    assert!(fs::read_to_string(&manifest)?.contains("[datasets.con-data]"));
    let lock = sandbox.work().join("data.lock");
    let cloned = fs::read_to_string(&lock)?.replace("name = \"con-data\"", "name = \"con\"");
    fs::write(&lock, &cloned)?;
    let synced = sandbox.run(&["sync"])?;
    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(50), "{stderr}");
    assert!(
        stderr.contains("data.lock names a dataset \"con\", which is not a valid dataset name"),
        "{stderr}"
    );
    assert_eq!(
        fs::read_to_string(&lock)?,
        cloned,
        "data.lock stays as it was"
    );
    Ok(())
}

/// a zip of `members`, each a path and its bytes.
fn zipped(members: &[(&str, &[u8])]) -> Result<Vec<u8>> {
    let mut archive = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (path, bytes) in members {
        archive.start_file(*path, zip::write::SimpleFileOptions::default())?;
        std::io::Write::write_all(&mut archive, bytes)?;
    }
    Ok(archive.finish()?.into_inner())
}

/// a file name windows or a fat disk cannot store is pinned and placed here as it is, with one
/// warning for each dataset naming its files that have one, imported, downloaded or unpacked.
#[cfg(unix)]
#[test]
fn names_windows_cannot_store_are_pinned_with_one_warning_per_dataset() -> Result {
    let sandbox = Sandbox::new()?;
    let odd = sandbox.folder("odd")?;
    for name in ["what?.csv", "con.txt", "fine.csv"] {
        fs::write(odd.join(name), b"x\n")?;
    }
    let zip = zipped(&[("pack/star*.csv", b"y\n"), ("pack/fine.csv", b"z\n")])?;
    let server = Server::start("/pack.zip", vec![file(&zip, "\"z\"")])?;
    sandbox.allow(&server.url);
    for (link, warning) in [
        (
            "../odd",
            "warning: odd has 2 files whose names windows and fat disks cannot store: con.txt and what?.csv\n",
        ),
        (
            server.url.as_str(),
            "warning: pack has a file whose name windows and fat disks cannot store: star*.csv\n",
        ),
    ] {
        let added = sandbox.run(&["add", link])?;
        let stderr = text(&added.stderr)?;
        assert_eq!(added.status.code(), Some(0), "{stderr}");
        assert_eq!(stderr.matches("warning:").count(), 1, "{stderr}");
        assert!(stderr.contains(warning), "{stderr}");
    }
    let work = sandbox.work();
    for placed in ["odd/what?.csv", "odd/con.txt", "pack/star*.csv"] {
        assert!(work.join("data").join(placed).is_file(), "{placed}");
    }
    Ok(())
}

/// a members table left under a dataset's old name when only its own table was renamed is told
/// as a rename to finish, not as a dataset without a ref, and once it is renamed too the pin
/// stays without a request.
#[test]
fn members_left_under_an_old_name_ask_to_finish_the_rename() -> Result {
    let sandbox = Sandbox::new()?;
    let zip = zipped(&[("a1.wav", b"1"), ("a2.wav", b"2")])?;
    let server = Server::start("/glob-000000.zip", vec![file(&zip, "\"g\"")])?;
    sandbox.allow(&server.url);
    let added = sandbox.run(&["add", &server.url, "--as", "glob"])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let manifest = sandbox.work().join("data.toml");
    let members = "[datasets.glob.members.\"glob-000000.zip\"]\nexclude = [\"a1.wav\"]\n";
    let table = |name: &str| format!("[datasets.{name}]\nref = \"{}\"\n\n", server.url);
    fs::write(&manifest, format!("{}{members}", table("glob")))?;
    let filtered = sandbox.run(&["sync"])?;
    assert_eq!(
        filtered.status.code(),
        Some(0),
        "{}",
        text(&filtered.stderr)?
    );
    let requests = server.requests().len();
    fs::write(&manifest, format!("{}{members}", table("sounds")))?;
    let status = sandbox.run(&["status"])?;
    let stderr = text(&status.stderr)?;
    assert_eq!(status.status.code(), Some(50), "{stderr}");
    assert!(
        stderr.starts_with("error: datasets.glob in data.toml has members tables but no ref\n"),
        "{stderr}"
    );
    assert!(
        stderr.contains("help: if glob is now sounds, rename its members tables too, as [datasets.sounds.members.\"glob-000000.zip\"]"),
        "{stderr}"
    );
    let renamed = members.replace("glob.members", "sounds.members");
    fs::write(&manifest, format!("{}{renamed}", table("sounds")))?;
    let synced = sandbox.run(&["sync"])?;
    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("Renamed glob to sounds in data.lock · its pin stays"),
        "{stderr}"
    );
    assert_eq!(server.requests().len(), requests);
    assert!(!sandbox.work().join("data/sounds/a1.wav").exists());
    Ok(())
}
