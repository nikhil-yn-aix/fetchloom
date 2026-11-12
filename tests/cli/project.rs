//! the project loop: sync, lock and status, and what spans several commands, against servers on
//! this machine.

use std::fs;

use crate::export::sha256;
#[cfg(unix)]
use crate::support::wait_until;
use crate::support::{Result, Sandbox, Server, delete, edit, file, json, reply, text};

pub(crate) const IRIS: &[u8] = b"sepal_length,species\n5.1,setosa\n";
pub(crate) const PATH: &str = "/files/iris.csv";

/// a project with iris added from `server`, whose first response serves it. later commands may
/// reach the server through data.toml and data.lock.
pub(crate) fn added(sandbox: &Sandbox, server: &Server) -> Result {
    sandbox.allow(&server.url);
    let output = sandbox.run(&["add", &server.url])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    Ok(())
}

/// copies data.toml and data.lock into a new folder, as a fresh clone of the project would.
pub(crate) fn clone(sandbox: &Sandbox, name: &str) -> Result<std::path::PathBuf> {
    let folder = sandbox.folder(name)?;
    for file in ["data.toml", "data.lock"] {
        fs::copy(sandbox.work().join(file), folder.join(file))?;
    }
    Ok(folder)
}

#[test]
fn list_shows_each_dataset_with_its_source_and_size() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let listed = sandbox.run(&["list"])?;
    assert_eq!(listed.status.code(), Some(0));
    let stdout = text(&listed.stdout)?;
    assert!(stdout.starts_with("iris  "), "{stdout}");
    assert!(stdout.contains("1 file · 32 B  data/iris"), "{stdout}");
    let document = json(&sandbox.run(&["--json", "list"])?)?;
    let dataset = &document["result"]["datasets"][0];
    assert_eq!(dataset["name"], "iris");
    assert_eq!(dataset["ref"], server.url.as_str());
    assert_eq!(dataset["files"], 1);
    assert_eq!(dataset["bytes"], IRIS.len());
    assert_eq!(dataset["folder"], "data/iris");
    assert_eq!(dataset["locked_files"][0]["path"], "iris.csv");
    assert_eq!(dataset["locked_files"][0]["size"], IRIS.len());
    assert_eq!(dataset["locked_files"][0]["sha256"], sha256(IRIS));
    assert_eq!(dataset["locked_files"][0]["url"], server.url.as_str());
    assert!(dataset.get("title").is_some(), "{dataset}");
    assert_eq!(
        dataset["access"], "unknown",
        "a plain link says nothing of it"
    );
    let lock = sandbox.work().join("data.lock");
    let pinned = fs::read_to_string(&lock)?;
    fs::write(
        &lock,
        pinned.replace("\nlocked = ", "\naccess = \"terms\"\nlocked = "),
    )?;
    let document = json(&sandbox.run(&["--json", "list"])?)?;
    assert_eq!(document["result"]["datasets"][0]["access"], "terms");
    fs::write(&lock, pinned)?;
    let one = sandbox.run(&["list", "iris"])?;
    assert_eq!(one.status.code(), Some(0), "{}", text(&one.stderr)?);
    assert_eq!(text(&one.stdout)?, "iris.csv  32 B\n");
    let document = json(&sandbox.run(&["--json", "list", "iris"])?)?;
    assert_eq!(
        document["result"]["datasets"].as_array().map(Vec::len),
        Some(1)
    );
    assert_eq!(document["result"]["datasets"][0]["name"], "iris");
    let unknown = sandbox.run(&["list", "irs"])?;
    assert_eq!(unknown.status.code(), Some(2));
    assert!(
        text(&unknown.stderr)?.contains("fetchloom list iris"),
        "{}",
        text(&unknown.stderr)?
    );
    Ok(())
}

/// after a change, a person at a terminal is told the usual next step. a pipe, `-q` and `--json`
/// never are, so what scripts read stays the same.
#[cfg(target_os = "linux")]
#[test]
fn get_offers_to_pin_a_download_in_the_project_it_ran_in() -> Result {
    let sandbox = Sandbox::new()?;
    let responses = vec![
        file(IRIS, "\"v1\""),
        file(IRIS, "\"v1\""),
        file(IRIS, "\"v1\""),
    ];
    let server = Server::start(PATH, responses)?;
    let Some(outside) = sandbox.on_a_terminal(&["get", &server.url])? else {
        return Ok(());
    };
    assert!(
        outside.contains("next: to pin this dataset in a project, run `fetchloom add "),
        "{outside}"
    );
    assert!(sandbox.run(&["add", &server.url])?.status.success());

    let inside = sandbox
        .on_a_terminal(&["get", &server.url, "--dir", "again"])?
        .unwrap_or_default();

    assert!(
        inside.contains("next: to pin it in this project instead, run `fetchloom add "),
        "{inside}"
    );
    Ok(())
}

#[test]
fn the_next_step_is_told_only_at_a_terminal() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), file(IRIS, "\"v1\"")])?;
    for args in [
        &["add", server.url.as_str()][..],
        &["lock"],
        &["sync"],
        &["remove", "iris"],
        &["get", server.url.as_str()],
    ] {
        let output = sandbox.run(args)?;
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(0), "{args:?}: {stderr}");
        assert!(!stderr.contains("next:"), "{args:?}: {stderr}");
    }
    #[cfg(target_os = "linux")]
    {
        let sandbox = Sandbox::new()?;
        let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), file(IRIS, "\"v1\"")])?;
        if let Some(screen) = sandbox.on_a_terminal(&["add", &server.url])? {
            assert!(
                screen.contains(
                    "\n  next: commit data.toml and data.lock; anyone who clones runs `fetchloom sync`\n"
                ),
                "{screen}"
            );
            for args in [
                &["-q", "remove", "iris"][..],
                &["--json", "add", &server.url],
            ] {
                let screen = sandbox.on_a_terminal(args)?.unwrap_or_default();
                assert!(!screen.contains("next:"), "{args:?}: {screen}");
            }
        }
    }
    Ok(())
}

#[test]
fn a_second_sync_does_nothing_at_all() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let lock = fs::read(sandbox.work().join("data.lock"))?;
    let output = sandbox.run(&["sync"])?;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        text(&output.stderr)?,
        "         Ok all pinned files are in place by size and time, nothing to do\n"
    );
    let quiet = json(&sandbox.run(&["--json", "sync"])?)?;
    assert_eq!(quiet["work"]["requests"], 0);
    assert_eq!(quiet["result"]["lock_changed"], false);
    assert_eq!(
        fs::read(sandbox.work().join("data.lock"))?,
        lock,
        "data.lock is not rewritten"
    );
    Ok(())
}

/// a sync's last line follows what it did: taking files that already hold the pinned bytes as
/// placed is work, and a dataset with `place = false` has no files in the data folder to be in
/// place, so neither ends with all files in place and nothing to do.
#[test]
fn a_sync_that_adopts_or_places_nothing_says_so() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    // a copy of the pinned bytes nothing records as placed, as one downloaded by hand
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    delete(&placed)?;
    fs::write(&placed, IRIS)?;
    delete(&sandbox.work().join(".fetchloom").join("state"))?;
    let adopted = text(&sandbox.run(&["sync"])?.stderr)?;
    assert!(
        adopted.contains("    Adopted data/iris · 1 file "),
        "{adopted}"
    );
    assert!(
        adopted.ends_with("     Synced 1 dataset into data\n"),
        "{adopted}"
    );
    let manifest = sandbox.work().join("data.toml");
    let written = fs::read_to_string(&manifest)?;
    fs::write(
        &manifest,
        written.replacen("[datasets.iris]\n", "[datasets.iris]\nplace = false\n", 1),
    )?;
    let retired = sandbox.run(&["sync"])?;
    assert_eq!(retired.status.code(), Some(0), "{}", text(&retired.stderr)?);
    for (args, said) in [
        (
            &["sync", "iris"][..],
            "         Ok iris is not placed · place = false in data.toml, nothing to do\n",
        ),
        (
            &["sync"][..],
            "         Ok nothing to place in data, nothing to do\n",
        ),
        (
            &["sync", "--dry-run", "iris"][..],
            "         Ok iris is not placed · place = false in data.toml; dry run, nothing was changed\n",
        ),
    ] {
        assert_eq!(text(&sandbox.run(args)?.stderr)?, said, "{args:?}");
    }
    Ok(())
}

#[test]
fn a_fresh_clone_gets_the_same_bytes_without_downloading_them_again() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let warm = clone(&sandbox, "warm")?;
    let document = json(&sandbox.command_in(&warm, &["--json", "sync"]).output()?)?;
    assert_eq!(document["status"], "ok");
    assert_eq!(
        document["work"]["requests"], 0,
        "the cache already holds iris"
    );
    let links = sandbox.links()?;
    assert_eq!(
        document["work"]["copies"],
        u64::from(!links.reflink && !links.hardlink),
        "a link costs no disk where the drive makes one"
    );
    assert_eq!(
        fs::read(warm.join("data").join("iris").join("iris.csv"))?,
        IRIS
    );
    let cold = clone(&sandbox, "cold")?;
    let output = sandbox
        .command_in(&cold, &["--json", "sync"])
        .env("FETCHLOOM_CACHE", sandbox.folder("other-cache")?)
        .output()?;
    assert_eq!(json(&output)?["work"]["requests"], 1);
    assert_eq!(
        fs::read(cold.join("data").join("iris").join("iris.csv"))?,
        IRIS
    );
    Ok(())
}

#[test]
fn hand_edits_survive_sync_until_forced() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    // editors save by writing a new file in place of the old one
    delete(&placed)?;
    fs::write(&placed, "my own notes\n")?;
    let status = sandbox.run(&["status"])?;
    assert!(
        text(&status.stdout)?.contains("Edited data/iris/iris.csv"),
        "{}",
        text(&status.stdout)?
    );
    let kept = sandbox.run(&["sync"])?;
    assert_eq!(kept.status.code(), Some(0));
    assert!(
        text(&kept.stderr)?.contains("data/iris/iris.csv has changed, so it stays"),
        "{}",
        text(&kept.stderr)?
    );
    assert_eq!(fs::read_to_string(&placed)?, "my own notes\n");
    let forced = sandbox.run(&["sync", "--force"])?;
    assert_eq!(forced.status.code(), Some(0), "{}", text(&forced.stderr)?);
    assert_eq!(fs::read(&placed)?, IRIS);
    assert_eq!(
        server.requests().len(),
        1,
        "the cache still had the locked bytes"
    );
    Ok(())
}

/// the folder a suggested `fetchloom -C <folder> ...` names, unquoted, from the help `told`
/// gives before `then`.
fn named_folder(told: &str, then: &str) -> Option<std::path::PathBuf> {
    let (_, after) = told.split_once("`fetchloom -C ")?;
    let (quoted, _) = after.split_once(then)?;
    let unquoted = quoted
        .strip_prefix(['"', '\''])
        .and_then(|inner| inner.strip_suffix(['"', '\'']))
        .unwrap_or(quoted);
    Some(std::path::PathBuf::from(unquoted))
}

/// a run that reached its project with -C suggests a force that names the same project, so a
/// person who pastes it into the folder they ran fetchloom from never forces another project's
/// files. a run inside the project keeps the plain command.
#[test]
fn a_force_after_a_run_with_c_names_its_project() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    // editors save by writing a new file in place of the old one
    delete(&placed)?;
    fs::write(&placed, "my own notes\n")?;
    let elsewhere = sandbox.folder("elsewhere")?;
    let work = sandbox.work();
    let project = fs::canonicalize(&work)?;
    let typed = work.to_string_lossy();
    for (args, then) in [
        (["-C", &typed, "sync"], " sync --force` replaces it"),
        (["-C", &typed, "status"], " sync --force` to replace"),
    ] {
        let output = sandbox.command_in(&elsewhere, &args).output()?;
        assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
        let told = text(&output.stderr)?;
        let named = named_folder(&told, then).ok_or_else(|| format!("no -C in {told}"))?;
        assert_eq!(fs::canonicalize(named)?, project, "{told}");
    }
    let inside = text(&sandbox.run(&["sync"])?.stderr)?;
    assert!(
        inside.contains("`fetchloom sync --force` replaces it"),
        "{inside}"
    );
    assert_eq!(fs::read_to_string(&placed)?, "my own notes\n");
    Ok(())
}

/// a hardlink is the cache's own copy, which every project placing the file shares, so an edit
/// made in place through it is damage to that copy: status says so, and sync puts the pinned file
/// back and keeps the changed copy beside it. a reflink or a copy is the project's own, and its
/// edit stays.
#[test]
fn an_edit_in_place_through_a_shared_link_is_put_back_and_kept_beside_it() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), file(IRIS, "\"v1\"")])?;
    sandbox.allow(&server.url);
    let added = json(&sandbox.run(&["--json", "add", &server.url])?)?;
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    edit(&placed, b"changed in place\n")?;
    let status = text(&sandbox.run(&["status"])?.stdout)?;
    let preview = text(&sandbox.run(&["sync", "--dry-run"])?.stderr)?;
    let synced = json(&sandbox.run(&["--json", "sync"])?)?;
    let placement = &synced["result"]["placement"];
    if added["work"]["hardlinks"] == 1 {
        assert!(
            status.contains(
                "Damaged data/iris/iris.csv · linked to the cache's copy, which was changed"
            ),
            "{status}"
        );
        assert!(
            preview.contains(
                "data/iris/iris.csv would be put back as pinned: it is linked to the cache's copy, \
                 which was changed. the changed copy would be kept as data/iris/iris.csv.changed"
            ),
            "{preview}"
        );
        assert_eq!(
            placement["restored"],
            serde_json::json!(["data/iris/iris.csv"])
        );
        assert_eq!(
            placement["changed_copies"],
            serde_json::json!([
                {"path": "data/iris/iris.csv", "kept_as": "data/iris/iris.csv.changed"}
            ])
        );
        assert_eq!(fs::read(&placed)?, IRIS);
        assert_eq!(
            fs::read(placed.with_file_name("iris.csv.changed"))?,
            b"changed in place\n",
            "the edit is kept, not dropped"
        );
        assert_eq!(
            server.requests().len(),
            2,
            "the changed copy is downloaded again"
        );
    } else {
        assert_eq!(
            placement["kept"][0]["reason"], "changed",
            "a reflink or a copy is this project's own"
        );
        assert_eq!(fs::read(&placed)?, b"changed in place\n");
    }
    Ok(())
}

#[test]
fn a_kept_changed_copy_never_replaces_a_file_already_there() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), file(IRIS, "\"v1\"")])?;
    sandbox.allow(&server.url);
    let added = json(&sandbox.run(&["--json", "add", &server.url])?)?;
    if added["work"]["hardlinks"] != 1 {
        return Ok(());
    }
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    fs::write(placed.with_file_name("iris.csv.changed"), "my notes\n")?;
    edit(&placed, b"changed in place\n")?;
    let synced = sandbox.run(&["sync"])?;
    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("the changed copy is kept as data/iris/iris.csv.changed.2"),
        "{stderr}"
    );
    assert_eq!(
        fs::read_to_string(placed.with_file_name("iris.csv.changed"))?,
        "my notes\n"
    );
    assert_eq!(
        fs::read(placed.with_file_name("iris.csv.changed.2"))?,
        b"changed in place\n"
    );
    assert_eq!(fs::read(&placed)?, IRIS);
    Ok(())
}

#[test]
fn an_edit_to_a_placed_file_never_spreads_to_other_projects() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    let mut edited = IRIS.to_vec();
    edited[0] = b'S';
    edit(&placed, &edited)?;
    let output = sandbox.run(&["sync", "--force"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    assert_eq!(fs::read(&placed)?, IRIS, "the locked bytes are back");
    // only a hardlink is the cached object itself, so only an edit through one damages the
    // object, which the next sync then downloads again
    let links = sandbox.links()?;
    let hardlinked = links.hardlinks_read_only();
    assert_eq!(server.requests().len(), if hardlinked { 2 } else { 1 });
    let other = clone(&sandbox, "other")?;
    sandbox.command_in(&other, &["sync"]).output()?;
    assert_eq!(
        fs::read(other.join("data").join("iris").join("iris.csv"))?,
        IRIS
    );
    Ok(())
}

/// bytes that changed upstream stop a sync, which names both ways on: the pinned bytes from a
/// folder mirror, or the new ones by an upgrade. only a person may choose the new bytes, so JSON
/// gives no fix a script would run.
#[test]
fn changed_bytes_upstream_stop_sync_with_both_ways_on() -> Result {
    let sandbox = Sandbox::new()?;
    let changed = || file(b"species\nsetosa\n", "\"v2\"");
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), changed(), changed()])?;
    added(&sandbox, &server)?;
    let cold = clone(&sandbox, "cold")?;
    let other_cache = sandbox.folder("other-cache")?;
    let output = sandbox
        .command_in(&cold, &["sync"])
        .env("FETCHLOOM_CACHE", &other_cache)
        .output()?;
    assert_eq!(output.status.code(), Some(30));
    assert!(
        !cold.join("data").join("iris").join("iris.csv").exists(),
        "nothing unverified is placed"
    );
    let stderr = text(&output.stderr)?;
    sandbox.filters().bind(|| insta::assert_snapshot!(stderr));

    let output = sandbox
        .command_in(&cold, &["--json", "sync", "--locked"])
        .env("FETCHLOOM_CACHE", &other_cache)
        .output()?;
    assert_eq!(output.status.code(), Some(30));
    let error = &json(&output)?["errors"][0];
    assert_eq!(error["kind"], "integrity.mismatch");
    assert_eq!(error["fix"], serde_json::Value::Null, "{error}");
    let help = error["help"].as_str().unwrap_or_default();
    for way in [
        "`fetchloom push --to <folder>`",
        "`fetchloom sync --mirror <folder>`",
        "`fetchloom lock --upgrade iris`",
    ] {
        assert!(help.contains(way), "{help}");
    }
    Ok(())
}

#[test]
fn changed_bytes_from_a_source_that_states_md5_get_the_same_fix() -> Result {
    let sandbox = Sandbox::new()?;
    let same_size = b"sepal_length,species\n4.9,setosa\n";
    assert_eq!(same_size.len(), IRIS.len());
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), file(same_size, "\"v2\"")])?;
    added(&sandbox, &server)?;
    // zenodo, figshare and dataverse state an MD5, which data.lock keeps beside the SHA-256
    let lock = sandbox.work().join("data.lock");
    let text_before = fs::read_to_string(&lock)?;
    fs::write(
        &lock,
        text_before.replace(
            ", url = ",
            ", md5 = \"0773746ec982652e1c31c2c7bbaacc9c\", url = ",
        ),
    )?;
    let cold = clone(&sandbox, "cold")?;
    let output = sandbox
        .command_in(&cold, &["sync"])
        .env("FETCHLOOM_CACHE", sandbox.folder("other-cache")?)
        .output()?;
    assert_eq!(output.status.code(), Some(30));
    let stderr = text(&output.stderr)?;
    assert!(
        stderr.contains("is not the file data.lock records"),
        "{stderr}"
    );
    assert!(stderr.contains("fetchloom lock --upgrade iris"), "{stderr}");
    Ok(())
}

#[test]
fn offline_uses_the_cache_or_says_what_is_missing() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let warm = clone(&sandbox, "warm")?;
    let output = sandbox.command_in(&warm, &["--offline", "sync"]).output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    let cold = clone(&sandbox, "cold")?;
    let output = sandbox
        .command_in(&cold, &["--offline", "sync"])
        .env("FETCHLOOM_CACHE", sandbox.folder("other-cache")?)
        .output()?;
    assert_eq!(output.status.code(), Some(20));
    assert_eq!(
        server.requests().len(),
        1,
        "offline never touches the network"
    );
    let stderr = text(&output.stderr)?;
    sandbox.filters().bind(|| insta::assert_snapshot!(stderr));
    Ok(())
}

#[test]
fn lock_check_fails_until_the_lock_follows_data_toml() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let manifest = sandbox.work().join("data.toml");
    let text_before = fs::read_to_string(&manifest)?;
    fs::write(&manifest, format!("{text_before}select = \"*.csv\"\n"))?;
    let stale = sandbox.run(&["lock", "--check"])?;
    assert_eq!(stale.status.code(), Some(1));
    assert!(
        text(&stale.stderr)?.contains("iris changed in data.toml"),
        "{}",
        text(&stale.stderr)?
    );
    let lock_before = fs::read(sandbox.work().join("data.lock"))?;
    let locked = sandbox.run(&["sync", "--locked"])?;
    assert_eq!(locked.status.code(), Some(1));
    assert!(
        text(&locked.stderr)?.contains("or sync without --locked"),
        "{}",
        text(&locked.stderr)?
    );
    assert_eq!(
        fs::read(sandbox.work().join("data.lock"))?,
        lock_before,
        "--locked writes nothing"
    );
    let relocked = sandbox.run(&["lock"])?;
    assert_eq!(
        relocked.status.code(),
        Some(0),
        "{}",
        text(&relocked.stderr)?
    );
    assert_eq!(
        server.requests().len(),
        1,
        "a new filter on the same version downloads nothing"
    );
    assert_eq!(sandbox.run(&["lock", "--check"])?.status.code(), Some(0));
    assert_eq!(sandbox.run(&["sync", "--locked"])?.status.code(), Some(0));
    let empty = sandbox.folder("empty")?;
    fs::write(empty.join("data.toml"), "")?;
    let refused = sandbox.command_in(&empty, &["sync", "--locked"]).output()?;
    assert_eq!(refused.status.code(), Some(1), "{}", text(&refused.stderr)?);
    assert!(
        !empty.join("data.lock").exists(),
        "--locked never writes data.lock"
    );
    Ok(())
}

/// a clone whose owner never committed data.lock: nothing pins the files' bytes yet, which sync
/// and status both say, with the two ways on, rather than calling data.toml changed.
#[test]
fn a_clone_without_data_lock_says_nothing_pins_its_files() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let clone = sandbox.folder("clone")?;
    fs::copy(sandbox.work().join("data.toml"), clone.join("data.toml"))?;

    let locked = sandbox.command_in(&clone, &["sync", "--locked"]).output()?;
    assert_eq!(locked.status.code(), Some(1));
    let status = sandbox.command_in(&clone, &["status"]).output()?;
    assert_eq!(status.status.code(), Some(0));
    let streams = format!(
        "{}status stdout:\n{}status stderr:\n{}",
        text(&locked.stderr)?,
        text(&status.stdout)?,
        text(&status.stderr)?
    );
    sandbox.filters().bind(|| insta::assert_snapshot!(streams));
    assert!(
        !clone.join("data.lock").exists(),
        "neither writes data.lock"
    );
    Ok(())
}

/// the parts of the lists in the work folder's data.lock.d, by path from the work folder.
fn parts(sandbox: &Sandbox) -> Result<Vec<String>> {
    let mut parts: Vec<String> = fs::read_dir(sandbox.work().join("data.lock.d"))?
        .map(|entry| {
            Ok(format!(
                "data.lock.d/{}",
                entry?.file_name().to_string_lossy()
            ))
        })
        .collect::<Result<_>>()?;
    parts.sort();
    Ok(parts)
}

/// the warnings of the json document `value` about lists git does not have, without those a
/// disk adds, such as fat's copies.
fn git_warnings(value: &serde_json::Value) -> Vec<String> {
    value["warnings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .filter(|warning| warning.contains("git does not have"))
        .map(str::to_owned)
        .collect()
}

/// a commit of changed files alone, as `git commit -a` makes, leaves a new list in data.lock.d
/// out, and a clone of it cannot read data.lock: status names each list git does not have, with
/// the git command that adds it, and says nothing once git has it.
#[test]
fn status_names_lists_git_does_not_have() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    sandbox.allow(&server.url);
    if crate::merge::ok(&sandbox, &["init", "-q"])?.is_none() {
        return Ok(());
    }
    let listed = |args: &[&str]| {
        sandbox
            .command(args)
            .env("FETCHLOOM_TEST_INLINE", "0")
            .output()
    };
    let added = listed(&["add", &server.url])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    crate::merge::ok(&sandbox, &["add", "data.toml", "data.lock", ".gitignore"])?;
    crate::merge::ok(&sandbox, &["commit", "-qm", "iris"])?;
    let [part] = parts(&sandbox)?.try_into().map_err(|_| "one list")?;

    let status = json(&sandbox.run(&["--json", "status"])?)?;
    assert_eq!(
        git_warnings(&status),
        [format!(
            "data.lock names {part}, which git does not have; run `git add data.lock.d`, then commit it with data.lock, since a clone without it cannot read data.lock"
        )]
    );
    crate::merge::ok(&sandbox, &["add", "data.lock.d"])?;
    let status = json(&sandbox.run(&["--json", "status"])?)?;
    assert_eq!(git_warnings(&status), Vec::<String>::new());
    Ok(())
}

/// the warnings of the json document `value` about recipe code git does not have.
fn code_warnings(value: &serde_json::Value) -> Vec<String> {
    value["warnings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .filter(|warning| warning.contains("code of clean"))
        .map(str::to_owned)
        .collect()
}

/// a recipe's code is part of what its key answers, so a commit that leaves out a code file, a
/// new file in a code folder or one .gitignore leaves out, leaves a clone unable to build the
/// recipe from the same code: status names each, with the git command that adds it, and says
/// nothing of what the code's hash skips, once git has them all, or with no git.
#[test]
fn status_names_recipe_code_git_does_not_have() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    if crate::merge::ok(&sandbox, &["init", "-q"])?.is_none() {
        return Ok(());
    }
    added(&sandbox, &server)?;
    let work = sandbox.work();
    let manifest = work.join("data.toml");
    let written = fs::read_to_string(&manifest)?;
    fs::write(
        &manifest,
        format!(
            "{written}\n[datasets.clean]\nfrom = [\"iris\"]\nrun = [\"python3\", \"prep.py\", \"{{iris}}\", \"{{out}}\"]\ncode = [\"prep.py\", \"lib\"]\n"
        ),
    )?;
    fs::create_dir_all(work.join("lib").join("__pycache__"))?;
    for (path, bytes) in [
        ("prep.py", "print(1)\n"),
        ("lib/a.py", "a = 1\n"),
        ("lib/new.py", "b = 2\n"),
        ("lib/model.bin", "weights"),
        ("lib/__pycache__/a.cpython-312.pyc", "cache"),
    ] {
        fs::write(work.join(path), bytes)?;
    }
    let ignore = fs::read_to_string(work.join(".gitignore"))?;
    fs::write(work.join(".gitignore"), format!("{ignore}*.bin\n"))?;
    let commit = ["data.toml", "data.lock", ".gitignore", "lib/a.py"];
    crate::merge::ok(&sandbox, &[&["add"][..], &commit].concat())?;
    crate::merge::ok(&sandbox, &["commit", "-qm", "clean"])?;

    let status = json(&sandbox.run(&["--json", "status"])?)?;
    assert_eq!(
        code_warnings(&status),
        [
            "git does not have 2 files of the code of clean, such as prep.py; run `git add prep.py lib/new.py`, then commit them, since a clone without them cannot build clean from the same code",
            ".gitignore leaves out lib/model.bin, code of clean; a clone without it cannot build clean from the same code: commit it with `git add -f lib/model.bin`, or move it out of the recipe's code",
        ]
    );
    let told = sandbox.run(&["-v", "status"])?;
    let stderr = text(&told.stderr)?;
    assert!(
        stderr
            .contains("warning: git does not have 2 files of the code of clean, such as prep.py\n"),
        "{stderr}"
    );
    assert!(stderr.contains("            lib/new.py\n"), "{stderr}");
    assert!(!stderr.contains("__pycache__"), "{stderr}");
    let unfound = sandbox
        .command(&["--json", "status"])
        .env("PATH", "")
        .output()?;
    assert_eq!(code_warnings(&json(&unfound)?), Vec::<String>::new());

    crate::merge::ok(&sandbox, &["add", "prep.py", "lib"])?;
    let status = json(&sandbox.run(&["--json", "status"])?)?;
    assert_eq!(code_warnings(&status).len(), 1, "{status}");
    crate::merge::ok(&sandbox, &["add", "-f", "lib/model.bin"])?;
    let status = json(&sandbox.run(&["--json", "status"])?)?;
    assert_eq!(code_warnings(&status), Vec::<String>::new());
    Ok(())
}

/// a command that writes a list git does not have ends by saying so, as a warning for a script
/// and as a `next:` line for a person at a terminal. outside a git repository, or once git has
/// the list, it says nothing.
#[test]
fn a_new_list_git_does_not_have_is_named_at_the_end() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, (0..3).map(|_| file(IRIS, "\"v1\"")).collect())?;
    sandbox.allow(&server.url);
    let listed = |args: &[&str]| {
        let mut command = sandbox.command(args);
        command.env("FETCHLOOM_TEST_INLINE", "0");
        command
    };
    let ran = |args: &[&str]| -> Result<serde_json::Value> {
        let output = listed(args).output()?;
        assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
        json(&output)
    };
    let outside = ran(&["--json", "add", &server.url, "--as", "first"])?;
    assert_eq!(
        git_warnings(&outside),
        Vec::<String>::new(),
        "no repository"
    );
    if crate::merge::ok(&sandbox, &["init", "-q"])?.is_none() {
        return Ok(());
    }
    crate::merge::ok(&sandbox, &["add", "."])?;
    crate::merge::ok(&sandbox, &["commit", "-qm", "first"])?;
    let before = parts(&sandbox)?;

    let second = ran(&["--json", "add", &server.url, "--as", "second"])?;
    let part = parts(&sandbox)?
        .into_iter()
        .find(|part| !before.contains(part))
        .ok_or("no new list")?;
    assert_eq!(
        git_warnings(&second),
        [format!(
            "git does not have {part}, which data.lock names now; run `git add data.lock.d` before you commit, since `git commit -a` leaves new files out"
        )]
    );
    let again = ran(&["--json", "sync"])?;
    assert_eq!(
        git_warnings(&again),
        Vec::<String>::new(),
        "nothing new written"
    );
    #[cfg(target_os = "linux")]
    {
        crate::merge::ok(&sandbox, &["add", "."])?;
        crate::merge::ok(&sandbox, &["commit", "-qm", "second"])?;
        let Some(told) =
            sandbox.on_a_terminal_running(&listed(&["add", &server.url, "--as", "third"]))?
        else {
            return Ok(());
        };
        let next = told.lines().last().unwrap_or_default();
        assert!(
            next.starts_with("  next: run `git add data.lock.d` before you commit: git does not have data.lock.d/third."),
            "{told}"
        );
    }
    Ok(())
}

/// renaming a dataset's table in data.toml keeps its pin: relocking carries the entry over to the
/// new name without asking the source, which here serves other bytes now, and says so. a sync of
/// the new name alone, as `path` makes, copies it, since it changes no other entry, and the next
/// lock of every dataset removes the old one.
#[test]
fn a_renamed_dataset_keeps_its_pin_without_asking_the_source() -> Result {
    let sandbox = Sandbox::new()?;
    let changed = || file(b"species\nsetosa\n", "\"v2\"");
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), changed(), changed()])?;
    added(&sandbox, &server)?;
    let manifest = sandbox.work().join("data.toml");
    let rename = |from: &str, to: &str| -> Result {
        let text = fs::read_to_string(&manifest)?;
        let table = format!("[datasets.{from}]\n");
        assert!(text.contains(&table), "{text}");
        fs::write(
            &manifest,
            text.replace(&table, &format!("[datasets.{to}]\n")),
        )?;
        Ok(())
    };
    let pinned = |name: &str| -> Result<bool> {
        let lock = fs::read_to_string(sandbox.work().join("data.lock"))?;
        Ok(lock.contains(&format!("name = \"{name}\"")) && lock.contains(&sha256(IRIS)))
    };

    rename("iris", "flowers")?;
    let synced = sandbox.run(&["sync"])?;
    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("Renamed iris to flowers in data.lock · its pin stays\n"),
        "{stderr}"
    );
    assert!(pinned("flowers")? && !pinned("iris")?);
    assert_eq!(
        fs::read(sandbox.work().join("data/flowers/iris.csv"))?,
        IRIS
    );
    assert_eq!(sandbox.run(&["lock", "--check"])?.status.code(), Some(0));

    rename("flowers", "petals")?;
    let found = sandbox.run(&["path", "petals"])?;
    assert_eq!(found.status.code(), Some(0), "{}", text(&found.stderr)?);
    assert!(pinned("petals")? && pinned("flowers")?);
    let locked = sandbox.run(&["lock"])?;
    assert!(
        text(&locked.stderr)?.contains("Removed flowers from data.lock\n"),
        "{}",
        text(&locked.stderr)?
    );
    assert!(pinned("petals")? && !pinned("flowers")?);
    assert_eq!(server.requests().len(), 1, "only the add asked the source");
    Ok(())
}

/// a run asked to leave data.lock as it is, with `--locked`, never ends with a JSON `fix` that
/// would change it, whatever stops it: a script that runs `fix` would undo `--locked`. each state
/// stops `sync`, `path` or `build` with `--locked` and the kind named, and the help still names
/// the ways on for a person.
#[test]
fn a_locked_run_never_offers_a_fix_that_changes_data_lock() -> Result {
    let sandbox = Sandbox::new()?;
    let changed = || file(b"species\nsetosa\n", "\"v2\"");
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), changed()])?;
    crate::build::with(&sandbox, &server, crate::recipes::WRITE)?;
    let work = sandbox.work();
    let (manifest, pins) = (work.join("data.toml"), work.join("data.lock"));
    let (asked, pinned) = (fs::read_to_string(&manifest)?, fs::read_to_string(&pins)?);
    let stops = |args: &[&str], kind: &str| -> Result {
        let output = crate::build::build(&sandbox, args)?;
        let error = &json(&output)?["errors"][0];
        assert_eq!(error["kind"], kind, "{args:?}: {error}");
        assert_eq!(error["fix"], serde_json::Value::Null, "{args:?}: {error}");
        assert!(error["help"].is_string(), "{args:?}: {error}");
        Ok(())
    };

    stops(&["--json", "sync", "--locked"], "build.needed")?;
    stops(&["--json", "path", "--locked", "clean"], "build.needed")?;
    fs::write(
        &manifest,
        asked.replacen(
            "[datasets.iris]\n",
            "[datasets.iris]\nselect = \"*.csv\"\n",
            1,
        ),
    )?;
    for args in [
        &["--json", "sync", "--locked"][..],
        &["--json", "path", "--locked", "iris"],
        &["--json", "build", "--locked"],
    ] {
        stops(args, "lock.outdated")?;
    }
    fs::write(
        &manifest,
        format!("<<<<<<< HEAD\n{asked}=======\n{asked}>>>>>>> theirs\n"),
    )?;
    stops(&["--json", "sync", "--locked"], "project.invalid")?;
    fs::write(&manifest, &asked)?;
    delete(&pins)?;
    stops(&["--json", "sync", "--locked"], "lock.outdated")?;
    stops(&["--json", "path", "--locked", "iris"], "lock.outdated")?;
    fs::write(&pins, &pinned)?;
    let cold = clone(&sandbox, "cold")?;
    let output = sandbox
        .command_in(&cold, &["--json", "sync", "--locked", "iris"])
        .env("FETCHLOOM_CACHE", sandbox.folder("other-cache")?)
        .output()?;
    let error = &json(&output)?["errors"][0];
    assert_eq!(error["kind"], "integrity.mismatch", "{error}");
    assert_eq!(error["fix"], serde_json::Value::Null, "{error}");
    Ok(())
}

#[test]
fn upgrading_to_the_same_data_leaves_the_lock_alone() -> Result {
    let sandbox = Sandbox::new()?;
    let unchanged = reply("304 Not Modified", &[("ETag", "\"v1\"")], b"");
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), unchanged])?;
    added(&sandbox, &server)?;
    let path = sandbox.work().join("data.lock");
    let pinned = fs::read_to_string(&path)?;
    let start = pinned.find("locked = \"").ok_or("no locked line")? + "locked = \"".len();
    let dated = format!(
        "{}2001-02-03T04:05:06Z{}",
        &pinned[..start],
        &pinned[start + 20..]
    );
    fs::write(&path, &dated)?;
    let output = sandbox.run(&["lock", "--upgrade"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert_eq!(
        stderr,
        "         Ok data.lock is already up to date, nothing to do\n"
    );
    assert_eq!(
        fs::read_to_string(&path)?,
        dated,
        "a pin dates from when its data last changed"
    );
    let asked = &server.requests()[1];
    assert!(
        asked.contains("if-none-match: \"v1\""),
        "an upgrade asks whether an unpinned file changed: {asked}"
    );
    Ok(())
}

#[test]
fn upgrading_a_link_whose_bytes_changed_pins_the_new_bytes() -> Result {
    let sandbox = Sandbox::new()?;
    let changed = b"sepal_length,species\n9.9,new\n";
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), file(changed, "\"v2\"")])?;
    added(&sandbox, &server)?;
    let before = fs::read_to_string(sandbox.work().join("data.lock"))?;
    let output = sandbox.run(&["lock", "--upgrade"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    let after = fs::read_to_string(sandbox.work().join("data.lock"))?;
    assert_ne!(before, after);
    assert!(
        after.contains(&format!("size = {}", changed.len())),
        "{after}"
    );
    Ok(())
}

#[test]
fn inside_git_the_data_folder_is_ignored_once() -> Result {
    let sandbox = Sandbox::new()?;
    fs::create_dir(sandbox.work().join(".git"))?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    let output = sandbox.run(&["add", &server.url])?;
    assert!(
        text(&output.stderr)?.contains("Ignored data/ in .gitignore"),
        "{}",
        text(&output.stderr)?
    );
    assert_eq!(
        fs::read_to_string(sandbox.work().join(".gitignore"))?,
        "/data/\n"
    );
    assert_eq!(
        fs::read_to_string(sandbox.work().join(".fetchloom").join(".gitignore"))?,
        "*\n"
    );
    Ok(())
}

/// a project that became a git repository after its first add gets the ignore rule from the next
/// command that places files, for the folder `data_dir` names: `add` again, `sync`, whose dry run
/// only says it would, and `path`, which places the files it answers with first.
#[test]
fn every_command_that_places_files_ignores_the_data_folder_in_git() -> Result {
    for (command, data_dir) in [("add", "data"), ("sync", "data"), ("path", "inputs/raw")] {
        let sandbox = Sandbox::new()?;
        let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), file(IRIS, "\"v1\"")])?;
        let work = sandbox.work();
        if data_dir != "data" {
            fs::write(
                work.join("data.toml"),
                format!("[project]\ndata_dir = \"{data_dir}\"\n"),
            )?;
        }
        added(&sandbox, &server)?;
        assert!(!work.join(".gitignore").exists(), "{command}");
        fs::create_dir(work.join(".git"))?;
        let args = match command {
            "add" => vec!["add", server.url.as_str()],
            "sync" => {
                let previewed = text(&sandbox.run(&["sync", "--dry-run"])?.stderr)?;
                assert!(
                    previewed.contains("Would add data/ to .gitignore"),
                    "{previewed}"
                );
                assert!(!work.join(".gitignore").exists());
                vec!["sync"]
            }
            _ => {
                delete(&work.join(data_dir).join("iris").join("iris.csv"))?;
                vec!["path", "iris"]
            }
        };
        let output = sandbox.run(&args)?;
        let stderr = text(&output.stderr)?;
        assert!(output.status.success(), "{command}: {stderr}");
        assert!(
            stderr.contains(&format!("Ignored {data_dir}/ in .gitignore")),
            "{command}: {stderr}"
        );
        assert_eq!(
            fs::read_to_string(work.join(".gitignore"))?,
            format!("/{data_dir}/\n"),
            "{command}"
        );
        match command {
            "add" => assert!(stderr.contains("iris is already in data.toml"), "{stderr}"),
            "path" => assert_eq!(
                text(&output.stdout)?,
                format!(
                    "{}\n",
                    data_dir
                        .split('/')
                        .fold(work.clone(), |folder, part| folder.join(part))
                        .join("iris")
                        .display()
                )
            ),
            _ => {}
        }
    }
    Ok(())
}

/// status prints each difference as a row on stdout, where a pipe or a file gets it, and its
/// closing line and hints on stderr, so `fetchloom status | grep Missing` finds what is missing.
/// `-v` rows go with their row, quiet prints none, and `--json` keeps stdout one document.
#[test]
fn status_lists_what_differs() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let folder = sandbox.work().join("data").join("iris");
    let clean = sandbox.run(&["status"])?;
    assert_eq!(
        text(&clean.stderr)?,
        "         Ok everything matches data.lock by size and time\n"
    );
    assert!(clean.stdout.is_empty(), "{}", text(&clean.stdout)?);
    // editors save by writing a new file in place of the old one
    delete(&folder.join("iris.csv"))?;
    fs::write(folder.join("iris.csv"), "mine")?;
    fs::write(folder.join("notes.txt"), "mine")?;
    let edited = sandbox.run(&["status"])?;
    assert_eq!(
        text(&edited.stdout)?,
        "     Edited data/iris/iris.csv\n      Extra data/iris/notes.txt\n"
    );
    let stderr = text(&edited.stderr)?;
    assert!(
        stderr.starts_with("      Found 2 differences from data.lock\n  hint: "),
        "{stderr}"
    );
    assert!(!stderr.contains("data/iris/"), "{stderr}");
    delete(&folder.join("iris.csv"))?;
    let output = sandbox.run(&["status"])?;
    assert_eq!(output.status.code(), Some(0));
    let streams = format!(
        "stdout:\n{}stderr:\n{}",
        text(&output.stdout)?,
        text(&output.stderr)?
    );
    sandbox.filters().bind(|| insta::assert_snapshot!(streams));
    let quiet = sandbox.run(&["-q", "status"])?;
    assert_eq!(quiet.status.code(), Some(0));
    assert!(quiet.stdout.is_empty(), "{}", text(&quiet.stdout)?);
    let document = json(&sandbox.run(&["--json", "status"])?)?;
    assert_eq!(document["result"]["missing"][0], "data/iris/iris.csv");
    assert_eq!(document["result"]["extra"][0], "data/iris/notes.txt");
    // a dataset set to place = false names the files the next sync removes, each with -v
    let synced = sandbox.run(&["sync"])?;
    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    let manifest = sandbox.work().join("data.toml");
    let written = fs::read_to_string(&manifest)?;
    fs::write(
        &manifest,
        written.replacen("[datasets.iris]\n", "[datasets.iris]\nplace = false\n", 1),
    )?;
    let retiring = sandbox.run(&["-v", "status"])?;
    assert_eq!(
        text(&retiring.stdout)?,
        " Not placed iris · place = false in data.toml, so `fetchloom sync` removes its file from data/iris\n            data/iris/iris.csv\n      Extra data/iris/notes.txt\n"
    );
    let stderr = text(&retiring.stderr)?;
    assert!(!stderr.contains("data/iris/"), "{stderr}");
    Ok(())
}

#[test]
fn status_creates_no_cache() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let unmade = sandbox.work().join("unmade-cache");
    let output = sandbox
        .command(&["status"])
        .env("FETCHLOOM_CACHE", &unmade)
        .output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    assert!(!unmade.exists());
    Ok(())
}

#[test]
fn fetchloom_alone_in_a_project_sums_up_its_status() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let inner = sandbox.work().join("data").join("iris");
    let calm = sandbox.command_in(&inner, &[]).output()?;
    assert_eq!(calm.status.code(), Some(0));
    assert!(calm.stdout.is_empty(), "{}", text(&calm.stdout)?);
    assert_eq!(
        text(&calm.stderr)?,
        "         Ok everything matches data.lock by size and time\n"
    );
    delete(&inner.join("iris.csv"))?;
    let missing = sandbox.run(&[])?;
    assert_eq!(
        text(&missing.stderr)?,
        "      Found 1 difference from data.lock\n  hint: `fetchloom status` lists what differs\n"
    );
    let document = json(&sandbox.run(&["--json"])?)?;
    assert_eq!(document["command"], "status");
    assert_eq!(document["result"]["missing"][0], "data/iris/iris.csv");
    Ok(())
}

#[test]
fn project_commands_outside_a_project_say_how_to_start_one() -> Result {
    let output = Sandbox::new()?.run(&["sync"])?;
    assert_eq!(output.status.code(), Some(50));
    let stderr = text(&output.stderr)?;
    assert!(
        stderr.contains("there is no data.toml in this folder or any folder above it"),
        "{stderr}"
    );
    assert!(stderr.contains("`fetchloom add <link>`"), "{stderr}");
    Ok(())
}

/// a git repository made inside a project is a project of its own, since data.toml belongs to
/// the repository that commits it: `add` there starts one and leaves the outer data.toml alone.
#[test]
fn a_git_repository_inside_a_project_starts_its_own() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let outer = fs::read(sandbox.work().join("data.toml"))?;
    let inner = sandbox.work().join("experiment");
    fs::create_dir_all(inner.join(".git"))?;
    let output = sandbox.command_in(&inner, &["add", &server.url]).output()?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains(&format!("Started a project in {}", inner.display())),
        "{stderr}"
    );
    assert!(!stderr.contains("using the project in"), "{stderr}");
    assert_eq!(
        fs::read(inner.join("data").join("iris").join("iris.csv"))?,
        IRIS
    );
    assert!(inner.join("data.toml").is_file());
    assert_eq!(fs::read(sandbox.work().join("data.toml"))?, outer);
    Ok(())
}

/// in a git repository without a data.toml, the search stops at the repository and says so; a
/// project outside it is used only when -C names its folder.
#[test]
fn a_project_outside_the_git_repository_needs_its_folder_named() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let inner = sandbox.work().join("experiment");
    fs::create_dir_all(inner.join(".git"))?;
    fs::create_dir_all(inner.join("notebooks"))?;
    let refused = sandbox
        .command_in(&inner.join("notebooks"), &["--json", "path", "iris"])
        .output()?;
    assert_eq!(refused.status.code(), Some(50));
    let error = &json(&refused)?["errors"][0];
    assert_eq!(error["kind"], "project.invalid");
    assert_eq!(
        error["message"],
        format!(
            "there is no data.toml in this folder or any folder above it inside the git repository at {}",
            inner.display()
        )
    );
    assert_eq!(
        error["help"],
        "run `fetchloom add <link>` to start a project here, or give -C the folder of a project outside this repository, as in `fetchloom -C <folder> <command>`"
    );
    assert_eq!(error["fix"], serde_json::Value::Null);
    let used = sandbox
        .command_in(&inner, &["-C", "..", "path", "iris"])
        .output()?;
    assert_eq!(used.status.code(), Some(0), "{}", text(&used.stderr)?);
    assert_eq!(
        text(&used.stdout)?,
        format!("{}\n", sandbox.work().join("data").join("iris").display())
    );
    Ok(())
}

/// a merge git could not finish in data.toml or data.lock stops a command before any request and
/// names `fetchloom lock`, which merges both sides and never picks one for a person.
#[test]
fn an_unfinished_merge_says_how_to_finish_it() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    for name in ["data.toml", "data.lock"] {
        let path = sandbox.work().join(name);
        let merged = fs::read_to_string(&path)?;
        fs::write(
            &path,
            format!("<<<<<<< HEAD\n{merged}=======\n{merged}>>>>>>> theirs\n"),
        )?;
        let output = sandbox.run(&["--json", "sync"])?;
        assert_eq!(output.status.code(), Some(50), "{name}");
        let report = json(&output)?;
        let error = &report["errors"][0];
        assert_eq!(error["kind"], "project.invalid");
        assert_eq!(
            error["message"],
            format!("{name} has an unfinished git merge conflict")
        );
        assert_eq!(error["details"], serde_json::json!([format!("{name}:1")]));
        assert_eq!(error["fix"], "fetchloom lock", "{name}");
        assert_eq!(report["work"]["requests"], 0);
        fs::write(&path, merged)?;
    }
    assert_eq!(server.requests().len(), 1);
    Ok(())
}

#[test]
fn a_script_never_gets_the_cache_reminder_and_nothing_is_freed() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let removed = sandbox.run(&["remove", "iris"])?;
    assert_eq!(removed.status.code(), Some(0), "{}", text(&removed.stderr)?);
    // the reminder only runs for a person at a terminal, and it never deletes
    assert!(!sandbox.cache().join("reminded").exists());
    let info = json(&sandbox.run(&["--json", "cache"])?)?;
    assert_eq!(info["result"]["objects"], 1);
    Ok(())
}
#[test]
fn the_cache_knows_its_projects_and_checks_its_files() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let info = json(&sandbox.run(&["--json", "cache"])?)?;
    assert_eq!(info["result"]["objects"], 1);
    assert_eq!(info["result"]["needed"], IRIS.len());
    assert_eq!(
        info["result"]["projects"][0],
        sandbox.work().display().to_string()
    );
    let gc = sandbox.run(&["cache", "gc"])?;
    assert_eq!(text(&gc.stderr)?, "         Ok nothing to free\n");
    let verify = sandbox.run(&["cache", "verify"])?;
    assert_eq!(verify.status.code(), Some(0));
    assert_eq!(
        text(&verify.stderr)?,
        "   Verified 1 file · 32 B, every one is intact\n"
    );
    fs::remove_file(sandbox.work().join("data.toml"))?;
    let forgotten = json(&sandbox.run(&["--json", "cache"])?)?;
    assert_eq!(
        forgotten["result"]["projects"].as_array().map(Vec::len),
        Some(0),
        "a project without data.toml is forgotten"
    );
    let young = sandbox.run(&["cache", "gc"])?;
    assert_eq!(
        text(&young.stderr)?,
        format!(
            "     Forgot {} · it has no data.toml anymore\n         Ok nothing to free yet\n  hint: 32 B no project needs is kept for a day in case you want it back; `fetchloom cache gc --now` frees it now\n  hint: if a project moved, run `fetchloom sync` in its new folder, so gc keeps its files\n",
            sandbox.work().display()
        )
    );
    let document = json(&sandbox.run(&["--json", "cache", "gc"])?)?;
    assert_eq!(document["result"]["kept_objects"], 1);
    assert_eq!(document["result"]["kept_bytes"], IRIS.len());
    let now = json(&sandbox.run(&["--json", "cache", "gc", "--now"])?)?;
    assert_eq!(now["result"]["removed_objects"], 1);
    assert_eq!(now["result"]["kept_objects"], 0);
    Ok(())
}

#[test]
fn a_changed_object_fails_verify_and_waits_for_replacement() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let objects = sandbox.cache().join("objects");
    let fan = fs::read_dir(&objects)?
        .next()
        .ok_or("no fan-out folder")??;
    let object = fs::read_dir(fan.path())?.next().ok_or("no object")??.path();
    edit(&object, b"damaged on disk")?;
    let verify = sandbox.run(&["cache", "verify"])?;
    assert_eq!(verify.status.code(), Some(30));
    let stderr = text(&verify.stderr)?;
    assert!(
        stderr.contains(&format!(
            "warning: {} has changed, so fetchloom downloads it again the next time it is needed",
            object.display()
        )),
        "{stderr}"
    );
    assert_eq!(fs::read(&object)?, b"damaged on disk");
    // a reflink or copy in the project still holds the locked bytes, so only a placement that
    // needs the object again brings the replacement
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    if fs::read(&placed)? == IRIS {
        delete(&placed)?;
    }
    let synced = sandbox.run(&["sync", "--force"])?;
    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    assert_eq!(fs::read(&object)?, IRIS);
    assert_eq!(fs::read(&placed)?, IRIS);
    Ok(())
}

/// a clone of the project in a folder whose path is `length` characters long.
#[cfg(windows)]
fn deep_clone(sandbox: &Sandbox, length: usize) -> Result<std::path::PathBuf> {
    let mut deep = sandbox.folder("deep")?;
    while length - deep.as_os_str().len() > 40 {
        deep.push("a_project_folder_name");
    }
    let rest = length - deep.as_os_str().len() - 1;
    deep.push("p".repeat(rest));
    fs::create_dir_all(&deep)?;
    for file in ["data.toml", "data.lock"] {
        fs::copy(sandbox.work().join(file), deep.join(file))?;
    }
    Ok(deep)
}

#[cfg(windows)]
#[test]
fn a_project_whose_metadata_passes_260_characters_syncs_and_answers_path() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    // as deep as the folder in the report, which windows can still work in
    let deep = deep_clone(&sandbox, 240)?;
    let output = sandbox.command_in(&deep, &["--json", "sync"]).output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
    assert!(deep.join(".fetchloom").join("state").is_file());
    let placed = deep.join("data").join("iris").join("iris.csv");
    assert_eq!(fs::read(&placed)?, IRIS);
    delete(&placed)?;
    let output = sandbox
        .command_in(&deep, &["path", "iris/iris.csv"])
        .output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    assert_eq!(fs::read(text(&output.stdout)?.trim())?, IRIS);
    let status = sandbox.command_in(&deep, &["--json", "status"]).output()?;
    assert_eq!(json(&status)?["status"], "ok");
    Ok(())
}

#[cfg(windows)]
#[test]
fn a_folder_too_long_to_work_in_is_named_with_its_length_before_anything_is_written() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let deep = deep_clone(&sandbox, 300)?;
    let named = deep.display().to_string();
    let output = sandbox.command(&["-C", &named, "sync"]).output()?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(2), "{stderr}");
    assert!(
        stderr.contains(&named) && stderr.contains("300 characters"),
        "{stderr}"
    );
    assert!(
        stderr.contains(
            "move the project to a folder with a shorter path, then run `fetchloom sync` there"
        ),
        "{stderr}"
    );
    for (arguments, expected) in [
        (
            vec![format!("-C{named}"), "sync".to_owned()],
            "fetchloom sync",
        ),
        (
            vec![format!("-C={named}"), "sync".to_owned()],
            "fetchloom sync",
        ),
        (
            vec![
                format!("-vC{named}"),
                "sync".to_owned(),
                "--offline".to_owned(),
            ],
            "fetchloom -v sync --offline",
        ),
    ] {
        let arguments: Vec<_> = arguments.iter().map(String::as_str).collect();
        let output = sandbox.run(&arguments)?;
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(2), "{stderr}");
        assert!(
            stderr.contains(&format!("then run `{expected}` there")),
            "{stderr}"
        );
    }
    assert!(!deep.join(".fetchloom").exists());
    assert!(!deep.join("data").exists());
    Ok(())
}

#[test]
fn status_and_sync_explain_untracked_conflicts() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    fs::remove_file(sandbox.work().join(".fetchloom").join("state"))?;
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    delete(&placed)?;
    fs::write(&placed, b"my work")?;
    let status = sandbox.run(&["status"])?;
    let stderr = text(&status.stderr)?;
    assert!(stderr.contains("fetchloom sync --force"), "{stderr}");
    let synced = sandbox.run(&["sync"])?;
    let stderr = text(&synced.stderr)?;
    assert!(stderr.contains("1 file left unchanged"), "{stderr}");
    assert!(
        !stderr.contains("all pinned files are in place"),
        "{stderr}"
    );
    assert_eq!(fs::read(placed)?, b"my work");
    Ok(())
}

#[test]
fn status_reports_datasets_removed_from_the_manifest() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let clone = clone(&sandbox, "removed")?;
    fs::write(clone.join("data.toml"), "[datasets]\n")?;
    let before = fs::read(clone.join("data.lock"))?;
    let output = sandbox.command_in(&clone, &["status"]).output()?;
    assert!(output.status.success());
    let stdout = text(&output.stdout)?;
    assert!(
        stdout.contains("    Removed iris from data.toml; `fetchloom sync` updates data.lock\n"),
        "{stdout}"
    );
    let stderr = text(&output.stderr)?;
    assert!(!stderr.contains("everything matches"), "{stderr}");
    let output = sandbox.command_in(&clone, &["--json", "status"]).output()?;
    let report = json(&output)?;
    assert_eq!(report["result"]["removed"], serde_json::json!(["iris"]));
    assert_eq!(report["result"]["unlocked"], serde_json::json!([]));
    assert_eq!(report["work"]["requests"], 0);
    let brief = sandbox.command_in(&clone, &[]).output()?;
    assert!(!text(&brief.stderr)?.contains("everything matches"));
    assert_eq!(fs::read(clone.join("data.lock"))?, before);
    Ok(())
}

#[test]
fn status_caps_human_lists_but_keeps_all_differences_in_json() -> Result {
    use std::fmt::Write as _;

    let sandbox = Sandbox::new()?;
    let manifest = "[datasets.new]\nref = 'zenodo:1'\n[datasets.set]\nref = 'zenodo:2'\n";
    fs::write(sandbox.work().join("data.toml"), manifest)?;
    let mut lock = String::from("version = 1\n");
    for (name, reference) in [("old", "zenodo:1"), ("set", "zenodo:2")] {
        writeln!(
            lock,
            "\n[[dataset]]\nname = '{name}'\nref = '{reference}'\nspec = ''\nresolved = '{reference}'\nlocked = '2026-01-01T00:00:00Z'"
        )?;
        if name == "set" {
            for index in 0..25 {
                writeln!(
                    lock,
                    "\n[[dataset.files]]\npath = 'file{index:02}.csv'\nsize = 1\nsha256 = '{}'\nurl = 'https://example.org/{index}.csv'",
                    "0".repeat(64)
                )?;
            }
        } else {
            writeln!(lock, "files = []")?;
        }
    }
    fs::write(sandbox.work().join("data.lock"), &lock)?;
    fs::create_dir(sandbox.work().join("data"))?;
    for index in 0..25 {
        fs::write(
            sandbox
                .work()
                .join("data")
                .join(format!("extra{index:02}.txt")),
            b"keep",
        )?;
    }
    let output = sandbox.run(&["--offline", "status"])?;
    let stdout = text(&output.stdout)?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(stdout.contains("Unlocked new"), "{stdout}");
    assert!(stdout.contains("Removed old from data.toml"), "{stdout}");
    assert!(stdout.contains("Missing data/set/file19.csv"), "{stdout}");
    assert!(!stdout.contains("file20.csv"), "{stdout}");
    assert!(stdout.contains("Extra data/extra19.txt"), "{stdout}");
    assert!(!stdout.contains("extra20.txt"), "{stdout}");
    assert_eq!(stdout.matches("and 5 more").count(), 2);
    assert!(stderr.contains("53 differences from data.lock"), "{stderr}");
    let output = sandbox.run(&["--offline", "--json", "status"])?;
    assert_eq!(output.status.code(), Some(0));
    let document = json(&output)?;
    assert_eq!(
        document["result"]["missing"]
            .as_array()
            .ok_or("missing is not an array")?
            .len(),
        25
    );
    assert_eq!(document["result"]["missing"][24], "data/set/file24.csv");
    assert_eq!(
        document["result"]["extra"]
            .as_array()
            .ok_or("extra is not an array")?
            .len(),
        25
    );
    assert_eq!(document["result"]["removed"], serde_json::json!(["old"]));
    assert_eq!(document["work"]["requests"], 0);
    assert_eq!(
        fs::read_to_string(sandbox.work().join("data.toml"))?,
        manifest
    );
    assert_eq!(fs::read_to_string(sandbox.work().join("data.lock"))?, lock);
    assert!(!sandbox.work().join(".fetchloom").exists());
    assert_eq!(fs::read(sandbox.work().join("data/extra24.txt"))?, b"keep");
    Ok(())
}

#[test]
fn manifest_citations_do_not_echo_reference_secrets() -> Result {
    let sandbox = Sandbox::new()?;
    for manifest in [
        r#"datasets = { sample = { ref = "https://example.org/a?token=EM_CANARY", extract = "yes" } }"#,
        r#"datasets = { sample = { ref = "https://user:EM_CANARY@example.org/a", extract = "yes" } }"#,
        r#"datasets = { sample = { "r\u0065f" = "ht\u0074ps://example.org/a?token=EM_CANARY", extract = "yes" } }"#,
        r#"datasets = { sample = { ref = "https://example.org/a?token=EM_CANARY", extract = } }"#,
    ] {
        fs::write(sandbox.work().join("data.toml"), manifest)?;
        for args in [&["status"][..], &["status", "--json"]] {
            let output = sandbox.run(args)?;
            assert_eq!(output.status.code(), Some(50));
            let shown = format!("{}{}", text(&output.stdout)?, text(&output.stderr)?);
            assert!(shown.contains("data.toml:1"), "{shown}");
            assert!(!shown.contains("EM_CANARY"), "{shown}");
        }
    }
    Ok(())
}

#[test]
fn equal_untracked_read_only_files_get_the_requested_writable_copy() -> Result {
    let sandbox = Sandbox::new()?;
    if !sandbox.links()?.hardlink {
        return Ok(());
    }
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    sandbox.allow(&server.url);
    fs::write(
        sandbox.work().join("data.toml"),
        format!("[datasets.iris]\nref = {:?}\ncopy = true\n", server.url),
    )?;
    let locked = sandbox.run(&["lock"])?;
    assert!(locked.status.success(), "{}", text(&locked.stderr)?);
    let target = sandbox.work().join("data/iris/iris.csv");
    fs::create_dir_all(target.parent().ok_or("target has no parent")?)?;
    let foreign = sandbox.work().join("original.csv");
    fs::write(&foreign, IRIS)?;
    let mut permissions = fs::metadata(&foreign)?.permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&foreign, permissions)?;
    fs::hard_link(&foreign, &target)?;

    let synced = sandbox.run(&["--json", "--offline", "sync"])?;
    assert!(synced.status.success(), "{}", text(&synced.stdout)?);
    assert_eq!(fs::read(&target)?, IRIS);
    assert!(!fs::metadata(&target)?.permissions().readonly());
    assert!(!same_file::is_same_file(&target, &foreign)?);
    assert_eq!(fs::read(&foreign)?, IRIS);
    assert!(fs::metadata(&foreign)?.permissions().readonly());
    let status = sandbox.run(&["--json", "status"])?;
    assert!(status.status.success(), "{}", text(&status.stdout)?);
    assert_eq!(json(&status)?["result"]["outdated"], serde_json::json!([]));
    assert_eq!(server.requests().len(), 1);
    Ok(())
}

#[test]
fn sync_reuses_its_validated_plan() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let project = clone(&sandbox, "plan-once")?;
    let trace = sandbox.work().join("sync.trace.json");
    let output = sandbox
        .command_in(&project, &["--json", "--offline", "sync"])
        .env("FETCHLOOM_TRACE", &trace)
        .output()?;
    assert!(output.status.success(), "{}", text(&output.stdout)?);
    let events: Vec<serde_json::Value> = serde_json::from_slice(&fs::read(&trace)?)?;
    let plans = events
        .iter()
        .filter(|event| event["ph"] == "B" && event["name"] == "plan")
        .count();
    assert_eq!(plans, 1);
    assert_eq!(fs::read(project.join("data/iris/iris.csv"))?, IRIS);
    assert_eq!(json(&output)?["work"]["requests"], 0);
    Ok(())
}

#[test]
fn prepared_downloads_keep_pins_and_shared_object_claims() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let project = clone(&sandbox, "prepared")?;
    let path = project.join("data.lock");
    let lock = fs::read_to_string(&path)?;
    let row = lock
        .lines()
        .find(|line| line.starts_with("  { path = "))
        .ok_or("no locked file")?;
    let second = row.replacen("iris.csv", "z.csv", 1);
    fs::write(&path, lock.replace(row, &format!("{row}\n{second}")))?;
    let parsed: toml::Value = toml::from_str(&lock)?;
    let digest = parsed["dataset"][0]["files"][0]["sha256"]
        .as_str()
        .ok_or("no digest")?;
    delete(
        &sandbox
            .cache()
            .join("objects")
            .join(&digest[..2])
            .join(&digest[2..]),
    )?;
    let output = sandbox.command_in(&project, &["--json", "sync"]).output()?;
    assert!(output.status.success(), "{}", text(&output.stdout)?);
    let document = json(&output)?;
    assert_eq!(document["work"]["requests"], 1);
    assert_eq!(document["work"]["bytes_down"], IRIS.len());
    assert_eq!(document["result"]["placement"]["placed"], 2);
    for file in ["iris.csv", "z.csv"] {
        assert_eq!(fs::read(project.join("data/iris").join(file))?, IRIS);
    }
    assert_eq!(server.requests().len(), 2);
    Ok(())
}

#[test]
fn prepared_downloads_validate_late_links_before_any_transfer() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let project = clone(&sandbox, "invalid-prepared")?;
    let path = project.join("data.lock");
    let lock = fs::read_to_string(&path)?;
    let row = lock
        .lines()
        .find(|line| line.starts_with("  { path = "))
        .ok_or("no locked file")?;
    let parsed: toml::Value = toml::from_str(&lock)?;
    let digest = parsed["dataset"][0]["files"][0]["sha256"]
        .as_str()
        .ok_or("no digest")?;
    let invalid = row
        .replacen("iris.csv", "z.csv", 1)
        .replace(&server.url, "not a URL")
        .replace(digest, &"0".repeat(64));
    fs::write(&path, lock.replace(row, &format!("{row}\n{invalid}")))?;
    delete(
        &sandbox
            .cache()
            .join("objects")
            .join(&digest[..2])
            .join(&digest[2..]),
    )?;
    let output = sandbox.command_in(&project, &["--json", "sync"]).output()?;
    assert_eq!(output.status.code(), Some(50), "{}", text(&output.stdout)?);
    assert_eq!(json(&output)?["errors"][0]["kind"], "project.invalid");
    assert_eq!(json(&output)?["work"]["requests"], 0);
    assert!(!project.join("data").exists());
    assert_eq!(server.requests().len(), 1);
    Ok(())
}

#[test]
fn path_and_status_refuse_case_collisions_before_reporting_locations() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let project = clone(&sandbox, "case-clash")?;
    let path = project.join("data.lock");
    let lock = fs::read_to_string(&path)?;
    let row = lock
        .lines()
        .find(|line| line.starts_with("  { path = "))
        .ok_or("no locked file")?;
    let other = row.replacen("iris.csv", "IRIS.csv", 1);
    fs::write(&path, lock.replace(row, &format!("{other}\n{row}")))?;
    for args in [
        &["status"][..],
        &["path", "iris"],
        &["path", "iris/iris.csv"],
    ] {
        let output = sandbox
            .command_in(&project, &["--json", "--offline"])
            .args(args)
            .output()?;
        assert_eq!(output.status.code(), Some(50), "{}", text(&output.stdout)?);
        let document = json(&output)?;
        assert_eq!(document["errors"][0]["kind"], "project.invalid");
        assert_eq!(document["work"]["requests"], 0);
        assert!(!project.join("data").exists());
        assert!(!project.join(".fetchloom").exists());
    }
    assert_eq!(server.requests().len(), 1);
    Ok(())
}

/// a data folder that links elsewhere is refused. `data_dir` cannot lead out of the project, so
/// the help says what works: a real folder there, or the whole project on the other disk.
#[cfg(unix)]
#[test]
fn a_linked_data_folder_is_refused_with_advice_that_works() -> Result {
    let sandbox = Sandbox::new()?;
    let elsewhere = sandbox.folder("elsewhere")?;
    std::os::unix::fs::symlink(&elsewhere, sandbox.work().join("data"))?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    let output = sandbox.run(&["--json", "add", &server.url])?;
    assert_eq!(output.status.code(), Some(50), "{}", text(&output.stdout)?);
    let document = json(&output)?;
    let help = document["errors"][0]["help"].as_str().unwrap_or_default();
    assert!(!help.contains("set data_dir"), "{help}");
    assert!(help.contains("move the whole project folder"), "{help}");
    assert_eq!(fs::read_dir(&elsewhere)?.count(), 0);
    Ok(())
}

/// a project fetchloom cannot lock is named as the project, not by the files fetchloom keeps
/// inside `.fetchloom` to lock it, and the help names the one file to delete when nothing else
/// holds it. a file far larger than any fetchloom writes there stands in for a damaged one.
#[test]
fn a_project_that_cannot_be_locked_is_named_plainly() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let owner = sandbox.work().join(".fetchloom").join("owner");
    fs::write(&owner, vec![b' '; 1 << 20])?;
    delete(&sandbox.work().join("data/iris/iris.csv"))?;
    let output = sandbox.run(&["sync"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(50), "{stderr}");
    assert!(
        stderr.starts_with("error: could not lock the project: "),
        "{stderr}"
    );
    assert!(
        stderr.contains(&format!(
            "help: if no other fetchloom is working in this project, delete {}, then run `fetchloom sync` again",
            owner.display()
        )),
        "{stderr}"
    );
    for internal in ["claim", "owner record"] {
        assert!(!stderr.contains(internal), "{internal}: {stderr}");
    }
    Ok(())
}

/// a sync that cannot save its list of placed files names that file and what it holds, with no
/// internal name for it. a folder this test cannot write stands in for a full disk.
#[cfg(unix)]
#[test]
fn a_sync_that_cannot_save_what_it_placed_names_the_file() -> Result {
    use std::os::unix::fs::PermissionsExt;
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    delete(&sandbox.work().join("data/iris/iris.csv"))?;
    let own = sandbox.work().join(".fetchloom");
    fs::set_permissions(&own, fs::Permissions::from_mode(0o555))?;
    // a user who may write anywhere, such as root, cannot show this
    if fs::write(own.join("probe"), b"").is_ok() {
        fs::remove_file(own.join("probe"))?;
        fs::set_permissions(&own, fs::Permissions::from_mode(0o755))?;
        return Ok(());
    }
    let output = sandbox.run(&["sync"]);
    fs::set_permissions(&own, fs::Permissions::from_mode(0o755))?;
    let output = output?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(50), "{stderr}");
    assert!(
        stderr.contains(&format!(
            "error: could not save the list of placed files in {}: permission was denied",
            own.join("state").display()
        )),
        "{stderr}"
    );
    Ok(())
}

/// the owner record only tells a waiting command who holds the project, so a sync that cannot
/// write it, as on a full disk, still runs under the project lock. so does one that cannot write
/// the state when all it would record is that files hashed as unchanged, as a filesystem that
/// keeps whole seconds asks right after an add. a folder this test cannot write stands in for
/// the full disk.
#[cfg(unix)]
#[test]
fn a_sync_that_cannot_write_the_owner_record_still_works() -> Result {
    use std::os::unix::fs::PermissionsExt;
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let own = sandbox.work().join(".fetchloom");
    // the state as new as the file it records, as a coarse clock leaves them, so sync hashes the
    // file and would settle it
    let placed = fs::metadata(sandbox.work().join("data/iris/iris.csv"))?.modified()?;
    fs::File::options()
        .write(true)
        .open(own.join("state"))?
        .set_times(fs::FileTimes::new().set_modified(placed))?;
    fs::set_permissions(&own, fs::Permissions::from_mode(0o555))?;
    // a user who may write anywhere, such as root, cannot show this
    if fs::write(own.join("probe"), b"").is_ok() {
        fs::remove_file(own.join("probe"))?;
        fs::set_permissions(&own, fs::Permissions::from_mode(0o755))?;
        return Ok(());
    }
    let output = sandbox.run(&["sync"]);
    let blocked = sandbox.run(&["--json", "sync"]);
    fs::set_permissions(&own, fs::Permissions::from_mode(0o755))?;
    let (output, blocked) = (output?, blocked?);
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(stderr.contains("nothing to do"), "{stderr}");
    assert_eq!(blocked.status.code(), Some(0), "{}", text(&blocked.stdout)?);
    let left: Vec<String> = fs::read_dir(&own)?
        .map(|entry| Ok(entry?.file_name().to_string_lossy().into_owned()))
        .collect::<std::io::Result<_>>()?;
    assert!(
        !left.iter().any(|name| name.starts_with("owner")),
        "{left:?}"
    );
    Ok(())
}

/// a dataset that `lock` or `sync` pins for the first time gets a line of its own with what it
/// pinned, so a typo in data.toml that pins an unrelated record stands out.
#[test]
fn a_first_pin_by_lock_or_sync_gets_a_line_of_its_own() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let manifest = sandbox.work().join("data.toml");
    for (command, name) in [("lock", "cows"), ("sync", "hens")] {
        let other = Server::start(
            &format!("/files/{name}.csv"),
            vec![file(b"cow\nbess\n", "\"c1\"")],
        )?;
        sandbox.allow(&other.url);
        let updated = format!(
            "{}\n[datasets.{name}]\nref = \"{}\"\n",
            fs::read_to_string(&manifest)?,
            other.url
        );
        fs::write(&manifest, updated)?;
        let output = sandbox.run(&[command])?;
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(0), "{stderr}");
        let line = format!(
            "     Pinned {name}, new in data.lock: {} · 1 file · 9 B\n",
            other.url
        );
        assert!(stderr.contains(&line), "{command}: {stderr}");
        assert!(
            !stderr.contains(&format!("Locked {name}")),
            "{command}: {stderr}"
        );
    }
    Ok(())
}

/// `sync`, `sync --dry-run` and `lock` name in JSON the datasets whose pins changed: pinned for
/// the first time, pinned to other data, or no longer pinned.
#[test]
fn json_names_the_datasets_whose_pins_changed() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let manifest = sandbox.work().join("data.toml");
    let first = fs::read_to_string(&manifest)?;
    let cows = Server::start("/files/cows.csv", vec![file(b"cow\n", "\"c\"")])?;
    sandbox.allow(&cows.url);
    let hens = Server::start("/files/hens.csv", vec![file(b"hen\n", "\"h\"")])?;
    sandbox.allow(&hens.url);
    let both = format!(
        "{first}\n[datasets.cows]\nref = \"{}\"\n\n[datasets.hens]\nref = \"{}\"\n",
        cows.url, hens.url
    );
    fs::write(&manifest, &both)?;
    let previewed = json(&sandbox.run(&["--json", "sync", "--dry-run"])?)?;
    assert_eq!(
        previewed["result"]["datasets"],
        serde_json::json!(["cows", "hens"])
    );
    let synced = json(&sandbox.run(&["--json", "sync"])?)?;
    assert_eq!(synced["result"]["lock_changed"], true, "{synced}");
    assert_eq!(
        synced["result"]["datasets"],
        serde_json::json!(["cows", "hens"])
    );
    fs::write(
        &manifest,
        both.replace("[datasets.iris]", "[datasets.iris-renamed]"),
    )?;
    let locked = json(&sandbox.run(&["--json", "lock"])?)?;
    assert_eq!(
        locked["result"]["datasets"],
        serde_json::json!(["iris", "iris-renamed"]),
        "{locked}"
    );
    let unchanged = json(&sandbox.run(&["--json", "sync"])?)?;
    assert_eq!(unchanged["result"]["datasets"], serde_json::json!([]));
    Ok(())
}

/// `lock --upgrade --dry-run`, or `-n`, tells what would change without writing data.lock or
/// downloading. a link no checksum pins is listed as a file to fetch, not as a change, since
/// only its bytes can tell.
#[test]
fn lock_dry_run_tells_what_would_change_and_writes_nothing() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let cows = Server::start("/files/cows.csv", vec![file(b"cow\n", "\"c\"")])?;
    let manifest = sandbox.work().join("data.toml");
    let updated = format!(
        "{}\n[datasets.cows]\nref = \"{}\"\n",
        fs::read_to_string(&manifest)?,
        cows.url
    );
    fs::write(&manifest, updated)?;
    let lock = fs::read(sandbox.work().join("data.lock"))?;
    let output = sandbox.run(&["lock", "--upgrade", "--dry-run"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("  Would pin cows, new in data.lock:"),
        "{stderr}"
    );
    assert!(stderr.contains("Would fetch iris/iris.csv"), "{stderr}");
    assert!(!stderr.contains("upgrade iris"), "{stderr}");
    assert!(stderr.contains("dry run, nothing was changed"), "{stderr}");
    let document = json(&sandbox.run(&["--json", "lock", "--upgrade", "-n"])?)?;
    let result = &document["result"];
    assert_eq!(result["dry_run"], true, "{document}");
    assert_eq!(result["lock_changed"], true);
    assert_eq!(result["datasets"], serde_json::json!(["cows"]));
    assert_eq!(result["placement"], serde_json::Value::Null);
    assert_eq!(
        result["changes"]["download"],
        serde_json::json!([
            { "path": "cows/cows.csv", "size": null },
            { "path": "iris/iris.csv", "size": null },
        ])
    );
    assert_eq!(fs::read(sandbox.work().join("data.lock"))?, lock);
    assert_eq!(server.requests().len(), 1, "only the first add downloads");
    assert_eq!(cows.requests().len(), 0);
    Ok(())
}

/// a command running in the background, with its stderr read as it comes.
pub(crate) struct Background {
    pub(crate) child: std::process::Child,
    stderr: std::thread::JoinHandle<String>,
}

impl Background {
    pub(crate) fn start(mut command: std::process::Command) -> Result<Background> {
        let mut child = command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()?;
        let mut stream = child.stderr.take().ok_or("no stderr")?;
        let stderr = std::thread::spawn(move || {
            let mut told = String::new();
            let _ = std::io::Read::read_to_string(&mut stream, &mut told);
            told
        });
        Ok(Background { child, stderr })
    }

    /// sends SIGINT, as ctrl-c at a terminal does.
    #[cfg(unix)]
    pub(crate) fn interrupt(&self) -> Result {
        let sent = std::process::Command::new("kill")
            .args(["-INT", &self.child.id().to_string()])
            .status()?;
        if sent.success() {
            Ok(())
        } else {
            Err("kill -INT failed".into())
        }
    }

    pub(crate) fn finish(mut self) -> Result<(Option<i32>, String)> {
        let status = self.child.wait()?;
        let stderr = self.stderr.join().map_err(|_| "reading stderr failed")?;
        Ok((status.code(), stderr))
    }
}

/// the sizes of the partial downloads in `cache`.
#[cfg(unix)]
pub(crate) fn partial_bytes(cache: &std::path::Path) -> u64 {
    crate::support::partial_files(cache)
        .iter()
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "part")
        })
        .filter_map(|path| fs::metadata(path).ok())
        .map(|metadata| metadata.len())
        .sum()
}

/// the line under `stopped` in `stderr`, which says what a stopped download kept, once stderr
/// shows a stop as a person asked for it: a note that a second ctrl-c quits at once, then
/// `stopped` and that line last, and no `error:`.
#[cfg(unix)]
pub(crate) fn told_a_stop<'s>(stderr: &'s str, stopped: &str) -> Result<&'s str> {
    let told: Vec<&str> = stderr.lines().collect();
    assert!(
        told.contains(&"   Stopping (press ctrl-c again to quit at once)"),
        "{stderr}"
    );
    assert!(!stderr.contains("error:"), "{stderr}");
    let [.., last, kept] = told.as_slice() else {
        return Err(format!("stderr ends before the stop's lines: {stderr}").into());
    };
    assert_eq!(*last, stopped, "{stderr}");
    assert!(
        kept.ends_with(" downloaded so far is kept, and the next run continues from there"),
        "{stderr}"
    );
    Ok(kept)
}

/// ctrl-c during a sync's download is a stop, not an error: one `Stopped` line says where it
/// stopped and that nothing changed, since the sync wrote neither data.lock nor the data folder
/// before every file was in, and a dim line under it what the download kept. the exit code stays
/// 130.
#[cfg(unix)]
#[test]
fn a_stopped_sync_says_so_in_a_plain_line() -> Result {
    let sandbox = Sandbox::new()?;
    let body: Vec<u8> = (0..1u32 << 20).map(|n| (n % 251) as u8).collect();
    let slow = Server::held_file("/files/cows.csv", body)?;
    sandbox.allow(&slow.url);
    fs::write(
        sandbox.work().join("data.toml"),
        format!("[datasets.cows]\nref = \"{}\"\n", slow.url),
    )?;
    let running = Background::start(sandbox.command(&["sync"]))?;
    wait_until(
        "bytes to arrive",
        || Ok(partial_bytes(&sandbox.cache()) > 0),
    )?;
    running.interrupt()?;
    let (code, stderr) = running.finish()?;
    assert_eq!(code, Some(130), "{stderr}");
    let kept = told_a_stop(
        &stderr,
        "    Stopped while talking to 127.0.0.1, so nothing changed",
    )?;
    assert!(kept.starts_with("            the "), "{stderr}");
    assert!(!sandbox.work().join("data.lock").exists(), "{stderr}");
    Ok(())
}

/// a sync stopped while it downloads several files says what all of them keep, which is what the
/// next sync goes on from, not what one of them keeps.
#[cfg(unix)]
#[test]
fn a_stopped_download_of_several_files_says_what_they_all_keep() -> Result {
    let sandbox = Sandbox::new()?;
    let body: Vec<u8> = (0..1u32 << 20).map(|n| (n % 251) as u8).collect();
    let cows = Server::held_file("/files/cows.csv", body.clone())?;
    sandbox.allow(&cows.url);
    let pigs = Server::held_file("/files/pigs.csv", body)?;
    sandbox.allow(&pigs.url);
    fs::write(
        sandbox.work().join("data.toml"),
        format!(
            "[datasets.cows]\nref = \"{}\"\n\n[datasets.pigs]\nref = \"{}\"\n",
            cows.url, pigs.url
        ),
    )?;
    let running = Background::start(sandbox.command(&["sync"]))?;
    wait_until("bytes of both files to arrive", || {
        let started = crate::support::partial_files(&sandbox.cache())
            .iter()
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "part")
            })
            .filter(|path| fs::metadata(path).is_ok_and(|metadata| metadata.len() > 0))
            .count();
        Ok(started == 2)
    })?;
    running.interrupt()?;
    let (code, stderr) = running.finish()?;
    assert_eq!(code, Some(130), "{stderr}");
    let kept = told_a_stop(
        &stderr,
        "    Stopped while talking to 127.0.0.1, so nothing changed",
    )?;
    let kept = kept
        .trim_start()
        .strip_prefix("the ")
        .and_then(|rest| rest.split_once(" downloaded so far"))
        .map(|(size, _)| size)
        .ok_or_else(|| format!("no size in {kept}"))?;
    let rerun = sandbox.run(&["sync"])?;
    let told = text(&rerun.stderr)?;
    assert!(rerun.status.success(), "{told}");
    let earlier = told
        .lines()
        .find_map(|line| {
            line.strip_suffix(" from an earlier run")?
                .rsplit(" \u{b7} ")
                .next()
        })
        .ok_or_else(|| format!("the second sync continued nothing: {told}"))?;
    assert_eq!(kept, earlier, "{stderr}\n{told}");
    Ok(())
}

/// ctrl-c at the question about a large download stops the downloads already running too,
/// rather than waiting for them to finish.
#[cfg(target_os = "linux")]
#[test]
fn a_cancel_at_a_question_stops_the_other_downloads() -> Result {
    let sandbox = Sandbox::new()?;
    let huge = || reply("200 OK", &[("Content-Length", "2147483648")], b"");
    let large = Server::start("/huge.bin", vec![huge(), huge()])?;
    sandbox.allow(&large.url);
    let body: Vec<u8> = (0..1u32 << 20).map(|n| (n % 251) as u8).collect();
    let slow = Server::held_file("/files/slow.csv", body)?;
    sandbox.allow(&slow.url);
    fs::write(
        sandbox.work().join("data.toml"),
        format!(
            "[datasets.huge]\nref = \"{}\"\n\n[datasets.slow]\nref = \"{}\"\n",
            large.url, slow.url
        ),
    )?;
    // -isig lets ctrl-c reach the question as the key it reads
    let Some((exit, screen)) = crate::support::typed(
        &sandbox.work(),
        &sandbox.command(&["sync"]),
        "-isig",
        "download huge/huge.bin (2.0 GiB)?",
        "\u{3}",
    )?
    else {
        return Ok(());
    };
    assert_eq!(exit, Some(130), "{screen}");
    assert!(!screen.contains("error:"), "{screen}");
    let mut folders = vec![sandbox.cache().join("objects")];
    while let Some(folder) = folders.pop() {
        for entry in fs::read_dir(&folder).into_iter().flatten().flatten() {
            let found = entry.metadata()?;
            assert!(found.is_dir() || found.len() != 1 << 20, "{screen}");
            if found.is_dir() {
                folders.push(entry.path());
            }
        }
    }
    Ok(())
}

/// a sync stopped while it downloads the files data.lock already pins, as after a clone, says
/// that nothing changed: it places files only once every one is in the cache.
#[cfg(unix)]
#[test]
fn a_sync_stopped_while_it_downloads_pinned_files_says_nothing_changed() -> Result {
    let sandbox = Sandbox::new()?;
    let body: Vec<u8> = (0..1u32 << 20).map(|n| (n % 251) as u8).collect();
    let site = crate::support::Ranged::start("/files/cows.csv", body, 0)?;
    sandbox.allow(&site.url);
    let work = sandbox.work();
    fs::write(
        work.join("data.toml"),
        format!("[datasets.cows]\nref = \"{}\"\n", site.url),
    )?;
    let locked = sandbox.run(&["lock"])?;
    assert!(locked.status.success(), "{}", text(&locked.stderr)?);
    let lock = fs::read_to_string(work.join("data.lock"))?;
    site.pace(64 * 1024);
    // a cache without the file, as on another machine
    let cache = sandbox.folder("another cache")?;
    let mut sync = sandbox.command(&["sync"]);
    sync.env("FETCHLOOM_CACHE", &cache);
    let running = Background::start(sync)?;
    wait_until("bytes to arrive", || Ok(partial_bytes(&cache) > 0))?;
    running.interrupt()?;
    let (code, stderr) = running.finish()?;
    assert_eq!(code, Some(130), "{stderr}");
    told_a_stop(
        &stderr,
        "    Stopped while talking to 127.0.0.1, so nothing changed",
    )?;
    assert_eq!(fs::read_to_string(work.join("data.lock"))?, lock);
    assert!(!work.join("data/cows").exists(), "{stderr}");
    Ok(())
}

/// a sync asks before it downloads what placing needs, once data.lock is as data.toml asks. no
/// to that question says nothing changed when data.lock was already so, and does not say it
/// when the same sync just wrote data.lock.
#[cfg(target_os = "linux")]
#[test]
fn a_cancelled_download_of_a_sync_says_nothing_changed_only_when_so() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    let large = Server::start("/files/huge.bin", vec![file(b"eleven byte", "\"h\"")])?;
    let cows = Server::start("/files/cows.csv", vec![file(b"cow\n", "\"c\"")])?;
    for (server, name) in [(&large, "huge"), (&cows, "cows")] {
        let output = sandbox.run(&["add", &server.url, "--as", name])?;
        assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    }
    // data.lock now pins 2 GiB of other bytes for the file, which the cache does not hold, so
    // placing it again asks first
    let pinned = fs::read_to_string(work.join("data.lock"))?;
    let grown = pinned
        .replace("size = 11,", "size = 2147483648,")
        .replace(&sha256(b"eleven byte"), &sha256(b"two gibibytes"));
    assert_eq!(grown.matches("2147483648").count(), 1, "{grown}");
    assert!(grown.contains(&sha256(b"two gibibytes")), "{grown}");
    fs::write(work.join("data.lock"), &grown)?;
    delete(&work.join("data/huge/huge.bin"))?;
    let sync = sandbox.command(&["sync"]);
    let question = "download 1 file (2.0 GiB)?";
    let Some((exit, screen)) = crate::support::typed(&work, &sync, "", question, "n\r")? else {
        return Ok(());
    };
    assert_eq!(exit, Some(1), "{screen}");
    assert!(
        screen.contains("  Cancelled the download of 1 file (2.0 GiB), so nothing changed\n"),
        "{screen}"
    );
    assert_eq!(fs::read_to_string(work.join("data.lock"))?, grown);
    // with cows gone from data.toml, the sync writes data.lock before it asks
    let manifest = fs::read_to_string(work.join("data.toml"))?;
    let kept: Vec<&str> = manifest
        .split("\n\n")
        .filter(|table| !table.starts_with("[datasets.cows]"))
        .collect();
    fs::write(work.join("data.toml"), kept.join("\n\n"))?;
    let Some((exit, screen)) = crate::support::typed(&work, &sync, "", question, "n\r")? else {
        return Ok(());
    };
    assert_eq!(exit, Some(1), "{screen}");
    assert!(
        screen.contains("  Cancelled the download of 1 file (2.0 GiB)\n"),
        "{screen}"
    );
    assert!(!screen.contains("nothing changed"), "{screen}");
    assert!(
        !fs::read_to_string(work.join("data.lock"))?.contains("cows"),
        "{screen}"
    );
    Ok(())
}

/// sets every folder and file under `root` read-only, or writable again, as a checkout on a
/// read-only mount or owned by another user is.
#[cfg(unix)]
pub(crate) fn writable(root: &std::path::Path, yes: bool) -> Result {
    use std::os::unix::fs::PermissionsExt;
    let mut paths = vec![root.to_path_buf()];
    let mut at = 0;
    while at < paths.len() {
        if paths[at].is_dir() {
            for entry in fs::read_dir(&paths[at])? {
                paths.push(entry?.path());
            }
        }
        at += 1;
    }
    for path in &paths {
        let mode = match (path.is_dir(), yes) {
            (true, true) => 0o755,
            (true, false) => 0o555,
            (false, true) => 0o644,
            (false, false) => 0o444,
        };
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    }
    Ok(())
}

/// a job that syncs before it reads, on a checkout it may not write, works when nothing needs
/// writing. one that would write still fails, naming the folder it could not write.
#[cfg(unix)]
#[test]
fn a_read_only_project_with_nothing_to_do_syncs() -> Result {
    if rustix::process::geteuid().is_root() {
        return Ok(());
    }
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    // a sync right after placing, as a fast machine or an image build runs it, finds file times
    // no older than its state's, so it hashes them before trusting them
    fs::File::options()
        .write(true)
        .open(sandbox.work().join(".fetchloom/state"))?
        .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_hours(24))?;
    let fresh = clone(&sandbox, "fresh")?;
    writable(&sandbox.work(), false)?;
    writable(&fresh, false)?;
    let settled = sandbox.run(&["--json", "sync"]);
    let locked = sandbox.run(&["sync", "--locked"]);
    let unsynced = sandbox.command_in(&fresh, &["--json", "sync"]).output();
    // a container's image may hold the cache read-only as well
    writable(&sandbox.cache(), false)?;
    let sealed = sandbox.run(&["sync"]);
    writable(&sandbox.cache(), true)?;
    writable(&sandbox.work(), true)?;
    writable(&fresh, true)?;
    let settled = settled?;
    assert_eq!(settled.status.code(), Some(0), "{}", text(&settled.stderr)?);
    let sealed = sealed?;
    assert_eq!(sealed.status.code(), Some(0), "{}", text(&sealed.stderr)?);
    let document = json(&settled)?;
    assert_eq!(document["result"]["lock_changed"], false);
    assert_eq!(document["result"]["placement"]["unchanged"], 1);
    let locked = locked?;
    assert_eq!(locked.status.code(), Some(0), "{}", text(&locked.stderr)?);
    assert!(
        text(&locked.stderr)?.contains("all pinned files are in place"),
        "{}",
        text(&locked.stderr)?
    );
    // a clone never synced would place files, so it needs the hold it cannot take
    let unsynced = unsynced?;
    assert_eq!(
        unsynced.status.code(),
        Some(50),
        "{}",
        text(&unsynced.stderr)?
    );
    assert_eq!(
        json(&unsynced)?["errors"][0]["message"],
        format!(
            "could not create {}: the project folder {} is read-only",
            fresh.join(".fetchloom").display(),
            fresh.display()
        )
    );
    Ok(())
}

/// a sync whose downloads each fit on the cache's disk but together do not is told once, before
/// any of them starts.
#[test]
fn a_sync_the_cache_disk_cannot_hold_asks_once_before_downloading() -> Result {
    let sandbox = Sandbox::new()?;
    // two different files, as the cache keeps identical ones once
    let (one, two) = (vec![b'x'; 1024], vec![b'y'; 1024]);
    let first = Server::start("/one.bin", vec![file(&one, "\"a\""), file(&one, "\"a\"")])?;
    let second = Server::start("/two.bin", vec![file(&two, "\"b\""), file(&two, "\"b\"")])?;
    for server in [&first, &second] {
        let output = sandbox.run(&["add", &server.url])?;
        assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    }
    let fresh = clone(&sandbox, "fresh")?;
    let empty = sandbox.folder("empty-cache")?;
    let output = sandbox
        .command_in(&fresh, &["--json", "sync"])
        .env("FETCHLOOM_CACHE", &empty)
        .env("FETCHLOOM_TEST_FREE", "1500")
        .output()?;
    assert_eq!(output.status.code(), Some(1), "{}", text(&output.stderr)?);
    let document = json(&output)?;
    assert_eq!(document["errors"][0]["kind"], "unconfirmed");
    assert_eq!(
        document["errors"][0]["message"],
        format!(
            "this sync needs at least 2.0 KiB on the disk holding the cache at {}, which has 1.5 KiB free",
            empty.display()
        )
    );
    assert_eq!(document["work"]["bytes_down"], 0);
    assert!(!fresh.join("data").join("one").exists());
    Ok(())
}

/// FAT32 holds no file of 4 GiB or more, so a sync refuses a pinned file that large before it
/// asks or downloads anything, even when the disk is short of room, since freeing space cannot
/// help. a file one byte smaller, or exFAT, goes on to ask.
#[test]
fn a_pinned_file_fat32_cannot_hold_is_refused_before_anything_downloads() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let fresh = clone(&sandbox, "fresh")?;
    let lock = fresh.join("data.lock");
    let text = fs::read_to_string(&lock)?;
    let pinned = format!("size = {}", IRIS.len());
    assert!(text.contains(&pinned), "{text}");
    let largest: u64 = (1 << 32) - 1;
    for (size, filesystem, kind) in [
        (largest + 1, "fat32", "local.io"),
        (largest, "fat32", "unconfirmed"),
        (largest + 1, "exfat", "unconfirmed"),
    ] {
        fs::write(&lock, text.replace(&pinned, &format!("size = {size}")))?;
        let cache = sandbox.folder(&format!("cache-{size}-{filesystem}"))?;
        let output = sandbox
            .command_in(&fresh, &["--json", "sync"])
            .env("FETCHLOOM_CACHE", &cache)
            .env("FETCHLOOM_TEST_FILESYSTEM", filesystem)
            .env("FETCHLOOM_TEST_FREE", "1024")
            .output()?;
        let document = json(&output)?;
        let error = &document["errors"][0];
        assert_eq!(error["kind"], kind, "{size} on {filesystem}: {error}");
        assert_eq!(document["work"]["requests"], 0, "{size} on {filesystem}");
        if kind == "local.io" {
            assert_eq!(
                error["message"],
                format!(
                    "the cache at {} is on a FAT32 disk, which holds no file of 4 GiB or more, and iris/iris.csv is 4.0 GiB",
                    cache.display()
                )
            );
        }
    }
    assert_eq!(server.requests().len(), 1, "the add alone downloaded");
    Ok(())
}

/// kaggle serves `train.csv` beside `train.csv.zip`, a zip of it. the exclude the clash's help
/// gives leaves out the listed train.csv, so the zip's lands, and no later run calls that
/// exclude one that missed or offers the drop, which would leave out the wanted file too.
#[cfg(debug_assertions)]
#[test]
fn an_exclude_that_left_out_a_listed_file_is_never_called_one_that_missed() -> Result {
    use crate::buckets::{md5, s3_listing, s3_page};
    use crate::support::Routes;

    let sandbox = Sandbox::new()?;
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    zip.start_file("train.csv", zip::write::SimpleFileOptions::default())?;
    std::io::Write::write_all(&mut zip, b"id,y\n1,0\n")?;
    let zipped = zip.finish()?.into_inner();
    let objects: [(&str, &[u8]); 3] = [
        ("kag/test.csv", b"id\n3\n"),
        ("kag/train.csv", b"id,y\n9,9\n"),
        ("kag/train.csv.zip", &zipped),
    ];
    let base = "/kag-bucket.s3.amazonaws.com/";
    let mut routes = vec![(s3_listing(base, "kag/", None), s3_page(&objects, None))];
    for (key, body) in objects {
        let etag = format!("\"{}\"", md5(body));
        routes.push((format!("{base}{key}"), file(body, &etag)));
    }
    let routes = Routes::start(routes)?;
    let run = |args: &[&str]| -> Result<(Option<i32>, String)> {
        let output = sandbox
            .command(args)
            .env("FETCHLOOM_TEST_BUCKETS", &routes.url)
            .output()?;
        let said = format!("{}{}", text(&output.stdout)?, text(&output.stderr)?);
        Ok((output.status.code(), said))
    };
    let (code, said) = run(&["add", "s3://kag-bucket/kag/"])?;
    assert_eq!(code, Some(50), "{said}");
    assert!(said.contains("--exclude \"train.csv\""), "{said}");
    let train = sandbox.work().join("data/kag/train.csv");
    let test = sandbox.work().join("data/kag/test.csv");
    let (code, said) = run(&["add", "s3://kag-bucket/kag/", "--exclude", "train.csv"])?;
    assert_eq!(code, Some(0), "{said}");
    assert!(!said.contains("does not reach"), "{said}");
    assert_eq!(fs::read(&train)?, b"id,y\n1,0\n", "the zip's train.csv");
    // a missing file makes status and sync plan, rather than find the project settled
    delete(&test)?;
    for args in [&["status"][..], &["sync"], &["status"]] {
        let (code, said) = run(args)?;
        assert_eq!(code, Some(0), "{args:?}: {said}");
        assert!(
            !said.contains("does not reach") && !said.contains("drop data/kag/train.csv"),
            "{args:?}: {said}"
        );
    }
    assert_eq!(fs::read(&train)?, b"id,y\n1,0\n");
    assert_eq!(fs::read(&test)?, b"id\n3\n", "sync put it back");
    Ok(())
}

/// a size in data.lock edited by hand, its SHA-256 left as it was. the file fetchloom placed
/// hashes to that SHA-256 at its real size, so no source or mirror can send bytes that have
/// both: status, lock --check and sync say data.lock is wrong rather than find everything in
/// place, and once it is restored, sync has nothing to do.
#[test]
fn a_size_the_pinned_bytes_do_not_have_stops_until_data_lock_is_restored() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let lock = sandbox.work().join("data.lock");
    let pinned = fs::read_to_string(&lock)?;
    let size = format!("size = {}", IRIS.len());
    assert!(pinned.contains(&size), "{pinned}");
    fs::write(
        &lock,
        pinned.replace(&size, &format!("size = {}", IRIS.len() + 1)),
    )?;
    let said = format!(
        "data.lock pins iris/iris.csv at {} bytes, but its SHA-256 is that of {} bytes on this machine",
        IRIS.len() + 1,
        IRIS.len()
    );
    for args in [
        &["--json", "status"][..],
        &["--json", "lock", "--check"],
        &["--json", "sync"],
        &["--json", "sync", "--force"],
    ] {
        let output = sandbox.run(args)?;
        let document = json(&output)?;
        let error = &document["errors"][0];
        assert_eq!(output.status.code(), Some(50), "{args:?}: {document}");
        assert_eq!(error["kind"], "project.invalid", "{args:?}");
        assert_eq!(error["message"], said.as_str(), "{args:?}");
        assert!(error["fix"].is_null(), "{args:?}: {error}");
    }
    assert_eq!(fs::read(sandbox.work().join("data/iris/iris.csv"))?, IRIS);
    fs::write(&lock, &pinned)?;
    let synced = sandbox.run(&["sync"])?;
    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(0), "{stderr}");
    assert!(stderr.contains("nothing to do"), "{stderr}");
    assert_eq!(server.requests().len(), 1, "only the add downloaded");
    Ok(())
}

/// a fresh clone of a project with a recipe, on an empty cache, as a CI runner has it, the files
/// a clone has: data.toml, data.lock, data.lock.d and the recipe's code.
fn fresh_with_recipe(sandbox: &Sandbox, name: &str) -> Result<std::path::PathBuf> {
    let folder = clone(sandbox, name)?;
    fs::copy(sandbox.work().join("clean.py"), folder.join("clean.py"))?;
    fs::create_dir_all(folder.join("data.lock.d"))?;
    for part in fs::read_dir(sandbox.work().join("data.lock.d"))? {
        let part = part?;
        fs::copy(
            part.path(),
            folder.join("data.lock.d").join(part.file_name()),
        )?;
    }
    Ok(folder)
}

/// a locked sync on a fresh runner: data.lock pins clean's result, which only a build makes
/// here, beside iris, which its source serves. the sync places iris before it stops with
/// build.needed, and its JSON keeps what it placed, so a job that checks its sources still
/// checks them: a source that changed iris fails with the mismatch instead.
#[test]
fn a_locked_sync_places_what_sources_serve_before_it_stops_for_a_build() -> Result {
    let sandbox = Sandbox::new()?;
    let changed: &[u8] = b"sepal_length,species\n9.9,changed\n";
    let server = Server::start(
        PATH,
        vec![
            file(IRIS, "\"v1\""),
            file(IRIS, "\"v1\""),
            file(changed, "\"v2\""),
        ],
    )?;
    crate::build::with_recipe(&sandbox, &server)?;
    crate::build::pin_clean(&sandbox, crate::build::CODE, &[])?;
    let runner = fresh_with_recipe(&sandbox, "runner")?;
    let output = sandbox
        .command_in(&runner, &["--json", "sync", "--locked", "--yes"])
        .env("FETCHLOOM_CACHE", sandbox.folder("runner-cache")?)
        .output()?;
    let document = json(&output)?;
    assert_eq!(output.status.code(), Some(60), "{document}");
    assert_eq!(document["errors"][0]["kind"], "build.needed", "{document}");
    assert_eq!(document["result"]["placement"]["placed"], 1, "{document}");
    assert_eq!(
        document["result"]["placement"]["unbuilt"],
        serde_json::json!(["clean"]),
        "{document}"
    );
    assert_eq!(fs::read(runner.join("data/iris/iris.csv"))?, IRIS);
    let drifted = fresh_with_recipe(&sandbox, "drifted")?;
    let output = sandbox
        .command_in(&drifted, &["--json", "sync", "--locked", "--yes"])
        .env("FETCHLOOM_CACHE", sandbox.folder("drifted-cache")?)
        .output()?;
    let document = json(&output)?;
    assert_eq!(output.status.code(), Some(30), "{document}");
    assert_eq!(document["errors"][0]["kind"], "integrity.mismatch");
    Ok(())
}
