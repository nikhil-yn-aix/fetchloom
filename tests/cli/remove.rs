//! `remove` against servers on this machine: what it drops and keeps, and that it downloads
//! nothing.

use std::fs;

use crate::project::{IRIS, PATH, added, clone};
use crate::support::{Result, Sandbox, Server, file, json, text};

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

/// a dataset's folder can hold more than fetchloom placed: work of yours, and the changed copies
/// sync kept beside files it put back. remove leaves them and the folder, and says so, as its dry
/// run does first.
#[test]
fn remove_names_the_files_it_leaves_in_the_dataset_folder() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let folder = sandbox.work().join("data").join("iris");
    fs::create_dir(folder.join("resized"))?;
    let left = ["notes.txt", "iris.csv.changed", "resized/small.csv"];
    for name in left {
        fs::write(folder.join(name), name)?;
    }
    let said = "data/iris still holds 3 files fetchloom did not place, such as data/iris/iris.csv.changed, so the folder stays";
    for args in [
        &["--json", "remove", "iris", "--dry-run"][..],
        &["--json", "remove", "iris"],
    ] {
        let output = sandbox.run(args)?;
        assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
        let warnings = json(&output)?["warnings"].to_string();
        assert!(warnings.contains(said), "{warnings}");
    }
    assert!(!folder.join("iris.csv").exists());
    for name in left {
        assert_eq!(fs::read_to_string(folder.join(name))?, name);
    }
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
