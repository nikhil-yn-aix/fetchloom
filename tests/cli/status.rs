//! `status` rows for files a sync left beside a dataset's: the changed copy it keeps of a file it
//! put back, and none for the files an operating system makes there.

use std::fs;

use crate::mirrors::IRIS;
use crate::support::{Result, Routes, Sandbox, delete, file, json, ok, text};

/// a file named as sync names the changed copy it keeps of a file it put back gets a row saying
/// what it is and what to do with it; any other extra file keeps the plain row.
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
            "      Extra data/iris/iris.csv.changed · {kept}\n      Extra data/iris/iris.csv.changed.2 · {kept}\n      Extra data/iris/notes.changed\n"
        )
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

/// the files finder, a mac on another disk and explorer make in a data folder are no difference,
/// and a folder sync or remove leaves holding only them goes, taking them with it.
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
    assert_eq!(text(&output.stdout)?, "      Extra data/b/notes.txt\n");
    let document = json(&sandbox.run(&["--json", "status"])?)?;
    assert_eq!(
        document["result"]["extra"],
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
/// still restore, is theirs: status lists it, and its folder stays when its dataset goes.
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
        document["result"]["extra"],
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
