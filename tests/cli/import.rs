//! `add <path>`: files on this machine pinned by their bytes, never by where they are, and a
//! clone that lacks those bytes getting them from a copy or from the folder they came from.

use std::fs;
use std::path::Path;

use crate::support::{Result, Sandbox, json, text};

/// a folder of two files, one of them in a folder of its own, as a person keeps measurements.
fn flowers(sandbox: &Sandbox) -> Result<std::path::PathBuf> {
    let folder = sandbox.folder("flowers")?;
    fs::create_dir_all(folder.join("petals"))?;
    fs::write(folder.join("sepals.csv"), b"length,width\n5.1,3.5\n")?;
    fs::write(folder.join("petals/red.csv"), b"length\n1.4\n")?;
    Ok(folder)
}

/// what a check that a file stayed as it was compares: its bytes, length and modified time, and on
/// unix its inode and mode, which a hardlink or a sealed copy would change.
fn look(path: &Path) -> Result<(Vec<u8>, u64, std::time::SystemTime, u64, u32)> {
    let metadata = fs::symlink_metadata(path)?;
    #[cfg(unix)]
    let (inode, mode) = {
        use std::os::unix::fs::MetadataExt;
        (metadata.ino(), metadata.mode())
    };
    #[cfg(not(unix))]
    let (inode, mode) = (0, 0);
    Ok((
        fs::read(path)?,
        metadata.len(),
        metadata.modified()?,
        inode,
        mode,
    ))
}

#[test]
fn adding_a_folder_pins_bytes_not_its_path() -> Result {
    let sandbox = Sandbox::new()?;
    let folder = flowers(&sandbox)?;
    let before = [
        look(&folder.join("sepals.csv"))?,
        look(&folder.join("petals/red.csv"))?,
    ];
    let output = sandbox.run(&["--json", "add", "../flowers"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
    assert_eq!(json(&output)?["work"]["requests"], 0);
    let work = sandbox.work();
    assert_eq!(
        fs::read_to_string(work.join("data.toml"))?,
        "[datasets.flowers]\nref = \"local\"\n"
    );
    let lock = fs::read_to_string(work.join("data.lock"))?;
    assert!(lock.contains("ref = \"local\""), "{lock}");
    assert!(lock.contains("resolved = \"local\""), "{lock}");
    assert_eq!(lock.matches("url = \"\"").count(), 2, "{lock}");
    let place = folder.display().to_string();
    assert!(!lock.contains(&place), "no path of this machine: {lock}");
    assert_eq!(
        fs::read(work.join("data/flowers/petals/red.csv"))?,
        b"length\n1.4\n"
    );
    assert_eq!(
        before,
        [
            look(&folder.join("sepals.csv"))?,
            look(&folder.join("petals/red.csv"))?,
        ],
        "the files imported stay as they were"
    );
    let told = sandbox.run(&["add", "../flowers"])?;
    let stderr = text(&told.stderr)?;
    assert_eq!(told.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("flowers is already in data.toml, its files are in data/flowers"),
        "the same files again change nothing: {stderr}"
    );
    assert_eq!(fs::read_to_string(work.join("data.lock"))?, lock);
    Ok(())
}

#[test]
fn an_import_tells_what_it_copied_and_that_the_folder_is_unchanged() -> Result {
    let sandbox = Sandbox::new()?;
    flowers(&sandbox)?;
    let output = sandbox.run(&["add", "../flowers"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    let line = stderr
        .lines()
        .find(|line| line.contains("Imported"))
        .unwrap_or_default();
    assert!(
        line.starts_with("   Imported flowers · 2 files · 32 B, ")
            && line.ends_with(" into the cache; ../flowers is unchanged"),
        "{stderr}"
    );
    assert!(
        stderr.contains("Added flowers, its files are in data/flowers"),
        "{stderr}"
    );
    Ok(())
}

/// copies the project in `work` to a folder of its own, as a teammate's clone of its repository
/// gets data.toml and data.lock.
fn clone_of(sandbox: &Sandbox) -> Result<std::path::PathBuf> {
    let clone = sandbox.folder("clone")?;
    for file in ["data.toml", "data.lock"] {
        fs::copy(sandbox.work().join(file), clone.join(file))?;
    }
    Ok(clone)
}

#[test]
fn a_clone_without_the_bytes_recovers_by_copying_them() -> Result {
    let sandbox = Sandbox::new()?;
    let folder = flowers(&sandbox)?;
    assert_eq!(sandbox.run(&["add", "../flowers"])?.status.code(), Some(0));
    let clone = clone_of(&sandbox)?;
    let elsewhere = sandbox.folder("another-cache")?;
    let sync = |clone: &Path| {
        sandbox
            .command_in(clone, &["--json", "sync"])
            .env("FETCHLOOM_CACHE", &elsewhere)
            .output()
    };
    let output = sync(&clone)?;
    assert_eq!(output.status.code(), Some(10), "{}", text(&output.stdout)?);
    let document = json(&output)?;
    let error = &document["errors"][0];
    assert_eq!(error["kind"], "reference.unresolved");
    assert_eq!(
        error["message"],
        "flowers was imported from files, and this machine's cache does not hold them"
    );
    assert_eq!(
        error["details"],
        serde_json::json!(["flowers/petals/red.csv", "flowers/sepals.csv"])
    );
    assert_eq!(
        error["help"],
        "copy them to data/flowers by hand, then run `fetchloom sync`, or run `fetchloom add <folder> --as flowers` where they are"
    );
    // running sync again, before the files are copied, would only fail again
    assert_eq!(error["fix"], serde_json::Value::Null);
    assert_eq!(document["work"]["requests"], 0);
    let data = clone.join("data/flowers");
    fs::create_dir_all(data.join("petals"))?;
    fs::copy(folder.join("sepals.csv"), data.join("sepals.csv"))?;
    fs::copy(folder.join("petals/red.csv"), data.join("petals/red.csv"))?;
    let adopted = sync(&clone)?;
    assert_eq!(adopted.status.code(), Some(0), "{}", text(&adopted.stdout)?);
    assert_eq!(
        fs::read(data.join("sepals.csv"))?,
        fs::read(folder.join("sepals.csv"))?
    );
    Ok(())
}

#[test]
fn supplying_the_same_folder_again_changes_no_pins() -> Result {
    let sandbox = Sandbox::new()?;
    let folder = flowers(&sandbox)?;
    assert_eq!(sandbox.run(&["add", "../flowers"])?.status.code(), Some(0));
    let clone = clone_of(&sandbox)?;
    let pins = [
        fs::read(clone.join("data.toml"))?,
        fs::read(clone.join("data.lock"))?,
    ];
    let elsewhere = sandbox.folder("another-cache")?;
    let supply = |folder: &str| {
        sandbox
            .command_in(&clone, &["--json", "add", folder, "--as", "flowers"])
            .env("FETCHLOOM_CACHE", &elsewhere)
            .output()
    };
    let unchanged = || -> Result<bool> {
        Ok(pins
            == [
                fs::read(clone.join("data.toml"))?,
                fs::read(clone.join("data.lock"))?,
            ])
    };

    let other = sandbox.folder("other")?;
    fs::create_dir_all(other.join("petals"))?;
    fs::write(other.join("sepals.csv"), b"length,width\n5.1,3.5\n")?;
    fs::write(other.join("petals/red.csv"), b"length\n9.9\n")?;
    let refused = supply("../other")?;
    assert_eq!(
        refused.status.code(),
        Some(50),
        "{}",
        text(&refused.stdout)?
    );
    let error = &json(&refused)?["errors"][0];
    assert_eq!(
        error["message"],
        "../other does not hold the files data.lock pins for flowers"
    );
    assert_eq!(
        error["details"],
        serde_json::json!(["petals/red.csv holds other bytes than data.lock pins"])
    );
    assert!(unchanged()?, "other bytes change nothing");
    assert!(!clone.join("data/flowers").exists());

    let supplied = supply(folder.to_str().ok_or("the sandbox path is not text")?)?;
    assert_eq!(
        supplied.status.code(),
        Some(0),
        "{}",
        text(&supplied.stdout)?
    );
    assert_eq!(
        json(&supplied)?["result"]["datasets"],
        serde_json::json!([])
    );
    assert!(unchanged()?, "the same bytes change no pin");
    assert_eq!(
        fs::read(clone.join("data/flowers/petals/red.csv"))?,
        b"length\n1.4\n"
    );
    Ok(())
}

/// a mac writes `.DS_Store` into a folder a person opens, and `._` files beside each file on a disk
/// not its own. they are no data: an import leaves them out and says so, and the folder supplied
/// later, after finder wrote more of them, still matches its pins.
#[test]
fn files_the_system_makes_are_left_out_of_imports_and_supplies() -> Result {
    let sandbox = Sandbox::new()?;
    let folder = flowers(&sandbox)?;
    fs::write(folder.join(".DS_Store"), b"finder")?;
    fs::write(folder.join("._sepals.csv"), b"appledouble")?;
    let output = sandbox.run(&["add", "../flowers"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("Skipped .DS_Store and 1 more the system makes in ../flowers"),
        "{stderr}"
    );
    let lock = fs::read_to_string(sandbox.work().join("data.lock"))?;
    assert!(
        !lock.contains("DS_Store") && !lock.contains("._sepals"),
        "{lock}"
    );
    let clone = clone_of(&sandbox)?;
    let pins = [
        fs::read(clone.join("data.toml"))?,
        fs::read(clone.join("data.lock"))?,
    ];
    fs::write(folder.join(".DS_Store"), b"finder, after another look")?;
    fs::write(folder.join("petals").join(".DS_Store"), b"finder")?;
    let elsewhere = sandbox.folder("another-cache")?;
    let supplied = sandbox
        .command_in(
            &clone,
            &[
                "--json",
                "add",
                folder.to_str().ok_or("the sandbox path is not text")?,
                "--as",
                "flowers",
            ],
        )
        .env("FETCHLOOM_CACHE", &elsewhere)
        .output()?;
    assert_eq!(
        supplied.status.code(),
        Some(0),
        "{}",
        text(&supplied.stdout)?
    );
    assert_eq!(
        pins,
        [
            fs::read(clone.join("data.toml"))?,
            fs::read(clone.join("data.lock"))?,
        ],
        "the same data changes no pin"
    );
    Ok(())
}

#[test]
fn an_import_is_never_resolved_again() -> Result {
    let sandbox = Sandbox::new()?;
    flowers(&sandbox)?;
    assert_eq!(sandbox.run(&["add", "../flowers"])?.status.code(), Some(0));
    let work = sandbox.work();
    let lock = fs::read(work.join("data.lock"))?;

    let one = sandbox.run(&["--json", "lock", "--upgrade", "flowers"])?;
    assert_eq!(one.status.code(), Some(10), "{}", text(&one.stdout)?);
    assert_eq!(
        json(&one)?["errors"][0]["message"],
        "flowers was imported from files, and no source has a newer version of them"
    );
    let all = sandbox.run(&["lock", "--upgrade"])?;
    let stderr = text(&all.stderr)?;
    assert_eq!(all.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains(
            "Skipped flowers, imported from files, which no source has a newer version of"
        ),
        "{stderr}"
    );
    assert_eq!(fs::read(work.join("data.lock"))?, lock);

    fs::write(
        work.join("data.toml"),
        "[datasets.flowers]\nref = \"local\"\nselect = [\"*.csv\"]\n",
    )?;
    let changed = sandbox.run(&["--json", "sync"])?;
    assert_eq!(
        changed.status.code(),
        Some(10),
        "{}",
        text(&changed.stdout)?
    );
    let document = json(&changed)?;
    assert_eq!(
        document["errors"][0]["message"],
        "flowers was imported from files, so a change to its entry in data.toml cannot pin other files"
    );
    assert_eq!(document["work"]["requests"], 0);
    assert_eq!(fs::read(work.join("data.lock"))?, lock);
    Ok(())
}

#[test]
fn a_lost_pin_of_an_import_is_pinned_again_from_its_files() -> Result {
    let sandbox = Sandbox::new()?;
    flowers(&sandbox)?;
    assert_eq!(sandbox.run(&["add", "../flowers"])?.status.code(), Some(0));
    let work = sandbox.work();
    let lock = fs::read(work.join("data.lock"))?;
    fs::remove_file(work.join("data.lock"))?;
    let lost = sandbox.run(&["--json", "sync"])?;
    assert_eq!(lost.status.code(), Some(10), "{}", text(&lost.stdout)?);
    assert_eq!(
        json(&lost)?["errors"][0]["message"],
        "flowers was imported from files, and data.lock holds no pin of them"
    );
    let pinned = sandbox.run(&["--json", "add", "../flowers", "--as", "flowers"])?;
    assert_eq!(pinned.status.code(), Some(0), "{}", text(&pinned.stdout)?);
    assert_eq!(
        json(&pinned)?["result"]["datasets"],
        serde_json::json!(["flowers"])
    );
    let again = fs::read_to_string(work.join("data.lock"))?;
    let files = |text: &str| -> Vec<String> {
        text.lines()
            .filter(|line| line.contains("path = "))
            .map(str::to_owned)
            .collect()
    };
    assert_eq!(files(&again), files(&String::from_utf8(lock)?));
    Ok(())
}

/// a zip of `pack/a.csv` and `pack/b.csv`.
fn pack() -> Result<Vec<u8>> {
    let mut archive = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (path, bytes) in [("pack/a.csv", b"a\n"), ("pack/b.csv", b"b\n")] {
        archive.start_file(path, zip::write::SimpleFileOptions::default())?;
        std::io::Write::write_all(&mut archive, bytes)?;
    }
    Ok(archive.finish()?.into_inner())
}

#[test]
fn an_imported_archive_unpacks_and_its_extract_switch_asks_no_source() -> Result {
    let sandbox = Sandbox::new()?;
    let shots = sandbox.folder("shots")?;
    let zip = pack()?;
    fs::write(shots.join("pack.zip"), &zip)?;
    assert_eq!(sandbox.run(&["add", "../shots"])?.status.code(), Some(0));
    let work = sandbox.work();
    assert_eq!(fs::read(work.join("data/shots/a.csv"))?, b"a\n");
    assert!(fs::read_to_string(work.join("data.lock"))?.contains("unpacks_to = "));
    fs::write(
        work.join("data.toml"),
        "[datasets.shots]\nref = \"local\"\nextract = false\n",
    )?;
    // the cache dropped the zip's own bytes once its files were in, so they are supplied again
    let packed = sandbox.run(&["--offline", "add", "../shots", "--as", "shots"])?;
    assert_eq!(packed.status.code(), Some(0), "{}", text(&packed.stderr)?);
    assert_eq!(fs::read(work.join("data/shots/pack.zip"))?, zip);
    let lock = fs::read_to_string(work.join("data.lock"))?;
    assert!(lock.contains("extract = false"), "{lock}");
    assert!(!lock.contains("unpacks_to"), "{lock}");
    fs::write(
        work.join("data.toml"),
        "[datasets.shots]\nref = \"local\"\n",
    )?;
    let unpacked = sandbox.run(&["--offline", "sync"])?;
    assert_eq!(
        unpacked.status.code(),
        Some(0),
        "{}",
        text(&unpacked.stderr)?
    );
    assert_eq!(fs::read(work.join("data/shots/b.csv"))?, b"b\n");
    Ok(())
}

#[test]
fn a_folder_that_holds_the_project_or_sits_in_its_data_folder_is_refused() -> Result {
    let sandbox = Sandbox::new()?;
    flowers(&sandbox)?;
    assert_eq!(sandbox.run(&["add", "../flowers"])?.status.code(), Some(0));
    let work = sandbox.work();
    let before = [
        fs::read(work.join("data.toml"))?,
        fs::read(work.join("data.lock"))?,
    ];
    for (path, message) in [
        (".", ". holds this project"),
        ("..", ".. holds this project"),
        (
            "./data/flowers",
            "./data/flowers is in the data folder, where fetchloom places what data.lock pins",
        ),
    ] {
        let output = sandbox.run(&["--json", "add", path, "--as", "copy"])?;
        assert_eq!(
            output.status.code(),
            Some(2),
            "{path}: {}",
            text(&output.stdout)?
        );
        assert_eq!(json(&output)?["errors"][0]["message"], message, "{path}");
    }
    assert_eq!(
        before,
        [
            fs::read(work.join("data.toml"))?,
            fs::read(work.join("data.lock"))?
        ]
    );
    Ok(())
}

/// a typo in the last name of a path is answered with the one name near it in that folder, as
/// the command typed with that name, its options kept and quoted for a shell, and nothing is
/// searched for.
#[test]
fn a_mistyped_path_suggests_the_near_name_and_asks_no_server() -> Result {
    let sandbox = Sandbox::new()?;
    let folder = sandbox.work().join("my flowers");
    fs::create_dir_all(&folder)?;
    fs::write(folder.join("sepals.csv"), b"length\n5.1\n")?;
    let output = sandbox.run(&["--json", "add", "./my flowrs", "--as", "sepals"])?;
    assert_eq!(output.status.code(), Some(50), "{}", text(&output.stdout)?);
    let document = json(&output)?;
    let error = &document["errors"][0];
    assert_eq!(
        error["message"],
        "could not find ./my flowrs: it does not exist"
    );
    assert_eq!(
        error["help"],
        "did you mean `fetchloom --json add \"./my flowers\" --as sepals`?"
    );
    assert_eq!(document["work"]["requests"], 0);
    assert!(!sandbox.work().join("data.toml").exists());
    Ok(())
}

#[test]
fn one_file_is_imported_under_its_own_name_and_a_dry_run_writes_nothing() -> Result {
    let sandbox = Sandbox::new()?;
    let folder = flowers(&sandbox)?;
    let work = sandbox.work();
    let typed = folder.join("sepals.csv").display().to_string();
    let dry = sandbox.run(&["--json", "add", &typed, "--dry-run"])?;
    assert_eq!(dry.status.code(), Some(0), "{}", text(&dry.stdout)?);
    let document = json(&dry)?;
    assert_eq!(
        document["result"]["datasets"],
        serde_json::json!(["sepals"])
    );
    assert_eq!(
        document["result"]["changes"]["place"],
        serde_json::json!(["data/sepals/sepals.csv"])
    );
    assert!(!work.join("data.toml").exists());
    assert!(
        !sandbox.cache().join("objects").exists()
            || fs::read_dir(sandbox.cache().join("objects"))?
                .next()
                .is_none()
    );
    let output = sandbox.run(&["add", &typed])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    assert_eq!(
        fs::read(work.join("data/sepals/sepals.csv"))?,
        fs::read(folder.join("sepals.csv"))?
    );
    Ok(())
}

/// files bigger than the free space on the cache's disk are told before any byte is copied, and
/// `--yes` goes on anyway, as on a disk that clones them.
#[test]
fn an_import_the_cache_disk_cannot_hold_asks_first() -> Result {
    let sandbox = Sandbox::new()?;
    flowers(&sandbox)?;
    let output = sandbox
        .command(&["--json", "add", "../flowers"])
        .env("FETCHLOOM_TEST_FREE", "10")
        .output()?;
    assert_eq!(output.status.code(), Some(1), "{}", text(&output.stdout)?);
    let document = json(&output)?;
    assert_eq!(document["errors"][0]["kind"], "unconfirmed");
    assert_eq!(
        document["errors"][0]["message"],
        format!(
            "importing ../flowers needs at least 32 B on the disk holding the cache at {}, which has 10 B free",
            sandbox.cache().display()
        )
    );
    assert_eq!(document["work"]["objects_written"], 0);
    assert!(!sandbox.work().join("data.toml").exists());
    let yes = sandbox
        .command(&["add", "--yes", "../flowers"])
        .env("FETCHLOOM_TEST_FREE", "10")
        .output()?;
    assert_eq!(yes.status.code(), Some(0), "{}", text(&yes.stderr)?);
    Ok(())
}

/// two files whose names differ only in how their unicode is written, as a mac and a linux
/// machine may write café, are refused as that, never as a file and a folder.
#[cfg(not(target_os = "macos"))]
#[test]
fn names_written_with_two_unicode_forms_are_refused_as_such() -> Result {
    let sandbox = Sandbox::new()?;
    let twins = sandbox.folder("twins")?;
    fs::write(twins.join("caf\u{e9}.csv"), b"1\n")?;
    fs::write(twins.join("cafe\u{301}.csv"), b"2\n")?;
    let output = sandbox.run(&["add", "../twins"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(10), "{stderr}");
    assert!(
        stderr.starts_with(
            "error: ../twins holds two files named \"caf\u{e9}.csv\", their unicode written two ways, which macos disks see as one name\n"
        ),
        "{stderr}"
    );
    assert!(!sandbox.work().join("data.toml").exists());
    Ok(())
}
