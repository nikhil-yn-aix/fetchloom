//! a project's inputs: dataset names, members tables and file names.

use std::fs;

use crate::support::{Result, Sandbox, Server, file, text, zip_of};

/// a dataset's name is its folder on every machine, so neither data.toml nor a cloned data.lock
/// may give a device name, while the name add makes from a file called that works.
#[test]
fn windows_device_names_are_refused_before_any_request_or_write() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/con.csv", vec![file(b"a,b\n1,2\n", "\"v1\"")])?;
    sandbox.allow(&server.url);
    let manifest = sandbox.work().join("data.toml");
    for device in ["con", "aux", "nul", "com1", "lpt1"] {
        fs::write(
            &manifest,
            format!("[datasets.{device}]\nref = \"{}\"\n", server.url),
        )?;
        let synced = sandbox.run(&["sync"])?;
        let stderr = text(&synced.stderr)?;
        assert_eq!(synced.status.code(), Some(50), "{stderr}");
        assert!(
            stderr.starts_with(&format!(
                "error: \"{device}\" is not a valid dataset name\n"
            )),
            "{stderr}"
        );
        assert!(
            stderr.contains(&format!(
                "help: rename it to \"{device}-data\" in data.toml"
            )),
            "{stderr}"
        );
    }
    assert_eq!(server.requests().len(), 0);
    assert!(!sandbox.work().join("data.lock").exists());
    assert!(!sandbox.work().join("data").exists());

    fs::remove_file(&manifest)?;
    // a device name keeps every rule for its letters, so only another name helps
    for device in ["con", "lpt1"] {
        let named = sandbox.run(&["add", &server.url, "--as", device])?;
        let stderr = text(&named.stderr)?;
        assert_eq!(named.status.code(), Some(2), "{stderr}");
        assert!(
            stderr.ends_with(&format!(
                "error: \"{device}\" is not a valid dataset name\n  windows keeps it for a device, so it cannot be a folder there\n  help: pick another name, such as --as {device}-data\n"
            )),
            "{stderr}"
        );
    }
    assert_eq!(server.requests().len(), 0);
    assert!(!manifest.exists());
    let added = sandbox.run(&["add", &server.url])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    assert!(fs::read_to_string(&manifest)?.contains("[datasets.con-data]"));
    let lock = sandbox.work().join("data.lock");
    let cloned = fs::read_to_string(&lock)?.replace("name = \"con-data\"", "name = \"con\"");
    fs::write(&lock, &cloned)?;
    let synced = sandbox.run(&["sync"])?;
    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(50), "{stderr}");
    assert!(
        stderr.contains("data.lock names a dataset \"con\", which is not a valid dataset name"),
        "{stderr}"
    );
    assert_eq!(
        fs::read_to_string(&lock)?,
        cloned,
        "data.lock stays as it was"
    );
    Ok(())
}

/// the files are placed here as they are, and each warning names them, imported or unpacked alike.
/// its help says what lets a teammate on windows go on: leaving a listed file out, or keeping an
/// archive packed, since windows cannot unpack one holding such a name.
#[cfg(unix)]
#[test]
fn names_windows_cannot_store_are_pinned_with_one_warning_per_dataset() -> Result {
    let sandbox = Sandbox::new()?;
    let odd = sandbox.folder("odd")?;
    for name in ["what?.csv", "con.txt", "fine.csv"] {
        fs::write(odd.join(name), b"x\n")?;
    }
    let zip = zip_of(&[("pack/star*.csv", b"y\n"), ("pack/fine.csv", b"z\n")])?;
    let server = Server::start("/pack.zip", vec![file(&zip, "\"z\"")])?;
    sandbox.allow(&server.url);
    for (link, warning) in [
        (
            "../odd",
            "warning: odd has 2 files whose names windows and fat disks cannot store: con.txt and what?.csv\n  help: a teammate on windows cannot place them; leave them out with exclude in data.toml if one needs to\n",
        ),
        (
            server.url.as_str(),
            "warning: pack has a file whose name windows and fat disks cannot store: star*.csv\n  help: a teammate on windows cannot unpack the archive that holds them; keep it packed with extract = false in data.toml if one needs to\n",
        ),
    ] {
        let added = sandbox.run(&["add", link])?;
        let stderr = text(&added.stderr)?;
        assert_eq!(added.status.code(), Some(0), "{stderr}");
        assert_eq!(stderr.matches("warning:").count(), 1, "{stderr}");
        assert!(stderr.contains(warning), "{stderr}");
    }
    let work = sandbox.work();
    for placed in ["odd/what?.csv", "odd/con.txt", "pack/star*.csv"] {
        assert!(work.join("data").join(placed).is_file(), "{placed}");
    }
    Ok(())
}

/// once the members table is renamed too, the pin stays without a request.
#[test]
fn members_left_under_an_old_name_ask_to_finish_the_rename() -> Result {
    let sandbox = Sandbox::new()?;
    let zip = zip_of(&[("a1.wav", b"1"), ("a2.wav", b"2")])?;
    let server = Server::start("/glob-000000.zip", vec![file(&zip, "\"g\"")])?;
    sandbox.allow(&server.url);
    let added = sandbox.run(&["add", &server.url, "--as", "glob"])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let manifest = sandbox.work().join("data.toml");
    let members = "[datasets.glob.members.\"glob-000000.zip\"]\nexclude = [\"a1.wav\"]\n";
    let table = |name: &str| format!("[datasets.{name}]\nref = \"{}\"\n\n", server.url);
    fs::write(&manifest, format!("{}{members}", table("glob")))?;
    let filtered = sandbox.run(&["sync"])?;
    assert_eq!(
        filtered.status.code(),
        Some(0),
        "{}",
        text(&filtered.stderr)?
    );
    let requests = server.requests().len();
    fs::write(&manifest, format!("{}{members}", table("sounds")))?;
    let status = sandbox.run(&["status"])?;
    let stderr = text(&status.stderr)?;
    assert_eq!(status.status.code(), Some(50), "{stderr}");
    assert!(
        stderr.starts_with("error: datasets.glob in data.toml has members tables but no ref\n"),
        "{stderr}"
    );
    assert!(
        stderr.contains("help: if glob is now sounds, rename its members tables too, as [datasets.sounds.members.\"glob-000000.zip\"]"),
        "{stderr}"
    );
    let renamed = members.replace("glob.members", "sounds.members");
    fs::write(&manifest, format!("{}{renamed}", table("sounds")))?;
    let synced = sandbox.run(&["sync"])?;
    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("Renamed glob to sounds in data.lock · its pin stays"),
        "{stderr}"
    );
    assert_eq!(server.requests().len(), requests);
    assert!(!sandbox.work().join("data/sounds/a1.wav").exists());
    Ok(())
}

/// the error names the path too, and the help offers a dataset for each or keeping them packed.
#[test]
fn a_clash_between_two_archives_names_both() -> Result {
    let sandbox = Sandbox::new()?;
    let fold = sandbox.folder("fold")?;
    for (shard, bytes) in [("fold-000000.zip", b"0\n"), ("fold-000001.zip", b"1\n")] {
        fs::write(fold.join(shard), zip_of(&[("train/0000.txt", bytes)])?)?;
    }
    let output = sandbox.run(&["add", "../fold"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(50), "{stderr}");
    assert!(
        stderr.contains("error: two files would land on data/fold/train/0000.txt: one from fold-000000.zip and one from fold-000001.zip\n"),
        "{stderr}"
    );
    assert!(
        stderr.contains("help: pass --no-extract to keep archives packed, or give each archive a dataset of its own, whose folder keeps its files apart"),
        "{stderr}"
    );
    assert!(!sandbox.work().join("data.toml").exists());
    Ok(())
}
