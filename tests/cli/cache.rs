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
            document["warnings"].to_string()
        } else {
            text(&output.stderr)?
        };
        assert!(
            report
                .lines()
                .any(|line| line.contains(digest) && line.contains("unreadable")),
            "{report}"
        );
        assert!(object.is_file());
        assert_eq!(fs::read(&object)?, b"abc");
        assert_eq!(fs::read_dir(sandbox.cache().join("changed"))?.count(), 0);
    }
    Ok(())
}
