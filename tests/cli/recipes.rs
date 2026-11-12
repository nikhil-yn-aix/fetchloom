//! recipes as every command but build sees them, against servers on this machine: what sync,
//! status and path do in each state of a pinned result, a dataset with `place = false` that a
//! recipe reads, and what `cache` and `cache gc` keep.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::json;

use crate::build::{build, building_in, runs, with};
use crate::project::{IRIS, PATH};
use crate::support::{Result, Sandbox, Server, delete, file, json, sha256_hex, text, zip_of};

/// a recipe that makes `clean` from iris: the file clean.csv, holding `done`.
pub(crate) const WRITE: &str = "\n[datasets.clean]\nfrom = [\"iris\"]\nrun = [\"recipe-step\", \"write\", \"clean.csv\", \"done\", \"{iris}\", \"{out}\"]\n";

/// [`WRITE`], with clean.py as its code.
const WRITE_CODE: &str = "\n[datasets.clean]\nfrom = [\"iris\"]\nrun = [\"recipe-step\", \"write\", \"clean.csv\", \"done\", \"{iris}\", \"{out}\"]\ncode = [\"clean.py\"]\n";

/// sets `place = false` on the dataset `name` in the data.toml of the project at `folder`.
pub(crate) fn unplace(folder: &Path, name: &str) -> Result {
    let manifest = folder.join("data.toml");
    let table = format!("[datasets.{name}]\n");
    let text = fs::read_to_string(&manifest)?;
    assert!(text.contains(&table), "{text}");
    fs::write(
        &manifest,
        text.replace(&table, &format!("{table}place = false\n")),
    )?;
    Ok(())
}

/// a fresh clone of the work folder's project in a folder called `name`, data.lock.d and all,
/// and an empty cache of its own beside it.
fn cloned(sandbox: &Sandbox, name: &str) -> Result<(PathBuf, PathBuf)> {
    let folder = crate::project::clone(sandbox, name)?;
    let parts = folder.join("data.lock.d");
    fs::create_dir_all(&parts)?;
    for entry in fs::read_dir(sandbox.work().join("data.lock.d"))? {
        let entry = entry?;
        fs::copy(entry.path(), parts.join(entry.file_name()))?;
    }
    Ok((folder, sandbox.folder(&format!("{name}-cache"))?))
}

/// how many of the warnings in the `--json` document `output` printed are `warning`.
fn warned(output: &std::process::Output, warning: &str) -> Result<usize> {
    Ok(crate::support::warned(&json(output)?)
        .iter()
        .filter(|told| *told == warning)
        .count())
}

/// the cached object holding `bytes`, in the cache at `cache`.
pub(crate) fn object(cache: &Path, bytes: &[u8]) -> PathBuf {
    let hex = sha256_hex(bytes);
    cache.join("objects").join(&hex[..2]).join(&hex[2..])
}

/// a recipe data.lock pins nothing of has nothing a sync could place, so a sync of every
/// dataset says so, while a sync that names it fails as `path` does.
#[test]
fn sync_warns_of_a_recipe_not_built_yet() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    with(&sandbox, &server, WRITE)?;
    let output = sandbox.run(&["--json", "sync"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
    let document = json(&output)?;
    assert_eq!(
        crate::support::warned(&document),
        [
            "clean is not built yet; run `fetchloom build clean` to make it, then commit data.lock and data.lock.d"
        ]
    );
    assert_eq!(document["warnings"][0]["kind"], "build.unbuilt");
    let named = sandbox.run(&["--json", "sync", "clean"])?;
    assert_eq!(named.status.code(), Some(60), "{}", text(&named.stdout)?);
    let error = &json(&named)?["errors"][0];
    assert_eq!(error["kind"], "build.needed");
    assert_eq!(
        error["message"],
        "data.lock is out of date: clean is not built yet"
    );
    assert_eq!(error["fix"], serde_json::Value::Null);
    Ok(())
}

/// a strict sync that fails, here for a recipe not built yet, never first says that everything
/// is in place: its error says what is not ready. once the data is ready, it says so again.
#[test]
fn a_strict_sync_that_fails_says_no_ok_first() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    with(&sandbox, &server, WRITE)?;
    let said_ok = |stderr: &str| {
        stderr
            .lines()
            .any(|line| line.trim_start().starts_with("Ok "))
    };
    let failed = sandbox.run(&["sync", "--strict"])?;
    let stderr = text(&failed.stderr)?;
    assert_eq!(failed.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("error: the data is not ready to use\n"),
        "{stderr}"
    );
    assert!(!said_ok(&stderr), "{stderr}");
    let built = build(&sandbox, &["build"])?;
    assert_eq!(built.status.code(), Some(0), "{}", text(&built.stderr)?);
    let ready = sandbox.run(&["sync", "--strict"])?;
    let stderr = text(&ready.stderr)?;
    assert_eq!(ready.status.code(), Some(0), "{stderr}");
    assert!(said_ok(&stderr), "{stderr}");
    Ok(())
}

/// a recipe whose code changed since its build keeps the result data.lock pins: sync and path
/// place that result and say what changed and which build makes it again, once each.
#[test]
fn sync_and_path_place_the_pin_of_a_changed_recipe_and_warn() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    with(&sandbox, &server, WRITE_CODE)?;
    let code = sandbox.work().join("clean.py");
    fs::write(&code, "print('before')\n")?;
    let built = build(&sandbox, &["build"])?;
    assert_eq!(built.status.code(), Some(0), "{}", text(&built.stderr)?);
    fs::write(&code, "print('after')\n")?;
    let warned_text = "clean changed since it was built: clean.py changed; data.lock still pins the result built before; run `fetchloom build clean`, then commit data.lock and data.lock.d";
    let placed = sandbox.work().join("data").join("clean").join("clean.csv");
    for args in [
        &["--json", "sync"][..],
        &["--json", "sync", "clean"],
        &["--json", "path", "clean"],
    ] {
        let output = sandbox.run(args)?;
        assert_eq!(
            output.status.code(),
            Some(0),
            "{args:?}: {}",
            text(&output.stdout)?
        );
        assert_eq!(
            crate::support::warned(&json(&output)?),
            [warned_text],
            "{args:?}"
        );
    }
    delete(&placed)?;
    let output = sandbox.run(&["--json", "path", "clean"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
    assert_eq!(
        warned(&output, warned_text)?,
        1,
        "{}",
        text(&output.stdout)?
    );
    assert_eq!(fs::read(&placed)?, b"done");
    let told = text(&sandbox.run(&["sync"])?.stderr)?;
    assert!(
        told.contains("warning: clean changed since it was built: clean.py changed\n"),
        "{told}"
    );
    assert_eq!(runs(&sandbox.work())?, 1, "only the build ran the step");
    Ok(())
}

/// a recipe renamed in data.toml, the rest of its table as it was, keeps its pinned result: its
/// key never held its name, so relocking carries the result over and nothing needs a build. a
/// build then runs nothing, and a locked sync places the result in the new name's folder.
#[test]
fn a_renamed_recipe_keeps_its_result() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    with(&sandbox, &server, WRITE)?;
    let built = build(&sandbox, &["build"])?;
    assert_eq!(built.status.code(), Some(0), "{}", text(&built.stderr)?);
    let manifest = sandbox.work().join("data.toml");
    let before = fs::read_to_string(&manifest)?;
    fs::write(
        &manifest,
        before.replace("[datasets.clean]\n", "[datasets.tidy]\n"),
    )?;

    let locked = sandbox.run(&["lock"])?;
    let stderr = text(&locked.stderr)?;
    assert_eq!(locked.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("Renamed clean to tidy in data.lock · its pin stays\n"),
        "{stderr}"
    );
    assert_eq!(sandbox.run(&["lock", "--check"])?.status.code(), Some(0));
    let again = build(&sandbox, &["build"])?;
    assert_eq!(again.status.code(), Some(0), "{}", text(&again.stderr)?);
    let synced = sandbox.run(&["sync", "--locked"])?;
    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    assert_eq!(
        fs::read(sandbox.work().join("data/tidy/clean.csv"))?,
        b"done"
    );
    assert_eq!(
        runs(&sandbox.work())?,
        1,
        "only the first build ran the step"
    );
    Ok(())
}

/// `lock` keeps a changed recipe's pin, since only a build pins one, so it still succeeds: it
/// says what moved as sync does, with the build and commit that bring data.lock up to date, and
/// its last line never calls data.lock up to date, dry run or not.
#[test]
fn lock_tells_a_changed_recipe_and_claims_nothing_is_up_to_date() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    with(&sandbox, &server, WRITE_CODE)?;
    let code = sandbox.work().join("clean.py");
    fs::write(&code, "print('before')\n")?;
    let built = build(&sandbox, &["build"])?;
    assert_eq!(built.status.code(), Some(0), "{}", text(&built.stderr)?);
    fs::write(&code, "print('after')\n")?;
    let pins = fs::read(sandbox.work().join("data.lock"))?;

    let locked = sandbox.run(&["lock"])?;
    assert_eq!(locked.status.code(), Some(0), "{}", text(&locked.stderr)?);
    let stderr = text(&locked.stderr)?;
    sandbox.filters().bind(|| insta::assert_snapshot!(stderr));
    assert_eq!(fs::read(sandbox.work().join("data.lock"))?, pins);
    let dry = text(&sandbox.run(&["lock", "--dry-run"])?.stderr)?;
    assert!(
        dry.contains("warning: clean changed since it was built: clean.py changed\n")
            && !dry.contains("up to date"),
        "{dry}"
    );
    Ok(())
}

/// a sync or path of a recipe alone relocks and fetches nothing of what it is made from: it
/// reads the inputs' pins and entries to tell that one changed in data.toml, and places the
/// result from the cache.
#[test]
fn a_named_sync_reads_its_inputs_and_fetches_none_of_them() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    with(&sandbox, &server, WRITE)?;
    let built = build(&sandbox, &["build"])?;
    assert_eq!(built.status.code(), Some(0), "{}", text(&built.stderr)?);
    delete(&object(&sandbox.cache(), IRIS))?;
    let manifest = sandbox.work().join("data.toml");
    let changed = fs::read_to_string(&manifest)?.replacen(
        "[datasets.iris]\n",
        "[datasets.iris]\nselect = \"*.csv\"\n",
        1,
    );
    fs::write(&manifest, changed)?;
    let lock = fs::read(sandbox.work().join("data.lock"))?;
    let placed = sandbox.work().join("data").join("clean").join("clean.csv");
    delete(&placed)?;
    let warned_text = "clean changed since it was built: iris changed in data.toml; data.lock still pins the result built before; run `fetchloom build clean`, then commit data.lock and data.lock.d";
    for args in [
        &["--json", "sync", "clean"][..],
        &["--json", "path", "clean"],
    ] {
        let output = sandbox.run(args)?;
        assert_eq!(
            output.status.code(),
            Some(0),
            "{args:?}: {}",
            text(&output.stdout)?
        );
        assert_eq!(warned(&output, warned_text)?, 1, "{args:?}");
    }
    assert_eq!(fs::read(&placed)?, b"done");
    assert_eq!(server.requests().len(), 1, "only the add asked the source");
    assert_eq!(fs::read(sandbox.work().join("data.lock"))?, lock);
    Ok(())
}

/// a clone whose cache lacks a pinned result that no mirror holds has nothing sync can place for
/// it, so status names the build that makes it rather than the result's files, and names the
/// input the build would fetch first.
#[test]
fn status_lists_a_result_no_cache_or_mirror_holds_as_unbuilt() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), file(IRIS, "\"v1\"")])?;
    with(&sandbox, &server, WRITE)?;
    let built = build(&sandbox, &["build"])?;
    assert_eq!(built.status.code(), Some(0), "{}", text(&built.stderr)?);
    let (folder, cache) = cloned(&sandbox, "clone")?;
    let there = |args: &[&str]| -> Result<std::process::Output> {
        Ok(sandbox
            .command_in(&folder, args)
            .env("FETCHLOOM_CACHE", &cache)
            .output()?)
    };
    let document = json(&there(&["--json", "status"])?)?;
    assert_eq!(document["result"]["unbuilt"], json!(["clean"]));
    assert_eq!(document["result"]["missing"], json!(["data/iris/iris.csv"]));
    let status = there(&["status"])?;
    let told = text(&status.stdout)?;
    assert!(
        told.contains("    Unbuilt clean · its pinned result is not on this machine or a mirror, nor is iris, which it is made from\n"),
        "{told}"
    );
    let hints = text(&status.stderr)?;
    assert!(
        hints.contains("hint: `fetchloom build clean` makes it on this machine\n"),
        "{hints}"
    );
    assert!(!told.contains("Missing data/clean"), "{told}");
    let synced = there(&["sync"])?;
    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    let status = there(&["status"])?;
    let told = text(&status.stdout)?;
    assert!(
        told.contains("    Unbuilt clean · its pinned result is not on this machine or a mirror\n"),
        "{told}"
    );
    let hints = text(&status.stderr)?;
    assert!(!hints.contains("`fetchloom sync`"), "{hints}");
    Ok(())
}

/// a project copied without `.fetchloom`, its data folder holding iris but not clean's result,
/// to a machine whose cache is empty: status names clean's build alone, since the build copies
/// iris from the data folder, as it does then with no request.
#[test]
fn status_counts_an_input_the_build_copies_from_the_data_folder() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    with(&sandbox, &server, WRITE)?;
    let built = build(&sandbox, &["build"])?;
    assert_eq!(built.status.code(), Some(0), "{}", text(&built.stderr)?);
    let (folder, cache) = cloned(&sandbox, "copied")?;
    let iris = Path::new("data").join("iris").join("iris.csv");
    fs::create_dir_all(folder.join("data").join("iris"))?;
    fs::copy(sandbox.work().join(&iris), folder.join(&iris))?;
    let told = text(
        &sandbox
            .command_in(&folder, &["status"])
            .env("FETCHLOOM_CACHE", &cache)
            .output()?
            .stdout,
    )?;
    assert!(
        told.contains("    Unbuilt clean · its pinned result is not on this machine or a mirror\n"),
        "{told}"
    );
    let output = building_in(&sandbox, &folder, &["--offline", "--json", "build"])?
        .env("FETCHLOOM_CACHE", &cache)
        .output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
    assert_eq!(json(&output)?["work"]["data_folder_objects"], 1);
    assert_eq!(server.requests().len(), 1, "only the add asked the source");
    Ok(())
}

/// a file sync puts back says how to leave it out for good as drop would: a file of a recipe's
/// result in the recipe's step, and the only file of a dataset by removing the dataset.
#[test]
fn a_put_back_file_names_what_leaves_it_out() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    with(&sandbox, &server, WRITE)?;
    let built = build(&sandbox, &["build"])?;
    assert_eq!(built.status.code(), Some(0), "{}", text(&built.stderr)?);
    for (path, help) in [
        (
            "data/clean/clean.csv",
            "clean makes data/clean/clean.csv, so to leave it out for good, choose the files inside clean's step",
        ),
        (
            "data/iris/iris.csv",
            "data/iris/iris.csv is the only file of iris, so to leave it out for good, run `fetchloom remove iris`",
        ),
    ] {
        delete(&sandbox.work().join(path))?;
        let synced = sandbox.run(&["sync"])?;
        assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
        let told = text(&synced.stderr)?;
        let warning = format!(
            "warning: put back {path}, which was deleted or moved, since data.lock pins it\n  help: {help}; keep files of your own in a folder outside data/\n"
        );
        assert!(told.contains(&warning), "{warning:?} in\n{told}");
    }
    Ok(())
}

/// raw data kept out of the data folder, an archive with a file a members table leaves out, and
/// a recipe that reads it. `cache gc --now` keeps the files the recipe reads and frees the
/// archive and the file left out. with the result gone from the data folder and the cache too,
/// as after a sync and a gc while another commit was checked out, status names only the build,
/// since the cache holds all the build reads, and the build then needs no network.
#[test]
fn status_counts_an_archive_whose_kept_files_the_cache_holds() -> Result {
    let sandbox = Sandbox::new()?;
    let zipped = zip_of(&[
        ("set/a.csv", &b"a,1\n"[..]),
        ("set/b.csv", b"b,2\n"),
        ("set/labels.csv", b"file,label\n"),
    ])?;
    let server = Server::start("/files/set.zip", vec![file(&zipped, "\"z1\"")])?;
    sandbox.allow(&server.url);
    let added = sandbox.run(&["add", &server.url, "--as", "set"])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let work = sandbox.work();
    unplace(&work, "set")?;
    let manifest = work.join("data.toml");
    fs::write(
        &manifest,
        format!(
            "{}\n[datasets.set.members.\"set.zip\"]\nexclude = [\"set/b.csv\"]\n\n[datasets.made]\nfrom = [\"set\"]\nrun = [\"recipe-step\", \"write\", \"made.csv\", \"made\", \"{{set}}\", \"{{out}}\"]\n",
            fs::read_to_string(&manifest)?
        ),
    )?;
    let built = build(&sandbox, &["build"])?;
    assert_eq!(built.status.code(), Some(0), "{}", text(&built.stderr)?);
    let collected = sandbox.run(&["cache", "gc", "--now"])?;
    assert_eq!(
        collected.status.code(),
        Some(0),
        "{}",
        text(&collected.stderr)?
    );
    let cache = sandbox.cache();
    for (bytes, kept) in [
        (&zipped[..], false),
        (b"b,2\n", false),
        (b"a,1\n", true),
        (b"file,label\n", true),
    ] {
        assert_eq!(object(&cache, bytes).exists(), kept, "{bytes:?}");
    }
    // a sync on the other commit retired the result, which gc then freed
    delete(&work.join("data").join("made").join("made.csv"))?;
    delete(&object(&cache, b"made"))?;
    let told = text(&sandbox.run(&["status"])?.stdout)?;
    assert!(
        told.contains("    Unbuilt made · its pinned result is not on this machine or a mirror\n"),
        "{told}"
    );
    let offline = build(&sandbox, &["--offline", "build"])?;
    assert_eq!(offline.status.code(), Some(0), "{}", text(&offline.stderr)?);
    assert_eq!(server.requests().len(), 1, "only the add asked the source");
    Ok(())
}

/// a clone whose cache lacks a pinned result gets it from a folder mirror that holds it, with
/// no request to the source and no step run, and status calls its files missing until then.
#[test]
fn a_result_only_a_folder_mirror_holds_is_synced_from_it() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    with(&sandbox, &server, WRITE)?;
    let built = build(&sandbox, &["build"])?;
    assert_eq!(built.status.code(), Some(0), "{}", text(&built.stderr)?);
    let mirror = sandbox.folder("mirror")?;
    fs::write(mirror.join("fetchloom-mirror"), "fetchloom mirror 1\n")?;
    for bytes in [IRIS, b"done".as_slice()] {
        let hex = sha256_hex(bytes);
        let fan = mirror.join("objects").join(&hex[..2]);
        fs::create_dir_all(&fan)?;
        fs::write(fan.join(&hex[2..]), bytes)?;
    }
    let (folder, cache) = cloned(&sandbox, "clone")?;
    let there = |args: &[&str]| -> Result<std::process::Output> {
        Ok(sandbox
            .command_in(&folder, args)
            .env("FETCHLOOM_CACHE", &cache)
            .env("FETCHLOOM_MIRRORS", &mirror)
            .output()?)
    };
    let document = json(&there(&["--json", "status"])?)?;
    assert_eq!(document["result"]["unbuilt"], json!([]));
    assert_eq!(
        document["result"]["missing"],
        json!(["data/iris/iris.csv", "data/clean/clean.csv"])
    );
    let output = there(&["--json", "sync", "--locked"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
    let document = json(&output)?;
    assert_eq!(document["work"]["mirror_objects"], 2);
    assert_eq!(document["result"]["placement"]["unbuilt"], json!([]));
    assert_eq!(
        fs::read(folder.join("data").join("clean").join("clean.csv"))?,
        b"done"
    );
    assert_eq!(server.requests().len(), 1, "only the add asked the source");
    assert_eq!(runs(&folder)?, 0, "sync never runs a step");
    Ok(())
}

/// a build of a recipe made from a dataset with `place = false` places only the result and
/// retires what was placed of the input; a clone whose cache lacks both fetches the input for
/// its step without placing it.
#[test]
fn a_build_places_only_the_result_of_an_unplaced_input() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), file(IRIS, "\"v1\"")])?;
    with(&sandbox, &server, WRITE)?;
    unplace(&sandbox.work(), "iris")?;
    let built = build(&sandbox, &["build"])?;
    assert_eq!(built.status.code(), Some(0), "{}", text(&built.stderr)?);
    let told = text(&built.stderr)?;
    assert!(
        told.contains("    Retired data/iris · 1 file, since iris has place = false\n"),
        "{told}"
    );
    let data = sandbox.work().join("data");
    assert!(!data.join("iris").exists());
    assert_eq!(fs::read(data.join("clean").join("clean.csv"))?, b"done");
    let status = text(&sandbox.run(&["status"])?.stderr)?;
    assert!(
        status.contains(" Not placed iris · place = false in data.toml\n"),
        "{status}"
    );
    assert!(
        status.contains("Ok everything matches data.lock by size and time\n"),
        "{status}"
    );

    let (folder, cache) = cloned(&sandbox, "clone")?;
    let output = building_in(&sandbox, &folder, &["build"])?
        .env("FETCHLOOM_CACHE", &cache)
        .output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    assert_eq!(
        server.requests().len(),
        2,
        "the clone fetched iris for its step"
    );
    assert!(!folder.join("data").join("iris").exists());
    assert_eq!(
        fs::read(folder.join("data").join("clean").join("clean.csv"))?,
        b"done"
    );
    Ok(())
}

/// the cache report counts what only builds read, and gc keeps it with the results projects
/// pin, while it frees the record of a result no project pins any more.
#[test]
fn gc_keeps_what_builds_read_and_frees_records_no_project_pins() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    with(&sandbox, &server, WRITE_CODE)?;
    unplace(&sandbox.work(), "iris")?;
    let code = sandbox.work().join("clean.py");
    fs::write(&code, "print('first')\n")?;
    assert_eq!(build(&sandbox, &["build"])?.status.code(), Some(0));
    let report = json(&sandbox.run(&["--json", "cache"])?)?;
    assert_eq!(report["result"]["inputs_only"], IRIS.len());
    let told = text(&sandbox.run(&["cache"])?.stderr)?;
    assert!(
        told.contains(&format!(
            "     Needed {} B by 1 project · {} B of it is read only by builds",
            IRIS.len() + 4,
            IRIS.len()
        )),
        "{told}"
    );
    let builds = sandbox.cache().join("builds");
    let first: Vec<PathBuf> = fs::read_dir(&builds)?
        .map(|entry| Ok(entry?.path()))
        .collect::<Result<_>>()?;
    fs::write(&code, "print('second')\n")?;
    assert_eq!(build(&sandbox, &["build"])?.status.code(), Some(0));
    assert_eq!(fs::read_dir(&builds)?.count(), 2);
    let gc = sandbox.run(&["cache", "gc", "--now"])?;
    assert_eq!(gc.status.code(), Some(0), "{}", text(&gc.stderr)?);
    let left: Vec<PathBuf> = fs::read_dir(&builds)?
        .map(|entry| Ok(entry?.path()))
        .collect::<Result<_>>()?;
    assert_eq!(left.len(), 1);
    assert!(!first.contains(&left[0]), "the first build's key is gone");
    assert_eq!(
        fs::read_dir(&left[0])?.count(),
        2,
        "its record and its lock"
    );
    assert!(object(&sandbox.cache(), IRIS).is_file(), "the step's input");
    assert!(object(&sandbox.cache(), b"done").is_file(), "the result");
    Ok(())
}
