//! `drop`: files left out of their dataset for good, written as patterns in data.toml, with no
//! request, a pin data.lock keeps current and the unchanged files gone from the data folder.

use std::fs;
use std::path::PathBuf;

use serde_json::json;

use crate::support::{
    Result, Sandbox, Server, delete, delete_folder, file, json, ok, text, without_copy_warning,
    zip_of,
};

/// the pets the curator keeps, in one zip whose files share the folder `pets-v1`.
const PETS: [(&str, &[u8]); 4] = [
    ("pets-v1/images/blurry_011.png", b"blurry"),
    ("pets-v1/images/cat_001.png", b"cat"),
    ("pets-v1/images/dog_010_corrupt.png", b"broken"),
    ("pets-v1/labels.csv", b"file,label\n"),
];

/// a project with the dataset pets, added from a server that answers once.
fn pets(sandbox: &Sandbox) -> Result<Server> {
    let server = Server::start("/files/pets.zip", vec![file(&zip_of(&PETS)?, "\"p1\"")])?;
    ok(&sandbox.run(&["add", &server.url])?)?;
    Ok(server)
}

/// a folder of plain files imported as the dataset tree: keep.txt, three files in sub and two
/// recordings in flac.
fn tree(sandbox: &Sandbox) -> Result<PathBuf> {
    let folder = sandbox.folder("tree")?;
    for (path, bytes) in [
        ("keep.txt", &b"keep\n"[..]),
        ("sub/s1.txt", b"s1\n"),
        ("sub/s2.txt", b"s2\n"),
        ("sub/s3.txt", b"s3\n"),
        ("flac/a.flac", b"a\n"),
        ("flac/b.flac", b"b\n"),
    ] {
        let file = folder.join(path);
        fs::create_dir_all(file.parent().ok_or("no parent")?)?;
        fs::write(file, bytes)?;
    }
    ok(&sandbox.run(&["add", "../tree"])?)?;
    Ok(sandbox.work().join("data").join("tree"))
}

/// a folder imported as the dataset set: notes beside train.zip and test.zip, which both hold
/// images/x.png.
fn set(sandbox: &Sandbox) -> Result<PathBuf> {
    let folder = sandbox.folder("set")?;
    fs::write(folder.join("notes [v1].md"), b"two splits\n")?;
    for split in ["train", "test"] {
        let entries: [(&str, &[u8]); 3] = [
            ("images/x.png", split.as_bytes()),
            ("images/y.png", b"y"),
            ("labels.csv", b"file,label\n"),
        ];
        fs::write(folder.join(format!("{split}.zip")), zip_of(&entries)?)?;
    }
    ok(&sandbox.run(&["add", "../set"])?)?;
    Ok(sandbox.work().join("data").join("set"))
}

/// the curator's journey: a bad sample inside an archive leaves for good as a pattern of the
/// archive's own paths, and nothing asks the server, then or at the next locked sync.
#[test]
fn drop_leaves_out_a_file_inside_an_archive_without_a_request() -> Result {
    let sandbox = Sandbox::new()?;
    let server = pets(&sandbox)?;
    let work = sandbox.work();
    let lock = fs::read(work.join("data.lock"))?;
    let output = sandbox.run(&["drop", "data/pets/images/dog_010_corrupt.png"])?;
    ok(&output)?;
    assert_eq!(
        text(&output.stderr)?,
        "   Excluded 1 file of pets.zip in data.toml\n    Removed 1 file from data/ that no dataset needs\n"
    );
    assert_eq!(
        fs::read_to_string(work.join("data.toml"))?,
        format!(
            "[datasets.pets]\nref = \"{}\"\n\n[datasets.pets.members.\"pets.zip\"]\nexclude = [\"pets-v1/images/dog_010_corrupt.png\"]\n",
            server.url
        )
    );
    let folder = work.join("data").join("pets");
    assert!(!folder.join("images/dog_010_corrupt.png").exists());
    assert_eq!(fs::read(folder.join("images/cat_001.png"))?, b"cat");
    assert_eq!(fs::read(work.join("data.lock"))?, lock);
    let locked = sandbox.run(&["--json", "sync", "--locked"])?;
    ok(&locked)?;
    assert_eq!(json(&locked)?["work"]["requests"], 0);
    assert_eq!(server.requests().len(), 1);
    Ok(())
}

#[test]
fn a_dry_run_names_what_drop_would_write_and_changes_nothing() -> Result {
    let sandbox = Sandbox::new()?;
    pets(&sandbox)?;
    let work = sandbox.work();
    let before = fs::read(work.join("data.toml"))?;
    let output = sandbox.run(&[
        "--json",
        "drop",
        "data/pets/images/dog_010_corrupt.png",
        "data/pets/images/blurry_011.png",
        "--dry-run",
    ])?;
    ok(&output)?;
    let result = &json(&output)?["result"];
    assert_eq!(result["dry_run"], true);
    assert_eq!(result["lock_changed"], false);
    assert_eq!(
        result["dropped"],
        json!([
            {"path": "data/pets/images/blurry_011.png", "dataset": "pets", "archive": "pets.zip", "pattern": "pets-v1/images/blurry_011.png"},
            {"path": "data/pets/images/dog_010_corrupt.png", "dataset": "pets", "archive": "pets.zip", "pattern": "pets-v1/images/dog_010_corrupt.png"},
        ])
    );
    assert_eq!(
        result["changes"]["remove"],
        json!([
            "data/pets/images/blurry_011.png",
            "data/pets/images/dog_010_corrupt.png"
        ])
    );
    assert_eq!(fs::read(work.join("data.toml"))?, before);
    assert!(work.join("data/pets/images/dog_010_corrupt.png").exists());
    Ok(())
}

/// a disk such as fat settles a file's time after it is written, so a file fetchloom placed can
/// look changed though it still holds the placed bytes. a drop removes it all the same.
#[test]
fn a_dropped_file_whose_time_moved_still_goes() -> Result {
    let sandbox = Sandbox::new()?;
    let folder = set(&sandbox)?;
    let file = folder.join("train/images/x.png");
    let bytes = fs::read(&file)?;
    let modified = fs::metadata(&file)?.modified()?;
    // a copy of its own, as fat places it, with the time the disk settled
    delete(&file)?;
    fs::write(&file, &bytes)?;
    fs::File::options()
        .write(true)
        .open(&file)?
        .set_modified(modified + std::time::Duration::from_secs(2))?;
    let mut permissions = fs::metadata(&file)?.permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&file, permissions)?;

    let output = sandbox.run(&["drop", "data/set/train/images/x.png"])?;

    ok(&output)?;
    let stderr = text(&output.stderr)?;
    assert!(!file.exists(), "{stderr}");
    assert!(!stderr.contains("has changed"), "{stderr}");
    Ok(())
}

/// train.zip and test.zip both hold images/x.png; a table names one archive.
#[test]
fn drop_excludes_a_member_of_its_own_archive_only() -> Result {
    let sandbox = Sandbox::new()?;
    let folder = set(&sandbox)?;
    ok(&sandbox.run(&["drop", "data/set/train/images/x.png"])?)?;
    assert!(!folder.join("train/images/x.png").exists());
    assert_eq!(fs::read(folder.join("test/images/x.png"))?, b"test");
    assert_eq!(
        fs::read_to_string(sandbox.work().join("data.toml"))?,
        "[datasets.set]\nref = \"local\"\n\n[datasets.set.members.\"train.zip\"]\nexclude = [\"images/x.png\"]\n"
    );
    Ok(())
}

/// data.toml for [`set`] with one members table under the glob key `*.zip`, which keeps the
/// images of both zips.
const GLOBBED: &str = "[datasets.set]\nref = \"local\"\n\n[datasets.set.members.\"*.zip\"]\nselect = [\"images/*\"]\n";

/// [`set`] synced with [`GLOBBED`] as its data.toml.
fn globbed(sandbox: &Sandbox) -> Result<PathBuf> {
    let folder = set(sandbox)?;
    fs::write(sandbox.work().join("data.toml"), GLOBBED)?;
    ok(&sandbox.run(&["sync"])?)?;
    assert!(!folder.join("train/labels.csv").exists());
    Ok(folder)
}

/// a file a glob key's table leaves out already is told by that key, as for any table.
#[test]
fn a_file_a_glob_table_leaves_out_is_named_by_its_key() -> Result {
    let sandbox = Sandbox::new()?;
    globbed(&sandbox)?;
    let output = sandbox.run(&["--json", "drop", "data/set/test/labels.csv"])?;
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        json(&output)?["errors"][0]["message"],
        "data/set/test/labels.csv is left out already, since select in members.\"*.zip\" of datasets.set does not keep it"
    );
    Ok(())
}

/// a file of an archive a glob key's table covers is left out in a table of the archive's own,
/// copied from the glob's, since an archive's own table wins over a glob: the archive keeps what
/// the glob chose, and the pattern never reaches the other archives the glob covers.
#[test]
fn drop_under_a_glob_table_writes_the_archives_own_table_from_it() -> Result {
    let sandbox = Sandbox::new()?;
    let folder = globbed(&sandbox)?;
    let preview = sandbox.run(&["--json", "drop", "-n", "data/set/train/images/x.png"])?;
    ok(&preview)?;
    assert_eq!(
        json(&preview)?["result"]["copied"],
        json!([{"dataset": "set", "archive": "train.zip", "from": "*.zip"}])
    );
    assert_eq!(
        fs::read_to_string(sandbox.work().join("data.toml"))?,
        GLOBBED
    );
    let output = sandbox.run(&["drop", "data/set/train/images/x.png"])?;
    ok(&output)?;
    assert!(
        text(&output.stderr)?.contains(
            "  hint: train.zip now has its own members table, copied from members.\"*.zip\", so later changes to that table do not reach it\n"
        ),
        "{}",
        text(&output.stderr)?
    );
    assert_eq!(
        fs::read_to_string(sandbox.work().join("data.toml"))?,
        format!(
            "{GLOBBED}\n[datasets.set.members.\"train.zip\"]\nselect = [\"images/*\"]\nexclude = [\"images/x.png\"]\n"
        )
    );
    assert!(!folder.join("train/images/x.png").exists());
    assert!(
        !folder.join("train/labels.csv").exists(),
        "the select stays"
    );
    assert_eq!(fs::read(folder.join("test/images/x.png"))?, b"test");
    let synced = sandbox.run(&["--json", "sync", "--locked"])?;
    ok(&synced)?;
    let placement = &json(&synced)?["result"]["placement"];
    assert_eq!(
        (&placement["placed"], &placement["removed"]),
        (&json!(0), &json!(0))
    );
    Ok(())
}

/// leaving out every file of an archive leaves out the archive, and a glob key's table goes once
/// it matches no archive left, as an archive's own table goes with it.
#[test]
fn a_glob_table_goes_with_the_last_archive_it_matches() -> Result {
    let sandbox = Sandbox::new()?;
    let folder = globbed(&sandbox)?;
    let manifest = sandbox.work().join("data.toml");
    ok(&sandbox.run(&["drop", "data/set/test"])?)?;
    assert_eq!(
        fs::read_to_string(&manifest)?,
        "[datasets.set]\nref = \"local\"\nexclude = [\"test.zip\"]\n\n[datasets.set.members.\"*.zip\"]\nselect = [\"images/*\"]\n",
        "train.zip still takes the table"
    );
    ok(&sandbox.run(&["drop", "data/set/train"])?)?;
    assert_eq!(
        fs::read_to_string(&manifest)?,
        "[datasets.set]\nref = \"local\"\nexclude = [\n  \"test.zip\",\n  \"train.zip\",\n]\n"
    );
    assert!(!folder.join("train").exists());
    ok(&sandbox.run(&["sync", "--locked"])?)?;
    Ok(())
}

/// a glob key's select pattern that matches no file of the archive drop copies its table for
/// stays out of the copy: it keeps nothing there, and the archive's own table, held to today's
/// rule, would refuse it.
#[test]
fn drop_copies_only_the_select_patterns_its_archive_matches() -> Result {
    let sandbox = Sandbox::new()?;
    let folder = sandbox.folder("uneven")?;
    fs::write(
        folder.join("a.zip"),
        zip_of(&[("x.png", b"x"), ("notes.md", b"n")])?,
    )?;
    fs::write(
        folder.join("b.zip"),
        zip_of(&[("y.png", b"y"), ("z.png", b"z")])?,
    )?;
    ok(&sandbox.run(&["add", "../uneven"])?)?;
    let manifest = sandbox.work().join("data.toml");
    let glob = "[datasets.uneven]\nref = \"local\"\n\n[datasets.uneven.members.\"*.zip\"]\nselect = [\"*.png\", \"*.md\"]\n";
    fs::write(&manifest, glob)?;
    ok(&sandbox.run(&["sync"])?)?;
    ok(&sandbox.run(&["drop", "data/uneven/b/y.png"])?)?;
    assert_eq!(
        fs::read_to_string(&manifest)?,
        format!(
            "{glob}\n[datasets.uneven.members.\"b.zip\"]\nselect = [\"*.png\"]\nexclude = [\"y.png\"]\n"
        )
    );
    let data = sandbox.work().join("data/uneven");
    assert!(!data.join("b/y.png").exists());
    assert_eq!(fs::read(data.join("b/z.png"))?, b"z");
    ok(&sandbox.run(&["sync", "--locked"])?)?;
    Ok(())
}

/// a file the source lists leaves data.lock too, rewritten as relocking would pin it, so a
/// locked sync takes it. an import could not be listed again, so a stale pin would fail here.
#[test]
fn drop_of_a_listed_file_keeps_the_lock_current() -> Result {
    let sandbox = Sandbox::new()?;
    let folder = set(&sandbox)?;
    let work = sandbox.work();
    let output = sandbox.run(&["--json", "drop", "data/set/notes [v1].md"])?;
    ok(&output)?;
    let result = &json(&output)?["result"];
    assert_eq!(result["lock_changed"], true);
    assert_eq!(result["dropped"][0]["archive"], serde_json::Value::Null);
    assert_eq!(result["dropped"][0]["pattern"], "notes [[]v1].md");
    assert_eq!(
        fs::read_to_string(work.join("data.toml"))?,
        "[datasets.set]\nref = \"local\"\nexclude = [\"notes [[]v1].md\"]\n"
    );
    assert!(!folder.join("notes [v1].md").exists());
    assert!(!fs::read_to_string(work.join("data.lock"))?.contains("notes"));
    ok(&sandbox.run(&["sync", "--locked"])?)?;
    let status = sandbox.run(&["status"])?;
    assert_eq!(
        text(&status.stderr)?,
        "         Ok everything matches data.lock by size and time\n"
    );
    let again = sandbox.run(&["--json", "drop", "data/set/notes [v1].md"])?;
    assert_eq!(again.status.code(), Some(2));
    assert_eq!(
        json(&again)?["errors"][0]["message"],
        "data/set/notes [v1].md is left out already, by exclude \"notes [[]v1].md\" in datasets.set"
    );
    Ok(())
}

/// a folder becomes one pattern when that pattern leaves out exactly its files, and every file
/// an archive keeps becomes the archive itself, so its members table goes.
#[test]
fn a_folder_is_dropped_as_one_pattern_and_a_whole_archive_leaves_data_lock() -> Result {
    let sandbox = Sandbox::new()?;
    let folder = set(&sandbox)?;
    let work = sandbox.work();
    ok(&sandbox.run(&["drop", "data/set/test/images"])?)?;
    assert_eq!(
        fs::read_to_string(work.join("data.toml"))?,
        "[datasets.set]\nref = \"local\"\n\n[datasets.set.members.\"test.zip\"]\nexclude = [\"images/**\"]\n"
    );
    let output = sandbox.run(&["drop", "data/set/test"])?;
    ok(&output)?;
    assert_eq!(
        text(&output.stderr)?,
        "   Excluded 1 file of set in data.toml and data.lock\n    Removed 1 file from data/ that no dataset needs\n"
    );
    assert_eq!(
        fs::read_to_string(work.join("data.toml"))?,
        "[datasets.set]\nref = \"local\"\nexclude = [\"test.zip\"]\n"
    );
    assert!(!fs::read_to_string(work.join("data.lock"))?.contains("test.zip"));
    assert!(!folder.join("test").exists());
    assert_eq!(fs::read(folder.join("train/images/x.png"))?, b"train");
    ok(&sandbox.run(&["sync", "--locked"])?)?;
    Ok(())
}

/// a file someone changed is never deleted: drop writes the pattern and says the file stays.
#[test]
fn drop_keeps_an_edited_file_and_says_so() -> Result {
    let sandbox = Sandbox::new()?;
    pets(&sandbox)?;
    let labels = sandbox.work().join("data/pets/labels.csv");
    delete(&labels)?;
    fs::write(&labels, b"my own labels\n")?;
    let output = sandbox.run(&["drop", "data/pets/labels.csv"])?;
    ok(&output)?;
    let stderr = text(&output.stderr)?;
    assert!(
        stderr.contains(
            "warning: data/pets/labels.csv is no longer needed, but it has changed, so it stays\n"
        ),
        "{stderr}"
    );
    assert_eq!(fs::read(&labels)?, b"my own labels\n");
    assert!(
        fs::read_to_string(sandbox.work().join("data.toml"))?
            .contains("exclude = [\"pets-v1/labels.csv\"]")
    );
    Ok(())
}

/// a teammate who pulls the new data.toml drops the file at their next sync, with no download.
#[test]
fn a_clone_syncs_a_drop_without_downloads() -> Result {
    let sandbox = Sandbox::new()?;
    let server = pets(&sandbox)?;
    let work = sandbox.work();
    let clone = sandbox.folder("clone")?;
    for name in ["data.toml", "data.lock"] {
        fs::copy(work.join(name), clone.join(name))?;
    }
    ok(&sandbox.command_in(&clone, &["sync"]).output()?)?;
    let dog = clone.join("data/pets/images/dog_010_corrupt.png");
    assert!(dog.exists());
    ok(&sandbox.run(&["drop", "data/pets/images/dog_010_corrupt.png"])?)?;
    fs::copy(work.join("data.toml"), clone.join("data.toml"))?;
    let pulled = sandbox
        .command_in(&clone, &["--json", "sync", "--locked"])
        .output()?;
    ok(&pulled)?;
    let document = json(&pulled)?;
    assert_eq!(document["work"]["requests"], 0);
    assert_eq!(document["result"]["placement"]["removed"], 1);
    assert!(!dog.exists());
    assert_eq!(server.requests().len(), 1);
    Ok(())
}

/// drop leaves out only files data.lock places, never every file of a dataset, never moves
/// the files a dataset keeps, and rewrites a pin only while it answers data.toml. each refusal
/// changes nothing.
#[test]
fn drop_refuses_what_it_cannot_leave_out_and_changes_nothing() -> Result {
    let sandbox = Sandbox::new()?;
    pets(&sandbox)?;
    set(&sandbox)?;
    let work = sandbox.work();
    let manifest = work.join("data.toml");
    fs::write(
        &manifest,
        format!(
            "{}\n[datasets.clean]\nfrom = [\"pets\"]\nrun = [\"python3\", \"clean.py\", \"{{pets}}\", \"{{out}}\"]\n",
            fs::read_to_string(&manifest)?
        ),
    )?;
    let before = (fs::read(&manifest)?, fs::read(work.join("data.lock"))?);
    for (arguments, code, message) in [
        (
            &["drop", "../notes.txt"][..],
            2,
            "../notes.txt is not inside the data folder, data/",
        ),
        (
            &["drop", "data/pets/notes.txt"],
            2,
            "data/pets/notes.txt names no file of pets",
        ),
        (
            &["drop", "data/pets"],
            2,
            "that is every file of pets, and a dataset needs one",
        ),
        (
            &["drop", "data/clean/x.csv"],
            2,
            "data/clean/x.csv belongs to clean, which a recipe makes",
        ),
        (
            &["drop", "data/set/notes [v1].md", "data/set/test"],
            2,
            "that would leave train.zip the only file of set, and its files would move, since a dataset that is one archive places them straight in its folder",
        ),
    ] {
        let output = sandbox.run(&[&["--json"][..], arguments].concat())?;
        assert_eq!(output.status.code(), Some(code), "{arguments:?}");
        assert_eq!(json(&output)?["errors"][0]["message"], message);
    }
    let stale = String::from_utf8(before.0.clone())?.replace(
        "[datasets.set]\nref = \"local\"\n",
        "[datasets.set]\nref = \"local\"\nexclude = \"*.tmp\"\n",
    );
    fs::write(&manifest, &stale)?;
    let output = sandbox.run(&["--json", "drop", "data/set/notes [v1].md"])?;
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(json(&output)?["errors"][0]["kind"], "lock.outdated");
    assert_eq!(fs::read_to_string(&manifest)?, stale);
    fs::write(&manifest, &before.0)?;
    assert_eq!(
        (fs::read(&manifest)?, fs::read(work.join("data.lock"))?),
        before
    );
    let fresh = sandbox.folder("fresh")?;
    for name in ["data.toml", "data.lock"] {
        fs::copy(work.join(name), fresh.join(name))?;
    }
    let unknown = sandbox
        .command_in(&fresh, &["drop", "data/pets/images/cat_001.png"])
        .env("FETCHLOOM_CACHE", sandbox.folder("fresh-cache")?)
        .output()?;
    assert_eq!(unknown.status.code(), Some(2));
    assert!(
        text(&unknown.stderr)?
            .contains("run `fetchloom sync` first, so fetchloom knows what pets.zip holds"),
        "{}",
        text(&unknown.stderr)?
    );
    Ok(())
}

/// a file of a dataset with `place = false`, raw data only recipes read, is dropped by the path
/// it would have in the data folder, as for any dataset. a file left out already says so, naming
/// the pattern, and a path that names no file says neither.
#[test]
fn drop_leaves_out_a_file_of_a_dataset_kept_out_of_the_data_folder() -> Result {
    let sandbox = Sandbox::new()?;
    pets(&sandbox)?;
    let work = sandbox.work();
    crate::recipes::unplace(&work, "pets")?;
    ok(&sandbox.run(&["sync"])?)?;
    let lock = fs::read(work.join("data.lock"))?;
    let output = sandbox.run(&["--json", "drop", "data/pets/images/dog_010_corrupt.png"])?;
    ok(&output)?;
    assert_eq!(
        json(&output)?["result"]["dropped"],
        json!([{"path": "data/pets/images/dog_010_corrupt.png", "dataset": "pets", "archive": "pets.zip", "pattern": "pets-v1/images/dog_010_corrupt.png"}])
    );
    let manifest = fs::read_to_string(work.join("data.toml"))?;
    assert!(
        manifest.ends_with("[datasets.pets.members.\"pets.zip\"]\nexclude = [\"pets-v1/images/dog_010_corrupt.png\"]\n"),
        "{manifest}"
    );
    assert_eq!(fs::read(work.join("data.lock"))?, lock);
    let keep = "check the path; a file of your own needs no drop: delete it yourself if you do not need it";
    for (path, message, help) in [
        (
            "data/pets/images/dog_010_corrupt.png",
            "data/pets/images/dog_010_corrupt.png is left out already, by exclude \"pets-v1/images/dog_010_corrupt.png\" in members.\"pets.zip\" of datasets.pets",
            "to bring it back, delete that pattern from data.toml and run `fetchloom sync`",
        ),
        (
            "data/pets/images/fish.png",
            "data/pets/images/fish.png names no file of pets",
            keep,
        ),
        (
            "data/fish.png",
            "data/fish.png names no file of any dataset",
            keep,
        ),
    ] {
        let output = sandbox.run(&["--json", "drop", path])?;
        assert_eq!(output.status.code(), Some(2), "{path}");
        let error = &json(&output)?["errors"][0];
        assert_eq!(
            (&error["message"], &error["help"]),
            (&json!(message), &json!(help))
        );
    }
    assert_eq!(fs::read_to_string(work.join("data.toml"))?, manifest);
    Ok(())
}

/// a sample deleted by hand comes back at the next sync, since data.lock still pins it, and the
/// sync says so: which files, and how to leave one out for good.
#[test]
fn a_sync_says_it_puts_back_files_someone_deleted() -> Result {
    let sandbox = Sandbox::new()?;
    pets(&sandbox)?;
    let folder = sandbox.work().join("data").join("pets");
    let dog = folder.join("images/dog_010_corrupt.png");
    delete(&dog)?;
    let output = sandbox.run(&["sync"])?;
    ok(&output)?;
    let stderr = text(&output.stderr)?;
    assert!(
        stderr.contains("warning: put back data/pets/images/dog_010_corrupt.png, which was deleted or moved, since data.lock pins it\n  help: to leave a file out for good, run `fetchloom drop data/pets/images/dog_010_corrupt.png`; keep files of your own in a folder outside data/\n"),
        "{stderr}"
    );
    assert_eq!(fs::read(&dog)?, b"broken");

    delete(&folder.join("labels.csv"))?;
    fs::create_dir(folder.join("mine"))?;
    fs::rename(
        folder.join("images/cat_001.png"),
        folder.join("mine/cat_001.png"),
    )?;
    let output = sandbox.run(&["--json", "sync"])?;
    ok(&output)?;
    let document = json(&output)?;
    assert_eq!(
        document["result"]["placement"]["put_back"],
        json!(["data/pets/images/cat_001.png", "data/pets/labels.csv"])
    );
    assert_eq!(document["result"]["placement"]["placed"], 2);
    // a drive that cannot link warns of that too, so the warning is found, not counted
    let warning = "put back 2 files that were deleted or moved, since data.lock pins them: data/pets/images/cat_001.png, data/pets/labels.csv; to leave a file out for good, run `fetchloom drop data/pets/images/cat_001.png`; keep files of your own in a folder outside data/";
    assert!(
        crate::support::warned(&document)
            .iter()
            .any(|told| *told == warning),
        "{}",
        document["warnings"]
    );
    assert_eq!(fs::read(folder.join("mine/cat_001.png"))?, b"cat");
    Ok(())
}

/// a folder deleted whole inside a dataset is named once, with the drop that leaves the folder
/// out. a dataset's own folder deleted whole, or the whole data folder, is a fresh start: the
/// sync places it again and says only that the folder was missing.
#[test]
fn a_sync_names_a_deleted_folder_once_and_refills_a_deleted_dataset_quietly() -> Result {
    let sandbox = Sandbox::new()?;
    let folder = tree(&sandbox)?;
    delete_folder(&folder.join("sub"))?;
    let output = sandbox.run(&["sync"])?;
    ok(&output)?;
    let stderr = text(&output.stderr)?;
    assert!(
        stderr.contains("warning: put back the 3 files in data/tree/sub/, which was deleted or moved, since data.lock pins them\n  help: to leave a folder out for good, run `fetchloom drop data/tree/sub`; keep files of your own in a folder outside data/\n"),
        "{stderr}"
    );
    for gone in [folder.clone(), sandbox.work().join("data")] {
        delete_folder(&gone)?;
        let output = sandbox.run(&["sync"])?;
        ok(&output)?;
        let stderr = text(&output.stderr)?;
        assert!(
            !without_copy_warning(&stderr).contains("warning"),
            "{stderr}"
        );
        assert!(
            stderr.contains("     Placed data/tree · 6 files ")
                && stderr.contains(" · the folder was missing\n"),
            "{stderr}"
        );
        assert_eq!(fs::read(folder.join("sub/s2.txt"))?, b"s2\n");
    }
    Ok(())
}

/// a folder of files the source lists leaves as one pattern, which stays the same however the
/// paths overlap, and each file keeps its own record. files named one by one, as a shell expands
/// `flac/*.flac`, stay one pattern each, so files added to the folder later are not left out.
#[test]
fn a_folder_of_listed_files_leaves_as_one_pattern() -> Result {
    let sandbox = Sandbox::new()?;
    let folder = tree(&sandbox)?;
    let work = sandbox.work();
    for arguments in [
        ["data/tree/sub/s2.txt", "data/tree/sub"],
        ["data/tree/sub", "data/tree/sub/s2.txt"],
    ] {
        let output = sandbox.run(&[&["--json", "drop", "--dry-run"][..], &arguments].concat())?;
        ok(&output)?;
        let record = |name: &str| json!({"path": format!("data/tree/sub/{name}"), "dataset": "tree", "archive": null, "pattern": "sub/**"});
        assert_eq!(
            json(&output)?["result"]["dropped"],
            json!([record("s1.txt"), record("s2.txt"), record("s3.txt")]),
            "{arguments:?}"
        );
    }
    ok(&sandbox.run(&["drop", "data/tree/sub"])?)?;
    assert_eq!(
        fs::read_to_string(work.join("data.toml"))?,
        "[datasets.tree]\nref = \"local\"\nexclude = [\"sub/**\"]\n"
    );
    assert!(!folder.join("sub/s1.txt").exists());
    assert_eq!(fs::read(folder.join("keep.txt"))?, b"keep\n");
    assert!(!fs::read_to_string(work.join("data.lock"))?.contains("sub/"));
    ok(&sandbox.run(&["sync", "--locked"])?)?;

    ok(&sandbox.run(&["drop", "data/tree/flac/a.flac", "data/tree/flac/b.flac"])?)?;
    assert_eq!(
        fs::read_to_string(work.join("data.toml"))?,
        "[datasets.tree]\nref = \"local\"\nexclude = [\n  \"sub/**\",\n  \"flac/a.flac\",\n  \"flac/b.flac\",\n]\n"
    );
    Ok(())
}

/// a dataset downloaded from a listing, here a bucket, leaves a folder as one pattern too, and
/// neither the drop nor a locked sync after it asks the server anything.
#[cfg(debug_assertions)]
#[test]
fn a_folder_of_a_downloaded_dataset_leaves_as_one_pattern() -> Result {
    use crate::buckets::{md5, s3_listing, s3_page};
    let sandbox = Sandbox::new()?;
    let base = "/s3.amazonaws.com/openneuro.org/";
    let objects: [(&str, &[u8]); 4] = [
        ("ds000001/README", b"a tiny dataset\n"),
        ("ds000001/sub-01/anat.tsv", b"anat\n"),
        ("ds000001/sub-01/func.tsv", b"func\n"),
        ("ds000001/sub-02/anat.tsv", b"other anat\n"),
    ];
    let mut routes = vec![(s3_listing(base, "ds000001/", None), s3_page(&objects, None))];
    for (key, body) in objects {
        routes.push((
            format!("{base}{key}"),
            file(body, &format!("\"{}\"", md5(body))),
        ));
    }
    let routes = crate::support::Routes::start(routes)?;
    let run = |arguments: &[&str]| {
        sandbox
            .command(arguments)
            .env("FETCHLOOM_TEST_BUCKETS", &routes.url)
            .output()
    };
    ok(&run(&["add", "s3://openneuro.org/ds000001/"])?)?;
    let asked = routes.requests().len();
    let work = sandbox.work();
    let output = run(&["drop", "data/ds000001/sub-01"])?;
    ok(&output)?;
    assert_eq!(
        text(&output.stderr)?,
        "   Excluded 2 files of ds000001 in data.toml and data.lock\n    Removed 2 files from data/ that no dataset needs\n"
    );
    let manifest = fs::read_to_string(work.join("data.toml"))?;
    assert!(
        manifest.ends_with("\nexclude = [\"sub-01/**\"]\n"),
        "{manifest}"
    );
    let folder = work.join("data").join("ds000001");
    assert!(!folder.join("sub-01").join("anat.tsv").exists());
    assert_eq!(
        fs::read(folder.join("sub-02").join("anat.tsv"))?,
        b"other anat\n"
    );
    let locked = run(&["--json", "sync", "--locked"])?;
    ok(&locked)?;
    assert_eq!(json(&locked)?["work"]["requests"], 0);
    assert_eq!(routes.requests().len(), asked);
    Ok(())
}

/// a ctrl-c while drop plans its patterns ends it there, before it writes or claims anything.
/// data.lock is a fifo the test fills only once the drop heard the ctrl-c, so the stop always
/// comes after the drop began and before it plans.
#[cfg(unix)]
#[test]
fn a_stop_while_drop_plans_changes_nothing_and_claims_nothing() -> Result {
    use std::io::Write as _;
    use std::os::unix::fs::FileTypeExt as _;

    use crate::support::Running;
    let sandbox = Sandbox::new()?;
    let folder = tree(&sandbox)?;
    let work = sandbox.work();
    let files = |folder: &std::path::Path| -> Result<Vec<(PathBuf, Vec<u8>)>> {
        let mut found = Vec::new();
        for name in ["keep.txt", "sub/s1.txt", "sub/s2.txt", "sub/s3.txt"] {
            found.push((folder.join(name), fs::read(folder.join(name))?));
        }
        Ok(found)
    };
    let placed = files(&folder)?;
    let manifest = fs::read(work.join("data.toml"))?;
    let lock_path = work.join("data.lock");
    let lock = fs::read(&lock_path)?;
    fs::remove_file(&lock_path)?;
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&lock_path)
            .status()?
            .success()
    );
    let mut command = sandbox.command(&["drop", "data/tree/sub"]);
    let running = Running::start(command.stdin(std::process::Stdio::null()))?;
    // opening the fifo waits for the drop to open it, long after it listens for ctrl-c
    let (opened, open) = std::sync::mpsc::channel();
    let fifo = lock_path.clone();
    std::thread::spawn(move || {
        let _ = opened.send(fs::OpenOptions::new().write(true).open(fifo));
    });
    let mut writer = open.recv_timeout(std::time::Duration::from_secs(30))??;
    running.interrupt()?;
    running.wait_for("Stopping")?;
    writer.write_all(&lock)?;
    drop(writer);
    let output = running.finish()?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(130), "{stderr}");
    assert_eq!(
        stderr,
        "   Stopping (press ctrl-c again to quit at once)\n    Stopped at your request, so nothing changed\n"
    );
    assert!(fs::symlink_metadata(&lock_path)?.file_type().is_fifo());
    assert_eq!(fs::read(work.join("data.toml"))?, manifest);
    assert_eq!(files(&folder)?, placed);
    fs::remove_file(&lock_path)?;
    fs::write(&lock_path, &lock)?;
    ok(&sandbox.run(&["drop", "data/tree/sub"])?)?;
    assert!(!folder.join("sub/s1.txt").exists());
    assert_eq!(fs::read(folder.join("keep.txt"))?, b"keep\n");
    Ok(())
}

/// a recipe made from a dataset a drop changed has a result data.lock pinned from the files the
/// drop left out, so the next step a person sees is that build, also for raw data only recipes
/// read. a drop no recipe depends on says to commit.
#[cfg(target_os = "linux")]
#[test]
fn drop_names_the_build_that_reads_what_it_left_out() -> Result {
    let sandbox = Sandbox::new()?;
    pets(&sandbox)?;
    set(&sandbox)?;
    let work = sandbox.work();
    crate::recipes::unplace(&work, "pets")?;
    ok(&sandbox.run(&["sync"])?)?;
    let Some(screen) = sandbox.on_a_terminal(&["drop", "data/set/notes [v1].md"])? else {
        return Ok(());
    };
    assert!(
        screen.contains(
            "\n  next: commit data.toml and data.lock; other clones drop them at their next `fetchloom sync`\n"
        ),
        "{screen}"
    );
    let manifest = work.join("data.toml");
    fs::write(
        &manifest,
        format!(
            "{}\n[datasets.clean]\nfrom = [\"pets\"]\nrun = [\"python3\", \"clean.py\", \"{{pets}}\", \"{{out}}\"]\n\n[datasets.small]\nfrom = [\"clean\"]\nrun = [\"python3\", \"small.py\", \"{{clean}}\", \"{{out}}\"]\n",
            fs::read_to_string(&manifest)?
        ),
    )?;
    let screen = sandbox
        .on_a_terminal(&["drop", "data/pets/images/dog_010_corrupt.png"])?
        .unwrap_or_default();
    assert!(
        screen.contains(
            "   Excluded 1 file of pets.zip in data.toml\n  next: run `fetchloom build small`, then commit data.toml, data.lock and data.lock.d\n"
        ),
        "{screen}"
    );
    Ok(())
}

/// a path one typo away from a file or folder drop knows gets the command that names it, with
/// the rest of the line as typed and the path as typed from the folder it was typed in.
#[test]
fn a_path_one_typo_away_names_the_drop_that_was_meant() -> Result {
    let sandbox = Sandbox::new()?;
    pets(&sandbox)?;
    let work = sandbox.work();
    for (arguments, message, meant) in [
        (
            &["drop", "data/pets/lables.csv"][..],
            "data/pets/lables.csv names no file of pets",
            "fetchloom drop data/pets/labels.csv",
        ),
        (
            &["drop", "data/pest/images/cat_001.png", "--dry-run"],
            "data/pest/images/cat_001.png names no file of any dataset",
            "fetchloom drop data/pets/images/cat_001.png --dry-run",
        ),
        (
            &["drop", "data/pets/imagse"],
            "data/pets/imagse names no file of pets",
            "fetchloom drop data/pets/images",
        ),
    ] {
        let output = sandbox.run(arguments)?;
        assert_eq!(output.status.code(), Some(2), "{arguments:?}");
        assert_eq!(
            text(&output.stderr)?,
            format!("error: {message}\n  help: did you mean `{meant}`?\n")
        );
    }
    let inside = sandbox
        .command_in(
            &work.join("data/pets/images"),
            &["drop", "dog_01_corrupt.png"],
        )
        .output()?;
    assert_eq!(inside.status.code(), Some(2));
    assert!(
        text(&inside.stderr)?
            .ends_with("  help: did you mean `fetchloom drop dog_010_corrupt.png`?\n"),
        "{}",
        text(&inside.stderr)?
    );
    Ok(())
}

/// drop takes paths, so a quoted pattern names no file: the error says so and gives the
/// pattern in the members table that does leave those files out, which works as given.
#[test]
fn a_quoted_pattern_is_explained_with_the_exclude_that_does_it() -> Result {
    let sandbox = Sandbox::new()?;
    pets(&sandbox)?;
    let work = sandbox.work();
    let output = sandbox.run(&["drop", "data/pets/images/*.png"])?;
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        text(&output.stderr)?,
        "error: data/pets/images/*.png names no file of pets\n  drop takes paths, not patterns, so * is part of the name it looks for\n  help: to leave out every file a pattern matches, add \"pets-v1/images/*.png\" to exclude in members.\"pets.zip\" of datasets.pets in data.toml by hand, then run `fetchloom sync`\n"
    );
    let manifest = work.join("data.toml");
    let agent = sandbox.run(&["--json", "drop", "data/pets/images/*.png"])?;
    assert_eq!(
        json(&agent)?["errors"][0]["fix"],
        serde_json::Value::Null,
        "a sync alone leaves the files in"
    );
    fs::write(
        &manifest,
        format!(
            "{}\n[datasets.pets.members.\"pets.zip\"]\nexclude = [\"pets-v1/images/*.png\"]\n",
            fs::read_to_string(&manifest)?
        ),
    )?;
    ok(&sandbox.run(&["sync"])?)?;
    let folder = work.join("data").join("pets");
    assert!(!folder.join("images/cat_001.png").exists());
    assert_eq!(fs::read(folder.join("labels.csv"))?, b"file,label\n");
    Ok(())
}

/// a file whose name holds a `*` is a path like any other.
#[cfg(unix)]
#[test]
fn a_file_named_with_a_star_drops_by_its_name() -> Result {
    let sandbox = Sandbox::new()?;
    let folder = sandbox.folder("odd")?;
    fs::write(folder.join("a*b.txt"), b"star\n")?;
    fs::write(folder.join("keep.txt"), b"keep\n")?;
    ok(&sandbox.run(&["add", "../odd"])?)?;
    ok(&sandbox.run(&["drop", "data/odd/a*b.txt"])?)?;
    assert_eq!(
        fs::read_to_string(sandbox.work().join("data.toml"))?,
        "[datasets.odd]\nref = \"local\"\nexclude = [\"a[*]b.txt\"]\n"
    );
    Ok(())
}
