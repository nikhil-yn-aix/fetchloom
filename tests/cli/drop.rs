//! `drop`: a pinned dataset's bytes freed on this machine once each of its files is found
//! elsewhere, left out of a plain sync, and brought back by a sync that names it.

use std::fs;

use serde_json::json;

use crate::mirrors::{IRIS, mirror_of};
use crate::support::{
    Result, Routes, Running, Sandbox, edit, file, json, object, ok, reply, text, wait_until, zip_of,
};

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
    // its source answers at the pinned size, which alone does not show the bytes are the same
    let output = sandbox.run(&["--json", "drop", "iris"])?;
    assert_eq!(output.status.code(), Some(1), "{}", text(&output.stderr)?);
    let error = &json(&output)?["errors"][0];
    assert_eq!(error["kind"], "unconfirmed");
    assert_eq!(
        error["message"],
        "iris stays on this machine, since 1 of its files was checked elsewhere by size alone, which does not show the bytes are the same"
    );
    assert_eq!(
        error["help"],
        "`fetchloom push iris --to <folder>` keeps a copy on a folder mirror, then `fetchloom drop iris --mirror <folder>` frees it; or `fetchloom drop iris --yes` frees it on the sizes alone"
    );
    assert!(placed.exists() && cached.exists());
    let output = sandbox.run(&["--json", "--yes", "drop", "iris", "--dry-run"])?;
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
    let output = sandbox.run(&["--json", "--yes", "drop", "iris"])?;
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
    // a list of the data folder's files leaves it out, and says so
    let exported = sandbox.run(&["export"])?;
    ok(&exported)?;
    assert!(
        !text(&exported.stdout)?.contains("data/iris/"),
        "{}",
        text(&exported.stdout)?
    );
    assert!(
        text(&exported.stderr)?.contains(
            "the list leaves out iris: it is dropped on this machine, so its files are not in the data folder"
        ),
        "{}",
        text(&exported.stderr)?
    );
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

/// adding a dropped dataset's link again brings it back, as `sync <name>` does, and says so.
#[test]
fn an_add_of_a_dropped_dataset_brings_it_back() -> Result {
    let sandbox = Sandbox::new()?;
    let source = iris(&sandbox)?;
    let placed = sandbox.work().join("data/iris/iris.csv");
    ok(&sandbox.run(&["--yes", "drop", "iris"])?)?;
    assert!(!placed.exists());
    let output = sandbox.run(&["add", &format!("{}/files/iris.csv", source.url)])?;
    ok(&output)?;
    let told = text(&output.stderr)?;
    assert!(
        told.contains(
            "Ok iris is already in data.toml and back on this machine, its files are in data/iris"
        ),
        "{told}"
    );
    assert_eq!(fs::read(&placed)?, IRIS);
    let status = json(&sandbox.run(&["--json", "status"])?)?;
    assert_eq!(status["result"]["dropped"], json!([]), "{status}");
    Ok(())
}

/// a drop that cannot read what the cache must keep for other projects, as a record no fetchloom
/// wrote stops it, refuses before it changes anything: the dataset stays placed and undropped.
#[test]
fn a_drop_refused_for_a_cache_record_changes_nothing() -> Result {
    let sandbox = Sandbox::new()?;
    let _source = iris(&sandbox)?;
    let placed = sandbox.work().join("data/iris/iris.csv");
    let projects = sandbox.cache().join("projects");
    fs::create_dir_all(&projects)?;
    fs::write(projects.join("planted"), vec![b'x'; 100 << 10])?;
    let output = sandbox.run(&["--yes", "drop", "iris"])?;
    assert_eq!(output.status.code(), Some(50), "{}", text(&output.stderr)?);
    let told = text(&output.stderr)?;
    assert!(
        told.contains("is larger than any record of a project"),
        "{told}"
    );
    assert!(
        placed.exists(),
        "the refused drop removed {}",
        placed.display()
    );
    assert_eq!(fs::read(&placed)?, IRIS);
    let status = json(&sandbox.run(&["--json", "status"])?)?;
    assert_eq!(status["result"]["dropped"], json!([]), "{status}");
    Ok(())
}

/// a drop killed once its files left the data folder, before it freed the cache, is told as cut
/// short until a drop finishes it, never as freed, since the cache still holds its files.
#[test]
fn a_drop_cut_short_before_it_frees_the_cache_is_never_told_freed() -> Result {
    let sandbox = Sandbox::new()?;
    let _source = iris(&sandbox)?;
    let release = sandbox.folder("hold")?.join("release");
    let mut command = sandbox.command(&["--yes", "drop", "iris"]);
    command.env("FETCHLOOM_TEST_HOLD_DROP", &release);
    let mut running = Running::start(&mut command)?;
    let mut held = release.as_os_str().to_owned();
    held.push(".held");
    wait_until("the drop to hold before it frees the cache", || {
        Ok(std::path::Path::new(&held).exists())
    })?;
    running.kill()?;
    running.wait()?;
    ok(&sandbox.run(&["sync"])?)?;
    let cut = "Dropped iris · a drop cut short before it freed the cache; `fetchloom drop iris` frees what the cache still holds of it";
    let told = text(&sandbox.run(&["status"])?.stderr)?;
    assert!(told.contains(cut), "{told}");
    ok(&sandbox.run(&["--yes", "drop", "iris"])?)?;
    let told = text(&sandbox.run(&["status"])?.stderr)?;
    assert!(
        told.contains("Dropped iris · freed on this machine"),
        "{told}"
    );
    assert!(!told.contains(cut), "{told}");
    Ok(())
}

/// a line of a list naming a dropped dataset brings it back too, as the same link typed does.
#[test]
fn a_list_line_naming_a_dropped_dataset_brings_it_back() -> Result {
    let sandbox = Sandbox::new()?;
    let source = iris(&sandbox)?;
    let placed = sandbox.work().join("data/iris/iris.csv");
    ok(&sandbox.run(&["--yes", "drop", "iris"])?)?;
    assert!(!placed.exists());
    fs::write(
        sandbox.work().join("links.txt"),
        format!("{}/files/iris.csv\n", source.url),
    )?;
    let output = sandbox.run(&["add", "-i", "links.txt"])?;
    ok(&output)?;
    let told = text(&output.stderr)?;
    assert!(
        told.contains("Ok iris is already in data.toml and back on this machine"),
        "{told}"
    );
    assert_eq!(fs::read(&placed)?, IRIS);
    let status = json(&sandbox.run(&["--json", "status"])?)?;
    assert_eq!(status["result"]["dropped"], json!([]), "{status}");
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
    assert_eq!(output.status.code(), Some(1), "{}", text(&output.stderr)?);
    let error = &json(&output)?["errors"][0];
    assert_eq!(error["kind"], "data.not_ready");
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
    let output = sandbox.run(&["--yes", "drop", "iris"])?;
    ok(&output)?;
    let told = text(&output.stderr)?;
    assert!(
        told.contains(
            "warning: data/iris/iris.csv has changed since fetchloom placed it, so drop keeps it\n"
        ),
        "{told}"
    );
    // the dropped dataset's own files are counted once, in its line, as nothing else needs them
    assert!(!told.contains("no dataset needs"), "{told}");
    assert!(told.contains("Dropped iris · freed "), "{told}");
    assert_eq!(fs::read(&placed)?, b"my notes\n");
    Ok(())
}

#[test]
fn gc_frees_what_only_a_dropped_dataset_pins() -> Result {
    let sandbox = Sandbox::new()?;
    let _source = iris(&sandbox)?;
    ok(&sandbox.run(&["--yes", "drop", "iris"])?)?;
    // its bytes back in the cache are needed no more
    let cached = object(&sandbox.cache(), IRIS);
    fs::create_dir_all(cached.parent().ok_or("an object in no folder")?)?;
    fs::write(&cached, IRIS)?;
    let output = sandbox.run(&["--json", "cache", "gc", "--now"])?;
    ok(&output)?;
    assert_eq!(json(&output)?["result"]["removed_objects"], 1);
    assert!(!cached.exists());
    Ok(())
}

/// a source that gives no answer leaves a file not checked: the dataset stays, with the exit a
/// download would give, not as if the file were lost.
#[test]
fn a_source_that_gives_no_answer_keeps_the_dataset_as_not_checked() -> Result {
    let sandbox = Sandbox::new()?;
    let source = iris(&sandbox)?;
    drop(source);
    let output = sandbox
        .command(&["--json", "--yes", "drop", "iris"])
        .env("FETCHLOOM_RETRIES", "0")
        .output()?;
    assert_eq!(output.status.code(), Some(20), "{}", text(&output.stderr)?);
    let error = &json(&output)?["errors"][0];
    assert_eq!(
        error["message"],
        "iris stays on this machine, since 1 of its files could not be checked elsewhere"
    );
    assert!(sandbox.work().join("data/iris/iris.csv").exists());
    Ok(())
}

/// in a copy of the project, whose state is not used, drop takes the files that hold the pinned
/// bytes as placed, as status counts them in place, and frees them, so status then agrees; one with
/// other bytes stays, and drop says why.
#[test]
fn a_drop_in_a_copied_project_frees_the_files_in_place() -> Result {
    let sandbox = Sandbox::new()?;
    let _source = iris(&sandbox)?;
    let (two, three) = (sandbox.folder("two")?, sandbox.folder("three")?);
    for copy in [&two, &three] {
        crate::copied::copy_tree(&sandbox.work(), copy)?;
    }
    let output = sandbox
        .command_in(&two, &["--json", "--yes", "drop", "iris"])
        .output()?;
    ok(&output)?;
    let result = &json(&output)?["result"];
    assert_eq!(result["files"], 1, "{result}");
    assert!(!two.join("data/iris/iris.csv").exists());
    let status = sandbox.command_in(&two, &["status"]).output()?;
    ok(&status)?;
    assert_eq!(text(&status.stdout)?, "");
    assert!(
        text(&status.stderr)?.contains("Dropped iris · freed on this machine"),
        "{}",
        text(&status.stderr)?
    );
    let other = three.join("data/iris/iris.csv");
    edit(&other, b"my notes\n")?;
    let output = sandbox
        .command_in(&three, &["--yes", "drop", "iris"])
        .output()?;
    ok(&output)?;
    let told = text(&output.stderr)?;
    assert!(
        told.contains("warning: data/iris/iris.csv holds other bytes than data.lock pins and fetchloom did not place it, so drop keeps it\n"),
        "{told}"
    );
    assert_eq!(fs::read(&other)?, b"my notes\n");
    // on a machine whose cache never held them, the data folder's files are still this machine's
    let (four, cache) = (sandbox.folder("four")?, sandbox.folder("four cache")?);
    crate::copied::copy_tree(&sandbox.work(), &four)?;
    let output = sandbox
        .command_in(&four, &["--json", "--yes", "drop", "iris"])
        .env("FETCHLOOM_CACHE", &cache)
        .output()?;
    ok(&output)?;
    assert_eq!(json(&output)?["result"]["files"], 1);
    assert!(!four.join("data/iris/iris.csv").exists());
    Ok(())
}

/// a drop that could not finish, cut short as a kill or a file that will not go cuts it, leaves its
/// dataset dropped with files in the data folder: status says so in the dataset's line, with the
/// drop that finishes it, and the sync that removes them says why.
#[test]
fn a_drop_cut_short_is_told_as_one() -> Result {
    let sandbox = Sandbox::new()?;
    let _source = iris(&sandbox)?;
    let folder = sandbox.work().join("data").join("iris");
    #[cfg(windows)]
    let holder = {
        use std::os::windows::fs::OpenOptionsExt as _;
        fs::OpenOptions::new()
            .read(true)
            .share_mode(3)
            .open(folder.join("iris.csv"))?
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if rustix::process::geteuid().is_root() {
            return crate::support::skipped("unprivileged", "root ignores file permissions");
        }
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o555))?;
    }
    let output = sandbox.run(&["--yes", "drop", "iris"])?;
    #[cfg(windows)]
    drop(holder);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o755))?;
    }
    assert_ne!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    let status = sandbox.run(&["status"])?;
    ok(&status)?;
    assert_eq!(
        text(&status.stdout)?,
        "    Dropped iris · a drop cut short left its file in data/iris; `fetchloom drop iris` finishes it\n"
    );
    let synced = sandbox.run(&["sync"])?;
    ok(&synced)?;
    let told = text(&synced.stderr)?;
    assert!(
        told.contains("Retired data/iris · 1 file a drop cut short left\n"),
        "{told}"
    );
    assert!(
        told.contains("`fetchloom drop iris` frees what the cache still holds of it"),
        "{told}"
    );
    Ok(())
}

/// an archive's dataset drops through the folder mirror a push filled: push keeps what the archive
/// unpacks to, its list of files and each file, which a sync then brings back without the archive.
#[test]
fn an_archive_drops_through_the_folder_mirror_a_push_filled() -> Result {
    let sandbox = Sandbox::new()?;
    let zip = zip_of(&[("rows/a.csv", b"a\n"), ("rows/b.csv", b"b\n")])?;
    let source = Routes::start(vec![("/files/rows.zip".to_owned(), file(&zip, "\"z\""))])?;
    sandbox.allow(&source.url);
    ok(&sandbox.run(&["add", &format!("{}/files/rows.zip", source.url)])?)?;
    let root = sandbox.folder("mirror")?;
    ok(&sandbox
        .command(&["--yes", "push", "rows", "--to"])
        .arg(&root)
        .output()?)?;
    let mirrored = |args: &[&str]| sandbox.command(args).arg("--mirror").arg(&root).output();
    let output = mirrored(&["--json", "drop", "rows"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
    assert_eq!(json(&output)?["result"]["dropped"], json!(["rows"]));
    let placed = sandbox.work().join("data/rows/a.csv");
    assert!(!placed.exists());
    ok(&mirrored(&["--offline", "sync", "rows"])?)?;
    assert_eq!(fs::read(&placed)?, b"a\n");
    Ok(())
}

/// a dataset dropped before whose bytes came back into the cache, as a build or a push brings
/// them, is freed again rather than called dropped already.
#[test]
fn a_dropped_dataset_whose_bytes_came_back_is_freed_again() -> Result {
    let sandbox = Sandbox::new()?;
    let _source = iris(&sandbox)?;
    ok(&sandbox.run(&["--yes", "drop", "iris"])?)?;
    let cached = object(&sandbox.cache(), IRIS);
    assert!(!cached.exists());
    if let Some(fan) = cached.parent() {
        fs::create_dir_all(fan)?;
    }
    fs::write(&cached, IRIS)?;
    let output = sandbox.run(&["--json", "--yes", "drop", "iris"])?;
    ok(&output)?;
    let result = &json(&output)?["result"];
    assert_eq!(result["dropped"], json!(["iris"]), "{result}");
    assert_eq!(result["objects"], 1, "{result}");
    assert!(!cached.exists());
    Ok(())
}

/// an add leaves a dataset dropped here dropped, as a plain sync does.
#[test]
fn an_add_leaves_a_dropped_dataset_dropped() -> Result {
    let sandbox = Sandbox::new()?;
    let source = Routes::start(vec![
        ("/files/iris.csv".to_owned(), file(IRIS, "\"i\"")),
        ("/files/pets.csv".to_owned(), file(b"name\nrex\n", "\"p\"")),
    ])?;
    sandbox.allow(&source.url);
    ok(&sandbox.run(&["add", &format!("{}/files/iris.csv", source.url)])?)?;
    ok(&sandbox.run(&["--yes", "drop", "iris"])?)?;
    let iris = |requests: Vec<String>| {
        requests
            .iter()
            .filter(|head| head.contains("/files/iris.csv"))
            .count()
    };
    let asked = iris(source.requests());
    ok(&sandbox.run(&["add", &format!("{}/files/pets.csv", source.url)])?)?;
    assert!(sandbox.work().join("data/pets/pets.csv").exists());
    assert!(!sandbox.work().join("data/iris").exists());
    assert_eq!(iris(source.requests()), asked, "the add fetched it");
    let status = json(&sandbox.run(&["--json", "status"])?)?;
    assert_eq!(status["result"]["dropped"], json!(["iris"]));
    Ok(())
}
