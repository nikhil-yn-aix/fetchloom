//! the project loop: add, sync, lock, status, path and remove, against servers on this machine.

use std::fs;

use crate::support::{
    Part, Result, Sandbox, Server, delete, edit, file, json, reply, text, wait_until,
};

const IRIS: &[u8] = b"sepal_length,species\n5.1,setosa\n";
const PATH: &str = "/files/iris.csv";

/// a project with iris added from `server`, whose first response serves it.
fn added(sandbox: &Sandbox, server: &Server) -> Result {
    let output = sandbox.run(&["add", &server.url])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    Ok(())
}

/// copies data.toml and data.lock into a new folder, as a fresh clone of the project would.
fn clone(sandbox: &Sandbox, name: &str) -> Result<std::path::PathBuf> {
    let folder = sandbox.folder(name)?;
    for file in ["data.toml", "data.lock"] {
        fs::copy(sandbox.work().join(file), folder.join(file))?;
    }
    Ok(folder)
}

#[test]
fn an_automatic_name_that_is_taken_gets_a_number() -> Result {
    let sandbox = Sandbox::new()?;
    let first = Server::start(PATH, vec![file(b"species\nsetosa\n", "\"a\"")])?;
    added(&sandbox, &first)?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    let output = sandbox.run(&["add", &server.url])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    let warning = "iris was taken, so this one is iris-2; choose a name with --as <name>";
    assert_eq!(stderr.matches(warning).count(), 1, "{stderr}");
    assert!(
        fs::read_to_string(sandbox.work().join("data.toml"))?.contains("[datasets.iris-2]"),
        "{stderr}"
    );
    assert!(sandbox.work().join("data/iris-2/iris.csv").is_file());
    let third = Server::start(PATH, vec![file(IRIS, "\"v2\""), file(IRIS, "\"v2\"")])?;
    let document = json(&sandbox.run(&["add", &third.url, "--dry-run", "--json"])?)?;
    assert_eq!(
        document["warnings"],
        serde_json::json!([
            "iris was taken, so this one is iris-3; choose a name with --as <name>"
        ])
    );
    Ok(())
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
    assert!(dataset.get("title").is_some(), "{dataset}");
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

#[test]
fn an_offline_add_of_a_new_link_says_it_must_be_fetched_once() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    let output = sandbox.run(&["--offline", "add", &server.url])?;
    assert_eq!(output.status.code(), Some(20));
    let stderr = text(&output.stderr)?;
    assert!(
        stderr.contains("a new link must be downloaded once to learn its fingerprint"),
        "{stderr}"
    );
    assert_eq!(server.requests().len(), 0, "offline sends nothing");
    Ok(())
}

#[test]
fn add_pins_downloads_and_places_a_link() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    let output = sandbox.run(&["add", &server.url])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    let work = sandbox.work();
    assert_eq!(
        fs::read_to_string(work.join("data.toml"))?,
        format!("[datasets.iris]\nref = \"{}\"\n", server.url)
    );
    assert_eq!(
        fs::read(work.join("data").join("iris").join("iris.csv"))?,
        IRIS
    );
    assert_eq!(server.requests().len(), 1, "one download, nothing else");
    let lock = fs::read_to_string(work.join("data.lock"))?;
    let name = sandbox.placed_story("add_story")?;
    let mut settings = sandbox.filters();
    settings.add_filter(r#"spec = "sha256:[0-9a-f]{64}""#, r#"spec = "[spec]""#);
    settings.add_filter(r#"locked = "[0-9T:-]+Z""#, r#"locked = "[time]""#);
    settings.bind(|| {
        insta::assert_snapshot!("add_lock", lock);
        insta::assert_snapshot!(name, stderr);
    });
    Ok(())
}

#[test]
fn no_hints_keeps_the_story_and_drops_the_hints() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    let output = sandbox
        .command(&["add", &server.url])
        .env("FETCHLOOM_NO_HINTS", "1")
        .output()?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(stderr.contains("Added iris"), "{stderr}");
    assert!(!stderr.contains("hint:"), "{stderr}");
    Ok(())
}

/// after a change, a person at a terminal is told the usual next step. a pipe, `-q` and `--json`
/// never are, so what scripts read stays the same.
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
fn adding_the_same_dataset_again_changes_nothing() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let before = fs::read(sandbox.work().join("data.toml"))?;
    let output = sandbox.run(&["add", &server.url])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert_eq!(
        stderr,
        "         Ok iris is already in data.toml, its files are in data/iris\n"
    );
    assert_eq!(fs::read(sandbox.work().join("data.toml"))?, before);
    assert_eq!(server.requests().len(), 1, "nothing is downloaded twice");
    Ok(())
}

#[test]
fn adding_it_again_with_other_patterns_names_the_dataset_it_matches() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let output = sandbox.run(&["add", &server.url, "--select", "*.csv"])?;
    assert_eq!(output.status.code(), Some(2));
    let stderr = text(&output.stderr)?;
    sandbox.filters().bind(|| insta::assert_snapshot!(stderr));
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
        text(&status.stderr)?.contains("Edited data/iris/iris.csv"),
        "{}",
        text(&status.stderr)?
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

/// a hardlink is the cache's own copy, which every project placing the file shares, so an edit
/// made in place through it is damage to that copy: status says so and sync puts the pinned file
/// back. a reflink or a copy is the project's own, and its edit stays.
#[test]
fn an_edit_in_place_through_a_shared_link_is_put_back() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), file(IRIS, "\"v1\"")])?;
    let added = json(&sandbox.run(&["--json", "add", &server.url])?)?;
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    edit(&placed, b"changed in place\n")?;
    let status = text(&sandbox.run(&["status"])?.stderr)?;
    let synced = json(&sandbox.run(&["--json", "sync"])?)?;
    let placement = &synced["result"]["placement"];
    if added["work"]["hardlinks"] == 1 {
        assert!(
            status.contains(
                "Damaged data/iris/iris.csv · linked to the cache's copy, which was changed"
            ),
            "{status}"
        );
        assert_eq!(
            placement["restored"],
            serde_json::json!(["data/iris/iris.csv"])
        );
        assert_eq!(fs::read(&placed)?, IRIS);
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

#[test]
fn remove_drops_the_dataset_and_only_its_files() -> Result {
    let sandbox = Sandbox::new()?;
    let first = Server::start("/a.csv", vec![file(b"a\n", "\"a\"")])?;
    let second = Server::start("/b.csv", vec![file(b"b\n", "\"b\"")])?;
    added(&sandbox, &first)?;
    added(&sandbox, &second)?;
    let output = sandbox.run(&["remove", "a"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    let work = sandbox.work();
    assert_eq!(
        fs::read_to_string(work.join("data.toml"))?,
        format!("[datasets.b]\nref = \"{}\"\n", second.url)
    );
    let lock = fs::read_to_string(work.join("data.lock"))?;
    assert!(
        !lock.contains("name = \"a\"") && lock.contains("name = \"b\""),
        "{lock}"
    );
    assert!(
        !work.join("data").join("a").exists(),
        "the empty folder goes too"
    );
    assert_eq!(fs::read(work.join("data").join("b").join("b.csv"))?, b"b\n");
    let unknown = sandbox.run(&["remove", "a"])?;
    assert_eq!(unknown.status.code(), Some(2));
    Ok(())
}

#[test]
fn changed_bytes_upstream_stop_sync_with_the_fix() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(
        PATH,
        vec![file(IRIS, "\"v1\""), file(b"species\nsetosa\n", "\"v2\"")],
    )?;
    added(&sandbox, &server)?;
    let cold = clone(&sandbox, "cold")?;
    let output = sandbox
        .command_in(&cold, &["sync"])
        .env("FETCHLOOM_CACHE", sandbox.folder("other-cache")?)
        .output()?;
    assert_eq!(output.status.code(), Some(30));
    assert!(
        !cold.join("data").join("iris").join("iris.csv").exists(),
        "nothing unverified is placed"
    );
    let stderr = text(&output.stderr)?;
    sandbox.filters().bind(|| insta::assert_snapshot!(stderr));
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
        stderr.contains("is not the file that was locked"),
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
fn path_prints_where_a_dataset_or_file_is() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let folder = sandbox.work().join("data").join("iris");
    let output = sandbox.run(&["path", "iris"])?;
    assert_eq!(text(&output.stdout)?, format!("{}\n", folder.display()));
    assert!(output.stderr.is_empty(), "{}", text(&output.stderr)?);
    let file = sandbox.run(&["path", "iris/iris.csv"])?;
    assert_eq!(
        text(&file.stdout)?,
        format!("{}\n", folder.join("iris.csv").display())
    );
    let typo = sandbox.run(&["path", "irs"])?;
    assert_eq!(typo.status.code(), Some(2));
    assert!(
        text(&typo.stderr)?.contains("did you mean `fetchloom path iris`?"),
        "{}",
        text(&typo.stderr)?
    );
    let swapped = sandbox.run(&["path", "irsi"])?;
    assert!(
        text(&swapped.stderr)?.contains("did you mean `fetchloom path iris`?"),
        "{}",
        text(&swapped.stderr)?
    );
    Ok(())
}

#[test]
fn path_answers_a_file_a_dataset_lacks_with_paths_it_holds() -> Result {
    let sandbox = Sandbox::new()?;
    let zip = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("archives")
        .join("python.zip");
    let server = Server::start("/files/dataset.zip", vec![file(&fs::read(zip)?, "\"z1\"")])?;
    added(&sandbox, &server)?;
    let typo = sandbox.run(&["path", "dataset/data/trian.csv"])?;
    assert_eq!(typo.status.code(), Some(2));
    assert!(
        text(&typo.stderr)?.contains("did you mean `fetchloom path dataset/data/train.csv`?"),
        "{}",
        text(&typo.stderr)?
    );
    let unknown = sandbox.run(&["path", "dataset/not-a-file.csv"])?;
    assert_eq!(unknown.status.code(), Some(2));
    let stderr = text(&unknown.stderr)?;
    assert!(!stderr.contains("fetchloom status"), "{stderr}");
    insta::assert_snapshot!(stderr);
    Ok(())
}

#[test]
fn path_syncs_first_when_the_files_are_not_there() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let clone = clone(&sandbox, "fresh")?;
    let output = sandbox.command_in(&clone, &["path", "iris"]).output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    assert_eq!(
        fs::read(clone.join("data").join("iris").join("iris.csv"))?,
        IRIS
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

#[test]
fn adding_again_after_git_init_ignores_the_data_folder() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), file(IRIS, "\"v1\"")])?;
    assert!(sandbox.run(&["add", &server.url])?.status.success());
    assert!(!sandbox.work().join(".gitignore").exists());
    fs::create_dir(sandbox.work().join(".git"))?;
    let output = sandbox.run(&["add", &server.url])?;
    let stderr = text(&output.stderr)?;
    assert!(output.status.success(), "{stderr}");
    assert!(stderr.contains("Ignored data/ in .gitignore"), "{stderr}");
    assert!(stderr.contains("iris is already in data.toml"), "{stderr}");
    assert_eq!(
        fs::read_to_string(sandbox.work().join(".gitignore"))?,
        "/data/\n"
    );
    Ok(())
}

#[test]
fn a_pattern_that_matches_nothing_explains_why_and_changes_nothing() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    let output = sandbox.run(&["add", &server.url, "--select", "*.txt"])?;
    assert_eq!(output.status.code(), Some(2), "the command line was wrong");
    assert!(
        !sandbox.work().join("data.toml").exists(),
        "a failed add leaves no data.toml behind"
    );
    let stderr = text(&output.stderr)?;
    sandbox.filters().bind(|| insta::assert_snapshot!(stderr));
    Ok(())
}

#[test]
fn status_lists_what_differs() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let folder = sandbox.work().join("data").join("iris");
    assert_eq!(
        text(&sandbox.run(&["status"])?.stderr)?,
        "         Ok everything matches data.lock by size and time\n"
    );
    delete(&folder.join("iris.csv"))?;
    fs::write(folder.join("notes.txt"), "mine")?;
    let output = sandbox.run(&["status"])?;
    assert_eq!(output.status.code(), Some(0));
    let stderr = text(&output.stderr)?;
    sandbox.filters().bind(|| insta::assert_snapshot!(stderr));
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
        stderr.contains("changed and was marked for replacement"),
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

#[test]
fn remove_on_a_fresh_clone_downloads_nothing() -> Result {
    let sandbox = Sandbox::new()?;
    let iris = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    let cows = Server::start(
        "/files/cows.csv",
        vec![file(b"cow,sound\nbess,moo\n", "\"c1\"")],
    )?;
    added(&sandbox, &iris)?;
    added(&sandbox, &cows)?;
    let clone = clone(&sandbox, "clone")?;
    let cold = sandbox.folder("cold")?;
    let output = sandbox
        .command_in(&clone, &["remove", "cows"])
        .env("FETCHLOOM_CACHE", &cold)
        .output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    assert_eq!(iris.requests().len(), 1, "remove fetches nothing");
    assert!(!fs::read_to_string(clone.join("data.toml"))?.contains("cows"));
    Ok(())
}

#[test]
fn a_second_project_adding_the_same_link_moves_no_body() -> Result {
    let sandbox = Sandbox::new()?;
    let unchanged = reply("304 Not Modified", &[("ETag", "\"v1\"")], b"");
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), unchanged])?;
    added(&sandbox, &server)?;
    let other = sandbox.folder("other")?;
    let output = sandbox
        .command_in(&other, &["--json", "add", &server.url])
        .output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    assert_eq!(json(&output)?["work"]["bytes_down"], 0);
    let asked = &server.requests()[1];
    assert!(asked.contains("if-none-match: \"v1\""), "{asked}");
    assert_eq!(
        fs::read(other.join("data").join("iris").join("iris.csv"))?,
        IRIS
    );
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
fn duplicate_add_chooses_the_entry_with_matching_settings() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, (0..4).map(|_| file(IRIS, "\"v1\"")).collect())?;
    let manifest = format!(
        "[datasets.a-extracted]\nref = \"{url}\"\n\n[datasets.b-selected]\nref = \"{url}\"\nextract = false\nselect = \"*.csv\"\n\n[datasets.c-excluded]\nref = \"{url}\"\nextract = false\nexclude = \"ignored.*\"\n\n[datasets.z-packed]\nref = \"{url}\"\nextract = false\n",
        url = server.url
    );
    fs::write(sandbox.work().join("data.toml"), &manifest)?;
    let synced = sandbox.run(&["sync"])?;
    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    let locked = fs::read(sandbox.work().join("data.lock"))?;
    let requests = server.requests().len();
    let output = sandbox.run(&["add", &server.url, "--no-extract"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert_eq!(
        stderr,
        "         Ok z-packed is already in data.toml, its files are in data/z-packed\n"
    );
    assert_eq!(
        fs::read_to_string(sandbox.work().join("data.toml"))?,
        manifest
    );
    assert_eq!(fs::read(sandbox.work().join("data.lock"))?, locked);
    assert_eq!(server.requests().len(), requests);
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

/// data.toml turned into a folder while a first add downloads is a change the add keeps: it
/// writes neither file, says so to a person and in json, and takes back the `.fetchloom` it made.
#[test]
fn a_first_add_whose_data_toml_turns_into_a_folder_writes_nothing() -> Result {
    for machine in [false, true] {
        let sandbox = Sandbox::new()?;
        let manifest = sandbox.work().join("data.toml");
        let blocked = manifest.clone();
        let mut response = vec![crate::support::Part::Action(Box::new(move || {
            fs::create_dir(&blocked).unwrap();
        }))];
        response.extend(file(IRIS, "\"v1\""));
        let server = Server::start(PATH, vec![response])?;
        let mut args = vec!["add", server.url.as_str(), "--as", "iris"];
        if machine {
            args.push("--json");
        }
        let output = sandbox.run(&args)?;
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(50), "{stderr}");
        if machine {
            assert_eq!(json(&output)?["errors"][0]["kind"], "project.changed");
        } else {
            assert!(
                stderr.contains("data.toml changed while the add was downloading"),
                "{stderr}"
            );
        }
        assert!(manifest.is_dir(), "{stderr}");
        assert_eq!(listed(&sandbox.work())?, ["data.toml"], "{stderr}");
    }
    Ok(())
}

#[test]
fn remove_checks_cache_and_plan_before_changing_metadata() -> Result {
    for blocked_cache in [true, false] {
        let sandbox = Sandbox::new()?;
        let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
        added(&sandbox, &server)?;
        let manifest = sandbox.work().join("data.toml");
        let lock = sandbox.work().join("data.lock");
        let blocker = sandbox.work().join("blocked");
        fs::write(&blocker, b"not a directory")?;
        if !blocked_cache {
            let text = fs::read_to_string(&manifest)?;
            fs::write(
                &manifest,
                format!("[project]\ndata_dir = \"blocked\"\n\n{text}"),
            )?;
        }
        let before = (fs::read(&manifest)?, fs::read(&lock)?);
        let mut command = sandbox.command(&["--json", "remove", "iris"]);
        if blocked_cache {
            command.env("FETCHLOOM_CACHE", &blocker);
        }
        let output = command.output()?;
        assert_eq!(output.status.code(), Some(50), "{}", text(&output.stdout)?);
        assert_eq!(fs::read(&manifest)?, before.0);
        assert_eq!(fs::read(&lock)?, before.1);
    }
    Ok(())
}

#[cfg(windows)]
#[test]
fn remove_explains_how_to_finish_after_placement_fails() -> Result {
    use std::os::windows::fs::OpenOptionsExt as _;

    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let path = sandbox.work().join("data").join("iris").join("iris.csv");
    let held = fs::OpenOptions::new()
        .read(true)
        .share_mode(3)
        .open(&path)?;
    let output = sandbox.run(&["--json", "remove", "iris"])?;
    assert_eq!(output.status.code(), Some(50));
    let error = json(&output)?;
    let details = error["errors"][0]["details"].to_string();
    assert!(details.contains("fetchloom sync"), "{details}");
    assert!(details.contains("finishes the removal"), "{details}");
    assert!(!fs::read_to_string(sandbox.work().join("data.toml"))?.contains("datasets.iris"));
    drop(held);
    let synced = sandbox.run(&["sync"])?;
    assert!(synced.status.success(), "{}", text(&synced.stderr)?);
    assert!(!path.exists());
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
    let stderr = text(&output.stderr)?;
    assert!(stderr.contains("Removed"), "{stderr}");
    assert!(stderr.contains("iris from data.toml"), "{stderr}");
    assert!(stderr.contains("fetchloom sync"), "{stderr}");
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
fn path_rejects_an_impossible_locked_member_before_syncing() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    for cold in [false, true] {
        let clone = clone(&sandbox, if cold { "cold-clone" } else { "warm-clone" })?;
        let mut command =
            sandbox.command_in(&clone, &["--json", "--offline", "path", "iris/no.csv"]);
        if cold {
            command.env("FETCHLOOM_CACHE", sandbox.folder("cold-cache")?);
        }
        let output = command.output()?;
        assert_eq!(output.status.code(), Some(2), "{}", text(&output.stdout)?);
        let report = json(&output)?;
        assert_eq!(report["errors"][0]["kind"], "usage");
        assert_eq!(report["work"]["requests"], 0);
        assert!(
            !clone.join("data").exists(),
            "an impossible member must not place data"
        );
        assert!(
            !clone.join(".fetchloom").exists(),
            "an impossible member must not sync"
        );
    }
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
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(stderr.contains("Unlocked new"), "{stderr}");
    assert!(stderr.contains("Removed old from data.toml"), "{stderr}");
    assert!(stderr.contains("Missing data/set/file19.csv"), "{stderr}");
    assert!(!stderr.contains("file20.csv"), "{stderr}");
    assert!(stderr.contains("Extra data/extra19.txt"), "{stderr}");
    assert!(!stderr.contains("extra20.txt"), "{stderr}");
    assert_eq!(stderr.matches("and 5 more").count(), 2);
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
fn failed_add_restores_both_project_files_after_writing_the_lock() -> Result {
    for existing in [false, true] {
        let sandbox = Sandbox::new()?;
        if existing {
            let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
            added(&sandbox, &server)?;
            let path = sandbox.work().join("data.lock");
            let locked = fs::read_to_string(&path)?;
            fs::write(path, format!("# keep lock formatting\n{locked}"))?;
        }
        let before_manifest = fs::read(sandbox.work().join("data.toml")).ok();
        let before_lock = fs::read(sandbox.work().join("data.lock")).ok();
        let data = sandbox.work().join("data");
        let mut response = vec![crate::support::Part::Action(Box::new(move || {
            fs::create_dir_all(&data).unwrap();
            fs::write(data.join("experiment"), b"block placement after locking").unwrap();
        }))];
        response.extend(file(IRIS, "\"v2\""));
        let server = Server::start("/experiment.csv", vec![response])?;
        let output = sandbox.run(&["add", &server.url, "--as", "experiment"])?;
        let stderr = text(&output.stderr)?;
        assert!(!output.status.success(), "{stderr}");
        assert_eq!(
            fs::read(sandbox.work().join("data.toml")).ok(),
            before_manifest
        );
        assert_eq!(fs::read(sandbox.work().join("data.lock")).ok(), before_lock);
        assert!(
            stderr.contains("restored data.toml and data.lock"),
            "{stderr}"
        );
        if existing {
            let status = sandbox.run(&["status"])?;
            assert!(!text(&status.stderr)?.contains("Removed experiment"));
        }
    }
    Ok(())
}

/// data.lock turned into a folder while an add downloads is a change the add keeps: it writes
/// neither data.toml nor data.lock, so it needs no recovery copy.
#[test]
fn an_add_whose_data_lock_turns_into_a_folder_writes_nothing() -> Result {
    let sandbox = Sandbox::new()?;
    let initial = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &initial)?;
    let lock = sandbox.work().join("data.lock");
    let before_manifest = fs::read(sandbox.work().join("data.toml"))?;
    let blocked = lock.clone();
    let mut response = vec![crate::support::Part::Action(Box::new(move || {
        fs::remove_file(&blocked).unwrap();
        fs::create_dir(&blocked).unwrap();
    }))];
    response.extend(file(b"experiment\n", "\"e1\""));
    let server = Server::start("/experiment.csv", vec![response])?;
    let output = sandbox.run(&["add", &server.url, "--as", "experiment"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(50), "{stderr}");
    assert!(
        stderr.contains("data.lock changed while the add was downloading"),
        "{stderr}"
    );
    assert_eq!(fs::read(sandbox.work().join("data.toml"))?, before_manifest);
    assert!(lock.is_dir(), "{stderr}");
    let copies = fs::read_dir(sandbox.work().join(".fetchloom"))?
        .collect::<std::io::Result<Vec<_>>>()?
        .into_iter()
        .filter(|entry| entry.file_name().to_string_lossy().contains(".recovery."))
        .count();
    assert_eq!(copies, 0, "{stderr}");
    Ok(())
}

#[test]
fn adding_a_previous_source_does_not_match_an_edited_reference() -> Result {
    let sandbox = Sandbox::new()?;
    let first = Server::start(
        "/old.csv",
        vec![
            file(b"old\n", "\"old\""),
            reply("304 Not Modified", &[("ETag", "\"old\"")], b""),
        ],
    )?;
    let second = Server::start("/new.csv", vec![file(b"new\n", "\"new\"")])?;
    let added = sandbox.run(&["add", &first.url, "--as", "existing"])?;
    assert!(added.status.success(), "{}", text(&added.stderr)?);
    let manifest = sandbox.work().join("data.toml");
    fs::write(
        &manifest,
        fs::read_to_string(&manifest)?.replace(&first.url, &second.url),
    )?;

    let output = sandbox.run(&["--json", "add", &first.url])?;
    assert!(output.status.success(), "{}", text(&output.stdout)?);
    let document = json(&output)?;
    assert_eq!(document["result"]["datasets"], serde_json::json!(["old"]));
    let updated = fs::read_to_string(&manifest)?;
    assert!(updated.contains("[datasets.existing]"), "{updated}");
    assert!(updated.contains("[datasets.old]"), "{updated}");
    assert!(updated.contains(&first.url) && updated.contains(&second.url));
    assert_eq!(fs::read(sandbox.work().join("data/old/old.csv"))?, b"old\n");
    assert_eq!(
        fs::read(sandbox.work().join("data/existing/new.csv"))?,
        b"new\n"
    );
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
fn completed_adds_and_rollbacks_remove_recovery_copies() -> Result {
    let sandbox = Sandbox::new()?;
    let first = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &first)?;
    for (name, response, success) in [
        ("second", file(b"second\n", "\"v2\""), true),
        ("denied", reply("403 Forbidden", &[], b"denied"), false),
    ] {
        let server = Server::start("/next.csv", vec![response])?;
        let output = sandbox.run(&["add", &server.url, "--as", name])?;
        assert_eq!(
            output.status.success(),
            success,
            "{}",
            text(&output.stderr)?
        );
        for entry in fs::read_dir(sandbox.work().join(".fetchloom"))? {
            assert!(!entry?.file_name().to_string_lossy().contains(".recovery."));
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
fn ready_paths_read_and_plan_once() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let trace = sandbox.work().join("path.trace.json");
    for target in ["iris", "iris/iris.csv"] {
        let output = sandbox
            .command(&["--offline", "path", target])
            .env("FETCHLOOM_TRACE", &trace)
            .output()?;
        assert!(output.status.success(), "{}", text(&output.stderr)?);
        let events: Vec<serde_json::Value> = serde_json::from_slice(&fs::read(&trace)?)?;
        for name in ["read_manifest", "read_lock", "plan", "open_cache", "scan"] {
            let calls = events
                .iter()
                .filter(|event| event["ph"] == "B" && event["name"] == name)
                .count();
            assert_eq!(calls, 1, "{target}: {name}");
        }
    }
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

/// a name given with `--as` is refused for its real reason, with the limit it breaks, and the
/// suggested name keeps the letters it can and is one the next run accepts.
#[test]
fn a_refused_name_says_why_and_suggests_one_that_works() -> Result {
    let sandbox = Sandbox::new()?;
    let long = "x".repeat(300);
    let short = "x".repeat(40);
    for (asked, reason, suggested) in [
        (
            long.as_str(),
            "it has 300 characters, and the most is 64",
            short.as_str(),
        ),
        ("ïris-数据", "it holds ï, 数 and 据", "iris"),
        ("CON", "it holds capital letters", "con-data"),
        ("my iris", "it holds a space", "my-iris"),
    ] {
        let output = sandbox.run(&[
            "--json",
            "add",
            "https://example.org/iris.csv",
            "--as",
            asked,
        ])?;
        assert_eq!(output.status.code(), Some(2), "{asked}");
        let document = json(&output)?;
        let error = &document["errors"][0];
        let message = error["message"].as_str().unwrap_or_default();
        let help = error["help"].as_str().unwrap_or_default();
        assert_eq!(error["details"], serde_json::json!([reason]), "{asked}");
        assert!(
            help.ends_with(&format!("--as {suggested}")),
            "{asked}: {help}"
        );
        assert!(message.len() < 120, "{asked}: {message}");
        let retried = sandbox.run(&[
            "--json",
            "add",
            "--dry-run",
            "--offline",
            "https://example.org/iris.csv",
            "--as",
            suggested,
        ])?;
        let kind = json(&retried)?["errors"][0]["kind"].clone();
        assert_ne!(kind, "usage", "{suggested} is refused too");
    }
    assert!(!sandbox.work().join("data.toml").exists());
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
    let hens = Server::start("/files/hens.csv", vec![file(b"hen\n", "\"h\"")])?;
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

/// an add into a project folder that is read-only names that folder, claims no rollback for files
/// it never changed, and leaves no recovery copies behind.
#[cfg(unix)]
#[test]
fn an_add_into_a_read_only_project_folder_names_it_and_leaves_no_copies() -> Result {
    use std::os::unix::fs::PermissionsExt;
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let work = sandbox.work();
    let before = (
        fs::read(work.join("data.toml"))?,
        fs::read(work.join("data.lock"))?,
    );
    let other = Server::start("/files/cows.csv", vec![file(b"cow\n", "\"c\"")])?;
    fs::set_permissions(&work, fs::Permissions::from_mode(0o555))?;
    // a user who may write anywhere, such as root, cannot show this
    if fs::write(work.join("probe"), b"").is_ok() {
        fs::remove_file(work.join("probe"))?;
        fs::set_permissions(&work, fs::Permissions::from_mode(0o755))?;
        return Ok(());
    }
    let output = sandbox.run(&["add", &other.url]);
    fs::set_permissions(&work, fs::Permissions::from_mode(0o755))?;
    let output = output?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(50), "{stderr}");
    assert!(
        stderr.contains(&format!("{} is read-only", work.display())),
        "{stderr}"
    );
    assert!(!stderr.contains("restore"), "{stderr}");
    assert!(!stderr.contains("rolling back"), "{stderr}");
    let copies: Vec<String> = fs::read_dir(work.join(".fetchloom"))?
        .map(|entry| Ok(entry?.file_name().to_string_lossy().into_owned()))
        .collect::<std::io::Result<_>>()?;
    assert!(
        !copies.iter().any(|name| name.contains(".recovery.")),
        "{copies:?}"
    );
    assert_eq!(
        (
            fs::read(work.join("data.toml"))?,
            fs::read(work.join("data.lock"))?
        ),
        before
    );
    Ok(())
}

/// what is in `folder`, by name.
fn listed(folder: &std::path::Path) -> Result<Vec<String>> {
    let mut names = fs::read_dir(folder)?
        .map(|entry| Ok(entry?.file_name().to_string_lossy().into_owned()))
        .collect::<std::io::Result<Vec<_>>>()?;
    names.sort();
    Ok(names)
}

/// a server that sends the head of `body` and its first `sent` bytes, then holds the rest until
/// the returned sender is dropped, as a download still running.
fn held(path: &str, body: &[u8], sent: usize) -> Result<(Server, std::sync::mpsc::Sender<()>)> {
    let (release, wait) = std::sync::mpsc::channel();
    let length = body.len().to_string();
    let mut response = reply(
        "200 OK",
        &[("Content-Length", &length), ("ETag", "\"held\"")],
        &body[..sent],
    );
    response.push(Part::Wait(wait));
    response.push(Part::Bytes(body[sent..].to_vec()));
    Ok((Server::start(path, vec![response])?, release))
}

/// a command running in the background, with its stderr read as it comes.
struct Background {
    child: std::process::Child,
    stderr: std::thread::JoinHandle<String>,
}

impl Background {
    fn start(mut command: std::process::Command) -> Result<Background> {
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
    fn interrupt(&self) -> Result {
        let sent = std::process::Command::new("kill")
            .args(["-INT", &self.child.id().to_string()])
            .status()?;
        if sent.success() {
            Ok(())
        } else {
            Err("kill -INT failed".into())
        }
    }

    fn finish(mut self) -> Result<(Option<i32>, String)> {
        let status = self.child.wait()?;
        let stderr = self.stderr.join().map_err(|_| "reading stderr failed")?;
        Ok((status.code(), stderr))
    }
}

/// the sizes of the partial downloads in `cache`.
#[cfg(unix)]
fn partial_bytes(cache: &std::path::Path) -> u64 {
    fs::read_dir(cache.join("partial")).map_or(0, |entries| {
        entries
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".part"))
            .filter_map(|entry| entry.metadata().ok())
            .map(|metadata| metadata.len())
            .sum()
    })
}

/// an add cut off while it downloads, as by a second ctrl-c, a closed terminal or a killed job,
/// has not changed data.toml or data.lock: it writes them only once every byte is in the cache.
#[test]
fn an_add_cut_off_while_it_downloads_leaves_the_project_as_it_was() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let work = sandbox.work();
    let before = (
        fs::read_to_string(work.join("data.toml"))?,
        fs::read_to_string(work.join("data.lock"))?,
    );
    let body = vec![b'x'; 1 << 20];
    let (cows, release) = held("/files/cows.csv", &body, 1 << 16)?;
    let mut running = Background::start(sandbox.command(&["add", &cows.url, "--as", "cows"]))?;
    wait_until("the download to start", || Ok(!cows.requests().is_empty()))?;
    running.child.kill()?;
    let (_, stderr) = running.finish()?;
    drop(release);
    assert_eq!(
        (
            fs::read_to_string(work.join("data.toml"))?,
            fs::read_to_string(work.join("data.lock"))?
        ),
        before,
        "{stderr}"
    );
    Ok(())
}

/// an edit to data.toml made while an add downloads stays: the add writes neither data.toml nor
/// data.lock, says which file changed, and keeps what it downloaded in the cache.
#[test]
fn an_add_keeps_an_edit_made_while_it_downloads_and_writes_nothing() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let work = sandbox.work();
    let lock = fs::read(work.join("data.lock"))?;
    let body: Vec<u8> = (0..1u32 << 20).map(|n| (n % 251) as u8).collect();
    let (cows, release) = held("/files/cows.csv", &body, 1 << 16)?;
    let running = Background::start(sandbox.command(&["-v", "add", &cows.url, "--as", "cows"]))?;
    wait_until("the download to start", || Ok(!cows.requests().is_empty()))?;
    let edited = format!(
        "{}# a note made meanwhile\n",
        fs::read_to_string(work.join("data.toml"))?
    );
    fs::write(work.join("data.toml"), &edited)?;
    drop(release);
    let (code, stderr) = running.finish()?;
    assert_eq!(code, Some(50), "{stderr}");
    assert!(
        stderr.contains("data.toml changed while the add was downloading")
            && stderr.contains("kind: project.changed"),
        "{stderr}"
    );
    assert_eq!(fs::read_to_string(work.join("data.toml"))?, edited);
    assert_eq!(fs::read(work.join("data.lock"))?, lock, "{stderr}");
    assert!(!work.join("data/cows").exists(), "{stderr}");
    let digest = sha256_hex(&body);
    assert!(
        sandbox
            .cache()
            .join("objects")
            .join(&digest[..2])
            .join(&digest[2..])
            .is_file(),
        "{stderr}"
    );
    Ok(())
}

/// ctrl-c during an add whose data.toml someone edited meanwhile says that nothing was added,
/// and keeps the edit: the add never wrote either file, so there is nothing it failed to put back.
#[cfg(unix)]
#[test]
fn a_stopped_add_keeps_an_edit_made_meanwhile_and_says_nothing_was_added() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let work = sandbox.work();
    let body: Vec<u8> = (0..1u32 << 20).map(|n| (n % 251) as u8).collect();
    let slow = Server::slow_file("/files/cows.csv", body)?;
    let running = Background::start(sandbox.command(&["add", &slow.url, "--as", "cows"]))?;
    wait_until(
        "bytes to arrive",
        || Ok(partial_bytes(&sandbox.cache()) > 0),
    )?;
    let edited = format!(
        "{}# a note made meanwhile\n",
        fs::read_to_string(work.join("data.toml"))?
    );
    fs::write(work.join("data.toml"), &edited)?;
    running.interrupt()?;
    let (code, stderr) = running.finish()?;
    assert_eq!(code, Some(130), "{stderr}");
    assert!(
        stderr.contains("nothing was added") && !stderr.contains("could not be put back"),
        "{stderr}"
    );
    assert_eq!(fs::read_to_string(work.join("data.toml"))?, edited);
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest as _, Sha256};
    use std::fmt::Write as _;
    let mut text = String::with_capacity(64);
    for byte in Sha256::digest(bytes) {
        let _ = write!(text, "{byte:02x}");
    }
    text
}

/// ctrl-c during an add's download ends it with the project as it was, says so, and keeps what
/// arrived in the cache, so the same add continues from there.
#[cfg(unix)]
#[test]
fn a_stopped_add_says_the_project_is_as_it_was_and_a_rerun_continues() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let work = sandbox.work();
    let before = (
        fs::read(work.join("data.toml"))?,
        fs::read(work.join("data.lock"))?,
    );
    let body: Vec<u8> = (0..1u32 << 20).map(|n| (n % 251) as u8).collect();
    let slow = Server::slow_file("/files/cows.csv", body.clone())?;
    let running = Background::start(sandbox.command(&["add", &slow.url, "--as", "cows"]))?;
    wait_until(
        "bytes to arrive",
        || Ok(partial_bytes(&sandbox.cache()) > 0),
    )?;
    running.interrupt()?;
    let (code, stderr) = running.finish()?;
    assert_eq!(code, Some(130), "{stderr}");
    assert!(
        stderr.contains("Stopped")
            && stderr.contains("the add left data.toml and data.lock unchanged"),
        "{stderr}"
    );
    assert_eq!(
        (
            fs::read(work.join("data.toml"))?,
            fs::read(work.join("data.lock"))?
        ),
        before,
        "{stderr}"
    );
    let rerun = sandbox.run(&["add", &slow.url, "--as", "cows"])?;
    assert_eq!(rerun.status.code(), Some(0), "{}", text(&rerun.stderr)?);
    assert_eq!(fs::read(work.join("data/cows/cows.csv"))?, body);
    let requests = slow.requests();
    assert!(
        requests.len() == 2 && requests[1].contains("range: bytes="),
        "{requests:?}"
    );
    Ok(())
}

/// a first add that fails or is refused leaves its folder as it was: no `.fetchloom`, no data
/// folder and no data.toml or data.lock.
#[test]
fn a_first_add_that_fails_leaves_the_folder_as_it_was() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![reply("403 Forbidden", &[], b"denied")])?;
    let output = sandbox.run(&["add", &server.url])?;
    assert!(!output.status.success());
    assert_eq!(
        listed(&sandbox.work())?,
        Vec::<String>::new(),
        "{}",
        text(&output.stderr)?
    );
    let named = sandbox.run(&["add", &server.url, "--as", "Not A Name"])?;
    assert_eq!(named.status.code(), Some(2));
    assert_eq!(listed(&sandbox.work())?, Vec::<String>::new());
    Ok(())
}

#[cfg(debug_assertions)]
const SEARCHED: [(&str, &[u8]); 3] = [
    (
        "FETCHLOOM_TEST_HF_API",
        include_bytes!("../fixtures/sources/search/hf.json"),
    ),
    (
        "FETCHLOOM_TEST_KAGGLE_API",
        include_bytes!("../fixtures/sources/search/kaggle.json"),
    ),
    (
        "FETCHLOOM_TEST_ZENODO_API",
        include_bytes!("../fixtures/sources/search/zenodo.json"),
    ),
];

/// a first add of search words that ends without a dataset, because nobody can choose one here
/// or because ctrl-c stopped the search, leaves its folder as it was.
#[cfg(debug_assertions)]
#[test]
fn a_first_add_whose_search_ends_without_a_choice_leaves_nothing() -> Result {
    let sandbox = Sandbox::new()?;
    let mut catalogs = Vec::new();
    let mut command = sandbox.command(&["add", "iris"]);
    for (variable, body) in SEARCHED {
        let catalog = Server::start(
            "/",
            vec![reply(
                "200 OK",
                &[("Content-Type", "application/json")],
                body,
            )],
        )?;
        command.env(variable, &catalog.url);
        catalogs.push(catalog);
    }
    let output = command.output()?;
    assert_eq!(output.status.code(), Some(10), "{}", text(&output.stderr)?);
    assert_eq!(listed(&sandbox.work())?, Vec::<String>::new());
    #[cfg(unix)]
    {
        let (release, wait) = std::sync::mpsc::channel();
        let stalled = Server::start("/", vec![vec![Part::Wait(wait)]])?;
        let mut command = sandbox.command(&["add", "iris"]);
        for (variable, _) in SEARCHED {
            command.env(variable, &stalled.url);
        }
        let running = Background::start(command)?;
        wait_until("the search to start", || Ok(!stalled.requests().is_empty()))?;
        running.interrupt()?;
        let (code, stderr) = running.finish()?;
        drop(release);
        assert_eq!(code, Some(130), "{stderr}");
        assert_eq!(listed(&sandbox.work())?, Vec::<String>::new(), "{stderr}");
    }
    Ok(())
}

/// the first add says it started a project, and where. in the home folder, where a project would
/// take in every folder below, it asks first, and with nobody to ask it refuses unless --yes.
#[test]
fn a_first_add_says_where_it_started_a_project_and_asks_in_the_home_folder() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), file(IRIS, "\"v1\"")])?;
    let output = sandbox.run(&["add", &server.url])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains(&format!(
            "    Started a project in {}\n",
            sandbox.work().display()
        )),
        "{stderr}"
    );
    let again = sandbox.run(&["add", &server.url])?;
    assert!(!text(&again.stderr)?.contains("Started"));
    let home = sandbox.folder("home")?;
    let refused = sandbox.command_in(&home, &["add", &server.url]).output()?;
    let stderr = text(&refused.stderr)?;
    assert_eq!(refused.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("your home folder"), "{stderr}");
    assert!(stderr.contains("mkdir"), "{stderr}");
    assert_eq!(listed(&home)?, Vec::<String>::new());
    let approved = sandbox
        .command_in(&home, &["--yes", "add", &server.url])
        .output()?;
    assert_eq!(
        approved.status.code(),
        Some(0),
        "{}",
        text(&approved.stderr)?
    );
    assert!(home.join("data.toml").is_file());
    Ok(())
}

/// an add run in a folder below its project names the project once, for a person at a terminal,
/// and shows where the files are from where the person stands.
#[test]
fn an_add_from_a_subfolder_names_the_project_and_shows_paths_from_there() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let below = sandbox.work().join("notebooks").join("week1");
    fs::create_dir_all(&below)?;
    let again = sandbox.command_in(&below, &["add", &server.url]).output()?;
    let stderr = text(&again.stderr)?;
    assert_eq!(again.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("Ok iris is already in data.toml, its files are in ../../data/iris"),
        "{stderr}"
    );
    let cows = Server::start("/files/cows.csv", vec![file(b"cow\n", "\"c\"")])?;
    let added_cows = sandbox.command_in(&below, &["add", &cows.url]).output()?;
    let stderr = text(&added_cows.stderr)?;
    assert_eq!(added_cows.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("Added cows, its files are in ../../data/cows"),
        "{stderr}"
    );
    #[cfg(target_os = "linux")]
    if let Some(screen) = sandbox.on_a_terminal(&["-C", "notebooks/week1", "add", &server.url])? {
        assert!(
            screen.contains(&format!(
                "using the project in {}",
                sandbox.work().display()
            )),
            "{screen}"
        );
    }
    Ok(())
}

/// what a person at a terminal sees from `command` after typing `keys`, through a pseudo
/// terminal `script` opens, with its exit code. none when `script` is not installed.
#[cfg(target_os = "linux")]
fn typed(
    folder: &std::path::Path,
    inner: &std::process::Command,
    keys: &str,
) -> Result<Option<(Option<i32>, String)>> {
    let line: Vec<String> = std::iter::once(inner.get_program())
        .chain(inner.get_args())
        .map(|word| format!("'{}'", word.to_string_lossy().replace('\'', r"'\''")))
        .collect();
    let mut script = std::process::Command::new("script");
    script
        .args(["-qec", &line.join(" "), "/dev/null"])
        .current_dir(folder)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    for (name, value) in inner.get_envs() {
        match value {
            Some(value) => script.env(name, value),
            None => script.env_remove(name),
        };
    }
    script
        .env("SHELL", "/bin/sh")
        .env_remove("ENV")
        .env_remove("BASH_ENV");
    let mut child = match script.spawn() {
        Ok(child) => child,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err.into()),
    };
    if let Some(mut stdin) = child.stdin.take() {
        std::io::Write::write_all(&mut stdin, keys.as_bytes())?;
    }
    let output = child.wait_with_output()?;
    let screen = String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n");
    Ok(Some((output.status.code(), screen)))
}

/// at a terminal, q at the list of search results, or no to starting a project in the home
/// folder, cancels the add and leaves the folder as it was.
#[cfg(all(target_os = "linux", debug_assertions))]
#[test]
fn a_first_add_cancelled_at_a_terminal_leaves_nothing() -> Result {
    let sandbox = Sandbox::new()?;
    let mut catalogs = Vec::new();
    let mut command = sandbox.command(&["add", "iris"]);
    for (variable, body) in SEARCHED {
        let catalog = Server::start(
            "/",
            vec![reply(
                "200 OK",
                &[("Content-Type", "application/json")],
                body,
            )],
        )?;
        command.env(variable, &catalog.url);
        catalogs.push(catalog);
    }
    let Some((code, screen)) = typed(&sandbox.work(), &command, "q")? else {
        return Ok(());
    };
    assert!(screen.contains("which one?"), "{screen}");
    assert_ne!(code, Some(0), "{screen}");
    assert_eq!(listed(&sandbox.work())?, Vec::<String>::new(), "{screen}");
    let home = sandbox.folder("home")?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    let asked = sandbox.command(&["add", &server.url]);
    let Some((code, screen)) = typed(&home, &asked, "n")? else {
        return Ok(());
    };
    assert!(screen.contains("your home folder"), "{screen}");
    assert_eq!(code, Some(1), "{screen}");
    assert_eq!(listed(&home)?, Vec::<String>::new(), "{screen}");
    assert_eq!(server.requests().len(), 0, "{screen}");
    Ok(())
}
