//! the project loop: add, sync, lock, status, path and remove, against servers on this machine.

use std::fs;

use crate::support::{Result, Sandbox, Server, delete, edit, file, json, reply, text};

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
        "✓ iris is already in data.toml. its files are in data/iris\n"
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
    let hardlinked = links.hardlink && !links.reflink;
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
        "✓ data.lock was already up to date. nothing to do\n"
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
        "✓ everything matches data.lock\n"
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
fn fetchloom_alone_in_a_project_sums_up_its_status() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let inner = sandbox.work().join("data").join("iris");
    let calm = sandbox.command_in(&inner, &[]).output()?;
    assert_eq!(calm.status.code(), Some(0));
    assert!(calm.stdout.is_empty(), "{}", text(&calm.stdout)?);
    assert_eq!(text(&calm.stderr)?, "✓ everything matches data.lock\n");
    delete(&inner.join("iris.csv"))?;
    let missing = sandbox.run(&[])?;
    assert_eq!(
        text(&missing.stderr)?,
        "✓ found 1 difference from data.lock\n  hint: `fetchloom status` lists what differs, and `fetchloom sync` brings it in line\n"
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
        stderr.contains("had changed and awaits replacement"),
        "{stderr}"
    );
    assert_eq!(fs::read(&object)?, b"damaged on disk");
    let synced = sandbox.run(&["sync", "--force"])?;
    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    assert_eq!(fs::read(&object)?, IRIS);
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
        "✓ z-packed is already in data.toml. its files are in data/z-packed\n"
    );
    assert_eq!(
        fs::read_to_string(sandbox.work().join("data.toml"))?,
        manifest
    );
    assert_eq!(fs::read(sandbox.work().join("data.lock"))?, locked);
    assert_eq!(server.requests().len(), requests);
    Ok(())
}
