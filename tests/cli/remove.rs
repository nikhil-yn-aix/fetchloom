//! `remove` against servers on this machine: what it drops and keeps, and that it downloads
//! nothing.

use std::fs;

use crate::project::{IRIS, PATH, added, clone};
use crate::support::{Result, Sandbox, Server, file, json, text};

/// `remove` given no name lists the datasets it could remove and names one as an example,
/// which `--json` gives as no fix, since only a person knows which one to remove.
#[test]
fn remove_without_a_name_lists_the_datasets() -> Result {
    let sandbox = Sandbox::new()?;
    fs::write(
        sandbox.work().join("data.toml"),
        "[datasets.iris]\nref = \"zenodo:1\"\n\n[datasets.wine]\nref = \"zenodo:2\"\n",
    )?;
    let output = sandbox.run(&["remove"])?;
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        text(&output.stderr)?,
        "error: remove needs <name>\n  datasets: iris, wine\n  help: name one of them, as in `fetchloom remove iris`\n"
    );
    let document = json(&sandbox.run(&["--json", "remove"])?)?;
    assert_eq!(document["errors"][0]["kind"], "usage");
    assert_eq!(document["errors"][0]["fix"], serde_json::Value::Null);
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

/// a mistyped name is answered with the command as typed, only the name changed: pasting the
/// suggestion for a dry run previews the removal rather than doing it.
#[test]
fn a_mistyped_name_keeps_the_rest_of_the_command_in_the_suggestion() -> Result {
    let sandbox = Sandbox::new()?;
    let manifest = "[datasets.iris]\nref = \"https://example.org/iris.csv\"\n\n[datasets.penguins]\nref = \"https://example.org/penguins.csv\"\n";
    fs::write(sandbox.work().join("data.toml"), manifest)?;
    for (typed, meant) in [
        (
            &["remove", "--dry-run", "irs"][..],
            "fetchloom remove --dry-run iris",
        ),
        (
            &["remove", "irs", "--dry-run"],
            "fetchloom remove iris --dry-run",
        ),
        (
            &["-q", "remove", "-n", "irs"],
            "fetchloom -q remove -n iris",
        ),
        (
            &["remove", "--dry-run", "imdb"],
            "fetchloom remove --dry-run iris",
        ),
    ] {
        let output = sandbox.run(typed)?;
        assert_eq!(output.status.code(), Some(2), "{typed:?}");
        let said = text(&output.stderr)?;
        assert!(said.contains(&format!("`{meant}`")), "{typed:?}: {said}");
    }
    assert_eq!(
        fs::read_to_string(sandbox.work().join("data.toml"))?,
        manifest
    );
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
