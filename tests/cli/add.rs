//! `add` against servers on this machine: naming a dataset, adding it again, putting the project
//! back when an add fails, and stops and cancels while it downloads.

use std::fs;

use crate::project::{IRIS, PATH, added};
#[cfg(unix)]
use crate::project::{partial_bytes, told_a_stop};
#[cfg(unix)]
use crate::support::wait_until;
use crate::support::{
    Part, Result, Routes, Running, Sandbox, Server, delete_folder, file, json, reply, sha256_hex,
    text, warned, without_copy_warning,
};

#[test]
fn an_automatic_name_that_is_taken_gets_a_number() -> Result {
    let sandbox = Sandbox::new()?;
    let first = Server::start(PATH, vec![file(b"species\nsetosa\n", "\"a\"")])?;
    added(&sandbox, &first)?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    let output = sandbox.run(&["add", &server.url])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    let warning = "iris was taken, so this one is iris-2\n  help: choose a name with --as <name>";
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
        serde_json::json!([{
            "kind": "name.taken",
            "message": "iris was taken, so this one is iris-3",
            "details": [],
            "help": "choose a name with --as <name>"
        }])
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
        stderr.contains("a new link must be downloaded once to learn its SHA-256"),
        "{stderr}"
    );
    assert_eq!(server.requests().len(), 0, "offline sends nothing");
    Ok(())
}

#[test]
fn add_pins_downloads_and_places_a_link() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    // the story of a link on the internet, which a later run reaches without allowed_origins
    sandbox.allow(&server.url);
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
    let mut settings = sandbox.placed_filters(&stderr)?;
    settings.add_filter(r#"spec = "sha256:[0-9a-f]{64}""#, r#"spec = "[spec]""#);
    settings.add_filter(r#"locked = "[0-9T:-]+Z""#, r#"locked = "[time]""#);
    settings.bind(|| {
        insta::assert_snapshot!("add_lock", lock);
        insta::assert_snapshot!("add_story", stderr);
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

/// the same add again, with or without the name it gave, as a script run twice does, changes
/// nothing and asks no server.
#[test]
fn adding_the_same_dataset_again_changes_nothing() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let before = fs::read(sandbox.work().join("data.toml"))?;
    let lock = fs::read(sandbox.work().join("data.lock"))?;
    for args in [
        &["add", &server.url][..],
        &["add", &server.url, "--as", "iris"],
    ] {
        let output = sandbox.run(args)?;
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(0), "{args:?}: {stderr}");
        assert_eq!(
            stderr,
            "         Ok iris is already in data.toml, its files are in data/iris\n"
        );
        assert_eq!(fs::read(sandbox.work().join("data.toml"))?, before);
        assert_eq!(fs::read(sandbox.work().join("data.lock"))?, lock);
    }
    assert_eq!(server.requests().len(), 1, "nothing is downloaded twice");
    Ok(())
}

/// a name data.toml has refuses another link, or the same link with other patterns, which only
/// a person can choose between, so json hands no command to run: the one an agent once ran
/// removed the dataset that had the name.
#[test]
fn a_name_data_toml_has_refuses_another_dataset_with_no_fix() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let before = fs::read(sandbox.work().join("data.toml"))?;
    let other = server.url.replace("iris.csv", "wine.csv");
    for args in [
        &["--json", "add", &other, "--as", "iris"][..],
        &[
            "--json",
            "add",
            &server.url,
            "--as",
            "iris",
            "--select",
            "*.csv",
        ],
    ] {
        let output = sandbox.run(args)?;
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        let error = &json(&output)?["errors"][0];
        assert_eq!(error["kind"], "usage", "{args:?}");
        assert_eq!(error["fix"], serde_json::Value::Null, "{args:?}: {error}");
        assert_eq!(fs::read(sandbox.work().join("data.toml"))?, before);
    }
    assert_eq!(server.requests().len(), 1, "a refused name asks nothing");
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

#[test]
fn duplicate_add_chooses_the_entry_with_matching_settings() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, (0..4).map(|_| file(IRIS, "\"v1\"")).collect())?;
    sandbox.allow(&server.url);
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
    // data.toml now names a server on this machine, which only allowed_origins lets it reach
    sandbox.allow(&second.url);

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
    let mut running = Running::start(&mut sandbox.command(&["add", &cows.url, "--as", "cows"]))?;
    cows.wait_for_requests(1)?;
    running.kill()?;
    let stderr = text(&running.finish()?.stderr)?;
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
    let running = Running::start(&mut sandbox.command(&["-v", "add", &cows.url, "--as", "cows"]))?;
    cows.wait_for_requests(1)?;
    let edited = format!(
        "{}# a note made meanwhile\n",
        fs::read_to_string(work.join("data.toml"))?
    );
    fs::write(work.join("data.toml"), &edited)?;
    drop(release);
    let output = running.finish()?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(50), "{stderr}");
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
    let slow = Server::held_file("/files/cows.csv", body)?;
    let running = Running::start(&mut sandbox.command(&["add", &slow.url, "--as", "cows"]))?;
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
    let output = running.finish()?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(130), "{stderr}");
    told_a_stop(&stderr, "    Stopped, and nothing was added")?;
    assert!(!stderr.contains("could not be put back"), "{stderr}");
    assert_eq!(fs::read_to_string(work.join("data.toml"))?, edited);
    Ok(())
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
    let slow = Server::held_file("/files/cows.csv", body.clone())?;
    let running = Running::start(&mut sandbox.command(&["add", &slow.url, "--as", "cows"]))?;
    wait_until(
        "bytes to arrive",
        || Ok(partial_bytes(&sandbox.cache()) > 0),
    )?;
    running.interrupt()?;
    let output = running.finish()?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(130), "{stderr}");
    let kept = told_a_stop(&stderr, "    Stopped, and nothing was added")?;
    assert!(kept.starts_with("            the "), "{stderr}");
    assert_eq!(stderr.matches("nothing was added").count(), 1, "{stderr}");
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
        let running = Running::start(&mut command)?;
        stalled.wait_for_requests(1)?;
        running.interrupt()?;
        let output = running.finish()?;
        let stderr = text(&output.stderr)?;
        drop(release);
        assert_eq!(output.status.code(), Some(130), "{stderr}");
        assert_eq!(listed(&sandbox.work())?, Vec::<String>::new(), "{stderr}");
    }
    Ok(())
}

/// the first add says it started a project, and where, the home folder as `~`. in the home
/// folder, where a project would take in every folder below, it asks first, and with nobody to
/// ask it refuses unless --yes.
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
    let stderr = text(&approved.stderr)?;
    assert!(stderr.contains("    Started a project in ~\n"), "{stderr}");
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
    let Some((code, screen)) = crate::support::typed(&sandbox.work(), &command, "", "", "q")?
    else {
        return Ok(());
    };
    assert!(screen.contains("which one?"), "{screen}");
    assert_ne!(code, Some(0), "{screen}");
    assert_eq!(listed(&sandbox.work())?, Vec::<String>::new(), "{screen}");
    let home = sandbox.folder("home")?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    let asked = sandbox.command(&["add", &server.url]);
    let Some((code, screen)) = crate::support::typed(&home, &asked, "", "", "n\r")? else {
        return Ok(());
    };
    assert!(screen.contains("your home folder"), "{screen}");
    assert_eq!(code, Some(1), "{screen}");
    assert_eq!(listed(&home)?, Vec::<String>::new(), "{screen}");
    assert_eq!(server.requests().len(), 0, "{screen}");
    Ok(())
}

/// a cancel the person chose is no error: q at the list of search results, ctrl-c there, and no
/// or ctrl-c at the question about starting a project in the home folder each say what was
/// cancelled and that nothing changed on a line of their own, never `error:`. the exit codes
/// stay those scripts know: 2 for q, 1 for no, 130 for ctrl-c.
#[cfg(all(target_os = "linux", debug_assertions))]
#[test]
fn a_cancel_at_a_question_is_no_error() -> Result {
    let sandbox = Sandbox::new()?;
    let mut catalogs = Vec::new();
    let mut command = sandbox.command(&["add", "iris"]);
    for (variable, body) in SEARCHED {
        let catalog = Server::start(
            "/",
            vec![
                reply("200 OK", &[("Content-Type", "application/json")], body),
                reply("200 OK", &[("Content-Type", "application/json")], body),
            ],
        )?;
        command.env(variable, &catalog.url);
        catalogs.push(catalog);
    }
    for (keys, code) in [("q", 2), ("\u{3}", 130)] {
        // -isig lets ctrl-c reach the list as the key it reads, whenever it is typed
        let Some((exit, screen)) =
            crate::support::typed(&sandbox.work(), &command, "-isig", "which one?", keys)?
        else {
            return Ok(());
        };
        assert!(screen.contains("which one?"), "{screen}");
        assert_eq!(exit, Some(code), "{keys:?}: {screen}");
        assert!(
            screen.contains("  Cancelled the choice of a dataset, so nothing changed\n"),
            "{keys:?}: {screen}"
        );
        assert!(!screen.contains("error:"), "{keys:?}: {screen}");
        assert_eq!(listed(&sandbox.work())?, Vec::<String>::new(), "{screen}");
    }
    let home = sandbox.folder("home")?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    let asked = sandbox.command(&["add", &server.url]);
    for (keys, code) in [("n\r", 1), ("\u{3}", 130)] {
        let Some((exit, screen)) =
            crate::support::typed(&home, &asked, "-isig", "start one anyway?", keys)?
        else {
            return Ok(());
        };
        assert!(screen.contains("your home folder"), "{screen}");
        assert_eq!(exit, Some(code), "{keys:?}: {screen}");
        assert!(
            screen.contains(&format!(
                "  Cancelled starting a project in {}, so nothing changed\n",
                home.display()
            )),
            "{keys:?}: {screen}"
        );
        assert!(!screen.contains("error:"), "{keys:?}: {screen}");
        assert_eq!(listed(&home)?, Vec::<String>::new(), "{screen}");
    }
    assert_eq!(server.requests().len(), 0);
    Ok(())
}

/// with `FETCHLOOM_ACCESSIBLE=1`, the datasets a search finds are a numbered list drawn once,
/// without an escape code to move the cursor, and the number typed picks one: a number out of
/// range asks again, and q cancels as at the picker.
#[cfg(all(target_os = "linux", debug_assertions))]
#[test]
fn an_accessible_terminal_picks_a_dataset_by_its_number() -> Result {
    let sandbox = Sandbox::new()?;
    let mut catalogs = Vec::new();
    let mut command = sandbox.command(&["add", "iris", "--dry-run"]);
    for (variable, body) in SEARCHED {
        let json = || reply("200 OK", &[("Content-Type", "application/json")], body);
        let catalog = Server::start("/", (0..4).map(|_| json()).collect())?;
        command.env(variable, &catalog.url);
        catalogs.push(catalog);
    }
    command.env("FETCHLOOM_ACCESSIBLE", "1");
    let asked = "or q to cancel: ";
    let Some((_, screen)) = crate::support::typed(&sandbox.work(), &command, "", asked, "12\r2\r")?
    else {
        return Ok(());
    };
    assert_eq!(screen.matches(asked).count(), 2, "{screen}");
    let second = screen
        .lines()
        .find_map(|line| line.strip_prefix("    2 "))
        .and_then(|row| row.split_whitespace().next())
        .ok_or_else(|| format!("no second row: {screen}"))?;
    assert!(screen.contains(&format!("  Chose {second}\n")), "{screen}");
    assert!(!screen.contains('\u{1b}'), "{screen:?}");
    let Some((code, screen)) = crate::support::typed(&sandbox.work(), &command, "", asked, "q\r")?
    else {
        return Ok(());
    };
    assert_eq!(code, Some(2), "{screen}");
    assert!(
        screen.contains("  Cancelled the choice of a dataset, so nothing changed\n"),
        "{screen}"
    );
    assert_eq!(listed(&sandbox.work())?, Vec::<String>::new(), "{screen}");
    Ok(())
}

/// no or ctrl-c at the question about a large download an add needs is a cancel, and the add has
/// changed nothing by then: it writes data.toml and data.lock only once every file is in.
#[cfg(target_os = "linux")]
#[test]
fn a_cancelled_download_of_an_add_says_nothing_changed() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let work = sandbox.work();
    let before = (
        fs::read(work.join("data.toml"))?,
        fs::read(work.join("data.lock"))?,
    );
    let huge = || reply("200 OK", &[("Content-Length", "2147483648")], b"");
    let large = Server::start("/huge.bin", (0..4).map(|_| huge()).collect())?;
    let command = sandbox.command(&["add", &large.url]);
    for (keys, code) in [("n\r", 1), ("\u{3}", 130)] {
        // -isig lets ctrl-c reach the question as the key it reads, whenever it is typed
        let Some((exit, screen)) = crate::support::typed(
            &work,
            &command,
            "-isig",
            "download huge.bin (2.0 GiB)?",
            keys,
        )?
        else {
            return Ok(());
        };
        assert_eq!(exit, Some(code), "{keys:?}: {screen}");
        assert!(
            screen.contains("  Cancelled the download of huge.bin (2.0 GiB), so nothing changed\n"),
            "{keys:?}: {screen}"
        );
        assert!(!screen.contains("error:"), "{keys:?}: {screen}");
        assert_eq!(
            (
                fs::read(work.join("data.toml"))?,
                fs::read(work.join("data.lock"))?
            ),
            before,
            "{screen}"
        );
    }
    Ok(())
}

const WINE: &[u8] = b"alcohol,class\n14.23,1\n";

#[test]
fn a_list_adds_one_dataset_per_line_and_checks_hashes() -> Result {
    let sandbox = Sandbox::new()?;
    let iris = Server::start("/iris.csv", vec![file(IRIS, "\"v1\"")])?;
    let wine = Server::start("/wine.csv", vec![file(WINE, "\"w1\"")])?;
    // the hosts come from a file, so a person allows them as data.toml's would be
    sandbox.allow(&iris.url);
    sandbox.allow(&wine.url);
    let work = sandbox.work();
    fs::write(
        work.join("links.txt"),
        format!(
            "# inputs\n{} sha256:{}\n\n{}\n",
            iris.url,
            sha256_hex(IRIS),
            wine.url
        ),
    )?;
    let output = sandbox.run(&["--json", "add", "-i", "links.txt"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
    assert_eq!(
        json(&output)?["result"]["datasets"],
        serde_json::json!(["iris", "wine"])
    );
    assert_eq!(
        fs::read_to_string(work.join("data.toml"))?,
        format!(
            "[datasets.iris]\nref = \"{}\"\n\n[datasets.wine]\nref = \"{}\"\n",
            iris.url, wine.url
        )
    );
    let lock = fs::read_to_string(work.join("data.lock"))?;
    assert!(lock.contains(&sha256_hex(IRIS)), "{lock}");
    assert!(lock.contains(&sha256_hex(WINE)), "{lock}");
    assert_eq!(fs::read(work.join("data/iris/iris.csv"))?, IRIS);
    assert_eq!(fs::read(work.join("data/wine/wine.csv"))?, WINE);

    let other = Server::start("/other.csv", vec![file(b"other\n", "\"o1\"")])?;
    sandbox.allow(&other.url);
    fs::write(
        work.join("more.txt"),
        format!("{} {}\n", other.url, sha256_hex(IRIS)),
    )?;
    let (manifest, pins) = (fs::read(work.join("data.toml"))?, lock.into_bytes());
    let wrong = sandbox.run(&["--json", "add", "-i", "more.txt"])?;
    assert_eq!(wrong.status.code(), Some(30), "{}", text(&wrong.stdout)?);
    assert_eq!(
        json(&wrong)?["errors"][0]["message"],
        "other/other.csv does not match the SHA-256 on line 1 of more.txt"
    );
    assert_eq!(fs::read(work.join("data.toml"))?, manifest);
    assert_eq!(fs::read(work.join("data.lock"))?, pins);
    assert!(!work.join("data/other").exists());
    Ok(())
}

/// a list ends with one line for each dataset it adds, as a single add does, and `-v` adds what
/// each one pinned and where its files were placed.
#[test]
fn a_list_tells_one_line_for_each_dataset_it_adds() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Routes::start(vec![
        (PATH.to_owned(), file(IRIS, "\"v1\"")),
        ("/files/wine.csv".to_owned(), file(WINE, "\"w1\"")),
    ])?;
    sandbox.allow(&server.url);
    let links = format!("{0}{PATH}\n{0}/files/wine.csv\n", server.url);
    let verbose = sandbox.folder("verbose")?;
    for folder in [sandbox.work(), verbose.clone()] {
        fs::write(folder.join("links.txt"), &links)?;
    }
    let output = sandbox.run(&["add", "-i", "links.txt"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    let dataset_lines: Vec<&str> = stderr
        .lines()
        .filter(|line| line.contains("iris") || line.contains("wine"))
        .collect();
    assert_eq!(
        dataset_lines,
        [
            "      Added iris, its files are in data/iris",
            "      Added wine, its files are in data/wine"
        ],
        "{stderr}"
    );
    let output = sandbox
        .command_in(&verbose, &["-v", "add", "-i", "links.txt"])
        .output()?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    for told in [
        format!(
            "     Pinned iris, new in data.lock: {}{PATH} · 1 file · 32 B",
            server.url
        ),
        "     Placed data/wine · 1 file".to_owned(),
        "      Added wine, its files are in data/wine".to_owned(),
    ] {
        assert!(stderr.contains(&told), "{told}: {stderr}");
    }
    Ok(())
}

/// several links on the command line add a dataset each, as a list does, and are trusted as one
/// typed link is. `--as`, which names one dataset, is refused before any request.
#[test]
fn several_typed_links_add_a_dataset_each() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Routes::start(vec![
        (PATH.to_owned(), file(IRIS, "\"v1\"")),
        ("/files/wine.csv".to_owned(), file(WINE, "\"w1\"")),
    ])?;
    let iris = format!("{}{PATH}", server.url);
    let wine = format!("{}/files/wine.csv", server.url);
    let named = sandbox.run(&["--json", "add", &iris, &wine, "--as", "flowers"])?;
    assert_eq!(named.status.code(), Some(2), "{}", text(&named.stdout)?);
    assert_eq!(
        json(&named)?["errors"][0]["message"],
        "--as names one dataset, and 2 links add 2"
    );
    assert_eq!(server.requests(), Vec::<String>::new());
    let output = sandbox.run(&["--json", "add", &iris, &wine])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
    assert_eq!(
        json(&output)?["result"]["datasets"],
        serde_json::json!(["iris", "wine"])
    );
    let work = sandbox.work();
    assert_eq!(
        fs::read_to_string(work.join("data.toml"))?,
        format!("[datasets.iris]\nref = \"{iris}\"\n\n[datasets.wine]\nref = \"{wine}\"\n")
    );
    assert_eq!(fs::read(work.join("data/iris/iris.csv"))?, IRIS);
    assert_eq!(fs::read(work.join("data/wine/wine.csv"))?, WINE);
    Ok(())
}

/// a list takes no --as, so the help for a name an earlier line took points at data.toml, where
/// a dataset can be renamed with its pin.
#[test]
fn a_list_line_whose_name_is_taken_says_how_to_rename_it() -> Result {
    let sandbox = Sandbox::new()?;
    let first = Server::start("/iris.csv", vec![file(IRIS, "\"v1\"")])?;
    let second = Server::start("/iris.csv", vec![file(WINE, "\"w1\"")])?;
    sandbox.allow(&first.url);
    sandbox.allow(&second.url);
    let links = format!("{}\n{}\n", first.url, second.url);
    fs::write(sandbox.work().join("links.txt"), links)?;
    let output = sandbox.run(&["--json", "add", "-i", "links.txt"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
    let mut warnings = warned(&json(&output)?);
    warnings.retain(|warning| {
        warning != "the drive holding data cannot link files, so files were copied"
    });
    assert_eq!(
        warnings,
        [
            "iris was taken, so this one is iris-2; to name it yourself, rename its table in data.toml, and the next sync moves its pin to the new name"
        ]
    );
    Ok(())
}

#[test]
fn a_list_with_a_secret_makes_no_request_and_changes_nothing() -> Result {
    let sandbox = Sandbox::new()?;
    let iris = Server::start("/iris.csv", vec![file(IRIS, "\"v1\"")])?;
    sandbox.allow(&iris.url);
    let work = sandbox.work();
    fs::write(
        work.join("links.txt"),
        format!("{}\n{}?token=hunter2\n", iris.url, iris.url),
    )?;
    let output = sandbox.run(&["--json", "add", "-i", "links.txt"])?;
    assert_eq!(output.status.code(), Some(10), "{}", text(&output.stdout)?);
    let document = json(&output)?;
    assert_eq!(
        document["errors"][0]["message"],
        "line 2 of links.txt: this link carries token, a secret that data.toml and data.lock would keep and share"
    );
    assert!(!document.to_string().contains("hunter2"), "{document}");
    assert_eq!(document["work"]["requests"], 0);
    assert_eq!(iris.requests(), Vec::<String>::new());
    assert!(!work.join("data.toml").exists());
    assert!(!work.join(".fetchloom").exists());
    let named = sandbox.run(&["add", "-i", "links.txt", "--as", "flowers"])?;
    assert_eq!(named.status.code(), Some(2), "--as names one dataset");
    Ok(())
}

/// a link typed for add reaches a private address, such as a lab server's, for that run alone.
/// the add warns of it with the exact origin and the settings file to list it in, which `-q`
/// keeps, and a clone with an empty cache is refused, naming the link once and quoting the
/// setting that lets it through, until `allowed_origins` lists that origin.
#[test]
fn a_private_link_typed_for_add_says_how_a_later_run_reaches_it() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), file(IRIS, "\"v1\"")])?;
    let origin = server.url.trim_end_matches(PATH).to_owned();
    let settings = sandbox.folder("config")?.join("settings.toml");
    fs::write(&settings, "")?;
    let line = format!(
        "put `allowed_origins = [\"{origin}\"]` in {}, or set `FETCHLOOM_ALLOWED_ORIGINS={origin}` for one run",
        settings.display()
    );
    let added = sandbox
        .command(&["-q", "add", &server.url])
        .env("FETCHLOOM_SETTINGS", &settings)
        .output()?;
    let stderr = text(&added.stderr)?;
    assert_eq!(added.status.code(), Some(0), "{stderr}");
    assert_eq!(
        without_copy_warning(&stderr),
        format!(
            "warning: a later run downloads from {origin}, a private address, only once allowed_origins lists it\n  help: {line}\n"
        )
    );
    let (clone, cache) = crate::mirrors::cold(&sandbox, "clone")?;
    let sync = |allowed: Option<&str>| {
        let mut command = sandbox.command_in(&clone, &["--json", "sync"]);
        command
            .env("FETCHLOOM_CACHE", &cache)
            .env("FETCHLOOM_SETTINGS", &settings);
        match allowed {
            Some(allowed) => command.env("FETCHLOOM_ALLOWED_ORIGINS", allowed),
            None => command.env_remove("FETCHLOOM_ALLOWED_ORIGINS"),
        };
        command.output()
    };
    let refused = sync(None)?;
    assert_eq!(
        refused.status.code(),
        Some(20),
        "{}",
        text(&refused.stdout)?
    );
    let error = &json(&refused)?["errors"][0];
    assert_eq!(error["kind"], "network.refused");
    assert_eq!(
        error["message"],
        format!(
            "fetchloom will not follow {}: it points at the private address 127.0.0.1",
            server.url
        )
    );
    assert_eq!(error["help"], format!("if you trust that server, {line}"));
    assert_eq!(error["fix"], serde_json::Value::Null);
    assert_eq!(server.requests().len(), 1, "the clone asked nothing");
    let allowed = sync(Some(&origin))?;
    assert_eq!(allowed.status.code(), Some(0), "{}", text(&allowed.stdout)?);
    assert_eq!(server.requests().len(), 2);
    Ok(())
}

/// a typed link whose name resolves to this machine, as localhost does, gets the same hint, from
/// what the add's own lookup found: nothing is looked up again for it.
#[test]
fn a_typed_name_on_a_private_address_gets_the_same_hint() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    let link = server.url.replace("127.0.0.1", "localhost");
    let origin = link.trim_end_matches(PATH);
    let added = sandbox.run(&["add", &link])?;
    let stderr = text(&added.stderr)?;
    assert_eq!(added.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains(&format!(
            "a later run downloads from {origin}, a private address, only once allowed_origins lists it"
        )),
        "{stderr}"
    );
    Ok(())
}

/// at a terminal, the warning about a private address comes before the closing line, and that
/// line says what anyone who clones needs before their sync can reach it.
#[cfg(target_os = "linux")]
#[test]
fn a_private_link_is_told_before_the_closing_line() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    let origin = server.url.trim_end_matches(PATH);
    let Some(screen) = sandbox.on_a_terminal(&["add", &server.url])? else {
        return Ok(());
    };
    let warned = screen.find(&format!(
        "warning: a later run downloads from {origin}, a private address"
    ));
    let next = screen.find(&format!(
        "next: commit data.toml and data.lock; anyone who clones adds `allowed_origins = [\"{origin}\"]` to their settings file, then runs `fetchloom sync`\n"
    ));
    assert!(
        warned.zip(next).is_some_and(|(warned, next)| warned < next),
        "{screen}"
    );
    Ok(())
}

/// a new link or id with nothing at it is fixed on the command line, since data.toml has no entry
/// for it: the help runs the add again and names no data.toml, whether naming the dataset or
/// syncing the project met it.
#[test]
fn a_new_link_with_nothing_at_it_is_fixed_on_the_command_line() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Routes::start(Vec::new())?;
    let link = format!("{}/files/missing.csv", server.url);
    let help = "check the link in a browser. if it moved, run `fetchloom add <new link>`";
    for args in [
        vec!["--json", "add", link.as_str()],
        vec!["--json", "add", link.as_str(), "--as", "missing"],
    ] {
        let output = sandbox.run(&args)?;
        let document = json(&output)?;
        assert_eq!(output.status.code(), Some(10), "{document}");
        let error = &document["errors"][0];
        assert_eq!(error["help"], help, "{args:?}");
        for failure in error["failures"].as_array().into_iter().flatten() {
            assert_eq!(failure["help"], help, "{args:?}");
        }
    }
    assert!(!sandbox.work().join("data.toml").exists());
    // a catalog on this machine stands in for zenodo, which debug builds alone allow
    #[cfg(debug_assertions)]
    {
        let nobody = Routes::start(Vec::new())?;
        let output = sandbox
            .command(&["--json", "add", "zenodo:1"])
            .env("FETCHLOOM_TEST_ZENODO_API", &nobody.url)
            .output()?;
        assert_eq!(
            json(&output)?["errors"][0]["help"],
            "check the id in a browser, then run `fetchloom add <id>` with the right one. a dataset that was removed cannot be fetched"
        );
    }
    Ok(())
}

/// a link data.toml has and data.lock pins is fixed there should it have moved, or its pinned
/// bytes come from a folder mirror, whether the add names it again or syncs it beside a new link.
#[test]
fn a_moved_link_data_toml_has_is_fixed_there() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Routes::start(vec![
        (PATH.to_owned(), file(IRIS, "\"v1\"")),
        ("/files/wine.csv".to_owned(), file(WINE, "\"w1\"")),
    ])?;
    let iris = format!("{}{PATH}", server.url);
    let wine = format!("{}/files/wine.csv", server.url);
    let first = sandbox.run(&["add", &iris])?;
    assert_eq!(first.status.code(), Some(0), "{}", text(&first.stderr)?);
    server.change(vec![(
        PATH.to_owned(),
        reply("404 Not Found", &[("Content-Length", "0")], b""),
    )]);
    delete_folder(&sandbox.cache())?;
    delete_folder(&sandbox.work().join("data"))?;
    for link in [&iris, &wine] {
        let output = sandbox.run(&["--json", "add", link])?;
        let document = json(&output)?;
        assert_eq!(output.status.code(), Some(10), "{document}");
        assert_eq!(
            document["errors"][0]["help"],
            "to keep the pinned bytes, someone who has them runs `fetchloom push --to <folder>`, then you run `fetchloom sync --mirror <folder>`. if the file moved, put its new link as ref in data.toml",
            "{link}"
        );
    }
    // a catalog on this machine stands in for zenodo, which debug builds alone allow
    #[cfg(debug_assertions)]
    {
        let manifest = fs::read_to_string(sandbox.work().join("data.toml"))?;
        fs::write(
            sandbox.work().join("data.toml"),
            format!("{manifest}\n[datasets.record]\nref = \"zenodo:1\"\n"),
        )?;
        let nobody = Routes::start(Vec::new())?;
        let output = sandbox
            .command(&["--json", "add", "zenodo:1"])
            .env("FETCHLOOM_TEST_ZENODO_API", &nobody.url)
            .output()?;
        assert_eq!(
            json(&output)?["errors"][0]["help"],
            "check the id in a browser, then fix it on the command line or as ref in data.toml. a dataset that was removed cannot be fetched"
        );
    }
    Ok(())
}

/// an answer of `status` with `body` as JSON.
#[cfg(debug_assertions)]
fn json_answer(status: &str, body: &str) -> Vec<Part> {
    let length = body.len().to_string();
    reply(
        status,
        &[
            ("Content-Type", "application/json"),
            ("Content-Length", length.as_str()),
        ],
        body.as_bytes(),
    )
}

/// where fetchloom asks hugging face about the dataset `scikit-learn/<name>`.
#[cfg(debug_assertions)]
fn revision(name: &str) -> String {
    format!(
        "/api/datasets/scikit-learn/{name}/revision/main?expand=sha&expand=gated&expand=private&expand=tags&expand=cardData"
    )
}

/// where fetchloom asks hugging face for the datasets of `scikit-learn`.
#[cfg(debug_assertions)]
const OWNED: &str =
    "/api/datasets?author=scikit-learn&sort=downloads&direction=-1&limit=100&expand%5B%5D=private";

/// a hugging face and a kaggle on this machine that answer for a dataset they do not show as
/// both answer for a typo, hugging face with `hidden`: 401 without a key or with one it refuses,
/// 404 with one it takes. they list the datasets of its owner, `scikit-learn` and `uciml`, a
/// private one a typo away first.
#[cfg(debug_assertions)]
fn typo_sources(hidden: &str) -> Result<(Routes, Routes)> {
    let refusal = r#"{"error":"Invalid username or password."}"#;
    let hub = Routes::start(vec![
        (revision("irsi"), json_answer(hidden, refusal)),
        (revision("zzzz"), json_answer(hidden, refusal)),
        (
            OWNED.to_owned(),
            json_answer(
                "200 OK",
                r#"[{"id":"scikit-learn/irsx","private":true},{"id":"scikit-learn/wine","private":false},{"id":"scikit-learn/iris","private":false}]"#,
            ),
        ),
    ])?;
    let kaggle = Routes::start(vec![
        (
            "/v1/datasets/view/uciml/irsi".to_owned(),
            json_answer("403 Forbidden", "{}"),
        ),
        (
            "/v1/datasets/list?user=uciml&page=1".to_owned(),
            json_answer(
                "200 OK",
                r#"[{"ref":"uciml/mushroom-classification","title":"Mushrooms"},{"ref":"uciml/iris","title":"Iris Species"}]"#,
            ),
        ),
    ])?;
    Ok((hub, kaggle))
}

/// a hugging face or kaggle id one typo from a public dataset of the same owner, which its source
/// answers as it answers a private one, gets the command meant rather than a sign-in, from one
/// request to the source's own list of the owner's datasets, and exits 10 as an id the source
/// does not have. `--json` asks nothing more, so it still says to sign in.
#[cfg(debug_assertions)]
#[test]
fn a_typo_in_an_id_gets_the_command_meant() -> Result {
    let sandbox = Sandbox::new()?;
    let (hub, kaggle) = typo_sources("401 Unauthorized")?;
    let add = |args: &[&str]| {
        sandbox
            .command(args)
            .env("FETCHLOOM_TEST_HF_API", &hub.url)
            .env("FETCHLOOM_TEST_KAGGLE_API", &kaggle.url)
            .output()
    };
    // the command line comes back as typed, with only the name fixed
    for (link, meant) in [
        (
            "hf:datasets/scikit-learn/irsi",
            "fetchloom add hf:datasets/scikit-learn/iris --dry-run",
        ),
        (
            "https://www.kaggle.com/datasets/uciml/irsi",
            "fetchloom add https://www.kaggle.com/datasets/uciml/iris --dry-run",
        ),
    ] {
        let output = add(&["add", link, "--dry-run"])?;
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(10), "{stderr}");
        assert!(
            stderr.contains(&format!("help: did you mean `{meant}`?\n")),
            "{stderr}"
        );
    }
    assert_eq!(hub.requests().len(), 2);
    assert_eq!(kaggle.requests().len(), 2);
    let output = add(&["--json", "add", "hf:datasets/scikit-learn/irsi"])?;
    assert_eq!(output.status.code(), Some(40), "{}", text(&output.stdout)?);
    let help = json(&output)?["errors"][0]["help"].clone();
    assert!(
        help.as_str()
            .is_some_and(|help| help.contains("sign in with `fetchloom auth login hf`")),
        "{help}"
    );
    assert_eq!(hub.requests().len(), 3, "--json asks nothing more");
    Ok(())
}

/// at a terminal, an id one typo from a public dataset ends with the command meant and asks
/// nothing, while one with no public dataset close still offers to sign in.
#[cfg(all(debug_assertions, target_os = "linux"))]
#[test]
fn a_sign_in_is_offered_only_when_no_public_name_is_close() -> Result {
    let sandbox = Sandbox::new()?;
    let (hub, _kaggle) = typo_sources("401 Unauthorized")?;
    for (name, after, keys, asked, exit) in [
        ("irsi", "did you mean", "", false, 10),
        ("zzzz", "sign in to hugging face now?", "\r", true, 40),
    ] {
        let mut add = sandbox.command(&["add", &format!("hf:datasets/scikit-learn/{name}")]);
        add.env("FETCHLOOM_TEST_HF_API", &hub.url);
        let Some((code, screen)) = crate::support::typed(&sandbox.work(), &add, "", after, keys)?
        else {
            return Ok(());
        };
        assert_eq!(code, Some(exit), "{screen}");
        assert_eq!(
            screen.contains("sign in to hugging face now?"),
            asked,
            "{screen}"
        );
    }
    Ok(())
}

/// a signed-in person's typo gets the command meant too, whether hugging face takes their key
/// and answers 404 or refuses an old one, and when kaggle names no sign-in: none of these stops
/// the look at the owner's datasets. a private one a typo away is never named.
#[cfg(debug_assertions)]
#[test]
fn a_typo_in_an_id_gets_the_command_meant_when_signed_in() -> Result {
    let sandbox = Sandbox::new()?;
    for (hidden, link, meant) in [
        (
            "404 Not Found",
            "hf:datasets/scikit-learn/irsi",
            "hf:datasets/scikit-learn/iris",
        ),
        (
            "401 Unauthorized",
            "hf:datasets/scikit-learn/irsi",
            "hf:datasets/scikit-learn/iris",
        ),
        ("404 Not Found", "kaggle:uciml/irsi", "kaggle:uciml/iris"),
    ] {
        let (hub, kaggle) = typo_sources(hidden)?;
        let output = sandbox
            .command(&["add", link])
            .env("FETCHLOOM_TEST_HF_API", &hub.url)
            .env("FETCHLOOM_TEST_KAGGLE_API", &kaggle.url)
            .env("HF_TOKEN", "hf_FAKEfakeFAKEfakeFAKEfake1234")
            .env("KAGGLE_API_TOKEN", "KGAT_fakefakefakefakefake1234")
            .output()?;
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(10), "{hidden}: {stderr}");
        assert!(
            stderr.contains(&format!("help: did you mean `fetchloom add {meant}`?\n")),
            "{hidden}: {stderr}"
        );
    }
    Ok(())
}

/// the list of the owner's datasets is a hint the person can do without, so it gets one try:
/// a source that answers it with 429 or 500, or whose rate limit is used up, gets no further
/// request, and the error comes at once, with no suggestion.
#[cfg(debug_assertions)]
#[test]
fn a_list_of_datasets_that_fails_is_asked_once() -> Result {
    let sandbox = Sandbox::new()?;
    let add = |hub: &Routes| {
        sandbox
            .command(&["add", "hf:datasets/scikit-learn/irsi"])
            .env("FETCHLOOM_TEST_HF_API", &hub.url)
            .output()
    };
    let asked = |hub: &Routes| {
        hub.targets()
            .iter()
            .filter(|target| *target == OWNED)
            .count()
    };
    for (status, retry) in [
        ("429 Too Many Requests", "2"),
        ("500 Internal Server Error", ""),
    ] {
        let (hub, _kaggle) = typo_sources("401 Unauthorized")?;
        let mut headers = vec![("Content-Length", "0")];
        if !retry.is_empty() {
            headers.push(("Retry-After", retry));
        }
        hub.change(vec![(OWNED.to_owned(), reply(status, &headers, b""))]);
        let output = add(&hub)?;
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(40), "{stderr}");
        assert!(!stderr.contains("did you mean"), "{stderr}");
        assert_eq!(asked(&hub), 1, "{status}: {stderr}");
    }
    let (hub, _kaggle) = typo_sources("401 Unauthorized")?;
    hub.change(vec![(
        revision("irsi"),
        reply(
            "401 Unauthorized",
            &[
                ("Content-Length", "0"),
                ("X-RateLimit-Remaining", "0"),
                ("X-RateLimit-Reset", "60"),
            ],
            b"",
        ),
    )]);
    let output = add(&hub)?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(40), "{stderr}");
    assert!(!stderr.contains("did you mean"), "{stderr}");
    assert_eq!(
        asked(&hub),
        0,
        "a used-up limit is not waited for: {stderr}"
    );
    Ok(())
}

/// a link on line 3 of a list that has nothing at it fails the add naming that line, in text and
/// in json, and the help says to fix the line, since data.toml has no entry for it yet.
#[test]
fn a_missing_link_in_a_list_names_its_line() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Routes::start(vec![("/files/a.csv".to_owned(), file(b"a\n1\n", "\"a1\""))])?;
    sandbox.allow(&server.url);
    fs::write(
        sandbox.work().join("links.txt"),
        format!(
            "# inputs\n{0}/files/a.csv\n{0}/files/missing.csv\n",
            server.url
        ),
    )?;
    let at = "line 3 of links.txt";
    let message = format!(
        "{at}: 127.0.0.1 has nothing at {}/files/missing.csv (404)",
        server.url
    );
    let help = format!("check the link on {at} in a browser. if it moved, put the new link there");
    let output = sandbox.run(&["--json", "add", "-i", "links.txt"])?;
    assert_eq!(output.status.code(), Some(10), "{}", text(&output.stdout)?);
    let error = &json(&output)?["errors"][0];
    assert_eq!(error["message"], message);
    assert_eq!(error["help"], help);
    assert_eq!(error["failures"][0]["message"], message);
    let story = text(&sandbox.run(&["add", "-i", "links.txt"])?.stderr)?;
    assert!(
        story.contains(&format!("error: {message}\n  help: {help}")),
        "{story}"
    );
    assert!(!sandbox.work().join("data.toml").exists());
    Ok(())
}

/// links of a list that fail are fixed on their lines of the list, which the error names, never
/// on the command line or in data.toml: the help says so whether the links failed alike or not.
#[test]
fn links_of_a_list_that_fail_are_fixed_on_their_lines() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Routes::start(vec![
        ("/files/a.csv".to_owned(), file(b"a\n1\n", "\"a1\"")),
        (
            "/files/denied.csv".to_owned(),
            reply("403 Forbidden", &[("Content-Length", "0")], b""),
        ),
    ])?;
    sandbox.allow(&server.url);
    let help =
        "check each link in a browser. if it moved, put the new link on its line of links.txt";
    for (lines, shared) in [
        (["gone-1.csv", "a.csv", "gone-2.csv"], true),
        (["gone-1.csv", "denied.csv", "gone-2.csv"], false),
    ] {
        let list: Vec<String> = lines
            .iter()
            .map(|file| format!("{}/files/{file}", server.url))
            .collect();
        fs::write(sandbox.work().join("links.txt"), list.join("\n"))?;
        let output = sandbox.run(&["--json", "add", "-i", "links.txt"])?;
        let document = json(&output)?;
        assert_eq!(output.status.code(), Some(10), "{document}");
        if shared {
            assert_eq!(document["errors"][0]["help"], help, "{document}");
        }
        let story = text(&sandbox.run(&["add", "-i", "links.txt"])?.stderr)?;
        assert!(story.contains(&format!("help: {help}\n")), "{story}");
        assert!(!story.contains("on the command line"), "{story}");
    }
    Ok(())
}
