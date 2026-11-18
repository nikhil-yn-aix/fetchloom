//! `add <path>`: files on this machine pinned by their bytes, never by where they are, and a
//! clone that lacks those bytes getting them from a copy or from the folder they came from.

use std::fs;
use std::path::Path;

use crate::support::{Result, Sandbox, edit, json, look, skipped, text, warned, zip_of};

/// a folder of two files, one of them in a folder of its own.
fn flowers(sandbox: &Sandbox) -> Result<std::path::PathBuf> {
    let folder = sandbox.folder("flowers")?;
    fs::create_dir_all(folder.join("petals"))?;
    fs::write(folder.join("sepals.csv"), b"length,width\n5.1,3.5\n")?;
    fs::write(folder.join("petals/red.csv"), b"length\n1.4\n")?;
    Ok(folder)
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

/// an add that fails after the copy, as a zip cut short does, never says it imported the files:
/// the import line shows only once the add succeeds.
#[test]
fn a_failed_import_never_says_it_imported() -> Result {
    let sandbox = Sandbox::new()?;
    let zips = sandbox.folder("zips")?;
    fs::write(zips.join("case.zip"), b"PK\x03\x04cut short")?;
    let output = sandbox.run(&["add", "../zips/case.zip"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(10), "{stderr}");
    assert!(
        stderr.contains("error: the archive is cut short"),
        "{stderr}"
    );
    assert!(!stderr.contains("Imported"), "{stderr}");
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

/// copies data.toml and data.lock to a folder of their own, as a teammate's clone gets them.
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
        "a folder mirror that holds them brings them back, as `fetchloom sync flowers --mirror <folder>`; else copy them to data/flowers by hand, then run `fetchloom sync`, or run `fetchloom add <folder> --as flowers` where they are"
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

/// a clone lacking the files of several imports hears of every one in one error, once it placed
/// what this machine has.
#[test]
fn a_clone_lacking_several_imports_names_each_and_what_it_can_place() -> Result {
    let sandbox = Sandbox::new()?;
    let folder = flowers(&sandbox)?;
    let tiny = sandbox.folder("tiny")?;
    fs::write(tiny.join("t.csv"), b"t\n1\n")?;
    let wide = sandbox.folder("wide")?;
    fs::write(wide.join("w.csv"), b"w\n2\n")?;
    for path in ["../flowers", "../tiny", "../wide"] {
        let added = sandbox.run(&["add", path])?;
        assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    }
    let clone = clone_of(&sandbox)?;
    // flowers reaches the clone as a copy of its folder would bring it
    let data = clone.join("data/flowers");
    fs::create_dir_all(data.join("petals"))?;
    fs::copy(folder.join("sepals.csv"), data.join("sepals.csv"))?;
    fs::copy(folder.join("petals/red.csv"), data.join("petals/red.csv"))?;
    let elsewhere = sandbox.folder("another-cache")?;
    let sync = |args: &[&str]| {
        sandbox
            .command_in(&clone, args)
            .env("FETCHLOOM_CACHE", &elsewhere)
            .output()
    };
    let output = sync(&["--json", "sync"])?;
    assert_eq!(output.status.code(), Some(10), "{}", text(&output.stdout)?);
    let error = &json(&output)?["errors"][0];
    assert_eq!(
        error["message"],
        "tiny and wide were imported from files, and this machine's cache does not hold them"
    );
    // flowers is whole on this machine, so the sync places it before it stops for the others
    assert_eq!(
        error["details"],
        serde_json::json!([
            "tiny/t.csv",
            "wide/w.csv",
            "placed flowers, whose files are all on this machine",
            "tiny, wide wait for files this run could not get"
        ])
    );
    assert_eq!(
        error["help"],
        "copy each one's files to its folder by hand, then run `fetchloom sync`, or run `fetchloom add <folder> --as <name>` where they are"
    );
    assert_eq!(error["fix"], serde_json::Value::Null);
    let named = sync(&["sync", "flowers"])?;
    assert_eq!(named.status.code(), Some(0), "{}", text(&named.stderr)?);
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

/// a mac writes `.DS_Store` into a folder a person opens and `._` files beside each file on a disk
/// not its own; they are no data, so a supply after finder wrote more still matches the pins. a
/// link is left out too, with a warning, as fetchloom never follows one.
#[test]
fn files_the_system_makes_are_left_out_of_imports_and_supplies() -> Result {
    let sandbox = Sandbox::new()?;
    let folder = flowers(&sandbox)?;
    fs::write(folder.join(".DS_Store"), b"finder")?;
    fs::write(folder.join("._sepals.csv"), b"appledouble")?;
    #[cfg(unix)]
    std::os::unix::fs::symlink("sepals.csv", folder.join("linked.csv"))?;
    let output = sandbox.run(&["add", "../flowers"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("Skipped .DS_Store and 1 more the system makes in ../flowers"),
        "{stderr}"
    );
    #[cfg(unix)]
    assert!(
        stderr.contains("left out linked.csv in ../flowers, a link or a special file, which fetchloom never follows or imports"),
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
    assert_eq!(one.status.code(), Some(2), "{}", text(&one.stdout)?);
    let error = &json(&one)?["errors"][0];
    assert_eq!(
        error["message"],
        "flowers was imported from files, and no source has a newer version of them"
    );
    assert_eq!(error["kind"], "usage");
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

/// every import's table reads `ref = "local"`, so one that goes while another comes in the same
/// edit is no rename: the new one is refused as written by hand, and nothing moves.
#[test]
fn an_import_is_never_carried_to_another_name() -> Result {
    let sandbox = Sandbox::new()?;
    flowers(&sandbox)?;
    assert_eq!(
        sandbox
            .run(&["add", "../flowers", "--as", "alpha"])?
            .status
            .code(),
        Some(0)
    );
    let work = sandbox.work();
    let lock = fs::read(work.join("data.lock"))?;
    let manifest = fs::read_to_string(work.join("data.toml"))?;
    fs::write(
        work.join("data.toml"),
        manifest.replace("[datasets.alpha]", "[datasets.gamma]"),
    )?;
    let output = sandbox.run(&["--json", "sync"])?;
    assert_eq!(output.status.code(), Some(10), "{}", text(&output.stderr)?);
    let error = &json(&output)?["errors"][0];
    assert_eq!(
        error["message"],
        "gamma was imported from files, and data.lock holds no pin of them"
    );
    assert_eq!(
        error["help"],
        "give it back its old name if you renamed it in data.toml, restore data.lock from git, or import the files again with `fetchloom add <folder> --as gamma` where they are"
    );
    assert_eq!(fs::read(work.join("data.lock"))?, lock);
    assert!(!work.join("data/gamma").exists());
    // importing the files again under the new name, as the help says, pins them there
    let again = sandbox.run(&["add", "../flowers", "--as", "gamma"])?;
    assert_eq!(again.status.code(), Some(0), "{}", text(&again.stderr)?);
    assert!(work.join("data/gamma/sepals.csv").is_file());
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

fn pack() -> Result<Vec<u8>> {
    zip_of(&[("pack/a.csv", b"a\n"), ("pack/b.csv", b"b\n")])
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

/// a folder that holds the project, or another dataset's files, is no folder to add: neither an
/// import nor an output's.
#[test]
fn a_folder_that_holds_the_project_or_a_datasets_files_is_refused() -> Result {
    let sandbox = Sandbox::new()?;
    flowers(&sandbox)?;
    assert_eq!(sandbox.run(&["add", "../flowers"])?.status.code(), Some(0));
    let work = sandbox.work();
    let before = [
        fs::read(work.join("data.toml"))?,
        fs::read(work.join("data.lock"))?,
    ];
    for (path, message) in [
        (
            ".",
            ". holds the whole project, so it cannot be an output's folder",
        ),
        ("..", ".. holds this project"),
        (
            "./data/flowers",
            "./data/flowers holds data/flowers/petals/red.csv, a file of flowers, so it cannot be an output's folder",
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

/// data a project keeps in data/ already, as cookiecutter's data/raw, is pinned where it is, as
/// an output: its files stay, writable, and a teammate gets them at data/raw. its bare name may
/// be search words too, so without --as it is refused with the line that adds it.
#[test]
fn data_already_in_the_data_folder_is_pinned_where_it_is() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    fs::create_dir_all(work.join("data/raw"))?;
    fs::write(work.join("data/raw/visits.csv"), b"id\n1\n")?;
    let output = sandbox.run(&["--json", "add", "data/raw"])?;
    assert_eq!(output.status.code(), Some(2), "{}", text(&output.stdout)?);
    let error = &json(&output)?["errors"][0];
    assert_eq!(
        error["message"],
        "\"data/raw\" is a folder here, so fetchloom did not search for it"
    );
    assert_eq!(
        error["help"],
        "run `fetchloom add ./data/raw` to pin it, or `fetchloom search data/raw` to search"
    );
    assert!(!work.join("data.toml").exists());
    let added = sandbox.run(&["add", "./data/raw"])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    assert_eq!(
        fs::read_to_string(work.join("data.toml"))?,
        "[datasets.raw]\nref = \"local\"\nat = \"data/raw\"\n"
    );
    assert_eq!(fs::read(work.join("data/raw/visits.csv"))?, b"id\n1\n");
    assert!(
        !work.join("data/raw/raw").exists(),
        "never nested in itself"
    );
    Ok(())
}

/// a path in the data folder that a dataset holds already is that dataset's, so nothing is added
/// twice, nor made an output's folder, and a name data.toml has already is never promised.
#[test]
fn a_path_a_dataset_holds_already_is_not_added_again() -> Result {
    let sandbox = Sandbox::new()?;
    flowers(&sandbox)?;
    assert_eq!(sandbox.run(&["add", "../flowers"])?.status.code(), Some(0));
    let work = sandbox.work();
    let inside = work.join("data").join("flowers");
    let refused = |here: &Path, typed: &str| -> Result<serde_json::Value> {
        let output = sandbox
            .command_in(here, &["--json", "add", typed])
            .output()?;
        assert_eq!(
            output.status.code(),
            Some(2),
            "{typed}: {}",
            text(&output.stdout)?
        );
        Ok(json(&output)?["errors"][0]["help"].clone())
    };
    for typed in ["./sepals.csv", "petals/red.csv"] {
        assert_eq!(
            refused(&inside, typed)?,
            "it is part of flowers already, so there is nothing to add; `fetchloom path flowers` prints where flowers is",
            "{typed}"
        );
    }
    assert_eq!(
        refused(&inside, ".")?,
        "pin a folder that holds only what your pipeline writes, such as data/processed"
    );
    fs::write(inside.join("flowers.csv"), b"mine\n")?;
    assert_eq!(
        refused(&inside, "./flowers.csv")?,
        "move it out first, with `mv ./flowers.csv ../../flowers.csv`, then run `fetchloom add ../../flowers.csv --as flowers-2` to place its files in ../../data/flowers-2, or keep data/ as it is by setting data_dir under [project] in data.toml to another folder"
    );
    Ok(())
}

/// raw data imported from a file inside the git work tree, where `git add .` takes it, gets a
/// warning with the commands that keep it out, and none once .gitignore leaves it out. a folder
/// there is pinned where it is, so it has no copy for git to keep twice.
#[test]
fn an_import_from_inside_the_work_tree_says_git_takes_it() -> Result {
    let sandbox = Sandbox::new()?;
    if crate::merge::ok(&sandbox, &["init", "-q"])?.is_none() {
        return Ok(());
    }
    let work = sandbox.work();
    fs::write(work.join("sepals.csv"), b"length\n5.1\n")?;
    fs::write(work.join("petals.csv"), b"length\n1.4\n")?;
    crate::merge::ok(&sandbox, &["add", "petals.csv"])?;
    fs::create_dir_all(work.join("flowers"))?;
    fs::write(work.join("flowers/red.csv"), b"length\n1.6\n")?;
    let run = |args: &[&str]| -> Result<serde_json::Value> {
        let mut command = sandbox.command(args);
        for name in crate::merge::git_variables() {
            command.env_remove(name);
        }
        let output = command.output()?;
        assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
        json(&output)
    };
    let untracked = run(&["--json", "add", "./sepals.csv"])?;
    assert_eq!(
        crate::support::kinds(&untracked),
        ["git.import_in_tree"],
        "{untracked}"
    );
    assert_eq!(
        warned(&untracked),
        [
            "./sepals.csv is still in the project, and git will take it; delete it, or add /sepals.csv to .gitignore"
        ]
    );
    let tracked = run(&["--json", "add", "./petals.csv"])?;
    assert_eq!(
        warned(&tracked),
        [
            "git tracks ./petals.csv, so a commit keeps its files in the repository; run `git rm -r --cached ./petals.csv`, then delete it or add /petals.csv to .gitignore"
        ]
    );
    let output = run(&["--json", "add", "./flowers"])?;
    assert_eq!(warned(&output), Vec::<String>::new(), "{output}");
    fs::write(work.join(".gitignore"), "/sepals.csv\n")?;
    let ignored = run(&["--json", "add", "./sepals.csv"])?;
    assert_eq!(warned(&ignored), Vec::<String>::new());
    Ok(())
}

/// a folder imported again with other files, under the name its import has, is refused with both
/// ways on: a number would leave data/flowers-2 beside the data/flowers scripts read.
#[test]
fn a_changed_folder_imported_again_keeps_its_name_and_says_how_to_go_on() -> Result {
    let sandbox = Sandbox::new()?;
    let folder = flowers(&sandbox)?;
    let added = sandbox.run(&["add", "../flowers"])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let work = sandbox.work();
    let before = [
        fs::read(work.join("data.toml"))?,
        fs::read(work.join("data.lock"))?,
    ];
    let help = |keep: &str| {
        format!(
            "to pin these instead, run `fetchloom remove flowers`, then `fetchloom add ../flowers`; to keep both, run `{keep}`"
        )
    };
    fs::write(folder.join("petals/white.csv"), b"length\n1.6\n")?;
    for (args, keep) in [
        (
            &["--json", "add", "../flowers"][..],
            "fetchloom --json add ../flowers --as flowers-2",
        ),
        (
            &["--json", "add", "../flowers", "--dry-run"],
            "fetchloom --json add ../flowers --as flowers-2 --dry-run",
        ),
    ] {
        let output = sandbox.run(args)?;
        assert_eq!(output.status.code(), Some(2), "{}", text(&output.stdout)?);
        let error = &json(&output)?["errors"][0];
        assert_eq!(
            error["message"],
            "flowers pins other files than ../flowers holds"
        );
        assert_eq!(
            error["details"],
            serde_json::json!(["petals/white.csv is not in data.lock"])
        );
        assert_eq!(error["help"], help(keep));
        assert_eq!(error["fix"], serde_json::Value::Null);
    }
    // the same paths and sizes holding other bytes are refused once they are read
    fs::remove_file(folder.join("petals/white.csv"))?;
    fs::write(folder.join("petals/red.csv"), b"length\n9.9\n")?;
    let output = sandbox.run(&["--json", "add", "../flowers"])?;
    assert_eq!(output.status.code(), Some(2), "{}", text(&output.stdout)?);
    let error = &json(&output)?["errors"][0];
    assert_eq!(
        error["details"],
        serde_json::json!(["petals/red.csv holds other bytes than data.lock pins"])
    );
    assert_eq!(
        error["help"],
        help("fetchloom --json add ../flowers --as flowers-2")
    );
    assert_eq!(
        before,
        [
            fs::read(work.join("data.toml"))?,
            fs::read(work.join("data.lock"))?
        ]
    );
    assert!(!work.join("data/flowers-2").exists());
    Ok(())
}

/// the help retypes the command with the near name from the same folder, quoted for a shell.
#[test]
fn a_mistyped_path_suggests_the_near_name_and_asks_no_server() -> Result {
    let sandbox = Sandbox::new()?;
    let folder = sandbox.work().join("my flowers");
    fs::create_dir_all(&folder)?;
    fs::write(folder.join("sepals.csv"), b"length\n5.1\n")?;
    let output = sandbox.run(&["--json", "add", "./my flowrs", "--as", "sepals"])?;
    assert_eq!(output.status.code(), Some(2), "{}", text(&output.stdout)?);
    let document = json(&output)?;
    let error = &document["errors"][0];
    assert_eq!(error["kind"], "usage");
    assert_eq!(
        error["message"],
        "there is no file or folder \"./my flowrs\""
    );
    assert_eq!(
        error["help"],
        "did you mean `fetchloom --json add \"./my flowers\" --as sepals`?"
    );
    assert_eq!(document["work"]["requests"], 0);
    assert!(!sandbox.work().join("data.toml").exists());
    // with no near name, the help says where a relative path is read from
    let output = sandbox.run(&["--json", "add", "./stuff"])?;
    assert_eq!(output.status.code(), Some(2), "{}", text(&output.stdout)?);
    assert_eq!(
        json(&output)?["errors"][0]["help"],
        format!(
            "check the path: fetchloom reads ./stuff from {}",
            sandbox.work().display()
        )
    );
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

/// `--yes` goes on anyway, as a disk that clones the files may hold them.
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

/// a file at the folder's top and one below it are both named by their path in it.
#[cfg(unix)]
#[test]
fn a_name_that_is_not_text_is_refused_by_its_path_in_the_folder() -> Result {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt as _;

    use crate::support::skipped;

    let sandbox = Sandbox::new()?;
    let notes = sandbox.folder("notes")?;
    fs::create_dir(notes.join("inner"))?;
    let name = OsString::from_vec(b"notes-\xff.csv".to_vec());
    // apfs refuses a name that is not UTF-8 and hfs+ stores it escaped, so on either no such
    // name reaches fetchloom
    match fs::write(notes.join(&name), b"1\n") {
        Err(err) if err.raw_os_error() == Some(rustix::io::Errno::ILSEQ.raw_os_error()) => {
            return skipped(
                "raw-names",
                "this filesystem refuses names that are not text",
            );
        }
        made => made?,
    }
    if !fs::read_dir(&notes)?.any(|entry| entry.is_ok_and(|entry| entry.file_name() == name)) {
        return skipped(
            "raw-names",
            "this filesystem rewrites names that are not text",
        );
    }
    fs::write(notes.join("inner").join(&name), b"1\n")?;
    let refusal = |path: &str| {
        format!(
            "error: ../notes holds \"{path}\", which fetchloom refuses because it is not text every machine can read\n"
        )
    };
    let top = sandbox.run(&["add", "../notes"])?;
    assert_eq!(top.status.code(), Some(10));
    assert!(
        text(&top.stderr)?.starts_with(&refusal("notes-\u{fffd}.csv")),
        "{}",
        text(&top.stderr)?
    );
    fs::remove_file(notes.join(&name))?;
    let below = sandbox.run(&["add", "../notes"])?;
    assert_eq!(below.status.code(), Some(10));
    assert!(
        text(&below.stderr)?.starts_with(&refusal("inner/notes-\u{fffd}.csv")),
        "{}",
        text(&below.stderr)?
    );
    Ok(())
}

/// three counts are checked: files past the most a run lists, names windows cannot store, and a
/// list in data.lock.d past the most.
#[cfg(unix)]
#[test]
fn a_large_import_is_counted_with_its_thousands_apart() -> Result {
    let sandbox = Sandbox::new()?;
    let many = sandbox.folder("many")?;
    for index in 0..1_001 {
        fs::write(many.join(format!("what?{index}.csv")), b"x\n")?;
    }
    let most = |args: &[&str]| {
        sandbox
            .command(args)
            .env("FETCHLOOM_MOST_FILES", "1000")
            .output()
    };
    let over = most(&["add", "../many"])?;
    assert!(
        text(&over.stderr)?.starts_with(
            "error: ../many lists more than 1,000 files, the most this run lists for one dataset\n"
        ),
        "{}",
        text(&over.stderr)?
    );
    let added = sandbox
        .command(&["add", "../many"])
        .env("FETCHLOOM_TEST_INLINE", "2")
        .output()?;
    let stderr = text(&added.stderr)?;
    assert_eq!(added.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains(
            "warning: many has 1,001 files whose names windows and fat disks cannot store: "
        ),
        "{stderr}"
    );
    let listed = most(&["status"])?;
    assert!(
        text(&listed.stderr)?.starts_with(
            "error: data.lock lists 1,001 files in many, more than the 1,000 this run lists for one dataset\n"
        ),
        "{}",
        text(&listed.stderr)?
    );
    Ok(())
}

/// an add that fails once it wrote data.lock and its file list, here as placing finds the data
/// folder read-only, takes the list back with data.toml and data.lock: the folder is as it was.
#[cfg(unix)]
#[test]
fn a_failed_first_add_takes_back_the_file_lists_it_wrote() -> Result {
    use std::os::unix::fs::PermissionsExt as _;
    if rustix::process::geteuid().is_root() {
        return crate::support::skipped("unprivileged", "root ignores file permissions");
    }
    let sandbox = Sandbox::new()?;
    flowers(&sandbox)?;
    let work = sandbox.work();
    let data = work.join("data");
    fs::create_dir(&data)?;
    fs::set_permissions(&data, fs::Permissions::from_mode(0o555))?;
    let output = sandbox
        .command(&["add", "../flowers"])
        .env("FETCHLOOM_TEST_INLINE", "1")
        .output()?;
    fs::set_permissions(&data, fs::Permissions::from_mode(0o755))?;
    let stderr = text(&output.stderr)?;
    assert_ne!(output.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("restored data.toml and data.lock to their state before add"),
        "{stderr}"
    );
    for name in ["data.toml", "data.lock", "data.lock.d"] {
        assert!(!work.join(name).exists(), "{name}: {stderr}");
    }
    Ok(())
}

/// a mac and a linux machine may write café two ways, and the refusal says so rather than call
/// them a file and a folder.
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

/// an import kept packed whose data.toml now asks to unpack it: a dry run names the archive it
/// would open, from the cache's copy, which no link can bring again, and changes nothing.
#[test]
fn a_dry_run_of_a_switched_import_names_the_archive_it_would_unpack() -> Result {
    let sandbox = Sandbox::new()?;
    let folder = sandbox.folder("shards")?;
    fs::write(folder.join("part.zip"), zip_of(&[("a.csv", b"id\n1\n")])?)?;
    crate::support::ok(&sandbox.run(&["add", "../shards", "--no-extract"])?)?;
    let manifest = sandbox.work().join("data.toml");
    let written = fs::read_to_string(&manifest)?;
    fs::write(
        &manifest,
        written.replace("extract = false", "extract = true"),
    )?;
    for args in [&["lock", "--dry-run"][..], &["sync", "--dry-run"]] {
        let output = sandbox.run(args)?;
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(0), "{args:?}: {stderr}");
        assert!(
            stderr.contains("      Would open shards/part.zip\n"),
            "{args:?}: {stderr}"
        );
    }
    assert!(sandbox.work().join("data/shards/part.zip").is_file());
    Ok(())
}

/// what status says of an import's files this machine cannot give back, missing or damaged, as
/// sync's refusal says it, with the ways that bring them: never that a sync places them.
const NO_SYNC_BRINGS: &str = "flowers was imported from files, and this machine's cache does not hold them: a folder mirror that holds them brings them back, as `fetchloom sync flowers --mirror <folder>`; else copy them to data/flowers by hand, then run `fetchloom sync`, or run `fetchloom add <folder> --as flowers` where they are";

/// status in a clone whose cache lacks an import's files lists them missing, but never says a
/// sync places them, since no link fetches them: it says how to bring them, as sync does, and
/// `--check` says it is what makes the data ready.
#[test]
fn status_says_how_to_bring_an_import_no_sync_can_get() -> Result {
    let sandbox = Sandbox::new()?;
    flowers(&sandbox)?;
    assert_eq!(sandbox.run(&["add", "../flowers"])?.status.code(), Some(0));
    let clone = clone_of(&sandbox)?;
    let elsewhere = sandbox.folder("another-cache")?;
    let status = sandbox
        .command_in(&clone, &["status"])
        .env("FETCHLOOM_CACHE", &elsewhere)
        .output()?;
    let (stdout, stderr) = (text(&status.stdout)?, text(&status.stderr)?);
    assert!(stdout.contains("Missing"), "{stdout}");
    assert!(!stderr.contains("`fetchloom sync` places"), "{stderr}");
    assert!(
        stderr.contains(&format!("hint: {NO_SYNC_BRINGS}\n")),
        "{stderr}"
    );
    let checked = sandbox
        .command_in(&clone, &["status", "--check"])
        .env("FETCHLOOM_CACHE", &elsewhere)
        .output()?;
    let stderr = text(&checked.stderr)?;
    assert_eq!(checked.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains(&format!("  {NO_SYNC_BRINGS}\n")),
        "{stderr}"
    );
    assert!(!stderr.contains("places it"), "{stderr}");
    Ok(())
}

/// a file of an import edited through its link takes the cache's copy with it, so no sync puts
/// it back: status says so as sync does, never that a sync puts it back.
#[test]
fn status_says_no_sync_puts_back_a_damaged_import() -> Result {
    let sandbox = Sandbox::new()?;
    if !sandbox.links()?.hardlinks_read_only() {
        // an edit of a clone or a copy leaves the cache's copy, which a sync puts back
        return skipped(
            "placed-hardlinks",
            "this disk places no read-only file as a hardlink",
        );
    }
    flowers(&sandbox)?;
    crate::support::ok(&sandbox.run(&["add", "../flowers"])?)?;
    // as long as the pinned bytes, so only the link tells the cache's copy changed
    edit(
        &sandbox.work().join("data/flowers/sepals.csv"),
        b"length,width\n9.9,9.9\n",
    )?;
    let checked = sandbox.run(&["status", "--check"])?;
    let stderr = text(&checked.stderr)?;
    assert_eq!(checked.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains(&format!("  {NO_SYNC_BRINGS}\n")),
        "{stderr}"
    );
    assert!(!stderr.contains("puts them back"), "{stderr}");
    assert!(!stderr.contains("`fetchloom sync` places"), "{stderr}");
    let synced = sandbox.run(&["sync"])?;
    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(10), "{stderr}");
    assert!(
        stderr.contains("help: a folder mirror that holds them brings them back"),
        "{stderr}"
    );
    Ok(())
}

/// an import data.toml names that data.lock holds no pin of, as a stop between the two writes of
/// an add leaves one, and an import whose entry changed, are told as what they are, with the steps
/// that pin them: status never sends a person to a sync, which refuses both.
#[test]
fn status_says_what_pins_an_import_no_sync_can_pin() -> Result {
    let sandbox = Sandbox::new()?;
    flowers(&sandbox)?;
    crate::support::ok(&sandbox.run(&["add", "../flowers", "--as", "alpha"])?)?;
    let work = sandbox.work();
    let manifest = work.join("data.toml");
    let written = fs::read_to_string(&manifest)?;
    fs::write(
        &manifest,
        format!("{written}\n[datasets.big]\nref = \"local\"\n"),
    )?;
    // a placed file is read-only, which windows refuses to delete as it is
    crate::support::delete(&work.join("data/alpha/sepals.csv"))?;
    let refused = sandbox.run(&["--json", "sync"])?;
    assert_eq!(
        refused.status.code(),
        Some(10),
        "{}",
        text(&refused.stderr)?
    );
    let steps = "give it back its old name if you renamed it in data.toml, restore data.lock from git, or import the files again with `fetchloom add <folder> --as big` where they are";
    assert_eq!(json(&refused)?["errors"][0]["help"], steps);
    let status = sandbox.run(&["status"])?;
    let (stdout, stderr) = (text(&status.stdout)?, text(&status.stderr)?);
    assert!(
        stdout.contains(
            "   Unlocked big · imported from files, and data.lock holds no pin of them\n"
        ),
        "{stdout}"
    );
    assert!(stderr.contains(&format!("hint: {steps}\n")), "{stderr}");
    // a sync works once big is pinned, and not before
    assert!(
        stderr.contains("hint: then `fetchloom sync` places missing or updated files"),
        "{stderr}"
    );
    let bare = sandbox.run(&[])?;
    assert!(
        text(&bare.stderr)?.contains("`fetchloom status` lists what differs"),
        "{}",
        text(&bare.stderr)?
    );
    let check = sandbox.run(&["--json", "status", "--check"])?;
    assert_eq!(check.status.code(), Some(1), "{}", text(&check.stderr)?);
    let error = &json(&check)?["errors"][0];
    assert_eq!(
        error["details"][0],
        format!("big was imported from files, and data.lock holds no pin of them: {steps}")
    );
    assert_eq!(error["help"], "each line says what makes it ready");
    // what the steps say pins it, after which a sync puts back the rest
    crate::support::ok(&sandbox.run(&["add", "../flowers", "--as", "big"])?)?;
    crate::support::ok(&sandbox.run(&["sync"])?)?;
    crate::support::ok(&sandbox.run(&["status", "--check"])?)?;
    let pinned = fs::read_to_string(&manifest)?;
    fs::write(
        &manifest,
        pinned.replace(
            "[datasets.alpha]\nref = \"local\"\n",
            "[datasets.alpha]\nref = \"local\"\nselect = [\"*.csv\"]\n",
        ),
    )?;
    let status = sandbox.run(&["status"])?;
    let (stdout, stderr) = (text(&status.stdout)?, text(&status.stderr)?);
    assert!(
        stdout.contains("   Unlocked alpha · imported from files, so a change to its entry in data.toml cannot pin other files\n"),
        "{stdout}"
    );
    assert!(
        stderr.contains("hint: put datasets.alpha back as it was, or run `fetchloom remove alpha`, then `fetchloom add <folder> --as alpha` where the files are\n"),
        "{stderr}"
    );
    assert!(!stderr.contains("`fetchloom sync`"), "{stderr}");
    fs::write(&manifest, pinned)?;
    crate::support::ok(&sandbox.run(&["status", "--check"])?)?;
    // with no data.lock, nothing pins an import but importing its files again
    fs::remove_file(work.join("data.lock"))?;
    let status = sandbox.run(&["status"])?;
    let stderr = text(&status.stderr)?;
    assert!(!stderr.contains("`fetchloom lock`"), "{stderr}");
    assert!(
        stderr.contains(
            "import each one's files again with `fetchloom add <folder> --as <name>` where they are"
        ),
        "{stderr}"
    );
    Ok(())
}

/// while an import no sync pins blocks a plain sync, no line of status names a sync that is
/// refused: what fetchloom placed for a dataset data.lock does not pin is no extra file, a row a
/// sync acts on says so without a command, the hints give the order, and `--check` names the
/// sync of each line's own dataset, which works then.
#[test]
fn an_unpinned_import_leaves_no_line_naming_a_refused_sync() -> Result {
    let sandbox = Sandbox::new()?;
    flowers(&sandbox)?;
    let routes = crate::support::Routes::start(vec![
        (
            "/iris.csv".to_owned(),
            crate::support::file(b"a\n1\n", "\"i\""),
        ),
        (
            "/gone.csv".to_owned(),
            crate::support::file(b"b\n2\n", "\"g\""),
        ),
    ])?;
    sandbox.allow(&routes.url);
    for name in ["iris", "gone"] {
        crate::support::ok(&sandbox.run(&["add", &format!("{}/{name}.csv", routes.url)])?)?;
    }
    let work = sandbox.work();
    let pinned_two = fs::read_to_string(work.join("data.lock"))?;
    crate::support::ok(&sandbox.run(&["add", "../flowers", "--as", "src"])?)?;
    fs::write(work.join("data.lock"), &pinned_two)?;
    let manifest = fs::read_to_string(work.join("data.toml"))?;
    let gone = format!("[datasets.gone]\nref = \"{}/gone.csv\"\n\n", routes.url);
    assert!(manifest.contains(&gone), "{manifest}");
    fs::write(work.join("data.toml"), manifest.replace(&gone, ""))?;
    crate::support::delete(&work.join("data/iris/iris.csv"))?;
    let status = sandbox.run(&["status"])?;
    let shown = format!("{}{}", text(&status.stdout)?, text(&status.stderr)?);
    for row in [
        "    Removed gone · not in data.toml, so a sync takes it out of data.lock\n",
        "      Extra data/gone/gone.csv · fetchloom placed it and no dataset needs it now, so a sync removes it\n",
    ] {
        assert!(shown.contains(row), "{shown}");
    }
    assert!(!shown.contains("data/src/"), "{shown}");
    let syncs: Vec<&str> = shown
        .lines()
        .filter(|line| line.contains("`fetchloom sync"))
        .collect();
    assert_eq!(
        syncs,
        [
            "  hint: then `fetchloom sync` places missing or updated files, removes those no dataset needs now and reports any conflicts it keeps"
        ]
    );
    let check = sandbox.run(&["--json", "status", "--check"])?;
    assert_eq!(check.status.code(), Some(1), "{}", text(&check.stderr)?);
    let details = json(&check)?["errors"][0]["details"].clone();
    let missing = "data/iris/iris.csv is missing: `fetchloom sync iris` places it";
    assert!(
        details
            .as_array()
            .is_some_and(|lines| lines.iter().any(|line| line == missing)),
        "{details}"
    );
    crate::support::ok(&sandbox.run(&["sync", "iris"])?)?;
    assert_eq!(fs::read(work.join("data/iris/iris.csv"))?, b"a\n1\n");
    // the hints' order: the import pinned again, then a sync settles the rest
    crate::support::ok(&sandbox.run(&["add", "../flowers", "--as", "src"])?)?;
    crate::support::ok(&sandbox.run(&["sync"])?)?;
    crate::support::ok(&sandbox.run(&["status", "--check"])?)?;
    assert!(!work.join("data/gone").exists());
    // with no data.lock, what fetchloom placed waits for its dataset's pin, not for removal
    fs::remove_file(work.join("data.lock"))?;
    let lockless = json(&sandbox.run(&["--json", "status"])?)?;
    assert_eq!(
        lockless["result"]["unneeded"],
        serde_json::json!([]),
        "{lockless}"
    );
    assert_eq!(
        lockless["result"]["extra"],
        serde_json::json!([]),
        "{lockless}"
    );
    Ok(())
}
