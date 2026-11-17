//! what fetchloom owns in a project: its datasets' folders, and nothing beside them. a file
//! outside every dataset's folder is the person's alone, never counted, listed or touched, and one
//! a person puts inside a dataset's folder is theirs too, named once and kept.

use std::fs;

use crate::project::{IRIS, PATH};
use crate::support::{Result, Routes, Sandbox, file, json, text, without_copy_warning};

/// a server answering iris at [`PATH`], as often as asked.
fn iris_server() -> Result<Routes> {
    Routes::start(vec![(PATH.to_owned(), file(IRIS, "\"v1\""))])
}

/// iris added to the project in `sandbox` from `server`, its file at data/iris/iris.csv.
fn with_iris(sandbox: &Sandbox, server: &Routes) -> Result {
    let link = format!("{}{PATH}", server.url);
    sandbox.allow(&link);
    let added = sandbox.run(&["add", &link])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    Ok(())
}

/// writes each `(path, text)` below the project in `sandbox`, making its folders.
fn write_all(sandbox: &Sandbox, files: &[(&str, &str)]) -> Result {
    for (path, text) in files {
        let file = sandbox.work().join(path);
        fs::create_dir_all(file.parent().ok_or("a path outside any folder")?)?;
        fs::write(&file, text)?;
    }
    Ok(())
}

/// status judges only what lies in a dataset's folder: files a pipeline writes beside the
/// datasets, as cookiecutter's data/processed and data/interim, are never counted or listed, and
/// files a person puts in a dataset's folder are named as theirs, kept, never counted. sync, with
/// --force too, leaves every one as it was, and lock --check has nothing to say of them.
#[test]
fn only_files_in_dataset_folders_are_judged() -> Result {
    let sandbox = Sandbox::new()?;
    let server = iris_server()?;
    with_iris(&sandbox, &server)?;
    let theirs = [
        ("data/processed/clean.csv", "a\n"),
        ("data/interim/step1.txt", "b\n"),
        ("data/iris/iris_clean.csv", "c\n"),
        ("data/iris/notes/readme.md", "d\n"),
    ];
    write_all(&sandbox, &theirs)?;

    let status = sandbox.run(&["status"])?;
    assert_eq!(status.status.code(), Some(0));
    assert_eq!(text(&status.stdout)?, "", "no row, since nothing differs");
    let told = without_copy_warning(&text(&status.stderr)?);
    assert_eq!(
        told,
        "       Kept 2 files of yours in dataset folders · `fetchloom status -v` lists them\n         Ok everything matches data.lock by size and time\n"
    );
    let bare = sandbox.run(&[])?;
    assert_eq!(
        without_copy_warning(&text(&bare.stderr)?),
        "         Ok everything matches data.lock by size and time\n",
        "the one-line answer counts none of them"
    );
    let detailed = without_copy_warning(&text(&sandbox.run(&["status", "-v"])?.stderr)?);
    assert!(
        detailed.contains(
            "       Kept 2 files of yours in dataset folders\n            data/iris/iris_clean.csv\n            data/iris/notes/readme.md\n"
        ),
        "{detailed}"
    );
    assert!(!detailed.contains("processed"), "{detailed}");
    let document = json(&sandbox.run(&["--json", "status"])?)?;
    let result = &document["result"];
    assert_eq!(result["extra"], serde_json::json!([]), "{document}");
    assert_eq!(
        result["yours"],
        serde_json::json!(["data/iris/iris_clean.csv", "data/iris/notes/readme.md"]),
        "{document}"
    );
    let whole = document.to_string();
    assert!(
        !whole.contains("processed") && !whole.contains("interim"),
        "{whole}"
    );

    write_all(&sandbox, &[("data/iris/one.csv", "e\n")])?;
    fs::remove_file(sandbox.work().join("data/iris/notes/readme.md"))?;
    fs::remove_file(sandbox.work().join("data/iris/iris_clean.csv"))?;
    let one = without_copy_warning(&text(&sandbox.run(&["status"])?.stderr)?);
    assert!(
        one.starts_with("       Kept data/iris/one.csv · yours\n"),
        "{one}"
    );

    for args in [&["sync"][..], &["sync", "--force"], &["lock", "--check"]] {
        let output = sandbox.run(args)?;
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(0), "{args:?}: {stderr}");
        assert!(!stderr.contains("processed"), "{args:?}: {stderr}");
        assert!(!stderr.contains("one.csv"), "{args:?}: {stderr}");
    }
    for (path, text) in [
        ("data/processed/clean.csv", "a\n"),
        ("data/interim/step1.txt", "b\n"),
        ("data/iris/one.csv", "e\n"),
    ] {
        assert_eq!(
            fs::read_to_string(sandbox.work().join(path))?,
            text,
            "{path}"
        );
    }
    Ok(())
}
