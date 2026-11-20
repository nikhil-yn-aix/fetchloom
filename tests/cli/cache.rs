//! what an earlier run leaves in the cache, and how the next run copes with it.

use std::fs;

use crate::support::{Result, Routes, Sandbox, Server, file, json, ok, partial_files, text};

const IRIS: &[u8] =
    b"sepal_length,sepal_width,petal_length,petal_width,species\n5.1,3.5,1.4,0.2,setosa\n";
const PENGUINS: &[u8] = b"species,island,bill_length_mm\nAdelie,Torgersen,39.1\n";
const WINE: &[u8] = b"fixed_acidity,volatile_acidity,quality\n7.4,0.7,5\n";
const NOTES: &[u8] = b"measured on the gaspe peninsula\n";
/// the help for a cache read-only for this account, as a lab's shared cache may be for everyone but
/// whoever keeps it.
#[cfg(unix)]
const SHARED: &str = "help: the cache is read-only for you, as a shared cache may be: set FETCHLOOM_CACHE to a folder you may write to, or ask whoever keeps the shared cache to add what you need";

/// a run reads the cache's tag once, however many of its steps ask whether the folder is the
/// cache: one that opens the cache to place a file, one that only reads it, and the first.
#[test]
fn a_run_reads_the_caches_tag_once() -> Result {
    let sandbox = Sandbox::new()?;
    let source = sandbox.folder("src")?;
    fs::write(source.join("a.csv"), b"a\n")?;
    let traces = sandbox.folder("traces")?;
    let placed = sandbox.work().join("data").join("src").join("a.csv");
    for (run, args) in [
        &["add", "../src"][..],
        &["sync"],
        &["status"],
        &["sync", "--dry-run"],
    ]
    .into_iter()
    .enumerate()
    {
        if run == 1 {
            crate::support::delete(&placed)?;
        }
        let trace = traces.join(format!("{run}.json"));
        let output = sandbox
            .command(args)
            .env("FETCHLOOM_TRACE", &trace)
            .output()?;
        assert_eq!(
            output.status.code(),
            Some(0),
            "{args:?}: {}",
            text(&output.stderr)?
        );
        let events: Vec<serde_json::Value> = serde_json::from_slice(&fs::read(&trace)?)?;
        let reads = events
            .iter()
            .filter(|event| event["ph"] == "B" && event["name"] == "read_tag")
            .count();
        assert_eq!(reads, 1, "{args:?}");
    }
    Ok(())
}

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
    sandbox.make_cache(&sandbox.cache())?;
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
        sandbox.make_cache(&sandbox.cache())?;
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
        sandbox.make_cache(&sandbox.cache())?;
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
        assert!(!sandbox.unrooted(&stderr).contains("marked"), "{stderr}");
        assert_eq!(stderr.contains("\x1b["), colored, "{stderr}");
    }
    Ok(())
}

/// `status` warns, as `sync` and `fetchloom cache` do, when other users may change the files
/// projects place from the cache, as through a folder in it every user may write.
#[cfg(unix)]
#[test]
fn status_warns_of_a_cache_others_may_change() -> Result {
    use std::os::unix::fs::PermissionsExt;
    let sandbox = Sandbox::new()?;
    let source = sandbox.folder("measured")?;
    fs::write(source.join("m.csv"), b"measured\n")?;
    let output = sandbox.run(&["add", &source.display().to_string(), "--as", "meas"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    fs::set_permissions(sandbox.cache(), fs::Permissions::from_mode(0o755))?;
    let objects = sandbox.cache().join("objects");
    fs::set_permissions(&objects, fs::Permissions::from_mode(0o777))?;

    let output = sandbox.run(&["status"])?;

    let stderr = text(&output.stderr)?;
    assert!(
        stderr.contains(&format!(
            "warning: the folder {} in the cache lets every user of this machine write to it",
            objects.display()
        )),
        "{stderr}"
    );
    Ok(())
}

/// a cached file every user may write, though fetchloom keeps each read-only, lets them change
/// the files projects placed from it, so the cache report warns of it.
#[cfg(unix)]
#[test]
fn the_cache_report_warns_of_a_file_others_may_write() -> Result {
    use std::os::unix::fs::PermissionsExt;
    let sandbox = Sandbox::new()?;
    let source = sandbox.folder("measured")?;
    fs::write(source.join("m.csv"), b"measured\n")?;
    let output = sandbox.run(&["add", &source.display().to_string(), "--as", "meas"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    let mut objects = Vec::new();
    for fan in fs::read_dir(sandbox.cache().join("objects"))? {
        for entry in fs::read_dir(fan?.path())? {
            objects.push(entry?.path());
        }
    }
    let [object] = objects.as_slice() else {
        return Err(format!("one object, not {objects:?}").into());
    };
    fs::set_permissions(object, fs::Permissions::from_mode(0o666))?;

    let output = sandbox.run(&["cache"])?;

    let stderr = text(&output.stderr)?;
    assert!(
        stderr.contains(&format!(
            "warning: the file {} in the cache lets every user of this machine write to it",
            object.display()
        )),
        "{stderr}"
    );
    Ok(())
}

/// in a cache a group shares, a setgid folder its group may write as a lab makes it, a folder a
/// run made without `umask 002` lets only its owner write to it, so the group's other members
/// cannot add to it: a run that uses the cache says so.
#[cfg(unix)]
#[test]
fn a_folder_the_group_cannot_write_in_a_shared_cache_is_told() -> Result {
    use std::os::unix::fs::PermissionsExt;
    let sandbox = Sandbox::new()?;
    fs::create_dir_all(sandbox.cache())?;
    fs::set_permissions(sandbox.cache(), fs::Permissions::from_mode(0o2770))?;
    let source = sandbox.folder("measured")?;
    fs::write(source.join("m.csv"), b"measured\n")?;
    let output = sandbox.run(&["add", &source.display().to_string(), "--as", "meas"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    // every folder as `umask 002` makes it, but one fan as a run with the usual 022 left it
    for entry in fs::read_dir(sandbox.cache())? {
        let path = entry?.path();
        if path.is_dir() {
            fs::set_permissions(&path, fs::Permissions::from_mode(0o2770))?;
        }
    }
    let objects = sandbox.cache().join("objects");
    let fan = fs::read_dir(&objects)?
        .next()
        .ok_or("no folder in objects")??
        .path();
    fs::set_permissions(&fan, fs::Permissions::from_mode(0o2755))?;

    let output = sandbox.run(&["--json", "sync"])?;

    let document = json(&output)?;
    let told = document["warnings"]
        .as_array()
        .and_then(|warnings| {
            warnings
                .iter()
                .find(|warning| warning["kind"] == "cache.unshared")
        })
        .ok_or_else(|| format!("no cache.unshared warning: {document}"))?;
    assert!(
        told["message"]
            .as_str()
            .is_some_and(|message| message.contains(&fan.display().to_string())),
        "{told}"
    );
    assert_eq!(
        told["help"],
        "whoever owns a folder the group cannot write gives the group its write with `chmod g+rwx`",
        "{told}"
    );
    Ok(())
}

/// a lab's cache is a setgid folder its group may write. what a run makes there with the usual
/// `umask 022` lets the group's other members add to each folder, as `umask 002` would, and with
/// `umask 077` read each file too; nothing becomes writable by every user, and an object stays
/// read-only for everyone. a run as root in the group, as a lab's container may be, gives the
/// group the same, though only what it makes is root's.
#[cfg(unix)]
#[test]
fn the_group_may_add_to_what_a_run_makes_in_its_cache() -> Result {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    // no test runs as root, so a debug build is told it does, and what it makes stays this user's
    for (umask, euid) in [("022", None), ("077", None), ("022", Some("0"))] {
        let sandbox = Sandbox::new()?;
        let cache = sandbox.cache();
        fs::create_dir_all(&cache)?;
        fs::set_permissions(&cache, fs::Permissions::from_mode(0o2770))?;
        if fs::metadata(&cache)?.mode() & 0o7777 != 0o2770 {
            return crate::support::skipped("permissions", "this disk keeps no setgid folder");
        }
        let source = sandbox.folder("measured")?;
        fs::write(source.join("m.csv"), b"measured\n")?;
        let server = Server::start("/iris.csv", vec![file(IRIS, "\"v1\"")])?;
        let notes = crate::support::zip_of(&[("notes.txt", NOTES)])?;
        let archive = Server::start("/notes.zip", vec![file(&notes, "\"z1\"")])?;
        let source = source.display().to_string();
        for args in [
            ["--json", "add", source.as_str(), "--as", "meas"],
            ["--json", "add", server.url.as_str(), "--as", "iris"],
            ["--json", "add", archive.url.as_str(), "--as", "notes"],
        ] {
            let mut command = sandbox.command(&args);
            if let Some(euid) = euid {
                command.env("FETCHLOOM_TEST_EUID", euid);
            }
            let output = command.with_umask(umask).output()?;
            assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
            // the umask no longer matters to what the group may do in the cache
            let document = json(&output)?;
            let kinds = crate::support::kinds(&document);
            assert!(
                !kinds.contains(&"cache.unshared"),
                "umask {umask} {euid:?}: {document}"
            );
        }

        let group = fs::metadata(&cache)?.gid();
        let mut wrong = Vec::new();
        for (path, found) in below(&cache)? {
            let mode = found.mode() & 0o7777;
            let shown = format!("{mode:o} {}", path.display());
            if mode & 0o002 != 0 {
                wrong.push(format!("every user may write {shown}"));
            }
            if found.is_dir() {
                if mode & 0o070 != 0o070 || found.gid() != group {
                    wrong.push(format!("the group cannot add to {shown}"));
                }
                continue;
            }
            if mode & 0o040 == 0 {
                wrong.push(format!("the group cannot read {shown}"));
            }
            if mode & 0o200 != 0 && mode & 0o020 == 0 {
                wrong.push(format!("only its owner may write {shown}"));
            }
            if path.starts_with(cache.join("objects")) && mode & 0o222 != 0 {
                wrong.push(format!("an object may be written {shown}"));
            }
        }
        assert!(
            wrong.is_empty(),
            "umask {umask} {euid:?}:\n{}",
            wrong.join("\n")
        );
    }
    Ok(())
}

/// a cache no group shares, as a person's own, or a folder that is not setgid or that its group
/// may not write, keeps the modes the umask gives what a run makes there.
#[cfg(unix)]
#[test]
fn a_cache_no_group_shares_keeps_the_umask() -> Result {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    for root in [None, Some(0o770), Some(0o2750)] {
        let sandbox = Sandbox::new()?;
        let cache = sandbox.cache();
        if let Some(root) = root {
            fs::create_dir_all(&cache)?;
            fs::set_permissions(&cache, fs::Permissions::from_mode(root))?;
            if fs::metadata(&cache)?.mode() & 0o7777 != root {
                return crate::support::skipped("permissions", "this disk keeps no setgid folder");
            }
        }
        let source = sandbox.folder("measured")?;
        fs::write(source.join("m.csv"), b"measured\n")?;
        let source = source.display().to_string();
        let output = sandbox
            .command(&["add", &source, "--as", "meas"])
            .with_umask("022")
            .output()?;
        assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);

        let given: Vec<String> = below(&cache)?
            .into_iter()
            .filter(|(_, found)| found.mode() & 0o020 != 0)
            .map(|(path, found)| format!("{:o} {}", found.mode() & 0o7777, path.display()))
            .collect();
        let root = root.map_or_else(
            || "made by fetchloom".to_owned(),
            |root| format!("{root:o}"),
        );
        assert!(given.is_empty(), "root {root}:\n{}", given.join("\n"));
    }
    Ok(())
}

/// every file and folder below `folder`, with what it is, never looked at through a link.
#[cfg(unix)]
fn below(folder: &std::path::Path) -> Result<Vec<(std::path::PathBuf, fs::Metadata)>> {
    let mut found = Vec::new();
    for entry in fs::read_dir(folder)? {
        let path = entry?.path();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.is_dir() {
            found.extend(below(&path)?);
        }
        found.push((path, metadata));
    }
    Ok(found)
}

/// a changed file imported from a folder comes back from no source, so verify says so, and its
/// help names what does bring it back rather than promise a download sync cannot make.
#[test]
fn verify_tells_a_changed_import_apart() -> Result {
    let sandbox = Sandbox::new()?;
    let source = sandbox.folder("measured")?;
    fs::write(source.join("m.csv"), b"measured 1\n")?;
    let output = sandbox.run(&["add", &source.display().to_string(), "--as", "meas"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    let mut objects = Vec::new();
    for fan in fs::read_dir(sandbox.cache().join("objects"))? {
        for entry in fs::read_dir(fan?.path())? {
            objects.push(entry?.path());
        }
    }
    let [object] = objects.as_slice() else {
        return Err(format!("one object, not {objects:?}").into());
    };
    let mut permissions = fs::metadata(object)?.permissions();
    #[expect(
        clippy::permissions_set_readonly_false,
        reason = "the test changes a cached object as an edit through a link would"
    )]
    permissions.set_readonly(false);
    fs::set_permissions(object, permissions)?;
    fs::write(object, b"measured 2\n")?;

    let output = sandbox.run(&["--json", "cache", "verify"])?;

    let document = json(&output)?;
    assert_eq!(output.status.code(), Some(30), "{document}");
    let warning = document["warnings"][0]["message"]
        .as_str()
        .unwrap_or_default();
    assert_eq!(
        warning,
        format!(
            "{} has changed, and it was imported from files, which no source gives back",
            object.display()
        ),
        "{document}"
    );
    let help = document["errors"][0]["help"].as_str().unwrap_or_default();
    assert!(
        help.contains("fetchloom add <folder> --as <name>"),
        "{document}"
    );
    assert!(!warning.contains("downloads it again"), "{document}");
    Ok(())
}

#[test]
fn an_unused_empty_file_is_not_reported_as_needed() -> Result {
    let sandbox = Sandbox::new()?;
    let empty = sandbox.run(&["cache"])?;
    assert!(text(&empty.stderr)?.contains("the cache has no data files"));
    sandbox.make_cache(&sandbox.cache())?;
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
    let story = text(&sandbox.run(&["cache"])?.stderr)?;
    if links.hardlinks_read_only() && !links.reflink {
        // what files take on disk follows the disk's block size, and windows does not say it
        let mut filters = sandbox.filters();
        filters.add_filter(r", [0-9.]+ [KMGT]?i?B on disk", "");
        filters.bind(|| insta::assert_snapshot!(story));
    }
    Ok(())
}

/// a data folder put back from a copy made by `rsync -a` without -H holds files of its own where it
/// had links to the cache, which the report counts as copies that were linked when placed, until
/// a sync in the project links them again.
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
    let data = sandbox.work().join("data");
    let backup = sandbox.folder("backup")?;
    crate::support::copy_apart(&data, &backup)?;
    crate::support::delete_folder(&data)?;
    crate::support::copy_apart(&backup, &data)?;
    let report = json(&sandbox.run(&["--json", "cache"])?)?;
    let result = &report["result"];
    assert_eq!(result["linked"], 0, "{report}");
    assert_eq!(result["copies"], IRIS.len(), "{report}");
    assert_eq!(result["unlinked_copies"], IRIS.len(), "{report}");
    let story = text(&sandbox.run(&["cache"])?.stderr)?;
    assert!(
        story.contains(
            "Copies 81 B in 1 project are separate copies of cached files; all of it was linked \
             when placed"
        ),
        "{story}"
    );
    assert!(
        story.contains("hint: `fetchloom sync` in a project links its copies to the cache again"),
        "{story}"
    );
    Ok(())
}

#[test]
fn gc_refuses_unreadable_project_records() -> Result {
    for broken in ["registration", "data.toml", "data.lock"] {
        let sandbox = Sandbox::new()?;
        sandbox.make_cache(&sandbox.cache())?;
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

/// a folder whose names are a cache's, as a scratch folder's may be, is no cache without
/// fetchloom's tag: gc, which clears old files from tmp/, refuses it, and nothing is added to it.
#[test]
fn gc_refuses_a_folder_with_a_caches_names_and_no_tag() -> Result {
    use std::time::{Duration, SystemTime};
    let folders: [&[&str]; 2] = [
        &["tmp/notes.txt", "objects/pack1"],
        &["projects/myapp/main.py"],
    ];
    for files in folders {
        let sandbox = Sandbox::new()?;
        let cache = sandbox.cache();
        let days_ago = SystemTime::now() - Duration::from_hours(72);
        for file in files {
            let path = cache.join(file);
            fs::create_dir_all(path.parent().ok_or("a file needs a folder")?)?;
            fs::write(&path, b"a person's file")?;
            fs::File::options()
                .write(true)
                .open(&path)?
                .set_modified(days_ago)?;
        }
        let before = entries(&cache)?;
        for args in [
            &["cache", "gc"][..],
            &["cache", "gc", "--now"],
            &["cache", "verify"],
        ] {
            let output = sandbox.run(args)?;
            let stderr = text(&output.stderr)?;
            assert_eq!(output.status.code(), Some(50), "{args:?}: {stderr}");
            assert!(
                stderr.contains("it holds other files and is not a fetchloom cache"),
                "{args:?}: {stderr}"
            );
            assert_eq!(entries(&cache)?, before, "{args:?}");
        }
    }
    Ok(())
}

/// a folder of objects/ this user may not write, as each one a container run as root makes there
/// is, refuses the files whose hash falls in it: the error names that folder, not objects/.
#[cfg(unix)]
#[test]
fn a_cache_folder_that_refuses_a_write_is_named() -> Result {
    use std::os::unix::fs::PermissionsExt;
    if rustix::process::geteuid().is_root() {
        return Ok(());
    }
    let sandbox = Sandbox::new()?;
    let cache = sandbox.cache();
    sandbox.make_cache(&cache)?;
    let source = sandbox.folder("src")?;
    // the SHA-256 of these bytes starts with 87
    fs::write(source.join("a.csv"), b"a\n")?;
    let fan = cache.join("objects").join("87");
    fs::create_dir(&fan)?;
    fs::set_permissions(&fan, fs::Permissions::from_mode(0o555))?;
    let output = sandbox.run(&["add", "../src/a.csv"]);
    fs::set_permissions(&fan, fs::Permissions::from_mode(0o755))?;
    let output = output?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(50), "{stderr}");
    assert!(
        stderr.contains(&format!(
            "error: could not put a.csv in the cache: permission was denied in {}\n",
            fan.display()
        )),
        "{stderr}"
    );
    Ok(())
}

/// a run as root takes no cache another user owns, as a person's own cache mounted into a
/// container run as root is: what it made there would be root's, which the person's later runs
/// could not change. it stops before writing anything, naming the folder, its owner and how to run
/// as them, and a command that only reads, as status, goes on, warning that the cache is another
/// user's.
#[cfg(all(unix, debug_assertions))]
#[test]
fn root_takes_no_cache_another_user_owns() -> Result {
    use std::os::unix::fs::MetadataExt;
    let sandbox = Sandbox::new()?;
    let source = sandbox.folder("src")?;
    fs::write(source.join("a.csv"), b"a\n")?;
    fs::write(source.join("b.csv"), b"b\n")?;
    ok(&sandbox.run(&["add", "../src/a.csv"])?)?;
    let cache = sandbox.cache();
    let owner = fs::metadata(&cache)?.uid();
    if owner == 0 {
        return Ok(());
    }
    let before = entries(&cache)?;
    let as_root = |args: &[&str]| {
        sandbox
            .command(args)
            .env("FETCHLOOM_TEST_EUID", "0")
            .output()
    };
    let output = as_root(&["add", "../src/b.csv"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(50), "{stderr}");
    assert!(
        stderr.contains(&format!(
            "error: could not open the cache at {}: it belongs to {}, who could not change what this run, as root, would make there\n",
            cache.display(),
            crate::support::user_named(owner)
        )) && stderr.contains(
            "help: run fetchloom as the cache's owner, as `docker run --user $(id -u):$(id -g)` runs a container as you, or give root a cache of its own with FETCHLOOM_CACHE\n"
        ),
        "{stderr}"
    );
    assert_eq!(entries(&cache)?, before);
    let status = as_root(&["--offline", "status"])?;
    let said = text(&status.stderr)?;
    assert_eq!(status.status.code(), Some(0), "{said}");
    // the run is root for every check, so the cache is another user's to its exposure check too
    assert!(
        said.contains(&format!(
            "warning: the cache folder {} belongs to another user, so they can change the files projects place from it\n",
            cache.display()
        )),
        "{said}"
    );
    Ok(())
}

/// a link's record in a cache another user owns stands for no bytes a run has not pinned, as they
/// may have written it: every check asks of the run's own user, so a run as another user pins
/// nothing from it offline, where the cache's owner does.
#[cfg(all(unix, debug_assertions))]
#[test]
fn a_links_record_in_another_users_cache_pins_nothing() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/iris.csv", vec![file(IRIS, "\"v1\"")])?;
    ok(&sandbox.run(&["get", &server.url])?)?;
    let another = sandbox
        .command(&["--offline", "add", &server.url])
        .env("FETCHLOOM_TEST_EUID", "3999999999")
        .output()?;
    let stderr = text(&another.stderr)?;
    assert_eq!(another.status.code(), Some(20), "{stderr}");
    assert!(
        stderr.contains(&format!(
            "error: offline, and {} is not in the cache\n",
            server.url
        )),
        "{stderr}"
    );
    let owner = sandbox.run(&["--offline", "add", &server.url])?;
    assert_eq!(owner.status.code(), Some(0), "{}", text(&owner.stderr)?);
    assert_eq!(server.requests().len(), 1, "only get downloads");
    Ok(())
}

/// a git repository's .git folder named as the cache by mistake stays refused, though its objects/
/// holds two-hex folders as a cache's does: HEAD, config and refs/ are no cache's names, and gc,
/// taking it for a cache whose tag is gone, would delete git's objects.
#[test]
fn a_git_folder_named_as_the_cache_is_refused() -> Result {
    let sandbox = Sandbox::new()?;
    let git = sandbox.folder("repo")?.join(".git");
    // a sha-256 repository names a loose object by 62 hex characters below its fan, as a cache does
    let fan = git.join("objects").join("ab");
    fs::create_dir_all(&fan)?;
    let object = fan.join("c".repeat(62));
    fs::write(&object, b"blob 4\0git\n")?;
    fs::write(git.join("HEAD"), b"ref: refs/heads/main\n")?;
    fs::write(
        git.join("config"),
        b"[core]\n\trepositoryformatversion = 1\n",
    )?;
    fs::create_dir_all(git.join("refs").join("heads"))?;
    for args in [&["cache"][..], &["cache", "gc", "--now"]] {
        let output = sandbox
            .command(args)
            .env("FETCHLOOM_CACHE", &git)
            .output()?;
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(50), "{args:?}: {stderr}");
        assert!(
            stderr.contains("it holds other files and is not a fetchloom cache"),
            "{args:?}: {stderr}"
        );
    }
    assert_eq!(fs::read(&object)?, b"blob 4\0git\n");
    assert!(!git.join("CACHEDIR.TAG").exists());
    Ok(())
}

/// a scratch purge takes a cache's oldest files, its tag first, and leaves its folders. a folder
/// that holds nothing but what fetchloom keeps there is plainly its cache, so every command goes
/// on and says so once a run: one that writes to the cache puts the tag back, and one that only
/// reads it, as status, changes nothing. a folder with any other name in it stays refused.
#[test]
fn a_purged_cache_gets_its_tag_back_and_every_command_goes_on() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = Routes::start(vec![("/iris.csv".to_owned(), file(IRIS, "\"1\""))])?;
    sandbox.allow(&routes.url);
    ok(&sandbox.run(&["add", &format!("{}/iris.csv", routes.url), "--as", "iris"])?)?;
    let cache = sandbox.cache();
    let tag = cache.join("CACHEDIR.TAG");
    purge(&cache)?;
    assert!(!tag.exists());
    let said = |args: &[&str]| -> Result<String> {
        let output = sandbox.run(args)?;
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(0), "{args:?}: {stderr}");
        Ok(stderr)
    };
    let lost = format!(
        "warning: the cache at {} has lost its tag, CACHEDIR.TAG, which a cleaner deleting old files takes first; a command that writes to the cache puts it back\n",
        cache.display()
    );
    for args in [&["--offline", "status"][..], &["--offline", "path", "iris"]] {
        let stderr = said(args)?;
        assert_eq!(
            stderr.matches("lost its tag").count(),
            1,
            "{args:?}: {stderr}"
        );
        assert!(stderr.contains(&lost), "{args:?}: {stderr}");
        assert!(!tag.exists(), "{args:?} changes nothing");
    }
    let put_back = format!(
        "warning: the cache at {} had lost its tag, CACHEDIR.TAG, which a cleaner deleting old files takes first, so fetchloom put it back\n",
        cache.display()
    );
    let synced = said(&["sync"])?;
    assert_eq!(synced.matches("lost its tag").count(), 1, "{synced}");
    assert!(synced.contains(&put_back), "{synced}");
    assert!(tagged(&tag)?, "{synced}");
    for args in [
        &["--offline", "status"][..],
        &["--offline", "path", "iris"],
        &["sync"],
    ] {
        let stderr = said(args)?;
        assert!(!stderr.contains("lost its tag"), "{args:?}: {stderr}");
    }
    // path reads the cache, then opens it to place the file it lacks, and tells only once
    purge(&cache)?;
    crate::support::delete(&sandbox.work().join("data").join("iris").join("iris.csv"))?;
    let placed = said(&["path", "iris"])?;
    assert_eq!(placed.matches("lost its tag").count(), 1, "{placed}");
    assert!(placed.contains(&lost), "{placed}");
    assert!(tagged(&tag)?, "{placed}");
    // a person's file among what a purge left is no cache's, so the folder stays refused
    purge(&cache)?;
    fs::write(cache.join("objects").join("notes.txt"), b"mine")?;
    for args in [&["--offline", "status"][..], &["sync"]] {
        let output = sandbox.run(args)?;
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(50), "{args:?}: {stderr}");
        assert!(
            stderr.contains("it holds other files and is not a fetchloom cache"),
            "{args:?}: {stderr}"
        );
        assert!(!tag.exists(), "{args:?}");
    }
    Ok(())
}

/// whether the file `tag` is fetchloom's tag.
fn tagged(tag: &std::path::Path) -> Result<bool> {
    Ok(fs::read_to_string(tag)?.starts_with("Signature: 8a477f597d28d172789f06886806bc55\n"))
}

/// deletes every file below `root` and keeps its folders, as a purge of old files does.
fn purge(root: &std::path::Path) -> Result {
    for entry in fs::read_dir(root)? {
        let path = entry?.path();
        if fs::symlink_metadata(&path)?.is_dir() {
            purge(&path)?;
        } else {
            crate::support::delete(&path)?;
        }
    }
    Ok(())
}

/// every file and folder below `root`, by its path there.
fn entries(root: &std::path::Path) -> Result<Vec<std::path::PathBuf>> {
    let mut found = Vec::new();
    let mut folders = vec![root.to_path_buf()];
    while let Some(folder) = folders.pop() {
        for entry in fs::read_dir(folder)? {
            let path = entry?.path();
            if path.is_dir() {
                folders.push(path.clone());
            }
            found.push(path.strip_prefix(root)?.to_path_buf());
        }
    }
    found.sort();
    Ok(found)
}

/// gc runs from one project, so a merge left unfinished in another names that project's folder in
/// the lock command that finishes it, which a script can then run as given.
#[test]
fn gc_names_the_project_whose_merge_is_unfinished() -> Result {
    let sandbox = Sandbox::new()?;
    let source = sandbox.folder("src")?;
    fs::write(source.join("a.csv"), b"a\n")?;
    let other = sandbox.folder("other project")?;
    for project in [sandbox.work(), other.clone()] {
        let output = sandbox
            .command_in(&project, &["add", &source.display().to_string()])
            .output()?;
        assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    }
    let lock = fs::read_to_string(other.join("data.lock"))?;
    fs::write(
        other.join("data.lock"),
        format!("<<<<<<< ours\n{lock}=======\n{lock}>>>>>>> theirs\n"),
    )?;
    let output = sandbox.run(&["--json", "cache", "gc"])?;
    let document = json(&output)?;
    assert_eq!(output.status.code(), Some(50), "{document}");
    let lock = format!("fetchloom -C \"{}\" lock", other.display());
    let error = &document["errors"][0];
    assert!(
        error["help"]
            .as_str()
            .is_some_and(|help| help.contains(&format!("`{lock}`"))),
        "{document}"
    );
    assert_eq!(error["fix"], serde_json::Value::Null, "{document}");
    // the report goes on, warning that the project's needs are not counted
    let report = sandbox.run(&["--json", "cache"])?;
    let document = json(&report)?;
    assert_eq!(report.status.code(), Some(0), "{document}");
    let warning = document["warnings"]
        .as_array()
        .and_then(|warnings| {
            warnings
                .iter()
                .find(|warning| warning["kind"] == "cache.unreadable")
        })
        .ok_or("no cache.unreadable warning")?;
    assert!(
        warning["message"].as_str().is_some_and(|message| message
            .contains(&other.join("data.lock").display().to_string())
            && message.ends_with("what that project needs is not counted")),
        "{warning}"
    );
    Ok(())
}

/// `fetchloom cache` only reports: it makes no cache where there is none, and leaves a folder of
/// other files as it is, which no cache may be.
#[test]
fn the_cache_report_creates_nothing() -> Result {
    let sandbox = Sandbox::new()?;
    let missing = sandbox.folder("place")?.join("no-cache-yet");
    let report = sandbox
        .command(&["--json", "cache"])
        .env("FETCHLOOM_CACHE", &missing)
        .output()?;
    assert_eq!(report.status.code(), Some(0), "{}", text(&report.stdout)?);
    assert!(!missing.exists());
    let documents = sandbox.folder("documents")?;
    fs::write(documents.join("notes.txt"), b"mine\n")?;
    let report = sandbox
        .command(&["--json", "cache"])
        .env("FETCHLOOM_CACHE", &documents)
        .output()?;
    assert_eq!(report.status.code(), Some(50), "{}", text(&report.stdout)?);
    let mut names: Vec<String> = fs::read_dir(&documents)?
        .map(|entry| Ok(entry?.file_name().to_string_lossy().into_owned()))
        .collect::<Result<_>>()?;
    names.sort();
    assert_eq!(names, ["notes.txt"]);
    // what fixes it is an empty or a new folder, not one fetchloom may write to
    let document = json(&report)?;
    assert_eq!(
        document["errors"][0]["help"],
        format!(
            "set FETCHLOOM_CACHE to an empty folder or a new one, such as {}",
            documents.join("fetchloom-cache").display()
        )
    );
    // a file on the way to the cache is named, not the cache folder that is not there
    let blocker = sandbox.folder("place")?.join("blocker");
    fs::write(&blocker, b"a file\n")?;
    let report = sandbox
        .command(&["--json", "cache"])
        .env("FETCHLOOM_CACHE", blocker.join("cache"))
        .output()?;
    assert_eq!(report.status.code(), Some(50), "{}", text(&report.stdout)?);
    assert_eq!(
        json(&report)?["errors"][0]["message"],
        format!(
            "could not open the cache at {}: {} is a file, not a folder",
            blocker.join("cache").display(),
            blocker.display()
        )
    );
    Ok(())
}

/// gc and verify work on a cache that is there: a folder `FETCHLOOM_CACHE` or the cache setting
/// names that is missing, as a mistyped one is, is refused, with the help naming where it was set.
/// an empty one, as a lab makes before anyone syncs, and the default folder before any command made
/// a cache there have nothing to free or check yet, and get no cache either.
#[test]
fn gc_and_verify_refuse_a_missing_folder_and_answer_an_empty_one() -> Result {
    let sandbox = Sandbox::new()?;
    let missing = sandbox.folder("place")?.join("no-cache-yet");
    let empty = sandbox.folder("empty")?;
    // a first run stopped while it wrote the tag leaves its start, which is no cache yet
    let begun = sandbox.folder("begun")?;
    fs::write(
        begun.join("CACHEDIR.TAG"),
        "Signature: 8a477f597d28d172789f06886806bc55\n",
    )?;
    let settings = sandbox.folder("settings")?.join("settings.toml");
    fs::write(&settings, format!("cache = '{}'\n", missing.display()))?;
    let home = sandbox.folder("home")?;
    for (args, nothing) in [
        (&["--json", "cache", "gc"][..], "to free"),
        (&["--json", "cache", "gc", "--now"], "to free"),
        (&["--json", "cache", "gc", "--dry-run"], "to free"),
        (&["--json", "cache", "gc", "--now", "--dry-run"], "to free"),
        (&["--json", "cache", "verify"], "to check"),
    ] {
        let variable = "FETCHLOOM_CACHE names that folder: check it for a typo";
        let setting = "the cache setting names that folder: check it for a typo, or change it with `fetchloom settings set cache <folder>`";
        for (folder, set, help) in [
            (&missing, "FETCHLOOM_CACHE", variable),
            (&missing, "FETCHLOOM_SETTINGS", setting),
        ] {
            let mut command = sandbox.command(args);
            if set == "FETCHLOOM_CACHE" {
                command.env(set, folder);
            } else {
                command.env_remove("FETCHLOOM_CACHE").env(set, &settings);
            }
            let output = command.output()?;
            assert_eq!(
                output.status.code(),
                Some(50),
                "{args:?} {set}: {}",
                text(&output.stdout)?
            );
            let error = &json(&output)?["errors"][0];
            assert_eq!(error["kind"], "local.io", "{args:?} {set}");
            assert_eq!(
                error["message"],
                format!(
                    "there is no cache at {}, so there is nothing {nothing}",
                    folder.display()
                ),
                "{args:?} {set}"
            );
            assert_eq!(error["help"], help, "{args:?} {set}");
        }
        assert!(!missing.exists(), "{args:?}");
        for folder in [&empty, &begun] {
            let output = sandbox
                .command(args)
                .env("FETCHLOOM_CACHE", folder)
                .output()?;
            assert_eq!(
                output.status.code(),
                Some(0),
                "{args:?} {}: {}",
                folder.display(),
                text(&output.stdout)?
            );
        }
        assert_eq!(fs::read_dir(&empty)?.count(), 0, "{args:?}");
        assert_eq!(fs::read_dir(&begun)?.count(), 1, "{args:?}");
        let default = sandbox
            .command(args)
            .env_remove("FETCHLOOM_CACHE")
            .output()?;
        assert_eq!(
            default.status.code(),
            Some(0),
            "{args:?}: {}",
            text(&default.stdout)?
        );
        assert_eq!(json(&default)?["status"], "ok", "{args:?}");
        assert_eq!(fs::read_dir(&home)?.count(), 0, "{args:?} made a cache");
        let told = sandbox
            .command(&args[1..])
            .env_remove("FETCHLOOM_CACHE")
            .output()?;
        let stderr = text(&told.stderr)?;
        assert!(
            stderr.contains(" yet, so there is nothing ") && stderr.contains(nothing),
            "{args:?}: {stderr}"
        );
    }
    Ok(())
}

/// windows keeps fetchloom's state in the default cache folder, `%LOCALAPPDATA%\fetchloom\state`,
/// so a download with the cache elsewhere writes it there before any cache: a later run without
/// `FETCHLOOM_CACHE` still takes that folder for its cache.
#[cfg(windows)]
#[test]
fn the_state_a_run_leaves_never_bars_the_default_cache() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(
        "/iris.csv",
        vec![file(IRIS, "\"v1\""), file(IRIS, "\"v1\"")],
    )?;
    ok(&sandbox.run(&["get", &server.url, "-o", "first.csv"])?)?;
    let default = sandbox.folder("home")?.join("fetchloom");
    let speeds = sandbox.speeds();
    assert!(
        speeds.starts_with(&default) && speeds.is_file(),
        "{}",
        speeds.display()
    );
    let report = sandbox
        .command(&["--json", "cache"])
        .env_remove("FETCHLOOM_CACHE")
        .output()?;
    assert_eq!(report.status.code(), Some(0), "{}", text(&report.stdout)?);
    let got = sandbox
        .command(&["--json", "get", &server.url, "-o", "second.csv"])
        .env_remove("FETCHLOOM_CACHE")
        .output()?;
    assert_eq!(got.status.code(), Some(0), "{}", text(&got.stdout)?);
    assert!(default.join("CACHEDIR.TAG").is_file());
    assert!(speeds.is_file());
    Ok(())
}

#[test]
fn verify_reports_unenumerable_object_folders() -> Result {
    let sandbox = Sandbox::new()?;
    sandbox.make_cache(&sandbox.cache())?;
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
    sandbox.make_cache(&sandbox.cache())?;
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
        return crate::support::skipped("unprivileged", "root ignores file permissions");
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
        return crate::support::skipped("unprivileged", "root ignores file permissions");
    }
    for broken in [
        "registration",
        "registrations",
        "data.toml",
        "data.lock",
        "project",
    ] {
        let sandbox = Sandbox::new()?;
        sandbox.make_cache(&sandbox.cache())?;
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
        return crate::support::skipped("unprivileged", "root ignores file permissions");
    }
    for json in [false, true] {
        let sandbox = Sandbox::new()?;
        sandbox.make_cache(&sandbox.cache())?;
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
        sandbox.make_cache(&cache)?;
        let object = cache.join("objects").join(&digest[..2]).join(&digest[2..]);
        fs::create_dir_all(object.parent().unwrap())?;
        let file = fs::File::create(&object)?;
        if file.set_len(64 * 1024 * 1024 * 1024).is_ok() {
            break object;
        }
        drop(file);
        fs::remove_file(&object)?;
        if cache.starts_with(build.path()) {
            return crate::support::skipped(
                "sparse-files",
                "neither disk holds a sparse file of 64 GiB",
            );
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
        sandbox.make_cache(&sandbox.cache())?;
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
        return crate::support::skipped("unprivileged", "root ignores file permissions");
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
    assert!(!sandbox.unrooted(message).contains("mark"), "{report}");
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
        return crate::support::skipped("unprivileged", "root ignores file permissions");
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
        return crate::support::skipped("unprivileged", "root ignores file permissions");
    }
    let sandbox = Sandbox::new()?;
    let server = Server::start("/iris.csv", vec![file(IRIS, "\"v1\"")])?;
    sandbox.make_cache(&sandbox.cache())?;
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
    let added = sandbox.run(&["add", &server.url]);
    for (folder, permissions) in folders.iter().zip(before) {
        fs::set_permissions(folder, permissions)?;
    }
    let (people, scripts) = (people?, scripts?);
    let refused = format!(
        "could not write to the cache: permission was denied in {}",
        sandbox.cache().join("partial").display()
    );
    for output in [people, added?] {
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(50), "{stderr}");
        assert!(stderr.contains(&refused), "{stderr}");
        assert!(stderr.contains(SHARED), "{stderr}");
    }
    let report: serde_json::Value = serde_json::from_slice(&scripts.stdout)?;
    assert_eq!(scripts.status.code(), Some(50), "{report}");
    assert_eq!(report["errors"][0]["kind"], "local.io", "{report}");
    assert_eq!(report["errors"][0]["message"], refused.as_str(), "{report}");
    Ok(())
}

/// an empty cache folder that cannot be written fails as it opens, with the same help, as a lab's
/// shared cache may before anything is in it.
#[cfg(unix)]
#[test]
fn an_empty_cache_that_cannot_be_written_gets_the_shared_cache_help() -> Result {
    use std::os::unix::fs::PermissionsExt;
    if rustix::process::geteuid().is_root() {
        return crate::support::skipped("unprivileged", "root ignores file permissions");
    }
    let sandbox = Sandbox::new()?;
    let server = Server::start("/iris.csv", vec![file(IRIS, "\"v1\"")])?;
    let cache = sandbox.cache();
    fs::create_dir_all(&cache)?;
    let before = fs::metadata(&cache)?.permissions();
    fs::set_permissions(&cache, fs::Permissions::from_mode(0o555))?;
    let added = sandbox.run(&["add", &server.url]);
    fs::set_permissions(&cache, before)?;
    let added = added?;
    let stderr = text(&added.stderr)?;
    assert_eq!(added.status.code(), Some(50), "{stderr}");
    assert!(
        stderr.contains(&format!("could not open the cache at {}", cache.display())),
        "{stderr}"
    );
    assert!(stderr.contains(SHARED), "{stderr}");
    Ok(())
}

/// the registration is flushed already, so a projects folder it cannot open warns of nothing.
#[cfg(unix)]
#[test]
fn a_sync_with_nothing_to_do_leaves_its_registration_alone() -> Result {
    use std::os::unix::fs::PermissionsExt;
    if rustix::process::geteuid().is_root() {
        return crate::support::skipped("unprivileged", "root ignores file permissions");
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

/// bytes a download kept without their record, as one stopped before its first record leaves,
/// are continued only by a run that knows the file's hash, which checks them, so the report says
/// so rather than promise to continue them.
#[test]
fn an_unfinished_download_without_its_record_is_told_to_start_again() -> Result {
    let sandbox = Sandbox::new()?;
    sandbox.make_cache(&sandbox.cache())?;
    let partial = sandbox.cache().join("partial");
    let at =
        |key: &str, extension: &str| partial.join(&key[..1]).join(format!("{key}.{extension}"));
    let recorded = "ab".repeat(32);
    let bare = "cd".repeat(32);
    for key in [&recorded, &bare] {
        fs::create_dir_all(partial.join(&key[..1]))?;
        fs::write(at(key, "part"), vec![7_u8; 1000])?;
        fs::write(at(key, "lock"), b"")?;
    }
    fs::write(at(&recorded, "json"), b"{}")?;

    let told = text(&sandbox.run(&["cache"])?.stderr)?;
    assert!(
        told.contains("; a later run continues 1 where the server allows, and starts the other again from its first byte unless it knows its hash\n"),
        "{told}"
    );
    fs::remove_file(at(&recorded, "json"))?;
    let told = text(&sandbox.run(&["cache"])?.stderr)?;
    assert!(
        told.contains(
            ", which a later run starts again from the first byte unless it knows the file's hash\n"
        ),
        "{told}"
    );
    Ok(())
}

/// an interrupted download keeps its bytes so a later run can continue it where the server
/// allows; `--now` frees them however new, but never those a running fetchloom holds.
#[test]
fn unfinished_downloads_are_shown_and_freed_by_gc_now() -> Result {
    let sandbox = Sandbox::new()?;
    sandbox.make_cache(&sandbox.cache())?;
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
    assert!(
        told.contains(", which a later run continues where the server allows\n"),
        "{told}"
    );
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
        sandbox.make_cache(&sandbox.cache())?;
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

/// a project's import, removed from it, so only the cache holds its bytes, at the path returned.
fn imported_then_removed(sandbox: &Sandbox) -> Result<std::path::PathBuf> {
    let precious = sandbox.folder("precious")?;
    fs::write(precious.join("notes.csv"), NOTES)?;
    let added = sandbox.run(&["add", "../precious"])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let removed = sandbox.run(&["remove", "precious", "--yes"])?;
    assert_eq!(removed.status.code(), Some(0), "{}", text(&removed.stderr)?);
    let hex = crate::support::sha256_hex(NOTES);
    Ok(sandbox
        .cache()
        .join("objects")
        .join(&hex[..2])
        .join(&hex[2..]))
}

/// a file imported from a folder comes back from no source once the cache frees it, so gc counts
/// such bytes apart, and `--now` frees them only after a yes.
#[test]
fn gc_now_frees_imported_bytes_only_after_a_yes() -> Result {
    let sandbox = Sandbox::new()?;
    let path = imported_then_removed(&sandbox)?;
    let object = |exists: bool| -> Result {
        assert_eq!(path.exists(), exists);
        Ok(())
    };
    let dry = json(&sandbox.run(&["--json", "cache", "gc", "--now", "--dry-run"])?)?;
    assert_eq!(dry["result"]["removed_bytes"], NOTES.len(), "{dry}");
    assert_eq!(dry["result"]["imported_objects"], 1, "{dry}");
    assert_eq!(dry["result"]["imported_bytes"], NOTES.len(), "{dry}");
    let told = text(&sandbox.run(&["cache", "gc", "--now", "--dry-run"])?.stderr)?;
    assert!(
        told.contains("Would remove 1 file · 32 B from the cache, 32 B of it imported from files no source can give back"),
        "{told}"
    );
    let refused = sandbox.run(&["--json", "cache", "gc", "--now"])?;
    assert_eq!(refused.status.code(), Some(1), "{}", text(&refused.stdout)?);
    let error = &json(&refused)?["errors"][0];
    assert_eq!(error["kind"], "unconfirmed");
    assert_eq!(
        error["message"],
        "1 file · 32 B gc would free was imported from files, and no source can give it back, so freeing it needs a yes"
    );
    // a dry run counts what gc frees; it names no imported file, which the cache knows by hash
    assert_eq!(
        error["help"],
        "answer yes at a terminal, or add --yes once you keep it elsewhere; a dry run counts it, as in `fetchloom cache gc --now --dry-run`"
    );
    assert_eq!(error["fix"], serde_json::Value::Null);
    object(true)?;
    let freed = sandbox.run(&["--json", "cache", "gc", "--now", "--yes"])?;
    let document = json(&freed)?;
    assert_eq!(freed.status.code(), Some(0), "{document}");
    assert_eq!(
        document["result"]["imported_bytes"],
        NOTES.len(),
        "{document}"
    );
    object(false)
}

/// at a terminal, `--now` asks before it frees imported bytes: a no keeps them, a yes frees them.
#[cfg(target_os = "linux")]
#[test]
fn gc_now_at_a_terminal_frees_imported_bytes_on_a_yes() -> Result {
    let sandbox = Sandbox::new()?;
    let object = imported_then_removed(&sandbox)?;
    let gc = sandbox.command(&["cache", "gc", "--now"]);
    let asked = "free it anyway? [y/N]";
    for (keys, code, kept) in [("n\r", 1, true), ("y\r", 0, false)] {
        let Some((exit, screen)) = crate::support::typed(&sandbox.work(), &gc, "", asked, keys)?
        else {
            return Ok(());
        };
        assert_eq!(exit, Some(code), "{keys:?}: {screen}");
        assert_eq!(
            screen.contains("Cancelled freeing 1 file · 32 B from the cache, so nothing changed"),
            kept,
            "{keys:?}: {screen}"
        );
        assert_eq!(object.exists(), kept, "{keys:?}: {screen}");
    }
    Ok(())
}

/// a cache folder other users of the machine can write lets them change what every project of
/// this user places from it, so a run that opens it says so.
#[cfg(unix)]
#[test]
fn a_cache_folder_others_can_write_is_warned_about() -> Result {
    use std::os::unix::fs::PermissionsExt;
    let sandbox = Sandbox::new()?;
    let cache = sandbox.cache();
    fs::create_dir_all(&cache)?;
    fs::set_permissions(&cache, fs::Permissions::from_mode(0o777))?;

    let document = json(&sandbox.run(&["--json", "cache"])?)?;

    assert!(
        crate::support::kinds(&document).contains(&"cache.foreign"),
        "{document}"
    );
    Ok(())
}

/// on windows a folder takes its permissions from the folders above it, so a cache outside the
/// user profile may let the machine's other users change what projects place from it; one inside
/// the profile does not, nor one on a FAT or exFAT drive, which keeps no permissions.
#[cfg(windows)]
#[test]
fn a_cache_folder_outside_the_windows_profile_is_warned_about() -> Result {
    let sandbox = Sandbox::new()?;
    fs::create_dir_all(sandbox.cache())?;
    let root = sandbox.work().parent().ok_or("no sandbox")?.to_path_buf();
    // the sandbox's USERPROFILE is its home folder, beside the cache rather than above it
    let report =
        |profile: Option<&std::path::Path>, disk: Option<&str>| -> Result<serde_json::Value> {
            let mut command = sandbox.command(&["--json", "cache"]);
            command.env_remove("FETCHLOOM_TEST_PROFILE");
            if let Some(profile) = profile {
                command.env("USERPROFILE", profile);
            }
            if let Some(disk) = disk {
                command.env("FETCHLOOM_TEST_FILESYSTEM", disk);
            }
            json(&command.output()?)
        };

    // ci also runs this on fat and exfat drives, which keep no permissions to warn about
    let outside = report(None, Some("ntfs"))?;

    let warning = crate::support::warning(&outside, "cache.foreign");
    let message = warning["message"].as_str().unwrap_or_default();
    assert!(
        message.starts_with("the cache folder ")
            && message.ends_with(" is outside your user profile and takes its permissions from the folders above it, so other users of this machine may be able to change the files projects place from it"),
        "{outside}"
    );
    assert!(
        warning["help"]
            .as_str()
            .is_some_and(|help| help.starts_with("keep the cache inside your user profile")),
        "{outside}"
    );
    for (case, document) in [
        ("inside the profile", report(Some(&root), None)?),
        ("on exfat", report(None, Some("exfat"))?),
        ("on fat32", report(None, Some("fat32"))?),
    ] {
        assert!(
            !crate::support::kinds(&document).contains(&"cache.foreign"),
            "{case}: {document}"
        );
    }
    Ok(())
}

/// gc frees imports spread over hundreds of fan folders, a fan at a time, one of them still needed
/// by another import: a dry run counts what the sweep then frees, and both lists of imports stay,
/// since each names an object the cache still holds.
#[test]
fn gc_frees_imports_over_many_fan_folders() -> Result {
    let bodies: Vec<String> = (0..300).map(|n| format!("row {n}\n")).collect();
    let sandbox = Sandbox::new()?;
    let first = sandbox.folder("first")?;
    for (n, body) in bodies.iter().enumerate() {
        fs::write(first.join(format!("{n}.csv")), body)?;
    }
    let second = sandbox.folder("second")?;
    fs::write(second.join("kept.csv"), &bodies[0])?;
    for folder in ["../first", "../second"] {
        let added = sandbox.run(&["add", folder])?;
        assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    }
    let removed = sandbox.run(&["remove", "first", "--yes"])?;
    assert_eq!(removed.status.code(), Some(0), "{}", text(&removed.stderr)?);
    let object = |body: &String| {
        let hex = crate::support::sha256_hex(body.as_bytes());
        sandbox
            .cache()
            .join("objects")
            .join(&hex[..2])
            .join(&hex[2..])
    };
    let counted =
        |output: &std::process::Output| -> Result<(serde_json::Value, serde_json::Value)> {
            let result = &json(output)?["result"];
            Ok((
                result["imported_objects"].clone(),
                result["imported_bytes"].clone(),
            ))
        };
    let bytes: usize = bodies[1..].iter().map(String::len).sum();
    let dry = sandbox.run(&["--json", "cache", "gc", "--now", "--dry-run"])?;
    assert_eq!(dry.status.code(), Some(0), "{}", text(&dry.stdout)?);
    assert_eq!(
        counted(&dry)?,
        (
            serde_json::json!(bodies.len() - 1),
            serde_json::json!(bytes)
        )
    );
    assert!(bodies.iter().all(|body| object(body).exists()));
    let freed = sandbox.run(&["--json", "cache", "gc", "--now", "--yes"])?;
    assert_eq!(freed.status.code(), Some(0), "{}", text(&freed.stdout)?);
    assert_eq!(counted(&freed)?, counted(&dry)?);
    assert!(object(&bodies[0]).exists());
    assert!(bodies[1..].iter().all(|body| !object(body).exists()));
    let lists = fs::read_dir(sandbox.cache().join("imported")).map_or(0, Iterator::count);
    assert_eq!(
        lists, 2,
        "both lists name the object the second import holds"
    );
    Ok(())
}
