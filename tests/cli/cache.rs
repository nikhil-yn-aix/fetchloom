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
    assert!(
        sealed && named("json").is_some(),
        "the first run left {left:?}"
    );
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
            assert_eq!(document["result"]["unreadable"], serde_json::json!([object]));
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
        fs::write(&registration, serde_json::to_vec(&serde_json::json!({"root": sandbox.work()}))?)?;
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
        assert!(document["errors"][0]["message"].as_str().unwrap().contains(&path.display().to_string()));
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
    assert_eq!(document["result"]["unreadable"], serde_json::json!([fan]));
    assert_eq!(document["result"]["objects"], 0);
    Ok(())
}

#[test]
fn verify_keeps_damage_findings_in_json() -> Result {
    let sandbox = Sandbox::new()?;
    let digest = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    let object = sandbox.cache().join("objects").join(&digest[..2]).join(&digest[2..]);
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
    for broken in ["registration", "data.toml", "data.lock", "project"] {
        let sandbox = Sandbox::new()?;
        let registrations = sandbox.cache().join("projects");
        fs::create_dir_all(&registrations)?;
        let registration = registrations.join("project.json");
        fs::write(&registration, serde_json::to_vec(&serde_json::json!({"root": sandbox.work()}))?)?;
        fs::write(sandbox.work().join("data.toml"), "")?;
        fs::write(sandbox.work().join("data.lock"), "version = 1\n")?;
        let path = match broken {
            "registration" => registration.clone(),
            "project" => sandbox.work(),
            _ => sandbox.work().join(broken),
        };
        let before = fs::metadata(&path)?.permissions();
        fs::set_permissions(&path, fs::Permissions::from_mode(0))?;
        let output = sandbox.command_in(&sandbox.folder("elsewhere")?, &["--json", "cache", "gc"]).output();
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
            document["warnings"].as_array().unwrap().iter().filter_map(serde_json::Value::as_str).collect::<Vec<_>>().join("\n")
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
    use std::time::{Duration, Instant};

    let sandbox = Sandbox::new()?;
    let digest = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    let object = sandbox.cache().join("objects").join(&digest[..2]).join(&digest[2..]);
    fs::create_dir_all(object.parent().unwrap())?;
    // a sparse file keeps hashing busy without allocating its payload on disk.
    fs::File::create(&object)?.set_len(64 * 1024 * 1024 * 1024)?;
    let mut child = sandbox.command(&["cache", "verify"]).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    let stderr = child.stderr.take().ok_or("missing child stderr")?;
    let (sender, receiver) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut report = String::new();
        for line in BufReader::new(stderr).lines().map_while(std::result::Result::ok) {
            if line.contains("Checking") {
                let _ = sender.send(());
            }
            report.push_str(&line);
            report.push('\n');
        }
        report
    });
    if receiver.recv_timeout(Duration::from_secs(10)).is_err() {
        let _ = child.kill();
        let _ = child.wait();
        let _ = reader.join();
        return Err("verification never reported that hashing started".into());
    }
    let signal = Command::new("kill").args(["-INT", &child.id().to_string()]).status()?;
    assert!(signal.success());
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            let _ = reader.join();
            return Err("verification ignored ctrl-c while hashing".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let report = reader.join().unwrap();
    assert_eq!(status.code(), Some(130), "{report}");
    assert!(report.contains("stopped"), "{report}");
    assert!(object.exists());
    assert_eq!(fs::read_dir(sandbox.cache().join("changed"))?.count(), 0);
    Ok(())
}
