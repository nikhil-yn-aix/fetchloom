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
        document["warnings"]
            .as_array()
            .is_some_and(|warnings| warnings.iter().any(|told| told == warning)),
        "{}",
        document["warnings"]
    );
    assert_eq!(fs::read(folder.join("mine/cat_001.png"))?, b"cat");
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
        "   Excluded 2 files of ds000001 in data.toml and data.lock\n    Removed 2 files no dataset needs\n"
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
