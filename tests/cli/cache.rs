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
            assert!(document["result"].is_null());
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
    assert!(stderr.contains("0 B of it is needed by a project"), "{stderr}");
    assert!(stderr.contains("fetchloom cache gc"), "{stderr}");
    Ok(())
}
