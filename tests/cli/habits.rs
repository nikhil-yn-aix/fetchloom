//! what people do with raw and processed data, and fetchloom staying out of the way: an edit of a
//! pinned file is kept and explained once, with the ways that keep it first, a dataset's folder
//! deleted to free room is put back with the command that keeps it off the machine, and every
//! help names what fits what the person did.

use std::fs;

use crate::project::{IRIS, PATH};
use crate::support::{Result, Routes, Sandbox, delete, delete_folder, file, json, text, without_copy_warning, zip_of};

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

/// a dataset's folder deleted whole, to free room, comes back with the next sync, which says once
/// how to keep it off this machine.
#[test]
fn a_deleted_dataset_folder_names_drop_once() -> Result {
    let sandbox = Sandbox::new()?;
    let server = iris_server()?;
    with_iris(&sandbox, &server)?;
    let folder = sandbox.work().join("data/iris");
    let hint = "  hint: to keep iris off this machine, run `fetchloom drop iris`\n";
    delete_folder(&folder)?;
    let first = synced(&sandbox)?;
    assert!(first.contains(hint), "{first}");
    assert_eq!(fs::read(folder.join("iris.csv"))?, IRIS);
    delete_folder(&folder)?;
    let again = synced(&sandbox)?;
    assert!(!again.contains("hint:"), "{again}");
    Ok(())
}

/// the data folder moved away whole, as to give it another name, comes back with the next sync,
/// which says once where its place is set, and names no dataset to drop.
#[test]
fn a_moved_data_folder_names_data_dir_once() -> Result {
    let sandbox = Sandbox::new()?;
    let server = iris_server()?;
    with_iris(&sandbox, &server)?;
    let data = sandbox.work().join("data");
    let hint =
        "  hint: to keep the data folder somewhere else than data/, set data_dir in data.toml\n";
    fs::rename(&data, sandbox.work().join("inputs"))?;
    let first = synced(&sandbox)?;
    assert!(first.contains(hint), "{first}");
    assert!(!first.contains("drop"), "{first}");
    assert_eq!(fs::read(data.join("iris/iris.csv"))?, IRIS);
    delete_folder(&data)?;
    let again = synced(&sandbox)?;
    assert!(!again.contains("hint:"), "{again}");
    Ok(())
}

/// a single compressed file unpacked by hand, as `gunzip -f` leaves it: the put-back help names
/// decompress = true first, which keeps the unpacked file, and the next sync takes that file as
/// placed and lets the packed one go.
#[test]
fn a_compressed_file_unpacked_by_hand_points_to_decompress() -> Result {
    let sandbox = Sandbox::new()?;
    let csv = b"gene,value\nbrca1,2.5\n";
    let packed = crate::decompress::gz(csv)?;
    let server = Routes::start(vec![(
        "/files/visits.csv.gz".to_owned(),
        file(&packed, "\"g1\""),
    )])?;
    let link = format!("{}/files/visits.csv.gz", server.url);
    sandbox.allow(&link);
    let added = sandbox.run(&["add", &link])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let folder = sandbox.work().join("data/visits");
    delete(&folder.join("visits.csv.gz"))?;
    fs::write(folder.join("visits.csv"), csv)?;
    let output = sandbox.run(&["--json", "sync"])?;
    let document = json(&output)?;
    assert_eq!(
        crate::support::warned(&document),
        [
            "put back data/visits/visits.csv.gz, which was deleted or moved, since data.lock pins it; to keep data/visits/visits.csv, which you unpacked, in its place, set decompress = true for visits in data.toml"
        ],
        "{document}"
    );
    let manifest = sandbox.work().join("data.toml");
    let written = fs::read_to_string(&manifest)?;
    fs::write(
        &manifest,
        written.replace(
            "[datasets.visits]\n",
            "[datasets.visits]\ndecompress = true\n",
        ),
    )?;
    synced(&sandbox)?;
    assert_eq!(fs::read(folder.join("visits.csv"))?, csv);
    assert!(!folder.join("visits.csv.gz").exists());
    Ok(())
}

/// files a person keeps in a dataset's folder stay when the dataset goes, and the warning says
/// they are theirs now, as status no longer names them.
#[test]
fn removing_a_dataset_says_the_files_left_are_yours() -> Result {
    let sandbox = Sandbox::new()?;
    let server = iris_server()?;
    with_iris(&sandbox, &server)?;
    fs::write(sandbox.work().join("data/iris/iris.parquet"), "par1")?;
    let removed = sandbox.run(&["--json", "remove", "iris"])?;
    let document = json(&removed)?;
    assert_eq!(
        crate::support::warned(&document),
        [
            "data/iris still holds data/iris/iris.parquet, a file fetchloom did not place, so the folder stays; it is yours, and fetchloom no longer looks at it"
        ],
        "{document}"
    );
    let status = json(&sandbox.run(&["--json", "status"])?)?;
    assert_eq!(status["result"]["yours"], serde_json::json!([]), "{status}");
    assert_eq!(
        fs::read(sandbox.work().join("data/iris/iris.parquet"))?,
        b"par1"
    );
    Ok(())
}

/// files renamed inside their dataset's folder are put back under the names data.lock pins, and
/// the help says nothing of moving the data folder, which was never moved.
#[test]
fn renamed_files_are_put_back_without_naming_data_dir() -> Result {
    let sandbox = Sandbox::new()?;
    let zip = zip_of(&[("wave1.csv", &b"a\n1\n"[..]), ("wave2.csv", b"a\n2\n")])?;
    let server = Routes::start(vec![("/files/survey.zip".to_owned(), file(&zip, "\"s1\""))])?;
    let link = format!("{}/files/survey.zip", server.url);
    sandbox.allow(&link);
    let added = sandbox.run(&["add", &link])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let folder = sandbox.work().join("data/survey");
    for (from, to) in [
        ("wave1.csv", "2026_survey_w1.csv"),
        ("wave2.csv", "2026_survey_w2.csv"),
    ] {
        fs::rename(folder.join(from), folder.join(to))?;
    }
    let renamed = synced(&sandbox)?;
    assert!(
        renamed.contains("warning: put back 2 files that were deleted or moved"),
        "{renamed}"
    );
    assert!(!renamed.contains("data_dir"), "{renamed}");
    Ok(())
}
