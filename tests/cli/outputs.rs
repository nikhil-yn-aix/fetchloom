//! outputs: a folder of the project pinned where its pipeline writes it, shared by hash, placed
//! writable for a teammate, and never partly healed once it changed.

use std::fs;
use std::path::{Path, PathBuf};

use crate::project::{IRIS, PATH};
use crate::support::{
    Result, Routes, Running, Sandbox, delete_folder, file, json, link_folder, look, ok, skipped,
    text, wait_until, warned, warning,
};

const CLEAN: &[u8] = b"sepal,petal\n5.1,1.4\n";
const SUMMARY: &[u8] = b"150 rows\n";

/// the folder a pipeline writes, as cookiecutter's dataset.py writes data/processed.
fn processed(project: &Path) -> Result {
    fs::create_dir_all(project.join("data/processed"))?;
    fs::write(project.join("data/processed/iris_clean.csv"), CLEAN)?;
    fs::write(project.join("data/processed/summary.txt"), SUMMARY)?;
    Ok(())
}

/// a rerun of the pipeline: one file rewritten, one gone, one new.
fn rerun(project: &Path) -> Result {
    let folder = project.join("data/processed");
    fs::write(
        folder.join("iris_clean.csv"),
        b"sepal,petal\n5.1,1.4\n4.9,1.4\n",
    )?;
    fs::remove_file(folder.join("summary.txt"))?;
    fs::write(folder.join("plot.png"), b"png")?;
    Ok(())
}

/// the paths of one kind status lists, as `--json` gives them.
fn listed(document: &serde_json::Value, kind: &str) -> Vec<String> {
    document["result"][kind]
        .as_array()
        .map(|paths| {
            paths
                .iter()
                .filter_map(|path| path.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// the pipeline's folder is pinned where it is: data.toml names it as the output's `at`, and its
/// files stay the author's, untouched and writable, with nothing left to place or hash.
#[test]
fn an_output_is_pinned_where_its_pipeline_wrote_it() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    processed(&work)?;
    let files = [
        work.join("data/processed/iris_clean.csv"),
        work.join("data/processed/summary.txt"),
    ];
    let before = [look(&files[0])?, look(&files[1])?];
    let output = sandbox.run(&["add", "data/processed", "--as", "processed"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert_eq!(
        fs::read_to_string(work.join("data.toml"))?,
        "[datasets.processed]\nref = \"local\"\nat = \"data/processed\"\n"
    );
    let lock = fs::read_to_string(work.join("data.lock"))?;
    assert!(lock.contains("\nat = \"data/processed\"\n"), "{lock}");
    assert_eq!(lock.matches("{ path = ").count(), 2, "{lock}");
    assert!(
        stderr.contains("Added processed, its files are in data/processed"),
        "{stderr}"
    );
    // read once while pinning, so placing has nothing to read or adopt
    assert!(!stderr.contains("Adopted"), "{stderr}");
    assert_eq!(before, [look(&files[0])?, look(&files[1])?]);
    fs::OpenOptions::new().append(true).open(&files[0])?;
    let status = sandbox.run(&["--json", "status"])?;
    let document = json(&status)?;
    for kind in ["missing", "edited", "untracked", "extra", "outdated"] {
        assert_eq!(
            listed(&document, kind),
            Vec::<String>::new(),
            "{kind}: {document}"
        );
    }
    Ok(())
}

/// a teammate's sync places the output where the author's pipeline wrote it, outside the data
/// folder too, as files of their own a pipeline may write over, never links to the cache's copy.
#[test]
fn a_teammate_gets_an_output_where_the_author_had_it_and_may_write_it() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    fs::write(
        work.join("data.toml"),
        "[project]\ndata_dir = \"data/raw\"\n",
    )?;
    processed(&work)?;
    ok(&sandbox.run(&["add", "data/processed", "--as", "processed"])?)?;
    let teammate = sandbox.folder("teammate")?;
    for file in ["data.toml", "data.lock"] {
        fs::copy(work.join(file), teammate.join(file))?;
    }
    let synced = sandbox.command_in(&teammate, &["sync"]).output()?;
    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(0), "{stderr}");
    let placed = teammate.join("data/processed/iris_clean.csv");
    assert!(
        placed.is_file(),
        "where the author's code reads it: {stderr}"
    );
    assert!(!teammate.join("data/raw/processed").exists(), "{stderr}");
    assert_eq!(fs::read(&placed)?, CLEAN);
    assert_eq!(
        fs::read(teammate.join("data/processed/summary.txt"))?,
        SUMMARY
    );
    let metadata = fs::metadata(&placed)?;
    assert!(!metadata.permissions().readonly(), "{stderr}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(metadata.nlink(), 1, "a file of its own, not the cache's");
    }
    // the files fetchloom placed for a teammate follow the author's next pin, as for any dataset
    rerun(&work)?;
    ok(&sandbox.run(&["add", "data/processed", "--as", "processed", "--yes"])?)?;
    for file in ["data.toml", "data.lock"] {
        fs::copy(work.join(file), teammate.join(file))?;
    }
    let synced = sandbox.command_in(&teammate, &["sync"]).output()?;
    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(0), "{stderr}");
    assert!(!stderr.contains("changed since"), "{stderr}");
    assert_eq!(
        fs::read(&placed)?,
        fs::read(work.join("data/processed/iris_clean.csv"))?
    );
    assert!(
        teammate.join("data/processed/plot.png").is_file(),
        "{stderr}"
    );
    assert!(
        !teammate.join("data/processed/summary.txt").exists(),
        "{stderr}"
    );
    fs::write(&placed, b"rerun\n")?;
    let status = sandbox
        .command_in(&teammate, &["--json", "status"])
        .output()?;
    let document = json(&status)?;
    assert_eq!(
        listed(&document, "edited"),
        ["data/processed/iris_clean.csv"],
        "{document}"
    );
    Ok(())
}

/// a teammate pulling an output's moved folder gets its pin in the new folder, and the copies a
/// sync placed in the old one go as any dataset's old placement goes, with the emptied folder and
/// its marker. a copy changed there, or a file added, stays under the folder's marker, and one line
/// names the folder, once. the marker goes with the folder once those files go.
#[test]
fn a_moved_outputs_old_copies_go_from_a_teammates_folder() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    processed(&work)?;
    ok(&sandbox.run(&["add", "data/processed", "--as", "processed"])?)?;
    let pull = |teammate: &Path| -> Result {
        for file in ["data.toml", "data.lock"] {
            fs::copy(work.join(file), teammate.join(file))?;
        }
        Ok(())
    };
    let [clean, edited] = [sandbox.folder("clean")?, sandbox.folder("edited")?];
    for teammate in [&clean, &edited] {
        pull(teammate)?;
        ok(&sandbox.command_in(teammate, &["sync"]).output()?)?;
        assert_eq!(
            crate::support::marker(&teammate.join("data/processed/.gitignore"))?,
            b"*\n"
        );
    }
    let old = edited.join("data/processed");
    fs::write(old.join("summary.txt"), b"mine\n")?;
    fs::write(old.join("notes.txt"), b"notes\n")?;
    fs::rename(work.join("data/processed"), work.join("data/proc2"))?;
    let manifest = fs::read_to_string(work.join("data.toml"))?;
    fs::write(
        work.join("data.toml"),
        manifest.replace("data/processed", "data/proc2"),
    )?;
    ok(&sandbox.run(&["sync"])?)?;
    pull(&clean)?;
    let synced = sandbox.command_in(&clean, &["sync"]).output()?;
    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(0), "{stderr}");
    assert_eq!(fs::read(clean.join("data/proc2/iris_clean.csv"))?, CLEAN);
    assert!(!clean.join("data/processed").exists(), "{stderr}");
    assert!(!stderr.contains("data/processed"), "{stderr}");
    pull(&edited)?;
    let synced = sandbox.command_in(&edited, &["sync"]).output()?;
    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(0), "{stderr}");
    assert!(!old.join("iris_clean.csv").exists(), "{stderr}");
    assert_eq!(fs::read(old.join("summary.txt"))?, b"mine\n");
    assert_eq!(fs::read(old.join("notes.txt"))?, b"notes\n");
    assert_eq!(
        crate::support::marker(&old.join(".gitignore"))?,
        b"*\n",
        "{stderr}"
    );
    let said = "data/processed held processed until its folder moved to data/proc2, and still holds 2 files its pin does not hold, such as data/processed/notes.txt, so the folder stays";
    assert_eq!(stderr.matches(said).count(), 1, "{stderr}");
    assert!(!stderr.contains("no longer needed"), "{stderr}");
    let again = sandbox.command_in(&edited, &["sync"]).output()?;
    let stderr = text(&again.stderr)?;
    assert_eq!(again.status.code(), Some(0), "{stderr}");
    assert!(!stderr.contains("data/processed"), "{stderr}");
    assert_eq!(
        crate::support::marker(&old.join(".gitignore"))?,
        b"*\n",
        "{stderr}"
    );
    for file in ["summary.txt", "notes.txt"] {
        fs::remove_file(old.join(file))?;
    }
    let emptied = sandbox.command_in(&edited, &["sync"]).output()?;
    let stderr = text(&emptied.stderr)?;
    assert_eq!(emptied.status.code(), Some(0), "{stderr}");
    assert!(!old.exists(), "{stderr}");
    Ok(())
}

/// an output moved back to the folder its move left is that folder's output again: removing it
/// leaves the folder to git as removing any output does, though the move once kept its marker for
/// the person's file there.
#[test]
fn an_output_moved_back_to_its_old_folder_is_removed_as_any_output() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    processed(&work)?;
    ok(&sandbox.run(&["add", "data/processed", "--as", "processed"])?)?;
    let teammate = sandbox.folder("teammate")?;
    let old = teammate.join("data/processed");
    let pull = || -> Result {
        for file in ["data.toml", "data.lock"] {
            fs::copy(work.join(file), teammate.join(file))?;
        }
        ok(&sandbox.command_in(&teammate, &["sync"]).output()?)
    };
    let move_to = |from: &str, to: &str| -> Result {
        fs::rename(work.join(from), work.join(to))?;
        let manifest = fs::read_to_string(work.join("data.toml"))?;
        fs::write(work.join("data.toml"), manifest.replace(from, to))?;
        ok(&sandbox.run(&["sync"])?)?;
        pull()
    };
    pull()?;
    fs::write(old.join("mine.txt"), b"mine\n")?;
    move_to("data/processed", "data/proc2")?;
    assert_eq!(crate::support::marker(&old.join(".gitignore"))?, b"*\n");
    move_to("data/proc2", "data/processed")?;
    let removed = sandbox
        .command_in(&teammate, &["remove", "processed"])
        .output()?;
    let stderr = text(&removed.stderr)?;
    assert_eq!(removed.status.code(), Some(0), "{stderr}");
    assert!(!old.join(".gitignore").exists(), "{stderr}");
    assert_eq!(fs::read(old.join("mine.txt"))?, b"mine\n");
    Ok(())
}

/// a sync deletes nothing outside the folders it lists: an output that moved from a folder outside
/// the data folder leaves a teammate's copies there, under their marker, with no word of `data_dir`.
#[test]
fn a_moved_outputs_copies_outside_the_data_folder_stay() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    fs::create_dir_all(work.join("results"))?;
    fs::write(work.join("results/r.csv"), b"r\n")?;
    ok(&sandbox.run(&["add", "results", "--as", "results"])?)?;
    let teammate = sandbox.folder("teammate")?;
    let pull = || -> Result {
        for file in ["data.toml", "data.lock"] {
            fs::copy(work.join(file), teammate.join(file))?;
        }
        Ok(())
    };
    pull()?;
    ok(&sandbox.command_in(&teammate, &["sync"]).output()?)?;
    fs::rename(work.join("results"), work.join("results2"))?;
    let manifest = fs::read_to_string(work.join("data.toml"))?;
    fs::write(
        work.join("data.toml"),
        manifest.replace("at = \"results\"", "at = \"results2\""),
    )?;
    ok(&sandbox.run(&["sync"])?)?;
    pull()?;
    let synced = sandbox.command_in(&teammate, &["sync"]).output()?;
    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(0), "{stderr}");
    assert_eq!(fs::read(teammate.join("results2/r.csv"))?, b"r\n");
    assert_eq!(
        fs::read(teammate.join("results/r.csv"))?,
        b"r\n",
        "{stderr}"
    );
    assert_eq!(
        crate::support::marker(&teammate.join("results/.gitignore"))?,
        b"*\n"
    );
    assert!(!stderr.contains("data_dir"), "{stderr}");
    Ok(())
}

/// an output's folder gets the .gitignore any dataset folder gets, outside the data folder too,
/// for the author and for a teammate. it is fetchloom's, never the output's: no check, pin,
/// status or sync takes it for a file of the folder.
#[test]
fn an_outputs_folder_gets_its_marker_and_never_takes_it_in() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    fs::create_dir_all(work.join("results"))?;
    fs::write(work.join("results/iris_clean.csv"), CLEAN)?;
    fs::write(work.join("results/summary.txt"), SUMMARY)?;
    ok(&sandbox.run(&["add", "results", "--as", "results"])?)?;
    assert_eq!(
        crate::support::marker(&work.join("results/.gitignore"))?,
        b"*\n"
    );
    let lock = fs::read_to_string(work.join("data.lock"))?;
    assert_eq!(lock.matches("{ path = ").count(), 2, "{lock}");
    let checked = sandbox.run(&["status", "--verify", "--check"])?;
    assert_eq!(checked.status.code(), Some(0), "{}", text(&checked.stderr)?);
    let again = sandbox.run(&["add", "results", "--as", "results"])?;
    let stderr = text(&again.stderr)?;
    assert_eq!(again.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains(
            "      Read results · 2 files · 29 B, the cache held the bytes already; results stays where it is\n"
        ),
        "{stderr}"
    );
    assert!(
        stderr.contains("results is pinned as results holds it"),
        "{stderr}"
    );
    assert_eq!(fs::read_to_string(work.join("data.lock"))?, lock);
    let teammate = sandbox.folder("teammate")?;
    for file in ["data.toml", "data.lock"] {
        fs::copy(work.join(file), teammate.join(file))?;
    }
    let synced = sandbox.command_in(&teammate, &["sync"]).output()?;
    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    assert_eq!(
        crate::support::marker(&teammate.join("results/.gitignore"))?,
        b"*\n"
    );
    for project in [&work, &teammate] {
        let status = sandbox
            .command_in(project, &["--json", "status"])
            .output()?;
        let document = json(&status)?;
        for kind in ["missing", "edited", "extra", "yours"] {
            assert_eq!(
                listed(&document, kind),
                Vec::<String>::new(),
                "{kind}: {document}"
            );
        }
        let synced = sandbox.command_in(project, &["--json", "sync"]).output()?;
        let document = json(&synced)?;
        assert_eq!(synced.status.code(), Some(0), "{document}");
        assert!(
            warned(&document)
                .iter()
                .all(|said| !said.contains("changed since")),
            "{document}"
        );
    }
    Ok(())
}

/// a renamed output folder takes fetchloom's marker with it, which the state records under the
/// old name: still the marker writer's own by its text, it is never taken into the output, by a
/// pin again or its dry run, and never a change, so nothing asks for a yes or tells of an
/// addition. a .gitignore of the author's own stays the output's, even one that begins as the
/// marker does, and so does a file of another name holding the marker's text: pinned, and a change
/// when a new one comes.
#[test]
fn a_renamed_outputs_folder_never_takes_its_marker_in() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    processed(&work)?;
    fs::create_dir(work.join("data/processed/figures"))?;
    fs::write(
        work.join("data/processed/figures/.gitignore"),
        "*\n!keep.csv\n",
    )?;
    fs::write(work.join("data/processed/patterns.txt"), "*\n")?;
    ok(&sandbox.run(&["add", "data/processed", "--as", "processed"])?)?;
    let lock = fs::read_to_string(work.join("data.lock"))?;
    assert!(lock.contains("path = \"patterns.txt\""), "{lock}");
    fs::rename(work.join("data/processed"), work.join("data/features"))?;
    assert_eq!(
        crate::support::marker(&work.join("data/features/.gitignore"))?,
        b"*\n"
    );
    let manifest = fs::read_to_string(work.join("data.toml"))?;
    fs::write(
        work.join("data.toml"),
        manifest.replace("at = \"data/processed\"", "at = \"data/features\""),
    )?;
    let document = json(&sandbox.run(&["--json", "status"])?)?;
    assert_eq!(
        listed(&document, "extra"),
        Vec::<String>::new(),
        "{document}"
    );
    for (args, line) in [
        (&["sync"][..], "Synced 1 dataset into data"),
        (
            &["add", "data/features", "--as", "processed", "--dry-run"],
            "Would keep processed as it is",
        ),
        (
            &["add", "data/features", "--as", "processed"],
            "processed is pinned as data/features holds it",
        ),
    ] {
        let said = sandbox.run(args)?;
        let stderr = text(&said.stderr)?;
        assert_eq!(said.status.code(), Some(0), "{args:?}: {stderr}");
        assert!(stderr.contains(line), "{args:?}: {stderr}");
        assert!(!stderr.contains(".gitignore"), "{args:?}: {stderr}");
        assert!(!stderr.contains("changed since"), "{args:?}: {stderr}");
    }
    let lock = fs::read_to_string(work.join("data.lock"))?;
    assert!(!lock.contains("path = \".gitignore\""), "{lock}");
    assert!(lock.contains("path = \"figures/.gitignore\""), "{lock}");
    let checked = sandbox.run(&["status", "--check"])?;
    assert_eq!(checked.status.code(), Some(0), "{}", text(&checked.stderr)?);
    let synced = sandbox.run(&["sync"])?;
    let stderr = text(&synced.stderr)?;
    assert!(
        stderr.contains("Ok all pinned files are in place by size and time, nothing to do"),
        "{stderr}"
    );
    fs::write(work.join("data/features/globs.txt"), "*\n")?;
    let document = json(&sandbox.run(&["--json", "status"])?)?;
    assert_eq!(
        listed(&document, "extra"),
        ["data/features/globs.txt"],
        "{document}"
    );
    Ok(())
}

/// a change of an output's `at` alone, as after the person moved its folder and said so in
/// data.toml, takes no new pin: status says a sync moves the pin to the new folder, as it does,
/// and names no add.
#[test]
fn a_moved_outputs_new_at_is_told_as_a_sync_moving_its_pin() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    processed(&work)?;
    ok(&sandbox.run(&["add", "data/processed", "--as", "processed"])?)?;
    fs::rename(work.join("data/processed"), work.join("data/features"))?;
    let manifest = fs::read_to_string(work.join("data.toml"))?;
    fs::write(
        work.join("data.toml"),
        manifest.replace("at = \"data/processed\"", "at = \"data/features\""),
    )?;
    let status = sandbox.run(&["status"])?;
    let (stdout, stderr) = (text(&status.stdout)?, text(&status.stderr)?);
    assert!(
        stdout.contains("Unlocked processed · data.lock pins it at data/processed, and `fetchloom sync` moves that pin to data/features\n"),
        "{stdout}"
    );
    assert!(!stderr.contains("fetchloom add"), "{stderr}");
    let checked = sandbox.run(&["--json", "status", "--check"])?;
    let document = json(&checked)?;
    assert_eq!(checked.status.code(), Some(1), "{document}");
    assert_eq!(
        document["errors"][0]["details"],
        serde_json::json!([
            "processed is not pinned as data.toml asks: `fetchloom sync` pins and places it"
        ]),
        "{document}"
    );
    let synced = sandbox.run(&["sync"])?;
    let stderr = text(&synced.stderr)?;
    assert!(
        stderr.contains("Locked processed · 2 files in data.lock\n"),
        "{stderr}"
    );
    let checked = sandbox.run(&["status", "--check"])?;
    assert_eq!(checked.status.code(), Some(0), "{}", text(&checked.stderr)?);
    Ok(())
}

/// an output whose `at` data.toml changed while its folder stayed is told to move the folder that
/// still holds its pin, by status, sync and an add of the new folder; once moved, sync has
/// nothing to tell of it.
#[test]
fn an_at_changed_without_the_folder_moved_names_the_move() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    // an output pinned before it, whose files no move of processed is about
    fs::create_dir_all(work.join("data/aaa"))?;
    fs::write(work.join("data/aaa/a.csv"), b"a\n")?;
    ok(&sandbox.run(&["add", "data/aaa", "--as", "aaa"])?)?;
    processed(&work)?;
    ok(&sandbox.run(&["add", "data/processed", "--as", "processed"])?)?;
    let manifest = work.join("data.toml");
    let table = fs::read_to_string(&manifest)?;
    let moved = table.replace("at = \"data/processed\"", "at = \"data/proc2\"");
    assert_ne!(moved, table);
    fs::write(&manifest, moved)?;
    let moved = if cfg!(windows) {
        r#"`move "data\processed" "data\proc2"`"#
    } else {
        "`mv data/processed data/proc2`"
    };
    let ways = format!(
        "{moved} moves it where data.toml puts it now, or `fetchloom sync --force processed` puts back the files processed pins"
    );
    let status = text(&sandbox.run(&["status"])?.stderr)?;
    assert!(
        status.contains(&format!("hint: data/proc2 is gone: {ways}\n")),
        "{status}"
    );
    let synced = sandbox.run(&["sync"])?;
    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(0), "{stderr}");
    assert!(stderr.contains(&format!("  help: {ways}\n")), "{stderr}");
    let added = text(
        &sandbox
            .run(&["add", "data/proc2", "--as", "processed"])?
            .stderr,
    )?;
    assert!(added.contains(&format!("  help: {ways}\n")), "{added}");
    fs::rename(work.join("data/processed"), work.join("data/proc2"))?;
    let synced = sandbox.run(&["sync"])?;
    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(0), "{stderr}");
    assert!(!stderr.contains("processed was pinned"), "{stderr}");
    assert_eq!(fs::read(work.join("data/proc2/iris_clean.csv"))?, CLEAN);
    // a folder left that no longer holds the pin is no move's
    fs::write(&manifest, table)?;
    crate::support::edit(&work.join("data/proc2/iris_clean.csv"), b"edited\n")?;
    let status = text(&sandbox.run(&["status"])?.stderr)?;
    assert!(
        status.contains("hint: data/processed is gone: `fetchloom sync --force processed` puts back the files processed pins, or if you moved it, set at"),
        "{status}"
    );
    Ok(())
}

/// an output pinned here whose folder is gone while no cache here holds its files, as after a new
/// `FETCHLOOM_CACHE`: the ways on name its pipeline, not a sync that would put back nothing, and no
/// line says where they were made. with a mirror, which may hold them, they name the
/// sync, which puts them back from it.
#[test]
fn a_gone_output_no_cache_holds_names_its_pipeline() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    processed(&work)?;
    ok(&sandbox.run(&["add", "data/processed", "--as", "processed"])?)?;
    let mirror = sandbox.folder("mirror")?;
    let mirror = mirror.to_string_lossy().into_owned();
    ok(&sandbox.run(&["push", "--to", &mirror, "--yes"])?)?;
    delete_folder(&work.join("data/processed"))?;
    let cache = sandbox.folder("empty-cache")?;
    let run = |args: &[&str]| -> Result<String> {
        let output = sandbox
            .command(args)
            .env("FETCHLOOM_CACHE", &cache)
            .output()?;
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(0), "{args:?}: {stderr}");
        Ok(stderr)
    };
    let ways = "the files processed pins are in no cache or mirror here, so push them from a machine that has them with `fetchloom push --to <mirror>` and add that mirror here with `fetchloom settings set mirrors <mirror>`, or run your pipeline here, then `fetchloom add ./data/processed --as processed`, or if you moved it, set at = \"<new folder>\" for processed in data.toml";
    let status = run(&["status"])?;
    assert!(
        status.contains(&format!("hint: data/processed is gone: {ways}\n")),
        "{status}"
    );
    assert!(!status.contains("another machine"), "{status}");
    let back = "`fetchloom sync --force processed` puts back the files processed pins, or if you moved it, set at = \"<new folder>\" for processed in data.toml";
    let mirrored = run(&["status", "--mirror", &mirror])?;
    assert!(
        mirrored.contains(&format!("hint: data/processed is gone: {back}\n")),
        "{mirrored}"
    );
    let previewed = run(&["sync", "--dry-run", "--mirror", &mirror])?;
    assert!(
        previewed.contains(&format!(
            "warning: data/processed is gone\n  help: {back}\n"
        )),
        "{previewed}"
    );
    let elsewhere = sandbox.folder("elsewhere")?;
    let pushed = sandbox
        .command(&["push", "--to", &elsewhere.to_string_lossy(), "--yes"])
        .env("FETCHLOOM_CACHE", &cache)
        .output()?;
    let refused = text(&pushed.stderr)?;
    assert_eq!(told_of_processed(&refused), REFUSED, "{refused}");
    // the mirror holds the files, so push refuses nothing and tells the folder as status does
    let served = sandbox
        .command(&["push", "--to", &mirror, "--yes"])
        .env("FETCHLOOM_CACHE", &cache)
        .output()?;
    let stderr = text(&served.stderr)?;
    assert_eq!(served.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains(&format!(
            "warning: data/processed is gone, so push sends the files processed pins\n  help: {ways}\n"
        )),
        "{stderr}"
    );
    let synced = run(&["sync"])?;
    assert!(
        synced.contains(&format!(
            "warning: data/processed is gone\n  help: {ways}\n"
        )),
        "{synced}"
    );
    let adding = sandbox
        .command(&["--offline", "add", "data/processed", "--as", "processed"])
        .env("FETCHLOOM_CACHE", &cache)
        .output()?;
    let gone = text(&adding.stderr)?;
    assert_eq!(adding.status.code(), Some(2), "{gone}");
    assert!(
        gone.contains(&format!("error: data/processed is the folder of the output processed, and it is not there\n  help: {ways}\n")),
        "{gone}"
    );
    let forced = run(&["sync", "--force", "processed"])?;
    assert!(
        forced.contains("warning: the files processed pins are in no cache or mirror here\n  help: push them from a machine that has them with `fetchloom push --to <mirror>` and add that mirror here with `fetchloom settings set mirrors <mirror>`, or run your pipeline here, then `fetchloom add ./data/processed --as processed`\n"),
        "{forced}"
    );
    run(&["sync", "--force", "processed", "--mirror", &mirror])?;
    assert_eq!(fs::read(work.join("data/processed/iris_clean.csv"))?, CLEAN);
    assert_eq!(fs::read(work.join("data/processed/summary.txt"))?, SUMMARY);
    Ok(())
}

/// an output captured here, put back by `sync --force` and then lost with its cache, is told as
/// any output whose pinned files no cache or mirror here holds, in the same words by `sync --force`
/// and push, which says it once.
#[test]
fn a_restored_output_lost_again_is_told_as_out_of_reach() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    processed(&work)?;
    ok(&sandbox.run(&["add", "data/processed", "--as", "processed"])?)?;
    delete_folder(&work.join("data/processed"))?;
    ok(&sandbox.run(&["sync", "--force", "processed"])?)?;
    assert_eq!(fs::read(work.join("data/processed/iris_clean.csv"))?, CLEAN);
    delete_folder(&work.join("data/processed"))?;
    let cache = sandbox.folder("empty-cache")?;
    let lost = "the files processed pins are in no cache or mirror here\n  help: push them from a machine that has them with `fetchloom push --to <mirror>` and add that mirror here with `fetchloom settings set mirrors <mirror>`, or run your pipeline here, then `fetchloom add ./data/processed --as processed`\n";
    let elsewhere = sandbox.folder("elsewhere")?;
    let pushed = sandbox
        .command(&["push", "--to", &elsewhere.to_string_lossy(), "--yes"])
        .env("FETCHLOOM_CACHE", &cache)
        .output()?;
    let stderr = text(&pushed.stderr)?;
    assert_eq!(pushed.status.code(), Some(10), "{stderr}");
    assert!(stderr.contains(&format!("error: {lost}")), "{stderr}");
    assert_eq!(
        stderr.matches("run your pipeline here").count(),
        1,
        "{stderr}"
    );
    let forced = sandbox
        .command(&["sync", "--force", "processed"])
        .env("FETCHLOOM_CACHE", &cache)
        .output()?;
    let stderr = text(&forced.stderr)?;
    assert_eq!(forced.status.code(), Some(0), "{stderr}");
    assert!(stderr.contains(&format!("warning: {lost}")), "{stderr}");
    Ok(())
}

/// an output pinned here whose pinned bytes no cache here holds, as after a new `FETCHLOOM_CACHE`:
/// an `at` changed without the folder moved, an edited folder and the folder a forced sync makes
/// to wait for the pipeline are offered no `sync --force`, which would put back nothing, and push
/// says it sends nothing it cannot. a folder whose only change is an extra file, which `--force`
/// deletes, still is.
#[test]
fn a_changed_output_no_cache_holds_offers_no_force() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    processed(&work)?;
    ok(&sandbox.run(&["add", "data/processed", "--as", "processed"])?)?;
    let cache = sandbox.folder("empty-cache")?;
    let run = |args: &[&str]| -> Result<(Option<i32>, String)> {
        let output = sandbox
            .command(args)
            .env("FETCHLOOM_CACHE", &cache)
            .output()?;
        Ok((output.status.code(), text(&output.stderr)?))
    };
    let manifest = work.join("data.toml");
    let table = fs::read_to_string(&manifest)?;
    let moved = table.replace("at = \"data/processed\"", "at = \"results/processed\"");
    assert_ne!(moved, table);
    fs::write(&manifest, moved)?;
    let moved = if cfg!(windows) {
        r#"`mkdir results`, then `move "data\processed" "results\processed"`"#
    } else {
        "`mkdir -p results && mv data/processed results/processed`"
    };
    let (_, status) = run(&["status"])?;
    assert!(
        status.contains(&format!(
            "hint: results/processed is gone: {moved} moves it where data.toml puts it now\n"
        )),
        "{status}"
    );
    fs::write(&manifest, &table)?;
    fs::write(work.join("data/processed/extra.csv"), b"x")?;
    let (_, extra) = run(&["status"])?;
    assert!(
        extra.contains("1 extra file: `fetchloom add ./data/processed --as processed` pins what it holds now, or `fetchloom sync --force processed` makes it hold just the files processed pins\n"),
        "{extra}"
    );
    fs::remove_file(work.join("data/processed/extra.csv"))?;
    fs::write(work.join("data/processed/iris_clean.csv"), b"edited")?;
    let ways = "the files processed pins are in no cache or mirror here, so push them from a machine that has them with `fetchloom push --to <mirror>` and add that mirror here with `fetchloom settings set mirrors <mirror>`, or run your pipeline here, then `fetchloom add ./data/processed --as processed`";
    let (_, edited) = run(&["status"])?;
    assert!(
        edited.contains(&format!("1 edited file: {ways}\n")),
        "{edited}"
    );
    let (code, synced) = run(&["sync"])?;
    assert_eq!(code, Some(0), "{synced}");
    assert!(
        synced.contains(&format!("warning: data/processed changed since processed was pinned: 1 edited file, so sync leaves it as it is\n  help: {ways}\n")),
        "{synced}"
    );
    let elsewhere = sandbox.folder("elsewhere")?;
    let (code, pushed) = run(&["push", "--to", &elsewhere.to_string_lossy(), "--yes"])?;
    assert_eq!(code, Some(10), "{pushed}");
    assert_eq!(told_of_processed(&pushed), REFUSED, "{pushed}");
    // the folder a forced sync makes for the pipeline holds none of the output's files: it is gone,
    // and nothing offers to pin what it holds
    delete_folder(&work.join("data/processed"))?;
    let (code, forced) = run(&["sync", "--force", "processed"])?;
    assert_eq!(code, Some(0), "{forced}");
    let (_, waiting) = run(&["status"])?;
    assert!(
        waiting.contains("hint: data/processed is gone: the files processed pins are in no cache or mirror here, so push them from a machine that has them with `fetchloom push --to <mirror>` and add that mirror here with `fetchloom settings set mirrors <mirror>`, or run your pipeline here, then `fetchloom add ./data/processed --as processed`, or if you moved it, set at = \"<new folder>\" for processed in data.toml\n"),
        "{waiting}"
    );
    Ok(())
}

/// the lines of `stderr` that tell of the output processed.
fn told_of_processed(stderr: &str) -> Vec<&str> {
    stderr
        .lines()
        .filter(|line| line.contains("processed"))
        .collect()
}

/// all a push says of processed when it refuses it, as nothing here holds its files.
const REFUSED: [&str; 2] = [
    "error: the files processed pins are in no cache or mirror here",
    "  help: push them from a machine that has them with `fetchloom push --to <mirror>` and add that mirror here with `fetchloom settings set mirrors <mirror>`, or run your pipeline here, then `fetchloom add ./data/processed --as processed`",
];

/// a teammate's sync placed an output's first pin from a mirror; the author pinned a new run on
/// their machine and pushed nothing, and the teammate added a note to the folder. no `sync --force`
/// is offered, as it would have to write the new pin's files, which nothing here holds, and every
/// command says so in the same words, which claim nothing of the machine that made them.
#[test]
fn a_teammates_output_pinned_anew_elsewhere_offers_no_force() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    fs::create_dir_all(work.join("results"))?;
    fs::write(work.join("results/a.csv"), b"v1\n")?;
    ok(&sandbox.run(&["add", "results", "--as", "results"])?)?;
    let mirror = sandbox.folder("mirror")?;
    let mirror = mirror.to_string_lossy().into_owned();
    ok(&sandbox.run(&["push", "--to", &mirror, "--yes"])?)?;
    let teammate = sandbox.folder("teammate")?;
    for file in ["data.toml", "data.lock"] {
        fs::copy(work.join(file), teammate.join(file))?;
    }
    let cache = sandbox.folder("teammate-cache")?;
    let run = |args: &[&str]| -> Result<String> {
        let output = sandbox
            .command_in(&teammate, args)
            .env("FETCHLOOM_CACHE", &cache)
            .output()?;
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(0), "{args:?}: {stderr}");
        Ok(stderr)
    };
    run(&["sync", "--mirror", &mirror])?;
    assert_eq!(fs::read(teammate.join("results/a.csv"))?, b"v1\n");
    fs::write(work.join("results/a.csv"), b"v2\n")?;
    ok(&sandbox.run(&["add", "./results", "--as", "results", "--yes"])?)?;
    fs::copy(work.join("data.lock"), teammate.join("data.lock"))?;
    fs::write(teammate.join("results/notes.txt"), b"my notes\n")?;
    let unreached = "the files results pins are in no cache or mirror here";
    let ways = "push them from a machine that has them with `fetchloom push --to <mirror>` and add that mirror here with `fetchloom settings set mirrors <mirror>`, or run your pipeline here, then `fetchloom add ./results --as results`";
    let status = run(&["status"])?;
    assert!(
        status.contains(&format!("1 extra file: {unreached}, so {ways}\n")),
        "{status}"
    );
    assert!(!status.contains("--force"), "{status}");
    let synced = run(&["sync"])?;
    assert!(
        synced.contains(&format!(
            "1 extra file, so sync leaves it as it is\n  help: {unreached}, so {ways}\n"
        )),
        "{synced}"
    );
    assert!(!synced.contains("--force"), "{synced}");
    let forced = run(&["sync", "--force", "results"])?;
    assert!(
        forced.contains(&format!("warning: {unreached}\n  help: {ways}\n")),
        "{forced}"
    );
    assert_eq!(fs::read(teammate.join("results/a.csv"))?, b"v1\n");
    Ok(())
}

/// a push that refuses an output, as neither the target nor anything here holds its files, says
/// nothing else of it, whether a forced sync put the capture back before the cache was lost or
/// not, and though a forced sync made its folder anew for the pipeline.
#[test]
fn a_push_refusing_an_output_says_only_that() -> Result {
    for restored in [false, true] {
        let sandbox = Sandbox::new()?;
        let work = sandbox.work();
        processed(&work)?;
        ok(&sandbox.run(&["add", "data/processed", "--as", "processed"])?)?;
        if restored {
            delete_folder(&work.join("data/processed"))?;
            ok(&sandbox.run(&["sync", "--force", "processed"])?)?;
        }
        delete_folder(&work.join("data/processed"))?;
        let cache = sandbox.folder("empty-cache")?;
        let forced = sandbox
            .command(&["sync", "--force", "processed"])
            .env("FETCHLOOM_CACHE", &cache)
            .output()?;
        assert_eq!(forced.status.code(), Some(0), "{}", text(&forced.stderr)?);
        let elsewhere = sandbox.folder("elsewhere")?;
        let pushed = sandbox
            .command(&["push", "--to", &elsewhere.to_string_lossy(), "--yes"])
            .env("FETCHLOOM_CACHE", &cache)
            .output()?;
        let stderr = text(&pushed.stderr)?;
        assert_eq!(pushed.status.code(), Some(10), "{stderr}");
        assert_eq!(
            told_of_processed(&stderr),
            REFUSED,
            "restored {restored}: {stderr}"
        );
    }
    Ok(())
}

/// the same add without a terminal or --yes offers `sync --force` only where it puts the files
/// back: with the pinned bytes in no cache here, it names the pipeline, and in a clone, where no
/// file there is one fetchloom placed, it names checking `at`, as sync does.
#[test]
fn the_same_add_offers_force_only_where_it_puts_files_back() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    processed(&work)?;
    ok(&sandbox.run(&["add", "data/processed", "--as", "processed"])?)?;
    rerun(&work)?;
    let cache = sandbox.folder("empty-cache")?;
    let lost = sandbox
        .command(&["add", "data/processed", "--as", "processed"])
        .env("FETCHLOOM_CACHE", &cache)
        .output()?;
    let stderr = text(&lost.stderr)?;
    assert_eq!(lost.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("  help: the files processed pins are in no cache or mirror here, so push them from a machine that has them with `fetchloom push --to <mirror>` and add that mirror here with `fetchloom settings set mirrors <mirror>`, or run your pipeline here, then `fetchloom add ./data/processed --as processed --yes`\n"),
        "{stderr}"
    );
    let teammate = sandbox.folder("teammate")?;
    for file in ["data.toml", "data.lock"] {
        fs::copy(work.join(file), teammate.join(file))?;
    }
    fs::create_dir_all(teammate.join("data/processed"))?;
    fs::write(teammate.join("data/processed/iris_clean.csv"), b"mine")?;
    let cloned = sandbox
        .command_in(&teammate, &["add", "data/processed", "--as", "processed"])
        .output()?;
    let stderr = text(&cloned.stderr)?;
    assert_eq!(cloned.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("  help: no file there is one fetchloom placed on this machine, so check that `at` for processed in data.toml names its folder; `fetchloom add ./data/processed --as processed --yes` pins what it holds now\n"),
        "{stderr}"
    );
    Ok(())
}

/// an output's `at` changed to a folder whose parent is not there yet: the help makes the parent
/// before the move, which then works, and status, whose only differences are that output's, which
/// sync leaves whole, names no sync that places files.
#[test]
fn a_move_into_a_missing_folder_makes_it_first() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    processed(&work)?;
    ok(&sandbox.run(&["add", "data/processed", "--as", "processed"])?)?;
    let manifest = work.join("data.toml");
    let table = fs::read_to_string(&manifest)?;
    let moved = table.replace(
        "at = \"data/processed\"",
        "at = \"results/final/processed\"",
    );
    assert_ne!(moved, table);
    fs::write(&manifest, moved)?;
    let moved = if cfg!(windows) {
        r#"`mkdir "results\final"`, then `move "data\processed" "results\final\processed"`"#
    } else {
        "`mkdir -p results/final && mv data/processed results/final/processed`"
    };
    let status = text(&sandbox.run(&["status"])?.stderr)?;
    assert!(
        status.contains(&format!("hint: results/final/processed is gone: {moved} moves it where data.toml puts it now, or `fetchloom sync --force processed` puts back the files processed pins\n")),
        "{status}"
    );
    assert!(
        !status.contains("places missing or updated files"),
        "{status}"
    );
    fs::create_dir_all(work.join("results/final"))?;
    fs::rename(
        work.join("data/processed"),
        work.join("results/final/processed"),
    )?;
    let synced = sandbox.run(&["sync"])?;
    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(0), "{stderr}");
    assert!(!stderr.contains("is gone"), "{stderr}");
    assert_eq!(
        fs::read(work.join("results/final/processed/iris_clean.csv"))?,
        CLEAN
    );
    Ok(())
}

/// an output's folder that is gone, as after a move data.toml does not know of yet or a failed
/// run, is told as gone with the two ways back, and an add of it in any spelling says so rather
/// than send the folder's name off as search words.
#[test]
fn a_gone_outputs_folder_is_told_as_gone_and_never_searched_for() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    processed(&work)?;
    ok(&sandbox.run(&["add", "data/processed", "--as", "processed"])?)?;
    fs::rename(work.join("data/processed"), work.join("data/features"))?;
    let back = "`fetchloom sync --force processed` puts back the files processed pins, or if you moved it, set at = \"<new folder>\" for processed in data.toml";
    let status = sandbox.run(&["status"])?;
    let stderr = text(&status.stderr)?;
    assert!(
        stderr.contains(&format!("hint: data/processed is gone: {back}\n")),
        "{stderr}"
    );
    let synced = sandbox.run(&["sync"])?;
    let stderr = text(&synced.stderr)?;
    assert!(
        stderr.contains(&format!(
            "warning: data/processed is gone\n  help: {back}\n"
        )),
        "{stderr}"
    );
    let checked = sandbox.run(&["--json", "status", "--check"])?;
    let document = json(&checked)?;
    assert_eq!(checked.status.code(), Some(1), "{document}");
    assert_eq!(
        document["errors"][0]["details"],
        serde_json::json!([format!("data/processed is gone: {back}")]),
        "{document}"
    );
    for typed in [
        &["data/processed", "--as", "processed"][..],
        &["./data/processed", "--as", "processed"],
        &["data/processed"],
    ] {
        let added = sandbox.run(&[&["--offline", "add"][..], typed].concat())?;
        let stderr = text(&added.stderr)?;
        assert_eq!(added.status.code(), Some(2), "{typed:?}: {stderr}");
        assert!(
            stderr.contains(&format!(
                "error: {} is the folder of the output processed, and it is not there\n  help: `fetchloom sync --force processed` puts back the files processed pins, or if you moved it, set at = \"<new folder>\" for processed in data.toml\n",
                typed[0]
            )),
            "{typed:?}: {stderr}"
        );
    }
    // the folder put back is told as any dataset's, by the key that names an output's folder
    let forced = sandbox.run(&["sync", "--force", "processed"])?;
    let stderr = text(&forced.stderr)?;
    assert_eq!(forced.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("  hint: if you deleted data/processed to free space, `fetchloom drop processed` keeps it off this machine, or if you moved it, set at = \"<new folder>\" for processed in data.toml\n"),
        "{stderr}"
    );
    assert_eq!(fs::read(work.join("data/processed/iris_clean.csv"))?, CLEAN);
    Ok(())
}

/// git carries data.lock, never an output's bytes: a teammate whose cache and mirrors lack them
/// still gets every other dataset, and the run names the output once, as in no cache or mirror
/// here, with both ways on, and ends well, so a pipeline run after it can make the output. the output's
/// folder is made with fetchloom's marker, as a placed output's is, so that run writes out of
/// git's sight, and `path` names a folder that is there. only a strict sync or a status check calls
/// the data not ready, and a dry run makes nothing.
#[test]
fn a_teammate_without_an_outputs_bytes_gets_the_rest_and_both_ways_on() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    let server = Routes::start(vec![(PATH.to_owned(), file(IRIS, "\"v1\""))])?;
    let link = format!("{}{PATH}", server.url);
    sandbox.allow(&link);
    ok(&sandbox.run(&["add", &link])?)?;
    processed(&work)?;
    ok(&sandbox.run(&["add", "data/processed", "--as", "processed"])?)?;
    let teammate = sandbox.folder("teammate")?;
    for file in ["data.toml", "data.lock"] {
        fs::copy(work.join(file), teammate.join(file))?;
    }
    let cache = sandbox.folder("teammate-cache")?;
    let sync = |args: &[&str]| {
        sandbox
            .command_in(&teammate, args)
            .env("FETCHLOOM_CACHE", &cache)
            .output()
    };
    let said = "the files processed pins are in no cache or mirror here";
    let ways = "push them from a machine that has them with `fetchloom push --to <mirror>` and add that mirror here with `fetchloom settings set mirrors <mirror>`, or run your pipeline here, then `fetchloom add ./data/processed --as processed`";
    let folder = teammate.join("data").join("processed");
    let dry = sync(&["sync", "--dry-run"])?;
    assert_eq!(dry.status.code(), Some(0), "{}", text(&dry.stderr)?);
    assert!(!teammate.join("data").exists(), "{}", text(&dry.stderr)?);
    let synced = sync(&["--json", "sync"])?;
    let document = json(&synced)?;
    assert_eq!(synced.status.code(), Some(0), "{document}");
    let elsewhere = warning(&document, "output.elsewhere");
    assert_eq!(elsewhere["message"], said, "{document}");
    assert_eq!(elsewhere["help"], ways, "{document}");
    assert_eq!(fs::read(teammate.join("data/iris/iris.csv"))?, IRIS);
    let marker = crate::support::marker(&folder.join(".gitignore")).ok();
    assert_eq!(marker, Some(b"*\n".to_vec()), "{document}");
    assert_eq!(fs::read_dir(&folder)?.count(), 1, "{document}");
    let synced = sync(&["sync"])?;
    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(0), "{stderr}");
    assert!(!stderr.contains(said), "told by the run before: {stderr}");
    assert!(
        stderr.contains(
            "Ok all pinned files but processed's are in place by size and time, nothing to do"
        ),
        "{stderr}"
    );
    let not_ready = format!("{said}: {ways}");
    for args in [
        ["--json", "sync", "--strict"],
        ["--json", "status", "--check"],
    ] {
        let checked = sync(&args)?;
        let document = json(&checked)?;
        assert_eq!(checked.status.code(), Some(1), "{args:?}: {document}");
        let error = &document["errors"][0];
        assert_eq!(error["kind"], "data.not_ready", "{args:?}: {document}");
        assert_eq!(
            error["details"],
            serde_json::json!([not_ready]),
            "{args:?}: {document}"
        );
    }
    let path = sync(&["path", "processed"])?;
    let stderr = text(&path.stderr)?;
    assert_eq!(path.status.code(), Some(0), "{stderr}");
    assert_eq!(text(&path.stdout)?, format!("{}\n", folder.display()));
    assert!(!stderr.contains(said), "told by the runs before: {stderr}");
    let document = json(&sync(&["--json", "path", "processed"])?)?;
    assert_eq!(
        document["result"],
        serde_json::json!({ "path": folder }),
        "{document}"
    );
    assert_eq!(
        warning(&document, "output.elsewhere")["message"],
        said,
        "{document}"
    );
    let offline = sync(&["--json", "--offline", "sync"])?;
    let document = json(&offline)?;
    assert_eq!(offline.status.code(), Some(0), "{document}");
    assert_eq!(
        warning(&document, "output.elsewhere")["message"],
        said,
        "{document}"
    );
    let dry = sync(&["--json", "sync", "--dry-run"])?;
    let document = json(&dry)?;
    assert_eq!(dry.status.code(), Some(0), "{document}");
    assert_eq!(
        warning(&document, "output.elsewhere")["message"],
        said,
        "{document}"
    );
    assert_eq!(fs::read_dir(&folder)?.count(), 1, "{document}");
    let mirror = sandbox.folder("mirror")?;
    let mirror = mirror.to_string_lossy();
    let pushed = json(&sync(&["--json", "push", "--to", &mirror, "--yes"])?)?;
    assert_eq!(pushed["errors"][0]["message"], said, "{pushed}");
    Ok(())
}

/// a teammate's waiting output that pins a .gitignore of the author's own, as dvc writes beside
/// what it makes, gets its folder with no marker of fetchloom's, since that file goes there once
/// the bytes come; `path` names the folder, which is there.
#[test]
fn a_waiting_output_with_its_own_gitignore_gets_its_folder_alone() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    processed(&work)?;
    fs::write(work.join("data/processed/.gitignore"), "/iris_clean.csv\n")?;
    ok(&sandbox.run(&["add", "data/processed", "--as", "processed"])?)?;
    let teammate = sandbox.folder("teammate")?;
    for file in ["data.toml", "data.lock"] {
        fs::copy(work.join(file), teammate.join(file))?;
    }
    let cache = sandbox.folder("teammate-cache")?;
    let run = |args: &[&str]| {
        sandbox
            .command_in(&teammate, args)
            .env("FETCHLOOM_CACHE", &cache)
            .output()
    };
    let synced = run(&["sync"])?;
    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(0), "{stderr}");
    let folder = teammate.join("data").join("processed");
    assert!(folder.is_dir(), "{stderr}");
    assert_eq!(fs::read_dir(&folder)?.count(), 0, "{stderr}");
    let path = run(&["path", "processed"])?;
    assert_eq!(text(&path.stdout)?, format!("{}\n", folder.display()));
    Ok(())
}

/// a teammate's status names every output whose pinned files no cache or mirror here holds in one
/// hint, with the ways on for each, in the words a sync that asked a mirror tells each with.
#[test]
fn status_names_the_outputs_out_of_reach_in_one_hint() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    for name in ["a", "b"] {
        let folder = work.join("data").join(name);
        fs::create_dir_all(&folder)?;
        fs::write(folder.join("x.csv"), name)?;
        ok(&sandbox.run(&["add", &format!("data/{name}"), "--as", name])?)?;
    }
    let teammate = sandbox.folder("teammate")?;
    for file in ["data.toml", "data.lock"] {
        fs::copy(work.join(file), teammate.join(file))?;
    }
    let cache = sandbox.folder("teammate-cache")?;
    let run = |args: &[&str]| {
        sandbox
            .command_in(&teammate, args)
            .env("FETCHLOOM_CACHE", &cache)
            .output()
    };
    let stderr = text(&run(&["status"])?.stderr)?;
    assert!(
        stderr.contains("  hint: the files a and b pin are in no cache or mirror here: push them from a machine that has them with `fetchloom push --to <mirror>` and add that mirror here with `fetchloom settings set mirrors <mirror>`, or run your pipeline here, then `fetchloom add <folder> --as <name>` for each\n"),
        "{stderr}"
    );
    let mirror = sandbox.folder("mirror")?;
    let document = json(&run(&[
        "--json",
        "sync",
        "--mirror",
        &mirror.to_string_lossy(),
    ])?)?;
    assert_eq!(
        warning(&document, "output.elsewhere")["message"],
        "the files a pins are in no cache or mirror here",
        "{document}"
    );
    Ok(())
}

/// a teammate's sync placed an output's first pin; the author pinned a second run with another
/// cache and pushed nothing. the teammate's outdated copy is one a sync must replace, and nothing
/// here holds the new pin's bytes, so sync and `status --check` name it in the same neutral words,
/// and nothing sends the person to a sync that places nothing.
#[test]
fn an_outdated_output_no_cache_holds_reads_alike_in_status_and_sync() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    fs::create_dir_all(work.join("results"))?;
    fs::write(work.join("results/a.csv"), b"v1\n")?;
    ok(&sandbox.run(&["add", "results", "--as", "results"])?)?;
    let teammate = sandbox.folder("teammate")?;
    for file in ["data.toml", "data.lock"] {
        fs::copy(work.join(file), teammate.join(file))?;
    }
    ok(&sandbox.command_in(&teammate, &["sync"]).output()?)?;
    assert_eq!(fs::read(teammate.join("results/a.csv"))?, b"v1\n");
    let other = sandbox.folder("other-cache")?;
    fs::write(work.join("results/a.csv"), b"v2\n")?;
    ok(&sandbox
        .command(&["add", "./results", "--as", "results", "--yes"])
        .env("FETCHLOOM_CACHE", &other)
        .output()?)?;
    fs::copy(work.join("data.lock"), teammate.join("data.lock"))?;
    let unreached = "the files results pins are in no cache or mirror here";
    let synced = sandbox.command_in(&teammate, &["sync"]).output()?;
    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains(&format!("warning: {unreached}\n")),
        "{stderr}"
    );
    let checked = sandbox
        .command_in(&teammate, &["status", "--check"])
        .output()?;
    let stderr = text(&checked.stderr)?;
    assert_eq!(checked.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains(&format!(
            "  {unreached}: push them from a machine that has them"
        )),
        "{stderr}"
    );
    assert!(!stderr.contains("replaces it"), "{stderr}");
    assert!(
        !stderr.contains("places missing or updated files"),
        "{stderr}"
    );
    Ok(())
}

/// a teammate of an author who pinned an output's first run, `results`, and pushed it to the team's
/// mirror, from which the teammate's sync placed it; the author then pinned a second run with a
/// cache only they hold, pushed nothing, and the teammate took the new data.lock.
struct Teammate<'s> {
    sandbox: &'s Sandbox,
    folder: PathBuf,
    cache: PathBuf,
    mirror: String,
    /// the author's cache, which alone holds their runs since the first.
    other: PathBuf,
}

impl<'s> Teammate<'s> {
    fn new(sandbox: &'s Sandbox) -> Result<Teammate<'s>> {
        let work = sandbox.work();
        fs::create_dir_all(work.join("results"))?;
        fs::write(work.join("results/a.csv"), b"v1\n")?;
        ok(&sandbox.run(&["add", "results", "--as", "results"])?)?;
        let mirror = sandbox.folder("mirror")?.to_string_lossy().into_owned();
        ok(&sandbox.run(&["push", "--to", &mirror, "--yes"])?)?;
        let folder = sandbox.folder("teammate")?;
        for file in ["data.toml", "data.lock"] {
            fs::copy(work.join(file), folder.join(file))?;
        }
        let teammate = Teammate {
            sandbox,
            folder,
            cache: sandbox.folder("teammate-cache")?,
            mirror,
            other: sandbox.folder("other-cache")?,
        };
        let (code, synced) = teammate.run(&["sync"])?;
        assert_eq!(code, Some(0), "{synced}");
        assert_eq!(fs::read(teammate.folder.join("results/a.csv"))?, b"v1\n");
        teammate.pin(b"v2\n")?;
        Ok(teammate)
    }

    /// `args` run in the teammate's folder with their cache and the mirror set: the exit code and
    /// what the run wrote to stderr.
    fn run(&self, args: &[&str]) -> Result<(Option<i32>, String)> {
        let output = self
            .sandbox
            .command_in(&self.folder, args)
            .env("FETCHLOOM_CACHE", &self.cache)
            .env("FETCHLOOM_MIRRORS", &self.mirror)
            .output()?;
        Ok((output.status.code(), text(&output.stderr)?))
    }

    /// `args` run by the author with the cache that holds their later runs.
    fn author(&self, args: &[&str]) -> Result {
        ok(&self
            .sandbox
            .command(args)
            .env("FETCHLOOM_CACHE", &self.other)
            .output()?)
    }

    /// the author pins `bytes` as the output's next run, and the teammate takes the new data.lock.
    fn pin(&self, bytes: &[u8]) -> Result {
        let work = self.sandbox.work();
        fs::write(work.join("results/a.csv"), bytes)?;
        self.author(&["add", "./results", "--as", "results", "--yes"])?;
        fs::copy(work.join("data.lock"), self.folder.join("data.lock"))?;
        Ok(())
    }
}

/// the c4 case with the team's mirror set: the author pushed the first pin there, pinned a second
/// run and pushed nothing. until a sync asks the mirror, `status --check` names sync, as the
/// mirror may hold the files; once a sync found them in no cache or mirror, status and `status
/// --check` say so in sync's words, so a gate on status stops sending people to a sync that places
/// nothing. a newer pin, which no sync asked the mirror for, names sync again, and once the author
/// pushed it, one sync places it and the check passes.
#[test]
fn status_names_sync_until_a_sync_finds_the_mirror_lacks_the_pin() -> Result {
    let sandbox = Sandbox::new()?;
    let teammate = Teammate::new(&sandbox)?;
    let replaces = "  results/a.csv is from an older data.lock: `fetchloom sync` replaces it\n";
    let unreached = "the files results pins are in no cache or mirror here";
    let (code, before) = teammate.run(&["status", "--check"])?;
    assert_eq!(code, Some(1), "{before}");
    assert!(before.contains(replaces), "{before}");
    assert!(!before.contains(unreached), "{before}");
    let (code, synced) = teammate.run(&["sync"])?;
    assert_eq!(code, Some(0), "{synced}");
    assert!(
        synced.contains(&format!("warning: {unreached}\n")),
        "{synced}"
    );
    let (code, after) = teammate.run(&["status", "--check"])?;
    assert_eq!(code, Some(1), "{after}");
    assert!(
        after.contains(&format!(
            "  {unreached}: push them from a machine that has them"
        )),
        "{after}"
    );
    assert!(!after.contains("replaces it"), "{after}");
    let (_, status) = teammate.run(&["status"])?;
    assert!(
        status.contains(&format!(
            "  hint: {unreached}: push them from a machine that has them"
        )),
        "{status}"
    );
    assert!(
        !status.contains("places missing or updated files"),
        "{status}"
    );
    teammate.pin(b"v3\n")?;
    teammate.author(&["push", "--to", &teammate.mirror, "--yes"])?;
    let (code, pinned) = teammate.run(&["status", "--check"])?;
    assert_eq!(code, Some(1), "{pinned}");
    assert!(pinned.contains(replaces), "{pinned}");
    let (code, placed) = teammate.run(&["sync"])?;
    assert_eq!(code, Some(0), "{placed}");
    assert_eq!(fs::read(teammate.folder.join("results/a.csv"))?, b"v3\n");
    let (code, settled) = teammate.run(&["status", "--check"])?;
    assert_eq!(code, Some(0), "{settled}");
    Ok(())
}

/// the ways on from an output's files a sync found in no cache or mirror here, `pin` the add that
/// pins what its pipeline makes, ending in what `sync --force` does once the files are pushed.
fn lacked_ways(pin: &str, back: &str) -> String {
    format!(
        "the files results pins are in no cache or mirror here, so push them from a machine that has them with `fetchloom push --to <mirror>` and add that mirror here with `fetchloom settings set mirrors <mirror>`, or run your pipeline here, then `{pin}`{back}"
    )
}

/// the c4m case where the teammate then adds a file of their own to the output's folder, which a
/// plain sync leaves whole and asks no mirror about: once a sync found the new pin's files in no
/// cache or mirror, status, sync and an add offer no `sync --force`, which would place nothing,
/// across plain syncs too, and name the way back instead. `sync --force` asks the mirror again,
/// and once the author pushed the pin, places it.
#[test]
fn a_changed_output_a_sync_found_lacking_offers_force_once_pushed() -> Result {
    let sandbox = Sandbox::new()?;
    let teammate = Teammate::new(&sandbox)?;
    let (code, synced) = teammate.run(&["sync"])?;
    assert_eq!(code, Some(0), "{synced}");
    let notes = teammate.folder.join("results/notes.txt");
    fs::write(&notes, b"mine\n")?;
    let back = "; once the files are pushed, `fetchloom sync --force results` makes it hold just the files results pins";
    let ways = lacked_ways("fetchloom add ./results --as results", back);
    let told = format!("  hint: results changed since results was pinned, 1 extra file: {ways}\n");
    let (_, status) = teammate.run(&["status"])?;
    assert!(status.contains(&told), "{status}");
    let (code, synced) = teammate.run(&["sync"])?;
    assert_eq!(code, Some(0), "{synced}");
    assert!(synced.contains(&format!("  help: {ways}\n")), "{synced}");
    let (_, status) = teammate.run(&["status"])?;
    assert!(status.contains(&told), "{status}");
    let (code, added) = teammate.run(&["add", "./results", "--as", "results"])?;
    assert_ne!(code, Some(0), "{added}");
    let yes = lacked_ways("fetchloom add ./results --as results --yes", back);
    assert!(added.contains(&yes), "{added}");
    let (code, forced) = teammate.run(&["sync", "--force", "results"])?;
    assert_eq!(code, Some(0), "{forced}");
    assert!(notes.is_file(), "{forced}");
    let (_, status) = teammate.run(&["status"])?;
    assert!(status.contains(&told), "{status}");
    teammate.author(&["push", "--to", &teammate.mirror, "--yes"])?;
    let (code, forced) = teammate.run(&["sync", "--force", "results"])?;
    assert_eq!(code, Some(0), "{forced}");
    assert_eq!(fs::read(teammate.folder.join("results/a.csv"))?, b"v2\n");
    assert!(!notes.exists(), "{forced}");
    let (code, settled) = teammate.run(&["status", "--check"])?;
    assert_eq!(code, Some(0), "{settled}");
    Ok(())
}

/// as above with the output's folder deleted: status and an add of the folder offer no `sync
/// --force` and name the way back, which puts the pinned files back once the author pushed them.
#[test]
fn a_gone_output_a_sync_found_lacking_names_the_way_back() -> Result {
    let sandbox = Sandbox::new()?;
    let teammate = Teammate::new(&sandbox)?;
    let (code, synced) = teammate.run(&["sync"])?;
    assert_eq!(code, Some(0), "{synced}");
    delete_folder(&teammate.folder.join("results"))?;
    let ways = lacked_ways(
        "fetchloom add ./results --as results",
        ", or if you moved it, set at = \"<new folder>\" for results in data.toml; once the files are pushed, `fetchloom sync --force results` puts back the files results pins",
    );
    let (_, status) = teammate.run(&["status"])?;
    assert!(
        status.contains(&format!("  hint: results is gone: {ways}\n")),
        "{status}"
    );
    let (code, added) = teammate.run(&["add", "./results", "--as", "results"])?;
    assert_ne!(code, Some(0), "{added}");
    assert!(added.contains(&ways), "{added}");
    teammate.author(&["push", "--to", &teammate.mirror, "--yes"])?;
    let (code, forced) = teammate.run(&["sync", "--force", "results"])?;
    assert_eq!(code, Some(0), "{forced}");
    assert_eq!(fs::read(teammate.folder.join("results/a.csv"))?, b"v2\n");
    let (code, settled) = teammate.run(&["status", "--check"])?;
    assert_eq!(code, Some(0), "{settled}");
    Ok(())
}

/// a teammate whose mirror holds an output's files, named by `--mirror` or by the settings, is
/// told of them as of any dataset's missing files, which `fetchloom sync` places, as it does from
/// that mirror; without a mirror, nothing here can, and status says so in the neutral words.
#[test]
fn status_tells_an_output_a_mirror_holds_as_missing_files() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    fs::create_dir_all(work.join("results"))?;
    fs::write(work.join("results/a.csv"), b"v1\n")?;
    fs::write(work.join("results/b.csv"), b"w1\n")?;
    ok(&sandbox.run(&["add", "results", "--as", "results"])?)?;
    let mirror = sandbox.folder("mirror")?;
    let mirror = mirror.to_string_lossy().into_owned();
    ok(&sandbox.run(&["push", "--to", &mirror, "--yes"])?)?;
    let teammate = sandbox.folder("teammate")?;
    for file in ["data.toml", "data.lock"] {
        fs::copy(work.join(file), teammate.join(file))?;
    }
    let cache = sandbox.folder("teammate-cache")?;
    let run = |args: &[&str], mirrors: Option<&str>| -> Result<(Option<i32>, String)> {
        let mut command = sandbox.command_in(&teammate, args);
        command.env("FETCHLOOM_CACHE", &cache);
        match mirrors {
            Some(mirrors) => command.env("FETCHLOOM_MIRRORS", mirrors),
            None => command.env_remove("FETCHLOOM_MIRRORS"),
        };
        let output = command.output()?;
        Ok((output.status.code(), text(&output.stderr)?))
    };
    let sync = "  hint: `fetchloom sync` places missing or updated files and reports any conflicts it keeps\n";
    let unreached = "the files results pins are in no cache or mirror here";
    let (_, flagged) = run(&["--mirror", &mirror, "status"], None)?;
    assert!(flagged.contains(sync), "{flagged}");
    assert!(!flagged.contains(unreached), "{flagged}");
    let (_, set) = run(&["status"], Some(&mirror))?;
    assert!(set.contains(sync), "{set}");
    assert!(!set.contains(unreached), "{set}");
    let (code, checked) = run(&["--mirror", &mirror, "status", "--check"], None)?;
    assert_eq!(code, Some(1), "{checked}");
    for file in ["a.csv", "b.csv"] {
        assert!(
            checked.contains(&format!(
                "  results/{file} is missing: `fetchloom sync` places it\n"
            )),
            "{checked}"
        );
    }
    assert!(!checked.contains(unreached), "{checked}");
    let (_, alone) = run(&["status"], None)?;
    assert!(alone.contains(unreached), "{alone}");
    assert!(!alone.contains(sync), "{alone}");
    let (code, synced) = run(&["--mirror", &mirror, "sync"], None)?;
    assert_eq!(code, Some(0), "{synced}");
    assert_eq!(fs::read(teammate.join("results/a.csv"))?, b"v1\n");
    Ok(())
}

/// a sync that makes the folder of an output made elsewhere, for the pipeline run here, counts it
/// in its last line rather than say it had nothing to do; with the folder there, it has nothing to
/// do.
#[test]
fn a_sync_that_makes_a_waiting_outputs_folder_counts_it() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    processed(&work)?;
    ok(&sandbox.run(&["add", "data/processed", "--as", "processed"])?)?;
    let teammate = sandbox.folder("teammate")?;
    for file in ["data.toml", "data.lock"] {
        fs::copy(work.join(file), teammate.join(file))?;
    }
    let cache = sandbox.folder("teammate-cache")?;
    let sync = || -> Result<String> {
        let output = sandbox
            .command_in(&teammate, &["sync"])
            .env("FETCHLOOM_CACHE", &cache)
            .output()?;
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(0), "{stderr}");
        Ok(stderr)
    };
    let made = "     Synced 1 dataset into data · processed waits for its files\n";
    let first = sync()?;
    assert!(first.contains(made), "{first}");
    let second = sync()?;
    assert!(
        second.contains(
            "         Ok all pinned files but processed's are in place by size and time, nothing to do\n"
        ),
        "{second}"
    );
    delete_folder(&teammate.join("data/processed"))?;
    let again = sync()?;
    assert!(again.contains(made), "{again}");
    Ok(())
}

/// a teammate without an output's bytes is told once per output and pin, by sync and path alike,
/// that no cache or mirror here holds them; each sync's last line still names it, and json says it on
/// every run, as a script reads each run alone. a run of another dataset alone, as a notebook's
/// `path` of it, keeps what was told. a new pin is told of again, and so is an output dropped from
/// data.toml and pinned again with the same files.
#[test]
fn an_output_out_of_reach_is_told_once_per_pin() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    let raw = b"station,rain\nlerwick,3\n";
    let server = Routes::start(vec![
        (PATH.to_owned(), file(IRIS, "\"v1\"")),
        ("/files/raw.csv".to_owned(), file(raw, "\"r1\"")),
    ])?;
    let link = format!("{}{PATH}", server.url);
    sandbox.allow(&link);
    ok(&sandbox.run(&["add", &link])?)?;
    processed(&work)?;
    ok(&sandbox.run(&["add", "data/processed", "--as", "processed"])?)?;
    let teammate = sandbox.folder("teammate")?;
    let cache = sandbox.folder("teammate-cache")?;
    let pull = || -> Result {
        for file in ["data.toml", "data.lock"] {
            fs::copy(work.join(file), teammate.join(file))?;
        }
        Ok(())
    };
    let run = |args: &[&str]| -> Result<String> {
        let output = sandbox
            .command_in(&teammate, args)
            .env("FETCHLOOM_CACHE", &cache)
            .output()?;
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(0), "{args:?}: {stderr}");
        Ok(stderr)
    };
    let said = "warning: the files processed pins are in no cache or mirror here";
    let path: &[&str] = &["path", "processed"];
    let sync: &[&str] = &["sync"];
    pull()?;
    let first = run(path)?;
    assert_eq!(first.matches(said).count(), 1, "{first}");
    for (args, last) in [
        (path, ""),
        (
            sync,
            "Synced 1 dataset into data · processed waits for its files",
        ),
        (
            sync,
            "Ok all pinned files but processed's are in place by size and time, nothing to do",
        ),
    ] {
        let again = run(args)?;
        assert!(!again.contains(said), "{args:?}: {again}");
        assert!(again.contains(last), "{args:?}: {again}");
    }
    let document = json(
        &sandbox
            .command_in(&teammate, &["--json", "sync"])
            .env("FETCHLOOM_CACHE", &cache)
            .output()?,
    )?;
    assert!(
        warning(&document, "output.elsewhere")["message"].is_string(),
        "{document}"
    );
    rerun(&work)?;
    ok(&sandbox.run(&["add", "data/processed", "--as", "processed", "--yes"])?)?;
    pull()?;
    let repinned = run(sync)?;
    assert_eq!(repinned.matches(said).count(), 1, "{repinned}");
    for args in [sync, path] {
        let again = run(args)?;
        assert!(!again.contains(said), "{args:?}: {again}");
    }
    // added after the teammate's syncs, so `path raw` places it, a run of raw alone
    ok(&sandbox.run(&["add", &format!("{}/files/raw.csv", server.url)])?)?;
    pull()?;
    run(&["path", "raw"])?;
    assert_eq!(fs::read(teammate.join("data/raw/raw.csv"))?, raw);
    let quiet = run(sync)?;
    assert!(!quiet.contains(said), "after a run of raw alone: {quiet}");
    ok(&sandbox.run(&["remove", "processed"])?)?;
    pull()?;
    run(sync)?;
    ok(&sandbox.run(&["add", "data/processed", "--as", "processed"])?)?;
    pull()?;
    let restored = run(sync)?;
    assert_eq!(restored.matches(said).count(), 1, "{restored}");
    Ok(())
}

/// an output whose files changed, went or came is the author's work in progress: status names
/// each change, and sync leaves the whole folder as it is, naming the two ways on.
#[test]
fn a_changed_output_is_never_partly_healed() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    processed(&work)?;
    ok(&sandbox.run(&["add", "data/processed", "--as", "processed"])?)?;
    rerun(&work)?;
    let document = json(&sandbox.run(&["--json", "status"])?)?;
    assert_eq!(
        listed(&document, "edited"),
        ["data/processed/iris_clean.csv"]
    );
    assert_eq!(listed(&document, "missing"), ["data/processed/summary.txt"]);
    assert_eq!(listed(&document, "extra"), ["data/processed/plot.png"]);
    let synced = sandbox.run(&["--json", "sync"])?;
    let document = json(&synced)?;
    assert_eq!(synced.status.code(), Some(0), "{document}");
    let warning = warned(&document)
        .into_iter()
        .find(|said| said.contains("processed"))
        .unwrap_or_default();
    assert!(
        warning.contains("data/processed changed since processed was pinned"),
        "{document}"
    );
    let said = document["warnings"].to_string();
    assert!(
        said.contains(
            "`fetchloom sync --force processed` makes it hold just the files processed pins"
        ),
        "{said}"
    );
    assert!(
        said.contains("fetchloom add ./data/processed --as processed"),
        "{said}"
    );
    let folder = work.join("data/processed");
    assert!(!folder.join("summary.txt").exists(), "never put back alone");
    assert_eq!(fs::read(folder.join("plot.png"))?, b"png");
    assert_eq!(
        fs::read(folder.join("iris_clean.csv"))?,
        b"sepal,petal\n5.1,1.4\n4.9,1.4\n"
    );
    let removing = "data/processed/plot.png · a file processed's pin does not hold";
    let previewed = sandbox.run(&["sync", "--force", "processed", "--dry-run"])?;
    let stderr = text(&previewed.stderr)?;
    assert_eq!(previewed.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains(&format!("Would remove {removing}\n")),
        "{stderr}"
    );
    assert!(!stderr.contains("git tracks"), "{stderr}");
    let document = json(&sandbox.run(&["--json", "sync", "--force", "processed", "-n"])?)?;
    assert_eq!(
        document["result"]["changes"]["remove"],
        serde_json::json!(["data/processed/plot.png"]),
        "{document}"
    );
    assert!(
        folder.join("plot.png").exists(),
        "a dry run deletes nothing"
    );
    let forced = sandbox.run(&["sync", "--force", "processed"])?;
    let stderr = text(&forced.stderr)?;
    assert_eq!(forced.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains(&format!("Removed {removing}\n")),
        "{stderr}"
    );
    assert!(!stderr.contains("that no dataset needs"), "{stderr}");
    assert!(!stderr.contains("git tracks"), "{stderr}");
    let help = text(&sandbox.run(&["sync", "--help"])?.stdout)?;
    let help = help.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        help.contains(
            "in an output's folder, also deletes each file data.lock does not list for it"
        ),
        "{help}"
    );
    assert_eq!(fs::read(folder.join("iris_clean.csv"))?, CLEAN);
    assert_eq!(fs::read(folder.join("summary.txt"))?, SUMMARY);
    assert!(!folder.join("plot.png").exists(), "the pinned tree, whole");
    assert_eq!(crate::support::marker(&folder.join(".gitignore"))?, b"*\n");
    let document = json(&sandbox.run(&["--json", "status"])?)?;
    for kind in ["missing", "edited", "extra"] {
        assert_eq!(
            listed(&document, kind),
            Vec::<String>::new(),
            "{kind}: {document}"
        );
    }
    Ok(())
}

/// the author switches git branches between two pinned runs of a pipeline: data.lock pins run 1
/// again while the folder holds run 2. sync renews no part of it, as that would leave both runs
/// mixed in one folder; it says the output changed, as status does, and `sync --force` restores
/// run 1 exactly.
#[test]
fn a_branch_switch_between_two_pinned_runs_mixes_no_runs() -> Result {
    let sandbox = Sandbox::new()?;
    if crate::merge::ok(&sandbox, &["init", "-q"])?.is_none() {
        return Ok(());
    }
    let work = sandbox.work();
    let folder = work.join("data/processed");
    fs::create_dir_all(&folder)?;
    fs::write(folder.join("clean.csv"), b"v1,a\n")?;
    fs::write(folder.join("summary.txt"), b"v1\n")?;
    ok(&sandbox.run(&["add", "data/processed", "--as", "processed"])?)?;
    let git = |args: &[&str]| -> Result {
        crate::merge::ok(&sandbox, args)?;
        Ok(())
    };
    git(&["add", "-A"])?;
    git(&["commit", "-qm", "run 1"])?;
    git(&["checkout", "-qb", "run2"])?;
    fs::write(folder.join("clean.csv"), b"v2,bb\n")?;
    fs::remove_file(folder.join("summary.txt"))?;
    fs::write(folder.join("fig.txt"), b"v2\n")?;
    ok(&sandbox.run(&["add", "data/processed", "--as", "processed", "--yes"])?)?;
    git(&["commit", "-qam", "run 2"])?;
    git(&["checkout", "-q", "-"])?;
    let held = || -> Result<Vec<(String, Vec<u8>)>> {
        let mut held = Vec::new();
        for entry in fs::read_dir(&folder)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name != ".gitignore" {
                held.push((name, fs::read(entry.path())?));
            }
        }
        held.sort();
        Ok(held)
    };
    let run2 = held()?;
    let checked = sandbox.run(&["status", "--check"])?;
    let said = text(&checked.stderr)?;
    assert_eq!(checked.status.code(), Some(1), "{said}");
    let synced = sandbox.run(&["sync"])?;
    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(0), "{stderr}");
    let changes = "1 edited file, 1 extra file, 1 missing file";
    assert!(
        said.contains(&format!(
            "data/processed changed since processed was pinned, {changes}"
        )),
        "{said}"
    );
    assert!(
        stderr.contains(&format!(
            "warning: data/processed changed since processed was pinned: {changes}, so sync leaves it as it is"
        )),
        "{stderr}"
    );
    assert_eq!(held()?, run2, "no part of run 1 placed among run 2");
    ok(&sandbox.run(&["sync", "--force", "processed"])?)?;
    let run1 = [
        ("clean.csv".to_owned(), b"v1,a\n".to_vec()),
        ("summary.txt".to_owned(), b"v1\n".to_vec()),
    ];
    assert_eq!(held()?, run1);
    ok(&sandbox.run(&["status", "--check"])?)?;
    Ok(())
}

/// an output's extra files, what `sync --force` deletes as its snapshot lacks them, are those no
/// record names: a person's file a forced run kept aside there, a file another dataset's record
/// names and one the output's own record names that changed since all stay, while the file no one
/// recorded goes. status lists another dataset's file as no change to the output, and counts the
/// change as sync does.
#[test]
fn force_on_an_output_keeps_a_file_kept_aside_and_another_datasets_file() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    processed(&work)?;
    ok(&sandbox.run(&["add", "data/processed", "--as", "processed"])?)?;
    let folder = work.join("data/processed");
    let hidden = ".fetchloom-0123456789abcdef.old";
    for (name, bytes) in [
        (hidden, "my notes\n"),
        ("theirs.csv", "theirs\n"),
        ("old.csv", "old, changed\n"),
        ("plot.png", "png"),
    ] {
        fs::write(folder.join(name), bytes)?;
    }
    let state_path = work.join(".fetchloom/state");
    let mut state: serde_json::Value = serde_json::from_slice(&fs::read(&state_path)?)?;
    // a forced run cut off left the person's file aside, its own name taken since
    state["aside"] = serde_json::json!({
        format!("data/processed/{hidden}"): "data/processed/iris_clean.csv"
    });
    for (path, dataset) in [("theirs.csv", "raw"), ("old.csv", "processed")] {
        state["files"][format!("data/processed/{path}")] = serde_json::json!({
            "dataset": dataset,
            "sha256": "0".repeat(64),
            "method": "copy",
            "look": { "size": 1, "modified": 1 },
            "writable": false,
            "checked": 0
        });
    }
    fs::write(&state_path, serde_json::to_vec(&state)?)?;
    let document = json(&sandbox.run(&["--json", "status"])?)?;
    assert_eq!(
        listed(&document, "extra"),
        [
            format!("data/processed/{hidden}"),
            "data/processed/old.csv".to_owned(),
            "data/processed/plot.png".to_owned(),
        ],
        "{document}"
    );
    let changes = "1 edited file, 1 extra file";
    let dry = text(&sandbox.run(&["sync", "--dry-run"])?.stderr)?;
    assert!(
        dry.contains(&format!("warning: data/processed changed since processed was pinned: {changes}, so sync leaves it as it is")),
        "{dry}"
    );
    let checked = json(&sandbox.run(&["--json", "status", "--check"])?)?;
    assert_eq!(
        checked["errors"][0]["details"],
        serde_json::json!([format!(
            "data/processed changed since processed was pinned, {changes}: `fetchloom add ./data/processed --as processed` pins what it holds now, or `fetchloom sync --force processed` makes it hold just the files processed pins"
        )]),
        "{checked}"
    );
    let forced = sandbox.run(&["sync", "--force", "processed"])?;
    let stderr = text(&forced.stderr)?;
    assert_eq!(forced.status.code(), Some(0), "{stderr}");
    assert!(!folder.join("plot.png").exists(), "{stderr}");
    let kept = |name: &str| fs::read(folder.join(name)).ok();
    assert_eq!(
        (kept(hidden), kept("theirs.csv"), kept("old.csv")),
        (
            Some(b"my notes\n".to_vec()),
            Some(b"theirs\n".to_vec()),
            Some(b"old, changed\n".to_vec())
        ),
        "{stderr}"
    );
    Ok(())
}

/// a teammate's clone holds no file of an output before its first sync: status says sync places
/// them, never that the output changed, and never offers to pin the empty folder.
#[test]
fn a_clone_before_its_first_sync_has_an_output_to_place_not_a_changed_one() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    processed(&work)?;
    ok(&sandbox.run(&["add", "data/processed", "--as", "processed"])?)?;
    let clone = sandbox.folder("clone")?;
    for name in ["data.toml", "data.lock"] {
        fs::copy(work.join(name), clone.join(name))?;
    }
    let checked = sandbox
        .command_in(&clone, &["status", "--check"])
        .output()?;
    let stderr = text(&checked.stderr)?;
    assert_eq!(checked.status.code(), Some(1), "{stderr}");
    assert!(!stderr.contains("changed since"), "{stderr}");
    assert!(
        stderr.contains("data/processed/iris_clean.csv is missing: `fetchloom sync` places it"),
        "{stderr}"
    );
    assert!(
        stderr.contains("help: run `fetchloom sync` to put them in place"),
        "{stderr}"
    );
    Ok(())
}

/// a file a person adds to an output's folder, and nothing else, changes the output for sync as for
/// status: sync warns and leaves it, `--strict` fails as `status --check` does, and `--force`
/// deletes the file, as its dry run says, though a sync before found nothing else to do.
#[test]
fn a_file_added_to_an_outputs_folder_changes_it_for_sync_too() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    processed(&work)?;
    ok(&sandbox.run(&["add", "data/processed", "--as", "processed"])?)?;
    ok(&sandbox.run(&["sync"])?)?;
    let notes = work.join("data/processed/NOTES.md");
    fs::write(&notes, b"# notes\n")?;
    let checked = sandbox.run(&["status", "--check"])?;
    assert_eq!(checked.status.code(), Some(1), "{}", text(&checked.stderr)?);
    let synced = sandbox.run(&["sync"])?;
    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("warning: data/processed changed since processed was pinned: 1 extra file, so sync leaves it as it is"),
        "{stderr}"
    );
    let strict = sandbox.run(&["--json", "sync", "--strict"])?;
    let document = json(&strict)?;
    assert_eq!(strict.status.code(), Some(1), "{document}");
    assert_eq!(
        document["errors"][0]["kind"], "data.not_ready",
        "{document}"
    );
    let previewed = sandbox.run(&["sync", "--force", "processed", "--dry-run"])?;
    let stderr = text(&previewed.stderr)?;
    assert!(
        stderr.contains(
            "Would remove data/processed/NOTES.md · a file processed's pin does not hold\n"
        ),
        "{stderr}"
    );
    assert!(notes.exists(), "a dry run deletes nothing");
    ok(&sandbox.run(&["sync", "--force", "processed"])?)?;
    assert!(!notes.exists());
    ok(&sandbox.run(&["status", "--check"])?)?;
    Ok(())
}

/// a sync that leaves a changed output whole says so in its closing line, as it says it kept an
/// edit, and so does a dry run: neither says every pinned file is in place while an output
/// differs from its pin.
#[test]
fn a_sync_leaving_a_changed_output_never_says_all_is_in_place() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    processed(&work)?;
    ok(&sandbox.run(&["add", "data/processed", "--as", "processed"])?)?;
    fs::write(
        work.join("data/processed/iris_clean.csv"),
        b"sepal,petal\n5.1,1.5\n",
    )?;
    let warning = "warning: data/processed changed since processed was pinned";
    for args in [&["sync", "--dry-run"][..], &["sync"]] {
        let synced = sandbox.run(args)?;
        let stderr = text(&synced.stderr)?;
        assert_eq!(synced.status.code(), Some(0), "{stderr}");
        assert!(stderr.contains(warning), "{args:?}: {stderr}");
        assert!(!stderr.contains("in place"), "{args:?}: {stderr}");
        if args.len() == 1 {
            let closing = ", processed left as it is; see the warnings above\n";
            assert!(stderr.contains(closing), "{stderr}");
        }
    }
    Ok(())
}

/// after the pipeline reruns, status names the add that pins the rerun first and `--force` as
/// what discards it, never a plain sync, which leaves a changed output whole.
#[test]
fn status_after_a_rerun_names_the_pin_first() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    processed(&work)?;
    ok(&sandbox.run(&["add", "data/processed", "--as", "processed"])?)?;
    rerun(&work)?;
    let status = sandbox.run(&["status"])?;
    let stderr = text(&status.stderr)?;
    let hints: Vec<&str> = stderr
        .lines()
        .filter_map(|line| line.trim_start().strip_prefix("hint: "))
        .collect();
    assert_eq!(
        hints,
        [
            "data/processed changed since processed was pinned, 1 edited file, 1 extra file, 1 missing file: `fetchloom add ./data/processed --as processed` pins what it holds now, or `fetchloom sync --force processed` makes it hold just the files processed pins"
        ],
        "{stderr}"
    );
    Ok(())
}

/// a changed output is told of once per change, as a kept edit is: a later sync finding the same
/// change, or its dry run, only counts it in the last line, in json too, also after runs that left
/// the output out, and a new change is told of again, as is a change after the output was pinned
/// again.
#[test]
fn a_changed_output_is_told_once_per_change() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    processed(&work)?;
    ok(&sandbox.run(&["add", "data/processed", "--as", "processed"])?)?;
    let clean = work.join("data/processed/iris_clean.csv");
    fs::write(&clean, b"sepal,petal\n5.1,1.5\n")?;
    let told = |args: &[&str]| -> Result<(bool, String)> {
        let synced = sandbox.run(args)?;
        let stderr = text(&synced.stderr)?;
        assert_eq!(synced.status.code(), Some(0), "{args:?}: {stderr}");
        let said = if args.contains(&"--json") {
            json(&synced)?["warnings"].to_string()
        } else {
            stderr.clone()
        };
        Ok((said.contains("changed since processed was pinned"), stderr))
    };
    let (said, stderr) = told(&["sync"])?;
    assert!(said, "{stderr}");
    // runs that leave the output out keep what was told of it
    let flowers = sandbox.folder("flowers")?;
    fs::write(flowers.join("sepals.csv"), b"5.1\n")?;
    ok(&sandbox.run(&["add", "../flowers"])?)?;
    ok(&sandbox.run(&["sync", "flowers"])?)?;
    for args in [&["sync"][..], &["sync", "--dry-run"], &["--json", "sync"]] {
        let (said, stderr) = told(args)?;
        assert!(!said, "{args:?} told it again: {stderr}");
        if args.len() == 1 {
            assert!(stderr.contains(", processed left as it is\n"), "{stderr}");
        }
    }
    // a size every clock tells apart, as a coarse one may give both writes one time
    fs::write(&clean, b"sepal,petal\n5.1,1.5\n4.9,1.4\n")?;
    let (said, stderr) = told(&["sync"])?;
    assert!(said, "a new change: {stderr}");
    ok(&sandbox.run(&["add", "data/processed", "--as", "processed", "--yes"])?)?;
    ok(&sandbox.run(&["sync"])?)?;
    fs::write(&clean, b"sepal,petal\n5.1,1.6\n")?;
    let (said, stderr) = told(&["sync"])?;
    assert!(said, "changed after a pin: {stderr}");
    Ok(())
}

/// after a rerun, the same add shows what changed and pins it in one step with --yes, keeping
/// the table's comments and settings; without a terminal or --yes it changes nothing.
#[test]
fn the_same_add_pins_a_rerun_in_one_step_keeping_the_table() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    processed(&work)?;
    let table = "# made by src/dataset.py\n[datasets.processed]\nref = \"local\"\nat = \"data/processed\"\nexclude = \"*.log\" # logs stay out\n";
    fs::write(work.join("data.toml"), table)?;
    ok(&sandbox.run(&["add", "data/processed", "--as", "processed"])?)?;
    assert_eq!(fs::read_to_string(work.join("data.toml"))?, table);
    let pinned = fs::read_to_string(work.join("data.lock"))?;
    rerun(&work)?;
    fs::write(work.join("data/processed/run.log"), b"log")?;
    let asked = sandbox.run(&["add", "data/processed", "--as", "processed"])?;
    let stderr = text(&asked.stderr)?;
    assert_eq!(asked.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("error: data/processed holds other files than processed pins"),
        "{stderr}"
    );
    for line in [
        "edited iris_clean.csv",
        "extra plot.png",
        "missing summary.txt",
    ] {
        assert!(stderr.contains(line), "{line}: {stderr}");
    }
    assert!(
        !stderr.contains("run.log"),
        "an excluded file is no change: {stderr}"
    );
    assert!(
        stderr.contains("  help: `fetchloom add ./data/processed --as processed --yes` pins what it holds now, or `fetchloom sync --force processed` makes it hold just the files processed pins\n"),
        "{stderr}"
    );
    assert_eq!(fs::read_to_string(work.join("data.lock"))?, pinned);
    let output = sandbox.run(&["add", "data/processed", "--as", "processed", "--yes"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains(", the cache held the bytes already; data/processed stays where it is\n"),
        "{stderr}"
    );
    assert!(
        stderr.contains("Pinned processed · 1 edited file, 1 extra file, 1 missing file"),
        "{stderr}"
    );
    assert_eq!(fs::read_to_string(work.join("data.toml"))?, table);
    let lock = fs::read_to_string(work.join("data.lock"))?;
    assert!(lock.contains("path = \"plot.png\""), "{lock}");
    assert!(!lock.contains("summary.txt"), "{lock}");
    assert!(!lock.contains("run.log"), "{lock}");
    let document = json(&sandbox.run(&["--json", "status"])?)?;
    for kind in ["missing", "edited", "extra"] {
        assert_eq!(
            listed(&document, kind),
            Vec::<String>::new(),
            "{kind}: {document}"
        );
    }
    Ok(())
}

/// lock never reads an output's folder: an upgrade of one is refused with the one step that pins
/// what its folder holds now, and an upgrade of every dataset passes it by, saying so.
#[test]
fn lock_upgrade_names_the_add_that_pins_an_output_again() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    processed(&work)?;
    ok(&sandbox.run(&["add", "data/processed", "--as", "processed"])?)?;
    let pinned = fs::read_to_string(work.join("data.lock"))?;
    rerun(&work)?;
    let refused = sandbox.run(&["--json", "lock", "--upgrade", "processed"])?;
    let document = json(&refused)?;
    assert_eq!(refused.status.code(), Some(2), "{document}");
    let error = &document["errors"][0];
    assert_eq!(error["kind"], "usage", "{document}");
    assert_eq!(
        error["message"], "processed is an output, so no source has a newer version of it",
        "{document}"
    );
    assert_eq!(
        error["help"],
        "run `fetchloom add ./data/processed --as processed` to pin what it holds now",
        "{document}"
    );
    let every = sandbox.run(&["lock", "--upgrade"])?;
    let stderr = text(&every.stderr)?;
    assert_eq!(every.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains(
            "Skipped processed, an output, which `fetchloom add ./data/processed --as processed` pins again"
        ),
        "{stderr}"
    );
    assert_eq!(fs::read_to_string(work.join("data.lock"))?, pinned);
    Ok(())
}

/// an output's table changed in data.toml, as a new exclude, is pinned by the same add, which
/// reads the folder by the new table and keeps it as written: sync, lock, status and both checks
/// name that one step, never a removal that would lose the table.
#[test]
fn a_changed_outputs_table_is_pinned_again_by_its_add() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    processed(&work)?;
    ok(&sandbox.run(&["add", "data/processed", "--as", "processed"])?)?;
    let manifest = fs::read_to_string(work.join("data.toml"))?;
    let table = manifest.replace(
        "at = \"data/processed\"\n",
        "at = \"data/processed\"\n# the clean table alone\nexclude = \"*.txt\"\n",
    );
    assert_ne!(table, manifest);
    fs::write(work.join("data.toml"), &table)?;
    let add = "`fetchloom add ./data/processed --as processed`";
    for args in [
        &["sync"][..],
        &["lock"],
        &["status"],
        &["status", "--check"],
        &["lock", "--check"],
    ] {
        let said = sandbox.run(args)?;
        let stderr = text(&said.stderr)?;
        assert!(stderr.contains(add), "{args:?}: {stderr}");
        assert!(!stderr.contains("remove processed"), "{args:?}: {stderr}");
        assert!(
            !stderr.contains("imported from files"),
            "{args:?}: {stderr}"
        );
    }
    let repinned = sandbox.run(&["add", "data/processed", "--as", "processed", "--yes"])?;
    let stderr = text(&repinned.stderr)?;
    assert_eq!(repinned.status.code(), Some(0), "{stderr}");
    assert_eq!(fs::read_to_string(work.join("data.toml"))?, table);
    let lock = fs::read_to_string(work.join("data.lock"))?;
    assert!(!lock.contains("summary.txt"), "{lock}");
    let checked = sandbox.run(&["status", "--check"])?;
    assert_eq!(checked.status.code(), Some(0), "{}", text(&checked.stderr)?);
    Ok(())
}

/// after a pipeline run, `status --verify --check` is the check a script runs on an output: it
/// hashes every file, so a rewrite that kept size and time shows, and it fails while the folder
/// holds other files than the output pins, naming the output once with what changed, as `sync
/// --strict --verify` does, which heals no part of it. a dry run of the same add shows what pinning
/// it again would change. none of them writes data.toml or data.lock.
#[test]
fn status_check_and_a_dry_run_judge_an_outputs_whole_folder() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    processed(&work)?;
    let clean = work.join("data/processed/iris_clean.csv");
    // an old time every disk keeps as given: on a coarse clock a file as new as the state is
    // hashed, which would find the rewrite without --verify
    let kept = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
    let keep_time = || {
        fs::File::options()
            .write(true)
            .open(&clean)?
            .set_modified(kept)
    };
    keep_time()?;
    ok(&sandbox.run(&["add", "data/processed", "--as", "processed"])?)?;
    let pinned = [
        fs::read_to_string(work.join("data.toml"))?,
        fs::read_to_string(work.join("data.lock"))?,
    ];
    let check = ["--json", "status", "--verify", "--check"];
    let same = sandbox.run(&check)?;
    assert_eq!(same.status.code(), Some(0), "{}", text(&same.stdout)?);
    fs::write(&clean, b"sepal,petal\n5.1,1.5\n")?;
    keep_time()?;
    let unhashed = sandbox.run(&["status", "--check"])?;
    assert_eq!(
        unhashed.status.code(),
        Some(0),
        "only hashing tells: {}",
        text(&unhashed.stderr)?
    );
    let strict = sandbox.run(&["--json", "sync", "--strict", "--verify"])?;
    let synced = json(&strict)?;
    assert_eq!(strict.status.code(), Some(1), "{synced}");
    assert_eq!(synced["errors"][0]["kind"], "data.not_ready", "{synced}");
    assert_eq!(
        synced["errors"][0]["details"],
        serde_json::json!([
            "data/processed changed since processed was pinned, 1 edited file: `fetchloom add ./data/processed --as processed` pins what it holds now, or `fetchloom sync --force processed` makes it hold just the files processed pins"
        ]),
        "{synced}"
    );
    assert_eq!(
        fs::read(&clean)?,
        b"sepal,petal\n5.1,1.5\n",
        "never partly healed"
    );
    fs::remove_file(work.join("data/processed/summary.txt"))?;
    fs::write(work.join("data/processed/plot.png"), b"png")?;
    let refused = sandbox.run(&check)?;
    let document = json(&refused)?;
    assert_eq!(refused.status.code(), Some(1), "{document}");
    let error = &document["errors"][0];
    assert_eq!(error["kind"], "data.not_ready", "{document}");
    assert_eq!(
        error["details"],
        serde_json::json!([
            "data/processed changed since processed was pinned, 1 edited file, 1 extra file, 1 missing file: `fetchloom add ./data/processed --as processed` pins what it holds now, or `fetchloom sync --force processed` makes it hold just the files processed pins"
        ]),
        "{document}"
    );
    let dry = sandbox.run(&["add", "data/processed", "--as", "processed", "--dry-run"])?;
    let stderr = text(&dry.stderr)?;
    assert_eq!(dry.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("Would pin processed · 1 edited file, 1 extra file, 1 missing file"),
        "{stderr}"
    );
    let listed = sandbox.run(&["-v", "add", "data/processed", "--as", "processed", "-n"])?;
    let stderr = text(&listed.stderr)?;
    for line in [
        "edited iris_clean.csv",
        "extra plot.png",
        "missing summary.txt",
    ] {
        assert!(stderr.contains(line), "{line}: {stderr}");
    }
    assert_eq!(fs::read_to_string(work.join("data.toml"))?, pinned[0]);
    assert_eq!(fs::read_to_string(work.join("data.lock"))?, pinned[1]);
    Ok(())
}

fn count_files(folder: &Path) -> usize {
    let Ok(entries) = fs::read_dir(folder) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| {
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                count_files(&entry.path())
            } else {
                1
            }
        })
        .sum()
}

/// every file in an output's folder is the output's: an add never takes in another dataset's
/// files, nor makes an output of the data folder, of the project or of a folder data.toml refuses,
/// and changes nothing then.
#[test]
fn an_output_never_takes_in_another_datasets_files() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    fs::create_dir_all(work.join("results/figures"))?;
    fs::write(work.join("results/r.csv"), b"r\n")?;
    fs::write(work.join("results/figures/f.png"), b"png")?;
    for folder in [".husky", "data/raw"] {
        fs::create_dir_all(work.join(folder))?;
        fs::write(work.join(folder).join("x.csv"), b"x\n")?;
    }
    ok(&sandbox.run(&["add", "results", "--as", "results"])?)?;
    let (toml, lock) = (
        fs::read_to_string(work.join("data.toml"))?,
        fs::read_to_string(work.join("data.lock"))?,
    );
    for (folder, said) in [
        (
            "results/figures",
            "error: results/figures is in results, the folder of the output results, where every file is the output's",
        ),
        (
            ".",
            "error: . holds the whole project, so it cannot be an output's folder",
        ),
        (
            ".husky",
            "error: .husky cannot be an output's folder: .husky is a folder other tools run files from",
        ),
        (
            "data",
            "error: data holds the data folder data, where fetched datasets land, so it cannot be an output's folder",
        ),
    ] {
        let refused = sandbox.run(&["add", folder, "--as", "other"])?;
        let stderr = text(&refused.stderr)?;
        assert_eq!(refused.status.code(), Some(2), "{folder}: {stderr}");
        assert!(stderr.contains(said), "{folder}: {stderr}");
        assert_eq!(fs::read_to_string(work.join("data.toml"))?, toml);
        assert_eq!(fs::read_to_string(work.join("data.lock"))?, lock);
    }
    Ok(())
}

/// a pipeline runs one add after each of its runs, so the same command pins the output again
/// every time: the --select and --exclude it gave the first time are data.toml's then, and
/// --no-extract holds for every output, which is never unpacked. patterns other than data.toml's
/// are refused, with advice about those two alone, before anything is written.
#[test]
fn a_pipelines_unchanged_add_pins_its_output_on_every_run() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    processed(&work)?;
    fs::write(work.join("data/processed/run.log"), b"log\n")?;
    let add = |patterns: &str| -> Result {
        let add = format!("add data/processed --as processed {patterns} --no-extract --yes");
        ok(&sandbox.run(&add.split(' ').collect::<Vec<_>>())?)
    };
    add("--exclude *.log --exclude *.tmp")?;
    rerun(&work)?;
    // a pattern's place in the line changes nothing
    add("--exclude *.tmp --exclude *.log")?;
    let lock = fs::read_to_string(work.join("data.lock"))?;
    assert!(lock.contains("{ path = \"plot.png\""), "{lock}");
    assert!(
        !lock.contains("summary.txt") && !lock.contains("run.log"),
        "{lock}"
    );
    let pinned = [fs::read_to_string(work.join("data.toml"))?, lock];
    fs::write(work.join("data/processed/extra.csv"), b"x\n")?;
    let other = [
        "add",
        "data/processed",
        "--as",
        "processed",
        "--exclude",
        "*.csv",
    ];
    let refused = sandbox.run(&other)?;
    let stderr = text(&refused.stderr)?;
    assert_eq!(refused.status.code(), Some(2), "{stderr}");
    assert!(
        stderr.contains(
            "error: processed is in data.toml already, with other --select and --exclude patterns than these\n  help: leave out --select and --exclude to keep data.toml's, or change them in datasets.processed in data.toml\n"
        ),
        "{stderr}"
    );
    assert_eq!(fs::read_to_string(work.join("data.toml"))?, pinned[0]);
    assert_eq!(fs::read_to_string(work.join("data.lock"))?, pinned[1]);
    Ok(())
}

/// an output keeps the files its pipeline wrote, so `--decompress`, which places single compressed
/// files unpacked, is refused for one, new or pinned, before anything is written, and so is
/// `--web-page`, which keeps the page a link answers with: a folder of the project is no link.
#[test]
fn an_output_takes_no_decompress_or_web_page() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    processed(&work)?;
    let add = ["add", "data/processed", "--as", "processed"];
    let flags = [
        ("--decompress", "kept as its pipeline wrote it"),
        ("--web-page", "a folder of this project rather than a link"),
    ];
    let refused = |flag: &str, why: &str| -> Result {
        let refused = sandbox.run(&[&add[..], &[flag]].concat())?;
        let stderr = text(&refused.stderr)?;
        assert_eq!(refused.status.code(), Some(2), "{stderr}");
        let said = format!(
            "error: data/processed is an output, {why}, so it takes no {flag}\n  help: leave out {flag}\n"
        );
        assert!(stderr.contains(&said), "{stderr}");
        Ok(())
    };
    for (flag, why) in flags {
        refused(flag, why)?;
        assert!(!work.join("data.toml").exists(), "{flag}");
    }
    ok(&sandbox.run(&add)?)?;
    let pinned = [
        fs::read_to_string(work.join("data.toml"))?,
        fs::read_to_string(work.join("data.lock"))?,
    ];
    for (flag, why) in flags {
        refused(flag, why)?;
        assert_eq!(fs::read_to_string(work.join("data.toml"))?, pinned[0]);
        assert_eq!(fs::read_to_string(work.join("data.lock"))?, pinned[1]);
    }
    Ok(())
}

/// a pipeline on a cluster writes data/processed through the data folder's link onto scratch,
/// which `data_links` lists, once an import put the raw files there: the output is pinned where it
/// is, by either spelling, as one in a real data folder is. reading the folder's link takes
/// nothing over, so the import's own sync records the link's first follow, and what the pipeline
/// writes there later is the person's.
#[test]
fn an_output_in_a_linked_data_folder_is_pinned_where_it_is() -> Result {
    let sandbox = Sandbox::new()?;
    let scratch = sandbox.folder("scratch")?;
    let target = scratch.join("project-data");
    fs::create_dir(&target)?;
    let work = sandbox.work();
    if link_folder(&target, &work.join("data")).is_err() {
        return skipped("symlinks", "no symbolic link can be made here");
    }
    let listed = scratch.display().to_string();
    ok(&sandbox.run(&["settings", "set", "data_links", &listed])?)?;
    let raw = sandbox.folder("raw")?;
    fs::write(raw.join("iris.csv"), IRIS)?;
    ok(&sandbox.run(&["add", "../raw"])?)?;
    assert_eq!(fs::read(target.join("raw/iris.csv"))?, IRIS);
    processed(&work)?;
    for typed in ["data/processed", "./data/processed"] {
        let pinned = sandbox.run(&["add", typed, "--as", "processed"])?;
        let stderr = text(&pinned.stderr)?;
        assert_eq!(pinned.status.code(), Some(0), "{typed}: {stderr}");
    }
    let manifest = fs::read_to_string(work.join("data.toml"))?;
    assert!(manifest.contains("at = \"data/processed\""), "{manifest}");
    assert_eq!(fs::read(target.join("processed/iris_clean.csv"))?, CLEAN);
    let checked = sandbox.run(&["status", "--verify", "--check"])?;
    assert_eq!(checked.status.code(), Some(0), "{}", text(&checked.stderr)?);
    Ok(())
}

/// the pipeline runs first and its output is pinned next, the first run to follow a fresh data
/// folder link: the capture claims the files of the folder it pins, so the link is taken with
/// them, and later runs follow it without a word. files of no dataset beside that folder, as the
/// pipeline's notes and an older run's, are the person's, and stay as they are.
#[test]
fn a_capture_may_be_the_first_to_follow_a_data_link() -> Result {
    let sandbox = Sandbox::new()?;
    let scratch = sandbox.folder("scratch")?;
    let target = scratch.join("project-data");
    fs::create_dir(&target)?;
    let work = sandbox.work();
    if link_folder(&target, &work.join("data")).is_err() {
        return skipped("symlinks", "no symbolic link can be made here");
    }
    let listed = scratch.display().to_string();
    ok(&sandbox.run(&["settings", "set", "data_links", &listed])?)?;
    processed(&work)?;
    let pinned = sandbox.run(&["add", "./data/processed", "--as", "processed"])?;
    let stderr = text(&pinned.stderr)?;
    assert_eq!(pinned.status.code(), Some(0), "{stderr}");
    assert_eq!(fs::read(target.join("processed/iris_clean.csv"))?, CLEAN);
    for args in [&["status", "--verify", "--check"][..], &["sync"]] {
        let quiet = sandbox.run(args)?;
        let stderr = text(&quiet.stderr)?;
        assert_eq!(quiet.status.code(), Some(0), "{args:?}: {stderr}");
        assert!(!stderr.contains("did not place"), "{args:?}: {stderr}");
    }
    let other = sandbox.folder("other")?;
    let elsewhere = scratch.join("other-data");
    fs::create_dir(&elsewhere)?;
    link_folder(&elsewhere, &other.join("data"))?;
    processed(&other)?;
    fs::write(elsewhere.join("notes.txt"), b"no dataset's")?;
    fs::create_dir(elsewhere.join("processed-old"))?;
    fs::write(elsewhere.join("processed-old/run.log"), b"an older run")?;
    let beside = sandbox
        .command_in(&other, &["add", "./data/processed", "--as", "processed"])
        .output()?;
    assert_eq!(beside.status.code(), Some(0), "{}", text(&beside.stderr)?);
    assert_eq!(fs::read(elsewhere.join("processed/iris_clean.csv"))?, CLEAN);
    assert_eq!(fs::read(elsewhere.join("notes.txt"))?, b"no dataset's");
    assert_eq!(
        fs::read(elsewhere.join("processed-old/run.log"))?,
        b"an older run"
    );
    Ok(())
}

/// a cloned data.toml chooses where an output lands, so `at` is held to the data folder's rules
/// and kept apart from the cache, before anything is written.
#[test]
fn a_cloned_outputs_folder_is_held_to_the_data_folders_rules() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    fs::write(
        work.join("data.toml"),
        "[datasets.out]\nref = \"local\"\nat = \".husky\"\n",
    )?;
    let output = sandbox.run(&["sync"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(50), "{stderr}");
    assert!(
        stderr.contains(
            "error: at = \".husky\" in data.toml is refused because .husky is a folder other tools run files from"
        ),
        "{stderr}"
    );
    assert!(!work.join(".husky").exists());
    fs::write(
        work.join("data.toml"),
        "[datasets.out]\nref = \"local\"\nat = \".fetchloom-cache/objects\"\n",
    )?;
    let cached = sandbox
        .command(&["sync"])
        .env("FETCHLOOM_CACHE", work.join(".fetchloom-cache"))
        .output()?;
    let stderr = text(&cached.stderr)?;
    assert_eq!(cached.status.code(), Some(50), "{stderr}");
    assert!(
        stderr.contains(".fetchloom-cache/objects is inside the cache folder"),
        "{stderr}"
    );
    fs::write(
        work.join("data.toml"),
        "[datasets.out]\nref = \"local\"\nat = \"results\"\n",
    )?;
    let holding = sandbox
        .command(&["sync"])
        .env("FETCHLOOM_CACHE", work.join("results").join("cache"))
        .output()?;
    let stderr = text(&holding.stderr)?;
    assert_eq!(holding.status.code(), Some(50), "{stderr}");
    assert!(
        stderr.contains("is inside the output folder results, where datasets' files are placed"),
        "{stderr}"
    );
    assert!(!work.join("data.lock").exists(), "{stderr}");
    Ok(())
}

/// an add never pins a folder that is the cache, inside it or holding it as an output: refused
/// before data.toml or data.lock exist, and nothing copied.
#[test]
fn an_add_never_pins_the_cache_as_an_output() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    let cache = work.join("results").join(".cache");
    fs::create_dir_all(work.join("results"))?;
    fs::write(work.join("results/r.csv"), b"r\n")?;
    sandbox.make_cache(&cache)?;
    fs::create_dir_all(cache.join("tmp/sub"))?;
    fs::write(cache.join("tmp/sub/x.csv"), b"x\n")?;
    for (folder, said) in [
        ("results", "error: results holds the cache folder"),
        (
            "results/.cache/tmp/sub",
            "error: results/.cache/tmp/sub is inside the cache folder",
        ),
    ] {
        let refused = sandbox
            .command(&["add", folder, "--as", "out"])
            .env("FETCHLOOM_CACHE", &cache)
            .output()?;
        let stderr = text(&refused.stderr)?;
        assert_eq!(refused.status.code(), Some(2), "{folder}: {stderr}");
        assert!(stderr.contains(said), "{folder}: {stderr}");
        assert!(!work.join("data.toml").exists(), "{folder}: {stderr}");
        assert!(!work.join("data.lock").exists(), "{folder}: {stderr}");
    }
    Ok(())
}

/// the help of add, remove and drop tells the files fetchloom manages, placed in the data folder
/// for a dataset it fetched, from an output's, which stay the author's.
#[test]
fn help_tells_managed_files_from_an_outputs() -> Result {
    let sandbox = Sandbox::new()?;
    for (command, said) in [
        (
            "add",
            "a folder of the project is kept where it is instead, as an output, whose files stay yours",
        ),
        ("remove", "an output's folder keeps its files"),
        ("drop", "an output's folder keeps its files"),
    ] {
        let help = sandbox.run(&[command, "--help"])?;
        let stdout = text(&help.stdout)?;
        let words = stdout.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(words.contains(said), "{command}: {stdout}");
    }
    Ok(())
}

/// removing an output untracks it, with no question, as nothing is lost: the author's files stay
/// as they are, and of its folder's marker, in the data folder or outside it, only one holding
/// what fetchloom wrote goes, so git sees the folder again. a .gitignore of the author's own,
/// pinned with the output, stays too.
#[test]
fn removing_an_output_keeps_the_authors_files() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    processed(&work)?;
    fs::create_dir_all(work.join("results"))?;
    fs::write(work.join("results/summary.txt"), SUMMARY)?;
    fs::create_dir_all(work.join("figures"))?;
    fs::write(work.join("figures/plot.png"), b"png")?;
    fs::write(work.join("figures/.gitignore"), b"*.tmp\n")?;
    for (at, name) in [
        ("data/processed", "processed"),
        ("results", "results"),
        ("figures", "figures"),
    ] {
        ok(&sandbox.run(&["add", at, "--as", name])?)?;
    }
    assert_eq!(
        crate::support::marker(&work.join("results/.gitignore"))?,
        b"*\n"
    );
    let changed = work.join("data/processed/.gitignore");
    fs::write(&changed, b"*\n!summary.txt\n")?;
    let file = work.join("data/processed/iris_clean.csv");
    let before = look(&file)?;
    let removed = sandbox.run(&["remove", "processed", "results", "figures"])?;
    let stderr = text(&removed.stderr)?;
    assert_eq!(removed.status.code(), Some(0), "{stderr}");
    assert!(!stderr.contains("imported from files"), "{stderr}");
    assert_eq!(look(&file)?, before, "{stderr}");
    assert_eq!(fs::read(work.join("data/processed/summary.txt"))?, SUMMARY);
    assert_eq!(fs::read(work.join("results/summary.txt"))?, SUMMARY);
    assert!(!work.join("results/.gitignore").exists(), "{stderr}");
    assert_eq!(fs::read(&changed)?, b"*\n!summary.txt\n");
    assert_eq!(fs::read(work.join("figures/.gitignore"))?, b"*.tmp\n");
    assert_eq!(fs::read(work.join("figures/plot.png"))?, b"png");
    assert_eq!(fs::read_to_string(work.join("data.toml"))?, "");
    Ok(())
}

/// at a terminal, a removal says what other clones do: an output's folder keeps its files there
/// too, as fetchloom never placed them for a sync to take back.
#[cfg(target_os = "linux")]
#[test]
fn removing_an_output_says_its_folder_keeps_its_files() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    processed(&work)?;
    ok(&sandbox.run(&["add", "data/processed", "--as", "processed"])?)?;
    let Some(screen) = sandbox.on_a_terminal(&["remove", "processed"])? else {
        return Ok(());
    };
    let words = screen.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        words.contains(
            "next: commit data.toml and data.lock; data/processed keeps its files, here and in other clones"
        ),
        "{screen}"
    );
    assert!(!words.contains("remove unchanged files"), "{screen}");
    assert_eq!(fs::read(work.join("data/processed/summary.txt"))?, SUMMARY);
    Ok(())
}

/// a removed output's folder that a link took the place of on its way, as a moved folder may, is
/// no folder of the project: what the link leads to keeps its .gitignore.
#[cfg(unix)]
#[test]
fn removing_an_output_deletes_no_marker_through_a_link() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    fs::create_dir_all(work.join("out/results"))?;
    fs::write(work.join("out/results/summary.txt"), SUMMARY)?;
    ok(&sandbox.run(&["add", "out/results", "--as", "results"])?)?;
    assert_eq!(
        crate::support::marker(&work.join("out/results/.gitignore"))?,
        b"*\n"
    );
    let elsewhere = sandbox.folder("elsewhere")?;
    fs::create_dir_all(elsewhere.join("results"))?;
    let theirs = elsewhere.join("results/.gitignore");
    fs::write(&theirs, b"*\n")?;
    fs::remove_dir_all(work.join("out"))?;
    std::os::unix::fs::symlink(&elsewhere, work.join("out"))?;
    let removed = sandbox.run(&["remove", "results", "--yes"])?;
    let stderr = text(&removed.stderr)?;
    assert_eq!(removed.status.code(), Some(0), "{stderr}");
    assert_eq!(fs::read(&theirs)?, b"*\n", "{stderr}");
    Ok(())
}

/// what tools keep beside the files they work on, one of each kind, with the folder's one table.
const DROPPINGS: [&str; 12] = [
    ".table.csv.swp",
    ".table.csv.swo",
    "table.csv~",
    "#table.csv#",
    ".#notes.txt",
    ".~lock.table.csv#",
    "~$book.xlsx",
    ".nfs0000000001a2b3c400000002",
    ".fuse_hidden0000000a00000001",
    ".ipynb_checkpoints/table-checkpoint.csv",
    "__pycache__/m.cpython-312.pyc",
    "sub/.ipynb_checkpoints/x-checkpoint.csv",
];

/// `folder` holding table.csv and each of [`DROPPINGS`], and on unix emacs' lock, a link.
fn with_droppings(folder: &Path) -> Result {
    for dropping in DROPPINGS {
        let file = folder.join(dropping);
        fs::create_dir_all(file.parent().ok_or("a dropping outside any folder")?)?;
        fs::write(file, b"tool")?;
    }
    fs::write(folder.join("table.csv"), CLEAN)?;
    #[cfg(unix)]
    std::os::unix::fs::symlink("someone@host.4242:1700000000", folder.join(".#table.csv"))?;
    Ok(())
}

/// the files editors, office suites, notebooks, python and network disks keep beside a person's
/// while they work are never data: a capture pins the folder without them, as an import from
/// outside the project does, and one line says what it skipped, no warning.
#[test]
fn a_pin_leaves_out_what_tools_keep_beside_files() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    let out = work.join("out");
    fs::create_dir_all(&out)?;
    with_droppings(&out)?;
    let mine = sandbox.folder("mine")?;
    with_droppings(&mine)?;
    for (args, name) in [
        (&["--json", "add", "out", "--as", "results"][..], "results"),
        (&["--json", "add", "../mine"], "mine"),
    ] {
        let added = sandbox.run(args)?;
        let document = json(&added)?;
        assert_eq!(added.status.code(), Some(0), "{name}: {document}");
        let skipped = document["warnings"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|warning| warning["kind"] == "import.skipped")
            .count();
        assert_eq!(skipped, 0, "{name}: {document}");
        let lock = fs::read_to_string(work.join("data.lock"))?;
        let pinned = lock
            .split("[[dataset]]")
            .find(|table| table.contains(&format!("name = \"{name}\"")));
        let pinned = pinned.ok_or("no table")?;
        assert_eq!(pinned.matches("{ path = ").count(), 1, "{name}: {lock}");
        assert!(pinned.contains("{ path = \"table.csv\""), "{name}: {lock}");
    }
    let human = sandbox.run(&["add", "out", "--as", "results", "--dry-run"])?;
    let stderr = text(&human.stderr)?;
    assert_eq!(human.status.code(), Some(0), "{stderr}");
    assert!(stderr.contains("Skipped #table.csv# and "), "{stderr}");
    assert!(
        stderr.contains(" more working files of editors and tools in out\n"),
        "{stderr}"
    );
    Ok(())
}

/// a dropping coming or going is no change: an output pinned while an editor had a file open is
/// unchanged once the editor closes, and once a notebook and an office suite leave theirs, the
/// system its own, a disk's trash what a person deleted there, a repository its metadata, and a
/// stopped run a copy under the name it stages files in.
#[test]
fn a_dropping_coming_or_going_is_no_change_to_an_output() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    processed(&work)?;
    let folder = work.join("data/processed");
    fs::write(folder.join(".iris_clean.csv.swp"), b"vim")?;
    ok(&sandbox.run(&["add", "data/processed", "--as", "processed"])?)?;
    let lock = fs::read_to_string(work.join("data.lock"))?;
    assert_eq!(lock.matches("{ path = ").count(), 2, "{lock}");
    fs::remove_file(folder.join(".iris_clean.csv.swp"))?;
    fs::create_dir_all(folder.join(".ipynb_checkpoints"))?;
    fs::write(
        folder.join(".ipynb_checkpoints/iris_clean-checkpoint.csv"),
        CLEAN,
    )?;
    fs::write(folder.join(".~lock.summary.txt#"), b"lock")?;
    fs::write(folder.join(".DS_Store"), b"finder")?;
    fs::create_dir_all(folder.join(".Trash-1000/files"))?;
    fs::write(folder.join(".Trash-1000/files/old.csv"), CLEAN)?;
    fs::create_dir_all(folder.join(".hg"))?;
    fs::write(folder.join(".hg/dirstate"), b"hg")?;
    fs::write(folder.join(".fetchloom-0123456789abcdef.copy"), CLEAN)?;
    let checked = sandbox.run(&["status", "--verify", "--check"])?;
    assert_eq!(checked.status.code(), Some(0), "{}", text(&checked.stderr)?);
    let document = json(&sandbox.run(&["--json", "status"])?)?;
    for kind in ["missing", "edited", "extra", "yours"] {
        assert_eq!(
            listed(&document, kind),
            Vec::<String>::new(),
            "{kind}: {document}"
        );
    }
    let synced = sandbox.run(&["--json", "sync"])?;
    let document = json(&synced)?;
    assert_eq!(synced.status.code(), Some(0), "{document}");
    assert!(
        warned(&document)
            .iter()
            .all(|said| !said.contains("changed since")),
        "{document}"
    );
    let again = sandbox.run(&["add", "data/processed", "--as", "processed"])?;
    let stderr = text(&again.stderr)?;
    assert_eq!(again.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("processed is pinned as data/processed holds it"),
        "{stderr}"
    );
    assert_eq!(fs::read_to_string(work.join("data.lock"))?, lock);
    Ok(())
}

/// a template's .gitkeep, git's placeholder for an empty folder, is never part of an output: the
/// pipeline's first run, writing the folder anew without it, changes nothing, nor does one coming
/// back.
#[test]
fn a_gitkeep_is_no_part_of_an_output() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    let folder = work.join("data/processed");
    fs::create_dir_all(&folder)?;
    fs::write(folder.join(".gitkeep"), b"")?;
    processed(&work)?;
    ok(&sandbox.run(&["add", "data/processed", "--as", "processed"])?)?;
    let lock = fs::read_to_string(work.join("data.lock"))?;
    assert!(!lock.contains(".gitkeep"), "{lock}");
    delete_folder(&folder)?;
    processed(&work)?;
    let document = json(&sandbox.run(&["--json", "status"])?)?;
    for kind in ["missing", "edited", "extra"] {
        assert_eq!(
            listed(&document, kind),
            Vec::<String>::new(),
            "{kind}: {document}"
        );
    }
    fs::write(folder.join(".gitkeep"), b"")?;
    let checked = sandbox.run(&["status", "--check"])?;
    assert_eq!(checked.status.code(), Some(0), "{}", text(&checked.stderr)?);
    Ok(())
}

/// a folder that changes while add reads it is no state it ever held whole: nothing is pinned and
/// the cache takes none of its files, whether a file came or one was rewritten at its size. a
/// tool's working file coming meanwhile, as an editor's swap file, is no change.
#[test]
fn a_folder_that_changes_while_it_is_read_is_not_pinned() -> Result {
    for (late, bytes, changes) in [
        ("late.csv", &b"late\n"[..], true),
        ("iris_clean.csv", &b"sepal,petal\n5.1,1.5\n"[..], true),
        (".iris_clean.csv.swp", &b"vim"[..], false),
    ] {
        let sandbox = Sandbox::new()?;
        let work = sandbox.work();
        processed(&work)?;
        let release = sandbox.folder("hold")?.join("release");
        let held = sandbox.folder("hold")?.join("release.held");
        let mut command = sandbox.command(&["add", "data/processed", "--as", "processed"]);
        command.env("FETCHLOOM_TEST_HOLD_CAPTURE", &release);
        let running = Running::start(&mut command)?;
        wait_until("add to read the folder", || Ok(held.exists()))?;
        fs::write(work.join("data/processed").join(late), bytes)?;
        fs::write(&release, b"")?;
        let output = running.finish()?;
        let stderr = text(&output.stderr)?;
        if !changes {
            assert_eq!(output.status.code(), Some(0), "{late}: {stderr}");
            let lock = fs::read_to_string(work.join("data.lock"))?;
            assert_eq!(lock.matches("{ path = ").count(), 2, "{late}: {lock}");
            continue;
        }
        assert_eq!(output.status.code(), Some(50), "{late}: {stderr}");
        assert!(
            stderr.contains(
                "error: data/processed changed while it was read: let the pipeline finish, then add again"
            ),
            "{late}: {stderr}"
        );
        assert!(!work.join("data.lock").exists(), "{late}: {stderr}");
        assert_eq!(count_files(&sandbox.cache().join("objects")), 0, "{late}");
    }
    Ok(())
}

/// an output has no source: audit calls a file of it no mirror holds local, and diff says when
/// an output moved, with the same files, as data.lock records where it lives.
#[test]
fn audit_and_diff_say_what_an_output_is() -> Result {
    let sandbox = Sandbox::new()?;
    if crate::merge::ok(&sandbox, &["init", "-q"])?.is_none() {
        return Ok(());
    }
    let work = sandbox.work();
    processed(&work)?;
    ok(&sandbox.run(&["add", "data/processed", "--as", "processed"])?)?;
    let document = json(&sandbox.run(&["--json", "audit"])?)?;
    let unserved = &document["result"]["datasets"][0]["not_served"];
    assert_eq!(unserved[0]["outcome"], "local", "{document}");
    assert_eq!(
        unserved[0]["message"],
        "it is an output made in this project, at data/processed, and no mirror holds it"
    );
    let audited = sandbox.run(&["audit"])?;
    let told = text(&audited.stdout)?;
    assert!(told.contains("Local processed · 2 of 2 files"), "{told}");
    crate::merge::ok(&sandbox, &["add", "-A"])?;
    crate::merge::ok(&sandbox, &["commit", "-qm", "pinned"])?;
    fs::rename(work.join("data/processed"), work.join("data/clean"))?;
    let moved = fs::read_to_string(work.join("data.toml"))?.replace("data/processed", "data/clean");
    fs::write(work.join("data.toml"), moved)?;
    ok(&sandbox.run(&["lock"])?)?;
    let diff = text(&sandbox.run(&["diff"])?.stdout)?;
    assert!(
        diff.contains("processed changed: at data/processed \u{2192} data/clean; the same files"),
        "{diff}"
    );
    let document = json(&sandbox.run(&["--json", "diff"])?)?;
    assert_eq!(
        document["result"]["datasets"][0]["fields"],
        serde_json::json!([{ "name": "at", "before": "data/processed", "after": "data/clean" }])
    );
    Ok(())
}

/// data.toml comes with a clone, so an output's `at` may name a folder of code: sync and status
/// offer no `--force` while nothing there is a file this machine placed, and `--force` never
/// deletes or overwrites a file git tracks, as one at a pinned path holding edits not committed,
/// though it deletes the rest its pin lacks; status says such a file stays.
#[test]
fn force_never_deletes_what_git_tracks_in_an_outputs_folder() -> Result {
    let sandbox = Sandbox::new()?;
    if crate::merge::ok(&sandbox, &["init", "-q"])?.is_none() {
        return Ok(());
    }
    let work = sandbox.work();
    fs::create_dir_all(work.join("src"))?;
    fs::write(work.join("src/main.py"), b"print(1)\n")?;
    fs::write(work.join("src/iris.csv"), b"mine\n")?;
    crate::merge::ok(&sandbox, &["add", "src"])?;
    crate::merge::ok(&sandbox, &["commit", "-qm", "code"])?;
    fs::write(work.join("src/iris.csv"), b"mine, edited\n")?;
    fs::create_dir_all(work.join("tmpout"))?;
    fs::write(work.join("tmpout/iris.csv"), CLEAN)?;
    ok(&sandbox.run(&["add", "tmpout", "--as", "derived"])?)?;
    for file in ["data.toml", "data.lock"] {
        let written = fs::read_to_string(work.join(file))?;
        fs::write(
            work.join(file),
            written.replace("at = \"tmpout\"", "at = \"src\""),
        )?;
    }
    // as a fresh clone has it: no state, and no folder of the pipeline's
    delete_folder(&work.join(".fetchloom"))?;
    delete_folder(&work.join("tmpout"))?;
    fs::write(work.join("src/wip.py"), b"work in progress\n")?;
    for (args, code) in [
        (&["sync"][..], 0),
        (&["status"], 0),
        (&["status", "--check"], 1),
    ] {
        let output = sandbox.run(args)?;
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(code), "{args:?}: {stderr}");
        assert!(
            stderr.contains("no file there is one fetchloom placed on this machine"),
            "{args:?}: {stderr}"
        );
        assert!(!stderr.contains("--force"), "{args:?}: {stderr}");
    }
    let said = "2 files git tracks stay in derived's folder, such as src/iris.csv, as `sync --force` never deletes or overwrites a file git tracks that a plain sync keeps";
    let dry = sandbox.run(&["sync", "--force", "derived", "--dry-run"])?;
    let stderr = text(&dry.stderr)?;
    assert_eq!(dry.status.code(), Some(0), "{stderr}");
    assert!(stderr.contains(said), "{stderr}");
    assert!(!stderr.contains("Would remove src/main.py"), "{stderr}");
    assert!(!stderr.contains("src/iris.csv ·"), "{stderr}");
    assert!(stderr.contains("Would remove src/wip.py"), "{stderr}");
    let forced = sandbox.run(&["sync", "--force", "derived"])?;
    let stderr = text(&forced.stderr)?;
    assert_eq!(forced.status.code(), Some(0), "{stderr}");
    assert!(stderr.contains(said), "{stderr}");
    assert_eq!(fs::read(work.join("src/main.py"))?, b"print(1)\n");
    assert_eq!(fs::read(work.join("src/iris.csv"))?, b"mine, edited\n");
    assert!(!work.join("src/wip.py").exists(), "{stderr}");
    let status = text(&sandbox.run(&["status"])?.stdout)?;
    assert!(
        status.contains("Committed src/iris.csv · git's copy holds other bytes than data.lock pins, and it stays, as `sync --force` never deletes or overwrites a file git tracks that a plain sync keeps"),
        "{status}"
    );
    Ok(())
}

/// a file git tracks that `sync --force` keeps stays unresolved: when a teammate commits a data.lock
/// pinning other bytes than git's copy of an output's file, the forced run never says the files
/// are in place nor marks the data ready, and every readiness check after it, hashing or not, names
/// the file and why it stayed, never offering a `--force` that cannot change it.
#[test]
fn a_file_git_tracks_that_force_keeps_leaves_the_data_not_ready() -> Result {
    let sandbox = Sandbox::new()?;
    if crate::merge::ok(&sandbox, &["init", "-q"])?.is_none() {
        return Ok(());
    }
    let work = sandbox.work();
    fs::create_dir_all(work.join("results"))?;
    fs::write(work.join("results/a.csv"), b"v1\n")?;
    crate::merge::ok(&sandbox, &["add", "results"])?;
    crate::merge::ok(&sandbox, &["commit", "-qm", "results"])?;
    ok(&sandbox.run(&["add", "results", "--as", "results"])?)?;
    let clone = sandbox.folder("clone")?;
    for file in ["data.toml", "data.lock"] {
        fs::copy(work.join(file), clone.join(file))?;
    }
    fs::create_dir_all(clone.join("results"))?;
    fs::write(clone.join("results/a.csv"), b"v2\n")?;
    ok(&sandbox
        .command_in(&clone, &["add", "./results", "--as", "results", "--yes"])
        .output()?)?;
    fs::copy(clone.join("data.lock"), work.join("data.lock"))?;
    let stays = "results/a.csv stays, as git tracks it, and `sync --force` never deletes or overwrites a file git tracks that a plain sync keeps";
    let ways = "`fetchloom add ./results --as results` pins what it holds now, or make git's copy match data.lock";
    let forced = sandbox.run(&["sync", "--force"])?;
    let stderr = text(&forced.stderr)?;
    assert_eq!(forced.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains(&format!("{stays}\n  help: {ways}\n")),
        "{stderr}"
    );
    assert!(!stderr.contains("in place"), "{stderr}");
    assert_eq!(fs::read(work.join("results/a.csv"))?, b"v1\n");
    for args in [
        &["status", "--check"][..],
        &["status", "--verify", "--check"],
        &["sync", "--strict"],
    ] {
        let checked = sandbox.run(args)?;
        let stderr = text(&checked.stderr)?;
        assert_eq!(checked.status.code(), Some(1), "{args:?}: {stderr}");
        assert!(
            stderr.contains("error: the data is not ready to use"),
            "{args:?}: {stderr}"
        );
        assert!(
            stderr.contains(&format!("{stays}; {ways}")),
            "{args:?}: {stderr}"
        );
        assert!(
            !stderr.contains("fetchloom sync --force"),
            "{args:?}: {stderr}"
        );
    }
    Ok(())
}

/// an output's folder git keeps, as a person may on purpose: a rerun that changes a file there
/// changes the output, and no line offers the `--force` that cannot change that file, not even the
/// re-pin that waits for a yes. each names the pin of what the folder holds now, or git's copy
/// brought back to data.lock, and a forced run keeps the file, as kept, never calling it in place
/// nor blaming `at`.
#[test]
fn a_tracked_output_file_a_rerun_changed_is_never_offered_to_force() -> Result {
    let sandbox = Sandbox::new()?;
    if crate::merge::ok(&sandbox, &["init", "-q"])?.is_none() {
        return Ok(());
    }
    let work = sandbox.work();
    fs::create_dir_all(work.join("results"))?;
    fs::write(work.join("results/table.csv"), b"v1\n")?;
    crate::merge::ok(&sandbox, &["add", "results"])?;
    crate::merge::ok(&sandbox, &["commit", "-qm", "results"])?;
    ok(&sandbox.run(&["add", "results", "--as", "results"])?)?;
    fs::write(work.join("results/table.csv"), b"rerun\n")?;
    let stays = "results/table.csv stays, as git tracks it, and `sync --force` never deletes or overwrites a file git tracks that a plain sync keeps";
    let ways = "`fetchloom add ./results --as results` pins what it holds now, or make git's copy match data.lock";
    for (args, code) in [
        (&["status"][..], 0),
        (&["status", "--check"], 1),
        (&["sync"], 0),
    ] {
        let output = sandbox.run(args)?;
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(code), "{args:?}: {stderr}");
        assert!(
            stderr.contains(&format!("{stays}; {ways}")),
            "{args:?}: {stderr}"
        );
        assert!(
            !stderr.contains("fetchloom sync --force"),
            "{args:?}: {stderr}"
        );
    }
    let asked = sandbox.run(&["add", "./results", "--as", "results"])?;
    let stderr = text(&asked.stderr)?;
    assert_eq!(asked.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains(&format!(
            "{stays}; `fetchloom add ./results --as results --yes` pins what it holds now, or make git's copy match data.lock"
        )),
        "{stderr}"
    );
    assert!(!stderr.contains("fetchloom sync --force"), "{stderr}");
    let forced = json(&sandbox.run(&["--json", "sync", "--force", "results"])?)?;
    assert_eq!(
        forced["result"]["placement"]["kept"],
        serde_json::json!([{"path": "results/table.csv", "reason": "tracked"}]),
        "{forced}"
    );
    let warned = forced["warnings"].as_array().and_then(|warnings| {
        warnings
            .iter()
            .find(|warning| warning["kind"] == "place.kept")
    });
    assert_eq!(
        warned.map(|warning| (&warning["message"], &warning["help"])),
        Some((&serde_json::json!(stays), &serde_json::json!(ways))),
        "{forced}"
    );
    assert_eq!(fs::read(work.join("results/table.csv"))?, b"rerun\n");
    let strict = sandbox.run(&["sync", "--force", "--strict"])?;
    let stderr = text(&strict.stderr)?;
    assert_eq!(strict.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains(&format!("{stays}; {ways}")), "{stderr}");
    assert!(!stderr.contains("in place"), "{stderr}");
    Ok(())
}

/// a file git tracks in an output's folder that its pin does not hold stays through `sync --force`,
/// a dry run of it too, and neither says the run has nothing left to do; status names it as a file
/// `--force` leaves, with the pin that takes it in, until git's copy matches data.lock again.
#[test]
fn a_tracked_file_an_outputs_pin_lacks_stays_named() -> Result {
    let sandbox = Sandbox::new()?;
    if crate::merge::ok(&sandbox, &["init", "-q"])?.is_none() {
        return Ok(());
    }
    let work = sandbox.work();
    fs::create_dir_all(work.join("results"))?;
    fs::write(work.join("results/table.csv"), b"v1\n")?;
    crate::merge::ok(&sandbox, &["add", "results"])?;
    crate::merge::ok(&sandbox, &["commit", "-qm", "results"])?;
    ok(&sandbox.run(&["add", "results", "--as", "results"])?)?;
    fs::write(work.join("results/notes.md"), b"mine\n")?;
    crate::merge::ok(&sandbox, &["add", "results/notes.md"])?;
    let stays = "results/notes.md stays, as git tracks it, and `sync --force` never deletes or overwrites a file git tracks that a plain sync keeps";
    let ways = "`fetchloom add ./results --as results` pins what it holds now, or make git's copy match data.lock";
    for (args, closing) in [
        (
            &["sync", "--force", "--dry-run"][..],
            "Ok dry run, nothing was changed",
        ),
        (
            &["sync", "--force"],
            "Synced 1 dataset into data, 1 file left unchanged; see the warnings above",
        ),
    ] {
        let forced = sandbox.run(args)?;
        let stderr = text(&forced.stderr)?;
        assert_eq!(forced.status.code(), Some(0), "{args:?}: {stderr}");
        assert!(
            stderr.contains(&format!("{stays}\n  help: {ways}\n")),
            "{args:?}: {stderr}"
        );
        assert!(stderr.contains(closing), "{args:?}: {stderr}");
        assert!(!stderr.contains("in place"), "{args:?}: {stderr}");
    }
    assert!(work.join("results/notes.md").exists());
    let checked = sandbox.run(&["status", "--check"])?;
    let stderr = text(&checked.stderr)?;
    assert_eq!(checked.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains(&format!("{stays}; {ways}")), "{stderr}");
    assert!(!stderr.contains("fetchloom sync --force"), "{stderr}");
    crate::merge::ok(&sandbox, &["rm", "-qf", "results/notes.md"])?;
    ok(&sandbox.run(&["status", "--check"])?)?;
    Ok(())
}

/// `sync --force` does all a plain sync does: a copy of an output a sync placed, which the teammate
/// then tracks in git, follows the author's new pin as any placed file does, since no one changed
/// it. only what `--force` alone replaces or deletes stays for git, as an edit, even one that kept
/// the file's size and time.
#[test]
fn force_updates_a_tracked_copy_sync_placed_as_a_plain_sync_does() -> Result {
    let sandbox = Sandbox::new()?;
    if crate::merge::ok(&sandbox, &["init", "-q"])?.is_none() {
        return Ok(());
    }
    let work = sandbox.work();
    let author = sandbox.folder("author")?;
    fs::create_dir_all(author.join("results"))?;
    fs::write(author.join("results/a.csv"), b"v1\n")?;
    let pin = |args: &[&str]| -> Result { ok(&sandbox.command_in(&author, args).output()?) };
    pin(&["add", "results", "--as", "results"])?;
    for file in ["data.toml", "data.lock"] {
        fs::copy(author.join(file), work.join(file))?;
    }
    ok(&sandbox.run(&["sync"])?)?;
    crate::merge::ok(&sandbox, &["add", "-f", "results/a.csv"])?;
    crate::merge::ok(&sandbox, &["commit", "-qm", "track the output"])?;
    fs::write(author.join("results/a.csv"), b"v2\n")?;
    pin(&["add", "./results", "--as", "results", "--yes"])?;
    fs::copy(author.join("data.lock"), work.join("data.lock"))?;
    let forced = sandbox.run(&["sync", "--force"])?;
    let stderr = text(&forced.stderr)?;
    assert_eq!(forced.status.code(), Some(0), "{stderr}");
    assert!(!stderr.contains("git tracks"), "{stderr}");
    assert_eq!(fs::read(work.join("results/a.csv"))?, b"v2\n", "{stderr}");
    ok(&sandbox.run(&["status", "--check"])?)?;
    // bytes changed under the same size and time, as only hashing finds, are an edit all the same
    let placed = work.join("results/a.csv");
    let state = work.join(".fetchloom").join("state");
    sandbox.wait_for_a_later_time(fs::metadata(&state)?.modified()?)?;
    fs::write(&state, fs::read(&state)?)?;
    let modified = fs::metadata(&placed)?.modified()?;
    crate::support::edit(&placed, b"v8\n")?;
    fs::File::options()
        .write(true)
        .open(&placed)?
        .set_modified(modified)?;
    let verified = sandbox.run(&["sync", "--force", "--verify"])?;
    let stderr = text(&verified.stderr)?;
    assert_eq!(verified.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("results/a.csv stays, as git tracks it"),
        "{stderr}"
    );
    assert_eq!(fs::read(&placed)?, b"v8\n", "{stderr}");
    Ok(())
}

/// an output git tracks, pinned with LF line ends and checked out where git makes them CRLF, as git
/// for windows does by default, differs from its pin only in those line ends: status, its check, the
/// re-pin's refusal and a forced sync say so, with the .gitattributes line and the fresh checkout
/// that write the bytes git keeps, never a re-pin, which would only move the difference to the
/// other system, nor a doubt about `at`. it stays not ready, as a pin is exact bytes, until that
/// checkout, which writes the file even where git trusts its record of the CRLF one.
#[test]
fn an_outputs_line_ends_git_converted_are_told_with_their_fix() -> Result {
    let sandbox = Sandbox::new()?;
    if crate::merge::ok(&sandbox, &["init", "-q"])?.is_none() {
        return Ok(());
    }
    let work = sandbox.work();
    let author = sandbox.folder("author")?;
    fs::create_dir_all(author.join("results"))?;
    fs::write(author.join("results/a.csv"), b"a,b\n1,2\n")?;
    ok(&sandbox
        .command_in(&author, &["add", "results", "--as", "results"])
        .output()?)?;
    for file in ["data.toml", "data.lock"] {
        fs::copy(author.join(file), work.join(file))?;
    }
    // as a clone where git converts line ends has it: LF committed, CRLF checked out
    crate::merge::ok(&sandbox, &["config", "core.autocrlf", "true"])?;
    fs::create_dir_all(work.join("results"))?;
    fs::write(work.join("results/a.csv"), b"a,b\r\n1,2\r\n")?;
    aged(&work.join("results/a.csv"))?;
    crate::merge::ok(&sandbox, &["add", "results"])?;
    crate::merge::ok(&sandbox, &["commit", "-qm", "results"])?;
    let told = "results/a.csv differs from the pin only in its line ends, made CRLF by git's checkout; add `results/** -text` to the .gitattributes beside data.toml and commit it, then `git rm --cached -q -- results/a.csv` and `git checkout HEAD -- results/a.csv` write the bytes git keeps, which the pin holds";
    for (args, code) in [
        (&["status"][..], 0),
        (&["status", "--check"], 1),
        (&["add", "./results", "--as", "results"], 1),
        (&["sync", "--force"], 0),
    ] {
        let output = sandbox.run(args)?;
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(code), "{args:?}: {stderr}");
        assert!(stderr.contains(told), "{args:?}: {stderr}");
        assert!(!stderr.contains("fetchloom add"), "{args:?}: {stderr}");
        assert!(!stderr.contains("--yes"), "{args:?}: {stderr}");
        assert!(!stderr.contains("`at`"), "{args:?}: {stderr}");
    }
    let rows = text(&sandbox.run(&["status"])?.stdout)?;
    assert!(
        rows.contains("Committed results/a.csv · git's copy differs from the pinned bytes only in its line ends"),
        "{rows}"
    );
    // line ends are told file by file: a file no pin holds is a change, pinned after their fix
    fs::write(work.join("results/extra.csv"), b"x\n")?;
    let mixed = text(&sandbox.run(&["status"])?.stderr)?;
    assert!(
        mixed.contains(&format!(
            "{told}; after that, `fetchloom add ./results --as results` pins what it holds now"
        )),
        "{mixed}"
    );
    assert!(!mixed.contains("`at`"), "{mixed}");
    fs::remove_file(work.join("results/extra.csv"))?;
    fs::write(work.join(".gitattributes"), b"results/** -text\n")?;
    crate::merge::ok(&sandbox, &["add", ".gitattributes"])?;
    crate::merge::ok(&sandbox, &["commit", "-qm", "keep the results' bytes"])?;
    crate::merge::ok(&sandbox, &["rm", "--cached", "-q", "--", "results/a.csv"])?;
    crate::merge::ok(&sandbox, &["checkout", "HEAD", "--", "results/a.csv"])?;
    assert_eq!(fs::read(work.join("results/a.csv"))?, b"a,b\n1,2\n");
    ok(&sandbox.run(&["status", "--check"])?)?;
    Ok(())
}

/// a file of an output's folder that git tracks as a link, as a clone can hold one to a fifo, a
/// terminal or any file of the person's, is never opened to look at its line ends: it is no file
/// that differs from its pin only in those, whatever the file it names holds.
#[cfg(unix)]
#[test]
fn a_tracked_link_in_an_outputs_folder_is_never_read_for_its_line_ends() -> Result {
    let sandbox = Sandbox::new()?;
    if crate::merge::ok(&sandbox, &["init", "-q"])?.is_none() {
        return Ok(());
    }
    let work = sandbox.work();
    let author = sandbox.folder("author")?;
    fs::create_dir_all(author.join("results"))?;
    fs::write(author.join("results/a.csv"), b"a,b\n1,2\n")?;
    ok(&sandbox
        .command_in(&author, &["add", "results", "--as", "results"])
        .output()?)?;
    for file in ["data.toml", "data.lock"] {
        fs::copy(author.join(file), work.join(file))?;
    }
    // the pinned bytes with CRLF, which a read through the link would take for git's checkout
    let named = sandbox.folder("outside")?.join("a.csv");
    fs::write(&named, b"a,b\r\n1,2\r\n")?;
    fs::create_dir_all(work.join("results"))?;
    std::os::unix::fs::symlink(&named, work.join("results/a.csv"))?;
    crate::merge::ok(&sandbox, &["add", "results"])?;
    crate::merge::ok(&sandbox, &["commit", "-qm", "results"])?;
    let output = sandbox.run(&["status"])?;
    let said = format!("{}{}", text(&output.stdout)?, text(&output.stderr)?);
    assert_eq!(output.status.code(), Some(0), "{said}");
    assert!(said.contains("results"), "{said}");
    assert!(!said.contains("line ends"), "{said}");
    assert_eq!(fs::read_link(work.join("results/a.csv"))?, named);
    Ok(())
}

/// the line-ends check reads the blobs git keeps for its files with one git run, as `-vv` tells,
/// tells each file and the one checkout that writes them all, and runs nothing the repository's own
/// config names.
#[test]
fn line_ends_are_told_with_one_git_run() -> Result {
    let sandbox = Sandbox::new()?;
    if crate::merge::ok(&sandbox, &["init", "-q"])?.is_none() {
        return Ok(());
    }
    let work = sandbox.work();
    let author = sandbox.folder("author")?;
    fs::create_dir_all(author.join("results"))?;
    for name in ["a", "b", "c"] {
        fs::write(
            author.join(format!("results/{name}.csv")),
            format!("{name},b\n1,2\n"),
        )?;
    }
    ok(&sandbox
        .command_in(&author, &["add", "results", "--as", "results"])
        .output()?)?;
    for file in ["data.toml", "data.lock"] {
        fs::copy(author.join(file), work.join(file))?;
    }
    crate::merge::ok(&sandbox, &["config", "core.autocrlf", "true"])?;
    fs::create_dir_all(work.join("results"))?;
    for name in ["a", "b", "c"] {
        let path = work.join(format!("results/{name}.csv"));
        fs::write(&path, format!("{name},b\r\n1,2\r\n"))?;
        aged(&path)?;
    }
    crate::merge::ok(&sandbox, &["add", "results"])?;
    crate::merge::ok(&sandbox, &["commit", "-qm", "results"])?;
    let marks = sandbox.folder("marks")?;
    crate::untrusted::hostile_git_config(&work, &marks)?;
    let output = sandbox.command(&["-vv", "status"]).output()?;
    let stderr = text(&output.stderr)?;
    let stdout = text(&output.stdout)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("read the blobs from git in one run blobs=3"),
        "{stderr}"
    );
    let started = stderr
        .matches("starts git cat-file --batch for the blobs blobs=3")
        .count();
    // a git that refuses `--no-lazy-fetch` is asked once more without it
    assert!((1..=2).contains(&started), "{stderr}");
    assert!(
        stderr.contains("`git checkout HEAD -- results/a.csv results/b.csv results/c.csv`"),
        "{stderr}"
    );
    for name in ["a", "b", "c"] {
        assert!(
            stdout.contains(&format!(
                "Committed results/{name}.csv · git's copy differs from the pinned bytes only in its line ends"
            )),
            "{stdout}"
        );
    }
    assert_eq!(crate::untrusted::ran(&marks)?, Vec::<String>::new());
    Ok(())
}

/// the line-ends check starts no git for blobs where it has none to read, and tells what it finds:
/// a file git tracks that holds other bytes than its pin, not only other line ends, is not told as
/// line ends, and a file that differs only in its line ends but whose staged blob is not the
/// committed one is told so, with no checkout offered.
#[test]
fn line_ends_with_no_blob_to_read_run_no_git_batch() -> Result {
    for restaged in [false, true] {
        let sandbox = Sandbox::new()?;
        if crate::merge::ok(&sandbox, &["init", "-q"])?.is_none() {
            return Ok(());
        }
        let work = sandbox.work();
        let author = sandbox.folder("author")?;
        fs::create_dir_all(author.join("results"))?;
        fs::write(author.join("results/a.csv"), b"a,b\n1,2\n")?;
        ok(&sandbox
            .command_in(&author, &["add", "results", "--as", "results"])
            .output()?)?;
        for file in ["data.toml", "data.lock"] {
            fs::copy(author.join(file), work.join(file))?;
        }
        crate::merge::ok(&sandbox, &["config", "core.autocrlf", "true"])?;
        let file = work.join("results/a.csv");
        fs::create_dir_all(work.join("results"))?;
        fs::write(&file, b"a,b\r\n1,2\r\n")?;
        aged(&file)?;
        crate::merge::ok(&sandbox, &["add", "results"])?;
        crate::merge::ok(&sandbox, &["commit", "-qm", "results"])?;
        if restaged {
            // another blob staged, then the pinned bytes back with CRLF, unstaged
            fs::write(&file, b"a,b\r\n9,9\r\n")?;
            crate::merge::ok(&sandbox, &["add", "results"])?;
            fs::write(&file, b"a,b\r\n1,2\r\n")?;
        } else {
            fs::write(&file, b"a,b\r\n7,7\r\n")?;
        }
        aged(&file)?;
        let output = sandbox.command(&["-vv", "status"]).output()?;
        let stderr = text(&output.stderr)?;
        let stdout = text(&output.stdout)?;
        assert_eq!(output.status.code(), Some(0), "{restaged}: {stderr}");
        assert!(
            stdout.contains("results/a.csv") || stderr.contains("results/a.csv"),
            "{restaged}: {stdout}{stderr}"
        );
        assert!(
            !stderr.contains("starts git cat-file --batch"),
            "{restaged}: {stderr}"
        );
        assert!(
            !stderr.contains("read the blobs from git in one run"),
            "{restaged}: {stderr}"
        );
        let told = "results/a.csv differs from the pin only in its line ends, made CRLF by git's checkout, but git's staged or committed copy is not the pinned one, so no checkout brings the pin back";
        assert_eq!(stderr.contains(told), restaged, "{restaged}: {stderr}");
        assert_eq!(
            stderr.contains("differs from the pin only in its line ends"),
            restaged,
            "{restaged}: {stderr}"
        );
        if restaged {
            assert!(!stderr.contains("git checkout"), "{stderr}");
        }
    }
    Ok(())
}

/// line ends are decided file by file: beside a rerun of a file git tracks, a file that differs
/// from the pin only in the line ends git's checkout gave it is told so, with the fresh checkout
/// of it alone, never as other bytes, and with no doubt about `at`. the re-pin comes after that
/// fix, so it pins the rerun beside the file as pinned, never its CRLF.
#[test]
fn line_ends_are_told_file_by_file_beside_a_rerun() -> Result {
    let sandbox = Sandbox::new()?;
    if crate::merge::ok(&sandbox, &["init", "-q"])?.is_none() {
        return Ok(());
    }
    let work = sandbox.work();
    let author = sandbox.folder("author")?;
    fs::create_dir_all(author.join("results"))?;
    fs::write(author.join("results/a.csv"), b"a\n1\n")?;
    fs::write(author.join("results/b.csv"), b"b\n1\n")?;
    ok(&sandbox
        .command_in(&author, &["add", "results", "--as", "results"])
        .output()?)?;
    for file in ["data.toml", "data.lock"] {
        fs::copy(author.join(file), work.join(file))?;
    }
    // a clone where git converts line ends, and a rerun of b.csv
    crate::merge::ok(&sandbox, &["config", "core.autocrlf", "true"])?;
    let results = work.join("results");
    fs::create_dir_all(&results)?;
    fs::write(results.join("a.csv"), b"a\r\n1\r\n")?;
    fs::write(results.join("b.csv"), b"b\r\n1\r\n")?;
    aged(&results.join("a.csv"))?;
    crate::merge::ok(&sandbox, &["add", "results"])?;
    crate::merge::ok(&sandbox, &["commit", "-qm", "results"])?;
    fs::write(results.join("b.csv"), b"b\n2\n")?;
    let fix = "results/a.csv differs from the pin only in its line ends, made CRLF by git's checkout; add `results/** -text` to the .gitattributes beside data.toml and commit it, then `git rm --cached -q -- results/a.csv` and `git checkout HEAD -- results/a.csv` write the bytes git keeps, which the pin holds; after that, ";
    for (args, code, pin) in [
        (&["status"][..], 0, "`fetchloom add ./results --as results`"),
        (
            &["status", "--check"],
            1,
            "`fetchloom add ./results --as results`",
        ),
        (
            &["add", "./results", "--as", "results"],
            1,
            "`fetchloom add ./results --as results --yes`",
        ),
        (
            &["sync", "--force"],
            0,
            "`fetchloom add ./results --as results`",
        ),
    ] {
        let output = sandbox.run(args)?;
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(code), "{args:?}: {stderr}");
        assert!(
            stderr.contains(&format!("{fix}{pin} pins what it holds now")),
            "{args:?}: {stderr}"
        );
        // every re-pin offered comes after the fix of a.csv's line ends
        assert_eq!(
            stderr.matches("fetchloom add").count(),
            stderr.matches("; after that, `fetchloom add").count(),
            "{args:?}: {stderr}"
        );
        assert!(!stderr.contains("`at`"), "{args:?}: {stderr}");
    }
    let rows = text(&sandbox.run(&["status"])?.stdout)?;
    for row in [
        "Committed results/a.csv · git's copy differs from the pinned bytes only in its line ends",
        "Committed results/b.csv · git's copy holds other bytes than data.lock pins",
    ] {
        assert!(rows.contains(row), "{rows}");
    }
    fs::write(work.join(".gitattributes"), b"results/** -text\n")?;
    crate::merge::ok(&sandbox, &["add", ".gitattributes"])?;
    crate::merge::ok(&sandbox, &["commit", "-qm", "keep the results' bytes"])?;
    crate::merge::ok(&sandbox, &["rm", "--cached", "-q", "--", "results/a.csv"])?;
    crate::merge::ok(&sandbox, &["checkout", "HEAD", "--", "results/a.csv"])?;
    assert_eq!(fs::read(results.join("a.csv"))?, b"a\n1\n");
    ok(&sandbox.run(&["add", "./results", "--as", "results", "--yes"])?)?;
    let lock = fs::read_to_string(work.join("data.lock"))?;
    for pinned in [&b"a\n1\n"[..], b"b\n2\n"] {
        assert!(lock.contains(&crate::support::sha256_hex(pinned)), "{lock}");
    }
    ok(&sandbox.run(&["status", "--check"])?)?;
    Ok(())
}

/// the line-ends fix told in a folder inside the project starts git in the project's folder: run
/// as told there, it rewrites the output's file and leaves a file of the same path below that
/// folder as it was, its staged edit and its edit not staged alike. told in the project's folder,
/// it names no folder.
#[test]
fn the_line_ends_fix_told_in_a_subfolder_starts_git_in_the_project() -> Result {
    let sandbox = Sandbox::new()?;
    if crate::merge::ok(&sandbox, &["init", "-q"])?.is_none() {
        return Ok(());
    }
    let work = sandbox.work();
    let author = sandbox.folder("author")?;
    fs::create_dir_all(author.join("results"))?;
    fs::write(author.join("results/table1.csv"), b"a,b\n1,2\n")?;
    ok(&sandbox
        .command_in(&author, &["add", "results", "--as", "results"])
        .output()?)?;
    for file in ["data.toml", "data.lock"] {
        fs::copy(author.join(file), work.join(file))?;
    }
    crate::merge::ok(&sandbox, &["config", "core.autocrlf", "true"])?;
    let paper = work.join("paper");
    let (pinned, shadow) = (
        work.join("results/table1.csv"),
        paper.join("results/table1.csv"),
    );
    fs::create_dir_all(work.join("results"))?;
    fs::create_dir_all(paper.join("results"))?;
    fs::write(&pinned, b"a,b\r\n1,2\r\n")?;
    aged(&pinned)?;
    fs::write(&shadow, b"paper copy\n")?;
    crate::merge::ok(&sandbox, &["add", "results", "paper"])?;
    crate::merge::ok(&sandbox, &["commit", "-qm", "results and paper"])?;
    fs::write(work.join(".gitattributes"), b"results/** -text\n")?;
    crate::merge::ok(&sandbox, &["add", ".gitattributes"])?;
    crate::merge::ok(&sandbox, &["commit", "-qm", "keep the results' bytes"])?;
    fs::write(&shadow, b"staged edit\n")?;
    crate::merge::ok(&sandbox, &["add", "paper/results/table1.csv"])?;
    fs::write(&shadow, b"edit not staged\n")?;
    let at_root = text(&sandbox.run(&["status"])?.stderr)?;
    assert!(
        at_root.contains("then `git rm --cached -q -- results/table1.csv` and `git checkout HEAD -- results/table1.csv` write"),
        "{at_root}"
    );
    assert!(!at_root.contains("git -C"), "{at_root}");
    let told = text(&sandbox.command_in(&paper, &["status"]).output()?.stderr)?;
    assert!(
        told.contains("then `git -C .. rm --cached -q -- results/table1.csv` and `git -C .. checkout HEAD -- results/table1.csv` write"),
        "{told}"
    );
    // each git command told, run as told in paper/, the next one too where one fails, as a
    // person runs a list of commands
    let mut failed = Vec::new();
    for command in told.split('`').skip(1).step_by(2) {
        let Some(words) = command.strip_prefix("git ") else {
            continue;
        };
        let words: Vec<&str> = words.split(' ').collect();
        let ran = crate::merge::git(&sandbox, &paper, &words)?.ok_or("git runs")?;
        if !ran.status.success() {
            failed.push(format!("{command}: {}", text(&ran.stderr)?));
        }
    }
    assert_eq!(fs::read(&shadow)?, b"edit not staged\n", "{failed:?}");
    let staged = crate::merge::git(&sandbox, &work, &["show", ":paper/results/table1.csv"])?
        .ok_or("git runs")?;
    assert_eq!(text(&staged.stdout)?, "staged edit\n", "{failed:?}");
    assert_eq!(fs::read(&pinned)?, b"a,b\n1,2\n", "{failed:?}");
    assert!(failed.is_empty(), "{failed:?}");
    ok(&sandbox.run(&["status", "--check"])?)?;
    Ok(())
}

/// gives the file at `path` a modified time long past before git records it, so git trusts its
/// record of the file, as one a checkout wrote long ago leaves it, and a checkout that compares
/// the file with that record alone skips it.
fn aged(path: &Path) -> Result {
    let long_ago = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);
    fs::File::options()
        .write(true)
        .open(path)?
        .set_modified(long_ago)?;
    Ok(())
}

/// the other way round: an output pinned with CRLF line ends, which git keeps as LF where it
/// converts them on commit, differs from its pin only in those line ends wherever git checks it out
/// as kept. no checkout here can bring the pinned bytes, so the fix is git keeping them as they
/// are, committed where they are, which a checkout then writes.
#[test]
fn an_outputs_line_ends_git_kept_as_lf_are_told_with_their_fix() -> Result {
    let sandbox = Sandbox::new()?;
    if crate::merge::ok(&sandbox, &["init", "-q"])?.is_none() {
        return Ok(());
    }
    let work = sandbox.work();
    let author = sandbox.folder("author")?;
    fs::create_dir_all(author.join("results"))?;
    fs::write(author.join("results/a.csv"), b"a,b\r\n1,2\r\n")?;
    ok(&sandbox
        .command_in(&author, &["add", "results", "--as", "results"])
        .output()?)?;
    for file in ["data.toml", "data.lock"] {
        fs::copy(author.join(file), work.join(file))?;
    }
    crate::merge::ok(&sandbox, &["config", "core.autocrlf", "false"])?;
    fs::create_dir_all(work.join("results"))?;
    fs::write(work.join("results/a.csv"), b"a,b\n1,2\n")?;
    crate::merge::ok(&sandbox, &["add", "results"])?;
    crate::merge::ok(&sandbox, &["commit", "-qm", "results"])?;
    let checked = sandbox.run(&["status", "--check"])?;
    let stderr = text(&checked.stderr)?;
    assert_eq!(checked.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("results/a.csv differs from the pin only in its line ends, which git keeps as LF where the pin has CRLF; where the pinned files are, add `results/** -text` to the .gitattributes beside data.toml and commit it, then `git add --renormalize -- results/a.csv` and commit, so a pull brings them as pinned"),
        "{stderr}"
    );
    assert!(!stderr.contains("fetchloom add"), "{stderr}");
    // the commit the fix names, made where the pinned bytes are, then the checkout a pull makes
    fs::write(work.join("results/a.csv"), b"a,b\r\n1,2\r\n")?;
    fs::write(work.join(".gitattributes"), b"results/** -text\n")?;
    crate::merge::ok(&sandbox, &["add", ".gitattributes"])?;
    crate::merge::ok(&sandbox, &["add", "--renormalize", "--", "results/a.csv"])?;
    crate::merge::ok(&sandbox, &["commit", "-qm", "keep the pinned bytes"])?;
    fs::remove_file(work.join("results/a.csv"))?;
    crate::merge::ok(&sandbox, &["checkout", "--", "results"])?;
    assert_eq!(fs::read(work.join("results/a.csv"))?, b"a,b\r\n1,2\r\n");
    ok(&sandbox.run(&["status", "--check"])?)?;
    Ok(())
}

/// a rerun pinned but not committed, which git wrote back with CRLF line ends while its index still
/// holds the first run, as `git stash`, a pull and `git stash pop` leave it, differs from the pin
/// only in line ends, though no checkout brings the pin back, as git keeps the first run: none is
/// offered, nor a re-pin, and nothing fetchloom does writes the first run over the rerun.
#[test]
fn line_ends_over_a_rerun_git_has_not_staged_offer_no_checkout() -> Result {
    let sandbox = Sandbox::new()?;
    if crate::merge::ok(&sandbox, &["init", "-q"])?.is_none() {
        return Ok(());
    }
    let work = sandbox.work();
    let author = sandbox.folder("author")?;
    fs::create_dir_all(author.join("results"))?;
    fs::write(author.join("results/table1.csv"), b"a,b\n3,4\n")?;
    ok(&sandbox
        .command_in(&author, &["add", "results", "--as", "results"])
        .output()?)?;
    for file in ["data.toml", "data.lock"] {
        fs::copy(author.join(file), work.join(file))?;
    }
    crate::merge::ok(&sandbox, &["config", "core.autocrlf", "true"])?;
    fs::create_dir_all(work.join("results"))?;
    fs::write(work.join("results/table1.csv"), b"a,b\r\n1,2\r\n")?;
    crate::merge::ok(&sandbox, &["add", "results"])?;
    crate::merge::ok(&sandbox, &["commit", "-qm", "the first run"])?;
    let rerun = b"a,b\r\n3,4\r\n";
    fs::write(work.join("results/table1.csv"), rerun)?;
    let told = "results/table1.csv differs from the pin only in its line ends, made CRLF by git's checkout, but git's staged or committed copy is not the pinned one, so no checkout brings the pin back; add `results/** -text` to the .gitattributes beside data.toml and commit it, then give it LF line ends";
    for (args, code) in [
        (&["status"][..], 0),
        (&["status", "--check"], 1),
        (&["add", "./results", "--as", "results"], 1),
        (&["sync", "--force"], 0),
    ] {
        let output = sandbox.run(args)?;
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(code), "{args:?}: {stderr}");
        assert!(stderr.contains(told), "{args:?}: {stderr}");
        assert!(!stderr.contains("git checkout"), "{args:?}: {stderr}");
        assert!(!stderr.contains("git rm"), "{args:?}: {stderr}");
        assert!(!stderr.contains("fetchloom add"), "{args:?}: {stderr}");
    }
    assert_eq!(fs::read(work.join("results/table1.csv"))?, rerun);
    Ok(())
}

/// a rerun pinned and staged but not committed, which git's checkout gave CRLF line ends, is never
/// written from the last commit, which holds the first run: unstaging it and checking it out from
/// there would write the first run over the rerun, so no command is offered for it.
#[test]
fn a_rerun_staged_but_not_committed_is_never_checked_out_from_the_last_commit() -> Result {
    let sandbox = Sandbox::new()?;
    if crate::merge::ok(&sandbox, &["init", "-q"])?.is_none() {
        return Ok(());
    }
    let work = sandbox.work();
    let author = sandbox.folder("author")?;
    fs::create_dir_all(author.join("results"))?;
    fs::write(author.join("results/table1.csv"), b"a,b\n3,4\n")?;
    ok(&sandbox
        .command_in(&author, &["add", "results", "--as", "results"])
        .output()?)?;
    for file in ["data.toml", "data.lock"] {
        fs::copy(author.join(file), work.join(file))?;
    }
    crate::merge::ok(&sandbox, &["config", "core.autocrlf", "true"])?;
    let table = work.join("results/table1.csv");
    fs::create_dir_all(work.join("results"))?;
    fs::write(&table, b"a,b\r\n1,2\r\n")?;
    crate::merge::ok(&sandbox, &["add", "results"])?;
    crate::merge::ok(&sandbox, &["commit", "-qm", "the first run"])?;
    fs::write(&table, b"a,b\r\n3,4\r\n")?;
    crate::merge::ok(&sandbox, &["add", "results"])?;
    let stderr = text(&sandbox.run(&["status"])?.stderr)?;
    assert!(
        stderr.contains("results/table1.csv differs from the pin only in its line ends, made CRLF by git's checkout, but git's staged or committed copy is not the pinned one, so no checkout brings the pin back"),
        "{stderr}"
    );
    assert!(!stderr.contains("git checkout"), "{stderr}");
    assert!(!stderr.contains("git rm"), "{stderr}");
    Ok(())
}

/// beside a rerun git has not staged, a file git keeps as pinned is checked out alone: the fix
/// names that file's checkout and asks LF line ends for the rerun, and following it leaves the rerun
/// as it was, still to be made ready.
#[test]
fn the_line_ends_checkout_leaves_out_a_file_git_staged_otherwise() -> Result {
    let sandbox = Sandbox::new()?;
    if crate::merge::ok(&sandbox, &["init", "-q"])?.is_none() {
        return Ok(());
    }
    let work = sandbox.work();
    let author = sandbox.folder("author")?;
    fs::create_dir_all(author.join("results"))?;
    fs::write(author.join("results/b.csv"), b"x\n")?;
    fs::write(author.join("results/table1.csv"), b"a,b\n3,4\n")?;
    ok(&sandbox
        .command_in(&author, &["add", "results", "--as", "results"])
        .output()?)?;
    for file in ["data.toml", "data.lock"] {
        fs::copy(author.join(file), work.join(file))?;
    }
    crate::merge::ok(&sandbox, &["config", "core.autocrlf", "true"])?;
    let results = work.join("results");
    fs::create_dir_all(&results)?;
    fs::write(results.join("b.csv"), b"x\r\n")?;
    fs::write(results.join("table1.csv"), b"a,b\r\n1,2\r\n")?;
    aged(&results.join("b.csv"))?;
    crate::merge::ok(&sandbox, &["add", "results"])?;
    crate::merge::ok(&sandbox, &["commit", "-qm", "the first run"])?;
    let rerun = b"a,b\r\n3,4\r\n";
    fs::write(results.join("table1.csv"), rerun)?;
    let checked = sandbox.run(&["status", "--check"])?;
    let stderr = text(&checked.stderr)?;
    assert_eq!(checked.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("2 files git tracks in results's folder, such as results/b.csv, differ from the pin only in their line ends, made CRLF by git's checkout; add `results/** -text` to the .gitattributes beside data.toml and commit it, then `git rm --cached -q -- results/b.csv` and `git checkout HEAD -- results/b.csv` write the bytes git keeps for those it keeps as pinned, and give results/table1.csv LF line ends, as git's staged or committed copy is not the pinned one"),
        "{stderr}"
    );
    fs::write(work.join(".gitattributes"), b"results/** -text\n")?;
    crate::merge::ok(&sandbox, &["add", ".gitattributes"])?;
    crate::merge::ok(&sandbox, &["commit", "-qm", "keep the results' bytes"])?;
    crate::merge::ok(&sandbox, &["rm", "--cached", "-q", "--", "results/b.csv"])?;
    crate::merge::ok(&sandbox, &["checkout", "HEAD", "--", "results/b.csv"])?;
    assert_eq!(fs::read(results.join("b.csv"))?, b"x\n");
    assert_eq!(fs::read(results.join("table1.csv"))?, rerun);
    let checked = sandbox.run(&["status", "--check"])?;
    assert_eq!(checked.status.code(), Some(1), "{}", text(&checked.stderr)?);
    Ok(())
}

/// the line-ends fix checks out just the pinned files whose staged copy holds the pinned bytes,
/// never the output's folder: a system file and a link git tracks there, which the pin leaves out,
/// keep the edits made to them, and the output is ready after the fix.
#[test]
fn the_line_ends_checkout_names_only_the_pinned_files() -> Result {
    let sandbox = Sandbox::new()?;
    if crate::merge::ok(&sandbox, &["init", "-q"])?.is_none() {
        return Ok(());
    }
    let work = sandbox.work();
    let author = sandbox.folder("author")?;
    fs::create_dir_all(author.join("results"))?;
    fs::write(author.join("results/table1.csv"), b"a,b\n1,2\n")?;
    ok(&sandbox
        .command_in(&author, &["add", "results", "--as", "results"])
        .output()?)?;
    for file in ["data.toml", "data.lock"] {
        fs::copy(author.join(file), work.join(file))?;
    }
    crate::merge::ok(&sandbox, &["config", "core.autocrlf", "true"])?;
    let results = work.join("results");
    fs::create_dir_all(&results)?;
    fs::write(results.join("table1.csv"), b"a,b\r\n1,2\r\n")?;
    aged(&results.join("table1.csv"))?;
    fs::write(results.join(".DS_Store"), b"first")?;
    #[cfg(unix)]
    std::os::unix::fs::symlink("table1.csv", results.join("latest"))?;
    crate::merge::ok(&sandbox, &["add", "-f", "results"])?;
    crate::merge::ok(&sandbox, &["commit", "-qm", "results"])?;
    fs::write(results.join(".DS_Store"), b"edited")?;
    #[cfg(unix)]
    {
        fs::remove_file(results.join("latest"))?;
        std::os::unix::fs::symlink("../notes/run2.csv", results.join("latest"))?;
    }
    let status = sandbox.run(&["status"])?;
    let stderr = text(&status.stderr)?;
    assert!(
        stderr.contains("; add `results/** -text` to the .gitattributes beside data.toml and commit it, then `git rm --cached -q -- results/table1.csv` and `git checkout HEAD -- results/table1.csv` write the bytes git keeps, which the pin holds"),
        "{stderr}"
    );
    fs::write(work.join(".gitattributes"), b"results/** -text\n")?;
    crate::merge::ok(&sandbox, &["add", ".gitattributes"])?;
    crate::merge::ok(&sandbox, &["commit", "-qm", "keep the results' bytes"])?;
    crate::merge::ok(
        &sandbox,
        &["rm", "--cached", "-q", "--", "results/table1.csv"],
    )?;
    crate::merge::ok(&sandbox, &["checkout", "HEAD", "--", "results/table1.csv"])?;
    assert_eq!(fs::read(results.join("table1.csv"))?, b"a,b\n1,2\n");
    assert_eq!(fs::read(results.join(".DS_Store"))?, b"edited");
    #[cfg(unix)]
    assert_eq!(
        fs::read_link(results.join("latest"))?,
        Path::new("../notes/run2.csv")
    );
    ok(&sandbox.run(&["status", "--check"])?)?;
    Ok(())
}

/// the .gitattributes line the line-ends fix tells names the output's folder as git reads a
/// pattern: brackets and a space in its name are escaped and quoted, so git keeps that folder's
/// bytes and goes on converting the line ends of a folder the brackets would otherwise match.
#[test]
fn the_told_attributes_line_keeps_just_the_outputs_folder() -> Result {
    let sandbox = Sandbox::new()?;
    if crate::merge::ok(&sandbox, &["init", "-q"])?.is_none() {
        return Ok(());
    }
    let work = sandbox.work();
    let author = sandbox.folder("author")?;
    let at = "results/[run] 1";
    fs::create_dir_all(author.join(at))?;
    fs::write(author.join(at).join("a.csv"), b"a,b\n1,2\n")?;
    ok(&sandbox
        .command_in(&author, &["add", &format!("./{at}"), "--as", "run"])
        .output()?)?;
    for file in ["data.toml", "data.lock"] {
        fs::copy(author.join(file), work.join(file))?;
    }
    crate::merge::ok(&sandbox, &["config", "core.autocrlf", "true"])?;
    for folder in [at, "results/r 1"] {
        fs::create_dir_all(work.join(folder))?;
        fs::write(work.join(folder).join("a.csv"), b"a,b\r\n1,2\r\n")?;
        aged(&work.join(folder).join("a.csv"))?;
    }
    crate::merge::ok(&sandbox, &["add", "results"])?;
    crate::merge::ok(&sandbox, &["commit", "-qm", "results"])?;
    let stderr = text(&sandbox.run(&["status"])?.stderr)?;
    assert!(
        stderr.contains("add `\"results/\\\\[run\\\\] 1/**\" -text` to the .gitattributes beside data.toml and commit it"),
        "{stderr}"
    );
    let line = stderr
        .split_once("add `")
        .and_then(|(_, rest)| rest.split_once("` to the .gitattributes"))
        .ok_or("the .gitattributes line is told")?
        .0;
    fs::write(work.join(".gitattributes"), format!("{line}\n"))?;
    let files = ["results/[run] 1/a.csv", "results/r 1/a.csv"];
    let mut args = vec!["check-attr", "text", "--"];
    args.extend(files);
    let checked = crate::merge::git(&sandbox, &work, &args)?.ok_or("git runs")?;
    assert_eq!(
        text(&checked.stdout)?,
        "results/[run] 1/a.csv: text: unset\nresults/r 1/a.csv: text: unspecified\n"
    );
    // the fresh checkout takes the folder's name as written, never as a pattern
    for told in [
        "git --literal-pathspecs rm --cached -q -- ",
        "git --literal-pathspecs checkout HEAD -- ",
    ] {
        assert!(stderr.contains(told), "{stderr}");
    }
    crate::merge::ok(&sandbox, &["add", ".gitattributes"])?;
    crate::merge::ok(&sandbox, &["commit", "-qm", "keep the run's bytes"])?;
    let file = "results/[run] 1/a.csv";
    crate::merge::ok(
        &sandbox,
        &["--literal-pathspecs", "rm", "--cached", "-q", "--", file],
    )?;
    crate::merge::ok(
        &sandbox,
        &["--literal-pathspecs", "checkout", "HEAD", "--", file],
    )?;
    assert_eq!(fs::read(work.join(file))?, b"a,b\n1,2\n");
    assert_eq!(fs::read(work.join("results/r 1/a.csv"))?, b"a,b\r\n1,2\r\n");
    ok(&sandbox.run(&["status", "--check"])?)?;
    Ok(())
}

/// push asks first, as no source says who may read an output, and then sends the files an output
/// pins, never what its folder holds since, and says so when the folder changed, with the pin that
/// sends the folder's files instead; a folder that is gone gets the ways back.
#[test]
fn push_sends_an_outputs_pinned_files_and_says_when_its_folder_changed() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    processed(&work)?;
    ok(&sandbox.run(&["add", "data/processed", "--as", "processed"])?)?;
    fs::write(work.join("data/processed/plot.png"), b"png")?;
    let mirror = sandbox.folder("mirror")?;
    let mirror = mirror.to_string_lossy().into_owned();
    let asked = json(&sandbox.run(&["--json", "push", "--to", &mirror])?)?;
    assert_eq!(
        asked["errors"][0]["message"],
        "fetchloom cannot tell who may read processed, an output of this project, so pushing it needs a yes",
        "{asked}"
    );
    let pushed = sandbox.run(&["--json", "push", "--to", &mirror, "--yes"])?;
    let document = json(&pushed)?;
    assert_eq!(pushed.status.code(), Some(0), "{document}");
    assert_eq!(
        crate::support::warning(&document, "output.changed")["message"],
        "data/processed changed since processed was pinned: 1 extra file, so push sends the files processed pins, not these",
        "{document}"
    );
    assert_eq!(
        document["result"]["objects"], 2,
        "the pinned files alone: {document}"
    );
    let help = |document: &serde_json::Value| {
        crate::support::warning(document, "output.changed")["help"].clone()
    };
    assert_eq!(
        help(&document),
        "run `fetchloom add ./data/processed --as processed` first to pin what it holds now"
    );
    delete_folder(&work.join("data/processed"))?;
    let gone = json(&sandbox.run(&["--json", "push", "--to", &mirror, "--yes"])?)?;
    assert_eq!(
        crate::support::warning(&gone, "output.changed")["message"],
        "data/processed is gone, so push sends the files processed pins",
        "{gone}"
    );
    assert_eq!(
        help(&gone),
        "`fetchloom sync --force processed` puts back the files processed pins, or if you moved it, set at = \"<new folder>\" for processed in data.toml",
        "{gone}"
    );
    Ok(())
}

/// data.toml keeps every pattern add is given, for an output and for an import alike, each
/// dataset in its place by name.
#[test]
fn add_keeps_every_pattern_it_is_given() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    fs::create_dir_all(work.join("results"))?;
    let flowers = sandbox.folder("flowers")?;
    for (folder, name) in [
        (work.join("results"), "r.csv"),
        (work.join("results"), "run.log"),
        (work.join("results"), "run.tmp"),
        (flowers.clone(), "sepals.csv"),
        (flowers.clone(), "petals.tsv"),
        (flowers, "notes.txt"),
    ] {
        fs::write(folder.join(name), b"x\n")?;
    }
    for add in [
        "add results --as results --exclude *.log --exclude *.tmp",
        "add ../flowers --select *.csv --select *.tsv",
    ] {
        ok(&sandbox.run(&add.split(' ').collect::<Vec<_>>())?)?;
    }
    assert_eq!(
        fs::read_to_string(work.join("data.toml"))?,
        "[datasets.flowers]\nref = \"local\"\nselect = [\"*.csv\", \"*.tsv\"]\n\n[datasets.results]\nref = \"local\"\nat = \"results\"\nexclude = [\"*.log\", \"*.tmp\"]\n"
    );
    Ok(())
}

/// select keeps an output to the files it names: one it leaves out, there at the pin or written
/// after, is no change to the output.
#[test]
fn select_leaves_an_outputs_other_files_out() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    fs::create_dir_all(work.join("results"))?;
    fs::write(work.join("results/r.csv"), b"r\n")?;
    fs::write(work.join("results/run.log"), b"log\n")?;
    ok(&sandbox.run(&["add", "results", "--as", "results", "--select", "*.csv"])?)?;
    fs::write(work.join("results/plot.png"), b"png\n")?;
    let checked = sandbox.run(&["status", "--check"])?;
    assert_eq!(checked.status.code(), Some(0), "{}", text(&checked.stderr)?);
    let document = json(&sandbox.run(&["--json", "status"])?)?;
    assert_eq!(
        listed(&document, "extra"),
        Vec::<String>::new(),
        "{document}"
    );
    Ok(())
}

/// exclude takes a path in an output's folder, wherever it is: the pin leaves the file out, and
/// the author's file stays, no difference since its pattern leaves it out.
#[test]
fn exclude_leaves_an_outputs_file_out_and_keeps_it() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    fs::create_dir_all(work.join("results"))?;
    fs::write(work.join("results/r.csv"), b"r\n")?;
    fs::write(work.join("results/run.log"), b"log\n")?;
    ok(&sandbox.run(&["add", "results", "--as", "results"])?)?;
    let excluded = sandbox.run(&["exclude", "results/run.log"])?;
    assert_eq!(
        excluded.status.code(),
        Some(0),
        "{}",
        text(&excluded.stderr)?
    );
    assert_eq!(
        fs::read_to_string(work.join("data.toml"))?,
        "[datasets.results]\nref = \"local\"\nat = \"results\"\nexclude = [\"run.log\"]\n"
    );
    assert!(!fs::read_to_string(work.join("data.lock"))?.contains("run.log"));
    assert_eq!(fs::read(work.join("results/run.log"))?, b"log\n");
    let document = json(&sandbox.run(&["--json", "status"])?)?;
    for kind in ["missing", "edited", "extra", "unneeded"] {
        assert_eq!(
            listed(&document, kind),
            Vec::<String>::new(),
            "{kind}: {document}"
        );
    }
    Ok(())
}

/// drop frees an output's bytes from the cache once a mirror keeps them, and leaves its folder,
/// the author's, as it is, still out of git since data.lock pins it; status counts none of its
/// files.
#[test]
fn dropping_an_output_keeps_its_folder() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    processed(&work)?;
    ok(&sandbox.run(&["add", "data/processed", "--as", "processed"])?)?;
    let mirror = sandbox.folder("mirror")?;
    let mirror = mirror.to_string_lossy().into_owned();
    ok(&sandbox.run(&["push", "--to", &mirror, "--yes"])?)?;
    let dropped = sandbox.run(&["drop", "processed", "--mirror", &mirror])?;
    assert_eq!(dropped.status.code(), Some(0), "{}", text(&dropped.stderr)?);
    assert_eq!(fs::read(work.join("data/processed/iris_clean.csv"))?, CLEAN);
    assert_eq!(fs::read(work.join("data/processed/summary.txt"))?, SUMMARY);
    let marker = work.join("data/processed/.gitignore");
    assert_eq!(crate::support::marker(&marker)?, b"*\n");
    let document = json(&sandbox.run(&["--json", "status"])?)?;
    for kind in ["missing", "edited", "extra", "unneeded"] {
        assert_eq!(
            listed(&document, kind),
            Vec::<String>::new(),
            "{kind}: {document}"
        );
    }
    ok(&sandbox.run(&["sync"])?)?;
    assert_eq!(fs::read(work.join("data/processed/iris_clean.csv"))?, CLEAN);
    assert_eq!(crate::support::marker(&marker)?, b"*\n");
    Ok(())
}

/// "changed" means content: a rerun that writes the same bytes is no change. status, which writes
/// nothing, hashes such a file each time it looks; the next sync records its new look, so a later
/// run hashes it no more: once per sync.
#[test]
fn a_rewrite_with_the_same_bytes_is_no_change_and_is_hashed_once_per_sync() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    processed(&work)?;
    ok(&sandbox.run(&["add", "data/processed", "--as", "processed"])?)?;
    let clean = work.join("data/processed/iris_clean.csv");
    let recorded = |state: &serde_json::Value| {
        state["files"]["data/processed/iris_clean.csv"]["look"]["modified"]
            .as_u64()
            .map(u128::from)
    };
    let state = || -> Result<serde_json::Value> {
        Ok(serde_json::from_slice(&fs::read(
            work.join(".fetchloom/state"),
        )?)?)
    };
    let pinned = recorded(&state()?);
    sandbox.wait_for_a_later_time(fs::metadata(&clean)?.modified()?)?;
    fs::write(&clean, CLEAN)?;
    let modified = fs::metadata(&clean)?
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    for _ in 0..2 {
        let document = json(&sandbox.run(&["--json", "status"])?)?;
        for kind in ["missing", "edited", "extra", "untracked"] {
            assert_eq!(
                listed(&document, kind),
                Vec::<String>::new(),
                "{kind}: {document}"
            );
        }
        assert_eq!(recorded(&state()?), pinned, "status writes nothing");
    }
    ok(&sandbox.run(&["sync"])?)?;
    assert_eq!(recorded(&state()?), Some(modified));
    Ok(())
}

/// at a terminal the same add shows what changed and asks: a no changes nothing, a yes pins it.
#[cfg(target_os = "linux")]
#[test]
fn the_same_add_asks_at_a_terminal_before_it_pins_a_rerun() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    processed(&work)?;
    ok(&sandbox.run(&["add", "data/processed", "--as", "processed"])?)?;
    let pinned = fs::read_to_string(work.join("data.lock"))?;
    rerun(&work)?;
    let asked = "pin what data/processed holds now as processed?";
    let add = sandbox.command(&["add", "data/processed", "--as", "processed"]);
    let Some((code, screen)) = crate::support::typed(&work, &add, "", asked, "n\r")? else {
        return Ok(());
    };
    assert!(
        screen.contains("Changed processed · 1 edited file, 1 extra file, 1 missing file"),
        "{screen}"
    );
    assert!(screen.contains(&format!("{asked} [y/N]")), "{screen}");
    assert_eq!(code, Some(1), "{screen}");
    assert_eq!(fs::read_to_string(work.join("data.lock"))?, pinned);
    let Some((code, screen)) = crate::support::typed(&work, &add, "", asked, "y\r")? else {
        return Ok(());
    };
    assert_eq!(code, Some(0), "{screen}");
    assert!(
        fs::read_to_string(work.join("data.lock"))?.contains("plot.png"),
        "{screen}"
    );
    Ok(())
}
