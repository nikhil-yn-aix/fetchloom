//! a project's inputs: dataset names, members tables, file names, and `cache gc --inputs`, which
//! frees raw data only builds read once it checks that a folder mirror holds a copy.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::build::{build, building_in, with};
use crate::project::{IRIS, PATH};
use crate::recipes::{WRITE, object, unplace};
use crate::support::{Result, Sandbox, Server, edit, file, json, text, zip_of};

/// a project with iris from `server` set to `place = false` and [`WRITE`] built from it, pushed
/// with its inputs into a folder mirror, whose folder this returns.
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

/// the result stays in the cache, and the run names the mirror it relied on.
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

/// the dry run still reads the copies, and a copy that does not hash to its name keeps the file,
/// with the mirror it is on.
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

/// a script gets `unconfirmed`, with what a yes would free and no fix, as only a person may agree.
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

/// the question at a terminal shows what gc would free.
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

/// the project still syncs and builds offline, while a check that runs clean again needs the
/// mirror, without which it has nowhere to turn.
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

/// a dataset's name is its folder on every machine, so neither data.toml nor a cloned data.lock
/// may give a device name, while the name add makes from a file called that works.
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
    // a device name keeps every rule for its letters, so only another name helps
    for device in ["con", "lpt1"] {
        let named = sandbox.run(&["add", &server.url, "--as", device])?;
        let stderr = text(&named.stderr)?;
        assert_eq!(named.status.code(), Some(2), "{stderr}");
        assert!(
            stderr.ends_with(&format!(
                "error: \"{device}\" is not a valid dataset name\n  windows keeps it for a device, so it cannot be a folder there\n  help: pick another name, such as --as {device}-data\n"
            )),
            "{stderr}"
        );
    }
    assert_eq!(server.requests().len(), 0);
    assert!(!manifest.exists());
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

/// the files are placed here as they are, and each warning names them, imported or unpacked alike.
#[cfg(unix)]
#[test]
fn names_windows_cannot_store_are_pinned_with_one_warning_per_dataset() -> Result {
    let sandbox = Sandbox::new()?;
    let odd = sandbox.folder("odd")?;
    for name in ["what?.csv", "con.txt", "fine.csv"] {
        fs::write(odd.join(name), b"x\n")?;
    }
    let zip = zip_of(&[("pack/star*.csv", b"y\n"), ("pack/fine.csv", b"z\n")])?;
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

/// once the members table is renamed too, the pin stays without a request.
#[test]
fn members_left_under_an_old_name_ask_to_finish_the_rename() -> Result {
    let sandbox = Sandbox::new()?;
    let zip = zip_of(&[("a1.wav", b"1"), ("a2.wav", b"2")])?;
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

/// the error names the path too, and the help offers a dataset for each or keeping them packed.
#[test]
fn a_clash_between_two_archives_names_both() -> Result {
    let sandbox = Sandbox::new()?;
    let fold = sandbox.folder("fold")?;
    for (shard, bytes) in [("fold-000000.zip", b"0\n"), ("fold-000001.zip", b"1\n")] {
        fs::write(fold.join(shard), zip_of(&[("train/0000.txt", bytes)])?)?;
    }
    let output = sandbox.run(&["add", "../fold"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(50), "{stderr}");
    assert!(
        stderr.contains("error: two files would land on data/fold/train/0000.txt: one from fold-000000.zip and one from fold-000001.zip\n"),
        "{stderr}"
    );
    assert!(
        stderr.contains("help: pass --no-extract to keep archives packed, or give each archive a dataset of its own, whose folder keeps its files apart"),
        "{stderr}"
    );
    assert!(!sandbox.work().join("data.toml").exists());
    Ok(())
}
