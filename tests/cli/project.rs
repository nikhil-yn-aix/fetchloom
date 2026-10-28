//! the project loop: add, sync, lock, status, path and remove, against servers on this machine.

use std::fs;
use std::process::Output;

use serde_json::Value;

use crate::support::{Result, Sandbox, Server, file, text};

const IRIS: &[u8] = b"sepal_length,species\n5.1,setosa\n";
const PATH: &str = "/files/iris.csv";

fn json(output: &Output) -> Result<Value> {
    Ok(serde_json::from_slice(&output.stdout)?)
}

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
    let mut settings = sandbox.filters();
    settings.add_filter(r#"spec = "sha256:[0-9a-f]{64}""#, r#"spec = "[spec]""#);
    settings.add_filter(r#"locked = "[0-9T:-]+Z""#, r#"locked = "[time]""#);
    settings.bind(|| {
        insta::assert_snapshot!("add_lock", lock);
        insta::assert_snapshot!("add_story", stderr);
    });
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
        "✓ everything already matches data.lock. nothing to do\n"
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
    assert_eq!(document["work"]["copies"], 0, "a link costs no disk");
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
    fs::remove_file(&placed)?;
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
        text(&kept.stderr)?.contains("data/iris/iris.csv was changed by hand, so it stays"),
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

#[test]
fn an_edit_through_a_hardlink_never_spreads_to_other_projects() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    let mut permissions = fs::metadata(&placed)?.permissions();
    #[expect(
        clippy::permissions_set_readonly_false,
        reason = "the test makes the edit a careless user would"
    )]
    permissions.set_readonly(false);
    fs::set_permissions(&placed, permissions)?;
    let mut edited = IRIS.to_vec();
    edited[0] = b'S';
    fs::write(&placed, &edited)?;
    let output = sandbox.run(&["sync", "--force"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    assert_eq!(fs::read(&placed)?, IRIS, "the locked bytes are back");
    assert_eq!(
        server.requests().len(),
        2,
        "the damaged object was downloaded again"
    );
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
        text(&typo.stderr)?.contains("did you mean iris?"),
        "{}",
        text(&typo.stderr)?
    );
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
fn a_pattern_that_matches_nothing_explains_why_and_changes_nothing() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    let output = sandbox.run(&["add", &server.url, "--select", "*.txt"])?;
    assert_eq!(output.status.code(), Some(50));
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
        "✓ everything matches data.lock\n"
    );
    fs::remove_file(folder.join("iris.csv"))?;
    fs::write(folder.join("notes.txt"), "mine")?;
    let output = sandbox.run(&["status"])?;
    assert_eq!(output.status.code(), Some(0));
    let stderr = text(&output.stderr)?;
    sandbox.filters().bind(|| insta::assert_snapshot!(stderr));
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
    assert_eq!(
        text(&gc.stderr)?,
        "✓ nothing to free. every object is needed by a project or newer than a day\n"
    );
    let verify = sandbox.run(&["cache", "verify"])?;
    assert_eq!(verify.status.code(), Some(0));
    assert_eq!(
        text(&verify.stderr)?,
        "✓ verified 1 object · 32 B. every one is intact\n"
    );
    fs::remove_file(sandbox.work().join("data.toml"))?;
    let forgotten = json(&sandbox.run(&["--json", "cache"])?)?;
    assert_eq!(
        forgotten["result"]["projects"].as_array().map(Vec::len),
        Some(0),
        "a project without data.toml is forgotten"
    );
    Ok(())
}
