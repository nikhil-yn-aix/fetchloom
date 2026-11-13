//! what an earlier run leaves in the cache, and how the next run copes with it.

use std::fs;

use crate::support::{Result, Routes, Sandbox, Server, file, json, partial_files, text};

const IRIS: &[u8] =
    b"sepal_length,sepal_width,petal_length,petal_width,species\n5.1,3.5,1.4,0.2,setosa\n";
const PENGUINS: &[u8] = b"species,island,bill_length_mm\nAdelie,Torgersen,39.1\n";
const WINE: &[u8] = b"fixed_acidity,volatile_acidity,quality\n7.4,0.7,5\n";
const NOTES: &[u8] = b"measured on the gaspe peninsula\n";

/// one line counts the projects, so ten long paths never fill the screen.
#[test]
fn many_projects_are_counted_and_named_with_v() -> Result {
    let sandbox = Sandbox::new()?;
    let source = sandbox.folder("src")?;
    fs::write(source.join("a.csv"), b"a\n")?;
    for name in ["p1", "p2", "p3", "p4"] {
        let project = sandbox.folder(name)?;
        let output = sandbox.command_in(&project, &["add", "../src"]).output()?;
        assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    }
    let told = text(&sandbox.run(&["cache"])?.stderr)?;
    assert!(
        told.contains("    Used by 4 projects · `fetchloom cache -v` lists them\n"),
        "{told}"
    );
    let listed = text(&sandbox.run(&["-v", "cache"])?.stderr)?;
    assert!(listed.contains("    Used by 4 projects\n"), "{listed}");
    let last = sandbox.folder("p4")?;
    assert!(
        listed.contains(&format!("            {}\n", last.display())),
        "{listed}"
    );
    Ok(())
}

#[test]
fn a_download_sealed_but_never_published_is_fetched_again() -> Result {
    let sandbox = Sandbox::new()?;
    // the scripted server answers every request whole, as a server that ignores ranges does
    let server = Server::start(
        "/iris.csv",
        vec![file(IRIS, "\"v1\""), file(IRIS, "\"v1\"")],
    )?;
    // a file where each object folder belongs makes publishing fail once the download is sealed
    let objects = sandbox.cache().join("objects");
    fs::create_dir_all(&objects)?;
    let blockers: Vec<_> = (0..=u8::MAX)
        .map(|prefix| objects.join(format!("{prefix:02x}")))
        .collect();
    for blocker in &blockers {
        fs::write(blocker, "")?;
    }
    let first = sandbox.run(&["--json", "get", &server.url])?;
    assert_eq!(first.status.code(), Some(50), "{}", text(&first.stdout)?);
    let left = partial_files(&sandbox.cache());
    let named = |extension: &str| {
        left.iter()
            .find(|path| path.extension().is_some_and(|found| found == extension))
    };
    let sealed = named("part").is_some_and(|path| {
        fs::metadata(path).is_ok_and(|metadata| metadata.permissions().readonly())
    });
    assert!(sealed, "the first run left {left:?}");
    for blocker in &blockers {
        fs::remove_file(blocker)?;
    }

    let second = sandbox.run(&["--json", "get", &server.url])?;
    assert_eq!(second.status.code(), Some(0), "{}", text(&second.stdout)?);
    assert_eq!(
        fs::read(sandbox.work().join("iris").join("iris.csv"))?,
        IRIS
    );
    Ok(())
}

#[cfg(windows)]
#[test]
fn unreadable_objects_have_their_own_report() -> Result {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_SHARE_DELETE: u32 = 4;

    let digest = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    for json in [false, true] {
        let sandbox = Sandbox::new()?;
        let object = sandbox
            .cache()
            .join("objects")
            .join(&digest[..2])
            .join(&digest[2..]);
        fs::create_dir_all(object.parent().unwrap())?;
        fs::write(&object, b"abc")?;
        let mut permissions = fs::metadata(&object)?.permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&object, permissions)?;
        let held = fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_DELETE)
            .open(&object)?;
        let args: &[&str] = if json {
            &["--json", "cache", "verify"]
        } else {
            &["cache", "verify"]
        };
        let output = sandbox.run(args)?;
        drop(held);
        assert_eq!(output.status.code(), Some(50), "{}", text(&output.stderr)?);
        let report = if json {
            let document: serde_json::Value = serde_json::from_slice(&output.stdout)?;
            assert_eq!(document["errors"][0]["kind"], "local.io");
            assert_eq!(document["result"]["objects"], 1);
            assert_eq!(document["result"]["bytes"], 3);
            assert_eq!(document["result"]["changed"], serde_json::json!([]));
            assert_eq!(
                document["result"]["unreadable"][0]["path"],
                serde_json::json!(object)
            );
            crate::support::warned(&document).join("\n")
        } else {
            text(&output.stderr)?
        };
        assert!(
            report.lines().any(|line| {
                line.contains(&object.display().to_string()) && line.contains("unreadable")
            }),
            "{report}"
        );
        assert!(object.is_file());
        assert_eq!(fs::read(&object)?, b"abc");
        assert_eq!(fs::read_dir(sandbox.cache().join("changed"))?.count(), 0);
    }
    Ok(())
}

#[test]
fn warnings_name_the_problem_with_or_without_color() -> Result {
    let digest = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    for (choice, colored) in [("always", true), ("never", false), ("auto", false)] {
        let sandbox = Sandbox::new()?;
        let object = sandbox
            .cache()
            .join("objects")
            .join(&digest[..2])
            .join(&digest[2..]);
        fs::create_dir_all(object.parent().ok_or("object has no parent")?)?;
        fs::write(&object, b"changed")?;
        let output = sandbox
            .command(&["cache", "verify", "--color", choice])
            .env("NO_COLOR", "1")
            .env("CLICOLOR_FORCE", "1")
            .output()?;
        assert_eq!(output.status.code(), Some(30));
        let stderr = text(&output.stderr)?;
        assert!(stderr.contains("warning:"), "{stderr}");
        if colored {
            assert!(stderr.contains("\x1b[1m\x1b[33mwarning:"), "{stderr}");
        }
        assert!(
            stderr.contains(
                "has changed, so fetchloom downloads it again the next time it is needed"
            ),
            "{stderr}"
        );
        assert!(!stderr.contains("marked"), "{stderr}");
        assert_eq!(stderr.contains("\x1b["), colored, "{stderr}");
    }
    Ok(())
}

#[test]
fn an_unused_empty_file_is_not_reported_as_needed() -> Result {
    let sandbox = Sandbox::new()?;
    let empty = sandbox.run(&["cache"])?;
    assert!(text(&empty.stderr)?.contains("the cache has no data files"));
    let digest = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    let object = sandbox
        .cache()
        .join("objects")
        .join(&digest[..2])
        .join(&digest[2..]);
    fs::create_dir_all(object.parent().ok_or("object has no parent")?)?;
    fs::write(object, b"")?;
    let output = sandbox.run(&["cache"])?;
    let stderr = text(&output.stderr)?;
    assert!(stderr.contains("Unneeded 0 B"), "{stderr}");
    assert!(!stderr.contains("needed by a project"), "{stderr}");
    assert!(stderr.contains("fetchloom cache gc"), "{stderr}");
    Ok(())
}

/// linked bytes are ones deleting the cache would not free, and gc frees unneeded bytes only
/// once a day old, so nothing here is freeable yet.
#[test]
fn the_report_counts_linked_copied_and_freeable_bytes() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = Routes::start(vec![
        ("/iris.csv".to_owned(), file(IRIS, "\"i1\"")),
        ("/penguins.csv".to_owned(), file(PENGUINS, "\"p1\"")),
        ("/wine.csv".to_owned(), file(WINE, "\"w1\"")),
        ("/notes.txt".to_owned(), file(NOTES, "\"n1\"")),
    ])?;
    let url = |name: &str| format!("{}/{name}", routes.url);
    sandbox.allow(&routes.url);
    fs::write(
        sandbox.work().join("data.toml"),
        format!(
            "[datasets.iris]\nref = \"{}\"\n\n[datasets.penguins]\nref = \"{}\"\ncopy = true\n",
            url("iris.csv"),
            url("penguins.csv")
        ),
    )?;
    let downloads = sandbox.folder("downloads")?;
    for (folder, args) in [
        (sandbox.work(), vec!["sync".to_owned()]),
        (sandbox.work(), vec!["add".to_owned(), url("wine.csv")]),
        (sandbox.work(), vec!["remove".to_owned(), "wine".to_owned()]),
        (downloads, vec!["get".to_owned(), url("notes.txt")]),
    ] {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let output = sandbox.command_in(&folder, &args).output()?;
        assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    }
    let links = sandbox.links()?;
    // read-only files are links to the cache's copy where the drive makes them, else files of
    // their own, and gc frees nothing of a file a folder outside the cache links
    let (linked, copies) = if links.hardlinks_read_only() {
        (IRIS.len(), PENGUINS.len())
    } else {
        (0, IRIS.len() + PENGUINS.len())
    };
    // where the drive clones, every file of its own is a clone
    let cloned = if links.reflink { copies } else { 0 };
    let report = json(&sandbox.run(&["--json", "cache"])?)?;
    let result = &report["result"];
    assert_eq!(result["needed"], IRIS.len() + PENGUINS.len(), "{report}");
    assert_eq!(result["linked"], linked, "{report}");
    assert_eq!(result["unneeded"], WINE.len() + NOTES.len(), "{report}");
    assert_eq!(result["freeable"], 0, "{report}");
    assert_eq!(result["copies"], copies, "{report}");
    assert_eq!(result["cloned"], cloned, "{report}");
    assert_eq!(result["unlinked_copies"], 0, "{report}");
    assert_eq!(result["inputs_only"], 0, "{report}");
    let story = text(&sandbox.run(&["cache"])?.stderr)?;
    if links.hardlinks_read_only() && !links.reflink {
        sandbox.filters().bind(|| insta::assert_snapshot!(story));
    }
    Ok(())
}

/// a copy by `rsync -a` without -H holds files of its own where it had links to the cache, which
/// the report counts as copies that were linked when placed.
#[test]
fn a_project_copied_without_hardlinks_is_reported() -> Result {
    let sandbox = Sandbox::new()?;
    if !sandbox.links()?.hardlinks_read_only() {
        // its files were never linked, so a copy breaks no link
        return crate::support::skipped(
            "placed-hardlinks",
            "this disk places no read-only file as a hardlink",
        );
    }
    let server = Server::start("/iris.csv", vec![file(IRIS, "\"v1\"")])?;
    let added = sandbox.run(&["add", &server.url])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let copy = sandbox.folder("copy")?;
    let (project, cache) = (copy.join("work"), copy.join("cache"));
    copy_apart(&sandbox.work(), &project)?;
    copy_apart(&sandbox.cache(), &cache)?;
    let in_copy = |args: &[&str]| {
        sandbox
            .command_in(&project, args)
            .env("FETCHLOOM_CACHE", &cache)
            .output()
    };
    let synced = in_copy(&["sync", "--offline"])?;
    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    let report = json(&in_copy(&["--json", "cache"])?)?;
    let result = &report["result"];
    assert_eq!(result["linked"], 0, "{report}");
    assert_eq!(result["copies"], IRIS.len(), "{report}");
    assert_eq!(result["unlinked_copies"], IRIS.len(), "{report}");
    let story = text(&in_copy(&["cache"])?.stderr)?;
    assert!(
        story.contains(
            "Copies 81 B in 1 project are separate copies of cached files; all of it was linked \
             when placed"
        ),
        "{story}"
    );
    assert!(story.contains("run `fetchloom sync` there"), "{story}");
    Ok(())
}

/// copies `from` to `to` as `rsync -a` without -H does, each file a new one with the same bytes,
/// time and read-only flag, so one file under two names becomes two.
fn copy_apart(from: &std::path::Path, to: &std::path::Path) -> Result {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let path = entry?.path();
        let target = to.join(path.file_name().ok_or("an entry has no name")?);
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.is_dir() {
            copy_apart(&path, &target)?;
        } else if metadata.is_file() {
            let mut copied = fs::File::create_new(&target)?;
            std::io::copy(&mut fs::File::open(&path)?, &mut copied)?;
            copied.set_modified(metadata.modified()?)?;
            drop(copied);
            fs::set_permissions(&target, metadata.permissions())?;
        }
    }
    Ok(())
}

#[test]
fn gc_refuses_unreadable_project_records() -> Result {
    for broken in ["registration", "data.toml", "data.lock"] {
        let sandbox = Sandbox::new()?;
        let registrations = sandbox.cache().join("projects");
        fs::create_dir_all(&registrations)?;
        let registration = registrations.join("project.json");
        fs::write(
            &registration,
            serde_json::to_vec(&serde_json::json!({"root": sandbox.work()}))?,
        )?;
        fs::write(sandbox.work().join("data.toml"), "")?;
        let path = if broken == "registration" {
            registration.clone()
        } else {
            sandbox.work().join(broken)
        };
        if path.is_file() {
            fs::remove_file(&path)?;
        }
        fs::create_dir(&path)?;
        let output = sandbox.run(&["--json", "cache", "gc"])?;
        let document: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(output.status.code(), Some(50), "{document}");
        assert_eq!(document["errors"][0]["kind"], "local.io");
        assert!(
            document["errors"][0]["message"]
                .as_str()
                .unwrap()
                .contains(&path.display().to_string())
        );
        assert!(registration.exists());
    }
    Ok(())
}

#[test]
fn verify_reports_unenumerable_object_folders() -> Result {
    let sandbox = Sandbox::new()?;
    let fan = sandbox.cache().join("objects").join("ba");
    fs::create_dir_all(fan.parent().unwrap())?;
    fs::write(&fan, b"not a folder")?;
    let output = sandbox.run(&["--json", "cache", "verify"])?;
    let document: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(output.status.code(), Some(50), "{document}");
    assert_eq!(document["errors"][0]["kind"], "local.io");
    assert_eq!(
        document["result"]["unreadable"][0]["path"],
        serde_json::json!(fan)
    );
    assert_eq!(document["result"]["objects"], 0);
    Ok(())
}

#[test]
fn verify_keeps_damage_findings_in_json() -> Result {
    let sandbox = Sandbox::new()?;
    let digest = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    let object = sandbox
        .cache()
        .join("objects")
        .join(&digest[..2])
        .join(&digest[2..]);
    fs::create_dir_all(object.parent().unwrap())?;
    fs::write(&object, b"bad")?;
    let output = sandbox.run(&["--json", "cache", "verify"])?;
    let document: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(output.status.code(), Some(30), "{document}");
    assert_eq!(document["status"], "error");
    assert_eq!(document["errors"][0]["kind"], "integrity.mismatch");
    assert_eq!(document["result"]["objects"], 1);
    assert_eq!(document["result"]["bytes"], 3);
    assert_eq!(document["result"]["changed"], serde_json::json!([digest]));
    assert_eq!(document["result"]["unreadable"], serde_json::json!([]));
    Ok(())
}

/// a container user with no home of its own, as openshift runs with HOME=/, cannot fix the
/// folder's permissions, so the help also names the variable that moves the cache.
#[cfg(unix)]
#[test]
fn a_default_cache_folder_that_cannot_be_made_says_how_to_move_it() -> Result {
    use std::os::unix::fs::PermissionsExt;
    if rustix::process::geteuid().is_root() {
        return Ok(());
    }
    let sandbox = Sandbox::new()?;
    let server = Server::start("/iris.csv", vec![file(IRIS, "\"v1\"")])?;
    let home = sandbox.folder("locked-home")?;
    fs::set_permissions(&home, fs::Permissions::from_mode(0o555))?;
    let output = sandbox
        .command(&["get", &server.url])
        .env_remove("FETCHLOOM_CACHE")
        .env("HOME", &home)
        .output();
    fs::set_permissions(&home, fs::Permissions::from_mode(0o755))?;
    let output = output?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(50), "{stderr}");
    assert!(
        stderr.contains(&format!(
            "help: check its permissions, or set FETCHLOOM_CACHE to a folder fetchloom may write to, then run `fetchloom get {}` again",
            server.url
        )),
        "{stderr}"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn gc_keeps_projects_with_inaccessible_metadata() -> Result {
    use std::os::unix::fs::PermissionsExt;
    if rustix::process::geteuid().is_root() {
        return Ok(());
    }
    for broken in [
        "registration",
        "registrations",
        "data.toml",
        "data.lock",
        "project",
    ] {
        let sandbox = Sandbox::new()?;
        let registrations = sandbox.cache().join("projects");
        fs::create_dir_all(&registrations)?;
        let registration = registrations.join("project.json");
        fs::write(
            &registration,
            serde_json::to_vec(&serde_json::json!({"root": sandbox.work()}))?,
        )?;
        fs::write(sandbox.work().join("data.toml"), "")?;
        fs::write(sandbox.work().join("data.lock"), "version = 1\n")?;
        let path = match broken {
            "registration" => registration.clone(),
            "registrations" => registrations.clone(),
            "project" => sandbox.work(),
            _ => sandbox.work().join(broken),
        };
        let before = fs::metadata(&path)?.permissions();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o000))?;
        let output = sandbox
            .command_in(&sandbox.folder("elsewhere")?, &["--json", "cache", "gc"])
            .output();
        fs::set_permissions(&path, before)?;
        let output = output?;
        let document: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(output.status.code(), Some(50), "{document}");
        assert_eq!(document["errors"][0]["kind"], "local.io");
        assert!(registration.exists());
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn sync_warns_when_gc_registration_fails() -> Result {
    use std::os::unix::fs::PermissionsExt;
    if rustix::process::geteuid().is_root() {
        return Ok(());
    }
    for json in [false, true] {
        let sandbox = Sandbox::new()?;
        assert!(sandbox.run(&["cache"])?.status.success());
        fs::write(sandbox.work().join("data.toml"), "")?;
        let registrations = sandbox.cache().join("projects");
        let before = fs::metadata(&registrations)?.permissions();
        fs::set_permissions(&registrations, fs::Permissions::from_mode(0o500))?;
        let args: &[&str] = if json { &["--json", "sync"] } else { &["sync"] };
        let output = sandbox.run(args);
        fs::set_permissions(&registrations, before)?;
        let output = output?;
        assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
        let warnings = if json {
            let document: serde_json::Value = serde_json::from_slice(&output.stdout)?;
            crate::support::warned(&document).join("\n")
        } else {
            text(&output.stderr)?
        };
        assert!(
            warnings.contains(&format!(
                "could not add this project to the cache's list of projects in {}",
                sandbox.cache().display()
            )),
            "{warnings}"
        );
        assert!(
            warnings.contains("`fetchloom cache gc` may then delete files this project needs"),
            "{warnings}"
        );
        assert!(warnings.contains("permissions"), "{warnings}");
        assert!(warnings.contains("again"), "{warnings}");
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn verify_handles_ctrl_c_during_a_large_hash() -> Result {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};
    use std::time::Duration;

    let sandbox = Sandbox::new()?;
    let digest = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    // a sparse file keeps hashing busy without allocating its payload on disk. hfs+ has no
    // sparse files, so there the cache moves to the build's folder, which usually has them
    let build = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR"))?;
    let mut cache = sandbox.cache();
    let object = loop {
        let object = cache.join("objects").join(&digest[..2]).join(&digest[2..]);
        fs::create_dir_all(object.parent().unwrap())?;
        let file = fs::File::create(&object)?;
        if file.set_len(64 * 1024 * 1024 * 1024).is_ok() {
            break object;
        }
        drop(file);
        fs::remove_file(&object)?;
        if cache.starts_with(build.path()) {
            return Ok(());
        }
        cache = build.path().join("cache");
    };
    let mut child = sandbox
        .command(&["cache", "verify"])
        .env("FETCHLOOM_CACHE", &cache)
        .env("FETCHLOOM_TEST_TICKER", "on")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stderr = child.stderr.take().ok_or("missing child stderr")?;
    let (sender, receiver) = std::sync::mpsc::channel();
    let (done, ended) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut report = String::new();
        for line in BufReader::new(stderr)
            .lines()
            .map_while(std::result::Result::ok)
        {
            if line.contains("Checking") {
                let _ = sender.send(());
            }
            report.push_str(&line);
            report.push('\n');
        }
        let _ = done.send(());
        report
    });
    if receiver.recv_timeout(Duration::from_secs(10)).is_err() {
        let _ = child.kill();
        let _ = child.wait();
        let _ = reader.join();
        return Err("verification never reported that hashing started".into());
    }
    let signal = Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()?;
    assert!(signal.success());
    if ended.recv_timeout(Duration::from_secs(10)).is_err() {
        let _ = child.kill();
        let _ = child.wait();
        let _ = reader.join();
        return Err("verification ignored ctrl-c while hashing".into());
    }
    let status = child.wait()?;
    let report = reader.join().unwrap();
    assert_eq!(status.code(), Some(130), "{report}");
    assert!(
        report.contains("    Stopped at your request\n") && !report.contains("error:"),
        "{report}"
    );
    assert!(object.exists());
    assert_eq!(fs::read_dir(cache.join("changed"))?.count(), 0);
    Ok(())
}

#[cfg(windows)]
#[test]
fn gc_reports_refused_removals_to_people_and_scripts() -> Result {
    use std::os::windows::fs::OpenOptionsExt;
    use std::time::{Duration, SystemTime};

    let digest = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    for json in [false, true] {
        let sandbox = Sandbox::new()?;
        let object = sandbox
            .cache()
            .join("objects")
            .join(&digest[..2])
            .join(&digest[2..]);
        fs::create_dir_all(object.parent().unwrap())?;
        let file = fs::File::create(&object)?;
        file.set_modified(SystemTime::now() - Duration::from_hours(48))?;
        drop(file);
        let held = fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(&object)?;
        let args: &[&str] = if json {
            &["--json", "cache", "gc"]
        } else {
            &["cache", "gc"]
        };
        let output = sandbox.run(args)?;
        drop(held);
        assert_eq!(output.status.code(), Some(50));
        if json {
            let document: serde_json::Value = serde_json::from_slice(&output.stdout)?;
            assert_eq!(document["errors"][0]["kind"], "local.io");
            assert_eq!(
                document["result"]["not_removed"][0]["path"],
                serde_json::json!(object)
            );
            assert_eq!(document["result"]["removed_objects"], 0);
            assert_eq!(document["result"]["removed_bytes"], 0);
            assert_eq!(document["result"]["removed_leftovers"], 0);
            assert!(
                crate::support::warned(&document)
                    .iter()
                    .any(|warning| warning.contains(&object.display().to_string()))
            );
        } else {
            let report = text(&output.stderr)?;
            assert!(report.contains("warning:"), "{report}");
            assert!(report.contains(&object.display().to_string()), "{report}");
        }
        assert!(object.exists());
    }
    Ok(())
}

#[test]
fn gc_forgets_a_project_only_when_its_manifest_is_missing() -> Result {
    let sandbox = Sandbox::new()?;
    fs::write(sandbox.work().join("data.toml"), "")?;
    assert!(sandbox.run(&["sync"])?.status.success());
    let registrations = sandbox.cache().join("projects");
    assert_eq!(fs::read_dir(&registrations)?.count(), 1);
    fs::remove_file(sandbox.work().join("data.toml"))?;
    // a report changes nothing, since the manifest may only be on an unplugged drive
    let output = sandbox.run(&["--json", "cache"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
    assert_eq!(fs::read_dir(&registrations)?.count(), 1);
    let registration = fs::read_dir(&registrations)?
        .next()
        .ok_or("missing registration")??
        .path();
    let dry = sandbox.run(&["--json", "cache", "gc", "--dry-run"])?;
    assert!(dry.status.success(), "{}", text(&dry.stdout)?);
    let report: serde_json::Value = serde_json::from_slice(&dry.stdout)?;
    assert!(
        report["result"]["changes"]["remove"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!(registration))
    );
    assert!(registration.exists());
    let output = sandbox.run(&["--json", "cache", "gc"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
    assert_eq!(fs::read_dir(registrations)?.count(), 0);
    Ok(())
}

#[cfg(unix)]
#[test]
fn gc_reports_failed_registration_removal_with_partial_counts() -> Result {
    use std::os::unix::fs::PermissionsExt;
    if rustix::process::geteuid().is_root() {
        return Ok(());
    }
    for json in [true, false] {
        let sandbox = Sandbox::new()?;
        fs::write(sandbox.work().join("data.toml"), "")?;
        assert!(sandbox.run(&["sync"])?.status.success());
        fs::remove_file(sandbox.work().join("data.toml"))?;
        let projects = sandbox.cache().join("projects");
        let registration = fs::read_dir(&projects)?
            .next()
            .ok_or("missing registration")??
            .path();
        let object = sandbox
            .cache()
            .join("objects/ba/7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        fs::create_dir_all(object.parent().ok_or("object needs a parent")?)?;
        fs::write(&object, b"abc")?;
        let before = fs::metadata(&projects)?.permissions();
        fs::set_permissions(&projects, fs::Permissions::from_mode(0o555))?;
        let args: &[&str] = if json {
            &["--json", "cache", "gc", "--now"]
        } else {
            &["cache", "gc", "--now"]
        };
        let output = sandbox.run(args);
        fs::set_permissions(&projects, before)?;
        let output = output?;
        assert_eq!(
            output.status.code(),
            Some(50),
            "{}\n{}",
            text(&output.stdout)?,
            text(&output.stderr)?
        );
        assert!(registration.exists());
        assert!(!object.exists(), "unneeded objects can still be cleaned up");
        if json {
            let report: serde_json::Value = serde_json::from_slice(&output.stdout)?;
            assert_eq!(report["errors"][0]["kind"], "local.io");
            assert_eq!(report["result"]["removed_objects"], 1);
            assert_eq!(report["result"]["removed_bytes"], 3);
            assert_eq!(report["result"]["not_removed"].as_array().unwrap().len(), 1);
            assert_eq!(
                report["result"]["not_removed"][0]["path"],
                serde_json::json!(registration)
            );
            assert_eq!(
                report["result"]["not_removed"][0]["kind"],
                "PermissionDenied"
            );
            assert!(
                crate::support::warned(&report)
                    .iter()
                    .any(|warning| warning.contains(&registration.display().to_string()))
            );
        } else {
            let report = text(&output.stderr)?;
            assert!(report.contains("warning:"), "{report}");
            assert!(
                report.contains(&registration.display().to_string()),
                "{report}"
            );
        }
        let recovered = sandbox.run(&["--json", "cache", "gc", "--now"])?;
        assert!(recovered.status.success(), "{}", text(&recovered.stdout)?);
        assert!(!registration.exists());
    }
    Ok(())
}

#[cfg(unix)]
fn check_unreadable_change_marks(mode: Option<u32>) -> Result {
    use std::os::unix::fs::PermissionsExt;
    if rustix::process::geteuid().is_root() {
        return Ok(());
    }
    let sandbox = Sandbox::new()?;
    let server = Server::start(
        "/good.bin",
        vec![file(b"good", "\"v1\""), file(b"good", "\"v1\"")],
    )?;
    sandbox.allow(&server.url);
    fs::write(
        sandbox.work().join("data.toml"),
        format!("[datasets.sample]\nref = \"{}\"\ncopy = true\n", server.url),
    )?;
    let prepared = sandbox.run(&["sync"])?;
    assert!(prepared.status.success(), "{}", text(&prepared.stderr)?);
    let state: serde_json::Value =
        serde_json::from_slice(&fs::read(sandbox.work().join(".fetchloom/state"))?)?;
    let digest = state["files"]["data/sample/good.bin"]["sha256"]
        .as_str()
        .ok_or("a placed file has a digest")?;
    let object = sandbox
        .cache()
        .join("objects")
        .join(&digest[..2])
        .join(&digest[2..]);
    crate::support::edit(&object, b"evil")?;
    let mut permissions = fs::metadata(&object)?.permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&object, permissions)?;
    let verified = sandbox.run(&["--json", "cache", "verify"])?;
    assert_eq!(
        verified.status.code(),
        Some(30),
        "{}",
        text(&verified.stdout)?
    );
    let copier = sandbox.folder("copier")?;
    for name in ["data.toml", "data.lock"] {
        fs::copy(sandbox.work().join(name), copier.join(name))?;
    }
    let changed = sandbox.cache().join("changed");
    let unreadable = if mode.is_some() {
        changed.clone()
    } else {
        changed.join(digest)
    };
    let before = fs::metadata(&unreadable)?.permissions();
    fs::set_permissions(&unreadable, fs::Permissions::from_mode(mode.unwrap_or(0)))?;
    let output = sandbox
        .command_in(&copier, &["--json", "sync", "--offline"])
        .output();
    fs::set_permissions(&unreadable, before)?;
    let output = output?;
    let report: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(output.status.code(), Some(50), "{report}");
    assert_eq!(report["errors"][0]["kind"], "local.io", "{report}");
    assert!(
        report
            .to_string()
            .contains(&unreadable.display().to_string()),
        "{report}"
    );
    let message = report["errors"][0]["message"].as_str().unwrap_or_default();
    assert!(
        message.starts_with(&format!(
            "could not read the cache's list of changed files at {}",
            unreadable.display()
        )),
        "{report}"
    );
    assert!(!message.contains("mark"), "{report}");
    assert!(
        !copier.join("data/sample/good.bin").exists(),
        "unknown cache bytes were placed"
    );
    let state_path = copier.join(".fetchloom/state");
    if state_path.exists() {
        let state: serde_json::Value = serde_json::from_slice(&fs::read(&state_path)?)?;
        assert_ne!(state["synced"]["complete"], true, "{state}");
    }
    let recovered = sandbox.command_in(&copier, &["--json", "sync"]).output()?;
    assert_eq!(
        recovered.status.code(),
        Some(0),
        "{}",
        text(&recovered.stdout)?
    );
    assert_eq!(fs::read(copier.join("data/sample/good.bin"))?, b"good");
    let state: serde_json::Value = serde_json::from_slice(&fs::read(state_path)?)?;
    assert_eq!(state["synced"]["complete"], true, "{state}");
    Ok(())
}

#[cfg(unix)]
#[test]
fn unenumerable_change_marks_stop_sync_before_placing() -> Result {
    check_unreadable_change_marks(Some(0o111))
}

#[cfg(unix)]
#[test]
fn inaccessible_change_marks_stop_sync_before_placing() -> Result {
    check_unreadable_change_marks(Some(0o000))
}

#[cfg(unix)]
#[test]
fn an_unreadable_change_mark_stops_sync_before_placing() -> Result {
    check_unreadable_change_marks(None)
}

#[cfg(unix)]
#[test]
fn unreadable_change_marks_stop_a_settled_sync() -> Result {
    use std::os::unix::fs::PermissionsExt;
    if rustix::process::geteuid().is_root() {
        return Ok(());
    }
    let sandbox = Sandbox::new()?;
    let server = Server::start("/good.bin", vec![file(b"good", "\"v1\"")])?;
    sandbox.allow(&server.url);
    fs::write(
        sandbox.work().join("data.toml"),
        format!("[datasets.sample]\nref = \"{}\"\n", server.url),
    )?;
    assert!(sandbox.run(&["sync"])?.status.success());
    let state_path = sandbox.work().join(".fetchloom/state");
    let state = fs::read(&state_path)?;
    let changed = sandbox.cache().join("changed");
    let before = fs::metadata(&changed)?.permissions();
    fs::set_permissions(&changed, fs::Permissions::from_mode(0o111))?;
    let output = sandbox.run(&["--json", "sync", "--offline"]);
    fs::set_permissions(&changed, before)?;
    let output = output?;
    assert_eq!(output.status.code(), Some(50), "{}", text(&output.stdout)?);
    assert_eq!(fs::read(state_path)?, state);
    Ok(())
}

#[test]
fn small_downloads_keep_their_bytes_and_sealing() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/abc.bin", vec![file(b"abc", "\"v1\"")])?;
    let output = sandbox.command(&["--json", "get", &server.url]).output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
    assert_eq!(fs::read(sandbox.work().join("abc/abc.bin"))?, b"abc");
    let object = sandbox
        .cache()
        .join("objects/ba/7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    assert_eq!(fs::read(&object)?, b"abc");
    assert!(fs::metadata(object)?.permissions().readonly());
    Ok(())
}

/// both forms name the folder, since "the cache" alone does not say which one to fix.
#[cfg(unix)]
#[test]
fn a_cache_that_cannot_be_written_is_named_in_the_error() -> Result {
    use std::os::unix::fs::PermissionsExt;
    if rustix::process::geteuid().is_root() {
        return Ok(());
    }
    let sandbox = Sandbox::new()?;
    let server = Server::start("/iris.csv", vec![file(IRIS, "\"v1\"")])?;
    assert!(sandbox.run(&["cache"])?.status.success());
    let mut folders = vec![sandbox.cache()];
    for entry in fs::read_dir(sandbox.cache())? {
        let path = entry?.path();
        if path.is_dir() {
            folders.push(path);
        }
    }
    let before: Vec<_> = folders
        .iter()
        .map(|folder| fs::metadata(folder).map(|metadata| metadata.permissions()))
        .collect::<std::io::Result<_>>()?;
    for folder in &folders {
        fs::set_permissions(folder, fs::Permissions::from_mode(0o555))?;
    }
    let people = sandbox.run(&["get", &server.url]);
    let scripts = sandbox.run(&["--json", "get", &server.url]);
    for (folder, permissions) in folders.iter().zip(before) {
        fs::set_permissions(folder, permissions)?;
    }
    let (people, scripts) = (people?, scripts?);
    let refused = format!(
        "could not write to the cache: permission was denied in {}",
        sandbox.cache().join("partial").display()
    );
    let stderr = text(&people.stderr)?;
    assert_eq!(people.status.code(), Some(50), "{stderr}");
    assert!(stderr.contains(&refused), "{stderr}");
    assert!(stderr.contains("help: check its permissions"), "{stderr}");
    let report: serde_json::Value = serde_json::from_slice(&scripts.stdout)?;
    assert_eq!(scripts.status.code(), Some(50), "{report}");
    assert_eq!(report["errors"][0]["kind"], "local.io", "{report}");
    assert_eq!(report["errors"][0]["message"], refused.as_str(), "{report}");
    Ok(())
}

/// the registration is flushed already, so a projects folder it cannot open warns of nothing.
#[cfg(unix)]
#[test]
fn a_sync_with_nothing_to_do_leaves_its_registration_alone() -> Result {
    use std::os::unix::fs::PermissionsExt;
    if rustix::process::geteuid().is_root() {
        return Ok(());
    }
    let sandbox = Sandbox::new()?;
    fs::write(sandbox.work().join("data.toml"), "")?;
    let first = sandbox.run(&["sync"])?;
    assert_eq!(first.status.code(), Some(0), "{}", text(&first.stderr)?);
    let registrations = sandbox.cache().join("projects");
    let before = fs::metadata(&registrations)?.permissions();
    // writable and searchable, but no longer readable, so opening it to flush it fails
    fs::set_permissions(&registrations, fs::Permissions::from_mode(0o333))?;
    let again = sandbox.run(&["sync"]);
    fs::set_permissions(&registrations, before)?;
    let again = again?;
    let stderr = text(&again.stderr)?;
    assert_eq!(again.status.code(), Some(0), "{stderr}");
    assert!(
        !stderr.contains("could not add this project to the cache's list of projects"),
        "{stderr}"
    );
    Ok(())
}

/// a moved project folder leaves its registration with no data.toml behind; gc names it to a
/// person and in `--json`, and so does a dry run with `-v`.
#[test]
fn gc_names_each_project_it_forgets() -> Result {
    for json in [false, true] {
        let sandbox = Sandbox::new()?;
        let server = Server::start("/iris.csv", vec![file(IRIS, "\"v1\"")])?;
        let project = sandbox.folder("thesis")?;
        let added = sandbox
            .command_in(&project, &["add", &server.url])
            .output()?;
        assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
        fs::rename(&project, project.with_file_name("thesis-moved"))?;
        let shown = project.display().to_string();
        let gc = |dry: bool| {
            let mut args = vec!["cache", "gc", "--now"];
            if dry {
                args.extend(["--dry-run", "-v"]);
            }
            if json {
                args.insert(0, "--json");
            }
            sandbox.run(&args)
        };
        for dry in [true, false] {
            let output = gc(dry)?;
            assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
            if json {
                let report: serde_json::Value = serde_json::from_slice(&output.stdout)?;
                assert_eq!(
                    report["result"]["forgotten"],
                    serde_json::json!([shown]),
                    "{report}"
                );
            } else {
                let stderr = text(&output.stderr)?;
                let told = if dry {
                    format!("would forget {shown}, which has no data.toml anymore")
                } else {
                    format!("Forgot {shown} · it has no data.toml anymore")
                };
                assert!(stderr.contains(&told), "{stderr}");
            }
        }
    }
    Ok(())
}

/// an interrupted download keeps its bytes so a later run can continue it; `--now` frees them
/// however new, but never those a running fetchloom holds.
#[test]
fn unfinished_downloads_are_shown_and_freed_by_gc_now() -> Result {
    let sandbox = Sandbox::new()?;
    assert!(sandbox.run(&["cache"])?.status.success());
    // each download sits in one of the folders under partial/, named by its key's first digit
    let partial = sandbox.cache().join("partial");
    let at =
        |key: &str, extension: &str| partial.join(&key[..1]).join(format!("{key}.{extension}"));
    let left = "ab".repeat(32);
    let running = "cd".repeat(32);
    for (key, size) in [(&left, 3000), (&running, 1000)] {
        fs::create_dir_all(partial.join(&key[..1]))?;
        fs::write(at(key, "part"), vec![7_u8; size])?;
        fs::write(at(key, "json"), b"{}")?;
        fs::write(at(key, "lock"), b"")?;
    }
    let info = sandbox.run(&["--json", "cache"])?;
    let report: serde_json::Value = serde_json::from_slice(&info.stdout)?;
    assert_eq!(report["result"]["unfinished_downloads"], 2, "{report}");
    let held = on_disk(&at(&left, "part"))? + on_disk(&at(&running, "part"))?;
    assert_eq!(report["result"]["unfinished_bytes"], held, "{report}");
    let told = text(&sandbox.run(&["cache"])?.stderr)?;
    assert!(told.contains("Unfinished 2 downloads · "), "{told}");
    let freed = on_disk(&at(&left, "part"))? + on_disk(&at(&left, "json"))?;
    let young = sandbox.run(&["cache", "gc"])?;
    assert_eq!(young.status.code(), Some(0), "{}", text(&young.stderr)?);
    assert!(at(&left, "part").exists(), "a day's grace without --now");
    let held = fs::File::options().write(true).open(at(&running, "lock"))?;
    held.lock()?;
    let now = sandbox.run(&["--json", "cache", "gc", "--now"])?;
    drop(held);
    let report: serde_json::Value = serde_json::from_slice(&now.stdout)?;
    assert_eq!(now.status.code(), Some(0), "{report}");
    assert!(!at(&left, "part").exists(), "{report}");
    assert!(!at(&left, "json").exists(), "{report}");
    assert!(at(&running, "part").exists(), "{report}");
    assert_eq!(
        report["result"]["removed_leftover_bytes"], freed,
        "{report}"
    );
    Ok(())
}

/// what the file at `path` takes on its disk, as fetchloom counts unfinished downloads: its
/// blocks on unix, and its length on windows, which makes no holes in them.
fn on_disk(path: &std::path::Path) -> Result<u64> {
    let metadata = fs::metadata(path)?;
    #[cfg(unix)]
    let held = std::os::unix::fs::MetadataExt::blocks(&metadata) * 512;
    #[cfg(not(unix))]
    let held = metadata.len();
    Ok(held)
}

/// the mark comes once gc has swept the cache or verify has hashed it, and the exit is 0.
#[cfg(unix)]
#[test]
fn a_ctrl_c_past_the_caches_mark_lets_it_tell_what_it_did() -> Result {
    for (args, told) in [
        (
            ["cache", "gc", "--now"].as_slice(),
            "Removed 1 file \u{b7} 3 B from the cache",
        ),
        (
            ["cache", "verify"].as_slice(),
            "Verified 1 file \u{b7} 3 B, every one is intact",
        ),
    ] {
        let sandbox = Sandbox::new()?;
        let digest = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        let fan = sandbox.cache().join("objects").join(&digest[..2]);
        fs::create_dir_all(&fan)?;
        fs::write(fan.join(&digest[2..]), b"abc")?;
        let (running, release) =
            crate::support::held_at_its_mark(&sandbox, &mut sandbox.command(args))?;
        let stderr = crate::support::finished_anyway(running, &release)?;
        assert!(stderr.contains(told), "{args:?}: {stderr}");
    }
    Ok(())
}
