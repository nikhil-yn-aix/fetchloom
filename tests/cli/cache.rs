//! what an earlier run leaves in the cache, and how the next run copes with it.

use std::fs;

use crate::support::{Result, Sandbox, Server, file, text};

const IRIS: &[u8] =
    b"sepal_length,sepal_width,petal_length,petal_width,species\n5.1,3.5,1.4,0.2,setosa\n";

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
    let left: Vec<_> = fs::read_dir(sandbox.cache().join("partial"))?
        .flatten()
        .map(|entry| entry.path())
        .collect();
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
            document["warnings"]
                .as_array()
                .ok_or("warnings is not an array")?
                .iter()
                .filter_map(serde_json::Value::as_str)
                .collect::<Vec<_>>()
                .join("\n")
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
            stderr.contains("changed and was marked for replacement"),
            "{stderr}"
        );
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
    assert!(
        stderr.contains("0 B of it is needed by a project"),
        "{stderr}"
    );
    assert!(stderr.contains("fetchloom cache gc"), "{stderr}");
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
            document["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(serde_json::Value::as_str)
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            text(&output.stderr)?
        };
        assert!(warnings.contains("gc may not keep"), "{warnings}");
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
    // sparse files, so there the cache moves to the build's folder, which usually has them.
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
    assert!(report.contains("stopped"), "{report}");
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
            assert_eq!(document["result"]["objects"], 0);
            assert_eq!(document["result"]["bytes"], 0);
            assert_eq!(document["result"]["leftovers"], 0);
            assert!(
                document["warnings"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|warning| warning
                        .as_str()
                        .unwrap()
                        .contains(&object.display().to_string()))
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
            assert_eq!(report["result"]["objects"], 1);
            assert_eq!(report["result"]["bytes"], 3);
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
                report["warnings"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|warning| warning
                        .as_str()
                        .unwrap()
                        .contains(&registration.display().to_string()))
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

/// a cache this user may not write to is named in the error a download ends with, for a person
/// and in `--json`, since "the cache" alone does not say which folder to fix.
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
