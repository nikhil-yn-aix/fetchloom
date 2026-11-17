//! what people do with raw and processed data, and fetchloom staying out of the way: an edit of a
//! pinned file is kept and explained once, with the ways that keep it first, a dataset's folder
//! deleted to free room is put back with the command that keeps it off the machine, and every
//! help names what fits what the person did.

use std::fs;

use crate::project::{IRIS, PATH};
use crate::support::{Result, Routes, Sandbox, delete, file, text, without_copy_warning};

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

/// what `sync` printed on stderr, which must succeed, without the copy note fat disks add.
fn synced(sandbox: &Sandbox) -> Result<String> {
    let output = sandbox.run(&["sync"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    Ok(without_copy_warning(&stderr))
}

/// a fix made in place, as `sed -i` makes it, writing a new file under the old name: the sync
/// after it says once that the edit stays, naming first the ways that keep it, and last the
/// command that discards it. later syncs only count it, until the file changes again.
#[test]
fn a_kept_edit_is_told_once_with_the_ways_that_keep_it() -> Result {
    let sandbox = Sandbox::new()?;
    let server = iris_server()?;
    with_iris(&sandbox, &server)?;
    let placed = sandbox.work().join("data/iris/iris.csv");
    delete(&placed)?;
    fs::write(&placed, "sl,sw,pl,pw,species\n")?;
    let told = "warning: data/iris/iris.csv has changed, so it stays\n  help: keep your version as a file or dataset of your own, or set copy = true for iris in data.toml to edit its files in place; `fetchloom sync --force` discards your edit\n";
    let dry = |sandbox: &Sandbox| -> Result<String> {
        text(&sandbox.run(&["sync", "--dry-run"])?.stderr)
    };
    let before = dry(&sandbox)?;
    assert!(before.contains(told), "{before}");
    let first = synced(&sandbox)?;
    assert!(first.contains(told), "{first}");
    let second = synced(&sandbox)?;
    assert_eq!(
        second,
        "     Synced 1 dataset into data, 1 file left unchanged\n"
    );
    let after = dry(&sandbox)?;
    assert!(!after.contains("has changed"), "{after}");
    delete(&placed)?;
    fs::write(&placed, "sl,sw,pl,pw,kind\n")?;
    let changed = synced(&sandbox)?;
    assert!(changed.contains(told), "{changed}");
    assert_eq!(fs::read(&placed)?, b"sl,sw,pl,pw,kind\n");
    Ok(())
}
