//! `drop`: files left out of their dataset for good, written as patterns in data.toml, with no
//! request, a pin data.lock keeps current and the unchanged files gone from the data folder.

use std::fs;
use std::io::Write as _;
use std::path::PathBuf;

use serde_json::json;

use crate::support::{Result, Sandbox, Server, delete, file, json, text};

/// the pets the curator keeps, in one zip whose files share the folder `pets-v1`.
const PETS: [(&str, &[u8]); 4] = [
    ("pets-v1/images/blurry_011.png", b"blurry"),
    ("pets-v1/images/cat_001.png", b"cat"),
    ("pets-v1/images/dog_010_corrupt.png", b"broken"),
    ("pets-v1/labels.csv", b"file,label\n"),
];

fn zipped(entries: &[(&str, &[u8])]) -> Result<Vec<u8>> {
    let mut archive = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (path, bytes) in entries {
        archive.start_file(*path, zip::write::SimpleFileOptions::default())?;
        archive.write_all(bytes)?;
    }
    Ok(archive.finish()?.into_inner())
}

fn ok(output: &std::process::Output) -> Result {
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    Ok(())
}

/// a project with the dataset pets, added from a server that answers once.
fn pets(sandbox: &Sandbox) -> Result<Server> {
    let server = Server::start("/files/pets.zip", vec![file(&zipped(&PETS)?, "\"p1\"")])?;
    ok(&sandbox.run(&["add", &server.url])?)?;
    Ok(server)
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
        fs::write(folder.join(format!("{split}.zip")), zipped(&entries)?)?;
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
        "   Excluded 1 file of pets.zip in data.toml\n    Removed 1 file no dataset needs\n"
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
        "   Excluded 1 file of set in data.toml and data.lock\n    Removed 1 file no dataset needs\n"
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
            "data/pets/notes.txt is not a file data.lock places",
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
