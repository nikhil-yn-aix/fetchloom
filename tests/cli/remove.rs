//! `remove`: what it takes out and keeps, and that it downloads nothing.

use std::fs;

use crate::project::{IRIS, PATH, added, clone};
use crate::support::{Result, Sandbox, Server, file, json, text};

/// the example names one dataset, but `--json` gives no fix, since only a person knows which.
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

/// a stop between the names of one remove, as a ctrl-c gives, leaves data.toml, data.lock and the
/// data folder as they were; the same remove then runs to its end.
#[test]
fn a_remove_stopped_between_names_changes_nothing() -> Result {
    let sandbox = Sandbox::new()?;
    let mut servers = Vec::new();
    for path in ["/a.csv", "/b.csv", "/c.csv"] {
        let server = Server::start(path, vec![file(path.as_bytes(), "\"v\"")])?;
        added(&sandbox, &server)?;
        servers.push(server);
    }
    let work = sandbox.work();
    let placed =
        ["a", "b", "c"].map(|name| work.join("data").join(name).join(format!("{name}.csv")));
    let before = (
        fs::read(work.join("data.toml"))?,
        fs::read(work.join("data.lock"))?,
    );
    let output = sandbox
        .command(&["remove", "a", "b", "c"])
        .env("FETCHLOOM_TEST_STOP_AFTER", "2")
        .output()?;
    assert_eq!(output.status.code(), Some(130), "{}", text(&output.stderr)?);
    assert_eq!(fs::read(work.join("data.toml"))?, before.0);
    assert_eq!(fs::read(work.join("data.lock"))?, before.1);
    for file in &placed {
        assert!(file.exists(), "{}", file.display());
    }
    let output = sandbox.run(&["remove", "a", "b", "c"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    assert!(placed.iter().all(|file| !file.exists()));
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

/// such files are work of yours and changed copies sync kept beside files it put back; remove
/// leaves them and the folder, and its dry run says so too.
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

/// an error planning what stays cites the line data.toml holds on disk, which the removal did not
/// write, rather than the line it would hold once the removed table is gone; a dry run too.
#[test]
fn a_plan_error_cites_the_line_data_toml_holds() -> Result {
    for dry_run in [false, true] {
        let sandbox = Sandbox::new()?;
        for path in ["/a.csv", "/b.csv"] {
            let server = Server::start(path, vec![file(path.as_bytes(), "\"v\"")])?;
            added(&sandbox, &server)?;
        }
        let work = sandbox.work();
        let manifest = work.join("data.toml");
        let table = "[datasets.b.members.\"b.zip\"]";
        let written = format!(
            "{}\n{table}\nselect = \"*.csv\"\n",
            fs::read_to_string(&manifest)?
        );
        fs::write(&manifest, &written)?;
        let line = written
            .lines()
            .position(|line| line == table)
            .ok_or("no members table")?
            + 1;
        let lock = fs::read(work.join("data.lock"))?;
        let mut args = vec!["--json", "remove", "a"];
        if dry_run {
            args.push("--dry-run");
        }
        let output = sandbox.run(&args)?;
        assert_eq!(output.status.code(), Some(50), "{}", text(&output.stdout)?);
        let error = &json(&output)?["errors"][0];
        assert_eq!(
            error["details"][0],
            format!("data.toml:{line}   {table}"),
            "{error}"
        );
        assert_eq!(fs::read_to_string(&manifest)?, written);
        assert_eq!(fs::read(work.join("data.lock"))?, lock);
    }
    Ok(())
}

/// a copy not yet synced has no state, so remove keeps the files below the removed dataset's
/// folder and tells them, in a dry run and in the removal: the folder its own `path` names in
/// data.toml as read, not one under its name.
#[test]
fn remove_in_a_copy_tells_the_files_at_the_dataset_s_own_path() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/w.bin", vec![file(b"weights", "\"v\"")])?;
    added(&sandbox, &server)?;
    let manifest = sandbox.work().join("data.toml");
    let pathed = fs::read_to_string(&manifest)?
        .replace("[datasets.w]\n", "[datasets.w]\npath = \"models/w\"\n");
    fs::write(&manifest, pathed)?;
    let copy = clone(&sandbox, "copy")?;
    let own = copy.join("data").join("models").join("w");
    fs::create_dir_all(&own)?;
    fs::write(own.join("w.bin"), b"weights")?;
    let why =
        "its 1 file, since fetchloom cannot tell which files it placed in this project folder";
    for (args, keeps) in [
        (&["--json", "remove", "--dry-run", "w"][..], "would keep"),
        (&["--json", "remove", "w"], "keeps"),
    ] {
        let output = sandbox.command_in(&copy, args).output()?;
        assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
        let document = json(&output)?;
        let told: Vec<&serde_json::Value> = document["warnings"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|warning| warning["kind"] == "place.left_behind")
            .collect();
        assert_eq!(told.len(), 1, "{document}");
        assert_eq!(
            told[0]["message"],
            format!("data/models/w {keeps} {why}"),
            "{document}"
        );
        assert_eq!(
            told[0]["details"],
            serde_json::json!(["data/models/w/w.bin"]),
            "{document}"
        );
    }
    assert_eq!(fs::read(own.join("w.bin"))?, b"weights");
    assert!(!fs::read_to_string(copy.join("data.toml"))?.contains("[datasets.w]"));
    Ok(())
}

/// a file remove cannot delete, one another program holds open on windows or in a read-only
/// folder elsewhere, comes after data.toml and data.lock changed, so the help names the sync that
/// finishes the removal, never a remove that would no longer find the dataset.
#[test]
fn remove_explains_how_to_finish_after_placement_fails() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let folder = sandbox.work().join("data").join("iris");
    let path = folder.join("iris.csv");
    #[cfg(windows)]
    let holder = {
        use std::os::windows::fs::OpenOptionsExt as _;
        fs::OpenOptions::new()
            .read(true)
            .share_mode(3)
            .open(&path)?
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if rustix::process::geteuid().is_root() {
            return crate::support::skipped("unprivileged", "root ignores file permissions");
        }
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o555))?;
    }
    let output = sandbox.run(&["--json", "remove", "iris"])?;
    #[cfg(windows)]
    drop(holder);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o755))?;
    }
    assert_eq!(output.status.code(), Some(50));
    let error = &json(&output)?["errors"][0];
    let message = error["message"].as_str().unwrap_or_default();
    assert!(
        message.starts_with(&format!("could not remove {}:", path.display())),
        "{message}"
    );
    let help = error["help"].as_str().unwrap_or_default();
    assert!(
        help.ends_with("then run `fetchloom sync` to finish the removal"),
        "{help}"
    );
    let details = error["details"].to_string();
    assert!(details.contains("data.toml and data.lock"), "{details}");
    assert!(!fs::read_to_string(sandbox.work().join("data.toml"))?.contains("datasets.iris"));
    let synced = sandbox.run(&["sync"])?;
    assert!(synced.status.success(), "{}", text(&synced.stderr)?);
    assert!(!path.exists());
    Ok(())
}

/// a project with `precious`, a dataset added from a folder of the sandbox.
fn imported(sandbox: &Sandbox) -> Result {
    let precious = sandbox.folder("precious")?;
    fs::write(precious.join("notes.csv"), b"id\n1\n")?;
    let added = sandbox.run(&["add", "../precious"])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    Ok(())
}

/// files imported from a folder come back from no source, so removing their dataset needs a yes,
/// which nobody gives under --json, a pipe or CI unless --yes does.
#[test]
fn removing_an_import_needs_a_yes() -> Result {
    let sandbox = Sandbox::new()?;
    imported(&sandbox)?;
    let work = sandbox.work();
    let before = fs::read(work.join("data.toml"))?;
    let refused = sandbox.run(&["--json", "remove", "precious"])?;
    assert_eq!(refused.status.code(), Some(1), "{}", text(&refused.stdout)?);
    let error = &json(&refused)?["errors"][0];
    assert_eq!(error["kind"], "unconfirmed");
    assert_eq!(
        error["message"],
        "precious was imported from files, and no source can give them back, so removing it needs a yes"
    );
    assert_eq!(
        error["help"],
        "answer yes at a terminal, or run `fetchloom remove precious --yes` once you keep its files elsewhere"
    );
    assert_eq!(error["fix"], serde_json::Value::Null);
    assert_eq!(fs::read(work.join("data.toml"))?, before);
    assert!(work.join("data/precious/notes.csv").exists());
    let removed = sandbox.run(&["remove", "precious", "--yes"])?;
    assert_eq!(removed.status.code(), Some(0), "{}", text(&removed.stderr)?);
    assert!(!work.join("data/precious").exists());
    Ok(())
}

/// an import named with a name data.toml lacks: the name is refused before any yes is asked for,
/// with help that leaves it out, and the command that help gives asks for the yes the import needs.
#[test]
fn a_typo_beside_an_import_is_refused_before_the_yes() -> Result {
    let sandbox = Sandbox::new()?;
    imported(&sandbox)?;
    let work = sandbox.work();
    let before = fs::read(work.join("data.toml"))?;
    let refused = sandbox.run(&["--json", "remove", "precious", "typo"])?;
    assert_eq!(refused.status.code(), Some(2), "{}", text(&refused.stdout)?);
    let error = &json(&refused)?["errors"][0];
    assert_eq!(error["message"], "data.toml has no dataset called \"typo\"");
    assert_eq!(
        error["help"],
        "leave it out, as in `fetchloom --json remove precious`"
    );
    assert_eq!(fs::read(work.join("data.toml"))?, before);
    let asked = sandbox.run(&["--json", "remove", "precious"])?;
    assert_eq!(asked.status.code(), Some(1), "{}", text(&asked.stdout)?);
    assert_eq!(
        json(&asked)?["errors"][0]["help"],
        "answer yes at a terminal, or run `fetchloom remove precious --yes` once you keep its files elsewhere"
    );
    let removed = sandbox.run(&["remove", "precious", "--yes"])?;
    assert_eq!(removed.status.code(), Some(0), "{}", text(&removed.stderr)?);
    assert!(!work.join("data/precious").exists());
    Ok(())
}

/// at a terminal the removal of an import asks first: a no changes nothing, and a yes or --yes
/// removes it.
#[cfg(target_os = "linux")]
#[test]
fn removing_an_import_asks_at_a_terminal() -> Result {
    let sandbox = Sandbox::new()?;
    imported(&sandbox)?;
    let work = sandbox.work();
    let before = fs::read(work.join("data.toml"))?;
    let asked = "remove it anyway?";
    let remove = sandbox.command(&["remove", "precious"]);
    let Some((code, screen)) = crate::support::typed(&work, &remove, "", asked, "n\r")? else {
        return Ok(());
    };
    assert!(
        screen.contains("precious was imported from files, and no source can give them back once data/precious and the cache's copy are gone. remove it anyway? [y/N]"),
        "{screen}"
    );
    assert!(
        screen.contains("Cancelled the removal of precious, so nothing changed"),
        "{screen}"
    );
    assert_eq!(code, Some(1), "{screen}");
    assert_eq!(fs::read(work.join("data.toml"))?, before);
    let Some((code, screen)) = crate::support::typed(&work, &remove, "", asked, "y\r")? else {
        return Ok(());
    };
    assert_eq!(code, Some(0), "{screen}");
    assert!(!work.join("data/precious").exists(), "{screen}");
    // --yes answers it unasked
    let added = sandbox.run(&["add", "../precious"])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let told = sandbox.command(&["remove", "precious", "--yes"]);
    let Some((code, screen)) = crate::support::typed(&work, &told, "", "", "")? else {
        return Ok(());
    };
    assert_eq!(code, Some(0), "{screen}");
    assert!(!screen.contains(asked), "{screen}");
    assert!(!work.join("data/precious").exists(), "{screen}");
    Ok(())
}

/// remove takes several names, as sync and exclude do: every one is checked before any changes.
#[test]
fn remove_takes_several_names() -> Result {
    let sandbox = Sandbox::new()?;
    let manifest = sandbox.work().join("data.toml");
    fs::write(
        &manifest,
        "[datasets.iris]\nref = \"zenodo:1\"\n\n[datasets.keep]\nref = \"zenodo:2\"\n\n[datasets.wine]\nref = \"zenodo:3\"\n",
    )?;
    let unknown = sandbox.run(&["--json", "remove", "iris", "wien"])?;
    assert_eq!(unknown.status.code(), Some(2), "{}", text(&unknown.stdout)?);
    assert!(fs::read_to_string(&manifest)?.contains("[datasets.iris]"));
    let output = sandbox.run(&["--json", "remove", "iris", "wine"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
    assert_eq!(
        json(&output)?["result"]["datasets"],
        serde_json::json!(["iris", "wine"])
    );
    assert_eq!(
        fs::read_to_string(&manifest)?,
        "[datasets.keep]\nref = \"zenodo:2\"\n"
    );
    Ok(())
}

/// a note a blank line above a removed dataset stays, at the end once no table follows it.
#[test]
fn a_note_above_removed_datasets_stays() -> Result {
    let sandbox = Sandbox::new()?;
    let mut servers = Vec::new();
    for path in ["/a.csv", "/b.csv", "/c.csv"] {
        let server = Server::start(path, vec![file(path.as_bytes(), "\"v\"")])?;
        added(&sandbox, &server)?;
        servers.push(server);
    }
    let work = sandbox.work();
    let manifest = work.join("data.toml");
    let noted =
        fs::read_to_string(&manifest)?.replace("[datasets.b]", "# raw data\n\n[datasets.b]");
    fs::write(&manifest, noted)?;
    let output = sandbox.run(&["remove", "c", "b"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    assert_eq!(
        fs::read_to_string(&manifest)?,
        format!("[datasets.a]\nref = \"{}\"\n\n# raw data\n", servers[0].url)
    );
    let lock = fs::read_to_string(work.join("data.lock"))?;
    assert!(
        lock.contains("name = \"a\"")
            && !lock.contains("name = \"b\"")
            && !lock.contains("name = \"c\""),
        "{lock}"
    );
    assert_eq!(fs::read(work.join("data/a/a.csv"))?, b"/a.csv");
    assert!(!work.join("data/b").exists() && !work.join("data/c").exists());
    Ok(())
}
