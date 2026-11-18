//! outputs: a folder of the project pinned where its pipeline writes it, shared by hash, placed
//! writable for a teammate, and never partly healed once it changed.

use std::fs;
use std::path::Path;

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
    assert!(stderr.contains(&format!("help: {back}\n")), "{stderr}");
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
                "error: {} is the folder of the output processed, and it is not there\n  help: run your pipeline to make it, or if you moved it, set at = \"<new folder>\" for processed in data.toml\n",
                typed[0]
            )),
            "{typed:?}: {stderr}"
        );
    }
    ok(&sandbox.run(&["sync", "--force", "processed"])?)?;
    assert_eq!(fs::read(work.join("data/processed/iris_clean.csv"))?, CLEAN);
    Ok(())
}

/// git carries data.lock, never an output's bytes: a teammate whose cache and mirrors lack them
/// still gets every other dataset, and the run names the output once, as made on another machine,
/// with both ways on, and ends well, so a pipeline run after it can make the output. the output's
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
    let said =
        "processed was made on another machine, and this machine's cache does not hold its files";
    let ways = "push them from the machine that made them with `fetchloom push --to <mirror>` and add that mirror here with `fetchloom settings set mirrors <mirror>`, or run your pipeline here, then `fetchloom add ./data/processed --as processed`";
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

/// a teammate's status names every output made on another machine in one hint, with the ways on
/// for each, and as it asks no mirror, says only what the cache lacks; a sync that asked one says
/// so.
#[test]
fn status_names_the_outputs_made_elsewhere_in_one_hint() -> Result {
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
        stderr.contains("  hint: a and b were made on another machine, and this machine's cache does not hold their files: push them from the machine that made them with `fetchloom push --to <mirror>` and add that mirror here with `fetchloom settings set mirrors <mirror>`, or run your pipeline here, then `fetchloom add <folder> --as <name>` for each\n"),
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
        "a was made on another machine, and neither this machine's cache nor a mirror holds its files",
        "{document}"
    );
    Ok(())
}

/// a teammate without an output's bytes is told once per output and pin, by sync and path alike,
/// that it was made on another machine; each sync's last line still names it, and json says it on
/// every run, as a script reads each run alone. a run of another dataset alone, as a notebook's
/// `path` of it, keeps what was told. a new pin is told of again, and so is an output dropped from
/// data.toml and pinned again with the same files.
#[test]
fn an_output_made_elsewhere_is_told_once_per_pin() -> Result {
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
    let said = "warning: processed was made on another machine";
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
            "`fetchloom sync --force processed` to make it hold just the files processed pins"
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
    let changes = "1 file changed, 1 added, 1 removed";
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
        stderr.contains("warning: data/processed changed since processed was pinned: 1 file added, so sync leaves it as it is"),
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
            "data/processed changed since processed was pinned: `fetchloom add ./data/processed --as processed` pins it as it is now, or `fetchloom sync --force processed` discards the changes"
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
    assert_eq!(asked.status.code(), Some(2), "{stderr}");
    assert!(
        stderr.contains("error: data/processed holds other files than processed pins"),
        "{stderr}"
    );
    for line in [
        "changed iris_clean.csv",
        "added plot.png",
        "removed summary.txt",
    ] {
        assert!(stderr.contains(line), "{line}: {stderr}");
    }
    assert!(
        !stderr.contains("run.log"),
        "an excluded file is no change: {stderr}"
    );
    assert!(
        stderr.contains("fetchloom add ./data/processed --as processed --yes"),
        "{stderr}"
    );
    assert!(
        stderr.contains(
            "`fetchloom sync --force processed` to make it hold just the files processed pins"
        ),
        "{stderr}"
    );
    assert_eq!(fs::read_to_string(work.join("data.lock"))?, pinned);
    let output = sandbox.run(&["add", "data/processed", "--as", "processed", "--yes"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("Pinned processed · 1 file changed, 1 added, 1 removed"),
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
    assert_eq!(refused.status.code(), Some(10), "{document}");
    let error = &document["errors"][0];
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
            "data/processed changed since processed was pinned, 1 file changed: `fetchloom add ./data/processed --as processed` pins it as it is now, or `fetchloom sync --force processed` discards the changes"
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
            "data/processed changed since processed was pinned, 1 file changed, 1 added, 1 removed: `fetchloom add ./data/processed --as processed` pins it as it is now, or `fetchloom sync --force processed` discards the changes"
        ]),
        "{document}"
    );
    let dry = sandbox.run(&["add", "data/processed", "--as", "processed", "--dry-run"])?;
    let stderr = text(&dry.stderr)?;
    assert_eq!(dry.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("Would pin processed · 1 file changed, 1 added, 1 removed"),
        "{stderr}"
    );
    let listed = sandbox.run(&["-v", "add", "data/processed", "--as", "processed", "-n"])?;
    let stderr = text(&listed.stderr)?;
    for line in [
        "changed iris_clean.csv",
        "added plot.png",
        "removed summary.txt",
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
/// unchanged once the editor closes, and once a notebook and an office suite leave theirs.
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

/// push asks first, as no source says who may read an output, and then sends the files an output
/// pins, never what its folder holds since, and says so when the folder changed.
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
        "data/processed changed since processed was pinned: 1 file added, so push sends the files processed pins, not these",
        "{document}"
    );
    assert_eq!(
        document["result"]["objects"], 2,
        "the pinned files alone: {document}"
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
        screen.contains("Changed processed · 1 file changed, 1 added, 1 removed"),
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
