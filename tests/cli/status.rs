//! `status` rows for files a sync left beside a dataset's: the changed copy it keeps of a file it
//! put back, and none for the files an operating system makes there.

use std::fs;

use crate::mirrors::IRIS;
use crate::support::{Result, Routes, Sandbox, delete, file, json, ok, text};

/// a file named as sync names the changed copy it keeps of a file it put back gets a row saying
/// what it is and what to do with it; any other file of a person's in the folder is theirs, kept,
/// with no row, and only `-v` names it.
#[test]
fn a_changed_copy_sync_kept_says_what_to_do_with_it() -> Result {
    let sandbox = Sandbox::new()?;
    let source = Routes::start(vec![("/files/iris.csv".to_owned(), file(IRIS, "\"i\""))])?;
    sandbox.allow(&source.url);
    ok(&sandbox.run(&["add", &format!("{}/files/iris.csv", source.url)])?)?;
    let folder = sandbox.work().join("data/iris");
    for name in ["iris.csv.changed", "iris.csv.changed.2", "notes.changed"] {
        fs::write(folder.join(name), "an edit\n")?;
    }
    let output = sandbox.run(&["status"])?;
    ok(&output)?;
    let kept = "a changed copy sync kept of data/iris/iris.csv: compare the two, keep what you need, then delete it";
    assert_eq!(
        text(&output.stdout)?,
        format!(
            "      Extra data/iris/iris.csv.changed · {kept}\n      Extra data/iris/iris.csv.changed.2 · {kept}\n"
        )
    );
    let stderr = text(&output.stderr)?;
    assert!(!stderr.contains("notes.changed"), "{stderr}");
    let detailed = text(&sandbox.run(&["status", "-v"])?.stderr)?;
    assert!(
        detailed.contains("       Kept data/iris/notes.changed · yours\n"),
        "{detailed}"
    );
    // with the pinned file edited too, status surveys the whole folder and says the same
    delete(&folder.join("iris.csv"))?;
    fs::write(folder.join("iris.csv"), "mine\n")?;
    let output = sandbox.run(&["status"])?;
    ok(&output)?;
    let rows = text(&output.stdout)?;
    assert!(
        rows.starts_with(&format!(
            "     Edited data/iris/iris.csv\n      Extra data/iris/iris.csv.changed · {kept}\n"
        )),
        "{rows}"
    );
    Ok(())
}

/// status writes a list's rows together, each as it wrote them one at a time: the rows, their
/// verbs, what it tells on stderr and its exit code, plain and with -v, and --json, which writes
/// no row, names the same files.
#[test]
fn status_writes_a_lists_rows_together() -> Result {
    let sandbox = Sandbox::new()?;
    let names = ["a", "b", "c", "d"];
    let source = Routes::start(
        names
            .iter()
            .map(|name| (format!("/files/{name}.csv"), file(name.as_bytes(), "\"v\"")))
            .collect(),
    )?;
    sandbox.allow(&source.url);
    for name in names {
        ok(&sandbox.run(&["add", &format!("{}/files/{name}.csv", source.url)])?)?;
    }
    let data = sandbox.work().join("data");
    delete(&data.join("a/a.csv"))?;
    delete(&data.join("b/b.csv"))?;
    delete(&data.join("c/c.csv"))?;
    fs::write(data.join("c/c.csv"), "mine\n")?;
    fs::write(data.join("d/notes.txt"), "mine\n")?;
    fs::write(data.join("d/more.txt"), "mine\n")?;
    // a person's own files in a dataset folder get no row in a plain status
    let rows = "    Missing data/a/a.csv\n    Missing data/b/b.csv\n     Edited data/c/c.csv\n";
    let plain = sandbox.run(&["status"])?;
    assert_eq!(plain.status.code(), Some(0));
    assert_eq!(text(&plain.stdout)?, rows);
    assert_eq!(
        text(&plain.stderr)?,
        "      Found 3 differences from data.lock\n  hint: `fetchloom sync` places missing or updated files and reports any conflicts it keeps\n  hint: save any changes you need, then use `fetchloom sync --force` to replace edited or conflicting files at paths in data.lock\n"
    );
    // -v's stderr names the resources it took, which follow the memory free at the moment
    let verbose = sandbox.run(&["status", "-v"])?;
    assert_eq!(verbose.status.code(), Some(0));
    assert_eq!(text(&verbose.stdout)?, rows);
    let document = json(&sandbox.run(&["--json", "status"])?)?;
    let result = &document["result"];
    assert_eq!(
        (&result["missing"], &result["edited"], &result["yours"]),
        (
            &serde_json::json!(["data/a/a.csv", "data/b/b.csv"]),
            &serde_json::json!(["data/c/c.csv"]),
            &serde_json::json!(["data/d/more.txt", "data/d/notes.txt"])
        ),
        "{document}"
    );
    Ok(())
}

/// the files finder, a mac on another disk and explorer make in a data folder are no difference,
/// a person's own file there is theirs, kept, which json names, and a folder sync or remove leaves
/// holding only the system's files goes, taking them with it.
#[test]
fn files_the_system_makes_are_no_difference() -> Result {
    let sandbox = Sandbox::new()?;
    let source = Routes::start(vec![
        ("/files/iris.csv".to_owned(), file(IRIS, "\"i\"")),
        ("/files/b.csv".to_owned(), file(b"b\n", "\"b\"")),
    ])?;
    sandbox.allow(&source.url);
    ok(&sandbox.run(&["add", &format!("{}/files/iris.csv", source.url)])?)?;
    ok(&sandbox.run(&["add", &format!("{}/files/b.csv", source.url)])?)?;
    let work = sandbox.work();
    let (iris, b) = (work.join("data/iris"), work.join("data/b"));
    fs::create_dir(iris.join("__MACOSX"))?;
    for path in [
        ".DS_Store",
        "._iris.csv",
        "Thumbs.db",
        "__MACOSX/._iris.csv",
    ] {
        fs::write(iris.join(path), "made by the system\n")?;
    }
    fs::write(b.join(".DS_Store"), "made by the system\n")?;
    fs::write(b.join("notes.txt"), "mine\n")?;
    let output = sandbox.run(&["status"])?;
    ok(&output)?;
    assert_eq!(text(&output.stdout)?, "");
    let stderr = text(&output.stderr)?;
    assert!(!stderr.contains("notes.txt"), "{stderr}");
    let document = json(&sandbox.run(&["--json", "status"])?)?;
    assert_eq!(document["result"]["extra"], serde_json::json!([]));
    assert_eq!(
        document["result"]["yours"],
        serde_json::json!(["data/b/notes.txt"])
    );
    let output = sandbox.run(&["remove", "iris"])?;
    ok(&output)?;
    assert!(!iris.exists(), "{}", text(&output.stderr)?);
    let output = sandbox.run(&["remove", "b"])?;
    ok(&output)?;
    let said = text(&output.stderr)?;
    assert!(
        said.contains("warning: data/b still holds data/b/notes.txt, a file fetchloom did not place, so the folder stays\n"),
        "{said}"
    );
    assert!(b.join(".DS_Store").exists());
    Ok(())
}

/// what a person files under a folder a system names, as a disk's trash holding what they may
/// still restore, is theirs: status names it as theirs, kept, and its folder stays when its
/// dataset goes.
#[test]
fn files_in_a_folder_a_system_names_are_a_persons() -> Result {
    let sandbox = Sandbox::new()?;
    let source = Routes::start(vec![
        ("/files/iris.csv".to_owned(), file(IRIS, "\"i\"")),
        ("/files/b.csv".to_owned(), file(b"b\n", "\"b\"")),
    ])?;
    sandbox.allow(&source.url);
    ok(&sandbox.run(&["add", &format!("{}/files/iris.csv", source.url)])?)?;
    ok(&sandbox.run(&["add", &format!("{}/files/b.csv", source.url)])?)?;
    let work = sandbox.work();
    let b = work.join("data/b");
    let theirs = [
        "data/b/.Trash-1000/files/thesis.docx",
        "data/b/.Trash-1000/info/thesis.docx.trashinfo",
        "data/b/__MACOSX/notes.txt",
    ];
    for path in theirs {
        let path = work.join(path);
        fs::create_dir_all(path.parent().ok_or("a file has a folder")?)?;
        fs::write(path, "mine\n")?;
    }
    fs::write(b.join(".DS_Store"), "made by the system\n")?;
    let document = json(&sandbox.run(&["--json", "status"])?)?;
    assert_eq!(
        document["result"]["yours"],
        serde_json::json!(theirs),
        "{document}"
    );
    let toml = fs::read_to_string(work.join("data.toml"))?;
    let kept = toml
        .split("\n\n")
        .filter(|table| !table.contains("[datasets.b]"))
        .collect::<Vec<_>>()
        .join("\n\n");
    fs::write(work.join("data.toml"), kept)?;
    let output = sandbox.run(&["sync"])?;
    ok(&output)?;
    let said = text(&output.stderr)?;
    for path in theirs {
        assert_eq!(fs::read(work.join(path))?, b"mine\n", "{said}");
    }
    Ok(())
}

/// iris added from a source of its own and synced as a writable copy, `copy = true`, with the
/// state that records it gone, as in a copy of the project: the file fetchloom did not place there.
fn copied_without_a_state(sandbox: &Sandbox) -> Result<(Routes, std::path::PathBuf)> {
    let source = Routes::start(vec![("/files/iris.csv".to_owned(), file(IRIS, "\"i\""))])?;
    sandbox.allow(&source.url);
    ok(&sandbox.run(&["add", &format!("{}/files/iris.csv", source.url)])?)?;
    let work = sandbox.work();
    let manifest = work.join("data.toml");
    let written = fs::read_to_string(&manifest)?;
    fs::write(
        &manifest,
        written.replace("[datasets.iris]\n", "[datasets.iris]\ncopy = true\n"),
    )?;
    ok(&sandbox.run(&["sync"])?)?;
    fs::remove_file(work.join(".fetchloom").join("state"))?;
    Ok((source, work.join("data/iris/iris.csv")))
}

/// a read-only file holding the pinned bytes of a dataset that asks for writable copies is no file
/// a sync takes as placed: status calls it outdated, and the sync replaces it with a writable copy.
#[test]
fn a_read_only_copy_nothing_recorded_is_outdated() -> Result {
    let sandbox = Sandbox::new()?;
    let (_source, placed) = copied_without_a_state(&sandbox)?;
    let mut permissions = fs::metadata(&placed)?.permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&placed, permissions)?;
    let document = json(&sandbox.run(&["--json", "status"])?)?;
    let result = &document["result"];
    assert_eq!(
        (&result["outdated"], &result["adoptable"]),
        (
            &serde_json::json!(["data/iris/iris.csv"]),
            &serde_json::json!([])
        ),
        "{document}"
    );
    let output = sandbox.run(&["sync"])?;
    ok(&output)?;
    assert!(
        !fs::metadata(&placed)?.permissions().readonly(),
        "{}",
        text(&output.stderr)?
    );
    ok(&sandbox.run(&["status", "--check"])?)?;
    Ok(())
}

/// a file at a pinned path that this account cannot read may hold any bytes, the pinned ones
/// too: status says it cannot be read, never that it holds other bytes, and offers no
/// `sync --force`, which cannot read it either. once it can be read, a sync takes it in.
#[cfg(unix)]
#[test]
fn a_file_no_one_can_read_is_unreadable_not_untracked() -> Result {
    use std::os::unix::fs::PermissionsExt;
    let sandbox = Sandbox::new()?;
    if !crate::placing::files_refuse_reads(&sandbox)? {
        return crate::support::skipped("permissions", "this disk keeps no file from being read");
    }
    let (_source, placed) = copied_without_a_state(&sandbox)?;
    fs::set_permissions(&placed, fs::Permissions::from_mode(0o000))?;
    let document = json(&sandbox.run(&["--json", "status"])?)?;
    let result = &document["result"];
    assert_eq!(
        (
            &result["unreadable"],
            &result["untracked"],
            &result["adoptable"]
        ),
        (
            &serde_json::json!(["data/iris/iris.csv"]),
            &serde_json::json!([]),
            &serde_json::json!([])
        ),
        "{document}"
    );
    let output = sandbox.run(&["status"])?;
    ok(&output)?;
    assert_eq!(
        text(&output.stdout)?,
        " Unreadable data/iris/iris.csv · this account cannot read it, so what it holds is unknown\n"
    );
    let stderr = text(&output.stderr)?;
    assert!(!stderr.contains("--force"), "{stderr}");
    let steps = "check its permissions, then run `fetchloom sync`";
    assert!(stderr.contains(steps), "{stderr}");
    let check = sandbox.run(&["--json", "status", "--check"])?;
    assert_eq!(check.status.code(), Some(1), "{}", text(&check.stderr)?);
    assert_eq!(
        json(&check)?["errors"][0]["details"],
        serde_json::json!([format!(
            "data/iris/iris.csv cannot be read, so what it holds is unknown: {steps}"
        )])
    );
    let synced = sandbox.run(&["sync"])?;
    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(50), "{stderr}");
    assert!(stderr.contains("could not place "), "{stderr}");
    // what the steps say
    fs::set_permissions(&placed, fs::Permissions::from_mode(0o644))?;
    ok(&sandbox.run(&["sync"])?)?;
    ok(&sandbox.run(&["status", "--check"])?)?;
    Ok(())
}
