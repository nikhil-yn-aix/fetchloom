//! `add` against servers on this machine: naming a dataset, adding it again, putting the project
//! back when an add fails, and stops and cancels while it downloads.

use std::fs;

use crate::project::{Background, IRIS, PATH, added};
#[cfg(unix)]
use crate::project::{partial_bytes, told_a_stop};
#[cfg(unix)]
use crate::support::wait_until;
use crate::support::{Part, Result, Sandbox, Server, file, json, reply, text};

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
        serde_json::json!([
            "iris was taken, so this one is iris-3; choose a name with --as <name>"
        ])
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
    let mut running = Background::start(sandbox.command(&["add", &cows.url, "--as", "cows"]))?;
    cows.wait_for_requests(1)?;
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
    cows.wait_for_requests(1)?;
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
    let slow = Server::held_file("/files/cows.csv", body)?;
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
    told_a_stop(&stderr, "    Stopped, and nothing was added")?;
    assert!(!stderr.contains("could not be put back"), "{stderr}");
    assert_eq!(fs::read_to_string(work.join("data.toml"))?, edited);
    Ok(())
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
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
    let slow = Server::held_file("/files/cows.csv", body.clone())?;
    let running = Background::start(sandbox.command(&["add", &slow.url, "--as", "cows"]))?;
    wait_until(
        "bytes to arrive",
        || Ok(partial_bytes(&sandbox.cache()) > 0),
    )?;
    running.interrupt()?;
    let (code, stderr) = running.finish()?;
    assert_eq!(code, Some(130), "{stderr}");
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
        let running = Background::start(command)?;
        stalled.wait_for_requests(1)?;
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
