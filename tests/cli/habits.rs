//! what people do with raw and processed data, and fetchloom staying out of the way: an edit of a
//! pinned file is kept and explained once, with the ways that keep it first, a dataset's folder
//! deleted to free room is put back with the command that keeps it off the machine, and every
//! help names what fits what the person did.

use std::fs;

use crate::boundary::{iris_server, synced, with_iris};
use crate::project::{IRIS, PATH};
use crate::support::{
    Result, Routes, Sandbox, add_ok, delete, delete_folder, file, json, text, zip_of,
};

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

/// a dataset's folder deleted whole, to free room, or renamed, comes back with the next sync,
/// which says once how to keep it off this machine, or to name the new folder in data.toml: it
/// cannot tell which, as a tool that deletes it before its own run does it too.
#[test]
fn a_deleted_dataset_folder_names_drop_once() -> Result {
    let sandbox = Sandbox::new()?;
    let server = iris_server()?;
    with_iris(&sandbox, &server)?;
    let folder = sandbox.work().join("data/iris");
    let hint = "  hint: if you deleted data/iris to free space, `fetchloom drop iris` keeps it off this machine, or if you moved it, set path = \"<new folder>\" for iris in data.toml\n";
    delete_folder(&folder)?;
    let first = synced(&sandbox)?;
    assert!(first.contains(hint), "{first}");
    assert_eq!(fs::read(folder.join("iris.csv"))?, IRIS);
    for _ in 0..2 {
        delete_folder(&folder)?;
        let again = synced(&sandbox)?;
        assert!(!again.contains("hint:"), "{again}");
    }
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

/// a single compressed file unpacked by hand, as `gunzip -f` leaves it: the put-back hint names
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
    add_ok(&sandbox, &format!("{}/files/visits.csv.gz", server.url))?;
    let folder = sandbox.work().join("data/visits");
    delete(&folder.join("visits.csv.gz"))?;
    fs::write(folder.join("visits.csv"), csv)?;
    let output = sandbox.run(&["sync"])?;
    let stderr = text(&output.stderr)?;
    assert!(
        stderr.contains("   Put back data/visits/visits.csv.gz, which was deleted or moved, since data.lock pins it\n  hint: to keep data/visits/visits.csv, which you unpacked, in its place, set decompress = true for visits in data.toml\n"),
        "{stderr}"
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
/// they are theirs now, as git sees them once the folder's .gitignore goes, and status no longer
/// lists them.
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
    assert!(!sandbox.work().join("data/iris/.gitignore").exists());
    let status = json(&sandbox.run(&["--json", "status"])?)?;
    let extra = status["result"]["extra"].as_array().ok_or("a list")?;
    assert!(
        !extra.iter().any(|path| path == "data/iris/iris.parquet"),
        "{status}"
    );
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
    add_ok(&sandbox, &format!("{}/files/survey.zip", server.url))?;
    let folder = sandbox.work().join("data/survey");
    for (from, to) in [
        ("wave1.csv", "2026_survey_w1.csv"),
        ("wave2.csv", "2026_survey_w2.csv"),
    ] {
        fs::rename(folder.join(from), folder.join(to))?;
    }
    let renamed = synced(&sandbox)?;
    assert!(
        renamed.contains("   Put back 2 files that were deleted or moved"),
        "{renamed}"
    );
    assert!(!renamed.contains("data_dir"), "{renamed}");
    Ok(())
}

/// an R package keeps R's own datasets in data/, where R CMD check refuses other files, so the add
/// that starts a project at its root says once where raw files go instead.
#[test]
fn an_r_package_hears_once_where_raw_files_go() -> Result {
    let sandbox = Sandbox::new()?;
    fs::write(
        sandbox.work().join("DESCRIPTION"),
        "Package: irisdata\nVersion: 0.1.0\nTitle: Iris Data\n",
    )?;
    let server = Routes::start(vec![
        (PATH.to_owned(), file(IRIS, "\"v1\"")),
        ("/files/wine.csv".to_owned(), file(b"a,b\n1,2\n", "\"w1\"")),
    ])?;
    sandbox.allow(&server.url);
    let added = sandbox.run(&["--json", "add", &format!("{}{PATH}", server.url)])?;
    let document = json(&added)?;
    assert_eq!(
        crate::support::warned(&document),
        [
            "this folder is an R package, whose data/ holds the datasets R loads, and R CMD check refuses other files there; set data_dir = \"data-raw\" under [project] in data.toml, then run `fetchloom sync` to place the files there"
        ],
        "{document}"
    );
    assert_eq!(crate::support::kinds(&document), ["project.r_package"]);
    let again = sandbox.run(&["--json", "add", &format!("{}/files/wine.csv", server.url)])?;
    let document = json(&again)?;
    assert_eq!(
        crate::support::kinds(&document),
        Vec::<&str>::new(),
        "{document}"
    );
    Ok(())
}

/// a project in a folder a sync client keeps, as onedrive, gets copies, since such a client breaks
/// links. the first run that copies says why, with no advice to move the project, which is the
/// person's choice, and later runs on that disk say nothing of it. the data folder on another
/// disk, as after the project moved there, hears it again.
#[test]
fn the_copy_reason_is_told_once_per_disk() -> Result {
    let sandbox = Sandbox::new()?;
    if sandbox.links()?.reflink {
        return crate::support::skipped("copies", "this disk clones files, so none is copied");
    }
    let server = Routes::start(vec![
        (PATH.to_owned(), file(IRIS, "\"v1\"")),
        ("/files/wine.csv".to_owned(), file(b"a,b\n1,2\n", "\"w1\"")),
        ("/files/beer.csv".to_owned(), file(b"c,d\n3,4\n", "\"b1\"")),
    ])?;
    sandbox.allow(&server.url);
    let inside = |disk: &str, name: &str| -> Result<String> {
        let output = sandbox
            .command(&["add", &format!("{}/files/{name}.csv", server.url)])
            .env("OneDrive", sandbox.work())
            .env("FETCHLOOM_TEST_DISK", disk)
            .output()?;
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(0), "{stderr}");
        Ok(stderr)
    };
    let told = "warning: data is inside onedrive, which breaks links, so files were copied\n";
    let first = inside("1", "iris")?;
    assert!(first.contains(told), "{first}");
    assert!(!first.contains("outside onedrive"), "{first}");
    let second = inside("1", "wine")?;
    assert_eq!(
        fs::read(sandbox.work().join("data/wine/wine.csv"))?,
        b"a,b\n1,2\n"
    );
    assert!(!second.contains("inside onedrive"), "{second}");
    let moved = inside("2", "beer")?;
    assert!(moved.contains(told), "{moved}");
    Ok(())
}

/// a cleanup that takes .fetchloom/.gitignore while the folder stays, as a purge of old files
/// does: the next sync makes it again, so git never takes the project's own records, and a file of
/// the person's there stays as it is.
#[test]
fn a_lost_gitignore_of_the_project_folder_comes_back() -> Result {
    let sandbox = Sandbox::new()?;
    let server = iris_server()?;
    with_iris(&sandbox, &server)?;
    let ignore = sandbox.work().join(".fetchloom/.gitignore");
    assert_eq!(fs::read(&ignore)?, b"*\n");
    fs::remove_file(&ignore)?;
    synced(&sandbox)?;
    assert_eq!(fs::read(&ignore)?, b"*\n");
    fs::write(&ignore, "state\n")?;
    synced(&sandbox)?;
    assert_eq!(fs::read(&ignore)?, b"state\n");
    Ok(())
}

/// a run that fails while it places tells nothing it says once, so it records none of it either:
/// the next run that ends well tells it. here a file to put back cannot go into a folder the
/// person made read-only, beside an edit kept in another dataset.
#[cfg(unix)]
#[test]
fn a_failed_sync_records_nothing_it_never_told() -> Result {
    use std::os::unix::fs::PermissionsExt;
    if rustix::process::geteuid().is_root() {
        return crate::support::skipped("unprivileged", "root ignores file permissions");
    }
    let sandbox = Sandbox::new()?;
    let zip = zip_of(&[("red.csv", &b"r\n1\n"[..]), ("white.csv", b"w\n2\n")])?;
    let server = Routes::start(vec![
        (PATH.to_owned(), file(IRIS, "\"v1\"")),
        ("/files/wine.zip".to_owned(), file(&zip, "\"w1\"")),
    ])?;
    add_ok(&sandbox, &format!("{}{PATH}", server.url))?;
    add_ok(&sandbox, &format!("{}/files/wine.zip", server.url))?;
    let edited = sandbox.work().join("data/iris/iris.csv");
    delete(&edited)?;
    fs::write(&edited, "mine\n")?;
    let wine = sandbox.work().join("data/wine");
    delete(&wine.join("red.csv"))?;
    let before = fs::metadata(&wine)?.permissions();
    fs::set_permissions(&wine, fs::Permissions::from_mode(0o555))?;
    let failed = sandbox.run(&["sync"]);
    fs::set_permissions(&wine, before)?;
    let failed = failed?;
    let stderr = text(&failed.stderr)?;
    assert_eq!(failed.status.code(), Some(50), "{stderr}");
    assert!(!stderr.contains("has changed"), "{stderr}");
    let next = synced(&sandbox)?;
    assert!(
        next.contains("warning: data/iris/iris.csv has changed, so it stays\n"),
        "{next}"
    );
    Ok(())
}

/// a name data.toml lacks, typed beside one it has, gets an example that never repeats the one
/// given: another dataset's name where data.toml has one, else the command line without it, as
/// for a typo of a name given already.
#[test]
fn a_typo_beside_a_name_never_gets_that_name_twice() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Routes::start(vec![
        (PATH.to_owned(), file(IRIS, "\"v1\"")),
        ("/files/wine.csv".to_owned(), file(b"a,b\n1,2\n", "\"w1\"")),
    ])?;
    sandbox.allow(&server.url);
    add_ok(&sandbox, &format!("{}{PATH}", server.url))?;
    let refused = |args: &[&str]| -> Result<String> {
        let output = sandbox.run(args)?;
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(2), "{args:?}: {stderr}");
        Ok(stderr)
    };
    for command in [
        &["sync"][..],
        &["audit"],
        &["export"],
        &["drop"],
        &["lock", "--upgrade"],
        &["push", "--to", "mirror"],
    ] {
        // push takes its folder after the names
        let (named, after) = if command[0] == "push" {
            command.split_at(1)
        } else {
            (command, &[][..])
        };
        let typed: Vec<&str> = [named, &["iris", "nosuch"], after].concat();
        let meant: Vec<&str> = [named, &["iris"], after].concat();
        let stderr = refused(&typed)?;
        let help = format!("help: leave it out, as in `fetchloom {}`", meant.join(" "));
        assert!(stderr.contains(&help), "{typed:?}: {stderr}");
    }
    add_ok(&sandbox, &format!("{}/files/wine.csv", server.url))?;
    let stderr = refused(&["sync", "iris", "nosuch"])?;
    assert!(
        stderr.contains("help: name one of iris, wine, as in `fetchloom sync iris wine`"),
        "{stderr}"
    );
    for command in ["sync", "remove"] {
        let stderr = refused(&[command, "iris", "irs"])?;
        let help = format!("help: leave it out, as in `fetchloom {command} iris`");
        assert!(stderr.contains(&help), "{stderr}");
    }
    let stderr = refused(&["sync", "wine", "irs"])?;
    assert!(
        stderr.contains("help: did you mean `fetchloom sync wine iris`?"),
        "{stderr}"
    );
    Ok(())
}
