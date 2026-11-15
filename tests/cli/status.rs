//! `status` rows for files a sync left beside a dataset's: the changed copy it keeps of a file it
//! put back.

use std::fs;

use crate::mirrors::IRIS;
use crate::support::{Result, Routes, Sandbox, delete, file, ok, text};

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
